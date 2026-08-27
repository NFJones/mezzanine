//! Seatbelt capability-probe transaction dispatch and settlement.
//!
//! The probe executes through the active pane shell before any configured
//! Seatbelt workload. Only an exact code-owned sentinel populates the cache;
//! stale identity, truncated or contaminated output, timeout, protocol
//! failure, and pane-write failure all settle every waiter fail closed without
//! caching a result or retrying outside the sandbox.

use super::{
    ActionStatus, EventKind, PaneReadinessState, Result, RunningShellTransactionKind,
    RunningShellTransactionRef, RuntimeSessionService, RuntimeShellTransactionActionFailure,
    ShellTransaction, current_unix_millis, json_escape, runtime_marker_for_action,
    runtime_pane_readiness_state_name,
};
use crate::runtime::SandboxConfig;
use mez_agent::{ShellChildArgument, ShellChildLaunch, ShellClassification};

const RUNTIME_SEATBELT_CAPABILITY_PROBE_TIMEOUT_MS: u64 = 15_000;

impl RuntimeSessionService {
    /// Returns the exact successful Seatbelt capability cached for one pane
    /// environment and complete backend/profile identity.
    pub(crate) fn seatbelt_capability(
        &self,
        cache_key: &crate::security::sandbox::SeatbeltCapabilityCacheKey,
    ) -> Option<crate::security::sandbox::SeatbeltCapability> {
        self.process
            .pane_seatbelt_capabilities
            .get(cache_key)
            .cloned()
    }

    /// Records one verified native or pane Seatbelt capability for exact reuse.
    pub(crate) fn record_seatbelt_capability(
        &mut self,
        cache_key: crate::security::sandbox::SeatbeltCapabilityCacheKey,
        capability: crate::security::sandbox::SeatbeltCapability,
    ) {
        self.process
            .pane_seatbelt_capabilities
            .insert(cache_key, capability);
    }

