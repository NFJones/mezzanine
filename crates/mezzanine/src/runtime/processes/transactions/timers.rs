//! Transaction timer planning and protocol-state maintenance.

use super::{
    RenderInvalidationReason, Result, RuntimeSessionService, RuntimeShellTransactionTimerKind,
    RuntimeShellTransactionTimerRef, RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind,
    RuntimeTransition, runtime_shell_transaction_effective_timeout_ms,
    runtime_shell_transaction_timer_kind,
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
        let expired = self.expire_timed_out_shell_transactions(now_unix_ms)?;
        let certifications = self.expire_timed_out_agent_subshell_certifications(now_unix_ms)?;
        let recovery_observations =
            self.expire_timed_out_shell_dispatch_recovery_observations(now_unix_ms)?;
        let focused = self.expire_timed_out_focused_shell_hooks(now_unix_ms)?;
        Ok(expired
            .saturating_add(certifications)
            .saturating_add(recovery_observations)
            .saturating_add(focused))
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
