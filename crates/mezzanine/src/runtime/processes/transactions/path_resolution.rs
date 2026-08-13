//! Pane-shell canonical path-resolution transaction dispatch and settlement.
//!
//! Resolution runs through the visible pane execution environment so local,
//! SSH, container, and chroot panes resolve against their own filesystems.
//! Results are cached only under the exact pane environment signature,
//! configuration generation, and bounded request that produced them.

use super::{
    ActionStatus, EventKind, MezError, PaneReadinessState, Result, RunningShellTransactionKind,
    RunningShellTransactionRef, RuntimeSessionService, RuntimeShellTransactionActionFailure,
    ShellTransaction, current_unix_millis, current_unix_seconds, json_escape,
    runtime_pane_readiness_state_name, runtime_random_marker_token,
};
use crate::runtime::RuntimePathResolutionCacheKey;
use mez_agent::ShellClassification;

const RUNTIME_PATH_RESOLUTION_TIMEOUT_MS: u64 = 10_000;

impl RuntimeSessionService {
    /// Builds the exact cache identity for one pane path-resolution request.
    pub(crate) fn path_resolution_cache_key(
        &self,
        pane_id: &str,
        request: &mez_agent::shell::PanePathResolutionRequest,
    ) -> Option<RuntimePathResolutionCacheKey> {
        let signature = self.pane_environment_signature(pane_id)?;
        Some(RuntimePathResolutionCacheKey {
            pane_id: pane_id.to_string(),
            environment_signature: signature.stable_hash(),
            config_generation: self.session.config_generation,
            request: request.clone(),
        })
    }

    /// Returns cached trusted authority only for the exact current identity.
    pub(crate) fn path_scopes_for_pane_request(
        &self,
        pane_id: &str,
        request: &mez_agent::shell::PanePathResolutionRequest,
    ) -> Result<Option<mez_agent::permissions::PathScopes>> {
        let key = self
            .path_resolution_cache_key(pane_id, request)
            .ok_or_else(|| {
                MezError::invalid_state("pane environment is unavailable for path resolution")
            })?;
        if let Some(reason) = self.process.pane_path_scope_failures.get(&key) {
            return Err(MezError::invalid_state(format!(
                "pane path resolution failed: {reason}"
            )));
        }
        Ok(self.process.pane_path_scopes.get(&key).cloned())
    }

    /// Dispatches a bounded read-only resolver through the pane shell.
    ///
    /// Returns `true` only when a new transaction was sent. A cached or already
    /// pending identical request returns `false` without duplicating work.
    pub(crate) fn dispatch_path_resolution_to_pane(
        &mut self,
        pane_id: &str,
        request: mez_agent::shell::PanePathResolutionRequest,
    ) -> Result<bool> {
        self.dispatch_path_resolution_to_pane_with_continuation(pane_id, request, None)
    }

    /// Dispatches path resolution retained as a prerequisite of one action.
    pub(crate) fn dispatch_action_path_resolution_to_pane(
        &mut self,
        turn: &mez_agent::AgentTurnRecord,
        action_id: &str,
        request: mez_agent::shell::PanePathResolutionRequest,
    ) -> Result<bool> {
        self.dispatch_path_resolution_to_pane_with_continuation(
            &turn.pane_id,
            request,
            Some((turn, action_id)),
        )
    }

