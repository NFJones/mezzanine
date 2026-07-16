//! Timer tracking and bounded side-effect queue maintenance.

use super::coalesce::{
    async_runtime_current_unix_millis, async_runtime_duration_millis,
    coalesce_output_side_effects_for_enqueue, pane_io_side_effect_targets_pane,
    runtime_side_effect_kind_summary,
};
use super::{
    AsyncRuntimeSessionActor, ClientId, DEFAULT_ASYNC_IDLE_CLEANUP_INTERVAL,
    DEFAULT_PANE_PIPE_HEALTH_DELAY_MS, DEFAULT_SHELL_RECOVERY_INTERVAL_MS, MezError, Result,
    RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind,
};

impl AsyncRuntimeSessionActor {
    /// Applies timer scheduling bookkeeping in emitted side-effect order.
    ///
    /// A cancellation followed by a schedule for the same generation must
    /// leave that timer active, while a schedule followed by cancellation must
    /// remove it. Keeping this ordering here mirrors the timer worker contract.
    pub(super) fn track_runtime_timer_side_effect(&mut self, effect: &RuntimeSideEffect) {
        let (key, scheduled) = match effect {
            RuntimeSideEffect::ScheduleTimer { key, .. } => (key, true),
            RuntimeSideEffect::CancelTimer { key } => (key, false),
            _ => return,
        };
        match key.kind {
            RuntimeTimerKind::ShellTransaction
            | RuntimeTimerKind::ReadinessProbe
            | RuntimeTimerKind::Bootstrap
            | RuntimeTimerKind::FocusedShellHook => {
                if scheduled {
                    self.timers.shell_transactions.insert(key.clone());
                } else {
                    self.timers.shell_transactions.remove(key);
                }
            }
            RuntimeTimerKind::IdleCleanup => {
                if scheduled {
                    self.timers.idle_cleanup = Some(key.clone());
                } else if self.timers.idle_cleanup.as_ref() == Some(key) {
                    self.timers.idle_cleanup = None;
                }
            }
            RuntimeTimerKind::ResizeDebounce => {
                if scheduled {
                    self.timers.resize_debounce.insert(key.clone());
                } else {
                    self.timers.resize_debounce.remove(key);
                }
            }
            RuntimeTimerKind::CursorBlink => {
                Self::track_owned_timer_key(&mut self.timers.cursor_blink, key, scheduled);
            }
            RuntimeTimerKind::StatusRefresh => {
                Self::track_owned_timer_key(&mut self.timers.status_refresh, key, scheduled);
            }
            RuntimeTimerKind::ProviderPoll => {
                if scheduled {
                    self.timers.provider_poll = Some(key.clone());
                } else if self.timers.provider_poll.as_ref() == Some(key) {
                    self.timers.provider_poll = None;
                }
            }
            RuntimeTimerKind::ProviderRetry => {
                Self::track_owned_timer_key(&mut self.timers.provider_retry, key, scheduled);
            }
            RuntimeTimerKind::ProviderClaim => {
                Self::track_owned_timer_key(&mut self.timers.provider_claim, key, scheduled);
            }
            RuntimeTimerKind::PanePipeHealth => {
                Self::track_owned_timer_key(&mut self.timers.pane_pipe_health, key, scheduled);
            }
        }
    }

    /// Updates one owner-keyed timer generation without discarding effect order.
    pub(super) fn track_owned_timer_key(
        timers: &mut std::collections::HashMap<String, RuntimeTimerKey>,
        key: &RuntimeTimerKey,
        scheduled: bool,
    ) {
        if scheduled {
            timers.insert(key.owner_id.clone(), key.clone());
        } else if timers.get(key.owner_id.as_str()) == Some(key) {
            timers.remove(key.owner_id.as_str());
        }
    }

