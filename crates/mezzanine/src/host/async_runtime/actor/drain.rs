//! Typed side-effect draining and client rendering.

use super::coalesce::{
    async_runtime_current_unix_millis, pane_io_side_effect_targets_instance,
    pane_io_side_effect_targets_pane,
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
        // Provider dispatches previously shared this queue and therefore
        // remained observable before later repaint work to generic test
        // adapters. Claim that dedicated route first so moving the provider
        // worker does not change the established compatibility order.
        let mut effects = self.side_effect_routes.drain_provider(limit);
        let drain_count = limit
            .saturating_sub(effects.len())
            .min(self.side_effects.len());
        effects.extend(self.side_effects.drain(..drain_count));
        effects.extend(
            self.side_effect_routes
                .drain_compat(limit.saturating_sub(effects.len())),
        );
        self.record_side_effect_drain(effects.len());
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
        self.metrics.side_effect_queue_depth = self
            .side_effects
            .len()
            .saturating_add(self.side_effect_routes.len());
        self.metrics
            .side_effect_queue_depth_samples
            .record(u64::try_from(self.metrics.side_effect_queue_depth).unwrap_or(u64::MAX));
        if let Some(nonempty_since) = self.side_effect_queue_nonempty_since
            && (drained > 0 || (self.side_effects.is_empty() && self.side_effect_routes.is_empty()))
        {
            self.metrics.record_phase_latency(
                crate::host::async_runtime::AsyncRuntimeLatencyPhase::SideEffectQueueAge,
                u64::try_from(nonempty_since.elapsed().as_millis()).unwrap_or(u64::MAX),
            );
        }
        if self.side_effects.is_empty() && self.side_effect_routes.is_empty() {
            self.side_effect_queue_nonempty_since = None;
        }
        if drained > 0 && (!self.side_effects.is_empty() || !self.side_effect_routes.is_empty()) {
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
        let drained = self.side_effect_routes.drain_provider(limit);
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Drains deferred interactive command dispatches from their dedicated lane.
    pub(super) fn drain_agent_command_dispatch_side_effects(
        &mut self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime command dispatch drain limit must be greater than zero",
            ));
        }
        let drained = self.side_effect_routes.drain_commands(limit);
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
        let drained = self.side_effect_routes.drain_renders(limit);
        self.record_side_effect_drain(drained.len());
        Ok(drained)
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
        let drained = if limit > 0 {
            self.side_effect_routes
                .drain_render_for_client(client_id)
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        self.record_side_effect_drain(drained.len());
        Ok(drained)
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
        let drained = self.side_effect_routes.drain_flushes(client_id, limit);
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
        let drained = self.side_effect_routes.drain_timers(limit);
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
        let drained = self.side_effect_routes.drain_persistence(limit);
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
        let drained = self.side_effect_routes.drain_hooks(limit);
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Drains bounded host-clipboard read work for the external worker.
    pub(super) fn drain_host_clipboard_side_effects(
        &mut self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime host clipboard side-effect drain limit must be greater than zero",
            ));
        }
        let drained = self.side_effect_routes.drain_clipboard(limit);
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Drains command-backed status-pill refresh work for the external worker.
    pub(super) fn drain_status_pill_side_effects(
        &mut self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime status pill side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained = self.side_effect_routes.drain_status(limit);
        let mut prepare_pane_providers = false;
        drained.retain(|effect| match effect {
            RuntimeSideEffect::PreparePaneStatusProviders => {
                prepare_pane_providers = true;
                false
            }
            _ => true,
        });
        if prepare_pane_providers || drained.len() < limit {
            let pane_limit = limit
                .saturating_sub(drained.len())
                .min(crate::runtime::MAX_CONCURRENT_PANE_STATUS_PROVIDERS);
            drained.extend(
                self.service
                    .prepare_pane_status_provider_refreshes(pane_limit)
                    .into_iter()
                    .map(|plan| RuntimeSideEffect::RefreshPaneStatusProvider {
                        plan: Box::new(plan),
                    }),
            );
        }
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
        // Legacy pane adapters do not carry a process generation, but test
        // adapters and retiring workers still use them. Make their drain see
        // all keyed generations for this pane while direct process workers
        // continue to claim only their exact FIFO.
        let mut routed = self
            .side_effect_routes
            .take_pane_processes_for_pane(pane_id);
        routed.append(&mut self.side_effects);
        while let Some(effect) = routed.pop_front() {
            if let RuntimeSideEffect::PaneProcessIo {
                instance: effect_instance,
                effect: crate::runtime::PaneProcessIoEffect::AcquireShellInputLease { owner_id },
            } = &effect
                && effect_instance.pane_id == pane_id
            {
                self.pane_input_leases
                    .entry(effect_instance.clone())
                    .or_insert_with(|| owner_id.clone());
                continue;
            }
            if let RuntimeSideEffect::PaneProcessIo {
                instance: effect_instance,
                effect: crate::runtime::PaneProcessIoEffect::ReleaseShellInputLease { owner_id },
            } = &effect
                && effect_instance.pane_id == pane_id
            {
                if self.pane_input_leases.get(effect_instance) == Some(owner_id) {
                    self.pane_input_leases.remove(effect_instance);
                }
                continue;
            }
            if drained.len() < limit && pane_io_side_effect_targets_pane(&effect, pane_id) {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        let mut legacy = VecDeque::with_capacity(retained.len());
        while let Some(effect) = retained.pop_front() {
            if matches!(effect, RuntimeSideEffect::PaneProcessIo { .. })
                && pane_io_side_effect_targets_pane(&effect, pane_id)
            {
                self.side_effect_routes.push_pane_process(effect);
            } else {
                legacy.push_back(effect);
            }
        }
        self.side_effects = legacy;
        self.record_side_effect_drain(drained.len());
        Ok(drained
            .into_iter()
            .map(|effect| match effect {
                RuntimeSideEffect::PaneProcessIo { instance, effect } => match effect {
                    crate::runtime::PaneProcessIoEffect::AcquireShellInputLease { .. }
                    | crate::runtime::PaneProcessIoEffect::ReleaseShellInputLease { .. } => {
                        unreachable!(
                            "pane input lease control effects are consumed by actor arbitration"
                        )
                    }
                    crate::runtime::PaneProcessIoEffect::WriteInput { bytes } => {
                        RuntimeSideEffect::WritePaneInput {
                            pane_id: instance.pane_id,
                            bytes,
                        }
                    }
                    crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery } => {
                        RuntimeSideEffect::WritePaneShellInput {
                            pane_id: instance.pane_id,
                            delivery,
                        }
                    }
                    crate::runtime::PaneProcessIoEffect::CancelShellInput { delivery_id } => {
                        RuntimeSideEffect::PaneProcessIo {
                            instance,
                            effect: crate::runtime::PaneProcessIoEffect::CancelShellInput {
                                delivery_id,
                            },
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
                    crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess {
                        observation_id,
                        expected_process_group_id,
                    } => RuntimeSideEffect::PaneProcessIo {
                        instance,
                        effect: crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess {
                            observation_id,
                            expected_process_group_id,
                        },
                    },
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
        let mut retained = VecDeque::new();
        let mut queued = self.side_effect_routes.take_pane_process(instance);
        while let Some(effect) = queued.pop_front() {
            if let RuntimeSideEffect::PaneProcessIo {
                instance: effect_instance,
                effect: crate::runtime::PaneProcessIoEffect::AcquireShellInputLease { owner_id },
            } = &effect
                && effect_instance == instance
            {
                self.pane_input_leases
                    .entry(effect_instance.clone())
                    .or_insert_with(|| owner_id.clone());
                continue;
            }
            if let RuntimeSideEffect::PaneProcessIo {
                instance: effect_instance,
                effect: crate::runtime::PaneProcessIoEffect::ReleaseShellInputLease { owner_id },
            } = &effect
                && effect_instance == instance
            {
                if self.pane_input_leases.get(effect_instance) == Some(owner_id) {
                    self.pane_input_leases.remove(effect_instance);
                }
                continue;
            }
            let lease_allows_effect = match (&effect, self.pane_input_leases.get(instance)) {
                (
                    RuntimeSideEffect::PaneProcessIo {
                        effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
                        ..
                    },
                    Some(owner_id),
                ) => delivery.delivery_id.as_deref() == Some(owner_id.as_str()),
                (
                    RuntimeSideEffect::PaneProcessIo {
                        effect: crate::runtime::PaneProcessIoEffect::WriteInputPriority { .. },
                        ..
                    },
                    Some(_),
                ) => true,
                (
                    RuntimeSideEffect::PaneProcessIo {
                        effect: crate::runtime::PaneProcessIoEffect::Terminate { .. },
                        ..
                    },
                    Some(_),
                ) => true,
                (
                    RuntimeSideEffect::PaneProcessIo {
                        effect: crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess { .. },
                        ..
                    },
                    Some(_),
                ) => true,
                (
                    RuntimeSideEffect::PaneProcessIo {
                        effect:
                            crate::runtime::PaneProcessIoEffect::CancelShellInput { delivery_id },
                        ..
                    },
                    Some(owner_id),
                ) => delivery_id == owner_id,
                (_, Some(_)) => false,
                (_, None) => true,
            };
            if !lease_allows_effect {
                retained.push_back(effect);
                continue;
            }
            if drained.len() < limit && pane_io_side_effect_targets_instance(&effect, instance) {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effect_routes
            .restore_pane_process(instance.clone(), retained);
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
        reason: RenderInvalidationReason,
        config: TerminalClientLoopConfig,
        status: Option<ClientStatusLine>,
        cursor_blink_elapsed_ms: u64,
    ) -> Result<Option<AsyncRenderedClientFlush>> {
        let Some(client_size) = self.attached_client_size(&client_id)? else {
            return Ok(None);
        };
        let role = match self
            .service
            .session()
            .clients()
            .iter()
            .find(|client| client.id == client_id)
            .map(|client| client.role)
        {
            Some(mez_mux::session::ClientRole::Primary) => ClientViewRole::Primary,
            Some(mez_mux::session::ClientRole::Observer) => ClientViewRole::Observer,
            _ => return Ok(None),
        };
        self.service.prepare_client_render(&client_id, role)?;
        let config = self.service.terminal_client_loop_config(config)?;
        let render_token = self.client_render_token(&client_id, role)?;
        if reason == RenderInvalidationReason::PaneOutput
            && let Some(render_token) = render_token.as_ref()
            && self
                .rendered_client_side_effects
                .get(&client_id)
                .is_some_and(|(previous, previous_config, previous_status)| {
                    previous == render_token
                        && previous_config == &config
                        && previous_status == &status
                })
        {
            self.metrics.render_compositions_skipped =
                self.metrics.render_compositions_skipped.saturating_add(1);
            return Ok(None);
        }
        let composition_started = std::time::Instant::now();
        let (view, presentation_ids) = self
            .service
            .render_client_view_for_client_with_resolved_config_and_receipts(
                &client_id,
                role,
                client_size,
                &config,
            )?;
        self.metrics.record_phase_latency(
            crate::host::async_runtime::AsyncRuntimeLatencyPhase::RenderComposition,
            u64::try_from(composition_started.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
        let Some(view) = view else {
            return Ok(None);
        };
        if let Some(render_token) = render_token {
            self.rendered_client_side_effects.insert(
                client_id.clone(),
                (render_token, config.clone(), status.clone()),
            );
        }
        let status_pill_effects = self
            .service
            .drain_status_pill_refresh_transition()
            .side_effects;
        if !status_pill_effects.is_empty() {
            self.queue_runtime_side_effects(status_pill_effects)?;
        }
        let cursor_visible = view.cursor_visible;
        let cursor_row = view.cursor_row;
        let cursor_column = view.cursor_column;
        let application_keypad = view.application_keypad;
        let bracketed_paste = view.bracketed_paste;
        let encoding_started = std::time::Instant::now();
        let (lines, line_style_spans) =
            compose_client_presentation_with_styles(&view, status.as_ref());
        self.metrics.record_phase_latency(
            crate::host::async_runtime::AsyncRuntimeLatencyPhase::RenderEncoding,
            u64::try_from(encoding_started.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
        let flush = AsyncRenderedClientFlush {
            client_id,
            presentation_ids,
            lines,
            line_style_spans,
            modes: AttachedTerminalOutputModes {
                application_keypad,
                enhanced_keyboard_reporting: view
                    .enhanced_keyboard_reporting_active(config.enhanced_keyboard_reporting),
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
