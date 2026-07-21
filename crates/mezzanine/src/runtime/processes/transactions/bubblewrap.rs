//! Bubblewrap capability-probe transaction settlement.
//!
//! Capability probes execute in the target pane environment before any
//! sandboxed workload. This module caches only exact successful probe results
//! and converts every failed, stale, truncated, or timed-out probe into a
//! fail-closed action result without retrying the workload unsandboxed.

use super::{
    ActionStatus, EventKind, PaneReadinessState, Result, RunningShellTransactionKind,
    RunningShellTransactionRef, RuntimeSessionService, RuntimeShellTransactionActionFailure,
    ShellTransaction, current_unix_millis, json_escape, runtime_marker_for_action,
    runtime_pane_readiness_state_name,
};
use crate::runtime::SandboxConfig;
use mez_agent::{ShellChildArgument, ShellChildLaunch};

const RUNTIME_BUBBLEWRAP_CAPABILITY_PROBE_TIMEOUT_MS: u64 = 15_000;

impl RuntimeSessionService {
    /// Returns the exact successful Bubblewrap capability cached for one pane
    /// environment and runtime-profile identity.
    pub(crate) fn bubblewrap_capability(
        &self,
        cache_key: &crate::security::sandbox::BubblewrapCapabilityCacheKey,
    ) -> Option<crate::security::sandbox::BubblewrapCapability> {
        self.process
            .pane_bubblewrap_capabilities
            .get(cache_key)
            .cloned()
    }

