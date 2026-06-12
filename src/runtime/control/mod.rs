//! Runtime Control implementation.
//!
//! This module owns the runtime control boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.
mod approval;
mod configuration;
mod context;
mod ingress;
mod lifecycle;
mod live_snapshot;
mod message;
mod mutations;
mod protocol;
mod snapshot;
mod state;
mod subagents;
use super::{
    AgentContext, AgentId, AgentScheduler, AgentShellStore, AgentTurnLedger, AgentTurnState,
    ApprovalDecision, ApprovalDecisionScopePersistence, AttachedTerminalClientStepPlan, AuditActor,
    AuditRecord, BlockedApprovalRequest, ClientRole, ClientState, ClientViewRole, CommandRule,
    CommandRuleScope, ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation,
    ConfigScope, ContextBlock, ContextSourceKind, ControlConnectionState,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, DeferredConfigFileWrite, DeferredProjectConfigWrite,
    Envelope, EventKind, EventVisibility, HookEvent, MemoryRecord, MezError, PaneCaptureSource,
    PaneId, PaneProcessStart, PaneReadinessOverrideStore, PaneReadinessState, Path, PathBuf,
    ProjectTrustStore, Recipient, Result, RuleDecision, RuleMatch, RuntimeAutoSizingConfig,
    RuntimeLifecycleState, RuntimeRegistryUpdatePlan, RuntimeSessionService,
    RuntimeSubagentLineage, RuntimeSubagentPlacement, SUBAGENT_FRIENDLY_NAMES, ScopeRegistry,
    SenderIdentity, SessionRecord, SnapshotCreationContext, SnapshotRepository, SplitDirection,
    SubagentScopeDeclaration, SubagentSpawnRequest, TaskState, TaskStatusPayload,
    TerminalClientLoopAction, TerminalClientLoopConfig, TrustDecision, agent_state_control_method,
    append_memory_context, append_permission_policy_context, append_scheduler_context,
    approval_decide_scope_persistence, compare_permission_preset_authority, current_unix_seconds,
    default_trust_database_path, destination_target_checked_resolved, discover_project_root,
    dispatch_control_request_cached, dispatch_control_request_for_client_with_agent_state,
    dispatch_control_request_for_client_with_agent_state_and_model_profiles,
    dispatch_control_request_for_client_with_config,
    dispatch_control_request_for_client_with_config_and_audit,
    dispatch_control_request_for_client_with_snapshot_context,
    dispatch_control_request_for_connection, dispatch_control_request_with_approvals,
    dispatch_control_request_with_approvals_and_audit, dispatch_control_request_with_captures,
    dispatch_control_request_with_mcp, dispatch_snapshot_request_with_context_async, fs,
    json_escape, layout_state_json, normalize_exact_command_text, observer_json,
    pane_target_checked_resolved, parse_json_rpc_request, plan_config_mutation,
    project_trust_state_filter_from_params, rendered_client_view_json, route_client_input_actions,
    runtime_agent_turn_state_json, runtime_append_observer_decision_audit,
    runtime_approval_decision_name_to_kind, runtime_approval_policy_name,
    runtime_config_apply_event_payload, runtime_config_method_applies_to_live_service,
    runtime_cooperation_mode_name, runtime_hook_event_for_lifecycle,
    runtime_initialize_requested_observer, runtime_initialize_requested_primary,
    runtime_initialize_terminal_size, runtime_json_bool_field, runtime_json_creation_command,
    runtime_json_input_bytes, runtime_json_optional_client_size, runtime_json_optional_size_field,
    runtime_json_optional_view_offset, runtime_json_rpc_error, runtime_json_size,
    runtime_json_start_directory, runtime_json_string_field, runtime_mcp_retry_event_payload,
    runtime_mutating_method, runtime_pane_by_id, runtime_pane_readiness_state_name,
    runtime_path_under_project_root, runtime_permission_decision_hook_payload,
    runtime_permission_preset_name, runtime_project_root_param, runtime_project_trust_record_json,
    runtime_split_direction, runtime_string_array_json, runtime_subagent_placement_mode,
    runtime_subagent_spawn_request, runtime_subagent_state_json, runtime_terminal_step_result_json,
    runtime_trust_decision_name, runtime_trust_decision_param, set_project_guidance_context,
    snapshot_id_for_idempotency_key, source_pane_target_checked_resolved, validate_config_text,
    window_target_checked_resolved,
};
use crate::config::compose_effective_config;
use crate::control::{
    ControlPersistTarget, authorize_control_request, config_audit_outcome, config_audit_plan,
    config_mutation_plan_result_json, config_mutation_value_from_json, config_request_cache_key,
    config_response_advances_generation, persist_target_from_json,
    validate_control_method_params_schema,
};
use crate::skills::{
    BUILTIN_MEZ_REFERENCE_SKILL_NAME, SkillDocument, is_valid_skill_name, load_skill_document,
    parse_skill_prompt_invocation, skill_context_text,
};
use context::{
    AGENT_TRANSCRIPT_CONTEXT_READ_BYTES, runtime_agent_transcript_context_blocks,
    runtime_context_block_is_compaction_refresh_owned, runtime_local_message_context_content,
    runtime_transcript_context_entry_limit,
};
use protocol::{
    pane_id_from_runtime_agent_id, paths_equivalent, runtime_project_trust_read_method,
    runtime_snapshot_resume_plan_json,
};