    /// Runs the queue runtime side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn queue_runtime_side_effects(
        &mut self,
        side_effects: Vec<RuntimeSideEffect>,
    ) -> Result<()> {
        let (side_effects, coalesced) =
            coalesce_output_side_effects_for_enqueue(&mut self.side_effects, side_effects);
        if self.side_effects.len().saturating_add(side_effects.len()) > self.side_effect_buffer {
            return Err(MezError::invalid_state(format!(
                "async runtime side-effect queue is full: queued={} incoming={} capacity={} queued_kinds={} incoming_kinds={}",
                self.side_effects.len(),
                side_effects.len(),
                self.side_effect_buffer,
                runtime_side_effect_kind_summary(self.side_effects.iter()),
                runtime_side_effect_kind_summary(side_effects.iter())
            )));
        }
        let timer_schedules = side_effects
            .iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::ScheduleTimer { .. }))
            .count();
        let timer_cancellations = side_effects
            .iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::CancelTimer { .. }))
            .count();
        let queued = side_effects.len();
        let should_notify = !side_effects.is_empty();
        for effect in &side_effects {
            self.track_runtime_timer_side_effect(effect);
        }
        for effect in side_effects {
            self.enqueue_runtime_side_effect(effect);
        }
        if should_notify {
            self.metrics.runtime_side_effects_queued = self
                .metrics
                .runtime_side_effects_queued
                .saturating_add(u64::try_from(queued).unwrap_or(u64::MAX));
            self.metrics.runtime_timer_schedules_queued = self
                .metrics
                .runtime_timer_schedules_queued
                .saturating_add(u64::try_from(timer_schedules).unwrap_or(u64::MAX));
            self.metrics.runtime_timer_cancellations_queued = self
                .metrics
                .runtime_timer_cancellations_queued
                .saturating_add(u64::try_from(timer_cancellations).unwrap_or(u64::MAX));
        }
        self.metrics
            .runtime_side_effect_enqueue_sizes
            .record(u64::try_from(queued).unwrap_or(u64::MAX));
        self.metrics.render_invalidations_coalesced = self
            .metrics
            .render_invalidations_coalesced
            .saturating_add(u64::try_from(coalesced).unwrap_or(u64::MAX));
        self.metrics.side_effect_queue_depth = self.side_effects.len();
        self.metrics.side_effect_queue_high_water = self
            .metrics
            .side_effect_queue_high_water
            .max(self.side_effects.len());
        self.metrics
            .side_effect_queue_depth_samples
            .record(u64::try_from(self.side_effects.len()).unwrap_or(u64::MAX));
        if should_notify {
            self.notify_side_effect_delivery();
        }
        Ok(())
    }

    /// Enqueues one runtime side effect, preserving priority pane input order.
    pub(super) fn enqueue_runtime_side_effect(&mut self, effect: RuntimeSideEffect) {
        match &effect {
            RuntimeSideEffect::WritePaneInputPriority { pane_id, .. } => {
                let insert_at = self
                    .side_effects
                    .iter()
                    .position(|queued| pane_io_side_effect_targets_pane(queued, pane_id))
                    .unwrap_or(self.side_effects.len());
                self.side_effects.insert(insert_at, effect);
            }
            _ => self.side_effects.push_back(effect),
        }
    }

    /// Runs the queue shell transaction timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn queue_shell_transaction_timer_side_effects(&mut self) -> Result<usize> {
        let side_effects = self.shell_transaction_timer_side_effects();
        let queued = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(queued)
    }

    /// Runs the ensure client render timers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn ensure_client_render_timers(&mut self, client_id: &ClientId) -> Result<usize> {
        let generation_base_ms = async_runtime_current_unix_millis();
        let mut side_effects = self
            .service
            .client_cursor_blink_timer_transition(
                client_id.as_str(),
                self.timers.cursor_blink.get(client_id.as_str()).cloned(),
                generation_base_ms,
            )?
            .side_effects;
        side_effects.extend(
            self.service
                .client_status_refresh_timer_transition(
                    client_id.as_str(),
                    self.timers.status_refresh.get(client_id.as_str()).cloned(),
                    generation_base_ms,
                )?
                .side_effects,
        );
        let queued = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(queued)
    }

    /// Runs the shell transaction timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn shell_transaction_timer_side_effects(&mut self) -> Vec<RuntimeSideEffect> {
        self.service
            .shell_transaction_timer_transition(
                &self.timers.shell_transactions,
                async_runtime_current_unix_millis(),
            )
            .side_effects
            .into_iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::ScheduleTimer { .. }))
            .collect()
    }

    /// Runs the cancel stale shell transaction timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn cancel_stale_shell_transaction_timer_side_effects(
        &mut self,
    ) -> Vec<RuntimeSideEffect> {
        self.service
            .shell_transaction_timer_transition(
                &self.timers.shell_transactions,
                async_runtime_current_unix_millis(),
            )
            .side_effects
            .into_iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::CancelTimer { .. }))
            .collect()
    }

    /// Runs the idle cleanup timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn idle_cleanup_timer_side_effects(&self) -> Vec<RuntimeSideEffect> {
        let actor_progress_turn_ids = self.actor_owned_agent_progress_turn_ids();
        self.idle_cleanup_timer_side_effects_with_actor_progress(&actor_progress_turn_ids)
    }

    /// Returns idle-cleanup timer side effects while honoring actor-owned
    /// progress such as delayed provider retry timers.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns whose progress is represented
    ///   by async actor state rather than service-owned queues.
    pub(super) fn idle_cleanup_timer_side_effects_with_actor_progress(
        &self,
        actor_progress_turn_ids: &std::collections::BTreeSet<String>,
    ) -> Vec<RuntimeSideEffect> {
        self.service
            .idle_cleanup_timer_transition_with_actor_progress(
                actor_progress_turn_ids,
                self.timers.idle_cleanup.is_some(),
                async_runtime_current_unix_millis(),
                async_runtime_duration_millis(DEFAULT_ASYNC_IDLE_CLEANUP_INTERVAL),
                DEFAULT_SHELL_RECOVERY_INTERVAL_MS,
            )
            .side_effects
    }

    /// Runs the cursor blink timer side effects for client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn cursor_blink_timer_side_effects_for_client(
        &mut self,
        client_id: &str,
        generation_base_ms: u64,
    ) -> Result<Vec<RuntimeSideEffect>> {
        Ok(self
            .service
            .client_cursor_blink_timer_transition(
                client_id,
                self.timers.cursor_blink.get(client_id).cloned(),
                generation_base_ms,
            )?
            .side_effects)
    }

    /// Runs the status refresh timer side effects for client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn status_refresh_timer_side_effects_for_client(
        &mut self,
        client_id: &str,
        generation_base_ms: u64,
    ) -> Result<Vec<RuntimeSideEffect>> {
        Ok(self
            .service
            .client_status_refresh_timer_transition(
                client_id,
                self.timers.status_refresh.get(client_id).cloned(),
                generation_base_ms,
            )?
            .side_effects)
    }

    /// Runs the pane pipe health timer side effects for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn pane_pipe_health_timer_side_effects_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Result<Vec<RuntimeSideEffect>> {
        let next_generation = self
            .timers
            .next_pane_pipe_health_generation
            .saturating_add(1);
        let transition = self.service.pane_pipe_health_timer_transition(
            pane_id,
            self.timers.pane_pipe_health.get(pane_id).cloned(),
            next_generation,
            DEFAULT_PANE_PIPE_HEALTH_DELAY_MS,
        )?;
        if transition.side_effects.iter().any(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::PanePipeHealth
            )
        }) {
            self.timers.next_pane_pipe_health_generation = next_generation;
        }
        Ok(transition.side_effects)
    }

    /// Runs the command pane pipe health timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn command_pane_pipe_health_timer_side_effects(
        &mut self,
    ) -> Result<Vec<RuntimeSideEffect>> {
        let pane_ids = self.service.active_command_pane_pipe_ids();
        let mut side_effects = Vec::new();
        for pane_id in pane_ids {
            side_effects.extend(self.pane_pipe_health_timer_side_effects_for_pane(&pane_id)?);
        }
        Ok(side_effects)
    }

    /// Runs the queue command pane pipe health timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn queue_command_pane_pipe_health_timer_side_effects(&mut self) -> Result<usize> {
        let side_effects = self.command_pane_pipe_health_timer_side_effects()?;
        let count = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(count)
    }
}
