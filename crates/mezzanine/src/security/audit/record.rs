//! Constructors and sanitization logic for audit records.
//!
//! Record helpers classify common security-sensitive events while keeping the
//! append-only writer free from domain policy decisions.

use std::collections::BTreeMap;

use super::redaction::{redact_optional_record_field, redact_record_field, redact_secret_like};
use super::time::current_timestamp;
use super::types::{AuditActor, AuditRecord};

impl AuditRecord {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(
        session_id: impl Into<String>,
        actor: AuditActor,
        event_type: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            version: 1,
            event_id: 0,
            timestamp: current_timestamp(),
            session_id: session_id.into(),
            window_id: None,
            pane_id: None,
            agent_id: None,
            actor,
            event_type: event_type.into(),
            action: action.into(),
            policy_mode: "default".to_string(),
            approval_state: "not_required".to_string(),
            outcome: "unknown".to_string(),
            redactions: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    /// Runs the with metadata operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Runs the with window id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn with_window_id(mut self, window_id: impl Into<String>) -> Self {
        self.window_id = Some(window_id.into());
        self
    }

    /// Runs the with pane id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_pane_id(mut self, pane_id: impl Into<String>) -> Self {
        self.pane_id = Some(pane_id.into());
        self
    }

    /// Runs the with agent id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    /// Runs the permission decision operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn permission_decision(
        session_id: impl Into<String>,
        actor: AuditActor,
        permission_id: impl Into<String>,
        action_kind: impl Into<String>,
        decision: impl Into<String>,
        policy_mode: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let decision = decision.into();
        let mut record = Self::new(session_id, actor, "permission", "decision")
            .with_metadata("permission_id", permission_id)
            .with_metadata("action_kind", action_kind)
            .with_metadata("decision", decision.clone());
        record.policy_mode = policy_mode.into();
        record.approval_state = decision;
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the approval decision operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn approval_decision(
        session_id: impl Into<String>,
        actor: AuditActor,
        approval_id: impl Into<String>,
        requester: impl Into<String>,
        decision: impl Into<String>,
        scope: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let decision = decision.into();
        let mut record = Self::new(session_id, actor, "approval", "decision")
            .with_metadata("approval_id", approval_id)
            .with_metadata("requester", requester)
            .with_metadata("decision", decision.clone())
            .with_metadata("scope", scope);
        record.approval_state = decision;
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the approval prompt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn approval_prompt(
        session_id: impl Into<String>,
        actor: AuditActor,
        approval_id: impl Into<String>,
        requester: impl Into<String>,
        action_kind: impl Into<String>,
        scope: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let mut record = Self::new(session_id, actor, "approval", "prompt")
            .with_metadata("approval_id", approval_id)
            .with_metadata("requester", requester)
            .with_metadata("action_kind", action_kind)
            .with_metadata("scope", scope);
        record.approval_state = "pending".to_string();
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the observer decision operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn observer_decision(
        session_id: impl Into<String>,
        actor: AuditActor,
        target_kind: impl Into<String>,
        target_id: impl Into<String>,
        decision: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let target_kind = target_kind.into();
        let target_id = target_id.into();
        let decision = decision.into();
        let mut record = Self::new(session_id, actor, "observer", "decision")
            .with_metadata("target_kind", target_kind.clone())
            .with_metadata("decision", decision.clone());
        record = match target_kind.as_str() {
            "client" => record.with_metadata("client_id", target_id),
            _ => record.with_metadata("observer_request_id", target_id),
        };
        record.approval_state = decision;
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the auth change operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn auth_change(
        session_id: impl Into<String>,
        actor: AuditActor,
        provider: impl Into<String>,
        account_id: impl Into<String>,
        change: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let change = change.into();
        let mut record = Self::new(session_id, actor, "auth", change.clone())
            .with_metadata("provider", provider)
            .with_metadata("account_id", account_id)
            .with_metadata("change", change);
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the logout operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn logout(
        session_id: impl Into<String>,
        actor: AuditActor,
        provider: impl Into<String>,
        account_id: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let mut record = Self::new(session_id, actor, "auth", "logout")
            .with_metadata("provider", provider)
            .with_metadata("account_id", account_id);
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the mcp call operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mcp_call(
        session_id: impl Into<String>,
        actor: AuditActor,
        server_id: impl Into<String>,
        tool_name: impl Into<String>,
        call_id: impl Into<String>,
        arguments_json: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let mut record = Self::new(session_id, actor, "external_integration", "mcp_call")
            .with_metadata("server_id", server_id)
            .with_metadata("tool_name", tool_name)
            .with_metadata("call_id", call_id)
            .with_metadata("arguments_json", arguments_json);
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the provider request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn provider_request(
        session_id: impl Into<String>,
        actor: AuditActor,
        provider: impl Into<String>,
        model: impl Into<String>,
        turn_id: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let mut record = Self::new(
            session_id,
            actor,
            "external_integration",
            "provider_request",
        )
        .with_metadata("provider", provider)
        .with_metadata("model", model)
        .with_metadata("turn_id", turn_id);
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the local protocol bridge change operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn local_protocol_bridge_change(
        session_id: impl Into<String>,
        actor: AuditActor,
        protocol: impl Into<String>,
        bridge_id: impl Into<String>,
        change: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let change = change.into();
        let mut record = Self::new(session_id, actor, "local_protocol_bridge", change.clone())
            .with_metadata("protocol", protocol)
            .with_metadata("bridge_id", bridge_id)
            .with_metadata("change", change);
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the config change operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn config_change(
        session_id: impl Into<String>,
        actor: AuditActor,
        scope: impl Into<String>,
        key: impl Into<String>,
        operation: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let operation = operation.into();
        let mut record = Self::new(session_id, actor, "configuration", operation.clone())
            .with_metadata("scope", scope)
            .with_metadata("key", key)
            .with_metadata("operation", operation);
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the snapshot operation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn snapshot_operation(
        session_id: impl Into<String>,
        actor: AuditActor,
        snapshot_id: impl Into<String>,
        operation: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let operation = operation.into();
        let mut record = Self::new(session_id, actor, "snapshot", operation.clone())
            .with_metadata("snapshot_id", snapshot_id)
            .with_metadata("operation", operation);
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the subagent spawn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn subagent_spawn(
        session_id: impl Into<String>,
        actor: AuditActor,
        parent_agent_id: impl Into<String>,
        subagent_id: impl Into<String>,
        role: impl Into<String>,
        cooperation_mode: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let subagent_id = subagent_id.into();
        let mut record = Self::new(session_id, actor, "subagent", "spawn")
            .with_metadata("parent_agent_id", parent_agent_id)
            .with_metadata("subagent_id", subagent_id.clone())
            .with_metadata("role", role)
            .with_metadata("cooperation_mode", cooperation_mode);
        record.agent_id = Some(subagent_id);
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the credential access attempt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn credential_access_attempt(
        session_id: impl Into<String>,
        actor: AuditActor,
        provider: impl Into<String>,
        credential_id: impl Into<String>,
        purpose: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let mut record = Self::new(session_id, actor, "credential", "access_attempt")
            .with_metadata("provider", provider)
            .with_metadata("credential_id", credential_id)
            .with_metadata("purpose", purpose);
        record.outcome = outcome.into();
        record.sanitized()
    }

    /// Runs the sanitized operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn sanitized(mut self) -> Self {
        redact_record_field(&mut self.session_id, &mut self.redactions, "session_id");
        redact_optional_record_field(&mut self.window_id, &mut self.redactions, "window_id");
        redact_optional_record_field(&mut self.pane_id, &mut self.redactions, "pane_id");
        redact_optional_record_field(&mut self.agent_id, &mut self.redactions, "agent_id");
        redact_record_field(&mut self.actor.kind, &mut self.redactions, "actor.kind");
        redact_record_field(&mut self.actor.id, &mut self.redactions, "actor.id");
        redact_record_field(&mut self.event_type, &mut self.redactions, "event_type");
        redact_record_field(&mut self.action, &mut self.redactions, "action");
        redact_record_field(&mut self.policy_mode, &mut self.redactions, "policy_mode");
        redact_record_field(
            &mut self.approval_state,
            &mut self.redactions,
            "approval_state",
        );
        redact_record_field(&mut self.outcome, &mut self.redactions, "outcome");
        let mut sanitized_metadata = BTreeMap::new();
        for (mut key, mut value) in std::mem::take(&mut self.metadata) {
            let before_key = key.clone();
            let before_value = value.clone();
            let (redacted_key, key_changed) = redact_secret_like(&key);
            if key_changed {
                key = redacted_key;
            }
            let (redacted_value, value_changed) = redact_secret_like(&value);
            if value_changed {
                value = redacted_value;
            }
            if key != before_key || value != before_value {
                self.redactions.push("metadata".to_string());
            }
            sanitized_metadata.insert(key, value);
        }
        self.metadata = sanitized_metadata;
        self.redactions.sort();
        self.redactions.dedup();
        self
    }
}
