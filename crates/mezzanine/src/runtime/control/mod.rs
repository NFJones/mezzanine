//! Runtime Control implementation.
//!
//! This module owns the runtime control boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.
mod approval;
mod component;
mod configuration;
mod context;
mod external_presentation;
mod ingress;
mod lifecycle;
mod live_snapshot;
mod message;
mod mutations;
mod protocol;
mod remote;
mod snapshot;
mod state;
mod subagents;
use super::{
    AgentContext, AgentId, AgentShellStore, AgentTurnLedger, AgentTurnState, ApprovalDecision,
    ApprovalDecisionScopePersistence, AttachedTerminalClientStepPlan, AuditActor, AuditRecord,
    BlockedApprovalRequest, ClientRole, ClientState, ClientViewRole, CommandRule, CommandRuleScope,
    ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation, ConfigScope, ContextBlock,
    ContextSourceKind, ControlConnectionState, DEFAULT_COMMAND_SHELL_CLASSIFICATION, Envelope,
    EventKind, EventVisibility, HookEvent, MemoryRecord, MezError, PaneCaptureSource, PaneId,
    PaneProcessStart, PaneReadinessState, Path, PathBuf, ProjectTrustStore, Recipient, Result,
    RuleDecision, RuleMatch, RuntimeAutoSizingConfig, RuntimeLifecycleState,
    RuntimeRegistryUpdatePlan, RuntimeSessionService, RuntimeSideEffect, RuntimeSubagentLineage,
    RuntimeSubagentPlacement, SUBAGENT_FRIENDLY_NAMES, SenderIdentity, SessionRecord,
    SnapshotCreationContext, SnapshotRepository, SplitDirection, SubagentScopeDeclaration,
    SubagentSpawnRequest, TaskState, TaskStatusPayload, TerminalClientLoopAction,
    TerminalClientLoopConfig, TrustDecision, agent_state_control_method,
    approval_decide_scope_persistence, compare_permission_preset_authority, current_unix_seconds,
    default_trust_database_path, destination_target_checked_resolved, discover_project_root,
    dispatch_control_request_cached, dispatch_control_request_for_client_with_agent_state,
    dispatch_control_request_for_client_with_agent_state_and_model_profiles,
    dispatch_control_request_for_client_with_config,
    dispatch_control_request_for_client_with_config_and_audit,
    dispatch_control_request_for_client_with_snapshot_context,
    dispatch_control_request_for_connection, dispatch_control_request_with_approvals,
    dispatch_control_request_with_approvals_and_audit, dispatch_control_request_with_captures,
    dispatch_control_request_with_mcp, dispatch_snapshot_request_with_context_async, json_escape,
    layout_state_json, normalize_exact_command_text, pane_target_checked_resolved,
    parse_json_rpc_request, plan_config_mutation, project_trust_state_filter_from_params,
    rendered_client_view_json, route_client_input_actions, runtime_agent_turn_state_json,
    runtime_approval_decision_name_to_kind, runtime_approval_policy_name,
    runtime_config_apply_event_payload, runtime_config_method_applies_to_live_service,
    runtime_cooperation_mode_name, runtime_hook_event_for_lifecycle,
    runtime_initialize_requested_observer, runtime_initialize_requested_primary,
    runtime_initialize_terminal_size, runtime_json_bool_field, runtime_json_creation_command,
    runtime_json_input_bytes, runtime_json_optional_client_size, runtime_json_optional_size_field,
    runtime_json_optional_view_offset, runtime_json_rpc_error, runtime_json_size,
    runtime_json_start_directory, runtime_json_string_field,
    runtime_json_terminal_step_render_if_changed, runtime_mcp_retry_event_payload,
    runtime_mutating_method, runtime_pane_by_id, runtime_pane_readiness_state_name,
    runtime_path_under_project_root, runtime_permission_decision_hook_payload,
    runtime_permission_preset_name, runtime_project_root_param, runtime_project_trust_record_json,
    runtime_split_direction, runtime_string_array_json, runtime_subagent_placement_mode,
    runtime_subagent_spawn_request, runtime_subagent_state_json, runtime_terminal_step_result_json,
    runtime_trust_decision_name, runtime_trust_decision_param, snapshot_id_for_idempotency_key,
    source_pane_target_checked_resolved, validate_config_text, window_target_checked_resolved,
};
use crate::config::compose_effective_config;
use crate::control::AgentStateProjection;
use crate::control::{
    ControlPersistTarget, authorize_control_request, config_audit_outcome, config_audit_plan,
    config_mutation_plan_result_json, config_mutation_value_from_json, config_request_cache_key,
    config_response_advances_generation, persist_target_from_json,
    validate_control_method_params_schema,
};
use crate::integrations::skills::{BUILTIN_MEZ_REFERENCE_SKILL_NAME, load_skill_document};
pub(crate) use component::RuntimeControlComponent;
use context::runtime_agent_transcript_context;
pub(crate) use context::runtime_local_message_context_content;
use mez_agent::{
    SkillDocument, insert_context_block_by_placement, is_valid_skill_name, memory_context_blocks,
    parse_skill_prompt_invocation, project_guidance_context_block, skill_context_text,
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

/// Durable prompt-boundary identity for pane-local plan mode.
const AGENT_PLAN_MODE_CONTEXT_LABEL: &str = "agent shell plan-only mode";
/// Persistent plan-mode activation visible until a later deactivation event.
const AGENT_PLAN_MODE_ENABLED_CONTEXT: &str = "[plan-only mode]\nstate=enabled\nPlan only for this and subsequent user prompts until a later plan-mode event disables this policy. Do not edit, create, delete, or otherwise modify any files. Do not implement the plan or perform any write-capable actions; only inspect and describe the changes that would be needed.";
/// Persistent plan-mode deactivation superseding an earlier activation.
const AGENT_PLAN_MODE_DISABLED_CONTEXT: &str = "[plan-only mode]\nstate=disabled\nNormal task execution is restored for this and subsequent user prompts.";

/// Prompt-boundary context plus the runtime ownership facts derived with it.
pub(super) struct RuntimeAgentPromptContext {
    /// Complete durable context assembled for the new turn.
    pub(super) context: AgentContext,
    /// Highest unread local-message sequence included in the context.
    pub(super) delivered_message_sequence: Option<mez_agent::messaging::MessageSequence>,
    /// Number of replayed history events at the front of the context.
    pub(super) imported_history_events: usize,
    /// Environment projection frozen for this prompt, when available.
    pub(super) current_environment_snapshot: Option<String>,
    /// Environment transition newly appended for durable persistence, when any.
    pub(super) new_environment_snapshot: Option<String>,
}

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
        max_history_lines: usize,
    ) -> Result<AgentContext> {
        self.agent_context_for_pane_prompt_with_message_delivery(
            pane_id,
            prompt,
            max_history_lines,
            false,
        )
        .map(|prepared| prepared.context)
    }

    /// Builds prompt context and optionally includes the unread local-message
    /// prefix that the caller will acknowledge after storing the context.
    pub(super) fn agent_context_for_pane_prompt_with_message_delivery(
        &mut self,
        pane_id: &str,
        prompt: &str,
        _max_history_lines: usize,
        include_unread_messages: bool,
    ) -> Result<RuntimeAgentPromptContext> {
        if prompt.trim().is_empty() {
            return Err(MezError::invalid_args("agent prompt must not be empty"));
        }
        self.refresh_project_config_layers_for_pane(pane_id)?;
        self.settle_recoverable_pane_readiness_for_agent_prompt(pane_id)?;
        let history = self.runtime_agent_history_epoch_context(pane_id)?;
        let mut blocks = history.blocks;
        let imported_execution_events = history.execution_events;
        let imported_history_events = blocks.len();
        let mut delivered_message_sequence = None;
        if include_unread_messages {
            let now_ms = super::current_unix_seconds().saturating_mul(1000);
            let identity = self.ensure_runtime_message_identity(
                &format!("agent-{pane_id}"),
                PaneId::opaque(pane_id.to_string()),
                "agent",
                &[],
                now_ms,
            )?;
            if self
                .control
                .message_service()
                .subscription(&identity.agent_id)
                .is_none()
            {
                self.control
                    .message_service_mut()
                    .subscribe_from_retained_start(&identity.agent_id)?;
            }
            let pending_messages = self.control.message_service().receive_subscribed(
                &identity.agent_id,
                now_ms,
                usize::MAX,
            )?;
            for message in pending_messages.messages {
                insert_context_block_by_placement(
                    &mut blocks,
                    ContextBlock::reference_event(
                        ContextSourceKind::LocalMessage,
                        format!(
                            "local message sequence {} id {}",
                            message.sequence, message.envelope.id
                        ),
                        runtime_local_message_context_content(&message.envelope),
                    ),
                );
                delivered_message_sequence = Some(message.sequence);
            }
        }
        let previous_environment_snapshot = blocks
            .iter()
            .rev()
            .find(|block| {
                block.source == ContextSourceKind::Configuration
                    && block.label == "task environment snapshot"
            })
            .map(|block| block.content.as_str());
        let current_environment_snapshot =
            self.runtime_agent_environment_snapshot_content(pane_id, previous_environment_snapshot);
        let new_environment_snapshot = current_environment_snapshot
            .as_ref()
            .filter(|content| previous_environment_snapshot != Some(content.as_str()));
        let new_environment_snapshot = new_environment_snapshot.cloned();
        if let Some(content) = new_environment_snapshot.as_ref() {
            insert_context_block_by_placement(
                &mut blocks,
                ContextBlock {
                    source: ContextSourceKind::Configuration,
                    placement: mez_agent::ContextPlacement::ConversationAppend,
                    label: "task environment snapshot".to_string(),
                    content: content.clone(),
                },
            );
        }
        let previous_mcp_catalog_snapshot = blocks
            .iter()
            .rev()
            .find(|block| block.source == ContextSourceKind::McpCatalogSnapshot)
            .map(|block| block.content.as_str());
        let current_mcp_catalog_snapshot = mez_agent::configured_mcp_catalog_snapshot_content(
            &self.mcp_registry().prompt_summary(),
            self.integration.always_exposed_mcp_servers(),
        )
        .or_else(|| {
            previous_mcp_catalog_snapshot
                .map(|_| mez_agent::MCP_CATALOG_REMOVED_CONTEXT.to_string())
        });
        if let Some(content) = current_mcp_catalog_snapshot
            .filter(|content| previous_mcp_catalog_snapshot != Some(content.as_str()))
        {
            insert_context_block_by_placement(
                &mut blocks,
                ContextBlock::reference_event(
                    ContextSourceKind::McpCatalogSnapshot,
                    mez_agent::MCP_CATALOG_SNAPSHOT_CONTEXT_LABEL,
                    content,
                ),
            );
        }
        let instruction_files = self
            .pane_agent_instruction_files(pane_id)
            .map(<[_]>::to_vec);
        if let Some(instruction_files) = instruction_files.as_deref()
            && instruction_files.iter().any(|file| file.truncated)
        {
            let truncated_paths: Vec<&str> = instruction_files
                .iter()
                .filter(|file| file.truncated)
                .map(|file| file.path.as_str())
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
        if let Some(instruction_files) = instruction_files.as_deref()
            && let Some(block) = project_guidance_context_block(instruction_files, 2)?
        {
            insert_context_block_by_placement(&mut blocks, block);
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
            insert_context_block_by_placement(
                &mut blocks,
                ContextBlock {
                    source: ContextSourceKind::SkillInstruction,
                    placement: mez_agent::ContextPlacement::ConversationAppend,
                    label: format!("explicit skill {}", invocation.name),
                    content: self.runtime_skill_context_text(
                        document.clone(),
                        invocation.additional_context.as_deref(),
                    )?,
                },
            );
            if document.summary.name == BUILTIN_MEZ_REFERENCE_SKILL_NAME {
                insert_context_block_by_placement(
                    &mut blocks,
                    ContextBlock {
                        source: ContextSourceKind::Configuration,
                        placement: mez_agent::ContextPlacement::ConversationAppend,
                        label: format!(
                            "explicit skill {} invocation-time config snapshot",
                            invocation.name
                        ),
                        content: format!(
                            "Effective Mezzanine config snapshot at skill invocation time. Later settled config_change results supersede this snapshot.\n\n```text\n{}\n```",
                            self.runtime_mez_config_skill_current_config()?
                        ),
                    },
                );
            }
        }
        let planning_enabled = self.agent_planning_enabled(pane_id);
        let previous_plan_mode = blocks
            .iter()
            .rev()
            .find(|block| {
                block.source == ContextSourceKind::Policy
                    && block.label == AGENT_PLAN_MODE_CONTEXT_LABEL
            })
            .map(|block| block.content.as_str());
        let current_plan_mode = planning_enabled
            .then_some(AGENT_PLAN_MODE_ENABLED_CONTEXT)
            .or_else(|| previous_plan_mode.map(|_| AGENT_PLAN_MODE_DISABLED_CONTEXT));
        if current_plan_mode.is_some_and(|content| previous_plan_mode != Some(content)) {
            insert_context_block_by_placement(
                &mut blocks,
                ContextBlock {
                    source: ContextSourceKind::Policy,
                    placement: mez_agent::ContextPlacement::ConversationAppend,
                    label: AGENT_PLAN_MODE_CONTEXT_LABEL.to_string(),
                    content: current_plan_mode.unwrap_or_default().to_string(),
                },
            );
        }
        insert_context_block_by_placement(
            &mut blocks,
            ContextBlock::user_event("user prompt", prompt),
        );
        let metadata = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| {
                mez_agent::ModelContextMetadata::new(
                    Some(session.session_id.clone()),
                    Some(session.prompt_cache_lineage_id.clone()),
                )
            })
            .unwrap_or_default();
        let mut context = AgentContext::import_durable_blocks(blocks)?.with_metadata(metadata);
        context
            .restore_imported_execution_events(&imported_execution_events)
            .map_err(|error| MezError::invalid_state(error.to_string()))?;
        Ok(RuntimeAgentPromptContext {
            context,
            delivered_message_sequence,
            imported_history_events,
            current_environment_snapshot,
            new_environment_snapshot,
        })
    }

    /// Builds the canonical bounded pane-environment projection for one prompt.
    ///
    /// A missing first signature preserves historical behavior and emits no
    /// speculative state. Losing a previously known signature appends an
    /// explicit unavailable transition so the model cannot treat stale facts
    /// as current.
    fn runtime_agent_environment_snapshot_content(
        &self,
        pane_id: &str,
        previous: Option<&str>,
    ) -> Option<String> {
        let Some(signature) = self.pane_environment_signature(pane_id) else {
            return previous.map(|_| {
                "environment_state=unavailable\nreason=pane_environment_signature_unavailable"
                    .to_string()
            });
        };
        let mut fields = vec!["environment_state=known".to_string()];
        fields.extend(signature.model_context_fields());
        if let Some(inventory) = self.agent_tool_inventory(signature) {
            fields.push(format!(
                "available_tools={} sed={} grep={} rg={}",
                inventory.tools.len(),
                inventory.sed,
                inventory.grep,
                inventory.rg
            ));
            if !inventory.modern_tools.is_empty() {
                let mut modern_tools = inventory.modern_tools.clone();
                modern_tools.sort();
                modern_tools.dedup();
                fields.push(format!("tools={}", modern_tools.join(",")));
            }
        }
        Some(fields.join("\n"))
    }

    /// Formats immutable skill context for one invocation.
    pub(super) fn runtime_skill_context_text(
        &self,
        document: SkillDocument,
        additional_context: Option<&str>,
    ) -> Result<String> {
        Ok(skill_context_text(&document, additional_context))
    }

    /// Builds the current-config snapshot appended to `$mez-reference`.
    fn runtime_mez_config_skill_current_config(&self) -> Result<String> {
        let effective = compose_effective_config(self.integration.config_layers())?;
        let mut lines = vec![format!(
            "layers={} applied_layers={} skipped_layers={} values={} diagnostics={}",
            self.integration.config_layers().len(),
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
        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Vec::new();
        };
        let compact_memory_id =
            mez_agent::memory::canonical_memory_uuid(&format!("compact-{}", session.session_id));
        self.memory_records()
            .into_iter()
            .filter(|record| record.id == compact_memory_id)
            .collect()
    }

    /// Builds one frozen historical epoch shared by initial prompt restoration
    /// and active-turn compaction refresh.
    ///
    /// The epoch order is invariant: older compact memory precedes the retained
    /// newer raw transcript. Task-local messages, prelude, prompt, steering, and
    /// same-turn execution events are appended by their owning producers after
    /// this epoch.
    fn runtime_agent_history_epoch_context(
        &self,
        pane_id: &str,
    ) -> Result<context::RuntimeAgentTranscriptContext> {
        let context_memory_records = self.model_context_memory_records_for_pane(pane_id);
        let mut blocks = Vec::new();
        let mut execution_events = Vec::new();
        blocks.extend(memory_context_blocks(
            &context_memory_records
                .iter()
                .map(mez_agent::MemoryContextRecord::from)
                .collect::<Vec<_>>(),
            1,
        ));

        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Ok(context::RuntimeAgentTranscriptContext {
                blocks,
                execution_events,
            });
        };
        let Some(store) = self.persistence.transcript_store() else {
            return Ok(context::RuntimeAgentTranscriptContext {
                blocks,
                execution_events,
            });
        };
        let transcript_conversation_id = session
            .ephemeral_transcript_source_conversation_id
            .as_deref()
            .unwrap_or(session.session_id.as_str());
        let transcript_entries = if session.ephemeral {
            session.ephemeral_transcript_source_entries
        } else {
            session.transcript_entries
        };
        if transcript_entries == 0 {
            return Ok(context::RuntimeAgentTranscriptContext {
                blocks,
                execution_events,
            });
        }
        let mut entries = match store.inspect(transcript_conversation_id) {
            Ok(entries) => entries,
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        entries.extend(
            self.persistence
                .pending_transcript_entries(transcript_conversation_id),
        );
        entries.sort_by_key(|entry| entry.sequence);
        entries.dedup_by_key(|entry| entry.sequence);
        if session.ephemeral {
            entries.retain(|entry| entry.sequence <= transcript_entries);
        } else {
            let active_entries = usize::try_from(transcript_entries).unwrap_or(usize::MAX);
            let first_active = entries.len().saturating_sub(active_entries);
            entries.drain(..first_active);
        }
        if !entries.is_empty() {
            let transcript = runtime_agent_transcript_context(pane_id, &entries);
            blocks.extend(transcript.blocks);
            execution_events = transcript.execution_events;
        }
        Ok(context::RuntimeAgentTranscriptContext {
            blocks,
            execution_events,
        })
    }

    /// Refreshes transcript and compact-memory context for a running turn.
    ///
    /// Automatic provider recovery can compact a pane conversation while the
    /// active turn remains running. The provider retry must then see the newly
    /// written summary and shorter transcript tail without discarding same-turn
    /// action results, steering, or other durable chronology.
    pub(crate) fn refresh_running_turn_context_after_conversation_compaction(
        &mut self,
        turn_id: &str,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger()
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
        let Some(session) = self.agent_shell_store().get(&turn.pane_id).cloned() else {
            return Ok(false);
        };
        if session.running_turn_id.as_deref() != Some(turn_id) {
            return Ok(false);
        }

        let refreshed_history = self.runtime_agent_history_epoch_context(&turn.pane_id)?;
        let mut refreshed_blocks = refreshed_history.blocks;
        let refreshed_execution_events = refreshed_history.execution_events;
        let imported_history_events = self.agent_turn_imported_history_events(turn_id);

        if !self.agent_turn_has_new_environment_snapshot(turn_id)
            && let Some(current_environment_snapshot) = self
                .agent_turn_current_environment_snapshot(turn_id)
                .map(str::to_string)
            && refreshed_blocks
                .iter()
                .rev()
                .find(|block| {
                    block.source == ContextSourceKind::Configuration
                        && block.label == "task environment snapshot"
                })
                .is_none_or(|block| block.content != current_environment_snapshot)
        {
            refreshed_blocks.push(ContextBlock {
                source: ContextSourceKind::Configuration,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "task environment snapshot".to_string(),
                content: current_environment_snapshot,
            });
        }

        let Some(mut refreshed_context) = self.agent_turn_contexts().get(turn_id).cloned() else {
            return Ok(false);
        };
        if imported_history_events == 0 {
            return Ok(false);
        }
        let mut remaining_imported_events = imported_history_events;
        let replacement_count = refreshed_context.replace_imported_history_prefix(
            |_| {
                let owned = remaining_imported_events > 0;
                remaining_imported_events = remaining_imported_events.saturating_sub(1);
                owned
            },
            refreshed_blocks,
        )?;
        refreshed_context
            .restore_imported_execution_events(&refreshed_execution_events)
            .map_err(|error| MezError::invalid_state(error.to_string()))?;
        let refreshed_block_count = refreshed_context.blocks().len();
        self.agent_turn_contexts_mut()
            .insert(turn_id.to_string(), refreshed_context);
        self.set_agent_turn_imported_history_events(turn_id.to_string(), replacement_count);
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
        if self.session.lifecycle_state() == RuntimeLifecycleState::Killed {
            RuntimeRegistryUpdatePlan::Remove {
                session_id: self.session.id.to_string(),
            }
        } else {
            RuntimeRegistryUpdatePlan::Upsert(SessionRecord::from_session(
                &self.session,
                self.session.socket_path().to_path_buf(),
                self.session.created_at_unix_seconds(),
                self.session.last_attach_at_unix_seconds(),
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
        primary_client_id: &mez_core::ids::ClientId,
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
            if request.method == "pane/capture" {
                return self.dispatch_runtime_pane_capture(body, &request.id, primary_client_id);
            }
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
            match self.dispatch_runtime_read_only_state_request(&request, primary_client_id) {
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
                let (agent_shell_store, agent_turn_ledger) = self.agent.control_turn_state();
                return dispatch_control_request_for_client_with_agent_state_and_model_profiles(
                    body,
                    &mut self.session,
                    primary_client_id,
                    None,
                    agent_shell_store,
                    agent_turn_ledger,
                    AgentStateProjection::new(Some(&model_profiles_by_pane), None),
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
                let approval_ids_by_turn = self.blocked_agent_approval_ids_by_turn();
                let (agent_shell_store, agent_turn_ledger) = self.agent.control_turn_state();
                return dispatch_control_request_for_client_with_agent_state_and_model_profiles(
                    body,
                    &mut self.session,
                    primary_client_id,
                    None,
                    agent_shell_store,
                    agent_turn_ledger,
                    AgentStateProjection::new(None, Some(&approval_ids_by_turn)),
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
                    self.integration.mcp_registry(),
                );
            }
            return dispatch_control_request_cached(
                body,
                &mut self.session,
                primary_client_id,
                self.control.idempotency_mut(),
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
            match self.control.idempotency_mut().cached_response(
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
            self.control.idempotency_mut().remember_response(
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
        .with_navigation_source(&caller_client_id)
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
            let prepared = match self.prepare_remote_initialize_authority(&request, connection) {
                Ok(prepared) => prepared,
                Err(error) => {
                    let reason = match error.kind() {
                        crate::error::MezErrorKind::InvalidArgs => "invalid_params",
                        crate::error::MezErrorKind::InvalidState => "invalid_state",
                        crate::error::MezErrorKind::Config => "config_error",
                        crate::error::MezErrorKind::Io => "io_error",
                        crate::error::MezErrorKind::Conflict => "conflict",
                        crate::error::MezErrorKind::NotFound => "not_found",
                        crate::error::MezErrorKind::Forbidden => "forbidden",
                        crate::error::MezErrorKind::RateLimited => "rate_limited",
                        crate::error::MezErrorKind::NotImplemented => "not_implemented",
                    };
                    if let Err(audit_error) = self.append_runtime_remote_initialize_rejection_audit(
                        &request, connection, reason,
                    ) {
                        return runtime_json_rpc_error(
                            &request.id,
                            audit_error.kind(),
                            audit_error.message(),
                        );
                    }
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            };
            if let Some(prepared) = prepared {
                return self
                    .dispatch_prepared_remote_initialize(body, &request, connection, prepared);
            }

            let primary_count_before = self.session.attached_primaries().count();
            if runtime_initialize_requested_observer(&request) {
                connection.set_observer_visible_from_event_id(Some(
                    self.control
                        .event_log()
                        .map(|event_log| event_log.latest_event_id().saturating_add(1))
                        .unwrap_or_else(|| self.session.mutation_revision().saturating_add(1)),
                ));
            }
            let mut response = dispatch_control_request_for_connection(
                body,
                &mut self.session,
                connection,
                self.control.idempotency_mut(),
            );
            if response.contains(r#""result""#)
                && let Err(error) = self.apply_runtime_initialize_side_effects(
                    &request,
                    primary_count_before,
                    connection.caller_client_id(),
                )
            {
                return runtime_json_rpc_error(&request.id, error.kind(), error.message());
            }
            if response.contains(r#""result""#)
                && request
                    .params
                    .as_deref()
                    .and_then(|params| serde_json::from_str::<serde_json::Value>(params).ok())
                    .and_then(|params| params.get("event_stream_version").cloned())
                    .and_then(|version| version.as_u64())
                    == Some(1)
                && let Some(crate::control::AuthenticatedPeer::UnixUser { uid }) =
                    connection.authenticated_peer()
                && let Some(client_id) = connection.caller_client_id().cloned()
            {
                let (token, expires_at_unix_seconds) =
                    self.mint_unix_event_binding(client_id, *uid);
                if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&response)
                    && let Some(result) = value
                        .get_mut("result")
                        .and_then(serde_json::Value::as_object_mut)
                {
                    result.insert(
                        "event_binding".to_string(),
                        serde_json::json!({
                            "version": 1,
                            "token": token,
                            "expires_at_unix_seconds": expires_at_unix_seconds
                        }),
                    );
                    response = value.to_string();
                }
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

        if request.method.starts_with("remote/") {
            return match self.dispatch_runtime_remote_request(&request, connection) {
                Ok(result) => format!(
                    r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                    request.id
                ),
                Err(error) => runtime_json_rpc_error(&request.id, error.kind(), error.message()),
            };
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
                .with_navigation_source(&caller_client_id)
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
            match self.dispatch_runtime_read_only_state_request(&request, &caller_client_id) {
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
                    let (agent_shell_store, agent_turn_ledger) = self.agent.control_turn_state();
                    return dispatch_control_request_for_client_with_agent_state_and_model_profiles(
                        body,
                        &mut self.session,
                        &caller_client_id,
                        None,
                        agent_shell_store,
                        agent_turn_ledger,
                        AgentStateProjection::new(Some(&model_profiles_by_pane), None),
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
                let approval_ids_by_turn = self.blocked_agent_approval_ids_by_turn();
                let (agent_shell_store, agent_turn_ledger) = self.agent.control_turn_state();
                return dispatch_control_request_for_client_with_agent_state_and_model_profiles(
                    body,
                    &mut self.session,
                    &caller_client_id,
                    None,
                    agent_shell_store,
                    agent_turn_ledger,
                    AgentStateProjection::new(None, Some(&approval_ids_by_turn)),
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
                    self.integration.mcp_registry(),
                );
            }
            return dispatch_control_request_for_connection(
                body,
                &mut self.session,
                connection,
                self.control.idempotency_mut(),
            );
        }
        let may_detach_caller =
            matches!(request.method.as_str(), "client/detach" | "terminal/step");
        let request_id = request.id.clone();
        let response = self.dispatch_runtime_mutating_request(request, &caller_client_id);
        if may_detach_caller
            && response.contains(r#""result""#)
            && !self.session.is_attached_primary(&caller_client_id)
            && let Err(error) = connection.deactivate_x11_route()
        {
            return runtime_json_rpc_error(&request_id, error.kind(), error.message());
        }
        response
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
        if let Some(event_log) = self.control.event_log_mut() {
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

impl RuntimeSessionService {
    /// Settles stale passive readiness before constructing a model request.
    ///
    /// Prompt context is durable guidance for the next provider turn, so it
    /// should not expose a transient post-transaction state once host process
    /// metadata already proves the pane is back at the primary shell prompt.
    /// Explicit foreground-interactive and genuinely unknown states remain
    /// non-ready so the model-visible warning continues to protect pane-shell
    /// input.
    fn settle_recoverable_pane_readiness_for_agent_prompt(&mut self, pane_id: &str) -> Result<()> {
        let previous = self.pane_readiness_state(pane_id);
        if previous == PaneReadinessState::Ready {
            return Ok(());
        }
        let foreground_primary_shell = self.pane_foreground_certified_shell_state(pane_id);
        let recoverable_passive_state = matches!(
            previous,
            PaneReadinessState::PromptCandidate | PaneReadinessState::Busy
        );
        if !recoverable_passive_state || foreground_primary_shell != Some(true) {
            return Ok(());
        }
        self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","readiness_event":"prompt_context_settled","previous_state":"{}","state":"ready"}}"#,
                json_escape(pane_id),
                runtime_pane_readiness_state_name(previous)
            ),
        )?;
        Ok(())
    }
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
fn runtime_mutating_response_is_cacheable(_method: &str) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::runtime_agent_transcript_context;
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};
    use mez_agent::{AgentContext, ProviderTranscriptEvent, TranscriptContextEvent};

    /// Verifies typed provider events and canonical assistant rationale survive
    /// transcript restoration into model context.
    ///
    /// Ordinary system transcript entries are durable audit records rather than
    /// chat history. DeepSeek replay metadata is also stored with the system
    /// role, but it must survive runtime transcript filtering so request
    /// assembly can render it back into native assistant/tool messages. Routed
    /// summaries use their dedicated context source, while malformed or unknown
    /// reserved events remain filtered with ordinary system records. Canonical
    /// assistant text must be imported byte-for-byte so issue selection and
    /// action intent remain available after restoration.
    #[test]
    fn runtime_transcript_context_preserves_provider_native_system_entries() {
        let provider_event = ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id: "call_1".to_string(),
            content: "[action_result action-1 shell_command succeeded]\noutput:\nnative-secret"
                .to_string(),
        }
        .to_transcript_content();
        let routed_handoff = TranscriptContextEvent::RoutedHandoff {
            content: r#"{"version":1,"result_summary":"durable summary"}"#.to_string(),
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
                role: TranscriptRole::System,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                content: routed_handoff,
            },
            TranscriptEntry {
                conversation_id: "conv1".to_string(),
                sequence: 4,
                created_at_unix_seconds: 100,
                role: TranscriptRole::System,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                content: format!(
                    "{}{}",
                    mez_agent::TRANSCRIPT_CONTEXT_EVENT_MARKER,
                    r#"{"version":"mez-transcript-context-event/v2","kind":"routed_handoff","content":"must not appear"}"#
                ),
            },
            TranscriptEntry {
                conversation_id: "conv1".to_string(),
                sequence: 5,
                created_at_unix_seconds: 100,
                role: TranscriptRole::Assistant,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                content: concat!(
                    "rationale: selected iss-42 because it is the oldest unblocked issue\n",
                    "thinking: Active issue: iss-42\n",
                    "action rationale query-1 (issue_query): inspect the selected issue"
                )
                .to_string(),
            },
            TranscriptEntry {
                conversation_id: "conv1".to_string(),
                sequence: 6,
                created_at_unix_seconds: 100,
                role: TranscriptRole::Tool,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                content: "[action_result query-1 issue_query succeeded]".to_string(),
            },
        ];

        let blocks = runtime_agent_transcript_context("%1", &entries).blocks;

        assert_eq!(blocks.len(), 4);
        assert_eq!(
            blocks[0].source,
            mez_agent::ContextSourceKind::TranscriptTool
        );
        let restored_provider_event =
            ProviderTranscriptEvent::from_transcript_content(&blocks[0].content).unwrap();
        let ProviderTranscriptEvent::DeepSeekToolResult { content, .. } = restored_provider_event
        else {
            panic!("expected restored DeepSeek tool result");
        };
        assert!(content.contains("[action_result action-1 shell_command succeeded]"));
        assert!(content.contains("historical_output: omitted"));
        assert!(!content.contains("native-secret"));
        assert_eq!(
            blocks[1].source,
            mez_agent::ContextSourceKind::RoutedHandoff
        );
        assert_eq!(blocks[1].label, "routed worker handoff context");
        assert_eq!(
            blocks[1].content,
            r#"{"version":1,"result_summary":"durable summary"}"#
        );
        assert_eq!(
            blocks[2].content,
            concat!(
                "rationale: selected iss-42 because it is the oldest unblocked issue\n",
                "thinking: Active issue: iss-42\n",
                "action rationale query-1 (issue_query): inspect the selected issue"
            )
        );
        assert_eq!(blocks[3].source, mez_agent::ContextSourceKind::ActionResult);
        assert_eq!(
            blocks[3].content,
            "[action_result query-1 issue_query succeeded]\nhistorical_output: omitted"
        );
        assert!(
            blocks
                .iter()
                .all(|block| !block.content.contains("must not appear"))
        );
    }

    /// Verifies persisted neutral and provider-native records restore as one
    /// complete causal execution group.
    ///
    /// Terminal persistence deliberately stores the neutral assistant first,
    /// then hidden native continuity records, then the generic action result.
    /// Runtime transcript conversion and compatibility import must preserve
    /// that order and infer one owner so provider assembly can select either
    /// the native or neutral projection without mixing them.
    #[test]
    fn runtime_transcript_restoration_recovers_provider_execution_group() {
        let native_assistant = ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content: String::new(),
            reasoning_content: Some("query issues".to_string()),
            tool_calls: vec![serde_json::json!({
                "id": "call-1",
                "type": "function",
                "function": {
                    "name": "submit_maap_action_batch",
                    "arguments": "{}"
                }
            })],
        }
        .to_transcript_content();
        let native_result = ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id: "call-1".to_string(),
            content: "[action_result query-1 issue_query succeeded]".to_string(),
        }
        .to_transcript_content();
        let contents = [
            (
                TranscriptRole::Assistant,
                "rationale: inspect issues\nthinking: Active issue: iss-42".to_string(),
            ),
            (TranscriptRole::System, native_assistant),
            (TranscriptRole::System, native_result),
            (
                TranscriptRole::Tool,
                "[action_result query-1 issue_query succeeded]".to_string(),
            ),
        ];
        let entries = contents
            .into_iter()
            .enumerate()
            .map(|(index, (role, content))| TranscriptEntry {
                conversation_id: "conv1".to_string(),
                sequence: u64::try_from(index).unwrap().saturating_add(1),
                created_at_unix_seconds: 100,
                role,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .collect::<Vec<_>>();

        let transcript = runtime_agent_transcript_context("%1", &entries);
        let mut context = AgentContext::import_durable_blocks(transcript.blocks).unwrap();
        context
            .restore_imported_execution_events(&transcript.execution_events)
            .unwrap();
        let group_ids = context
            .chronology()
            .iter()
            .map(|event| event.execution_group_id().cloned())
            .collect::<Vec<_>>();

        assert!(group_ids[0].is_some());
        assert!(group_ids.iter().all(|group| group == &group_ids[0]));
        assert_eq!(
            context.chronology()[3].block().source,
            mez_agent::ContextSourceKind::ActionResult
        );
        assert!(context.chronology()[1].provider_owner().is_some());
        assert!(context.chronology()[2].provider_owner().is_some());
    }

    /// Verifies typed execution-block records supersede display-oriented rows
    /// and restore the original cache-visible source, label, content, and order.
    #[test]
    fn runtime_transcript_restoration_replays_exact_execution_blocks() {
        let group = mez_agent::ContextExecutionGroupId::new("execution-group-abc123").unwrap();
        let request_state = TranscriptContextEvent::execution_block_with_metadata(
            mez_agent::ContextSourceKind::CommittedEvidence,
            "Mezzanine request state",
            "generation=1\ninteraction_kind=action_execution\nallowed_actions=say",
            group.clone(),
            1,
            None,
        )
        .unwrap();
        let decision = TranscriptContextEvent::execution_block_with_metadata(
            mez_agent::ContextSourceKind::CommittedEvidence,
            "controller capability decision",
            "[capability continuation]\ncontinue with the granted surface",
            group.clone(),
            2,
            None,
        )
        .unwrap();
        let assistant = TranscriptContextEvent::execution_block_with_metadata(
            mez_agent::ContextSourceKind::TranscriptAssistant,
            "assistant response for turn-1 execution abc123",
            "rationale: inspect the repository",
            group.clone(),
            3,
            None,
        )
        .unwrap();
        let result = TranscriptContextEvent::execution_block_with_metadata(
            mez_agent::ContextSourceKind::ActionResult,
            "action result shell-1",
            "[action_result shell-1 shell_command succeeded]\noutput:\nexact output",
            group.clone(),
            4,
            None,
        )
        .unwrap();
        let contents = [
            (TranscriptRole::User, "inspect the repository".to_string()),
            (
                TranscriptRole::Assistant,
                "display-oriented assistant row".to_string(),
            ),
            (
                TranscriptRole::Tool,
                "[action_result shell-1 shell_command succeeded]".to_string(),
            ),
            (
                TranscriptRole::System,
                request_state.to_transcript_content(),
            ),
            (TranscriptRole::System, decision.to_transcript_content()),
            (TranscriptRole::System, assistant.to_transcript_content()),
            (TranscriptRole::System, result.to_transcript_content()),
        ];
        let entries = contents
            .into_iter()
            .enumerate()
            .map(|(index, (role, content))| TranscriptEntry {
                conversation_id: "conv1".to_string(),
                sequence: u64::try_from(index).unwrap().saturating_add(1),
                created_at_unix_seconds: 100,
                role,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .collect::<Vec<_>>();

        let transcript = runtime_agent_transcript_context("%1", &entries);
        let blocks = transcript.blocks.clone();

        assert_eq!(blocks.len(), 5, "{blocks:#?}");
        assert_eq!(
            blocks[0].source,
            mez_agent::ContextSourceKind::TranscriptUser
        );
        assert_eq!(
            blocks[1].source,
            mez_agent::ContextSourceKind::CommittedEvidence
        );
        assert_eq!(blocks[1].label, "Mezzanine request state");
        assert_eq!(
            blocks[2].source,
            mez_agent::ContextSourceKind::CommittedEvidence
        );
        assert_eq!(blocks[2].label, "controller capability decision");
        assert_eq!(
            blocks[3].source,
            mez_agent::ContextSourceKind::TranscriptAssistant
        );
        assert_eq!(
            blocks[3].label,
            "assistant response for turn-1 execution abc123"
        );
        assert_eq!(blocks[3].content, "rationale: inspect the repository");
        assert_eq!(blocks[4].source, mez_agent::ContextSourceKind::ActionResult);
        assert_eq!(blocks[4].label, "action result shell-1");
        assert_eq!(
            blocks[4].content,
            "[action_result shell-1 shell_command succeeded]\noutput:\nexact output"
        );
        assert!(
            blocks
                .iter()
                .all(|block| { !block.content.contains("display-oriented assistant row") })
        );
        let mut context = AgentContext::import_durable_blocks(blocks).unwrap();
        context
            .restore_imported_execution_events(&transcript.execution_events)
            .unwrap();
        context.validate_durable().unwrap();
        assert!(
            context.chronology()[1..]
                .iter()
                .all(|event| event.execution_group_id() == Some(&group))
        );
    }

    /// Verifies a legacy raw-MAAP owner cannot collapse its result into the
    /// preceding canonical assistant execution group.
    ///
    /// Legacy or alternate persistence can contain a raw action-batch JSON
    /// assistant record followed by a generic action result. Replay omits the
    /// protocol JSON, but it must retain a safe canonical assistant boundary:
    /// otherwise the result is incorrectly represented as evidence owned by
    /// the preceding canonical assistant execution.
    #[test]
    fn runtime_transcript_restoration_preserves_owner_for_legacy_raw_maap_result() {
        let contents = [
            (
                TranscriptRole::Assistant,
                "rationale: inspect repository\naction inspect-1: shell_command".to_string(),
            ),
            (
                TranscriptRole::Tool,
                "[action_result inspect-1 shell_command succeeded]".to_string(),
            ),
            (
                TranscriptRole::Assistant,
                serde_json::json!({
                    "actions": [{
                        "type": "shell_command",
                        "action_id": "legacy-1"
                    }]
                })
                .to_string(),
            ),
            (
                TranscriptRole::Tool,
                "[action_result legacy-1 shell_command succeeded]".to_string(),
            ),
        ];
        let entries = contents
            .into_iter()
            .enumerate()
            .map(|(index, (role, content))| TranscriptEntry {
                conversation_id: "conv1".to_string(),
                sequence: u64::try_from(index).unwrap().saturating_add(1),
                created_at_unix_seconds: 100,
                role,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .collect::<Vec<_>>();

        let transcript = runtime_agent_transcript_context("%1", &entries);
        let mut context = AgentContext::import_durable_blocks(transcript.blocks).unwrap();
        context
            .restore_imported_execution_events(&transcript.execution_events)
            .unwrap();
        let first_group = context.chronology()[0].execution_group_id().cloned();
        let legacy_owner = &context.chronology()[2];
        let legacy_result = &context.chronology()[3];

        assert!(first_group.is_some());
        assert_eq!(
            context.chronology()[1].execution_group_id(),
            first_group.as_ref()
        );
        assert_eq!(
            legacy_owner.block().content,
            "[legacy MAAP assistant execution omitted from transcript replay]"
        );
        assert_ne!(legacy_owner.execution_group_id(), first_group.as_ref());
        assert_eq!(
            legacy_result.execution_group_id(),
            legacy_owner.execution_group_id()
        );
        context.validate_durable().unwrap();
    }
}
