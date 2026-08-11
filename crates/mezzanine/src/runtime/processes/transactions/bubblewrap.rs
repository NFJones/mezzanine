//! Bubblewrap capability-probe transaction settlement.
//!
//! Capability probes execute in the target pane environment before any
//! sandboxed workload. This module caches only exact successful probe results
//! and converts every failed, stale, truncated, or timed-out probe into a
//! fail-closed action result without retrying the workload unsandboxed.

use super::{
    ActionStatus, BubblewrapEnvironmentProfile, EventKind, PaneReadinessState, Result,
    RunningShellTransactionKind, RunningShellTransactionRef, RuntimeSessionService,
    RuntimeShellTransactionActionFailure, ShellTransaction, current_unix_millis, json_escape,
    runtime_marker_for_action, runtime_pane_readiness_state_name,
};
use crate::runtime::SandboxConfig;
use mez_agent::{ShellChildArgument, ShellChildLaunch};

const RUNTIME_BUBBLEWRAP_CAPABILITY_PROBE_TIMEOUT_MS: u64 = 15_000;
const RUNTIME_BUBBLEWRAP_CAPABILITY_PREVIEW_BYTES: usize = 512;

/// Typed completion evidence retained for one failed capability probe.
#[derive(Debug, Clone, Copy)]
struct BubblewrapProbeFailureEvidence<'a> {
    /// Stable action-result error code.
    code: &'a str,
    /// Bounded user-facing failure message.
    message: &'a str,
    /// Whether pane readiness must remain degraded after settlement.
    degraded: bool,
    /// Whether the failure represents probe timeout.
    timed_out: bool,
    /// Actual wrapper child exit status when an end marker was observed.
    exit_code: Option<i32>,
    /// Stable failure classification exposed to action-result consumers.
    failure_class: &'a str,
    /// Whether strict start/end transaction framing completed.
    framing_complete: bool,
    /// Whether retained output exactly matched the fixed sentinel.
    exact_sentinel: bool,
}

