//! Typed side-effect draining and client rendering.

use super::coalesce::{
    async_runtime_current_unix_millis, coalesce_render_invalidation_reason,
    pane_io_side_effect_targets_instance, pane_io_side_effect_targets_pane,
    timer_side_effect_targets_timer_worker,
};
use super::{
    AsyncRenderedClientFlush, AsyncRuntimeSessionActor, AttachedTerminalOutputModes, ClientId,
    ClientState, ClientStatusLine, ClientViewRole, MezError, RenderInvalidationReason, Result,
    RuntimeSideEffect, TerminalClientLoopConfig, VecDeque, compose_client_presentation_with_styles,
};

impl AsyncRuntimeSessionActor {
    /// Runs the drain runtime side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn drain_runtime_side_effects(
        &mut self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime side-effect drain limit must be greater than zero",
            ));
        }
        let drain_count = limit.min(self.side_effects.len());
        let effects = self.side_effects.drain(..drain_count).collect();
        self.record_side_effect_drain(drain_count);
        Ok(effects)
    }

    /// Records a side-effect drain and wakes peers when retained work remains.
    ///
    /// The side-effect queue is shared by several filtered workers. One worker
    /// can drain its own work and retain work for another worker, consuming the
    /// original queue notification in the process. Re-notifying only after real
    /// drain progress keeps retained work responsive without spinning workers
    /// that inspected an unrelated non-empty queue.
    pub(super) fn record_side_effect_drain(&mut self, drained: usize) {
        self.metrics.runtime_side_effects_drained = self
            .metrics
            .runtime_side_effects_drained
            .saturating_add(u64::try_from(drained).unwrap_or(u64::MAX));
        self.metrics
            .runtime_side_effect_drain_sizes
            .record(u64::try_from(drained).unwrap_or(u64::MAX));
        self.metrics.side_effect_queue_depth = self.side_effects.len();
        self.metrics
            .side_effect_queue_depth_samples
            .record(u64::try_from(self.side_effects.len()).unwrap_or(u64::MAX));
        if drained > 0 && !self.side_effects.is_empty() {
            self.notify_side_effect_delivery();
        }
    }

    /// Runs the drain agent provider dispatch side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn drain_agent_provider_dispatch_side_effects(
        &mut self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime provider dispatch drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit
                && matches!(
                    effect,
                    RuntimeSideEffect::DispatchAgentProvider { .. }
                        | RuntimeSideEffect::DispatchAgentCompaction { .. }
                        | RuntimeSideEffect::DispatchAgentRemember { .. }
                )
            {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Runs the drain render side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn drain_render_side_effects(
        &mut self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime render side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained: Vec<(ClientId, RenderInvalidationReason)> = Vec::new();
        let mut removed = 0usize;
        let mut coalesced = 0usize;
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            match effect {
                RuntimeSideEffect::RenderClient { client_id, reason } => {
                    if let Some((_, retained_reason)) = drained
                        .iter_mut()
                        .find(|(drained_client_id, _)| drained_client_id == &client_id)
                    {
                        *retained_reason =
                            coalesce_render_invalidation_reason(*retained_reason, reason);
                        removed = removed.saturating_add(1);
                        coalesced = coalesced.saturating_add(1);
                    } else if drained.len() < limit {
                        drained.push((client_id, reason));
                        removed = removed.saturating_add(1);
                    } else {
                        retained.push_back(RuntimeSideEffect::RenderClient { client_id, reason });
                    }
                }
                effect => retained.push_back(effect),
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(removed);
        self.metrics.render_invalidations_coalesced = self
            .metrics
            .render_invalidations_coalesced
            .saturating_add(u64::try_from(coalesced).unwrap_or(u64::MAX));
        Ok(drained
            .into_iter()
            .map(|(client_id, reason)| RuntimeSideEffect::RenderClient { client_id, reason })
            .collect())
    }

    /// Runs the drain render side effects for client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn drain_render_side_effects_for_client(
        &mut self,
        client_id: &ClientId,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime render side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained: Vec<RenderInvalidationReason> = Vec::new();
        let mut removed = 0usize;
        let mut coalesced = 0usize;
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            match effect {
                RuntimeSideEffect::RenderClient {
                    client_id: effect_client_id,
                    reason,
                } if &effect_client_id == client_id => {
                    if let Some(retained_reason) = drained.first_mut() {
                        *retained_reason =
                            coalesce_render_invalidation_reason(*retained_reason, reason);
                        coalesced = coalesced.saturating_add(1);
                    } else if drained.len() < limit {
                        drained.push(reason);
                    } else {
                        retained.push_back(RuntimeSideEffect::RenderClient {
                            client_id: effect_client_id,
                            reason,
                        });
                        continue;
                    }
                    removed = removed.saturating_add(1);
                }
                effect => retained.push_back(effect),
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(removed);
        self.metrics.render_invalidations_coalesced = self
            .metrics
            .render_invalidations_coalesced
            .saturating_add(u64::try_from(coalesced).unwrap_or(u64::MAX));
        Ok(drained
            .into_iter()
            .map(|reason| RuntimeSideEffect::RenderClient {
                client_id: client_id.clone(),
                reason,
            })
            .collect())
    }

    /// Runs the drain client output flush side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn drain_client_output_flush_side_effects(
        &mut self,
        client_id: Option<&ClientId>,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime client output flush drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit {
                match &effect {
                    RuntimeSideEffect::FlushClientOutput {
                        client_id: effect_client_id,
                        ..
                    } if client_id.is_none_or(|target| target == effect_client_id) => {
                        drained.push(effect);
                    }
                    _ => retained.push_back(effect),
                }
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Runs the drain timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn drain_timer_side_effects(
        &mut self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime timer side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit && timer_side_effect_targets_timer_worker(&effect) {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Runs the drain persistence side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn drain_persistence_side_effects(
        &mut self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime persistence side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit
                && matches!(
                    effect,
                    RuntimeSideEffect::Persist { .. }
                        | RuntimeSideEffect::PersistAuditLog { .. }
                        | RuntimeSideEffect::PersistTranscriptEntries { .. }
                        | RuntimeSideEffect::PersistPromptHistory { .. }
                        | RuntimeSideEffect::PersistCommandPromptHistory { .. }
                        | RuntimeSideEffect::PersistRegistry { .. }
                )
            {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Runs the drain hook side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn drain_hook_side_effects(
        &mut self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime hook side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit && matches!(effect, RuntimeSideEffect::RunProgramHook { .. }) {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Runs the drain pane io side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn drain_pane_io_side_effects(
        &mut self,
        pane_id: &str,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if pane_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "async runtime pane I/O side-effect drain pane id must not be empty",
            ));
        }
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime pane I/O side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit && pane_io_side_effect_targets_pane(&effect, pane_id) {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained
            .into_iter()
            .map(|effect| match effect {
                RuntimeSideEffect::PaneProcessIo { instance, effect } => match effect {
                    crate::runtime::PaneProcessIoEffect::WriteInput { bytes } => {
                        RuntimeSideEffect::WritePaneInput {
                            pane_id: instance.pane_id,
                            bytes,
                        }
                    }
                    crate::runtime::PaneProcessIoEffect::WriteInputPriority { bytes } => {
                        RuntimeSideEffect::WritePaneInputPriority {
                            pane_id: instance.pane_id,
                            bytes,
                        }
                    }
                    crate::runtime::PaneProcessIoEffect::Resize { size } => {
                        RuntimeSideEffect::ResizePane {
                            pane_id: instance.pane_id,
                            size,
                        }
                    }
                    crate::runtime::PaneProcessIoEffect::Terminate { force } => {
                        RuntimeSideEffect::TerminatePane {
                            pane_id: instance.pane_id,
                            force,
                        }
                    }
                },
                effect => effect,
            })
            .collect())
    }

    /// Drains pane I/O side effects for one exact adapter-owned process.
    pub(super) fn drain_pane_process_io_side_effects(
        &mut self,
        instance: &crate::runtime::PaneProcessInstance,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if instance.pane_id.trim().is_empty() || instance.generation == 0 {
            return Err(MezError::invalid_args(
                "async runtime pane process identity must be complete",
            ));
        }
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime pane process I/O drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit && pane_io_side_effect_targets_instance(&effect, instance) {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Runs the render client side effect operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn render_client_side_effect(
        &mut self,
        client_id: ClientId,
        config: TerminalClientLoopConfig,
        status: Option<ClientStatusLine>,
        cursor_blink_elapsed_ms: u64,
    ) -> Result<Option<AsyncRenderedClientFlush>> {
        let Some(client_size) = self.attached_client_size(&client_id)? else {
            return Ok(None);
        };
        let role = if self.service.session().primary_client_id() == Some(&client_id) {
            ClientViewRole::Primary
        } else {
            ClientViewRole::Observer
        };
        let config = self.service.terminal_client_loop_config(config)?;
        let Some(view) =
            self.service
                .render_client_view_with_resolved_config(role, client_size, &config)?
        else {
            return Ok(None);
        };
        let cursor_visible = view.cursor_visible;
        let cursor_row = view.cursor_row;
        let cursor_column = view.cursor_column;
        let application_keypad = view.application_keypad;
        let bracketed_paste = view.bracketed_paste;
        let (lines, line_style_spans) =
            compose_client_presentation_with_styles(&view, status.as_ref());
        let flush = AsyncRenderedClientFlush {
            client_id,
            lines,
            line_style_spans,
            modes: AttachedTerminalOutputModes {
                application_keypad,
                bracketed_paste,
                focus_events: view.focus_events,
                alternate_screen: view.alternate_screen,
                host_mouse_reporting: config.mouse_policy.enabled,
                cursor_style: config.cursor_style,
                cursor_blink: config.cursor_blink,
                cursor_blink_interval_ms: config.cursor_blink_interval_ms,
                cursor_blink_elapsed_ms,
                animation_refresh_interval_ms: view.animation_refresh_interval_ms,
                cursor_visible,
                cursor_row,
                cursor_column,
            },
        };
        let generation_base_ms = async_runtime_current_unix_millis();
        let mut timer_effects = self.cursor_blink_timer_side_effects_for_client(
            flush.client_id.as_str(),
            generation_base_ms,
        )?;
        timer_effects.extend(self.status_refresh_timer_side_effects_for_client(
            flush.client_id.as_str(),
            generation_base_ms,
        )?);
        self.queue_runtime_side_effects(timer_effects)?;
        Ok(Some(flush))
    }

    /// Runs the render side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn render_side_effects(
        &self,
        reason: RenderInvalidationReason,
    ) -> Vec<RuntimeSideEffect> {
        self.service
            .session()
            .clients()
            .iter()
            .filter(|client| client.state == ClientState::Attached)
            .map(|client| RuntimeSideEffect::RenderClient {
                client_id: client.id.clone(),
                reason,
            })
            .collect()
    }
}
