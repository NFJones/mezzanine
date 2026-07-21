//! Runtime agent audit-record helpers.
//!
//! This module owns audit metadata construction for shell, network, credential,
//! and provider activity initiated by the runtime agent. It keeps audit record
//! shaping close to policy-sensitive execution without mixing it into the main
//! agent facade.

use super::{
    AgentAction, AgentActionPayload, AgentTurnRecord, AuditActor, AuditRecord,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, MezError, ModelProfile, Result, RuntimeSessionService,
    exact_command_sha256, runtime_mezzanine_error_code, runtime_permission_preset_name,
    runtime_provider_audit_error_message,
};
use crate::security::sandbox::SandboxAuditSummary;
use mez_agent::permissions::{EffectCompleteness, PermissionEvaluation};

impl RuntimeSessionService {
    /// Runs the append agent shell command audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_shell_command_audit(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        command: &str,
        permission_evaluation: Option<&PermissionEvaluation>,
        sandbox_summary: Option<&SandboxAuditSummary>,
        outcome: &str,
    ) -> Result<()> {
        let fallback_audit = self
            .agent
            .sandbox_fallback_audits
            .get(&(turn.turn_id.clone(), action.id.clone()))
            .cloned();
        let fallback_bypass = fallback_audit.is_some()
            && self.sandbox_bypass_active_for_action(&turn.turn_id, &action.id);
        let sandbox_backend = if fallback_bypass {
            "policy-only".to_string()
        } else {
            self.configured_permissions().sandbox.as_str().to_string()
        };
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            "shell_command",
            "send_to_pane",
        )
        .with_pane_id(turn.pane_id.clone())
        .with_agent_id(turn.agent_id.clone())
        .with_metadata("turn_id", turn.turn_id.clone())
        .with_metadata("action_id", action.id.clone())
        .with_metadata(
            "command_sha256",
            exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, command),
        )
        .with_metadata("sandbox_backend", sandbox_backend);
        if let Some(summary) = sandbox_summary {
            record = record
                .with_metadata("sandbox_profile_version", summary.runtime_profile_version)
                .with_metadata(
                    "sandbox_authority_source",
                    summary.authority_source.as_str(),
                )
                .with_metadata(
                    "sandbox_read_only_mount_count",
                    summary.read_only_mount_count.to_string(),
                )
                .with_metadata(
                    "sandbox_read_write_mount_count",
                    summary.read_write_mount_count.to_string(),
                )
                .with_metadata(
                    "sandbox_protected_mask_count",
                    summary.protected_mask_count.to_string(),
                )
                .with_metadata("sandbox_plan_sha256", summary.plan_sha256.clone());
        }
        if let Some(evaluation) = permission_evaluation {
            record = record
                .with_metadata(
                    "matched_rule_ids",
                    serde_json::to_string(&evaluation.matched_rule_ids)
                        .unwrap_or_else(|_| "[]".to_string()),
                )
                .with_metadata(
                    "effect_completeness",
                    match evaluation.completeness {
                        EffectCompleteness::Unknown => "unknown",
                        EffectCompleteness::Complete => "complete",
                    },
                )
                .with_metadata("effect_unknown", evaluation.effects.unknown.to_string())
                .with_metadata("effect_network", evaluation.effects.network.to_string())
                .with_metadata(
                    "effect_credentials",
                    evaluation.effects.credentials.to_string(),
                )
                .with_metadata(
                    "effect_process_control",
                    evaluation.effects.process_control.to_string(),
                )
                .with_metadata(
                    "effect_read_count",
                    evaluation.effects.reads.len().to_string(),
                )
                .with_metadata(
                    "effect_write_count",
                    evaluation.effects.writes.len().to_string(),
                );
        }
        if let Some(fallback) = fallback_audit {
            record = record
                .with_metadata("sandbox_fallback", "approved_exact_retry")
                .with_metadata("sandbox_fallback_origin_backend", "bubblewrap")
                .with_metadata("sandbox_fallback_reason", fallback.reason)
                .with_metadata(
                    "sandbox_fallback_proof_sha256",
                    exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, &fallback.proof),
                )
                .with_metadata(
                    "sandbox_fallback_partial_effect_warning",
                    fallback.partial_effect_warning.to_string(),
                );
            if let Some(client_id) = fallback.approving_client_id {
                record = record.with_metadata("sandbox_fallback_approving_client", client_id);
            }
            record.approval_state = "approved_exact_sandbox_bypass".to_string();
        } else {
            record.approval_state = "not_required_or_preapproved".to_string();
        }
        record.policy_mode =
            runtime_permission_preset_name(self.integration.permission_policy().preset).to_string();
        record.outcome = outcome.to_string();
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Appends and consumes redacted outcome metadata for one approved
    /// unsandboxed Bubblewrap fallback retry.
    pub(crate) fn append_sandbox_fallback_result_audit(
        &mut self,
        turn_id: &str,
        action_id: &str,
        outcome: &str,
    ) -> Result<()> {
        let Some(fallback) = self
            .agent
            .sandbox_fallback_audits
            .remove(&(turn_id.to_string(), action_id.to_string()))
        else {
            return Ok(());
        };
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: turn_id.to_string(),
            },
            "shell_command",
            "sandbox_fallback_result",
        )
        .with_metadata("turn_id", turn_id.to_string())
        .with_metadata("action_id", action_id.to_string())
        .with_metadata("sandbox_fallback_origin_backend", "bubblewrap")
        .with_metadata("sandbox_fallback_reason", fallback.reason)
        .with_metadata(
            "sandbox_fallback_proof_sha256",
            exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, &fallback.proof),
        )
        .with_metadata(
            "sandbox_fallback_partial_effect_warning",
            fallback.partial_effect_warning.to_string(),
        );
        if let Some(client_id) = fallback.approving_client_id {
            record = record.with_metadata("sandbox_fallback_approving_client", client_id);
        }
        record.approval_state = "approved_exact_sandbox_bypass".to_string();
        record.outcome = outcome.to_string();
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Appends an audit event for a runtime-owned network action.
    ///
    /// URL and query values are hashed rather than stored directly so external
    /// content requests remain diagnosable without leaking sensitive inputs into
    /// the audit log.
    pub(crate) fn append_agent_network_action_audit(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        outcome: &str,
    ) -> Result<()> {
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            "external_integration",
            "runtime_network_action",
        )
        .with_pane_id(turn.pane_id.clone())
        .with_agent_id(turn.agent_id.clone())
        .with_metadata("turn_id", turn.turn_id.clone())
        .with_metadata("action_id", action.id.clone())
        .with_metadata("action_type", action.action_type().to_string());
        match &action.payload {
            AgentActionPayload::FetchUrl { url, .. } => {
                record = record.with_metadata(
                    "url_sha256",
                    exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, url),
                );
            }
            AgentActionPayload::WebSearch { query, domains, .. } => {
                record = record.with_metadata(
                    "query_sha256",
                    exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, query),
                );
                if !domains.is_empty() {
                    record = record.with_metadata("domain_count", domains.len().to_string());
                }
            }
            _ => {}
        }
        record.policy_mode =
            runtime_permission_preset_name(self.integration.permission_policy().preset).to_string();
        record.approval_state = "not_required_or_preapproved".to_string();
        record.outcome = outcome.to_string();
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Appends an audit event for a runtime-owned persistent-memory action.
    ///
    /// Freeform search and store content is hashed rather than written
    /// directly so durable-memory inputs remain diagnosable without leaking the
    /// full user-authored payload into the audit log.
    pub(crate) fn append_agent_memory_action_audit(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        outcome: &str,
    ) -> Result<()> {
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            "external_integration",
            "runtime_memory_action",
        )
        .with_pane_id(turn.pane_id.clone())
        .with_agent_id(turn.agent_id.clone())
        .with_metadata("turn_id", turn.turn_id.clone())
        .with_metadata("action_id", action.id.clone())
        .with_metadata("action_type", action.action_type().to_string());
        match &action.payload {
            AgentActionPayload::MemorySearch { query, limit } => {
                record = record
                    .with_metadata("query_bytes", query.len().to_string())
                    .with_metadata(
                        "query_sha256",
                        exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, query),
                    );
                if let Some(limit) = limit {
                    record = record.with_metadata("limit", limit.to_string());
                }
            }
            AgentActionPayload::MemoryStore {
                kind,
                priority,
                scope,
                keywords,
                content,
                expires_in_days,
            } => {
                record = record
                    .with_metadata("kind", kind.clone())
                    .with_metadata("priority", priority.unwrap_or(50).min(100).to_string())
                    .with_metadata(
                        "scope",
                        scope.clone().unwrap_or_else(|| "project".to_string()),
                    )
                    .with_metadata("keyword_count", keywords.len().to_string())
                    .with_metadata("content_bytes", content.len().to_string())
                    .with_metadata(
                        "content_sha256",
                        exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, content),
                    );
                if let Some(expires_in_days) = expires_in_days {
                    record = record.with_metadata("expires_in_days", expires_in_days.to_string());
                }
            }
            _ => {}
        }
        record.policy_mode =
            runtime_permission_preset_name(self.integration.permission_policy().preset).to_string();
        record.approval_state = "not_required_or_preapproved".to_string();
        record.outcome = outcome.to_string();
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Runs the append credential access audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_credential_access_audit(
        &mut self,
        provider: &str,
        credential_id: &str,
        purpose: &str,
        outcome: &str,
    ) -> Result<()> {
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let record = AuditRecord::credential_access_attempt(
            self.session.id.to_string(),
            AuditActor {
                kind: "runtime".to_string(),
                id: "provider".to_string(),
            },
            provider.to_string(),
            credential_id.to_string(),
            purpose.to_string(),
            outcome.to_string(),
        );
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Runs the append provider request audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_provider_request_audit(
        &mut self,
        turn: &AgentTurnRecord,
        model_profile: &ModelProfile,
        provider_id: &str,
        outcome: &str,
    ) -> Result<()> {
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let record = AuditRecord::provider_request(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            provider_id.to_string(),
            model_profile.model.clone(),
            turn.turn_id.clone(),
            outcome.to_string(),
        )
        .with_agent_id(turn.agent_id.clone())
        .with_pane_id(turn.pane_id.clone());
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Runs the append provider request failure audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_provider_request_failure_audit(
        &mut self,
        turn: &AgentTurnRecord,
        model_profile: &ModelProfile,
        provider_id: &str,
        error: &MezError,
    ) -> Result<()> {
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::provider_request(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            provider_id.to_string(),
            model_profile.model.clone(),
            turn.turn_id.clone(),
            "failed",
        )
        .with_agent_id(turn.agent_id.clone())
        .with_pane_id(turn.pane_id.clone())
        .with_metadata("error_kind", runtime_mezzanine_error_code(error.kind()))
        .with_metadata(
            "error_message",
            runtime_provider_audit_error_message(error.message()),
        );
        if let Some(raw_text) = error.provider_raw_text() {
            record = record
                .with_metadata("provider_raw_text_bytes", raw_text.len().to_string())
                .with_metadata(
                    "provider_raw_text_sha256",
                    exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, raw_text),
                );
        }
        if let Some(failure_json) = error.provider_failure_json() {
            record = record
                .with_metadata("provider_failure_json", failure_json.to_string())
                .with_metadata(
                    "provider_failure_json_bytes",
                    failure_json.len().to_string(),
                )
                .with_metadata(
                    "provider_failure_json_sha256",
                    exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, failure_json),
                );
        }
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }
}
