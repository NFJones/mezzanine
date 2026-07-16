//! Control, diagnostics, process, terminal, permission, approval, and memory stores.

use super::*;

impl RuntimeSessionService {
    /// Runs the control idempotency operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn control_idempotency(&self) -> &ControlIdempotencyCache {
        &self.control_idempotency
    }

    /// Appends a runtime diagnostic event for async worker status that has
    /// re-entered the single-owner actor path.
    pub(crate) fn append_runtime_diagnostic_event(&mut self, payload: String) -> Result<()> {
        self.append_lifecycle_event(EventKind::Diagnostic, payload)
    }

    /// Applies one persistence-worker completion through the transport-neutral transition contract.
    pub(crate) fn apply_persistence_transition(
        &mut self,
        event: crate::runtime::PersistenceEvent,
    ) -> Result<crate::runtime::RuntimeTransition> {
        let payload = match event {
            crate::runtime::PersistenceEvent::Completed {
                target,
                path,
                bytes,
            } => serde_json::json!({
                "worker": "async-persistence",
                "target": target.as_str(),
                "path": path.to_string_lossy(),
                "state": "completed",
                "bytes": bytes,
            })
            .to_string(),
            crate::runtime::PersistenceEvent::Failed {
                target,
                path,
                error,
            } => {
                if target == crate::runtime::PersistenceTarget::PanePipe {
                    let _ =
                        self.stop_file_pane_pipes_for_path(path.as_path(), "persistence-failed")?;
                }
                serde_json::json!({
                    "worker": "async-persistence",
                    "target": target.as_str(),
                    "path": path.to_string_lossy(),
                    "state": "failed",
                    "error": error,
                })
                .to_string()
            }
        };
        self.append_runtime_diagnostic_event(payload)?;
        Ok(crate::runtime::RuntimeTransition {
            applied: true,
            side_effects: Vec::new(),
        })
    }

    /// Runs the message service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn message_service(&self) -> &MessageService {
        &self.message_service
    }

    /// Runs the message service mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn message_service_mut(&mut self) -> &mut MessageService {
        &mut self.message_service
    }

    /// Runs the record pane transcript ref operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn record_pane_transcript_ref(
        &mut self,
        pane_id: impl Into<String>,
        transcript_ref: impl Into<String>,
    ) -> Result<()> {
        let pane_id = pane_id.into();
        let transcript_ref = transcript_ref.into();
        if self.find_pane_descriptor(&pane_id).is_none() {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane not found for transcript reference",
            ));
        }
        if transcript_ref.trim().is_empty() {
            return Err(MezError::invalid_args(
                "pane transcript reference must not be empty",
            ));
        }
        if transcript_ref.contains(MEZ_ENV_FIELD_SEPARATOR) {
            return Err(MezError::invalid_args(
                "pane transcript reference contains reserved separator",
            ));
        }
        self.persistence
            .record_pane_transcript_ref(pane_id, transcript_ref);
        Ok(())
    }

    /// Runs the permission policy operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn permission_policy(&self) -> &PermissionPolicy {
        &self.permission_policy
    }

    /// Runs the permission policy mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn permission_policy_mut(&mut self) -> &mut PermissionPolicy {
        &mut self.permission_policy
    }

    /// Applies an explicit user-selected approval-bypass state.
    ///
    /// # Parameters
    /// - `active`: Whether approval bypass should be active after the change.
    pub fn set_live_approval_bypass_override(&mut self, active: bool) {
        self.live_approval_bypass_override = Some(active);
        self.permission_policy.set_approval_bypass(active);
    }

    /// Applies an explicit user-selected approval policy override.
    ///
    /// # Parameters
    /// - `policy`: Approval policy that should survive unrelated config reloads.
    pub fn set_live_approval_policy_override(&mut self, policy: mez_agent::ApprovalPolicy) {
        self.live_approval_policy_override = Some(policy);
        self.permission_policy.approval_policy = policy;
    }

    /// Runs the blocked approvals operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn blocked_approvals(&self) -> &BlockedApprovalQueue {
        &self.blocked_approvals
    }

    /// Runs the session approvals operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn session_approvals(&self) -> &SessionApprovalStore {
        &self.session_approvals
    }

    /// Runs the session approvals mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn session_approvals_mut(&mut self) -> &mut SessionApprovalStore {
        &mut self.session_approvals
    }

    /// Runs the queue blocked approval operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn queue_blocked_approval(&mut self, request: BlockedApprovalRequest) -> Result<String> {
        let approval_id = self
            .blocked_approvals
            .create_at(request, current_unix_seconds())?;
        let approval = self
            .blocked_approvals
            .get(&approval_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("blocked approval was not retained"))?;
        self.append_blocked_approval_prompt_audit(&approval)?;
        self.append_primary_lifecycle_event(
            EventKind::ApprovalChanged,
            format!(
                r#"{{"approval_id":"{}","state":"pending"}}"#,
                json_escape(&approval_id)
            ),
        )?;
        Ok(approval_id)
    }

    /// Runs the append blocked approval prompt audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_blocked_approval_prompt_audit(
        &mut self,
        approval: &BlockedApprovalRequest,
    ) -> Result<()> {
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let scope = if approval.read_scopes.is_empty() && approval.write_scopes.is_empty() {
            "none".to_string()
        } else {
            format!(
                "read=[{}];write=[{}]",
                approval.read_scopes.join(","),
                approval.write_scopes.join(",")
            )
        };
        let record = AuditRecord::approval_prompt(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: approval.requesting_agent_id.clone(),
            },
            approval.id.clone(),
            approval.requesting_agent_id.clone(),
            approval.action_kind.clone(),
            scope,
            "prompted",
        );
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Runs the session memory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn session_memory(&self) -> &SessionMemoryStore {
        &self.session_memory
    }

    /// Runs the session memory mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn session_memory_mut(&mut self) -> &mut SessionMemoryStore {
        &mut self.session_memory
    }

    /// Runs the memory records operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn memory_records(&self) -> Vec<MemoryRecord> {
        self.session_memory.export()
    }

    /// Runs the upsert session memory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn upsert_session_memory(&mut self, record: MemoryRecord) -> Result<()> {
        self.require_live()?;
        self.session_memory.upsert(record)?;
        Ok(())
    }

    /// Runs the delete session memory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn delete_session_memory(&mut self, id: &str) -> Result<bool> {
        self.require_live()?;
        Ok(self.session_memory.delete(id))
    }
}
