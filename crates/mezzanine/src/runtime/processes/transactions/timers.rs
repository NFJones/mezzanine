//! Transaction timer planning and protocol-state maintenance.

use super::{
    AgentTurnState, EventKind, MezError, PaneReadinessState, RenderInvalidationReason, Result,
    RuntimeSessionService, RuntimeShellTransactionTimerKind, RuntimeShellTransactionTimerRef,
    RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition, json_escape,
    runtime_shell_transaction_effective_timeout_ms, runtime_shell_transaction_timer_kind,
};
use std::collections::{BTreeMap, HashSet};

impl RuntimeSessionService {
    /// Applies a runtime timer firing for live Mezzanine-owned shell work,
    /// including post-transaction bootstrap certification.
    ///
    /// Returns the number of transactions, certifications, and focused hooks
    /// that were expired. A zero return means the timer was accepted but no
    /// live work had reached its deadline.
    pub fn apply_shell_transaction_timer_event(&mut self, now_unix_ms: u64) -> Result<usize> {
        // A foreign boundary owns its bootstrap transaction and must settle it
        // before generic transaction expiry discards the marker needed to
        // cancel an in-flight private-receiver delivery.
        let foreign_bootstraps = self.expire_timed_out_foreign_shell_bootstraps(now_unix_ms)?;
        let expired = self.expire_timed_out_shell_transactions(now_unix_ms)?;
        let certifications = self.expire_timed_out_agent_subshell_certifications(now_unix_ms)?;
        let recovery_observations =
            self.expire_timed_out_shell_dispatch_recovery_observations(now_unix_ms)?;
        let focused = self.expire_timed_out_focused_shell_hooks(now_unix_ms)?;
        Ok(expired
            .saturating_add(certifications)
            .saturating_add(recovery_observations)
            .saturating_add(foreign_bootstraps)
            .saturating_add(focused))
    }

