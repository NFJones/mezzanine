//! Control, diagnostics, process, terminal, permission, approval, and memory stores.

#[cfg(test)]
use super::ControlIdempotencyCache;
use super::{
    AuditActor, AuditRecord, BlockedApprovalQueue, BlockedApprovalRequest, EventKind,
    MEZ_ENV_FIELD_SEPARATOR, MemoryRecord, MessageService, MezError, PermissionPolicy, Result,
    RuntimeSessionService, SessionApprovalStore, SessionMemoryStore, current_unix_seconds,
    json_escape,
};
use mez_agent::permissions::{
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, exact_command_sha256, normalize_exact_command_text,
};

impl RuntimeSessionService {
    /// Plans one saved-session archive lifecycle operation for the persistence worker.
    #[allow(
        dead_code,
        reason = "archive operation planning is consumed by the dependent resume browser work"
    )]
    pub(crate) fn queue_session_archive_operation(
        &mut self,
        conversation_id: &str,
        operation: crate::runtime::SessionArchiveOperation,
    ) -> Result<crate::runtime::RuntimeTransition> {
        mez_agent::transcript::validate_conversation_id(conversation_id)?;
        if matches!(
            operation,
            crate::runtime::SessionArchiveOperation::Archive { .. }
        ) && self
            .agent_shell_store()
            .sessions()
            .any(|session| !session.ephemeral && session.session_id == conversation_id)
        {
            return Err(MezError::conflict(
                "cannot archive a conversation bound to a live durable pane",
            ));
        }
        let store = self.persistence.cloned_transcript_store().ok_or_else(|| {
            MezError::invalid_state("session archival requires transcript storage")
        })?;
        let queued = self.persistence.queue_session_archive(
            crate::runtime::RuntimeSideEffect::PersistSessionArchive {
                store,
                conversation_id: conversation_id.to_string(),
                operation,
            },
        )?;
        Ok(crate::runtime::RuntimeTransition {
            applied: queued,
            side_effects: Vec::new(),
        })
    }

    /// Runs the control idempotency operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn control_idempotency(&self) -> &ControlIdempotencyCache {
        self.control.idempotency()
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
            } => {
                if target == crate::runtime::PersistenceTarget::TokenUsage {
                    self.persistence.clear_token_usage_health_error();
                }
                serde_json::json!({
                    "worker": "async-persistence",
                    "target": target.as_str(),
                    "path": path.to_string_lossy(),
                    "state": "completed",
                    "bytes": bytes,
                })
                .to_string()
            }
            crate::runtime::PersistenceEvent::Failed {
                target,
                path,
                error,
            } => {
                if target == crate::runtime::PersistenceTarget::PanePipe {
                    let _ =
                        self.stop_file_pane_pipes_for_path(path.as_path(), "persistence-failed")?;
                }
                if target == crate::runtime::PersistenceTarget::TokenUsage {
                    self.persistence.set_token_usage_health_error(
                        "persistent token accounting is degraded after a storage write failure",
                    );
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
            crate::runtime::PersistenceEvent::SessionArchiveCompleted {
                conversation_id,
                operation,
                bytes,
            } => {
                self.persistence.finish_session_archive(&conversation_id);
                let resume = self
                    .persistence
                    .take_session_archive_resume(&conversation_id);
                self.refresh_saved_session_overlay_after_archive(
                    &conversation_id,
                    Some(operation),
                    None,
                )?;
                if matches!(operation, crate::runtime::SessionArchiveOperation::Restore)
                    && let Some((client_id, pane_id)) = resume
                {
                    let resume_error = if !self.session.is_attached_primary(&client_id) {
                        Some("restore completed, but the requesting primary client detached before resume".to_string())
                    } else {
                        self.execute_agent_shell_resume_command(
                            &pane_id,
                            &format!("/resume {conversation_id}"),
                        )
                        .err()
                        .map(|error| error.message().to_string())
                    };
                    if let Some(error) = resume_error {
                        self.refresh_saved_session_overlay_after_archive(
                            &conversation_id,
                            Some(operation),
                            Some(&error),
                        )?;
                    } else {
                        self.dismiss_primary_display_overlay();
                    }
                }
                self.invalidate_agent_prompt_selector_extra_candidates();
                serde_json::json!({
                    "worker": "async-persistence",
                    "target": "session_archive",
                    "conversation_id": conversation_id,
                    "operation": operation.as_str(),
                    "state": "completed",
                    "bytes": bytes,
                })
                .to_string()
            }
            crate::runtime::PersistenceEvent::SessionArchiveFailed {
                conversation_id,
                operation,
                error,
            } => {
                self.persistence.finish_session_archive(&conversation_id);
                self.persistence
                    .take_session_archive_resume(&conversation_id);
                self.refresh_saved_session_overlay_after_archive(
                    &conversation_id,
                    Some(operation),
                    Some(error.as_str()),
                )?;
                serde_json::json!({
                    "worker": "async-persistence",
                    "target": "session_archive",
                    "conversation_id": conversation_id,
                    "operation": operation.as_str(),
                    "state": "failed",
                    "error": error,
                })
                .to_string()
            }
        };
        self.append_runtime_diagnostic_event(payload)?;
        Ok(self.runtime_transition_with_render(
            true,
            Some(crate::runtime::RenderInvalidationReason::Overlay),
        ))
    }

    /// Refreshes an open saved-session browser after one archive lifecycle settlement.
    fn refresh_saved_session_overlay_after_archive(
        &mut self,
        conversation_id: &str,
        operation: Option<crate::runtime::SessionArchiveOperation>,
        error: Option<&str>,
    ) -> Result<()> {
        let source = self.active_saved_session_browser_source();
        let Some(source @ crate::runtime::service_state::RuntimeRecordBrowserOverlaySource::SavedSessions { .. }) = source else {
            return Ok(());
        };
        let (source, mut browser) = self.refresh_saved_session_browser_preserving(
            &source,
            error.is_some().then_some(conversation_id),
        )?;
        browser.set_error(error.map(str::to_string).or_else(|| {
            operation
                .map(|operation| format!("{} completed for {conversation_id}", operation.as_str()))
        }));
        self.replace_active_saved_session_browser(source, browser);
        Ok(())
    }

    /// Runs the message service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn message_service(&self) -> &MessageService {
        self.control.message_service()
    }

    /// Runs the message service mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn message_service_mut(&mut self) -> &mut MessageService {
        self.control.message_service_mut()
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
        self.integration.permission_policy()
    }

    /// Returns the complete configured authorization and confinement state.
    pub(crate) fn configured_permissions(&self) -> &crate::runtime::config::ConfiguredPermissions {
        self.integration.configured_permissions()
    }

    /// Runs the permission policy mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn permission_policy_mut(&mut self) -> &mut PermissionPolicy {
        self.integration.permission_policy_mut()
    }

    /// Sets or clears the preset override explicitly owned by one pane.
    pub(crate) fn set_pane_permission_preset_override(
        &mut self,
        pane_id: &str,
        value: Option<mez_agent::PermissionPreset>,
    ) {
        self.integration
            .set_pane_permission_preset_override(pane_id, value);
    }

    /// Sets or clears the approval-policy override explicitly owned by one pane.
    pub(crate) fn set_pane_approval_policy_override(
        &mut self,
        pane_id: &str,
        value: Option<mez_agent::ApprovalPolicy>,
    ) {
        self.integration
            .set_pane_approval_policy_override(pane_id, value);
    }

    /// Applies an explicit user-selected approval-bypass state.
    ///
    /// # Parameters
    /// - `active`: Whether approval bypass should be active after the change.
    pub fn set_live_approval_bypass_override(&mut self, active: bool) {
        self.integration
            .set_live_approval_bypass_override(Some(active));
        self.integration
            .permission_policy_mut()
            .set_approval_bypass(active);
    }

    /// Runs the blocked approvals operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn blocked_approvals(&self) -> &BlockedApprovalQueue {
        self.integration.blocked_approvals()
    }

    /// Runs the session approvals operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn session_approvals(&self) -> &SessionApprovalStore {
        self.integration.session_approvals()
    }

    /// Runs the session approvals mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub fn session_approvals_mut(&mut self) -> &mut SessionApprovalStore {
        self.integration.session_approvals_mut()
    }

    /// Runs the queue blocked approval operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn queue_blocked_approval(&mut self, request: BlockedApprovalRequest) -> Result<String> {
        let binding =
            self.pane_current_working_directory(&request.pane_id)
                .map(|working_directory| {
                    (
                        crate::security::project::discover_project_root(&working_directory),
                        working_directory,
                        exact_command_sha256(
                            DEFAULT_COMMAND_SHELL_CLASSIFICATION,
                            &normalize_exact_command_text(&request.action_summary, false),
                        ),
                    )
                });
        let approval_id = self
            .integration
            .blocked_approvals_mut()
            .create_at(request, current_unix_seconds())?;
        if let Some((project_root, working_directory, command_sha256)) = binding {
            self.control.insert_approval_binding(
                approval_id.clone(),
                project_root,
                working_directory,
                command_sha256,
            );
        }
        let approval = self
            .integration
            .blocked_approvals()
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
    #[cfg(test)]
    pub fn session_memory(&self) -> &SessionMemoryStore {
        self.integration.session_memory()
    }

    /// Runs the session memory mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn session_memory_mut(&mut self) -> &mut SessionMemoryStore {
        self.integration.session_memory_mut()
    }

    /// Runs the memory records operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn memory_records(&self) -> Vec<MemoryRecord> {
        self.integration.session_memory().export()
    }

    /// Runs the upsert session memory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn upsert_session_memory(&mut self, record: MemoryRecord) -> Result<()> {
        self.require_live()?;
        self.integration.session_memory_mut().upsert(record)?;
        Ok(())
    }

    /// Runs the delete session memory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub fn delete_session_memory(&mut self, id: &str) -> Result<bool> {
        self.require_live()?;
        Ok(self.integration.session_memory_mut().delete(id))
    }
}
