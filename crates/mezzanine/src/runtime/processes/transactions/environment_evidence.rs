//! Pane-local Bubblewrap environment evidence transactions.
//!
//! Values are retained only in protected runtime state. Logs, lifecycle events,
//! warnings, and traces expose configured names and redacted reason classes only.

use super::{
    EventKind, PaneReadinessState, Result, RunningShellTransactionKind, RunningShellTransactionRef,
    RuntimeSessionService, ShellTransaction, current_unix_millis, json_escape,
    runtime_marker_for_action,
};
use crate::runtime::RuntimeEnvironmentEvidenceCacheKey;

const ENVIRONMENT_EVIDENCE_TIMEOUT_MS: u64 = 10_000;

/// Selects how one Bubblewrap workload obtains optional pane environment
/// values without weakening the fixed sandbox environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BubblewrapEnvironmentProfile {
    /// Resolve and forward the configured pane variables for ordinary actions.
    ConfiguredForwarding,
    /// Omit configured pane variables from internal semantic patch phases.
    SemanticPatchNoForwarding,
}

impl RuntimeSessionService {
    fn environment_evidence_cache_key(
        &self,
        pane_id: &str,
        turn_id: &str,
        action_id: &str,
        request: &mez_agent::shell::PaneEnvironmentRequest,
    ) -> Option<RuntimeEnvironmentEvidenceCacheKey> {
        let signature = self.pane_environment_signature(pane_id)?;
        Some(RuntimeEnvironmentEvidenceCacheKey {
            pane_id: pane_id.to_string(),
            environment_signature: signature.stable_hash(),
            config_generation: self.session.config_generation,
            turn_id: turn_id.to_string(),
            action_id: action_id.to_string(),
            request: request.clone(),
        })
    }

    pub(crate) fn pane_environment_evidence(
        &self,
        turn: &mez_agent::AgentTurnRecord,
        action_id: &str,
        request: &mez_agent::shell::PaneEnvironmentRequest,
    ) -> Option<mez_agent::shell::PaneEnvironmentEvidence> {
        let key =
            self.environment_evidence_cache_key(&turn.pane_id, &turn.turn_id, action_id, request)?;
        self.process.pane_environment_evidence.get(&key).cloned()
    }

    /// Resolves the exact environment evidence used by both Bubblewrap
    /// capability probing and workload compilation for one action profile.
    pub(crate) fn bubblewrap_environment_evidence_for_action(
        &self,
        turn: &mez_agent::AgentTurnRecord,
        action_id: &str,
        request: &mez_agent::shell::PaneEnvironmentRequest,
        profile: BubblewrapEnvironmentProfile,
    ) -> Option<mez_agent::shell::PaneEnvironmentEvidence> {
        match profile {
            BubblewrapEnvironmentProfile::SemanticPatchNoForwarding => {
                Some(mez_agent::shell::PaneEnvironmentEvidence::restrictive(
                    request,
                    "semantic_patch_not_forwarded",
                ))
            }
            BubblewrapEnvironmentProfile::ConfiguredForwarding if request.names.is_empty() => Some(
                mez_agent::shell::PaneEnvironmentEvidence::restrictive(request, "not_configured"),
            ),
            BubblewrapEnvironmentProfile::ConfiguredForwarding => {
                self.pane_environment_evidence(turn, action_id, request)
            }
        }
    }

