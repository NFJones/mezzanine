//! Pane-shell canonical path-resolution transaction dispatch and settlement.
//!
//! Resolution runs through the visible pane execution environment so local,
//! SSH, container, and chroot panes resolve against their own filesystems.
//! Results are cached only under the exact pane environment signature,
//! configuration generation, and bounded request that produced them.

use super::{
    EventKind, MezError, PaneReadinessState, Result, RunningShellTransactionKind,
    RunningShellTransactionRef, RuntimeSessionService, ShellTransaction, current_unix_millis,
    current_unix_seconds, json_escape, runtime_pane_readiness_state_name,
    runtime_random_marker_token,
};
use crate::runtime::RuntimePathResolutionCacheKey;

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
        if self.process.pane_path_scopes.contains_key(&cache_key)
            || self
                .process
                .running_shell_transactions
                .values()
                .any(|transaction| {
                    matches!(
                        &transaction.kind,
                        RunningShellTransactionKind::PathResolution { cache_key: pending }
                            if pending == &cache_key
                    )
                })
        {
            return Ok(false);
        }
        self.require_pane_ready_for_agent_command(pane_id)?;

        let classification = self.shell_classification_for_pane(pane_id);
        let command = mez_agent::shell::pane_path_resolution_command(&request, classification)
            .map_err(|error| MezError::invalid_args(error.message()))?;
        let turn_id = format!("path-resolution-{pane_id}-{}", current_unix_seconds());
        let agent_id = format!("agent-{pane_id}");
        let marker = runtime_random_marker_token(&format!(
            "path-resolution\0{pane_id}\0{turn_id}\0{}",
            cache_key.environment_signature
        ))?;
        let marker_id = marker.as_str().to_string();
        let transaction = ShellTransaction::new(
            marker,
            &turn_id,
            &agent_id,
            pane_id,
            self.session.shell.path(),
            command.clone(),
        )?;
        let transaction_input = transaction.render_for_classification_input(classification);
        let mut wrapper = transaction_input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        self.remember_mez_wrapper_filter_command(pane_id, &command);
        self.write_runtime_pane_input(pane_id, wrapper.as_bytes())?;
        let previous = self.pane_readiness_state(pane_id);
        self.set_pane_readiness(pane_id, PaneReadinessState::Busy);
        self.process.running_shell_transactions.insert(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id,
                kind: RunningShellTransactionKind::PathResolution { cache_key },
                pane_id: pane_id.to_string(),
                command,
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(RUNTIME_PATH_RESOLUTION_TIMEOUT_MS),
                pending_input_payload: (!transaction_input.payload.is_empty())
                    .then(|| transaction_input.payload.into_bytes()),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
        );
        self.process
            .shell_transaction_require_start_markers
            .insert(marker_id.clone());
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

    /// Settles one internal path-resolution transaction and caches only fresh,
    /// complete, validated pane-shell evidence.
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
                        .into_path_scopes(&cache_key.request)
                        .map_err(|error| error.message().to_string())
                });
                match resolved {
                    Ok(scopes) => {
                        self.process
                            .pane_path_scopes
                            .retain(|key, _| key.pane_id != pane_id);
                        self.process
                            .pane_path_scope_failures
                            .retain(|key, _| key.pane_id != pane_id);
                        self.process
                            .pane_path_scopes
                            .insert(cache_key.clone(), scopes);
                        outcome = "completed";
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
            self.process
                .pane_path_scopes
                .retain(|key, _| key.pane_id != pane_id);
            self.process
                .pane_path_scope_failures
                .retain(|key, _| key.pane_id != pane_id);
            self.process
                .pane_path_scope_failures
                .insert(cache_key, reason);
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

    /// Fails one internal path-resolution transaction without retaining any
    /// authority produced by the failed request.
    pub(crate) fn fail_path_resolution_transaction(
        &mut self,
        marker: &str,
        transaction: &RunningShellTransactionRef,
        reason: &str,
    ) -> Result<()> {
        self.process
            .pane_path_scopes
            .retain(|key, _| key.pane_id != transaction.pane_id);
        if let RunningShellTransactionKind::PathResolution { cache_key } = &transaction.kind
            && self
                .path_resolution_cache_key(&transaction.pane_id, &cache_key.request)
                .as_ref()
                == Some(cache_key)
        {
            self.process
                .pane_path_scope_failures
                .retain(|key, _| key.pane_id != transaction.pane_id);
            self.process
                .pane_path_scope_failures
                .insert(cache_key.clone(), reason.to_string());
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","path_resolution":"failed","marker":"{}","reason":"{}"}}"#,
                json_escape(&transaction.pane_id),
                json_escape(marker),
                json_escape(reason)
            ),
        )?;
        Ok(())
    }

    /// Expires a resolver without retaining stale or partial path authority.
    pub(crate) fn expire_path_resolution_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.fail_path_resolution_transaction(
            marker,
            &transaction,
            &format!("timed out after {elapsed_ms} ms (limit {timeout_ms} ms)"),
        )?;
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=path_resolution_timeout marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        Ok(())
    }

    /// Settles a resolver whose pane input could not be written.
    pub(crate) fn fail_path_resolution_for_pane_write_failure(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        error: &str,
    ) -> Result<()> {
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.fail_path_resolution_transaction(
            marker,
            &transaction,
            &format!("pane input write failed: {error}"),
        )?;
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=path_resolution_pane_input_write_failed marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        Ok(())
    }
}
