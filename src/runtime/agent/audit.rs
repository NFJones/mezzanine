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
        );
        record.policy_mode =
            runtime_permission_preset_name(self.integration.permission_policy().preset).to_string();
        record.approval_state = "not_required_or_preapproved".to_string();
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