    /// Ensures the active pane environment has passed the exact Bubblewrap
    /// runtime-profile probe required by the configured backend.
    ///
    /// Returns `true` when workload compilation may continue. Returns `false`
    /// after starting or observing an in-flight probe; the pending action stays
    /// running and is resumed only after successful probe settlement.
    pub(crate) fn ensure_bubblewrap_capability_for_action(
        &mut self,
        turn: &mez_agent::AgentTurnRecord,
        action_id: &str,
    ) -> Result<bool> {
        let SandboxConfig::Bubblewrap(config) = self.configured_permissions().sandbox.clone()
        else {
            return Ok(true);
        };
        let signature = self
            .pane_environment_signature(&turn.pane_id)
            .cloned()
            .ok_or_else(|| {
                crate::MezError::invalid_state(
                    "pane environment is unavailable for Bubblewrap capability probing",
                )
            })?;
        let probe_plan = crate::security::sandbox::bubblewrap_capability_probe_plan(
            &config,
            &signature.shell_path,
        )
        .map_err(|error| crate::MezError::invalid_state(error.message()))?;
        let cache_key = crate::security::sandbox::BubblewrapCapabilityCacheKey {
            pane_environment_signature: signature.stable_hash(),
            executable: probe_plan.executable.clone(),
            runtime_profile_version: crate::security::sandbox::BUBBLEWRAP_RUNTIME_PROFILE_VERSION,
            probe_sha256: probe_plan.probe_sha256.clone(),
        };
        if self
            .process
            .pane_bubblewrap_capabilities
            .contains_key(&cache_key)
        {
            return Ok(true);
        }
        if let Some(transaction) =
            self.process
                .running_shell_transactions
                .values_mut()
                .find(|transaction| {
                    matches!(
                        &transaction.kind,
                        RunningShellTransactionKind::BubblewrapCapabilityProbe {
                            cache_key: pending,
                            ..
                        } if pending == &cache_key
                    )
                })
        {
            let RunningShellTransactionKind::BubblewrapCapabilityProbe { waiters, .. } =
                &mut transaction.kind
            else {
                return Ok(false);
            };
            let waiter = (turn.turn_id.clone(), action_id.to_string());
            if !waiters.contains(&waiter) {
                waiters.push(waiter);
            }
            return Ok(false);
        }

        self.require_pane_ready_for_agent_command(&turn.pane_id)?;
        let marker =
            runtime_marker_for_action(turn, &format!("bubblewrap-capability-probe-{action_id}"))?;
        let marker_id = marker.as_str().to_string();
        let child_launch = ShellChildLaunch::new(
            probe_plan.executable.clone(),
            probe_plan
                .arguments
                .iter()
                .cloned()
                .map(ShellChildArgument::Literal)
                .collect(),
        )?;
        let transaction = ShellTransaction::new(
            marker,
            &turn.turn_id,
            &turn.agent_id,
            &turn.pane_id,
            self.session.shell.path(),
            "",
        )?
        .with_child_launch(child_launch);
        let classification = self.shell_classification_for_pane(&turn.pane_id);
        let transaction_input = transaction.render_for_classification_input(classification);
        let mut wrapper = transaction_input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        self.write_runtime_pane_input(&turn.pane_id, wrapper.as_bytes())?;
        let previous = self.pane_readiness_state(&turn.pane_id);
        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Busy);
        self.register_running_shell_transaction(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: turn.turn_id.clone(),
                kind: RunningShellTransactionKind::BubblewrapCapabilityProbe {
                    action_id: action_id.to_string(),
                    waiters: vec![(turn.turn_id.clone(), action_id.to_string())],
                    cache_key,
                    probe_plan,
                },
                pane_id: turn.pane_id.clone(),
                command: "Bubblewrap capability probe".to_string(),
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(RUNTIME_BUBBLEWRAP_CAPABILITY_PROBE_TIMEOUT_MS),
                pending_input_payload: (!transaction_input.payload.is_empty())
                    .then(|| transaction_input.payload.into_bytes()),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
            true,
        );
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "pane_readiness {} -> busy reason=bubblewrap_capability_probe_sent marker={marker_id}",
                runtime_pane_readiness_state_name(previous)
            ),
        )?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","turn_id":"{}","action_id":"{}","bubblewrap_probe":"sent","marker":"{}"}}"#,
                json_escape(&turn.pane_id),
                json_escape(&turn.turn_id),
                json_escape(action_id),
                json_escape(&marker_id)
            ),
        )?;
        Ok(false)
    }

    /// Settles a completed Bubblewrap capability probe and resumes the pending
    /// action only when the exact pane environment and probe plan still match.
    pub(crate) fn observe_bubblewrap_capability_probe_transaction_end(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        exit_code: i32,
    ) -> Result<usize> {
        let RunningShellTransactionKind::BubblewrapCapabilityProbe {
            action_id: _,
            waiters,
            cache_key,
            probe_plan,
        } = transaction.kind.clone()
        else {
            return Ok(0);
        };
        let current_environment = self
            .pane_environment_signature(&transaction.pane_id)
            .map(|signature| signature.stable_hash());
        let current_key_matches =
            current_environment.as_deref() == Some(cache_key.pane_environment_signature.as_str());
        let parsed = if current_key_matches && !transaction.observed_output_truncated {
            crate::security::sandbox::parse_bubblewrap_capability_probe(
                &cache_key.pane_environment_signature,
                &probe_plan,
                exit_code,
                &transaction.observed_output_preview,
            )
            .map_err(|error| error.message().to_string())
        } else if !current_key_matches {
            Err("Bubblewrap capability probe completed for a stale pane environment".to_string())
        } else {
            Err("Bubblewrap capability probe output was truncated".to_string())
        };

        match parsed {
            Ok(capability) if capability.cache_key == cache_key => {
                self.process.pane_bubblewrap_capabilities.retain(|key, _| {
                    key.pane_environment_signature != cache_key.pane_environment_signature
                        || key.executable == cache_key.executable
                });
                self.process
                    .pane_bubblewrap_capabilities
                    .insert(cache_key, capability);
                let previous = self.pane_readiness_state(&transaction.pane_id);
                self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Ready);
                self.append_agent_trace_turn_event(
                    &transaction.pane_id,
                    &transaction.turn_id,
                    &format!(
                        "pane_readiness {} -> ready reason=bubblewrap_capability_probe_completed marker={marker}",
                        runtime_pane_readiness_state_name(previous)
                    ),
                )?;
                for turn_id in waiters
                    .into_iter()
                    .map(|(turn_id, _)| turn_id)
                    .collect::<std::collections::BTreeSet<_>>()
                {
                    let _ = self.dispatch_stored_running_shell_actions(&turn_id)?;
                }
                Ok(1)
            }
            Ok(_) => {
                self.fail_bubblewrap_capability_probe_transaction(
                    marker,
                    transaction,
                    "bubblewrap_probe_identity_mismatch",
                    "Bubblewrap capability probe result did not match its requested identity",
                    false,
                    false,
                )?;
                Ok(1)
            }
            Err(message) => {
                self.fail_bubblewrap_capability_probe_transaction(
                    marker,
                    transaction,
                    "bubblewrap_probe_failed",
                    &message,
                    false,
                    false,
                )?;
                Ok(1)
            }
        }
    }

    /// Expires a Bubblewrap probe before any workload can be launched.
    pub(crate) fn expire_bubblewrap_capability_probe_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        self.fail_bubblewrap_capability_probe_transaction(
            marker,
            transaction,
            "bubblewrap_probe_timeout",
            &format!(
                "Bubblewrap capability probe timed out after {elapsed_ms} ms (limit {timeout_ms} ms)"
            ),
            true,
            true,
        )
    }

    /// Records one fail-closed Bubblewrap probe outcome and settles the action
    /// that was waiting for it.
    pub(crate) fn fail_bubblewrap_capability_probe_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        code: &str,
        message: &str,
        degraded: bool,
        timed_out: bool,
    ) -> Result<()> {
        let RunningShellTransactionKind::BubblewrapCapabilityProbe {
            action_id: _,
            waiters,
            cache_key,
            ..
        } = transaction.kind.clone()
        else {
            return Ok(());
        };
        self.process.pane_bubblewrap_capabilities.remove(&cache_key);
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(
            &transaction.pane_id,
            if degraded {
                PaneReadinessState::Degraded
            } else {
                PaneReadinessState::Ready
            },
        );
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> {} reason={code} marker={marker}",
                runtime_pane_readiness_state_name(previous),
                if degraded { "degraded" } else { "ready" }
            ),
        )?;
        let terminal_observation = serde_json::json!({
            "source": "pty",
            "stream": "pty_combined",
            "marker": marker,
            "exit_code": null,
            "timed_out": timed_out,
            "combined_output_bytes": transaction.observed_output_bytes,
            "boundary_state": "bubblewrap-capability-probe-failed",
            "output_truncated": transaction.observed_output_truncated
        });
        for (turn_id, action_id) in waiters {
            let mut waiter_transaction = transaction.clone();
            waiter_transaction.turn_id = turn_id;
            if self.offer_sandbox_pre_payload_fallback_approval(
                marker,
                &waiter_transaction.turn_id,
                &action_id,
                code,
            )? {
                continue;
            }
            let _ = self.fail_running_shell_transaction_action(
                &waiter_transaction,
                marker,
                RuntimeShellTransactionActionFailure {
                    action_id,
                    status: if timed_out {
                        ActionStatus::TimedOut
                    } else {
                        ActionStatus::Failed
                    },
                    code: code.to_string(),
                    message: message.to_string(),
                    sent_to_pane: false,
                    terminal_observation: terminal_observation.clone(),
                    trace_reason: code.to_string(),
                },
            )?;
        }
        Ok(())
    }
}
