//! Runtime event application and lifecycle notification.

use super::coalesce::{
    provider_retry_timer_side_effect_turn_id, runtime_event_requires_registry_persistence,
    runtime_timer_kind_is_shell_transaction, side_effects_include_registry_persistence,
};
use super::{
    AgentId, AgentProviderEvent, AsyncHookEvent, AsyncRuntimeSessionActor, ClientEvent, ClientId,
    ClientState, MezError, PaneEvent, PersistenceEvent, ProviderErrorRetryClass,
    RenderInvalidationReason, Result, RuntimeEvent, RuntimeEventBatch, RuntimeEventIngressReport,
    RuntimeLifecycleState, RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition,
    ShutdownEvent, Size, TimerEvent, provider_error_retry_class_from_parts,
    provider_event_error_from_parts, provider_event_error_kind,
};

impl AsyncRuntimeSessionActor {
    /// Runs the notify message delivery operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn notify_message_delivery(&mut self) {
        self.metrics.message_delivery_notifications = self
            .metrics
            .message_delivery_notifications
            .saturating_add(1);
        self.message_delivery_notify.notify_waiters();
    }

    /// Runs the notify event delivery operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn notify_event_delivery(&mut self) {
        self.metrics.event_delivery_notifications =
            self.metrics.event_delivery_notifications.saturating_add(1);
        self.event_delivery_notify.notify_waiters();
        self.event_delivery_notify.notify_one();
    }

    /// Runs the notify side effect delivery operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn notify_side_effect_delivery(&mut self) {
        self.metrics.side_effect_delivery_notifications = self
            .metrics
            .side_effect_delivery_notifications
            .saturating_add(1);
        let _ = self
            .side_effect_delivery_tx
            .send(self.metrics.side_effect_delivery_notifications);
        self.side_effect_delivery_notify.notify_waiters();
        self.side_effect_delivery_notify.notify_one();
    }

    /// Runs the notify lifecycle state if changed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn notify_lifecycle_state_if_changed(&mut self, previous: RuntimeLifecycleState) {
        let current = self.service.lifecycle_state();
        if current == previous {
            return;
        }
        self.metrics.lifecycle_state_notifications =
            self.metrics.lifecycle_state_notifications.saturating_add(1);
        let _ = self.lifecycle_state_tx.send(current);
    }

    /// Runs the apply runtime event batch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn apply_runtime_event_batch(
        &mut self,
        batch: RuntimeEventBatch,
    ) -> Result<RuntimeEventIngressReport> {
        let mut report = batch.ingress_report();
        let mut registry_persistence_queued = false;
        let mut registry_persistence_required = false;
        for event in batch.prioritized_events() {
            let event_requires_registry_persistence =
                runtime_event_requires_registry_persistence(&event);
            let mut application = self.apply_runtime_event(event).await?;
            if application.applied {
                registry_persistence_required =
                    registry_persistence_required || event_requires_registry_persistence;
                self.service.maybe_bootstrap_ready_panes()?;
                let mut actor_progress_turn_ids = self.actor_owned_agent_progress_turn_ids();
                actor_progress_turn_ids.extend(
                    application
                        .side_effects
                        .iter()
                        .filter_map(provider_retry_timer_side_effect_turn_id),
                );
                let reconciled = self
                    .service
                    .reconcile_agent_runtime_progress_paths_with_actor_progress(
                        &actor_progress_turn_ids,
                    )?;
                if reconciled > 0 {
                    application
                        .side_effects
                        .extend(self.render_side_effects(RenderInvalidationReason::FullRedraw));
                    application
                        .side_effects
                        .extend(self.pending_provider_dispatch_side_effects()?);
                }
                application
                    .side_effects
                    .extend(self.deferred_service_side_effects_from_service());
                application
                    .side_effects
                    .extend(self.cancel_stale_shell_transaction_timer_side_effects());
                application
                    .side_effects
                    .extend(self.shell_transaction_timer_side_effects());
                application
                    .side_effects
                    .extend(self.idle_cleanup_timer_side_effects());
            } else {
                application
                    .side_effects
                    .extend(self.deferred_service_side_effects_from_service());
            }
            registry_persistence_queued = registry_persistence_queued
                || side_effects_include_registry_persistence(&application.side_effects);
            report.side_effects = report
                .side_effects
                .saturating_add(application.side_effects.len());
            self.queue_runtime_side_effects(application.side_effects)?;
            if application.applied {
                report.applied = report.applied.saturating_add(1);
            }
        }
        if report.applied > 0
            && registry_persistence_required
            && report
                .families
                .iter()
                .any(|family| family.as_str() != "persistence")
            && !registry_persistence_queued
            && let Some((registry, update)) = self.service.registry_update_for_async_persistence()
        {
            self.queue_runtime_side_effects(vec![RuntimeSideEffect::PersistRegistry {
                registry,
                update,
            }])?;
            report.side_effects = report.side_effects.saturating_add(1);
        }
        self.metrics.runtime_event_batches = self.metrics.runtime_event_batches.saturating_add(1);
        self.metrics.runtime_events_accepted = self
            .metrics
            .runtime_events_accepted
            .saturating_add(u64::try_from(report.accepted).unwrap_or(u64::MAX));
        self.metrics.runtime_events_applied = self
            .metrics
            .runtime_events_applied
            .saturating_add(u64::try_from(report.applied).unwrap_or(u64::MAX));
        self.metrics
            .runtime_event_batch_sizes
            .record(u64::try_from(report.accepted).unwrap_or(u64::MAX));
        Ok(report)
    }

    /// Runs the apply runtime event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn apply_runtime_event(
        &mut self,
        event: RuntimeEvent,
    ) -> Result<RuntimeTransition> {
        match event {
            RuntimeEvent::Pane(PaneEvent::Output { pane_id, bytes }) => {
                let byte_count = bytes.len();
                let pane_id_for_pipe_health = pane_id.clone();
                let mut transition = self.service.apply_pane_output_transition(pane_id, bytes)?;
                if transition.applied {
                    self.metrics.pane_output_chunks =
                        self.metrics.pane_output_chunks.saturating_add(1);
                    self.metrics.pane_output_bytes = self
                        .metrics
                        .pane_output_bytes
                        .saturating_add(u64::try_from(byte_count).unwrap_or(u64::MAX));
                    self.metrics
                        .pane_output_chunk_bytes
                        .record(u64::try_from(byte_count).unwrap_or(u64::MAX));
                }
                if transition.applied {
                    transition.side_effects.extend(
                        self.pane_pipe_health_timer_side_effects_for_pane(
                            &pane_id_for_pipe_health,
                        )?,
                    );
                }
                Ok(transition)
            }
            RuntimeEvent::Pane(pane_event) => {
                self.service.apply_pane_completion_transition(pane_event)
            }
            RuntimeEvent::Client(client_event) => self.apply_runtime_client_event(client_event),
            RuntimeEvent::AgentProvider(provider_event) => {
                self.apply_runtime_agent_provider_event(provider_event)
                    .await
            }
            RuntimeEvent::AgentCompaction(compaction_event) => {
                let mut transition = self
                    .service
                    .apply_agent_compaction_transition(compaction_event)?;
                if transition.applied {
                    transition
                        .side_effects
                        .extend(self.pending_provider_dispatch_side_effects()?);
                }
                Ok(transition)
            }
            RuntimeEvent::AgentRemember(remember_event) => {
                self.service.apply_agent_remember_transition(remember_event)
            }
            RuntimeEvent::Hook(hook_event) => self.apply_runtime_hook_event(hook_event),
            RuntimeEvent::Persistence(persistence_event) => {
                self.apply_runtime_persistence_event(persistence_event)
            }
            RuntimeEvent::Shutdown(shutdown) => self.apply_runtime_shutdown_event(shutdown),
            RuntimeEvent::Timer(timer) => self.apply_runtime_timer_event(timer),
            RuntimeEvent::Process(process_event) => {
                self.service.apply_process_transition(process_event)
            }
        }
    }

    /// Runs the apply runtime timer event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_runtime_timer_event(
        &mut self,
        timer: TimerEvent,
    ) -> Result<RuntimeTransition> {
        if runtime_timer_kind_is_shell_transaction(timer.key.kind) {
            self.timers.shell_transactions.remove(&timer.key);
        }
        match timer.key.kind {
            RuntimeTimerKind::ShellTransaction
            | RuntimeTimerKind::ReadinessProbe
            | RuntimeTimerKind::Bootstrap
            | RuntimeTimerKind::FocusedShellHook => {
                let mut transition = self
                    .service
                    .apply_shell_transaction_timer_transition(timer.now_ms)?;
                if !transition.applied {
                    transition
                        .side_effects
                        .extend(self.shell_transaction_timer_side_effects());
                }
                Ok(transition)
            }
            RuntimeTimerKind::ResizeDebounce => {
                let active = self.timers.resize_debounce.remove(&timer.key);
                if !active {
                    self.record_ignored_timer_event();
                }
                Ok(self.service.apply_resize_debounce_timer_transition(active))
            }
            RuntimeTimerKind::CursorBlink => {
                if self.timers.cursor_blink.get(timer.key.owner_id.as_str()) != Some(&timer.key) {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers.cursor_blink.remove(timer.key.owner_id.as_str());
                let mut application =
                    self.apply_render_timer_event(RenderInvalidationReason::CursorBlink);
                application
                    .side_effects
                    .extend(self.cursor_blink_timer_side_effects_for_client(
                        &timer.key.owner_id,
                        timer.now_ms,
                    )?);
                Ok(application)
            }
            RuntimeTimerKind::StatusRefresh => {
                if self.timers.status_refresh.get(timer.key.owner_id.as_str()) != Some(&timer.key) {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers
                    .status_refresh
                    .remove(timer.key.owner_id.as_str());
                let mut application =
                    self.apply_render_timer_event(RenderInvalidationReason::StatusLine);
                application.side_effects.extend(
                    self.status_refresh_timer_side_effects_for_client(
                        &timer.key.owner_id,
                        timer.now_ms,
                    )?,
                );
                Ok(application)
            }
            RuntimeTimerKind::IdleCleanup => {
                if self.timers.idle_cleanup.as_ref() != Some(&timer.key) {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers.idle_cleanup = None;
                let actor_progress_turn_ids = self.actor_owned_agent_progress_turn_ids();
                let cleaned = self
                    .service
                    .apply_idle_cleanup_timer_event_with_actor_progress(&actor_progress_turn_ids)?;
                let mut side_effects = self
                    .idle_cleanup_timer_side_effects_with_actor_progress(&actor_progress_turn_ids);
                if cleaned > 0 {
                    side_effects.extend(self.pending_provider_dispatch_side_effects()?);
                    side_effects
                        .extend(self.render_side_effects(RenderInvalidationReason::FullRedraw));
                }
                Ok(RuntimeTransition {
                    applied: cleaned > 0,
                    side_effects,
                })
            }
            RuntimeTimerKind::ProviderPoll => {
                if self.timers.provider_poll.as_ref() != Some(&timer.key) {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers.provider_poll = None;
                self.apply_provider_poll_timer_event()
            }
            RuntimeTimerKind::ProviderRetry => {
                if self.timers.provider_retry.get(timer.key.owner_id.as_str()) != Some(&timer.key) {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers
                    .provider_retry
                    .remove(timer.key.owner_id.as_str());
                self.apply_provider_retry_timer_event(&timer.key)
            }
            RuntimeTimerKind::ProviderClaim => {
                if self.timers.provider_claim.get(timer.key.owner_id.as_str()) != Some(&timer.key) {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers
                    .provider_claim
                    .remove(timer.key.owner_id.as_str());
                self.apply_provider_claim_timer_event(&timer.key)
            }
            RuntimeTimerKind::PanePipeHealth => {
                if self
                    .timers
                    .pane_pipe_health
                    .get(timer.key.owner_id.as_str())
                    != Some(&timer.key)
                {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers
                    .pane_pipe_health
                    .remove(timer.key.owner_id.as_str());
                let stopped = self
                    .service
                    .stop_completed_command_pane_pipe_for(&timer.key.owner_id)?;
                let mut side_effects = if stopped > 0 {
                    self.render_side_effects(RenderInvalidationReason::FullRedraw)
                } else {
                    Vec::new()
                };
                if stopped == 0 {
                    side_effects.extend(
                        self.pane_pipe_health_timer_side_effects_for_pane(&timer.key.owner_id)?,
                    );
                }
                Ok(RuntimeTransition {
                    applied: stopped > 0,
                    side_effects,
                })
            }
        }
    }

    /// Runs the record ignored timer event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn record_ignored_timer_event(&mut self) {
        self.metrics.runtime_timer_events_ignored =
            self.metrics.runtime_timer_events_ignored.saturating_add(1);
    }

    /// Runs the apply render timer event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_render_timer_event(
        &self,
        reason: RenderInvalidationReason,
    ) -> RuntimeTransition {
        let side_effects = self.render_side_effects(reason);
        RuntimeTransition {
            applied: !side_effects.is_empty(),
            side_effects,
        }
    }

    /// Runs the apply provider poll timer event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_provider_poll_timer_event(&self) -> Result<RuntimeTransition> {
        let side_effects = self.pending_provider_dispatch_side_effects()?;
        Ok(RuntimeTransition {
            applied: !side_effects.is_empty(),
            side_effects,
        })
    }

    /// Runs the apply provider retry timer event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_provider_retry_timer_event(
        &mut self,
        key: &RuntimeTimerKey,
    ) -> Result<RuntimeTransition> {
        self.service
            .apply_agent_provider_retry_timer_transition(&key.owner_id, key.generation)
    }

    /// Applies the timeout for an async provider worker claim lease.
    pub(super) fn apply_provider_claim_timer_event(
        &mut self,
        key: &RuntimeTimerKey,
    ) -> Result<RuntimeTransition> {
        let applied = self
            .service
            .fail_expired_claimed_agent_provider_task(&key.owner_id, key.generation)?;
        let mut side_effects = if applied {
            self.pending_provider_dispatch_side_effects()?
        } else {
            Vec::new()
        };
        if applied {
            side_effects.extend(self.render_side_effects(RenderInvalidationReason::FullRedraw));
        }
        Ok(RuntimeTransition {
            applied,
            side_effects,
        })
    }

    /// Removes and returns a timer cancellation for a claimed provider task.
    pub(super) fn provider_claim_cancel_timer_side_effects(
        &mut self,
        turn_id: &str,
    ) -> Vec<RuntimeSideEffect> {
        self.timers
            .provider_claim
            .remove(turn_id)
            .map(|key| RuntimeSideEffect::CancelTimer { key })
            .into_iter()
            .collect()
    }

    /// Runs the pending provider dispatch side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn pending_provider_dispatch_side_effects(&self) -> Result<Vec<RuntimeSideEffect>> {
        let mut side_effects = Vec::new();
        for task in self.service.pending_agent_provider_tasks() {
            if self.provider_dispatch_is_already_queued(&task.turn_id) {
                continue;
            }
            let Some(agent_id) = AgentId::opaque(task.agent_id.clone()) else {
                return Err(MezError::invalid_state(format!(
                    "runtime provider task has invalid agent id `{}`",
                    task.agent_id
                )));
            };
            side_effects.push(RuntimeSideEffect::DispatchAgentProvider {
                agent_id,
                turn_id: task.turn_id,
            });
        }
        for pane_id in self.service.pending_agent_compaction_tasks() {
            if self.compaction_dispatch_is_already_queued(&pane_id) {
                continue;
            }
            side_effects.push(RuntimeSideEffect::DispatchAgentCompaction { pane_id });
        }
        for pane_id in self.service.pending_agent_remember_tasks() {
            if self.remember_dispatch_is_already_queued(&pane_id) {
                continue;
            }
            side_effects.push(RuntimeSideEffect::DispatchAgentRemember { pane_id });
        }
        Ok(side_effects)
    }

    /// Returns true when a queued compaction dispatch already exists for a pane.
    pub(super) fn compaction_dispatch_is_already_queued(&self, target_pane_id: &str) -> bool {
        self.side_effects.iter().any(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::DispatchAgentCompaction { pane_id }
                    if pane_id == target_pane_id
            )
        })
    }

    /// Returns true when a queued durable memory dispatch already exists for a pane.
    pub(super) fn remember_dispatch_is_already_queued(&self, target_pane_id: &str) -> bool {
        self.side_effects.iter().any(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::DispatchAgentRemember { pane_id } if pane_id == target_pane_id
            )
        })
    }

    /// Returns agent turns whose progress currently depends on actor-owned
    /// scheduling state instead of service-owned runtime state.
    ///
    /// Provider retry timers and automatic output-limit compaction dispatch are
    /// intentionally held outside ordinary service-owned provider tasks, so
    /// service-level reconciliation must be told that these turns still have a
    /// valid path to progress.
    pub(super) fn actor_owned_agent_progress_turn_ids(&self) -> std::collections::BTreeSet<String> {
        let mut turn_ids: std::collections::BTreeSet<String> = self
            .service
            .agent_provider_retry_turn_ids()
            .chain(self.timers.provider_retry.keys())
            .cloned()
            .collect();
        turn_ids.extend(self.service.agent_compaction_resume_turn_ids());
        turn_ids
    }

    /// Runs the queue pending provider dispatch side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn queue_pending_provider_dispatch_side_effects(&mut self) -> Result<usize> {
        let side_effects = self.pending_provider_dispatch_side_effects()?;
        let count = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(count)
    }

    /// Runs the queue deferred pane io side effects from service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn queue_deferred_pane_io_side_effects_from_service(&mut self) -> Result<usize> {
        let side_effects = self.deferred_service_side_effects_from_service();
        let count = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(count)
    }

    /// Runs the deferred service side effects from service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn deferred_service_side_effects_from_service(&mut self) -> Vec<RuntimeSideEffect> {
        self.service
            .drain_deferred_effects_transition()
            .side_effects
    }

    /// Runs the deferred pane io side effects from service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// Runs the queue provider poll timer if needed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn queue_provider_poll_timer_if_needed(
        &mut self,
        generation: u64,
        delay_ms: u64,
    ) -> Result<bool> {
        let transition = self.service.provider_poll_timer_transition(
            self.timers.provider_poll.is_some(),
            generation,
            delay_ms,
        );
        let queued = !transition.side_effects.is_empty();
        self.queue_runtime_side_effects(transition.side_effects)?;
        Ok(queued)
    }

    /// Runs the provider dispatch is already queued operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn provider_dispatch_is_already_queued(&self, turn_id: &str) -> bool {
        self.side_effects.iter().any(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::DispatchAgentProvider {
                    turn_id: queued_turn_id,
                    ..
                } if queued_turn_id == turn_id
            )
        })
    }

    /// Runs the apply runtime client event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_runtime_client_event(
        &mut self,
        client_event: ClientEvent,
    ) -> Result<RuntimeTransition> {
        match client_event {
            event @ (ClientEvent::Resize { .. } | ClientEvent::Disconnected { .. }) => {
                self.service.apply_client_lifecycle_transition(event)
            }
            ClientEvent::ResizeSignal { client_id } => Ok(self
                .apply_runtime_client_render_signal_event(
                    client_id,
                    RenderInvalidationReason::Resize,
                )),
            ClientEvent::OutputReady { client_id } => Ok(self
                .apply_runtime_client_render_signal_event(
                    client_id,
                    RenderInvalidationReason::FullRedraw,
                )),
            ClientEvent::Input { client_id, bytes } => self
                .service
                .apply_client_input_transition(&client_id, &bytes),
        }
    }

    /// Runs the apply runtime client render signal event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_runtime_client_render_signal_event(
        &self,
        client_id: ClientId,
        reason: RenderInvalidationReason,
    ) -> RuntimeTransition {
        if !self
            .service
            .session()
            .clients()
            .iter()
            .any(|client| client.id == client_id && client.state == ClientState::Attached)
        {
            return RuntimeTransition::default();
        }
        RuntimeTransition {
            applied: true,
            side_effects: vec![RuntimeSideEffect::RenderClient { client_id, reason }],
        }
    }

    /// Runs the apply runtime client input event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// Runs the attached client size operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn attached_client_size(&self, client_id: &ClientId) -> Result<Option<Size>> {
        let Some(client) = self
            .service
            .session()
            .clients()
            .iter()
            .find(|client| client.id == *client_id && client.state == ClientState::Attached)
        else {
            return Ok(None);
        };
        if let Some(terminal) = client.terminal.as_ref() {
            return Size::new(terminal.columns, terminal.rows)
                .map(Some)
                .map_err(MezError::from);
        }
        Ok(self
            .service
            .session()
            .active_window()
            .map(|window| window.size))
    }

    /// Runs the client step application side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// Runs the apply runtime hook event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_runtime_hook_event(
        &mut self,
        hook_event: AsyncHookEvent,
    ) -> Result<RuntimeTransition> {
        self.service.apply_hook_transition(hook_event)
    }

    /// Runs the apply runtime persistence event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_runtime_persistence_event(
        &mut self,
        persistence_event: PersistenceEvent,
    ) -> Result<RuntimeTransition> {
        self.service.apply_persistence_transition(persistence_event)
    }

    /// Runs the apply runtime agent provider event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn apply_runtime_agent_provider_event(
        &mut self,
        provider_event: AgentProviderEvent,
    ) -> Result<RuntimeTransition> {
        match provider_event {
            AgentProviderEvent::Failed {
                agent_id,
                turn_id,
                kind,
                message,
                provider_failure_json,
                provider_raw_text,
            } => {
                let claim_cancellations = self.provider_claim_cancel_timer_side_effects(&turn_id);
                self.service.clear_claimed_agent_provider_task(&turn_id);
                let retry_class = provider_error_retry_class_from_parts(
                    provider_event_error_kind(&kind),
                    &message,
                    provider_failure_json.as_deref(),
                );
                let error = provider_event_error_from_parts(
                    &kind,
                    &message,
                    provider_failure_json.as_deref(),
                    provider_raw_text.as_deref(),
                );
                if let Some(mut application) =
                    self.service.schedule_agent_provider_retry_transition(
                        &agent_id,
                        &turn_id,
                        retry_class,
                        &error,
                    )?
                {
                    if application.applied {
                        application
                            .side_effects
                            .extend(self.render_side_effects(RenderInvalidationReason::FullRedraw));
                    } else {
                        self.timers.provider_retry.remove(turn_id.as_str());
                    }
                    application.side_effects.extend(claim_cancellations);
                    return Ok(application);
                }
                if matches!(retry_class, ProviderErrorRetryClass::OutputLimit)
                    && !self
                        .provider_output_limit_compaction_turns
                        .contains(turn_id.as_str())
                    && self
                        .service
                        .queue_agent_output_limit_recovery_compaction(&agent_id, &turn_id, &error)?
                {
                    self.service
                        .clear_agent_provider_retry_attempt(turn_id.as_str());
                    self.timers.provider_retry.remove(turn_id.as_str());
                    self.provider_output_limit_compaction_turns
                        .insert(turn_id.clone());
                    let mut side_effects =
                        self.render_side_effects(RenderInvalidationReason::FullRedraw);
                    side_effects.extend(self.pending_provider_dispatch_side_effects()?);
                    side_effects.extend(claim_cancellations);
                    return Ok(RuntimeTransition {
                        applied: true,
                        side_effects,
                    });
                }
                self.service
                    .clear_agent_provider_retry_attempt(turn_id.as_str());
                self.timers.provider_retry.remove(turn_id.as_str());
                self.provider_output_limit_compaction_turns
                    .remove(turn_id.as_str());
                let mut transition = self.service.apply_agent_provider_failed_transition(
                    &agent_id,
                    &turn_id,
                    &kind,
                    &message,
                    provider_failure_json.as_deref(),
                    provider_raw_text.as_deref(),
                )?;
                transition.side_effects.extend(claim_cancellations);
                Ok(transition)
            }
            AgentProviderEvent::OutputProgress {
                agent_id: _,
                turn_id: _,
                pane_id,
                action_id: _,
                lines,
            } => Ok(self
                .service
                .apply_agent_provider_output_progress_transition(&pane_id, &lines)),
            AgentProviderEvent::Completed {
                agent_id,
                turn_id,
                execution,
            } => {
                let claim_cancellations = self.provider_claim_cancel_timer_side_effects(&turn_id);
                self.service.clear_claimed_agent_provider_task(&turn_id);
                self.service
                    .clear_agent_provider_retry_attempt(turn_id.as_str());
                self.timers.provider_retry.remove(turn_id.as_str());
                self.provider_output_limit_compaction_turns
                    .remove(turn_id.as_str());
                let mut transition = self
                    .service
                    .apply_agent_provider_completed_transition(&agent_id, &turn_id, *execution)
                    .await?;
                if transition.applied {
                    transition
                        .side_effects
                        .extend(self.pending_provider_dispatch_side_effects()?);
                }
                transition.side_effects.extend(claim_cancellations);
                Ok(transition)
            }
        }
    }

    /// Runs the apply runtime shutdown event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_runtime_shutdown_event(
        &mut self,
        shutdown: ShutdownEvent,
    ) -> Result<RuntimeTransition> {
        self.service.apply_shutdown_transition(shutdown)
    }
}