    /// Dispatches one exact resolver request with an optional action continuation.
    fn dispatch_path_resolution_to_pane_with_continuation(
        &mut self,
        pane_id: &str,
        request: mez_agent::shell::PanePathResolutionRequest,
        continuation: Option<(&mez_agent::AgentTurnRecord, &str)>,
    ) -> Result<bool> {
        let cache_key = self
            .path_resolution_cache_key(pane_id, &request)
            .ok_or_else(|| {
                MezError::invalid_state("pane environment is unavailable for path resolution")
            })?;
        if let Some(reason) = self.process.pane_path_scope_failures.get(&cache_key) {
            return Err(MezError::invalid_state(format!(
                "pane path resolution failed: {reason}"
            )));
        }
        if self.process.pane_path_scopes.contains_key(&cache_key) {
            return Ok(false);
        }
        if let Some(transaction) =
            self.process
                .running_shell_transactions
                .values_mut()
                .find(|transaction| {
                    matches!(
                        &transaction.kind,
                        RunningShellTransactionKind::PathResolution {
                            cache_key: pending,
                            ..
                        } if pending == &cache_key
                    )
                })
        {
            if let Some((turn, action_id)) = continuation {
                let RunningShellTransactionKind::PathResolution { waiters, .. } =
                    &mut transaction.kind
                else {
                    return Err(MezError::invalid_state(
                        "matching path-resolution transaction has the wrong kind",
                    ));
                };
                let waiter = (turn.turn_id.clone(), action_id.to_string());
                if !waiters.contains(&waiter) {
                    waiters.push(waiter);
                }
            }
            return Ok(false);
        }
        self.require_pane_ready_for_agent_command(pane_id)?;

        let shell_identity = self.shell_execution_identity_for_pane(pane_id)?;
        let classification = shell_identity.classification();
        let command = mez_agent::shell::pane_path_resolution_command(&request, classification)
            .map_err(|error| MezError::invalid_args(error.message()))?;
        let (turn_id, agent_id, waiters) = continuation.map_or_else(
            || {
                (
                    format!("path-resolution-{pane_id}-{}", current_unix_seconds()),
                    format!("agent-{pane_id}"),
                    Vec::new(),
                )
            },
            |(turn, action_id)| {
                (
                    turn.turn_id.clone(),
                    turn.agent_id.clone(),
                    vec![(turn.turn_id.clone(), action_id.to_string())],
                )
            },
        );
        let marker = runtime_random_marker_token(&format!(
            "path-resolution\0{pane_id}\0{turn_id}\0{}",
            cache_key.environment_signature
        ))?;
        let marker_id = marker.as_str().to_string();
        let transaction = self.configure_shell_transaction_for_pane(
            pane_id,
            ShellTransaction::new(
                marker,
                &turn_id,
                &agent_id,
                pane_id,
                shell_identity.shell_path(),
                command.clone(),
            )?,
        );
        let transaction_input = transaction.render_for_classification_input(classification);
        self.require_generated_shell_input(&transaction_input)?;
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
        let requires_payload_receiver_ready =
            classification == ShellClassification::Fish && !transaction_input.payload.is_empty();
        self.remember_mez_wrapper_filter_command(pane_id, &command);
        let previous = self.pane_readiness_state(pane_id);
        self.set_pane_readiness(pane_id, PaneReadinessState::Busy);
        self.register_running_shell_transaction(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id,
                kind: RunningShellTransactionKind::PathResolution { cache_key, waiters },
                pane_id: pane_id.to_string(),
                command,
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(RUNTIME_PATH_RESOLUTION_TIMEOUT_MS),
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
        if requires_payload_receiver_ready {
            self.require_shell_transaction_payload_receiver_ready(&marker_id);
        }
        if let Some(receiver_payload) = receiver_payload {
            self.register_shell_receiver_payload(&marker_id, receiver_payload);
        }
        if let Err(error) = self.write_runtime_pane_shell_input(pane_id, wrapper.as_bytes()) {
            self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
            return Err(error);
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","path_resolution":"sent","marker":"{}","previous_state":"{}"}}"#,
                json_escape(pane_id),
                json_escape(&marker_id),
                runtime_pane_readiness_state_name(previous)
            ),
        )?;
        Ok(true)
    }

    /// Resumes every waiting action after successful or restrictive resolution.
    ///
    /// A stale cache identity remains fatal because it cannot safely attribute
    /// authority to the active pane environment.
    pub(crate) fn settle_action_path_resolution_transaction(
        &mut self,
        marker: &str,
        transaction: &RunningShellTransactionRef,
        cache_key: &RuntimePathResolutionCacheKey,
        waiters: &[(String, String)],
    ) -> Result<usize> {
        match self.path_scopes_for_pane_request(&transaction.pane_id, &cache_key.request) {
            Ok(Some(_)) => {
                for turn_id in waiters
                    .iter()
                    .map(|(turn_id, _)| turn_id)
                    .collect::<std::collections::BTreeSet<_>>()
                {
                    let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
                }
                Ok(1)
            }
            Ok(None) => self.fail_action_path_resolution_waiters(
                marker,
                transaction,
                waiters,
                ActionStatus::Failed,
                "bubblewrap_path_resolution_stale",
                "Bubblewrap action path resolution completed for a stale pane environment",
            ),
            Err(error) => self.fail_action_path_resolution_waiters(
                marker,
                transaction,
                waiters,
                ActionStatus::Failed,
                "bubblewrap_path_resolution_failed",
                error.message(),
            ),
        }
    }