/// Returns a bounded escaped preview suitable for probe failure evidence.
fn bubblewrap_probe_output_preview(output: &str) -> String {
    let mut preview = String::new();
    for character in output.chars() {
        let escaped = character.escape_default().to_string();
        if preview.len().saturating_add(escaped.len()) > RUNTIME_BUBBLEWRAP_CAPABILITY_PREVIEW_BYTES
        {
            break;
        }
        preview.push_str(&escaped);
    }
    preview
}

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
        self.ensure_bubblewrap_capability_for_action_with_environment_profile(
            turn,
            action_id,
            BubblewrapEnvironmentProfile::ConfiguredForwarding,
        )
    }

    /// Ensures the active pane has a capability matching the selected action
    /// environment profile exactly.
    pub(crate) fn ensure_bubblewrap_capability_for_action_with_environment_profile(
        &mut self,
        turn: &mez_agent::AgentTurnRecord,
        action_id: &str,
        environment_profile: BubblewrapEnvironmentProfile,
    ) -> Result<bool> {
        self.ensure_bubblewrap_capability_for_action_with_environment_profile_and_child_shell(
            turn,
            action_id,
            environment_profile,
            None,
        )
    }

    /// Ensures the active pane has a capability matching both the selected
    /// environment profile and an optional declared child interpreter.
    pub(crate) fn ensure_bubblewrap_capability_for_action_with_environment_profile_and_child_shell(
        &mut self,
        turn: &mez_agent::AgentTurnRecord,
        action_id: &str,
        environment_profile: BubblewrapEnvironmentProfile,
        child_shell_path: Option<&str>,
    ) -> Result<bool> {
        let permission_policy = self.permission_policy_for_turn(turn);
        let sandbox_config = self.sandbox_config_for_pane(&turn.pane_id);
        if !crate::runtime::config::bubblewrap_applies_to_policy(
            &sandbox_config,
            &permission_policy,
        ) {
            return Ok(true);
        }
        let SandboxConfig::Bubblewrap(config) = sandbox_config else {
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
        let identity =
            crate::security::sandbox::resolve_sandbox_identity(&config.group_whitelist, &signature)
                .map_err(|error| crate::MezError::invalid_state(error.message()))?;
        for warning in &identity.mapping_warnings {
            self.append_sandbox_mapping_warning_once(
                &turn.pane_id,
                &format!(
                    "{}:{}:{}",
                    warning.mapping_kind, warning.configured_value, warning.reason
                ),
                &format!(
                    "{} `{}` ({})",
                    warning.mapping_kind, warning.configured_value, warning.reason
                ),
            )?;
        }
        let environment_request = mez_agent::shell::PaneEnvironmentRequest::new(
            config.env_whitelist.requested_names.clone(),
        )
        .map_err(|error| crate::MezError::invalid_args(error.message()))?;
        let environment_evidence = self
            .bubblewrap_environment_evidence_for_action(
                turn,
                action_id,
                &environment_request,
                environment_profile,
            )
            .ok_or_else(|| {
                crate::MezError::invalid_state(
                    "pane environment evidence is unavailable for Bubblewrap capability probing",
                )
            })?;
        let probe_plan = crate::security::sandbox::bubblewrap_capability_probe_plan_for_identity(
            &config,
            child_shell_path.unwrap_or(&signature.shell_path),
            &identity,
            &environment_evidence,
        )
        .map_err(|error| crate::MezError::invalid_state(error.message()))?;
        let cache_key = crate::security::sandbox::bubblewrap_capability_cache_key(
            &turn.pane_id,
            &signature.stable_hash(),
            self.session.config_generation,
            &probe_plan,
        )
        .map_err(|error| crate::MezError::invalid_state(error.message()))?;
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
                        } if pending.as_ref() == &cache_key
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
        let shell_identity = self.shell_execution_identity_for_pane(&turn.pane_id)?;
        let transaction = self.configure_shell_transaction_for_pane(
            &turn.pane_id,
            ShellTransaction::new(
                marker,
                &turn.turn_id,
                &turn.agent_id,
                &turn.pane_id,
                shell_identity.shell_path(),
                "",
            )?
            .with_child_launch(child_launch),
        );
        let classification = shell_identity.classification();
        let transaction_input = transaction.render_for_classification_input(classification);
        let receiver_payload = (!transaction_input.receiver_payload.is_empty()).then(|| {
            mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                transaction_input.receiver_payload.clone().into_bytes(),
                marker_id.clone(),
                true,
            )
        });
        let mut wrapper = transaction_input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        let previous = self.pane_readiness_state(&turn.pane_id);
        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Busy);
        self.register_running_shell_transaction(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: turn.turn_id.clone(),
                kind: RunningShellTransactionKind::BubblewrapCapabilityProbe {
                    action_id: action_id.to_string(),
                    waiters: vec![(turn.turn_id.clone(), action_id.to_string())],
                    cache_key: Box::new(cache_key),
                    probe_plan,
                },
                pane_id: turn.pane_id.clone(),
                command: "Bubblewrap capability probe".to_string(),
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(RUNTIME_BUBBLEWRAP_CAPABILITY_PROBE_TIMEOUT_MS),
                pending_input_payload: (!transaction_input.payload.is_empty()).then(|| {
                    mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                        transaction_input.payload.into_bytes(),
                        marker_id.clone(),
                        transaction_input.payload_receiver_acknowledgements,
                    )
                }),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
            true,
        );
        if let Some(receiver_payload) = receiver_payload {
            self.register_shell_receiver_payload(&marker_id, receiver_payload);
        }
        if let Err(error) = self.write_runtime_pane_shell_input(&turn.pane_id, wrapper.as_bytes()) {
            self.fail_shell_transactions_for_pane_write_failure(&turn.pane_id, error.message())?;
            return Err(error);
        }
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
        let current_key_matches = cache_key.pane_id == transaction.pane_id
            && cache_key.config_generation == self.session.config_generation
            && current_environment.as_deref()
                == Some(cache_key.pane_environment_signature.as_str());
        let exact_sentinel = transaction.observed_output_preview == probe_plan.expected_stdout;
        let parsed = if current_key_matches && !transaction.observed_output_truncated {
            crate::security::sandbox::parse_bubblewrap_capability_probe(
                &cache_key.pane_id,
                &cache_key.pane_environment_signature,
                cache_key.config_generation,
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
            Ok(capability) if capability.cache_key == *cache_key => {
                self.process.pane_bubblewrap_capabilities.retain(|key, _| {
                    key.pane_environment_signature != cache_key.pane_environment_signature
                        || key.executable == cache_key.executable
                });
                self.process
                    .pane_bubblewrap_capabilities
                    .insert(*cache_key, capability);
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
                self.fail_bubblewrap_capability_probe_transaction_with_evidence(
                    marker,
                    transaction,
                    BubblewrapProbeFailureEvidence {
                        code: "bubblewrap_probe_identity_mismatch",
                        message: "Bubblewrap capability probe result did not match its requested identity",
                        degraded: false,
                        timed_out: false,
                        exit_code: Some(exit_code),
                        failure_class: "identity_mismatch",
                        framing_complete: true,
                        exact_sentinel,
                    },
                )?;
                Ok(1)
            }
            Err(message) if !current_key_matches => {
                self.fail_bubblewrap_capability_probe_transaction_with_evidence(
                    marker,
                    transaction,
                    BubblewrapProbeFailureEvidence {
                        code: "bubblewrap_probe_stale_identity",
                        message: &message,
                        degraded: false,
                        timed_out: false,
                        exit_code: Some(exit_code),
                        failure_class: "stale_identity",
                        framing_complete: true,
                        exact_sentinel,
                    },
                )?;
                Ok(1)
            }
            Err(message) if transaction.observed_output_truncated => {
                self.fail_bubblewrap_capability_probe_transaction_with_evidence(
                    marker,
                    transaction,
                    BubblewrapProbeFailureEvidence {
                        code: "bubblewrap_probe_output_truncated",
                        message: &message,
                        degraded: false,
                        timed_out: false,
                        exit_code: Some(exit_code),
                        failure_class: "output_truncated",
                        framing_complete: true,
                        exact_sentinel,
                    },
                )?;
                Ok(1)
            }
            Err(message) if exit_code != 0 => {
                self.fail_bubblewrap_capability_probe_transaction_with_evidence(
                    marker,
                    transaction,
                    BubblewrapProbeFailureEvidence {
                        code: "bubblewrap_probe_nonzero_exit",
                        message: &message,
                        degraded: false,
                        timed_out: false,
                        exit_code: Some(exit_code),
                        failure_class: "nonzero_exit",
                        framing_complete: true,
                        exact_sentinel,
                    },
                )?;
                Ok(1)
            }
            Err(message) => {
                let failure_class = if transaction.observed_output_preview.is_empty() {
                    "empty_output"
                } else {
                    "output_mismatch"
                };
                self.fail_bubblewrap_capability_probe_transaction_with_evidence(
                    marker,
                    transaction,
                    BubblewrapProbeFailureEvidence {
                        code: "bubblewrap_probe_output_mismatch",
                        message: &message,
                        degraded: false,
                        timed_out: false,
                        exit_code: Some(exit_code),
                        failure_class,
                        framing_complete: true,
                        exact_sentinel,
                    },
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
        self.fail_bubblewrap_capability_probe_transaction_with_evidence(
            marker,
            transaction,
            BubblewrapProbeFailureEvidence {
                code,
                message,
                degraded,
                timed_out,
                exit_code: None,
                failure_class: code,
                framing_complete: false,
                exact_sentinel: false,
            },
        )
    }

    /// Settles a failed capability probe with bounded typed evidence.
    fn fail_bubblewrap_capability_probe_transaction_with_evidence(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        evidence: BubblewrapProbeFailureEvidence<'_>,
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
        self.process
            .pane_bubblewrap_capabilities
            .remove(cache_key.as_ref());
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(
            &transaction.pane_id,
            if evidence.degraded {
                PaneReadinessState::Degraded
            } else {
                PaneReadinessState::Ready
            },
        );
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> {} reason={} marker={marker}",
                runtime_pane_readiness_state_name(previous),
                if evidence.degraded {
                    "degraded"
                } else {
                    "ready"
                },
                evidence.code
            ),
        )?;
        let terminal_observation = serde_json::json!({
            "source": "pty",
            "stream": "pty_combined",
            "marker": marker,
            "pane_id": transaction.pane_id,
            "transaction_turn_id": transaction.turn_id,
            "runtime_profile_version": cache_key.runtime_profile_version,
            "probe_sha256": cache_key.probe_sha256,
            "pane_environment_signature": cache_key.pane_environment_signature,
            "config_generation": cache_key.config_generation,
            "framing_complete": evidence.framing_complete,
            "exit_code": evidence.exit_code,
            "timed_out": evidence.timed_out,
            "combined_output_bytes": transaction.observed_output_bytes,
            "decoded_output_bytes": transaction.observed_output_preview.len(),
            "combined_output_preview": bubblewrap_probe_output_preview(
                &transaction.observed_output_preview
            ),
            "failure_class": evidence.failure_class,
            "exact_sentinel": evidence.exact_sentinel,
            "boundary_state": "bubblewrap-capability-probe-failed",
            "output_truncated": transaction.observed_output_truncated
        });
        let mut waiters_by_turn = std::collections::BTreeMap::<String, Vec<String>>::new();
        for (turn_id, action_id) in waiters {
            waiters_by_turn.entry(turn_id).or_default().push(action_id);
        }
        for (turn_id, action_ids) in waiters_by_turn {
            let mut waiter_transaction = transaction.clone();
            waiter_transaction.turn_id = turn_id;
            let message =
                crate::security::sandbox::bubblewrap_failure_remediation(evidence.message);
            let failures = action_ids
                .into_iter()
                .map(|action_id| RuntimeShellTransactionActionFailure {
                    action_id,
                    status: if evidence.timed_out {
                        ActionStatus::TimedOut
                    } else {
                        ActionStatus::Failed
                    },
                    code: evidence.code.to_string(),
                    message: message.clone(),
                    sent_to_pane: false,
                    terminal_observation: terminal_observation.clone(),
                    trace_reason: evidence.code.to_string(),
                })
                .collect();
            let _ =
                self.fail_running_shell_transaction_actions(&waiter_transaction, marker, failures)?;
        }
        Ok(())
    }
}
