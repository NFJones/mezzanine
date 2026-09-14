//! Pane-local Bubblewrap environment evidence transactions.
//!
//! Values are retained only in protected runtime state. Logs, lifecycle events,
//! warnings, and traces expose configured names and redacted reason classes only.

use super::{
    EventKind, PaneReadinessState, Result, RunningShellTransactionKind, RunningShellTransactionRef,
    RuntimeSessionService, json_escape,
};
use crate::runtime::RuntimeEnvironmentEvidenceCacheKey;

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

    #[allow(
        dead_code,
        reason = "retained for settlement of legacy in-flight transactions"
    )]
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
        _turn: &mez_agent::AgentTurnRecord,
        _action_id: &str,
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
            BubblewrapEnvironmentProfile::ConfiguredForwarding => Some(
                crate::runtime::processes::native_workload_environment::server_environment_evidence(
                    request,
                    self.server_environment(),
                ),
            ),
        }
    }

    pub(crate) fn ensure_bubblewrap_environment_evidence_for_action(
        &mut self,
        turn: &mez_agent::AgentTurnRecord,
        action_id: &str,
    ) -> Result<bool> {
        let _ = (turn, action_id);
        // Ordinary Bubblewrap and Seatbelt workloads obtain their configured
        // environment directly from the immutable Mez-server snapshot.
        Ok(true)
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