    /// Resumes actions after a resolver safely degraded to reduced authority.
    pub(crate) fn resume_action_path_resolution_waiters(
        &mut self,
        waiters: &[(String, String)],
    ) -> Result<()> {
        for turn_id in waiters
            .iter()
            .map(|(turn_id, _)| turn_id)
            .collect::<std::collections::BTreeSet<_>>()
        {
            let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
        }
        Ok(())
    }

    /// Settles all actions whose shared path-resolution prerequisite failed.
    pub(crate) fn fail_action_path_resolution_waiters(
        &mut self,
        marker: &str,
        transaction: &RunningShellTransactionRef,
        waiters: &[(String, String)],
        status: ActionStatus,
        code: &str,
        message: &str,
    ) -> Result<usize> {
        let timed_out = status == ActionStatus::TimedOut;
        let message = crate::security::sandbox::bubblewrap_failure_remediation(message);
        let terminal_observation = serde_json::json!({
            "source": "pty",
            "stream": "pty_combined",
            "marker": marker,
            "exit_code": null,
            "timed_out": timed_out,
            "combined_output_bytes": transaction.observed_output_bytes,
            "boundary_state": "bubblewrap-path-resolution-failed",
            "output_truncated": transaction.observed_output_truncated
        });
        let mut waiters_by_turn = std::collections::BTreeMap::<String, Vec<String>>::new();
        for (turn_id, action_id) in waiters {
            waiters_by_turn
                .entry(turn_id.clone())
                .or_default()
                .push(action_id.clone());
        }
        let mut settled = 0;
        for (turn_id, action_ids) in waiters_by_turn {
            let mut waiter_transaction = transaction.clone();
            waiter_transaction.turn_id = turn_id;
            let failures = action_ids
                .into_iter()
                .map(|action_id| RuntimeShellTransactionActionFailure {
                    action_id,
                    status,
                    code: code.to_string(),
                    message: message.clone(),
                    sent_to_pane: false,
                    terminal_observation: terminal_observation.clone(),
                    trace_reason: code.to_string(),
                })
                .collect();
            settled +=
                self.fail_running_shell_transaction_actions(&waiter_transaction, marker, failures)?;
        }
        Ok(settled)
    }