    /// Ensures the active pane environment has passed the exact Seatbelt
    /// runtime-profile probe required by the configured backend.
    pub(crate) fn ensure_seatbelt_capability_for_action(
        &mut self,
        turn: &mez_agent::AgentTurnRecord,
        action_id: &str,
        child_shell_path: Option<&str>,
        omit_forwarded_environment: bool,
    ) -> Result<bool> {
        let permission_policy = self.permission_policy_for_turn(turn);
        let sandbox_config = self.sandbox_config_for_pane(&turn.pane_id);
        if !crate::runtime::config::sandbox_applies_to_policy(&sandbox_config, &permission_policy) {
            return Ok(true);
        }
        let SandboxConfig::Seatbelt(config) = sandbox_config else {
            return Ok(true);
        };
        let signature = self
            .pane_environment_signature(&turn.pane_id)
            .cloned()
            .ok_or_else(|| {
                crate::MezError::invalid_state(
                    "pane environment is unavailable for Seatbelt capability probing",
                )
            })?;
        let request = mez_agent::shell::PaneEnvironmentRequest::new(
            config.env_whitelist.requested_names.clone(),
        )
        .map_err(|error| crate::MezError::invalid_args(error.message()))?;
        let environment_evidence = if omit_forwarded_environment || request.names.is_empty() {
            mez_agent::shell::PaneEnvironmentEvidence::restrictive(
                &request,
                if omit_forwarded_environment {
                    "semantic_patch_not_forwarded"
                } else {
                    "not_configured"
                },
            )
        } else {
            self.pane_environment_evidence(turn, action_id, &request)
                .ok_or_else(|| {
                    crate::MezError::invalid_state(
                        "pane environment evidence is unavailable for Seatbelt capability probing",
                    )
                })?
        };
        let probe_plan = crate::security::sandbox::seatbelt_capability_probe_plan(
            &config,
            child_shell_path.unwrap_or(&signature.shell_path),
            &signature,
            &environment_evidence,
        )
        .map_err(|error| crate::MezError::invalid_state(error.message()))?;
        let cache_key = crate::security::sandbox::seatbelt_capability_cache_key(
            &turn.pane_id,
            &signature.stable_hash(),
            self.session.config_generation,
            &probe_plan,
        )
        .map_err(|error| crate::MezError::invalid_state(error.message()))?;
        if self
            .process
            .pane_seatbelt_capabilities
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
                        RunningShellTransactionKind::SeatbeltCapabilityProbe {
                            cache_key: pending,
                            ..
                        } if pending.as_ref() == &cache_key
                    )
                })
        {
            let RunningShellTransactionKind::SeatbeltCapabilityProbe { waiters, .. } =
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
            runtime_marker_for_action(turn, &format!("seatbelt-capability-probe-{action_id}"))?;
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
        let input = transaction.render_for_classification_input(classification);
        self.require_generated_shell_input(&input)?;
        let receiver_payload = (!input.receiver_payload.is_empty()).then(|| {
            mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                input.receiver_payload.clone().into_bytes(),
                marker_id.clone(),
                true,
            )
        });
        let mut wrapper = input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        let requires_payload_receiver_ready =
            classification == ShellClassification::Fish && !input.payload.is_empty();
        let previous = self.pane_readiness_state(&turn.pane_id);
        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Busy);
        self.register_running_shell_transaction(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: turn.turn_id.clone(),
                kind: RunningShellTransactionKind::SeatbeltCapabilityProbe {
                    action_id: action_id.to_string(),
                    waiters: vec![(turn.turn_id.clone(), action_id.to_string())],
                    cache_key: Box::new(cache_key),
                    probe_plan,
                },
                pane_id: turn.pane_id.clone(),
                command: "Seatbelt capability probe".to_string(),
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(RUNTIME_SEATBELT_CAPABILITY_PROBE_TIMEOUT_MS),
                pending_input_payload: (!input.payload.is_empty()).then(|| {
                    mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                        input.payload.into_bytes(),
                        marker_id.clone(),
                        input.payload_receiver_acknowledgements,
                    )
                }),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
            true,
        );
        if requires_payload_receiver_ready {
            self.require_shell_transaction_payload_receiver_ready(&marker_id);
        }
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
                "pane_readiness {} -> busy reason=seatbelt_capability_probe_sent marker={marker_id}",
                runtime_pane_readiness_state_name(previous)
            ),
        )?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","turn_id":"{}","action_id":"{}","seatbelt_probe":"sent","marker":"{}"}}"#,
                json_escape(&turn.pane_id),
                json_escape(&turn.turn_id),
                json_escape(action_id),
                json_escape(&marker_id)
            ),
        )?;
        Ok(false)
    }

    /// Settles one completed Seatbelt probe and resumes every exact waiter only
    /// after fresh identity and exact-sentinel validation.
    pub(crate) fn observe_seatbelt_capability_probe_transaction_end(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        exit_code: i32,
    ) -> Result<usize> {
        let RunningShellTransactionKind::SeatbeltCapabilityProbe {
            waiters,
            cache_key,
            probe_plan,
            ..
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
        let parsed = if current_key_matches && !transaction.observed_output_truncated {
            crate::security::sandbox::parse_seatbelt_capability_probe(
                &cache_key.pane_id,
                &cache_key.pane_environment_signature,
                cache_key.config_generation,
                &probe_plan,
                exit_code,
                &transaction.observed_output_preview,
            )
            .ok()
        } else {
            None
        };
        if let Some(capability) = parsed.filter(|capability| capability.cache_key == *cache_key) {
            self.process.pane_seatbelt_capabilities.retain(|key, _| {
                key.pane_environment_signature != cache_key.pane_environment_signature
                    || key.executable == cache_key.executable
            });
            self.process
                .pane_seatbelt_capabilities
                .insert(*cache_key, capability);
            let previous = self.pane_readiness_state(&transaction.pane_id);
            self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Ready);
            self.append_agent_trace_turn_event(
                &transaction.pane_id,
                &transaction.turn_id,
                &format!(
                    "pane_readiness {} -> ready reason=seatbelt_capability_probe_completed marker={marker}",
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
            return Ok(1);
        }
        let (code, message) = if !current_key_matches {
            (
                "seatbelt_probe_stale_identity",
                "Seatbelt capability probe completed for a stale pane environment",
            )
        } else if transaction.observed_output_truncated {
            (
                "seatbelt_probe_output_truncated",
                "Seatbelt capability probe output was truncated",
            )
        } else if exit_code != 0 {
            (
                "seatbelt_probe_nonzero_exit",
                "Seatbelt did not satisfy the fixed runtime-profile capability probe",
            )
        } else {
            (
                "seatbelt_probe_output_mismatch",
                "Seatbelt capability probe output did not exactly match its sentinel",
            )
        };
        self.fail_seatbelt_capability_probe_transaction(
            marker,
            transaction,
            code,
            message,
            false,
            false,
        )?;
        Ok(1)
    }

    /// Expires a Seatbelt probe before any workload can launch.
    pub(crate) fn expire_seatbelt_capability_probe_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        self.fail_seatbelt_capability_probe_transaction(
            marker,
            transaction,
            "seatbelt_probe_timeout",
            &format!(
                "Seatbelt capability probe timed out after {elapsed_ms} ms (limit {timeout_ms} ms)"
            ),
            true,
            true,
        )
    }

    /// Settles every waiter for a failed Seatbelt probe without caching it.
    pub(crate) fn fail_seatbelt_capability_probe_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        code: &str,
        message: &str,
        degraded: bool,
        timed_out: bool,
    ) -> Result<()> {
        let RunningShellTransactionKind::SeatbeltCapabilityProbe {
            waiters, cache_key, ..
        } = transaction.kind.clone()
        else {
            return Ok(());
        };
        self.process
            .pane_seatbelt_capabilities
            .remove(cache_key.as_ref());
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
            "pane_id": transaction.pane_id,
            "runtime_profile_version": cache_key.runtime_profile_version,
            "probe_sha256": cache_key.probe_sha256,
            "config_generation": cache_key.config_generation,
            "exit_observed": !timed_out,
            "timed_out": timed_out,
            "combined_output_bytes": transaction.observed_output_bytes,
            "exact_sentinel": transaction.observed_output_preview == crate::security::sandbox::seatbelt_probe::SEATBELT_CAPABILITY_SENTINEL,
            "boundary_state": "seatbelt-capability-probe-failed",
            "output_truncated": transaction.observed_output_truncated
        });
        let mut waiters_by_turn = std::collections::BTreeMap::<String, Vec<String>>::new();
        for (turn_id, action_id) in waiters {
            waiters_by_turn.entry(turn_id).or_default().push(action_id);
        }
        for (turn_id, action_ids) in waiters_by_turn {
            let mut waiter_transaction = transaction.clone();
            waiter_transaction.turn_id = turn_id;
            let failures = action_ids
                .into_iter()
                .map(|action_id| RuntimeShellTransactionActionFailure {
                    action_id,
                    status: if timed_out {
                        ActionStatus::TimedOut
                    } else {
                        ActionStatus::Failed
                    },
                    code: code.to_string(),
                    message: message.to_string(),
                    sent_to_pane: true,
                    terminal_observation: terminal_observation.clone(),
                    trace_reason: code.to_string(),
                })
                .collect();
            let _ =
                self.fail_running_shell_transaction_actions(&waiter_transaction, marker, failures)?;
        }
        Ok(())
    }
}