// Runtime control, message, event, and mutation dispatch.

/// Defines the RUNTIME CONTROL LIVE OVERRIDE LAYER const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_CONTROL_LIVE_OVERRIDE_LAYER: &str = "runtime-control-live-override";
impl RuntimeSessionService {
    /// Runs the agent context for pane prompt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn agent_context_for_pane_prompt(
        &mut self,
        pane_id: &str,
        prompt: &str,
        _max_history_lines: usize,
    ) -> Result<AgentContext> {
        if prompt.trim().is_empty() {
            return Err(MezError::invalid_args("agent prompt must not be empty"));
        }
        self.refresh_project_config_layers_for_pane(pane_id)?;
        let mut blocks = vec![];

        blocks.push(ContextBlock {
            source: ContextSourceKind::Configuration,
            label: "session identity".to_string(),
            content: format!(
                "session_id={} session_name={}",
                self.session.id, self.session.name
            ),
        });
        if let Some(lineage_id) = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.prompt_cache_lineage_id.clone())
            .filter(|lineage_id| !lineage_id.trim().is_empty())
        {
            blocks.push(ContextBlock {
                source: ContextSourceKind::Configuration,
                label: "prompt cache lineage".to_string(),
                content: lineage_id,
            });
        }

        let readiness_state = self.pane_readiness_state(pane_id);
        let window_name = runtime_pane_by_id(&self.session, pane_id)
            .map(|(window, _pane)| window.name.clone())?;
        blocks.push(ContextBlock {
            source: ContextSourceKind::Configuration,
            label: "pane identity".to_string(),
            content: format!(
                "pane_id={pane_id} window_name={window_name} readiness_state={}",
                runtime_pane_readiness_state_name(readiness_state)
            ),
        });
        if let Some(readiness_hint) =
            runtime_agent_pane_readiness_context_block(pane_id, readiness_state)
        {
            blocks.push(readiness_hint);
        }

        if let Some(session) = self.agent_shell_store.get(pane_id)
            && let Some(store) = self.agent_transcript_store.as_ref()
        {
            let transcript_conversation_id = session
                .ephemeral_transcript_source_conversation_id
                .as_deref()
                .unwrap_or(session.session_id.as_str());
            let transcript_entries = if session.ephemeral {
                session.ephemeral_transcript_source_entries
            } else {
                session.transcript_entries
            };
            if transcript_entries > 0 {
                let transcript_context_entries =
                    runtime_transcript_context_entry_limit(transcript_entries);
                match store.inspect_recent(
                    transcript_conversation_id,
                    transcript_context_entries,
                    AGENT_TRANSCRIPT_CONTEXT_READ_BYTES,
                ) {
                    Ok(entries) if !entries.is_empty() => {
                        blocks.extend(runtime_agent_transcript_context_blocks(pane_id, &entries));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
        let agent_id = crate::ids::AgentId::opaque(format!("agent-{pane_id}"))
            .ok_or_else(|| MezError::invalid_args("agent id must be non-empty"))?;
        let pending_messages = self
            .message_service
            .receive_for(&agent_id, super::current_unix_seconds());
        if !pending_messages.is_empty() {
            let message_lines: Vec<String> = pending_messages
                .iter()
                .map(runtime_local_message_context_content)
                .collect();
            blocks.push(ContextBlock {
                source: ContextSourceKind::LocalMessage,
                label: format!("pending local messages for agent {agent_id}"),
                content: message_lines.join("\n\n"),
            });
        }
        let active_subagent_scopes = self.subagent_scopes.active_write_scopes();
        if !active_subagent_scopes.is_empty() {
            let insert_at = blocks
                .iter()
                .position(|block| block.source == ContextSourceKind::UserInstruction)
                .unwrap_or(blocks.len());
            blocks.insert(
                insert_at,
                ContextBlock {
                    source: ContextSourceKind::Policy,
                    label: "active subagent write scopes".to_string(),
                    content: active_subagent_scopes
                        .iter()
                        .map(|scope| {
                            format!(
                                "agent={} mode={} scope={} serial_lock={}",
                                scope.agent_id,
                                runtime_cooperation_mode_name(scope.mode),
                                scope.scope,
                                scope.serial_lock.as_deref().unwrap_or("none")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                },
            );
        }
        if let Some(signature) = self.pane_environment_signatures.get(pane_id) {
            let mut env_lines = signature.model_context_fields();
            if let Some(inventory) = self.tool_discovery_cache.get(signature) {
                env_lines.push(format!(
                    "available_tools={} sed={} grep={} python={} rg={}",
                    inventory.tools.len(),
                    inventory.sed,
                    inventory.grep,
                    inventory.python,
                    inventory.rg
                ));
                if !inventory.modern_tools.is_empty() {
                    env_lines.push(format!("tools={}", inventory.modern_tools.join(",")));
                }
            }
            let insert_at = blocks
                .iter()
                .position(|block| block.source == ContextSourceKind::Configuration)
                .unwrap_or(blocks.len());
            blocks.insert(
                insert_at,
                ContextBlock {
                    source: ContextSourceKind::Configuration,
                    label: format!("environment signature for pane {pane_id}"),
                    content: env_lines.join("\n"),
                },
            );
        }
        if let Some(instruction_files) = self.pane_instruction_files.get(pane_id)
            && !instruction_files.is_empty()
        {
            let context = AgentContext::new(blocks)?;
            let context = set_project_guidance_context(context, instruction_files, 2)?;
            blocks = context.blocks;
            if instruction_files.iter().any(|f| f.truncated) {
                let truncated_paths: Vec<&str> = instruction_files
                    .iter()
                    .filter(|f| f.truncated)
                    .map(|f| f.path.as_str())
                    .collect();
                let _ = self.append_lifecycle_event(
                    EventKind::Diagnostic,
                    format!(
                        r#"{{"pane_id":"{}","kind":"instruction_truncated","paths":{},"message":"project instruction content was truncated to the configured byte limit"}}"#,
                        json_escape(pane_id),
                        serde_json::to_string(&truncated_paths).unwrap_or_else(|_| "[]".to_string()),
                    ),
                );
            }
        }
        if let Some(invocation) = parse_skill_prompt_invocation(prompt) {
            if !is_valid_skill_name(&invocation.name) {
                return Err(MezError::invalid_args(
                    "skill name must contain only lowercase letters, digits, and hyphens",
                ));
            }
            let catalog = self.effective_skill_catalog_for_pane(pane_id);
            let Some(summary) = catalog.get(&invocation.name) else {
                let available = if catalog.skills.is_empty() {
                    "none".to_string()
                } else {
                    catalog.names().join(",")
                };
                return Err(MezError::invalid_args(format!(
                    "skill {:?} is not available; available skills: {available}",
                    invocation.name
                )));
            };
            let document = load_skill_document(summary)?;
            blocks.push(ContextBlock {
                source: ContextSourceKind::SkillInstruction,
                label: format!("explicit skill {}", invocation.name),
                content: self.runtime_skill_context_text(
                    document,
                    invocation.additional_context.as_deref(),
                )?,
            });
            blocks.push(ContextBlock {
                source: ContextSourceKind::RuntimeHint,
                label: format!("explicit skill invocation {}", invocation.name),
                content: format!(
                    "[explicit skill invocation resolved]\n\
                     skill={}\n\
                     The selected skill context has already been loaded above. Treat the text after the `$<skill-name>` token as the user's task-specific instruction. Do not call request_skills or call_skill to load this skill again; use the loaded skill guidance and request the missing action capability needed for the next concrete step.",
                    invocation.name
                ),
            });
        }
        let context_memory_records = self.model_context_memory_records_for_pane(pane_id);
        if let Some(block) =
            Self::runtime_agent_compaction_notice_context_block(&context_memory_records)
        {
            blocks.push(block);
        }
        blocks.push(ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user prompt".to_string(),
            content: prompt.to_string(),
        });
        let context = AgentContext::new(blocks)?;
        let context = append_permission_policy_context(context, &self.permission_policy)?;
        let context = append_scheduler_context(context, &self.agent_scheduler)?;
        append_memory_context(context, &context_memory_records, 1)
    }

    /// Formats loaded skill context with runtime-only additions where needed.
    pub(super) fn runtime_skill_context_text(
        &self,
        mut document: SkillDocument,
        additional_context: Option<&str>,
    ) -> Result<String> {
        if document.summary.name == BUILTIN_MEZ_REFERENCE_SKILL_NAME {
            document.text = format!(
                "{}\n\n## Current effective Mezzanine config\n\n```text\n{}\n```",
                document.text.trim_end(),
                self.runtime_mez_config_skill_current_config()?
            );
        }
        Ok(skill_context_text(&document, additional_context))
    }

    /// Builds the current-config snapshot appended to `$mez-reference`.
    fn runtime_mez_config_skill_current_config(&self) -> Result<String> {
        let effective = compose_effective_config(&self.config_layers)?;
        let mut lines = vec![format!(
            "layers={} applied_layers={} skipped_layers={} values={} diagnostics={}",
            self.config_layers.len(),
            effective.applied_layers().len(),
            effective.skipped_layers().len(),
            effective.values().len(),
            effective.diagnostics().len()
        )];
        for diagnostic in effective.diagnostics() {
            lines.push(format!(
                "diagnostic path={} message={}",
                json_escape(&diagnostic.path),
                json_escape(&diagnostic.message)
            ));
        }
        for (path, value) in effective.values() {
            lines.push(format!(
                "value path={} source={} value={}",
                json_escape(path),
                json_escape(&value.source_layer),
                json_escape(&value.value)
            ));
        }
        Ok(lines.join("\n"))
    }

    /// Returns memory records that should automatically enter model context.
    ///
    /// Default provider context already contains live transcript, project, and
    /// configuration state. To keep memory from becoming a repetitive token
    /// sink, only the active conversation's compacted transcript summary is
    /// injected automatically.
    ///
    /// # Parameters
    /// - `pane_id`: The pane whose active agent conversation is being prepared.
    fn model_context_memory_records_for_pane(&self, pane_id: &str) -> Vec<MemoryRecord> {
        let Some(session) = self.agent_shell_store.get(pane_id) else {
            return Vec::new();
        };
        let compact_memory_id = format!("compact-{}", session.session_id);
        self.memory_records()
            .into_iter()
            .filter(|record| record.id == compact_memory_id)
            .collect()
    }

    /// Builds an explicit model-facing notice for compacted conversation memory.
    ///
    /// # Parameters
    /// - `records`: Memory records selected for automatic context injection.
    fn runtime_agent_compaction_notice_context_block(
        records: &[MemoryRecord],
    ) -> Option<ContextBlock> {
        if !records
            .iter()
            .any(|record| record.id.starts_with("compact-"))
        {
            return None;
        }
        Some(ContextBlock {
            source: ContextSourceKind::Memory,
            label: "conversation compaction notice".to_string(),
            content: "Conversation compaction occurred before this turn. Older durable transcript entries were summarized into compact memory, and only the retained recent raw tail remains exact. Treat the summary as lossy; use targeted shell, search, or capture actions if older exact details are needed."
                .to_string(),
        })
    }

    /// Refreshes transcript and compact-memory context for a running turn.
    ///
    /// Automatic provider recovery can compact a pane conversation while the
    /// active turn remains running. The provider retry must then see the newly
    /// written summary and shorter transcript tail without discarding same-turn
    /// action results, steering, rationale ledgers, or execution pressure.
    pub(in crate::runtime) fn refresh_running_turn_context_after_conversation_compaction(
        &mut self,
        turn_id: &str,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            return Ok(false);
        }
        let Some(session) = self.agent_shell_store.get(&turn.pane_id).cloned() else {
            return Ok(false);
        };
        if session.running_turn_id.as_deref() != Some(turn_id) {
            return Ok(false);
        }

        let mut refreshed_blocks = Vec::new();
        if let Some(store) = self.agent_transcript_store.as_ref() {
            let transcript_conversation_id = session
                .ephemeral_transcript_source_conversation_id
                .as_deref()
                .unwrap_or(session.session_id.as_str());
            let transcript_entries = if session.ephemeral {
                session.ephemeral_transcript_source_entries
            } else {
                session.transcript_entries
            };
            if transcript_entries > 0 {
                let transcript_context_entries =
                    runtime_transcript_context_entry_limit(transcript_entries);
                match store.inspect_recent(
                    transcript_conversation_id,
                    transcript_context_entries,
                    AGENT_TRANSCRIPT_CONTEXT_READ_BYTES,
                ) {
                    Ok(entries) if !entries.is_empty() => {
                        refreshed_blocks.extend(runtime_agent_transcript_context_blocks(
                            &turn.pane_id,
                            &entries,
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
        let context_memory_records = self.model_context_memory_records_for_pane(&turn.pane_id);
        if let Some(block) =
            Self::runtime_agent_compaction_notice_context_block(&context_memory_records)
        {
            refreshed_blocks.push(block);
        }
        let refreshed_context = append_memory_context(
            AgentContext::new(refreshed_blocks)?,
            &context_memory_records,
            1,
        )?;
        let refreshed_blocks = refreshed_context.blocks;

        let Some(existing_context) = self.agent_turn_contexts.get(turn_id).cloned() else {
            return Ok(false);
        };
        let mut blocks = existing_context.blocks;
        blocks.retain(|block| !runtime_context_block_is_compaction_refresh_owned(block));
        let insert_at = blocks
            .iter()
            .position(|block| {
                block.source == ContextSourceKind::UserInstruction && block.label == "user prompt"
            })
            .unwrap_or(blocks.len());
        for (offset, block) in refreshed_blocks.into_iter().enumerate() {
            blocks.insert(insert_at + offset, block);
        }
        let refreshed_block_count = blocks.len();
        self.agent_turn_contexts
            .insert(turn_id.to_string(), AgentContext::new(blocks)?);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "context refreshed reason=conversation_compaction_completed blocks={refreshed_block_count}"
            ),
        )?;
        Ok(true)
    }

    /// Runs the registry update plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn registry_update_plan(&self) -> RuntimeRegistryUpdatePlan {
        if self.lifecycle_state == RuntimeLifecycleState::Killed {
            RuntimeRegistryUpdatePlan::Remove {
                session_id: self.session.id.to_string(),
            }
        } else {
            RuntimeRegistryUpdatePlan::Upsert(SessionRecord::from_session(
                &self.session,
                self.socket_path.clone(),
                self.created_at_unix_seconds,
                self.last_attach_at_unix_seconds,
            ))
        }
    }

    /// Runs the dispatch runtime control body operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_control_body(
        &mut self,
        body: &str,
        primary_client_id: &crate::ids::ClientId,
    ) -> String {
        let request = match parse_json_rpc_request(body) {
            Ok(request) => request,
            Err(error) => {
                return runtime_json_rpc_error("null", error.kind(), error.message());
            }
        };
        if let Err(error) = validate_control_method_params_schema(&request) {
            return runtime_json_rpc_error(&request.id, error.kind(), error.message());
        }

        if !runtime_mutating_method(&request.method) {
            if request.method == "event/list" {
                return match self.dispatch_runtime_event_list_request(&request, primary_client_id) {
                    Ok(result) => format!(
                        r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                        request.id
                    ),
                    Err(error) => {
                        runtime_json_rpc_error(&request.id, error.kind(), error.message())
                    }
                };
            }
            match self.dispatch_runtime_read_only_state_request(&request) {
                Ok(Some(result)) => {
                    return format!(
                        r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                        request.id
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            }
            if request.method == "terminal/view" {
                return match self
                    .dispatch_runtime_terminal_view(primary_client_id, request.params.as_deref())
                {
                    Ok(result) => format!(
                        r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                        request.id
                    ),
                    Err(error) => {
                        runtime_json_rpc_error(&request.id, error.kind(), error.message())
                    }
                };
            }
            if request.method.starts_with("approval/") {
                return self.dispatch_runtime_approval_request(body, &request, primary_client_id);
            }
            if request.method == "agent/list" {
                let model_profiles_by_pane = self.runtime_agent_model_profiles_by_pane();
                return dispatch_control_request_for_client_with_agent_state_and_model_profiles(
                    body,
                    &mut self.session,
                    primary_client_id,
                    None,
                    &mut self.agent_shell_store,
                    &self.agent_turn_ledger,
                    Some(&model_profiles_by_pane),
                );
            }
            if matches!(
                request.method.as_str(),
                "agent/shell/show" | "agent/shell/hide"
            ) {
                return self.dispatch_runtime_agent_shell_visibility_request(
                    body,
                    &request,
                    primary_client_id,
                );
            }
            if agent_state_control_method(&request.method) {
                return dispatch_control_request_for_client_with_agent_state(
                    body,
                    &mut self.session,
                    primary_client_id,
                    None,
                    &mut self.agent_shell_store,
                    &self.agent_turn_ledger,
                );
            }
            if request.method.starts_with("config/") {
                return self.dispatch_runtime_config_request(body, &request, primary_client_id);
            }
            if runtime_project_trust_read_method(&request.method) {
                return self.dispatch_runtime_project_trust_request(&request, primary_client_id);
            }
            if request.method == "mcp/list" {
                return dispatch_control_request_with_mcp(
                    body,
                    &mut self.session,
                    primary_client_id,
                    &self.mcp_registry,
                );
            }
            return dispatch_control_request_cached(
                body,
                &mut self.session,
                primary_client_id,
                &mut self.control_idempotency,
            );
        }

        let params = request.params.clone().unwrap_or_else(|| "{}".to_string());
        let idempotency_key = match runtime_json_string_field(&params, "idempotency_key") {
            Some(value) => value,
            None => {
                return runtime_json_rpc_error(
                    &request.id,
                    crate::error::MezErrorKind::InvalidArgs,
                    "mutating control request requires idempotency_key",
                );
            }
        };
        let cache_key = format!("{primary_client_id}:{idempotency_key}");
        let cacheable_response = runtime_mutating_response_is_cacheable(&request.method);
        if cacheable_response {
            match self.control_idempotency.cached_response(
                &cache_key,
                &request.method,
                &request.params,
            ) {
                Ok(Some(response)) => return response,
                Ok(None) => {}
                Err(error) => {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            }
        }

        let result = self.dispatch_runtime_mutating_result(
            request.method.as_str(),
            primary_client_id,
            &params,
        );
        let response = match result {
            Ok(result) => format!(
                r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                request.id
            ),
            Err(error) => runtime_json_rpc_error(&request.id, error.kind(), error.message()),
        };
        if cacheable_response {
            self.control_idempotency.remember_response(
                cache_key,
                request.method,
                request.params,
                response.clone(),
            );
        }
        response
    }

    /// Runs the dispatch runtime control body for connection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_control_body_for_connection(
        &mut self,
        body: &str,
        connection: &mut ControlConnectionState,
    ) -> String {
        self.dispatch_runtime_control_body_for_connection_inner(body, connection, None)
    }

    /// Runs the dispatch runtime control body for connection with snapshots operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_control_body_for_connection_with_snapshots(
        &mut self,
        body: &str,
        connection: &mut ControlConnectionState,
        snapshots: &SnapshotRepository,
    ) -> String {
        self.dispatch_runtime_control_body_for_connection_inner(body, connection, Some(snapshots))
    }

    /// Runs the dispatch runtime control body for connection with snapshots async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn dispatch_runtime_control_body_for_connection_with_snapshots_async(
        &mut self,
        body: &str,
        connection: &mut ControlConnectionState,
        snapshots: &SnapshotRepository,
    ) -> String {
        let request = match parse_json_rpc_request(body) {
            Ok(request) => request,
            Err(error) => {
                return runtime_json_rpc_error("null", error.kind(), error.message());
            }
        };
        if !connection.initialized()
            || request.method == "control/initialize"
            || !request.method.starts_with("snapshot/")
        {
            return self.dispatch_runtime_control_body_for_connection_inner(
                body,
                connection,
                Some(snapshots),
            );
        }
        let Some(caller_client_id) = connection.caller_client_id().cloned() else {
            return runtime_json_rpc_error(
                &request.id,
                crate::error::MezErrorKind::Forbidden,
                "control connection has no authenticated session client",
            );
        };
        if let Err(error) = authorize_control_request(&self.session, &caller_client_id, &request) {
            return runtime_json_rpc_error(&request.id, error.kind(), error.message());
        }
        if let Err(error) = validate_control_method_params_schema(&request) {
            return runtime_json_rpc_error(&request.id, error.kind(), error.message());
        }
        if request.method == "snapshot/resume" {
            let result = self
                .dispatch_runtime_snapshot_resume_for_connection_async(
                    &request,
                    snapshots,
                    connection,
                    &caller_client_id,
                )
                .await;
            let response_succeeded = result.is_ok();
            if let Err(error) = self.append_runtime_snapshot_audit(
                &request,
                &caller_client_id,
                if response_succeeded {
                    "applied"
                } else {
                    "failed"
                },
            ) {
                return runtime_json_rpc_error(&request.id, error.kind(), error.message());
            }
            return match result {
                Ok(result) => format!(
                    r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                    request.id
                ),
                Err(error) => runtime_json_rpc_error(&request.id, error.kind(), error.message()),
            };
        }

        let captures = self.live_snapshot_pane_captures();
        let active_config_layers = self.live_snapshot_config_layers();
        let frame_state = self.live_snapshot_frame_state();
        let agent_sessions = self.live_snapshot_agent_sessions();
        let approval_grants = self.live_snapshot_approval_grants();
        let approval_requests = self.live_snapshot_approval_requests();
        let message_state = self.live_snapshot_message_state();
        let mcp_servers = self.live_snapshot_mcp_servers();
        let context = SnapshotCreationContext::new(
            &captures,
            &active_config_layers,
            &frame_state,
            &agent_sessions,
        )
        .with_approvals(&approval_grants, &approval_requests)
        .with_message_state(&message_state)
        .with_mcp_servers(&mcp_servers);
        let result = dispatch_snapshot_request_with_context_async(
            &request,
            &self.session,
            snapshots,
            context,
        )
        .await;
        let response_succeeded = result.is_ok();
        if let Err(error) = self.append_runtime_snapshot_audit(
            &request,
            &caller_client_id,
            if response_succeeded {
                "applied"
            } else {
                "failed"
            },
        ) {
            return runtime_json_rpc_error(&request.id, error.kind(), error.message());
        }
        if response_succeeded && request.method == "snapshot/create" {
            let _ = self.append_lifecycle_event(
                EventKind::SnapshotChanged,
                format!(r#"{{"method":"{}","live_capture":true}}"#, request.method),
            );
        }
        match result {
            Ok(result) => format!(
                r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                request.id
            ),
            Err(error) => runtime_json_rpc_error(&request.id, error.kind(), error.message()),
        }
    }

    /// Runs the dispatch runtime control body for connection inner operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_control_body_for_connection_inner(
        &mut self,
        body: &str,
        connection: &mut ControlConnectionState,
        snapshots: Option<&SnapshotRepository>,
    ) -> String {
        let request = match parse_json_rpc_request(body) {
            Ok(request) => request,
            Err(error) => {
                return runtime_json_rpc_error("null", error.kind(), error.message());
            }
        };

        if !connection.initialized() || request.method == "control/initialize" {
            let primary_before = self.session.primary_client_id().cloned();
            let observer_count_before = self.session.observers().len();
            let response = dispatch_control_request_for_connection(
                body,
                &mut self.session,
                connection,
                &mut self.control_idempotency,
            );
            if response.contains(r#""result""#)
                && let Err(error) = self.apply_runtime_initialize_side_effects(
                    &request,
                    primary_before.as_ref(),
                    observer_count_before,
                )
            {
                return runtime_json_rpc_error(&request.id, error.kind(), error.message());
            }
            return response;
        }

        let Some(caller_client_id) = connection.caller_client_id().cloned() else {
            return runtime_json_rpc_error(
                &request.id,
                crate::error::MezErrorKind::Forbidden,
                "control connection has no authenticated session client",
            );
        };
        if let Err(error) = authorize_control_request(&self.session, &caller_client_id, &request) {
            return runtime_json_rpc_error(&request.id, error.kind(), error.message());
        }
        if let Err(error) = validate_control_method_params_schema(&request) {
            return runtime_json_rpc_error(&request.id, error.kind(), error.message());
        }

        if request.method == "pane/capture" {
            return self.dispatch_runtime_pane_capture(body, &request.id, &caller_client_id);
        }

        if request.method.starts_with("approval/") {
            return self.dispatch_runtime_approval_request(body, &request, &caller_client_id);
        }

        if request.method == "terminal/view" {
            return match self
                .dispatch_runtime_terminal_view(&caller_client_id, request.params.as_deref())
            {
                Ok(result) => format!(
                    r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                    request.id
                ),
                Err(error) => runtime_json_rpc_error(&request.id, error.kind(), error.message()),
            };
        }

        if request.method.starts_with("snapshot/") {
            let Some(snapshots) = snapshots else {
                return runtime_json_rpc_error(
                    &request.id,
                    crate::error::MezErrorKind::InvalidState,
                    "runtime snapshot repository is not configured",
                );
            };
            if request.method == "snapshot/resume" {
                let result = self.dispatch_runtime_snapshot_resume_for_connection(
                    &request,
                    snapshots,
                    connection,
                    &caller_client_id,
                );
                let response_succeeded = result.is_ok();
                if let Err(error) = self.append_runtime_snapshot_audit(
                    &request,
                    &caller_client_id,
                    if response_succeeded {
                        "applied"
                    } else {
                        "failed"
                    },
                ) {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
                return match result {
                    Ok(result) => format!(
                        r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                        request.id
                    ),
                    Err(error) => {
                        runtime_json_rpc_error(&request.id, error.kind(), error.message())
                    }
                };
            }
            let captures = self.live_snapshot_pane_captures();
            let active_config_layers = self.live_snapshot_config_layers();
            let frame_state = self.live_snapshot_frame_state();
            let agent_sessions = self.live_snapshot_agent_sessions();
            let approval_grants = self.live_snapshot_approval_grants();
            let approval_requests = self.live_snapshot_approval_requests();
            let message_state = self.live_snapshot_message_state();
            let mcp_servers = self.live_snapshot_mcp_servers();
            let response = dispatch_control_request_for_client_with_snapshot_context(
                body,
                &mut self.session,
                &caller_client_id,
                snapshots,
                SnapshotCreationContext::new(
                    &captures,
                    &active_config_layers,
                    &frame_state,
                    &agent_sessions,
                )
                .with_approvals(&approval_grants, &approval_requests)
                .with_message_state(&message_state)
                .with_mcp_servers(&mcp_servers),
            );
            let response_succeeded = response.contains(r#""result""#);
            if let Err(error) = self.append_runtime_snapshot_audit(
                &request,
                &caller_client_id,
                if response_succeeded {
                    "applied"
                } else {
                    "failed"
                },
            ) {
                return runtime_json_rpc_error(&request.id, error.kind(), error.message());
            }
            if response_succeeded && request.method == "snapshot/create" {
                let _ = self.append_lifecycle_event(
                    EventKind::SnapshotChanged,
                    format!(r#"{{"method":"{}","live_capture":true}}"#, request.method),
                );
            }
            return response;
        }

        if !runtime_mutating_method(&request.method) {
            if request.method == "event/list" {
                return match self.dispatch_runtime_event_list_request(&request, &caller_client_id) {
                    Ok(result) => format!(
                        r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                        request.id
                    ),
                    Err(error) => {
                        runtime_json_rpc_error(&request.id, error.kind(), error.message())
                    }
                };
            }
            match self.dispatch_runtime_read_only_state_request(&request) {
                Ok(Some(result)) => {
                    return format!(
                        r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                        request.id
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            }
            if agent_state_control_method(&request.method) {
                if request.method == "agent/list" {
                    let model_profiles_by_pane = self.runtime_agent_model_profiles_by_pane();
                    return dispatch_control_request_for_client_with_agent_state_and_model_profiles(
                        body,
                        &mut self.session,
                        &caller_client_id,
                        None,
                        &mut self.agent_shell_store,
                        &self.agent_turn_ledger,
                        Some(&model_profiles_by_pane),
                    );
                }
                if matches!(
                    request.method.as_str(),
                    "agent/shell/show" | "agent/shell/hide"
                ) {
                    return self.dispatch_runtime_agent_shell_visibility_request(
                        body,
                        &request,
                        &caller_client_id,
                    );
                }
                return dispatch_control_request_for_client_with_agent_state(
                    body,
                    &mut self.session,
                    &caller_client_id,
                    None,
                    &mut self.agent_shell_store,
                    &self.agent_turn_ledger,
                );
            }
            if request.method.starts_with("config/") {
                return self.dispatch_runtime_config_request(body, &request, &caller_client_id);
            }
            if runtime_project_trust_read_method(&request.method) {
                return self.dispatch_runtime_project_trust_request(&request, &caller_client_id);
            }
            if request.method == "mcp/list" {
                return dispatch_control_request_with_mcp(
                    body,
                    &mut self.session,
                    &caller_client_id,
                    &self.mcp_registry,
                );
            }
            return dispatch_control_request_for_connection(
                body,
                &mut self.session,
                connection,
                &mut self.control_idempotency,
            );
        }
        self.dispatch_runtime_mutating_request(request, &caller_client_id)
    }

    /// Runs the append lifecycle event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_lifecycle_event(
        &mut self,
        kind: EventKind,
        payload: String,
    ) -> Result<()> {
        if let Some(event_log) = &mut self.event_log {
            event_log.append(
                kind,
                Some(self.session.id.to_string()),
                EventVisibility::SessionView,
                payload.clone(),
            )?;
        }
        if let Some(hook_event) = runtime_hook_event_for_lifecycle(kind, &payload) {
            self.run_configured_completed_hooks(hook_event, &payload)?;
        }
        Ok(())
    }
}

/// Builds an explicit model-visible readiness hint for non-ready panes.
fn runtime_agent_pane_readiness_context_block(
    pane_id: &str,
    readiness_state: PaneReadinessState,
) -> Option<ContextBlock> {
    if readiness_state == PaneReadinessState::Ready {
        return None;
    }
    let state_name = runtime_pane_readiness_state_name(readiness_state);
    let content = match readiness_state {
        PaneReadinessState::Unknown
        | PaneReadinessState::PromptCandidate
        | PaneReadinessState::Probing
        | PaneReadinessState::Busy
        | PaneReadinessState::Degraded => format!(
            "pane_id={pane_id} readiness_state={state_name}\n\
             Shell-backed actions for this pane may be delayed or rejected until Mezzanine confirms a safe shell boundary. \
             Do not assume shell_command or apply_patch can execute immediately unless later action results show the pane became ready."
        ),
        PaneReadinessState::FullScreen
        | PaneReadinessState::PasswordPrompt
        | PaneReadinessState::InteractiveBlocked => format!(
            "pane_id={pane_id} readiness_state={state_name}\n\
             Foreground interactive content is still active in this pane, so shell_command and apply_patch cannot execute until the user exits that UI or the pane readiness changes. \
             If local shell work is required, report the blockage or tell the user to return the pane to its shell prompt instead of emitting shell-backed actions immediately."
        ),
        PaneReadinessState::Ready => return None,
    };
    Some(ContextBlock {
        source: ContextSourceKind::RuntimeHint,
        label: "pane readiness".to_string(),
        content,
    })
}

/// Runs the runtime mcp retry result json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_mcp_retry_result_json(report: &super::RuntimeMcpRetryReport) -> String {
    let diagnostic = report
        .reason
        .as_deref()
        .map(|reason| {
            format!(
                r#"{{"severity":"error","message":"{}"}}"#,
                json_escape(reason)
            )
        })
        .unwrap_or_else(|| "[]".to_string());
    let diagnostics = if report.reason.is_some() {
        format!("[{diagnostic}]")
    } else {
        diagnostic
    };
    format!(
        r#"{{"server_id":"{}","retried":true,"previous_status":"{}","status":"{}","retryable_before_retry":{},"rediscovered":{},"tools":{},"reason":{},"diagnostics":{diagnostics}}}"#,
        json_escape(&report.server_id),
        report.previous_status_name(),
        report.status_name(),
        report.retryable_before_retry,
        report.rediscovered,
        report.tools,
        report
            .reason
            .as_deref()
            .map(|reason| format!(r#""{}""#, json_escape(reason)))
            .unwrap_or_else(|| "null".to_string())
    )
}

/// Runs the runtime mutating response is cacheable operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_mutating_response_is_cacheable(method: &str) -> bool {
    method != "terminal/step"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ProviderTranscriptEvent;
    use crate::transcript::{TranscriptEntry, TranscriptRole};

    /// Verifies only provider-native system transcript entries become model
    /// context.
    ///
    /// Ordinary system transcript entries are durable audit records rather than
    /// chat history. DeepSeek replay metadata is also stored with the system
    /// role, but it must survive runtime transcript filtering so request
    /// assembly can render it back into native assistant/tool messages.
    #[test]
    fn runtime_transcript_context_preserves_provider_native_system_entries() {
        let provider_event = ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id: "call_1".to_string(),
            content: "action result".to_string(),
        }
        .to_transcript_content();
        let entries = vec![
            TranscriptEntry {
                conversation_id: "conv1".to_string(),
                sequence: 1,
                created_at_unix_seconds: 100,
                role: TranscriptRole::System,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                content: "ordinary system audit record".to_string(),
            },
            TranscriptEntry {
                conversation_id: "conv1".to_string(),
                sequence: 2,
                created_at_unix_seconds: 100,
                role: TranscriptRole::System,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                content: provider_event.clone(),
            },
            TranscriptEntry {
                conversation_id: "conv1".to_string(),
                sequence: 3,
                created_at_unix_seconds: 100,
                role: TranscriptRole::Assistant,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                content: "visible assistant history".to_string(),
            },
        ];

        let blocks = runtime_agent_transcript_context_blocks("%1", &entries);

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].content, provider_event);
        assert!(ProviderTranscriptEvent::from_transcript_content(&blocks[0].content).is_some());
        assert_eq!(blocks[1].content, "visible assistant history");
    }
}