    /// Settles one internal path-resolution transaction and caches only fresh,
    /// independently validated pane-shell evidence.
    pub(crate) fn observe_path_resolution_transaction_end(
        &mut self,
        marker: &str,
        pane_id: &str,
        exit_code: i32,
        cache_key: RuntimePathResolutionCacheKey,
        observed_output_preview: &str,
        observed_output_truncated: bool,
    ) -> Result<usize> {
        let mut outcome = "failed";
        let mut failure_reason = None;
        if exit_code == 0 && !observed_output_truncated {
            let current_key = self.path_resolution_cache_key(pane_id, &cache_key.request);
            if current_key.as_ref() == Some(&cache_key) {
                let resolved = mez_agent::shell::parse_pane_path_resolution_output(
                    observed_output_preview,
                    &cache_key.request,
                )
                .map_err(|error| error.message().to_string())
                .and_then(|parsed| {
                    parsed
                        .into_outcome(&cache_key.request)
                        .map_err(|error| error.message().to_string())
                });
                match resolved {
                    Ok(resolved) => {
                        for (path, reason) in &resolved.unavailable_paths {
                            self.append_sandbox_mapping_warning_once(
                                pane_id,
                                &format!("path:{}:{reason}", path),
                                &format!("path `{path}` ({reason})"),
                            )?;
                        }
                        self.process.pane_path_scope_failures.remove(&cache_key);
                        self.process
                            .pane_path_scopes
                            .insert(cache_key.clone(), resolved.scopes);
                        outcome = if resolved.unavailable_paths.is_empty() {
                            "completed"
                        } else {
                            "degraded"
                        };
                    }
                    Err(reason) => failure_reason = Some(reason),
                }
            } else {
                outcome = "stale";
            }
        } else if observed_output_truncated {
            outcome = "truncated";
            failure_reason = Some("resolver output was truncated".to_string());
        } else {
            failure_reason = Some(format!("resolver exited with status {exit_code}"));
        }
        if let Some(reason) = failure_reason
            && self
                .path_resolution_cache_key(pane_id, &cache_key.request)
                .as_ref()
                == Some(&cache_key)
        {
            self.process.pane_path_scope_failures.remove(&cache_key);
            let current_directory = self
                .pane_environment_signature(pane_id)
                .map(|signature| signature.working_directory.clone())
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "pane environment became unavailable during path resolution",
                    )
                })?;
            let scopes = mez_agent::permissions::PathScopes::try_shell_resolved(
                current_directory,
                Vec::new(),
                Vec::new(),
                Default::default(),
            )
            .map_err(|error| MezError::invalid_state(error.message()))?;
            self.process.pane_path_scopes.insert(cache_key, scopes);
            self.append_sandbox_mapping_warning_once(
                pane_id,
                &format!("path-resolution:{reason}"),
                &format!("configured paths could not be resolved ({reason})"),
            )?;
            outcome = "degraded";
        }
        if self.pane_readiness_state(pane_id) == PaneReadinessState::Busy {
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","path_resolution":"{}","marker":"{}","exit_code":{},"output_truncated":{}}}"#,
                json_escape(pane_id),
                outcome,
                json_escape(marker),
                exit_code,
                observed_output_truncated
            ),
        )?;
        Ok(1)
    }

    /// Degrades one failed resolver transaction to empty filesystem authority.
    pub(crate) fn fail_path_resolution_transaction(
        &mut self,
        marker: &str,
        transaction: &RunningShellTransactionRef,
        reason: &str,
    ) -> Result<()> {
        if let RunningShellTransactionKind::PathResolution { cache_key, .. } = &transaction.kind
            && self
                .path_resolution_cache_key(&transaction.pane_id, &cache_key.request)
                .as_ref()
                == Some(cache_key)
        {
            self.process.pane_path_scope_failures.remove(cache_key);
            let current_directory = self
                .pane_environment_signature(&transaction.pane_id)
                .map(|signature| signature.working_directory.clone())
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "pane environment became unavailable during path resolution",
                    )
                })?;
            let scopes = mez_agent::permissions::PathScopes::try_shell_resolved(
                current_directory,
                Vec::new(),
                Vec::new(),
                Default::default(),
            )
            .map_err(|error| MezError::invalid_state(error.message()))?;
            self.process
                .pane_path_scopes
                .insert(cache_key.clone(), scopes);
            self.append_sandbox_mapping_warning_once(
                &transaction.pane_id,
                &format!("path-resolution:{reason}"),
                &format!("configured paths could not be resolved ({reason})"),
            )?;
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","path_resolution":"degraded","marker":"{}","reason":"{}"}}"#,
                json_escape(&transaction.pane_id),
                json_escape(marker),
                json_escape(reason)
            ),
        )?;
        Ok(())
    }

    /// Degrades an expired resolver to empty authority and resumes its waiters.
    pub(crate) fn expire_path_resolution_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Ready);
        self.fail_path_resolution_transaction(
            marker,
            &transaction,
            &format!("timed out after {elapsed_ms} ms (limit {timeout_ms} ms)"),
        )?;
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> ready reason=path_resolution_degraded marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        if let RunningShellTransactionKind::PathResolution { waiters, .. } = &transaction.kind {
            self.resume_action_path_resolution_waiters(waiters)?;
        }
        Ok(())
    }

    /// Degrades a resolver whose input could not be written to empty authority.
    pub(crate) fn fail_path_resolution_for_pane_write_failure(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        error: &str,
    ) -> Result<()> {
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Ready);
        self.fail_path_resolution_transaction(
            marker,
            &transaction,
            &format!("pane input write failed: {error}"),
        )?;
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> ready reason=path_resolution_degraded marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        if let RunningShellTransactionKind::PathResolution { waiters, .. } = &transaction.kind {
            self.resume_action_path_resolution_waiters(waiters)?;
        }
        Ok(())
    }
}