    pub(crate) fn ensure_bubblewrap_environment_evidence_for_action(
        &mut self,
        turn: &mez_agent::AgentTurnRecord,
        action_id: &str,
    ) -> Result<bool> {
        let policy = self.permission_policy_for_turn(turn);
        let crate::runtime::SandboxConfig::Bubblewrap(config) =
            self.sandbox_config_for_pane(&turn.pane_id)
        else {
            return Ok(true);
        };
        if !crate::runtime::config::bubblewrap_applies_to_policy(
            &crate::runtime::SandboxConfig::Bubblewrap(config.clone()),
            &policy,
        ) || config.env_whitelist.requested_names.is_empty()
        {
            return Ok(true);
        }
        let request = mez_agent::shell::PaneEnvironmentRequest::new(
            config.env_whitelist.requested_names.clone(),
        )
        .map_err(|error| crate::MezError::invalid_args(error.message()))?;
        let cache_key = self
            .environment_evidence_cache_key(&turn.pane_id, &turn.turn_id, action_id, &request)
            .ok_or_else(|| {
                crate::MezError::invalid_state(
                    "pane environment is unavailable for environment forwarding",
                )
            })?;
        if self
            .process
            .pane_environment_evidence
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
                        RunningShellTransactionKind::EnvironmentEvidence { cache_key: pending, .. }
                            if pending == &cache_key
                    )
                })
        {
            let RunningShellTransactionKind::EnvironmentEvidence { waiters, .. } =
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
        let classification = self.shell_classification_for_pane(&turn.pane_id);
        let command = mez_agent::shell::pane_environment_evidence_command(&request, classification)
            .map_err(|error| crate::MezError::invalid_args(error.message()))?;
        let marker = runtime_marker_for_action(turn, &format!("environment-evidence-{action_id}"))?;
        let marker_id = marker.as_str().to_string();
        let transaction = ShellTransaction::new(
            marker,
            &turn.turn_id,
            &turn.agent_id,
            &turn.pane_id,
            self.session.shell.path(),
            command.clone(),
        )?;
        let input = transaction.render_for_classification_input(classification);
        let mut wrapper = input.wrapper;
        if !wrapper.ends_with(char::from(10)) {
            wrapper.push(char::from(10));
        }
        self.remember_mez_wrapper_filter_command(&turn.pane_id, &command);
        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Busy);
        self.register_running_shell_transaction(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: turn.turn_id.clone(),
                kind: RunningShellTransactionKind::EnvironmentEvidence {
                    cache_key,
                    waiters: vec![(turn.turn_id.clone(), action_id.to_string())],
                },
                pane_id: turn.pane_id.clone(),
                command,
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(ENVIRONMENT_EVIDENCE_TIMEOUT_MS),
                pending_input_payload: (!input.payload.is_empty())
                    .then(|| input.payload.into_bytes()),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
            true,
        );
        if let Err(error) = self.write_runtime_pane_input(&turn.pane_id, wrapper.as_bytes()) {
            self.fail_shell_transactions_for_pane_write_failure(&turn.pane_id, error.message())?;
            return Err(error);
        }
        self.append_lifecycle_event(EventKind::AgentStatus, format!(
            r#"{{"pane_id":"{}","environment_evidence":"sent","marker":"{}","requested_count":{}}}"#,
            json_escape(&turn.pane_id), json_escape(&marker_id), request.names.len()
        ))?;
        Ok(false)
    }

    fn cache_restrictive_environment_evidence(
        &mut self,
        transaction: &RunningShellTransactionRef,
        cache_key: &RuntimeEnvironmentEvidenceCacheKey,
        reason: &str,
    ) -> Result<()> {
        let current = self.environment_evidence_cache_key(
            &transaction.pane_id,
            &cache_key.turn_id,
            &cache_key.action_id,
            &cache_key.request,
        );
        let Some(current_key) = current else {
            return Ok(());
        };
        let evidence =
            mez_agent::shell::PaneEnvironmentEvidence::restrictive(&cache_key.request, reason);
        for name in &cache_key.request.names {
            self.append_sandbox_mapping_warning_once(
                &transaction.pane_id,
                &format!("environment:{name}:{reason}"),
                &format!("environment variable `{name}` ({reason})"),
            )?;
        }
        self.process
            .pane_environment_evidence
            .insert(current_key, evidence);
        Ok(())
    }

    pub(crate) fn observe_environment_evidence_transaction_end(
        &mut self,
        marker: &str,
        transaction: &RunningShellTransactionRef,
        exit_code: i32,
        cache_key: &RuntimeEnvironmentEvidenceCacheKey,
        waiters: &[(String, String)],
    ) -> Result<usize> {
        let fresh = self
            .environment_evidence_cache_key(
                &transaction.pane_id,
                &cache_key.turn_id,
                &cache_key.action_id,
                &cache_key.request,
            )
            .as_ref()
            == Some(cache_key);
        if exit_code == 0 && !transaction.observed_output_truncated && fresh {
            match mez_agent::shell::parse_pane_environment_evidence(
                &transaction.observed_output_preview,
                &cache_key.request,
            ) {
                Ok(mut evidence) => {
                    let reserved = evidence
                        .values
                        .keys()
                        .filter(|name| {
                            matches!(
                                name.as_str(),
                                "HOME"
                                    | "TMPDIR"
                                    | "LANG"
                                    | "LC_ALL"
                                    | "USER"
                                    | "LOGNAME"
                                    | "SHELL"
                                    | "XDG_CACHE_HOME"
                                    | "XDG_CONFIG_HOME"
                                    | "XDG_DATA_HOME"
                                    | "XDG_STATE_HOME"
                                    | "GIT_CONFIG_NOSYSTEM"
                                    | "GIT_CONFIG_GLOBAL"
                                    | "GIT_CONFIG_COUNT"
                            ) || name.starts_with("GIT_CONFIG_KEY_")
                                || name.starts_with("GIT_CONFIG_VALUE_")
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    for name in reserved {
                        evidence.values.remove(&name);
                        evidence.omitted.insert(name, "reserved".to_string());
                    }
                    evidence = mez_agent::shell::PaneEnvironmentEvidence::from_parts(
                        &cache_key.request,
                        evidence.values,
                        evidence.omitted,
                    )
                    .map_err(|error| crate::MezError::invalid_state(error.message()))?;
                    for (name, reason) in &evidence.omitted {
                        self.append_sandbox_mapping_warning_once(
                            &transaction.pane_id,
                            &format!("environment:{name}:{reason}"),
                            &format!("environment variable `{name}` ({reason})"),
                        )?;
                    }
                    self.process
                        .pane_environment_evidence
                        .insert(cache_key.clone(), evidence);
                }
                Err(_) => self.cache_restrictive_environment_evidence(
                    transaction,
                    cache_key,
                    "protocol_invalid",
                )?,
            }
        } else {
            let reason = if !fresh {
                "stale"
            } else if transaction.observed_output_truncated {
                "truncated"
            } else {
                "resolver_failed"
            };
            self.cache_restrictive_environment_evidence(transaction, cache_key, reason)?;
        }
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Ready);
        self.append_lifecycle_event(EventKind::AgentStatus, format!(
            r#"{{"pane_id":"{}","environment_evidence":"settled","marker":"{}","requested_count":{}}}"#,
            json_escape(&transaction.pane_id), json_escape(marker), cache_key.request.names.len()
        ))?;
        for turn_id in waiters
            .iter()
            .map(|(turn_id, _)| turn_id)
            .collect::<std::collections::BTreeSet<_>>()
        {
            let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
        }
        Ok(1)
    }

    pub(crate) fn degrade_environment_evidence_transaction(
        &mut self,
        marker: &str,
        transaction: &RunningShellTransactionRef,
        reason: &str,
    ) -> Result<()> {
        let RunningShellTransactionKind::EnvironmentEvidence { cache_key, waiters } =
            &transaction.kind
        else {
            return Ok(());
        };
        self.cache_restrictive_environment_evidence(transaction, cache_key, reason)?;
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Ready);
        self.append_lifecycle_event(EventKind::AgentStatus, format!(
            r#"{{"pane_id":"{}","environment_evidence":"degraded","marker":"{}","reason":"{}"}}"#,
            json_escape(&transaction.pane_id), json_escape(marker), json_escape(reason)
        ))?;
        for turn_id in waiters
            .iter()
            .map(|(turn_id, _)| turn_id)
            .collect::<std::collections::BTreeSet<_>>()
        {
            let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
        }
        Ok(())
    }
}