    /// Settles foreign bootstrap phases whose bounded adapter owner expired.
    fn expire_timed_out_foreign_shell_bootstraps(&mut self, now_unix_ms: u64) -> Result<usize> {
        let expired = self
            .process
            .pane_foreign_shell_boundaries
            .iter()
            .filter(|(_, boundary)| {
                boundary.phase.has_bounded_owner()
                    && (now_unix_ms.saturating_sub(boundary.phase_started_at_unix_ms)
                        >= super::super::RUNTIME_FOREIGN_SHELL_BOOTSTRAP_PHASE_TIMEOUT_MS
                        || now_unix_ms.saturating_sub(boundary.lifecycle_started_at_unix_ms)
                            >= super::super::RUNTIME_FOREIGN_SHELL_BOOTSTRAP_ABSOLUTE_TIMEOUT_MS)
            })
            .map(|(pane_id, boundary)| {
                (
                    pane_id.clone(),
                    boundary.interaction_generation,
                    boundary.phase,
                )
            })
            .collect::<Vec<_>>();
        for (pane_id, interaction_generation, expired_phase) in &expired {
            let Some(boundary) = self.process.pane_foreign_shell_boundaries.get_mut(pane_id) else {
                continue;
            };
            if boundary.interaction_generation != *interaction_generation
                || !boundary.phase.has_bounded_owner()
            {
                continue;
            }
            boundary.phase = super::super::RuntimeForeignShellBootstrapPhase::Failed;
            boundary.phase_started_at_unix_ms = now_unix_ms;
            boundary.challenge = None;
            boundary.child_token = None;
            boundary.child_staging_source = None;
            boundary.identity_marker = None;

            let owned_marker = self
                .process
                .pane_managed_shell_handoffs
                .get(pane_id)
                .map(|handoff| handoff.identity().marker.clone())
                .or_else(|| {
                    self.process
                        .running_shell_transactions
                        .iter()
                        .find(|(_, transaction)| {
                            transaction.pane_id == *pane_id
                                && matches!(
                                    transaction.kind,
                                    super::RunningShellTransactionKind::Bootstrap
                                        | super::RunningShellTransactionKind::ShellIdentityProbe {
                                            ..
                                        }
                                )
                        })
                        .map(|(marker, _)| marker.clone())
                });
            if let Some(marker) = owned_marker.as_deref() {
                self.cancel_runtime_pane_shell_delivery(pane_id, marker);
                self.process
                    .bootstrap_shell_certification_evidence
                    .remove(marker);
                self.remove_running_shell_transaction(marker);
                self.clear_shell_transaction_protocol_state(marker);
            }
            if self
                .process
                .pane_managed_shell_handoffs
                .get(pane_id)
                .is_some_and(|handoff| !handoff.child_is_installed())
            {
                self.interrupt_shell_transaction_pane_if_live(pane_id)?;
            }
            self.process.pane_managed_shell_handoffs.remove(pane_id);
            self.process.pane_shell_handoffs.remove(pane_id);
            self.process
                .pane_agent_subshell_parent_return_pending
                .remove(pane_id);
            self.process
                .pending_agent_subshell_start_observations
                .remove(pane_id);
            self.process
                .pending_agent_subshell_certifications
                .remove(pane_id);
            self.process.pane_bootstrap_pending.remove(pane_id);
            self.clear_agent_subshell_shell_identity(pane_id);
            self.mark_pane_environment_authority_unavailable(
                pane_id,
                super::super::RuntimePaneEnvironmentAuthorityUnavailableReason::ForeignBootstrapTimedOut,
            );
            self.set_pane_readiness(pane_id, PaneReadinessState::Degraded);
            self.append_agent_error_text_to_terminal_buffer(
                pane_id,
                "agent: foreign shell bootstrap timed out; install or activate Mezzanine shell integration inside this environment and wait for its prompt",
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","foreign_bootstrap":"timed_out","generation":{},"phase":"{}","state":"degraded"}}"#,
                    json_escape(pane_id),
                    interaction_generation,
                    expired_phase.as_str()
                ),
            )?;
            let pending_turn_ids = self
                .agent_turn_ledger()
                .turns()
                .iter()
                .filter(|turn| {
                    turn.pane_id == *pane_id
                        && turn.state == AgentTurnState::Running
                        && self.agent_provider_task_is_pending(&turn.turn_id)
                })
                .map(|turn| turn.turn_id.clone())
                .collect::<Vec<_>>();
            let error = MezError::invalid_state(
                "foreign shell bootstrap timed out; install or activate Mezzanine shell integration inside the SSH or container environment and wait for its prompt",
            );
            for turn_id in pending_turn_ids {
                self.fail_configured_agent_provider_task(&turn_id, &error)?;
            }
        }
        Ok(expired.len())
    }

    /// Applies shell-transaction expiry through the transport-neutral transition contract.
    pub(crate) fn apply_shell_transaction_timer_transition(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<RuntimeTransition> {
        let expired = self.apply_shell_transaction_timer_event(now_unix_ms)?;
        Ok(self.runtime_transition_with_render(
            expired > 0,
            Some(RenderInvalidationReason::FullRedraw),
        ))
    }

    /// Returns timer-visible snapshots for live shell work with configured
    /// timeouts.
    pub fn running_shell_transaction_timers(&self) -> Vec<RuntimeShellTransactionTimerRef> {
        let mut timers = self
            .process
            .running_shell_transactions
            .iter()
            .filter_map(|(marker, transaction)| {
                let timeout_ms = runtime_shell_transaction_effective_timeout_ms(transaction)?;
                Some(RuntimeShellTransactionTimerRef {
                    marker: marker.clone(),
                    kind: runtime_shell_transaction_timer_kind(&transaction.kind),
                    started_at_unix_ms: transaction.started_at_unix_ms,
                    timeout_ms,
                })
            })
            .collect::<Vec<_>>();
        timers.extend(
            self.process
                .pending_agent_subshell_certifications
                .values()
                .map(|pending| RuntimeShellTransactionTimerRef {
                    marker: pending.observation_id.clone(),
                    kind: RuntimeShellTransactionTimerKind::Bootstrap,
                    started_at_unix_ms: pending.started_at_unix_ms,
                    timeout_ms: pending.timeout_ms,
                }),
        );
        timers.extend(
            self.process
                .pending_shell_dispatch_recovery_observations
                .values()
                .map(|pending| RuntimeShellTransactionTimerRef {
                    marker: pending.observation_id.clone(),
                    kind: RuntimeShellTransactionTimerKind::Bootstrap,
                    started_at_unix_ms: pending.started_at_unix_ms,
                    timeout_ms: super::RUNTIME_SHELL_DISPATCH_RECOVERY_OBSERVATION_TIMEOUT_MS,
                }),
        );
        timers.extend(
            self.process
                .pane_foreign_shell_boundaries
                .iter()
                .filter(|(_, boundary)| boundary.phase.has_bounded_owner())
                .map(|(pane_id, boundary)| {
                    let idle_deadline = boundary.phase_started_at_unix_ms.saturating_add(
                        super::super::RUNTIME_FOREIGN_SHELL_BOOTSTRAP_PHASE_TIMEOUT_MS,
                    );
                    let absolute_deadline = boundary.lifecycle_started_at_unix_ms.saturating_add(
                        super::super::RUNTIME_FOREIGN_SHELL_BOOTSTRAP_ABSOLUTE_TIMEOUT_MS,
                    );
                    let (started_at_unix_ms, timeout_ms) = if idle_deadline <= absolute_deadline {
                        (
                            boundary.phase_started_at_unix_ms,
                            super::super::RUNTIME_FOREIGN_SHELL_BOOTSTRAP_PHASE_TIMEOUT_MS,
                        )
                    } else {
                        (
                            boundary.lifecycle_started_at_unix_ms,
                            super::super::RUNTIME_FOREIGN_SHELL_BOOTSTRAP_ABSOLUTE_TIMEOUT_MS,
                        )
                    };
                    RuntimeShellTransactionTimerRef {
                        marker: format!(
                            "foreign-shell-bootstrap:{pane_id}:{}",
                            boundary.interaction_generation
                        ),
                        kind: RuntimeShellTransactionTimerKind::Bootstrap,
                        started_at_unix_ms,
                        timeout_ms,
                    }
                }),
        );
        timers.extend(
            self.integration
                .focused_shell_hook_transactions()
                .iter()
                .map(|(marker, transaction)| RuntimeShellTransactionTimerRef {
                    marker: marker.clone(),
                    kind: RuntimeShellTransactionTimerKind::FocusedShellHook,
                    started_at_unix_ms: transaction.started_at_unix_ms,
                    timeout_ms: transaction.timeout_ms,
                }),
        );
        timers
    }

    /// Reconciles live shell transaction timers against adapter-owned active keys.
    pub(crate) fn shell_transaction_timer_transition(
        &self,
        active_keys: &HashSet<RuntimeTimerKey>,
        now_ms: u64,
    ) -> RuntimeTransition {
        let desired = self
            .running_shell_transaction_timers()
            .into_iter()
            .map(|timer| {
                let key = RuntimeTimerKey::new(
                    match timer.kind {
                        RuntimeShellTransactionTimerKind::AgentAction => {
                            RuntimeTimerKind::ShellTransaction
                        }
                        RuntimeShellTransactionTimerKind::ReadinessProbe => {
                            RuntimeTimerKind::ReadinessProbe
                        }
                        RuntimeShellTransactionTimerKind::Bootstrap => RuntimeTimerKind::Bootstrap,
                        RuntimeShellTransactionTimerKind::PathResolution => {
                            RuntimeTimerKind::PathResolution
                        }
                        RuntimeShellTransactionTimerKind::EnvironmentEvidence => {
                            RuntimeTimerKind::ShellTransaction
                        }
                        RuntimeShellTransactionTimerKind::BubblewrapCapabilityProbe => {
                            RuntimeTimerKind::ShellTransaction
                        }
                        RuntimeShellTransactionTimerKind::FocusedShellHook => {
                            RuntimeTimerKind::FocusedShellHook
                        }
                    },
                    timer.marker,
                    timer.started_at_unix_ms,
                );
                let deadline_ms = timer.started_at_unix_ms.saturating_add(timer.timeout_ms);
                (key, deadline_ms.saturating_sub(now_ms))
            })
            .collect::<BTreeMap<_, _>>();
        let mut side_effects = active_keys
            .iter()
            .filter(|key| !desired.contains_key(*key))
            .cloned()
            .map(|key| RuntimeSideEffect::CancelTimer { key })
            .collect::<Vec<_>>();
        side_effects.extend(
            desired
                .into_iter()
                .filter(|(key, _)| !active_keys.contains(key))
                .map(|(key, delay_ms)| RuntimeSideEffect::ScheduleTimer { key, delay_ms }),
        );
        RuntimeTransition {
            applied: false,
            side_effects,
        }
    }

    /// Clears strict marker protocol state for one settled shell transaction.
    pub(crate) fn clear_shell_transaction_protocol_state(&mut self, marker: &str) {
        self.process
            .shell_transaction_require_start_markers
            .remove(marker);
        self.process
            .shell_transaction_started_markers
            .remove(marker);
        self.process
            .shell_transaction_payload_receiver_ready_required
            .remove(marker);
        self.process
            .shell_transaction_start_boundary_pending
            .remove(marker);
        self.process
            .shell_transaction_end_boundary_pending
            .remove(marker);
        self.process
            .shell_transaction_control_osc_pending
            .remove(marker);
        self.process
            .shell_transaction_output_utf8_pending
            .remove(marker);
        self.process
            .shell_transaction_receiver_acknowledgements
            .remove(marker);
        self.process.shell_receiver_pending_payloads.remove(marker);
        self.process
            .shell_receiver_completion_required
            .remove(marker);
        self.process.shell_receiver_pending_ends.remove(marker);
        self.process
            .shell_transaction_encoded_output_markers
            .remove(marker);
        self.process
            .sandboxed_shell_transaction_markers
            .remove(marker);
        self.process.managed_home_activity_locks.remove(marker);
    }

    /// Records that one live agent-action transaction uses Bubblewrap.
    pub(crate) fn register_sandboxed_shell_transaction_marker(&mut self, marker: &str) {
        self.process
            .sandboxed_shell_transaction_markers
            .insert(marker.to_string());
    }

    /// Retains one managed-home activity lock until its transaction settles.
    pub(crate) fn register_managed_home_activity_lock(
        &mut self,
        marker: &str,
        activity_lock: crate::security::sandbox::BubblewrapManagedHomeActivityLock,
    ) {
        self.process
            .managed_home_activity_locks
            .insert(marker.to_string(), activity_lock);
    }

    /// Interrupts a pane after a protocol violation when the process is live.
    pub(super) fn interrupt_shell_transaction_pane_if_live(&mut self, pane_id: &str) -> Result<()> {
        match self.interrupt_shell_transaction_pane(pane_id) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}
