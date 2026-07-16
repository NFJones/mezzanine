//! Provider/auth/audit stores and transcript checkpoint and restoration behavior.

use super::mcp_helpers::runtime_agent_session_metadata_visibility;
use super::*;

impl RuntimeSessionService {
    /// Runs the provider registry operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn provider_registry(&self) -> &RuntimeProviderRegistry {
        self.integration.provider_registry()
    }

    /// Runs the auth store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn auth_store(&self) -> Option<&AuthStore> {
        self.auth_store.as_ref()
    }

    /// Returns the configured provider auth refresh leeway in seconds.
    pub fn provider_auth_refresh_leeway_seconds(&self) -> u64 {
        self.provider_auth_refresh_leeway_seconds
    }

    /// Runs the set auth store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_auth_store(&mut self, store: AuthStore) {
        self.auth_store = Some(store);
    }

    /// Runs the set audit log operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_audit_log(&mut self, mut audit_log: AuditLog) {
        if let Some(existing) = self.persistence.audit_log_mut() {
            let pending = existing.drain_deferred_writes();
            for write in pending {
                self.persistence
                    .queue_audit(audit_persistence_effect(write));
            }
        }
        audit_log.set_defer_writes(self.persistence.audit_uses_adapter());
        self.persistence.set_audit_log(audit_log);
    }

    /// Clears the active audit writer while preserving deferred writes.
    pub(super) fn clear_audit_log(&mut self) {
        if let Some(existing) = self.persistence.audit_log_mut() {
            let pending = existing.drain_deferred_writes();
            for write in pending {
                self.persistence
                    .queue_audit(audit_persistence_effect(write));
            }
        }
        self.persistence.clear_audit_log();
    }

    /// Runs the audit log operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn audit_log(&self) -> Option<&AuditLog> {
        self.persistence.audit_log()
    }

    /// Drains queued audit persistence through the transport-neutral transition contract.
    pub(crate) fn drain_audit_persistence_transition(&mut self) -> RuntimeTransition {
        if let Some(audit_log) = self.persistence.audit_log_mut() {
            let pending = audit_log.drain_deferred_writes();
            for write in pending {
                self.persistence
                    .queue_audit(audit_persistence_effect(write));
            }
        }
        RuntimeTransition {
            applied: false,
            side_effects: self.persistence.take_audit_effects(),
        }
    }

    /// Runs the agent transcript store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn agent_transcript_store(&self) -> Option<&AgentTranscriptStore> {
        self.persistence.transcript_store()
    }

    /// Runs the set agent transcript store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_agent_transcript_store(&mut self, store: AgentTranscriptStore) {
        self.persistence.set_transcript_store(store);
    }

    /// Restores pane-scoped active agent shell metadata for this live session.
    ///
    /// Transcript files hold the durable conversation content, but the runtime
    /// also needs a small pane binding record to recover which pane owned which
    /// conversation after a daemon restart. Records are scoped by Mezzanine
    /// session id so a fresh daemon or different pane cannot inherit another
    /// session's context automatically.
    pub fn restore_agent_sessions_from_transcript_store(&mut self) -> Result<usize> {
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(0);
        };
        let session_id = self.session.id.as_str().to_string();
        let records = store.load_agent_session_metadata(&session_id)?;
        let restored_at = current_unix_seconds();
        let mut restored = 0usize;
        let mut interrupted = 0usize;
        for metadata in records {
            if runtime_pane_by_id(&self.session, &metadata.pane_id).is_err() {
                continue;
            }
            let visibility = runtime_agent_session_metadata_visibility(&metadata.visibility)?;
            let log_level = AgentLogLevel::parse(&metadata.log_level).ok_or_else(|| {
                MezError::invalid_args("agent session metadata log level is invalid")
            })?;
            let running_turn_id = metadata.running_turn_id.clone();
            let session = self
                .agent_shell_store_mut()
                .ensure_session(metadata.pane_id.clone())?;
            session.session_id = metadata.conversation_id.clone();
            session.prompt_cache_lineage_id = metadata.prompt_cache_lineage_id.clone();
            session.visibility = visibility;
            session.running_turn_id = None;
            session.transcript_entries = metadata.transcript_entries;
            session.ephemeral = false;
            session.ephemeral_transcript_source_conversation_id = None;
            session.ephemeral_transcript_source_entries = 0;
            session.log_level = log_level;
            session.directive = metadata.directive.clone();
            if let Some(profile) = metadata.pane_model_profile.as_ref() {
                self.integration
                    .model_profile_overrides_mut()
                    .pane_profiles
                    .insert(metadata.pane_id.clone(), profile.clone());
            } else {
                self.integration
                    .model_profile_overrides_mut()
                    .pane_profiles
                    .remove(&metadata.pane_id);
            }
            self.set_agent_planning_enabled(&metadata.pane_id, metadata.planning_enabled);
            self.set_agent_response_style(&metadata.pane_id, metadata.response_style.clone());
            self.set_agent_routing_override(&metadata.pane_id, metadata.routing_enabled);
            self.restore_agent_approval_policy_from_metadata(
                metadata.approval_policy.as_deref(),
                "agent-session-restore",
            )?;
            if let Some(working_directory) = metadata.working_directory.as_ref() {
                self.set_pane_current_working_directory(
                    metadata.pane_id.clone(),
                    PathBuf::from(working_directory),
                );
            }
            let token_usage_by_model = runtime_agent_token_usage_by_model_from_metadata(&metadata);
            self.replace_restored_agent_token_usage(
                &metadata.conversation_id,
                &metadata.pane_id,
                token_usage_by_model,
            );
            self.record_pane_transcript_ref(
                &metadata.pane_id,
                format!(
                    "transcript:{}:{}",
                    metadata.pane_id, metadata.conversation_id
                ),
            )?;
            self.reload_agent_prompt_history_for_pane(&metadata.pane_id)?;
            if let Some(turn_id) = running_turn_id {
                self.agent_turn_ledger_mut().start_turn(AgentTurnRecord {
                    turn_id: turn_id.clone(),
                    agent_id: format!("agent-{}", metadata.pane_id),
                    pane_id: metadata.pane_id.clone(),
                    trigger: AgentTurnTrigger::ScheduledTask,
                    started_at_unix_seconds: restored_at,
                    policy_profile: "agent-session-restore".to_string(),
                    model_profile: "default".to_string(),
                    parent_turn_id: None,
                    state: AgentTurnState::Queued,
                    cooperation_mode: None,
                    initial_capability: None,
                })?;
                self.agent_turn_ledger_mut()
                    .finish_turn(&turn_id, AgentTurnState::Interrupted)?;
                interrupted = interrupted.saturating_add(1);
            }
            restored = restored.saturating_add(1);
        }
        if restored > 0 || interrupted > 0 {
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"source":"agent-session-restore","restored_agent_sessions":{},"interrupted_agent_turns":{},"retry_requires_confirmation":{}}}"#,
                    restored,
                    interrupted,
                    interrupted > 0
                ),
            )?;
        }
        if restored > 0 {
            self.checkpoint_agent_session_metadata()?;
        }
        Ok(restored)
    }

    /// Persists the active pane-to-agent-session bindings for crash recovery.
    ///
    /// The checkpoint is intentionally metadata-only. Conversation content
    /// remains in per-conversation transcript files, while this file records
    /// which pane should point at which conversation when the same Mezzanine
    /// session is restored.
    pub(in crate::runtime) fn checkpoint_agent_session_metadata(&mut self) -> Result<usize> {
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(0);
        };
        let mezzanine_session_id = self.session.id.as_str().to_string();
        let records = self
            .agent_shell_store()
            .sessions()
            .filter(|session| runtime_pane_by_id(&self.session, &session.pane_id).is_ok())
            .filter(|session| {
                !session.ephemeral
                    || session
                        .ephemeral_transcript_source_conversation_id
                        .is_some()
                    || self.agent_loop_is_active(&session.pane_id)
            })
            .map(|session| {
                let fallback_parent = self.agent_loop_state(&session.pane_id);
                let conversation_id = session
                    .ephemeral_transcript_source_conversation_id
                    .clone()
                    .or_else(|| fallback_parent.map(|state| state.parent_conversation_id.clone()))
                    .unwrap_or_else(|| session.session_id.clone());
                let transcript_entries = if session.ephemeral {
                    session
                        .ephemeral_transcript_source_conversation_id
                        .as_ref()
                        .map(|_| session.ephemeral_transcript_source_entries)
                        .or_else(|| fallback_parent.map(|state| state.parent_transcript_entries))
                        .unwrap_or(session.transcript_entries)
                } else {
                    session.transcript_entries
                };
                let working_directory = self
                    .pane_current_working_directory(&session.pane_id)
                    .map(|path| path.to_string_lossy().into_owned());
                let project_root = working_directory
                    .as_deref()
                    .map(PathBuf::from)
                    .map(|path| discover_project_root(&path).to_string_lossy().into_owned());
                let token_usage_by_model =
                    self.agent_token_usage_for_conversation(&conversation_id);
                AgentSessionMetadata {
                    mezzanine_session_id: mezzanine_session_id.clone(),
                    pane_id: session.pane_id.clone(),
                    conversation_id: conversation_id.clone(),
                    prompt_cache_lineage_id: session.prompt_cache_lineage_id.clone(),
                    visibility: agent_shell_visibility_json_name(session.visibility).to_string(),
                    running_turn_id: session.running_turn_id.clone(),
                    transcript_entries,
                    log_level: session.log_level.as_str().to_string(),
                    pane_model_profile: self
                        .integration
                        .model_profile_overrides()
                        .pane_profiles
                        .get(&session.pane_id)
                        .cloned(),
                    planning_enabled: self.agent_planning_enabled(&session.pane_id),
                    response_style: self
                        .agent_response_style(&session.pane_id)
                        .map(ToOwned::to_owned),
                    directive: session.directive.clone(),
                    routing_enabled: self.agent_routing_override(&session.pane_id),
                    approval_policy: self
                        .integration
                        .live_approval_policy_override()
                        .map(runtime_approval_policy_name)
                        .map(ToOwned::to_owned),
                    working_directory,
                    project_root,
                    token_usage: runtime_agent_total_token_usage_by_model(&token_usage_by_model),
                    token_usage_by_model,
                    context_usage: self.agent_context_usage_display(&conversation_id),
                    context_usage_snapshot: self.agent_context_usage_snapshot(&conversation_id),
                }
            })
            .collect::<Vec<_>>();
        store.save_agent_session_metadata(&mezzanine_session_id, &records)
    }

    /// Restores persisted pane-local agent settings for one rebound conversation.
    ///
    /// `/resume` can bind a saved conversation without going through daemon
    /// startup recovery. This helper reloads the matching metadata row so
    /// explicit session choices such as routing, approval policy, and
    /// provider token accounting continue from saved state instead of falling
    /// back to current defaults.
    pub(in crate::runtime) fn restore_agent_resume_state_for_conversation(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
    ) -> Result<()> {
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(());
        };
        let mezzanine_session_id = self.session.id.as_str().to_string();
        for metadata in store.load_agent_session_metadata(&mezzanine_session_id)? {
            if metadata.conversation_id != conversation_id {
                continue;
            }
            let session = self
                .agent_shell_store_mut()
                .ensure_session(pane_id.to_string())?;
            session.prompt_cache_lineage_id = metadata.prompt_cache_lineage_id.clone();
            session.directive = metadata.directive.clone();
            if let Some(profile) = metadata.pane_model_profile.as_ref() {
                self.integration
                    .model_profile_overrides_mut()
                    .pane_profiles
                    .insert(pane_id.to_string(), profile.clone());
            } else {
                self.integration
                    .model_profile_overrides_mut()
                    .pane_profiles
                    .remove(pane_id);
            }
            self.set_agent_planning_enabled(pane_id, metadata.planning_enabled);
            self.set_agent_response_style(pane_id, metadata.response_style.clone());
            self.set_agent_routing_override(pane_id, metadata.routing_enabled);
            self.restore_agent_approval_policy_from_metadata(
                metadata.approval_policy.as_deref(),
                "agent-session-resume",
            )?;
            let token_usage_by_model = runtime_agent_token_usage_by_model_from_metadata(&metadata);
            self.merge_restored_agent_token_usage(conversation_id, pane_id, token_usage_by_model);
            self.restore_agent_context_usage(
                conversation_id,
                metadata.context_usage,
                metadata.context_usage_snapshot,
            );
            let _ = self.checkpoint_agent_session_metadata();
            break;
        }
        Ok(())
    }

    /// Applies a saved approval-policy value directly from session metadata.
    ///
    /// New checkpoints only persist this field for explicit live approval
    /// choices. Older checkpoints stored the effective policy, so restore must
    /// avoid letting legacy inherited values narrow a stronger configured
    /// default.
    pub(super) fn restore_agent_approval_policy_from_metadata(
        &mut self,
        approval_policy: Option<&str>,
        source: &str,
    ) -> Result<()> {
        let Some(approval_policy) =
            approval_policy.filter(|approval_policy| !approval_policy.trim().is_empty())
        else {
            return Ok(());
        };
        let requested = runtime_parse_approval_policy(approval_policy).map_err(|_| {
            MezError::invalid_args("agent session metadata approval policy is invalid")
        })?;
        if matches!(
            compare_approval_policy_authority(self.permission_policy().approval_policy, requested),
            PermissionAuthorityChange::Narrowing
        ) {
            return Ok(());
        }
        let previous_permission_policy = self.permission_policy().clone();
        self.set_live_approval_policy_override(requested);
        self.reconcile_pending_agent_approvals_after_permission_change(
            None,
            &previous_permission_policy,
            source,
        )?;
        Ok(())
    }

    /// Drains transcript and prompt-history persistence through one runtime transition.
    pub(crate) fn drain_transcript_persistence_transition(&mut self) -> RuntimeTransition {
        RuntimeTransition {
            applied: false,
            side_effects: self.persistence.take_transcript_effects(),
        }
    }

    /// Drains configuration persistence through one transport-neutral runtime transition.
    pub(crate) fn drain_config_persistence_transition(&mut self) -> RuntimeTransition {
        RuntimeTransition {
            applied: false,
            side_effects: coalesce_config_persistence_effects(
                self.persistence.take_config_effects(),
            ),
        }
    }

    /// Drains all queued external work through one transport-neutral transition.
    pub(crate) fn drain_deferred_effects_transition(&mut self) -> RuntimeTransition {
        let mut side_effects = self.drain_pane_io_transition().side_effects;
        side_effects.extend(self.drain_audit_persistence_transition().side_effects);
        side_effects.extend(self.drain_transcript_persistence_transition().side_effects);
        side_effects.extend(self.drain_config_persistence_transition().side_effects);
        side_effects.extend(self.drain_pane_pipe_persistence_transition().side_effects);
        side_effects.extend(self.drain_program_hook_transition().side_effects);
        RuntimeTransition {
            applied: false,
            side_effects,
        }
    }
}
