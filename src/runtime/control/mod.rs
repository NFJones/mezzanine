//! Runtime Control implementation.
//!
//! This module owns the runtime control boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.
mod configuration;
mod subagents;
use super::{
    AgentContext, AgentId, AgentScheduler, AgentShellStore, AgentShellVisibility, AgentTurnLedger,
    AgentTurnState, ApprovalDecision, ApprovalDecisionScopePersistence, ApprovalGrant,
    ApprovalScope, AttachedTerminalClientStepPlan, AuditActor, AuditRecord, BlockedApprovalRequest,
    BlockedApprovalState, ClientRole, ClientState, ClientViewRole, CommandRule, CommandRuleScope,
    ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation, ConfigScope, ContextBlock,
    ContextSourceKind, ControlConnectionState, DEFAULT_COMMAND_SHELL_CLASSIFICATION,
    DeferredConfigFileWrite, DeferredProjectConfigWrite, Envelope, EventKind, EventVisibility,
    HookEvent, McpApprovalSetting, McpExternalCapability, McpServerKind, McpServerStatus,
    McpToolEffects, McpToolState, MemoryRecord, MessageConnection, MessageServiceSnapshot,
    MezError, PaneCaptureSource, PaneId, PaneProcessStart, PaneReadinessOverrideStore,
    PaneReadinessState, PaneResizeUpdate, Path, PathBuf, ProjectTrustStore, Recipient, Result,
    RuleDecision, RuleMatch, RuntimeAutoSizingConfig, RuntimeLifecycleState,
    RuntimeRegistryUpdatePlan, RuntimeSessionService, RuntimeSnapshotControlAsyncOutcome,
    RuntimeSnapshotControlAsyncWork, RuntimeSnapshotControlAsyncWorkKind,
    RuntimeSnapshotOwnedCreationContext, RuntimeSubagentLineage, RuntimeSubagentPlacement,
    SUBAGENT_FRIENDLY_NAMES, ScopeRegistry, SenderIdentity, SessionRecord, SnapshotAgentSession,
    SnapshotApprovalGrantMetadata, SnapshotApprovalRequestMetadata, SnapshotConfigDiagnostic,
    SnapshotConfigLayerMetadata, SnapshotCreationContext, SnapshotFrameSettings,
    SnapshotFrameState, SnapshotMcpExternalCapability, SnapshotMcpServerState,
    SnapshotMcpToolEffects, SnapshotMcpToolState, SnapshotPaneCapture, SnapshotRepository,
    SnapshotState, SplitDirection, SubagentScopeDeclaration, SubagentSpawnRequest, TaskState,
    TaskStatusPayload, TerminalClientLoopAction, TerminalClientLoopConfig, TerminalFramePosition,
    TerminalFrameStyle, TranscriptEntry, TranscriptRole, TrustDecision, agent_state_control_method,
    append_memory_context, append_permission_policy_context, append_scheduler_context,
    approval_decide_scope_persistence, compare_permission_preset_authority, current_unix_seconds,
    decode_control_frame, decode_mmp_frame, default_trust_database_path,
    destination_target_checked_resolved, discover_project_root, dispatch_control_request_cached,
    dispatch_control_request_for_client_with_agent_state,
    dispatch_control_request_for_client_with_agent_state_and_model_profiles,
    dispatch_control_request_for_client_with_config,
    dispatch_control_request_for_client_with_config_and_audit,
    dispatch_control_request_for_client_with_snapshot_context,
    dispatch_control_request_for_connection, dispatch_control_request_with_approvals,
    dispatch_control_request_with_approvals_and_audit, dispatch_control_request_with_captures,
    dispatch_control_request_with_mcp, dispatch_event_list_request,
    dispatch_snapshot_request_with_context_async, encode_control_body,
    frame_read_json_with_context, fs, handle_mmp_frame, json_escape, layout_state_json,
    normalize_exact_command_text, observer_json, observers_json, pane_target_checked_resolved,
    parse_json_rpc_request, plan_config_mutation, project_trust_state_filter_from_params,
    rendered_client_view_json, route_client_input_actions, runtime_agent_turn_state_json,
    runtime_append_observer_decision_audit, runtime_approval_decision_name_to_kind,
    runtime_approval_policy_name, runtime_config_apply_event_payload,
    runtime_config_method_applies_to_live_service, runtime_cooperation_mode_name,
    runtime_hook_event_for_lifecycle, runtime_initialize_requested_observer,
    runtime_initialize_requested_primary, runtime_initialize_terminal_size,
    runtime_json_bool_field, runtime_json_creation_command, runtime_json_input_bytes,
    runtime_json_optional_client_size, runtime_json_optional_size_field,
    runtime_json_optional_view_offset, runtime_json_rpc_error, runtime_json_size,
    runtime_json_start_directory, runtime_json_string_field, runtime_mcp_retry_event_payload,
    runtime_mutating_method, runtime_pane_by_id, runtime_pane_readiness_state_name,
    runtime_path_under_project_root, runtime_permission_decision_hook_payload,
    runtime_permission_preset_name, runtime_project_root_param, runtime_project_trust_record_json,
    runtime_split_direction, runtime_string_array_json, runtime_subagent_placement_mode,
    runtime_subagent_spawn_request, runtime_subagent_state_json, runtime_terminal_step_result_json,
    runtime_trust_decision_name, runtime_trust_decision_param, session_state_name,
    set_project_guidance_context, snapshot_id_for_idempotency_key,
    source_pane_target_checked_resolved, state_request_pane_list_window_ids,
    state_request_session_target_matches, unix_seconds_to_rfc3339, validate_config_text,
    window_target_checked_resolved,
};
use crate::agent::ProviderTranscriptEvent;

use crate::config::compose_effective_config;
use crate::control::{
    ControlPersistTarget, authorize_control_request, config_audit_outcome, config_audit_plan,
    config_mutation_plan_result_json, config_mutation_value_from_json, config_request_cache_key,
    config_response_advances_generation, persist_target_from_json,
    validate_control_method_params_schema,
};
use crate::skills::{
    BUILTIN_MEZ_CONFIG_SKILL_NAME, SkillDocument, is_valid_skill_name, load_skill_document,
    parse_skill_prompt_invocation, skill_context_text,
};

// Runtime control, message, event, and mutation dispatch.

/// Defines the RUNTIME CONTROL LIVE OVERRIDE LAYER const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_CONTROL_LIVE_OVERRIDE_LAYER: &str = "runtime-control-live-override";
/// Defines the AGENT LOCAL MESSAGE CONTEXT PAYLOAD CHARS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const AGENT_LOCAL_MESSAGE_CONTEXT_PAYLOAD_CHARS: usize = 256 * 1024;
/// Defines the AGENT TRANSCRIPT CONTEXT READ BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const AGENT_TRANSCRIPT_CONTEXT_READ_BYTES: u64 = 100 * 1024 * 1024;
const AGENT_TRANSCRIPT_TOOL_CONTEXT_LIMIT_BYTES: usize = 256 * 1024;

/// Returns the number of transcript entries from the current post-compaction
/// window that may be replayed into model context.
fn runtime_transcript_context_entry_limit(entries_since_compaction: u64) -> usize {
    usize::try_from(entries_since_compaction).unwrap_or(usize::MAX)
}

/// Runs the runtime project trust read method operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_project_trust_read_method(method: &str) -> bool {
    matches!(method, "project/trust/list" | "project/trust/inspect")
}

/// Runs the runtime agent transcript context operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_agent_transcript_context_blocks(
    pane_id: &str,
    entries: &[TranscriptEntry],
) -> Vec<ContextBlock> {
    let context_entries = entries
        .iter()
        .filter(|entry| {
            entry.role != TranscriptRole::System
                || ProviderTranscriptEvent::from_transcript_content(&entry.content).is_some()
        })
        .collect::<Vec<_>>();
    let mut blocks = Vec::new();
    for entry in context_entries {
        let Some(content) = runtime_transcript_entry_context_content(entry) else {
            continue;
        };
        blocks.push(ContextBlock {
            source: runtime_transcript_context_source_kind(entry.role),
            label: format!(
                "previous {} message for pane {pane_id}",
                runtime_context_transcript_role_name(entry.role)
            ),
            content,
        });
    }
    blocks
}

/// Returns true for context blocks owned by transcript replay or compact memory.
fn runtime_context_block_is_compaction_refresh_owned(block: &ContextBlock) -> bool {
    match block.source {
        ContextSourceKind::Transcript
        | ContextSourceKind::TranscriptUser
        | ContextSourceKind::TranscriptTool => true,
        ContextSourceKind::TranscriptAssistant => block
            .label
            .starts_with("previous assistant message for pane "),
        ContextSourceKind::Memory => {
            block.label == "conversation compaction notice"
                || block.label.starts_with("memory compact-")
        }
        _ => false,
    }
}

/// Maps a stored transcript role to a model-context source that preserves the
/// role across request assembly.
fn runtime_transcript_context_source_kind(role: TranscriptRole) -> ContextSourceKind {
    match role {
        TranscriptRole::User => ContextSourceKind::TranscriptUser,
        TranscriptRole::Assistant => ContextSourceKind::TranscriptAssistant,
        TranscriptRole::Tool => ContextSourceKind::TranscriptTool,
        TranscriptRole::System => ContextSourceKind::Transcript,
    }
}

/// Returns model-facing transcript content after removing protocol scaffolding
/// that is useful for durable audit but harmful as future prompt context.
fn runtime_transcript_entry_context_content(entry: &TranscriptEntry) -> Option<String> {
    match entry.role {
        TranscriptRole::System
            if ProviderTranscriptEvent::from_transcript_content(&entry.content).is_some() =>
        {
            Some(entry.content.clone())
        }
        TranscriptRole::System => None,
        TranscriptRole::Tool => runtime_transcript_tool_context_content(&entry.content),
        TranscriptRole::User if transcript_content_looks_like_skill_context(&entry.content) => None,
        TranscriptRole::Assistant
            if transcript_content_looks_like_maap_action_json(&entry.content) =>
        {
            None
        }
        _ => Some(entry.content.clone()),
    }
}

/// Returns transcript tool output for model-facing replay.
///
/// Previous action results are often the user's freshest evidence, especially
/// failed file reads and shell observations. Historical replay should stay
/// byte-stable so later turns see the same durable tool context they already
/// observed.
fn runtime_transcript_tool_context_content(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    if transcript_tool_content_is_omitted_for_replay(trimmed) {
        return None;
    }
    Some(truncate_runtime_context_text(
        trimmed,
        AGENT_TRANSCRIPT_TOOL_CONTEXT_LIMIT_BYTES,
        "transcript tool context",
    ))
}

/// Returns whether one durable tool transcript payload should stay out of
/// later model context because it is metadata or workflow body rather than
/// execution evidence.
fn transcript_tool_content_is_omitted_for_replay(content: &str) -> bool {
    content.starts_with("[action_result ")
        && [" fetch_url ", " web_search "]
            .iter()
            .any(|needle| content.contains(needle))
        || content.contains("action_type=request_skills")
        || content.contains("action_type=call_skill")
}

/// Reports whether transcript text is an expanded skill body rather than the
/// user's original prompt.
fn transcript_content_looks_like_skill_context(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("# Skill: ")
        && trimmed.contains("\nSource: ")
        && trimmed.contains("\nPath: ")
        && trimmed.contains("\nInvocation state: this skill is already loaded")
}

/// Reports whether transcript text is a raw MAAP action object rather than
/// conversational assistant content.
fn transcript_content_looks_like_maap_action_json(content: &str) -> bool {
    let trimmed = content.trim();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    object.contains_key("actions") || object.contains_key("action_batch")
}

/// Runs the runtime context transcript role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_context_transcript_role_name(role: TranscriptRole) -> &'static str {
    match role {
        TranscriptRole::User => "user",
        TranscriptRole::Assistant => "assistant",
        TranscriptRole::Tool => "tool",
        TranscriptRole::System => "system",
    }
}

/// Returns bounded context text without splitting UTF-8 characters.
fn truncate_runtime_context_text(content: &str, max_bytes: usize, label: &str) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }
    let mut end = max_bytes;
    while !content.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}...[mez: {label} truncated; original_bytes={}]",
        &content[..end],
        content.len()
    )
}

/// Returns bounded local-message context including the message payload.
fn runtime_local_message_context_content(envelope: &Envelope) -> String {
    let mut lines = vec![format!(
        "from={} id={} type={} content_type={} ttl_ms={}",
        envelope.sender.agent_id,
        envelope.id,
        envelope.message_type,
        envelope.content_type,
        envelope
            .ttl_ms
            .map_or("none".to_string(), |ms| ms.to_string())
    )];
    if let Some(correlation_id) = &envelope.correlation_id {
        lines.push(format!("correlation_id={correlation_id}"));
    }
    lines.push("payload:".to_string());
    lines.push(truncate_runtime_context_text(
        &envelope.payload,
        AGENT_LOCAL_MESSAGE_CONTEXT_PAYLOAD_CHARS,
        "local message payload",
    ));
    lines.join("\n")
}

/// Runs the runtime validate state request params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_validate_state_request_params(
    params: Option<&str>,
    method: &str,
    allowed: &[&str],
) -> Result<()> {
    let Some(params) = params else {
        return Ok(());
    };
    let value = serde_json::from_str::<serde_json::Value>(params)
        .map_err(|_| MezError::invalid_args(format!("{method} params must be a JSON object")))?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{method} params must be a JSON object")))?;
    if let Some(key) = object
        .keys()
        .find(|key| !allowed.iter().any(|allowed| allowed == &key.as_str()))
    {
        return Err(MezError::invalid_args(format!(
            "{method} params contains unknown field `{key}`"
        )));
    }
    Ok(())
}

/// Runs the runtime optional string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_optional_string(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the runtime mmp message type operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_mmp_message_type(body: &str) -> Option<String> {
    runtime_json_string_field(body, "type")
        .or_else(|| runtime_json_string_field(body, "message_type"))
}

/// Runs the runtime mmp response succeeded operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_mmp_response_succeeded(output: &[u8], max_content_length: usize) -> bool {
    decode_mmp_frame(output, max_content_length)
        .map(|(body, _)| !body.contains(r#""type":"error""#))
        .unwrap_or(false)
}

/// Runs the paths equivalent operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn paths_equivalent(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

/// Derives the pane identity encoded by runtime-created agent ids.
///
/// Runtime subagents use `agent-%pane` identifiers so MMP discovery can connect
/// an agent identity back to its terminal pane without adding another mapping
/// store. Agent ids outside that convention remain valid opaque identities.
fn pane_id_from_runtime_agent_id(agent_id: &str) -> Option<PaneId> {
    agent_id
        .strip_prefix("agent-")
        .and_then(|pane_id| PaneId::parse('%', pane_id.to_string()))
}

/// Runs the runtime snapshot resume plan json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_resume_plan_json(plan: &crate::snapshot::LayoutLoadPlan) -> String {
    format!(
        r#"{{"session_id":"{}","window_count":{},"pane_count":{},"restart_required_panes":{},"limitations":{}}}"#,
        json_escape(&plan.session_id),
        plan.window_count,
        plan.pane_count,
        runtime_string_array_json(&plan.restart_required_panes),
        runtime_string_array_json(&plan.limitations)
    )
}

/// Runs the runtime snapshot id from request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_id_from_request(request: &crate::control::JsonRpcRequest) -> String {
    request
        .params
        .as_deref()
        .and_then(|params| runtime_json_string_field(params, "snapshot_id"))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Runs the runtime timestamp json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_timestamp_json(value: u64) -> String {
    format!(r#""{}""#, unix_seconds_to_rfc3339(value))
}

/// Runs the runtime optional timestamp json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_optional_timestamp_json(value: Option<u64>) -> String {
    value
        .map(runtime_timestamp_json)
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the runtime client role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_client_role_name(role: ClientRole) -> &'static str {
    match role {
        ClientRole::Primary => "primary",
        ClientRole::PendingObserver => "pending_observer",
        ClientRole::Observer => "observer",
        ClientRole::Agent => "agent",
        ClientRole::Automation => "automation",
    }
}

/// Runs the runtime client requested role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_client_requested_role_name(role: ClientRole) -> &'static str {
    match role {
        ClientRole::PendingObserver => "observer",
        ClientRole::Primary => "primary",
        ClientRole::Observer => "observer",
        ClientRole::Agent => "agent",
        ClientRole::Automation => "automation",
    }
}

/// Runs the runtime client state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_client_state_name(state: ClientState) -> &'static str {
    match state {
        ClientState::Attached => "attached",
        ClientState::Pending => "pending",
        ClientState::Detached => "detached",
        ClientState::Revoked => "revoked",
        ClientState::Failed => "failed",
    }
}

/// Runs the runtime size object json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_size_object_json(size: Option<crate::layout::Size>) -> String {
    size.map(|size| format!(r#"{{"columns":{},"rows":{}}}"#, size.columns, size.rows))
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the runtime client terminal descriptor json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_client_terminal_descriptor_json(
    size: Option<crate::layout::Size>,
    term: &str,
) -> String {
    size.map(|size| {
        format!(
            r#"{{"columns":{},"rows":{},"term":"{}"}}"#,
            size.columns,
            size.rows,
            json_escape(term)
        )
    })
    .unwrap_or_else(|| "null".to_string())
}

impl RuntimeSessionService {
    /// Runs the handle control input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn handle_control_input(
        &mut self,
        input: &[u8],
        max_content_length: usize,
    ) -> Result<(Vec<u8>, usize)> {
        self.require_live()?;
        let primary_client_id = self.session.primary_client_id().cloned().ok_or_else(|| {
            MezError::invalid_state("control service requires an attached primary")
        })?;
        let mut offset = 0usize;
        let mut output = Vec::new();
        while offset < input.len() {
            let (body, consumed) = decode_control_frame(&input[offset..], max_content_length)?;
            let response = self.dispatch_runtime_control_body(&body, &primary_client_id);
            output.extend_from_slice(&encode_control_body(&response));
            offset += consumed;
        }
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        self.persist_or_defer_registry_update()?;
        Ok((output, offset))
    }

    /// Runs the handle control input for connection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn handle_control_input_for_connection(
        &mut self,
        input: &[u8],
        max_content_length: usize,
        connection: &mut ControlConnectionState,
    ) -> Result<(Vec<u8>, usize)> {
        self.require_live()?;
        let mut offset = 0usize;
        let mut output = Vec::new();
        while offset < input.len() {
            let (body, consumed) = decode_control_frame(&input[offset..], max_content_length)?;
            let response = self.dispatch_runtime_control_body_for_connection(&body, connection);
            output.extend_from_slice(&encode_control_body(&response));
            offset += consumed;
        }
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        self.persist_or_defer_registry_update()?;
        Ok((output, offset))
    }

    /// Runs the handle control input for connection with snapshots operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn handle_control_input_for_connection_with_snapshots(
        &mut self,
        input: &[u8],
        max_content_length: usize,
        connection: &mut ControlConnectionState,
        snapshots: &SnapshotRepository,
    ) -> Result<(Vec<u8>, usize)> {
        self.require_live()?;
        let mut offset = 0usize;
        let mut output = Vec::new();
        while offset < input.len() {
            let (body, consumed) = decode_control_frame(&input[offset..], max_content_length)?;
            let response = self.dispatch_runtime_control_body_for_connection_with_snapshots(
                &body, connection, snapshots,
            );
            output.extend_from_slice(&encode_control_body(&response));
            offset += consumed;
        }
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        self.persist_or_defer_registry_update()?;
        Ok((output, offset))
    }

    /// Runs the handle control input for connection with snapshots async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn handle_control_input_for_connection_with_snapshots_async(
        &mut self,
        input: &[u8],
        max_content_length: usize,
        connection: &mut ControlConnectionState,
        snapshots: &SnapshotRepository,
    ) -> Result<(Vec<u8>, usize)> {
        self.require_live()?;
        let mut offset = 0usize;
        let mut output = Vec::new();
        while offset < input.len() {
            let (body, consumed) = decode_control_frame(&input[offset..], max_content_length)?;
            let response = self
                .dispatch_runtime_control_body_for_connection_with_snapshots_async(
                    &body, connection, snapshots,
                )
                .await;
            output.extend_from_slice(&encode_control_body(&response));
            offset += consumed;
        }
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        self.persist_or_defer_registry_update()?;
        Ok((output, offset))
    }

    /// Prepares a single snapshot control request for repository I/O outside
    /// the actor turn.
    ///
    /// Non-snapshot requests, initialization requests, and unauthenticated
    /// connections return `None` so the caller can use the ordinary control
    /// dispatch path. Snapshot request validation errors are converted to a
    /// JSON-RPC response body because they are successful protocol handling,
    /// not actor transport failures.
    pub(crate) fn prepare_runtime_snapshot_control_async_work(
        &self,
        body: &str,
        connection: &ControlConnectionState,
    ) -> Option<std::result::Result<RuntimeSnapshotControlAsyncWork, String>> {
        let request = match parse_json_rpc_request(body) {
            Ok(request) => request,
            Err(error) => {
                return Some(Err(runtime_json_rpc_error(
                    "null",
                    error.kind(),
                    error.message(),
                )));
            }
        };
        if !connection.initialized()
            || request.method == "control/initialize"
            || !request.method.starts_with("snapshot/")
        {
            return None;
        }
        let Some(caller_client_id) = connection.caller_client_id().cloned() else {
            return Some(Err(runtime_json_rpc_error(
                &request.id,
                crate::error::MezErrorKind::Forbidden,
                "control connection has no authenticated session client",
            )));
        };
        if let Err(error) = authorize_control_request(&self.session, &caller_client_id, &request) {
            return Some(Err(runtime_json_rpc_error(
                &request.id,
                error.kind(),
                error.message(),
            )));
        }
        if let Err(error) = validate_control_method_params_schema(&request) {
            return Some(Err(runtime_json_rpc_error(
                &request.id,
                error.kind(),
                error.message(),
            )));
        }
        let kind = if request.method == "snapshot/resume" {
            RuntimeSnapshotControlAsyncWorkKind::Resume {
                shell: self.session.shell.clone(),
            }
        } else {
            RuntimeSnapshotControlAsyncWorkKind::Dispatch {
                session: Box::new(self.session.clone()),
                context: Box::new(RuntimeSnapshotOwnedCreationContext {
                    pane_captures: self.live_snapshot_pane_captures(),
                    active_config_layers: self.live_snapshot_config_layers(),
                    frame_state: self.live_snapshot_frame_state(),
                    agent_sessions: self.live_snapshot_agent_sessions(),
                    approval_grants: self.live_snapshot_approval_grants(),
                    approval_requests: self.live_snapshot_approval_requests(),
                    message_state: self.live_snapshot_message_state(),
                    mcp_servers: self.live_snapshot_mcp_servers(),
                }),
            }
        };
        Some(Ok(RuntimeSnapshotControlAsyncWork {
            request,
            caller_client_id,
            kind,
        }))
    }

    /// Completes a snapshot control request after repository I/O finished
    /// outside the actor turn.
    pub(crate) fn complete_runtime_snapshot_control_async_work(
        &mut self,
        work: RuntimeSnapshotControlAsyncWork,
        outcome: RuntimeSnapshotControlAsyncOutcome,
        connection: &mut ControlConnectionState,
    ) -> String {
        let _ = connection;
        let result = match outcome {
            RuntimeSnapshotControlAsyncOutcome::Dispatch(result) => result,
            RuntimeSnapshotControlAsyncOutcome::Resume(result) => {
                result.and_then(|(payload, _restored)| {
                    self.require_snapshot_resume_hooks_allow(&payload)?;
                    let snapshot_id = runtime_snapshot_id_from_request(&work.request);
                    let resume_plan = payload.resume_plan();
                    self.apply_runtime_snapshot_resume_for_connection(
                        snapshot_id.as_str(),
                        payload,
                        resume_plan,
                        &work.caller_client_id,
                    )
                })
            }
        };
        let response_succeeded = result.is_ok();
        if let Err(error) = self.append_runtime_snapshot_audit(
            &work.request,
            &work.caller_client_id,
            if response_succeeded {
                "applied"
            } else {
                "failed"
            },
        ) {
            return runtime_json_rpc_error(&work.request.id, error.kind(), error.message());
        }
        if response_succeeded && work.request.method == "snapshot/create" {
            let _ = self.append_lifecycle_event(
                EventKind::SnapshotChanged,
                format!(
                    r#"{{"method":"{}","live_capture":true}}"#,
                    work.request.method
                ),
            );
        }
        let body = match result {
            Ok(result) => format!(
                r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                work.request.id
            ),
            Err(error) => runtime_json_rpc_error(&work.request.id, error.kind(), error.message()),
        };
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        body
    }

    /// Runs the live snapshot pane captures operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn live_snapshot_pane_captures(&self) -> Vec<SnapshotPaneCapture> {
        self.session
            .windows()
            .iter()
            .flat_map(|window| window.panes().iter())
            .map(|pane| {
                let pane_id = pane.id.as_str();
                let screen = self.pane_screens.get(pane_id);
                let history_styled_lines = screen
                    .map(|screen| screen.history().styled_lines().collect::<Vec<_>>())
                    .unwrap_or_default();
                let visible_styled_lines = screen
                    .map(|screen| {
                        if screen.alternate_screen_active() {
                            Vec::new()
                        } else {
                            screen.visible_styled_lines()
                        }
                    })
                    .unwrap_or_default();
                let primary_pid = self.primary_pid_for_live_pane_process(pane_id);
                let process_state = if primary_pid.is_some() {
                    "running"
                } else if pane.live {
                    "starting"
                } else {
                    "exited"
                };
                SnapshotPaneCapture {
                    pane_id: pane_id.to_string(),
                    primary_pid,
                    process_state: Some(process_state.to_string()),
                    current_working_directory: self
                        .pane_current_working_directory(pane_id)
                        .map(|path| path.to_string_lossy().to_string()),
                    readiness_state: Some(
                        runtime_pane_readiness_state_name(self.pane_readiness_state(pane_id))
                            .to_string(),
                    ),
                    terminal_history: history_styled_lines
                        .iter()
                        .map(|line| line.text.clone())
                        .collect(),
                    terminal_history_line_style_spans: history_styled_lines
                        .into_iter()
                        .map(|line| line.style_spans)
                        .collect(),
                    visible_lines: visible_styled_lines
                        .iter()
                        .map(|line| line.text.clone())
                        .collect(),
                    visible_line_style_spans: visible_styled_lines
                        .into_iter()
                        .map(|line| line.style_spans)
                        .collect(),
                    terminal_modes: screen.map(|screen| screen.mode_state()).unwrap_or_default(),
                    terminal_saved_state: screen
                        .map(|screen| screen.saved_state())
                        .unwrap_or_default(),
                    exit_status: self
                        .pane_exit_records
                        .get(pane_id)
                        .map(|record| record.exit_status),
                    alternate_screen_active: screen
                        .is_some_and(|screen| screen.alternate_screen_active()),
                    transcript_refs: self.snapshot_transcript_refs_for_pane(pane_id),
                }
            })
            .collect()
    }

    /// Runs the snapshot transcript refs for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn snapshot_transcript_refs_for_pane(&self, pane_id: &str) -> Vec<String> {
        let mut refs = self
            .pane_transcript_refs
            .get(pane_id)
            .cloned()
            .unwrap_or_default();
        if let Some(session) = self.agent_shell_store.get(pane_id) {
            let transcript_ref = format!("transcript:{pane_id}:{}", session.session_id);
            if !refs.iter().any(|existing| existing == &transcript_ref) {
                refs.push(transcript_ref);
            }
        }
        refs
    }

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
            && session.transcript_entries > 0
        {
            let transcript_context_entries =
                runtime_transcript_context_entry_limit(session.transcript_entries);
            match store.inspect_recent(
                &session.session_id,
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
                source: ContextSourceKind::UserInstruction,
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
        if document.summary.name == BUILTIN_MEZ_CONFIG_SKILL_NAME {
            document.text = format!(
                "{}\n\n## Current effective Mezzanine config\n\n```text\n{}\n```",
                document.text.trim_end(),
                self.runtime_mez_config_skill_current_config()?
            );
        }
        Ok(skill_context_text(&document, additional_context))
    }

    /// Builds the current-config snapshot appended to `$mez-config`.
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
        if let Some(store) = self.agent_transcript_store.as_ref()
            && session.transcript_entries > 0
        {
            let transcript_context_entries =
                runtime_transcript_context_entry_limit(session.transcript_entries);
            match store.inspect_recent(
                &session.session_id,
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

    /// Runs the create live snapshot operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn create_live_snapshot(
        &self,
        snapshots: &SnapshotRepository,
        snapshot_id: &str,
        name: Option<String>,
    ) -> Result<SnapshotState> {
        let active_config_layers = self.live_snapshot_config_layers();
        let pane_captures = self.live_snapshot_pane_captures();
        let frame_state = self.live_snapshot_frame_state();
        let agent_sessions = self.live_snapshot_agent_sessions();
        let approval_grants = self.live_snapshot_approval_grants();
        let approval_requests = self.live_snapshot_approval_requests();
        let message_state = self.live_snapshot_message_state();
        let mcp_servers = self.live_snapshot_mcp_servers();
        snapshots.create_from_session_with_context(
            snapshot_id,
            name,
            &self.session,
            SnapshotCreationContext::new(
                &pane_captures,
                &active_config_layers,
                &frame_state,
                &agent_sessions,
            )
            .with_approvals(&approval_grants, &approval_requests)
            .with_message_state(&message_state)
            .with_mcp_servers(&mcp_servers),
        )
    }

    /// Runs the live snapshot agent sessions operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn live_snapshot_agent_sessions(&self) -> Vec<SnapshotAgentSession> {
        self.agent_shell_store
            .sessions()
            .map(|session| SnapshotAgentSession {
                pane_id: session.pane_id.clone(),
                conversation_id: session.session_id.clone(),
                visibility: runtime_snapshot_agent_visibility_name(session.visibility).to_string(),
                running_turn_id: session.running_turn_id.clone(),
                transcript_entries: session.transcript_entries,
            })
            .collect()
    }

    /// Runs the live snapshot approval grants operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn live_snapshot_approval_grants(&self) -> Vec<SnapshotApprovalGrantMetadata> {
        self.session_approvals
            .grants()
            .map(runtime_snapshot_approval_grant)
            .collect()
    }

    /// Runs the live snapshot approval requests operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn live_snapshot_approval_requests(&self) -> Vec<SnapshotApprovalRequestMetadata> {
        self.blocked_approvals
            .requests()
            .filter(|request| request.state != BlockedApprovalState::Pending)
            .map(runtime_snapshot_approval_request)
            .collect()
    }

    /// Runs the live snapshot message state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn live_snapshot_message_state(&self) -> MessageServiceSnapshot {
        self.message_service.snapshot_state()
    }

    /// Runs the live snapshot mcp servers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn live_snapshot_mcp_servers(&self) -> Vec<SnapshotMcpServerState> {
        self.mcp_registry
            .list_servers()
            .iter()
            .map(|server| runtime_snapshot_mcp_server_state(server))
            .collect()
    }

    /// Runs the live snapshot frame state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn live_snapshot_frame_state(&self) -> SnapshotFrameState {
        SnapshotFrameState {
            window: SnapshotFrameSettings {
                enabled: self.window_frames_enabled,
                position: runtime_snapshot_frame_position_name(self.window_frame_position)
                    .to_string(),
                style: runtime_snapshot_frame_style_name(self.window_frame_style).to_string(),
                template: self.window_frame_template.clone(),
                visible_fields: self.window_frame_visible_fields.clone(),
            },
            pane: SnapshotFrameSettings {
                enabled: self.pane_frames_enabled,
                position: runtime_snapshot_frame_position_name(self.pane_frame_position)
                    .to_string(),
                style: runtime_snapshot_frame_style_name(self.pane_frame_style).to_string(),
                template: self.pane_frame_template.clone(),
                visible_fields: self.pane_frame_visible_fields.clone(),
            },
        }
    }

    /// Runs the live snapshot config layers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn live_snapshot_config_layers(&self) -> Vec<SnapshotConfigLayerMetadata> {
        self.config_layers
            .iter()
            .enumerate()
            .map(|(precedence, layer)| {
                let validation = validate_config_text(layer.format, &layer.text, layer.scope);
                let applied = validation.valid
                    && (layer.scope != ConfigScope::ProjectOverlay || layer.trusted);
                SnapshotConfigLayerMetadata {
                    id: layer.name.clone(),
                    layer_type: runtime_snapshot_config_scope_name(layer.scope).to_string(),
                    precedence,
                    path: layer.path.as_ref().map(|path| path.display().to_string()),
                    trusted: layer.trusted,
                    applied,
                    schema_version: 1,
                    diagnostics: validation
                        .diagnostics
                        .into_iter()
                        .map(|diagnostic| SnapshotConfigDiagnostic {
                            path: diagnostic.path,
                            message: diagnostic.message,
                        })
                        .collect(),
                }
            })
            .collect()
    }

    /// Runs the handle message input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn handle_message_input(
        &mut self,
        input: &[u8],
        max_content_length: usize,
        connection: &mut MessageConnection,
        now_ms: u64,
    ) -> Result<(Vec<u8>, usize)> {
        self.require_live()?;
        let decoded_body = decode_mmp_frame(input, max_content_length)
            .ok()
            .map(|(body, _)| body);
        let previous_agent_id = connection.agent_id.clone();
        let (output, consumed) = handle_mmp_frame(
            input,
            max_content_length,
            &mut self.message_service,
            connection,
            now_ms,
        )?;
        if runtime_mmp_response_succeeded(&output, max_content_length)
            && let Some(body) = decoded_body.as_deref()
        {
            self.append_runtime_message_protocol_audit(
                body,
                previous_agent_id.as_ref(),
                connection.agent_id.as_ref(),
            )?;
        }
        Ok((output, consumed))
    }

    /// Runs the append runtime message protocol audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn append_runtime_message_protocol_audit(
        &mut self,
        body: &str,
        previous_agent_id: Option<&AgentId>,
        current_agent_id: Option<&AgentId>,
    ) -> Result<()> {
        let Some(message_type) = runtime_mmp_message_type(body) else {
            return Ok(());
        };
        let (change, bridge_id) = match message_type.as_str() {
            "hello" => ("register", current_agent_id),
            "presence" => ("presence", previous_agent_id.or(current_agent_id)),
            _ => return Ok(()),
        };
        let Some(bridge_id) = bridge_id else {
            return Ok(());
        };
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::local_protocol_bridge_change(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: bridge_id.to_string(),
            },
            "mmp/1",
            bridge_id.to_string(),
            change,
            "applied",
        );
        if let Some(role) = runtime_json_string_field(body, "role") {
            record = record.with_metadata("role", role);
        }
        if let Some(status) = runtime_json_string_field(body, "status") {
            record = record.with_metadata("status", status);
        }
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
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

    /// Runs the dispatch runtime read only state request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_read_only_state_request(
        &self,
        request: &crate::control::JsonRpcRequest,
    ) -> Result<Option<String>> {
        match request.method.as_str() {
            "session/list" => {
                runtime_validate_state_request_params(
                    request.params.as_deref(),
                    "session/list",
                    &[],
                )?;
                Ok(Some(format!(
                    r#"{{"sessions":[{}]}}"#,
                    self.runtime_session_summary_json()
                )))
            }
            "session/get" => {
                runtime_validate_state_request_params(
                    request.params.as_deref(),
                    "session/get",
                    &["target"],
                )?;
                state_request_session_target_matches(
                    &self.session,
                    request.params.as_deref(),
                    "session/get params",
                )?;
                Ok(Some(format!(
                    r#"{{"session":{}}}"#,
                    self.runtime_session_state_json()
                )))
            }
            "client/list" => {
                runtime_validate_state_request_params(
                    request.params.as_deref(),
                    "client/list",
                    &["target"],
                )?;
                state_request_session_target_matches(
                    &self.session,
                    request.params.as_deref(),
                    "client/list params",
                )?;
                Ok(Some(format!(
                    r#"{{"clients":{}}}"#,
                    self.runtime_clients_json()
                )))
            }
            "window/list" => {
                runtime_validate_state_request_params(
                    request.params.as_deref(),
                    "window/list",
                    &["target"],
                )?;
                state_request_session_target_matches(
                    &self.session,
                    request.params.as_deref(),
                    "window/list params",
                )?;
                Ok(Some(format!(
                    r#"{{"windows":{}}}"#,
                    self.runtime_windows_state_json()
                )))
            }
            "pane/list" => {
                runtime_validate_state_request_params(
                    request.params.as_deref(),
                    "pane/list",
                    &["target"],
                )?;
                let window_ids = state_request_pane_list_window_ids(
                    &self.session,
                    request.params.as_deref(),
                    "pane/list params",
                )?;
                Ok(Some(format!(
                    r#"{{"panes":{}}}"#,
                    match window_ids {
                        Some(window_ids) =>
                            self.runtime_panes_state_json_for_window_ids(&window_ids)?,
                        None => self.runtime_panes_state_json(),
                    }
                )))
            }
            "frame/read" => Ok(Some(frame_read_json_with_context(
                &self.session,
                request.params.as_deref(),
                &self.terminal_frame_context(),
            )?)),
            _ => Ok(None),
        }
    }

    /// Runs the dispatch runtime event list request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_event_list_request(
        &self,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &crate::ids::ClientId,
    ) -> Result<String> {
        let event_log = self
            .event_log
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("runtime event log is not configured"))?;
        dispatch_event_list_request(request, &self.session, caller_client_id, event_log)
    }

    /// Runs the runtime session summary json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_session_summary_json(&self) -> String {
        let session = &self.session;
        let active_window_id = session.active_window().map(|window| window.id.to_string());
        let attached_client_count = session
            .clients()
            .iter()
            .filter(|client| client.state == ClientState::Attached)
            .count();
        format!(
            r#"{{"id":"{}","version":1,"name":"{}","state":"{}","created_at":{},"last_attached_at":{},"window_count":{},"attached_client_count":{},"has_primary":{},"active_window_id":{}}}"#,
            json_escape(session.id.as_str()),
            json_escape(&session.name),
            session_state_name(session.state),
            runtime_timestamp_json(self.created_at_unix_seconds),
            runtime_optional_timestamp_json(self.last_attach_at_unix_seconds),
            session.windows().len(),
            attached_client_count,
            session.primary_client_id().is_some(),
            runtime_optional_string(active_window_id.as_deref())
        )
    }

    /// Runs the runtime session state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_session_state_json(&self) -> String {
        let session = &self.session;
        let primary_client_id = session
            .primary_client_id()
            .map(|client_id| client_id.to_string());
        let active_window_id = session.active_window().map(|window| window.id.to_string());
        let updated_at = self
            .last_attach_at_unix_seconds
            .unwrap_or(self.created_at_unix_seconds);
        format!(
            r#"{{"id":"{}","version":1,"session_id":"{}","name":"{}","state":"{}","created_at":{},"updated_at":{},"primary_client_id":{},"authoritative_size":{{"columns":{},"rows":{}}},"active_window_id":{},"windows":{},"window_count":{},"clients":{},"observers":{},"config_generation":{},"permission_summary":{}}}"#,
            json_escape(session.id.as_str()),
            json_escape(session.id.as_str()),
            json_escape(&session.name),
            session_state_name(session.state),
            runtime_timestamp_json(self.created_at_unix_seconds),
            runtime_timestamp_json(updated_at),
            runtime_optional_string(primary_client_id.as_deref()),
            session.authoritative_size.columns,
            session.authoritative_size.rows,
            runtime_optional_string(active_window_id.as_deref()),
            self.runtime_windows_state_json(),
            session.windows().len(),
            self.runtime_clients_json(),
            observers_json(session),
            session.config_generation,
            self.runtime_permission_summary_json()
        )
    }

    /// Runs the runtime permission summary json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_permission_summary_json(&self) -> String {
        let trusted_project = self
            .config_layers
            .iter()
            .any(|layer| layer.scope == ConfigScope::ProjectOverlay && layer.trusted);
        let trusted_directories = self
            .project_trust_store
            .as_ref()
            .map(|store| {
                store
                    .records()
                    .filter(|record| record.state == TrustDecision::Trusted)
                    .map(|record| record.project_root.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        format!(
            r#"{{"preset":"{}","approval_policy":"{}","bypass_active":{},"trusted_project":{},"trusted_directories":{},"read_scopes":[],"write_scopes":[],"command_rule_generation":{}}}"#,
            runtime_permission_preset_name(self.permission_policy.preset),
            runtime_approval_policy_name(self.permission_policy.approval_policy),
            self.permission_policy.approval_bypass(),
            trusted_project,
            runtime_string_array_json(&trusted_directories),
            self.permission_policy.rules().len()
        )
    }

    /// Runs the runtime clients json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_clients_json(&self) -> String {
        let clients = self
            .session
            .clients()
            .iter()
            .map(|client| self.runtime_client_state_json(client))
            .collect::<Vec<_>>();
        format!("[{}]", clients.join(","))
    }

    /// Runs the runtime client state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_client_state_json(&self, client: &crate::session::Client) -> String {
        let is_primary = self
            .session
            .primary_client_id()
            .is_some_and(|primary| primary == &client.id);
        let attached_at = if is_primary {
            self.last_attach_at_unix_seconds
                .or(client.attached_at_unix_seconds)
        } else {
            client.attached_at_unix_seconds
        };
        let last_seen_at = if is_primary {
            self.last_attach_at_unix_seconds
                .or(client.last_seen_at_unix_seconds)
        } else {
            client.last_seen_at_unix_seconds
        };
        let terminal_size =
            (is_primary && client.interactive).then_some(self.session.authoritative_size);
        format!(
            r#"{{"id":"{}","version":1,"client_id":"{}","name":"{}","role":"{}","requested_role":"{}","state":"{}","attached_at":{},"last_seen_at":{},"descriptor":{{"name":"{}","interactive":{},"terminal":{}}},"terminal_size":{},"interactive":{}}}"#,
            json_escape(client.id.as_str()),
            json_escape(client.id.as_str()),
            json_escape(&client.name),
            runtime_client_role_name(client.role),
            runtime_client_requested_role_name(client.role),
            runtime_client_state_name(client.state),
            runtime_optional_timestamp_json(attached_at),
            runtime_optional_timestamp_json(last_seen_at),
            json_escape(&client.name),
            client.interactive,
            runtime_client_terminal_descriptor_json(terminal_size, self.terminal_term()),
            runtime_size_object_json(terminal_size),
            client.interactive
        )
    }

    /// Runs the runtime windows state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_windows_state_json(&self) -> String {
        let windows = self
            .session
            .windows()
            .iter()
            .map(|window| self.runtime_window_state_json(window))
            .collect::<Vec<_>>();
        format!("[{}]", windows.join(","))
    }

    /// Runs the runtime panes state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_panes_state_json(&self) -> String {
        let panes = self
            .session
            .active_window()
            .map(|window| {
                window
                    .panes()
                    .iter()
                    .map(|pane| self.runtime_control_pane_state_json(window, pane))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        format!("[{}]", panes.join(","))
    }

    /// Runs the runtime panes state json for window ids operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_panes_state_json_for_window_ids(
        &self,
        window_ids: &[String],
    ) -> Result<String> {
        let panes = window_ids
            .iter()
            .map(|window_id| {
                self.session
                    .windows()
                    .iter()
                    .find(|window| window.id.as_str() == window_id)
                    .ok_or_else(|| {
                        MezError::new(crate::error::MezErrorKind::NotFound, "window not found")
                    })
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flat_map(|window| {
                window
                    .panes()
                    .iter()
                    .map(|pane| self.runtime_control_pane_state_json(window, pane))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        Ok(format!("[{}]", panes.join(",")))
    }

    /// Runs the runtime window state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_window_state_json(&self, window: &crate::layout::Window) -> String {
        let created_at = self
            .window_created_at_unix_seconds
            .get(window.id.as_str())
            .copied()
            .unwrap_or(self.created_at_unix_seconds);
        let panes = window
            .panes()
            .iter()
            .map(|pane| self.runtime_control_pane_state_json(window, pane))
            .collect::<Vec<_>>();
        format!(
            r#"{{"id":"{}","version":1,"session_id":"{}","window_id":"{}","index":{},"name":"{}","active":{},"created_at":{},"size":{{"columns":{},"rows":{}}},"active_pane_id":{},"panes":[{}],"pane_count":{},"layout":{}}}"#,
            json_escape(window.id.as_str()),
            json_escape(self.session.id.as_str()),
            json_escape(window.id.as_str()),
            window.index,
            json_escape(&window.name),
            self.session
                .active_window()
                .is_some_and(|active| active.id == window.id),
            runtime_timestamp_json(created_at),
            window.size.columns,
            window.size.rows,
            runtime_optional_string(Some(window.active_pane().id.as_str())),
            panes.join(","),
            window.panes().len(),
            layout_state_json(window)
        )
    }

    /// Runs the runtime control pane state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_control_pane_state_json(
        &self,
        window: &crate::layout::Window,
        pane: &crate::layout::Pane,
    ) -> String {
        let primary_pid = self.primary_pid_for_live_pane_process(pane.id.as_str());
        let exit_status = self
            .pane_exit_records
            .get(pane.id.as_str())
            .map(|record| record.exit_status.to_json())
            .unwrap_or_else(|| "null".to_string());
        let process_state = if self.pane_closing.contains(pane.id.as_str()) {
            "closing"
        } else if primary_pid.is_some() {
            "running"
        } else if pane.live {
            "starting"
        } else {
            "exited"
        };
        let alternate_screen_active = self
            .pane_screens
            .get(pane.id.as_str())
            .is_some_and(|screen| screen.alternate_screen_active());
        let current_working_directory = self
            .pane_current_working_directory(pane.id.as_str())
            .map(|path| path.to_string_lossy().to_string());
        let agent_id = self
            .agent_shell_store
            .get(pane.id.as_str())
            .map(|_| format!("agent-{}", pane.id));
        format!(
            r#"{{"id":"{}","version":1,"session_id":"{}","window_id":"{}","pane_id":"{}","index":{},"title":"{}","active":{},"size":{{"columns":{},"rows":{}}},"columns":{},"rows":{},"primary_pid":{},"process_state":"{}","exit_status":{},"current_working_directory":{},"terminal_profile":"{}","history_limit":{},"alternate_screen_active":{},"readiness_state":"{}","agent_id":{},"live":{}}}"#,
            json_escape(pane.id.as_str()),
            json_escape(self.session.id.as_str()),
            json_escape(window.id.as_str()),
            json_escape(pane.id.as_str()),
            pane.index,
            json_escape(&pane.title),
            pane.active,
            pane.size.columns,
            pane.size.rows,
            pane.size.columns,
            pane.size.rows,
            primary_pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "null".to_string()),
            process_state,
            exit_status,
            runtime_optional_string(current_working_directory.as_deref()),
            json_escape(self.terminal_term()),
            self.terminal_history_limit(),
            alternate_screen_active,
            runtime_pane_readiness_state_name(self.pane_readiness_state(pane.id.as_str())),
            runtime_optional_string(agent_id.as_deref()),
            pane.live
        )
    }

    /// Runs the runtime started pane result json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn runtime_started_pane_result_json(
        &self,
        started: &PaneProcessStart,
        include_window: bool,
    ) -> Result<String> {
        let (window, pane) = runtime_pane_by_id(&self.session, &started.pane_id)?;
        let pane_json = self.runtime_control_pane_state_json(window, pane);
        let layout_json = layout_state_json(window);
        if include_window {
            let window_json = self.runtime_window_state_json(window);
            Ok(format!(
                r#"{{"window":{window_json},"pane":{pane_json},"layout":{layout_json}}}"#
            ))
        } else {
            Ok(format!(r#"{{"pane":{pane_json},"layout":{layout_json}}}"#))
        }
    }

    /// Runs the runtime pane resize result json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn runtime_pane_resize_result_json(&self, update: &PaneResizeUpdate) -> Result<String> {
        let (window, pane) = runtime_pane_by_id(&self.session, &update.pane_id)?;
        Ok(format!(
            r#"{{"pane":{},"layout":{}}}"#,
            self.runtime_control_pane_state_json(window, pane),
            layout_state_json(window)
        ))
    }

    /// Runs the runtime active layout state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn runtime_active_layout_state_json(&self) -> Result<String> {
        let window = self
            .session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        Ok(layout_state_json(window))
    }

    /// Builds the live pane-to-model-profile view used by runtime `agent/list`.
    ///
    /// The latest turn model profile is authoritative when a turn exists for a
    /// pane. Otherwise the currently selected runtime override/default profile
    /// is used when it can be resolved, with the generic serializer's `default`
    /// fallback preserved only for non-runtime or unconfigured contexts.
    fn runtime_agent_model_profiles_by_pane(&self) -> std::collections::BTreeMap<String, String> {
        let mut profiles = std::collections::BTreeMap::new();
        for window in self.session.windows() {
            for pane in window.panes() {
                let pane_id = pane.id.to_string();
                let latest_turn_profile = self
                    .agent_turn_ledger
                    .turns()
                    .iter()
                    .rev()
                    .find(|turn| turn.pane_id == pane_id)
                    .map(|turn| turn.model_profile.clone());
                let profile = latest_turn_profile.or_else(|| {
                    let agent_id = format!("agent-{pane_id}");
                    self.active_model_profile_for_pane(&pane_id, &agent_id, None)
                        .ok()
                        .map(|(profile_name, _profile)| profile_name)
                });
                if let Some(profile) = profile {
                    profiles.insert(pane_id, profile);
                }
            }
        }
        profiles
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

    /// Runs the apply runtime initialize side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_runtime_initialize_side_effects(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        primary_before: Option<&crate::ids::ClientId>,
        observer_count_before: usize,
    ) -> Result<()> {
        if runtime_initialize_requested_observer(request) {
            self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
            return self.apply_runtime_observer_initialize_side_effects(observer_count_before);
        }
        if !runtime_initialize_requested_primary(request) {
            self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
            return Ok(());
        }
        let Some(primary_after) = self.session.primary_client_id().cloned() else {
            self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
            return Ok(());
        };
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        if let Some(size) = runtime_initialize_terminal_size(request) {
            self.session
                .resize_authoritative_terminal(&primary_after, size)?;
            self.sync_tracked_pty_sizes()?;
        }
        if primary_before == Some(&primary_after) {
            return Ok(());
        }
        self.last_attach_at_unix_seconds = Some(current_unix_seconds());
        self.append_lifecycle_event(
            EventKind::ClientAttached,
            format!(
                r#"{{"client_id":"{}","role":"primary","columns":{},"rows":{}}}"#,
                json_escape(primary_after.as_str()),
                self.session.authoritative_size.columns,
                self.session.authoritative_size.rows
            ),
        )
    }

    /// Publishes runtime-visible side effects for a successful observer request.
    fn apply_runtime_observer_initialize_side_effects(
        &mut self,
        observer_count_before: usize,
    ) -> Result<()> {
        let Some(observer) = self.session.observers().get(observer_count_before).cloned() else {
            return Ok(());
        };
        let observer_id = observer.id.to_string();
        let payload = format!(
            r#"{{"observer_id":"{}","client_id":"{}","state":"pending","descriptor":"{}","interactive":{},"terminal":"{}"}}"#,
            json_escape(&observer_id),
            json_escape(observer.client_id.as_str()),
            json_escape(&observer.descriptor_name),
            observer.descriptor_interactive,
            json_escape(
                &observer
                    .descriptor_terminal
                    .as_ref()
                    .map(|terminal| format!(
                        "{}x{} {}",
                        terminal.columns, terminal.rows, terminal.term
                    ))
                    .unwrap_or_else(|| "none".to_string())
            )
        );
        self.append_observer_requested_lifecycle_event(observer_id.as_str(), payload)?;
        let active_pane_id = self.active_pane_id()?;
        self.append_agent_status_text_to_terminal_buffer(
            &active_pane_id,
            &format!(
                "observer request {} from {} is pending",
                observer.id, observer.descriptor_name
            ),
        )
    }

    /// Appends an observer-request event with pending-observer visibility.
    fn append_observer_requested_lifecycle_event(
        &mut self,
        observer_id: &str,
        payload: String,
    ) -> Result<()> {
        if let Some(event_log) = &mut self.event_log {
            event_log.append(
                EventKind::ObserverRequested,
                Some(self.session.id.to_string()),
                EventVisibility::PendingObserverRequest(observer_id.to_string()),
                payload.clone(),
            )?;
        }
        if let Some(hook_event) =
            runtime_hook_event_for_lifecycle(EventKind::ObserverRequested, &payload)
        {
            self.run_configured_completed_hooks(hook_event, &payload)?;
        }
        Ok(())
    }

    /// Runs the append runtime snapshot audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn append_runtime_snapshot_audit(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &crate::ids::ClientId,
        outcome: &str,
    ) -> Result<()> {
        let Some(operation) = request.method.strip_prefix("snapshot/") else {
            return Ok(());
        };
        if !matches!(operation, "create" | "resume" | "delete") {
            return Ok(());
        }
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let params = request.params.as_deref().unwrap_or("{}");
        let snapshot_id = match operation {
            "create" => runtime_json_string_field(params, "idempotency_key")
                .map(|key| snapshot_id_for_idempotency_key(&self.session, &key))
                .unwrap_or_else(|| "unknown".to_string()),
            _ => runtime_json_string_field(params, "snapshot_id")
                .unwrap_or_else(|| "unknown".to_string()),
        };
        let mut record = AuditRecord::snapshot_operation(
            self.session.id.to_string(),
            AuditActor {
                kind: "client".to_string(),
                id: caller_client_id.to_string(),
            },
            snapshot_id,
            operation,
            outcome,
        );
        if let Some(name) = runtime_json_string_field(params, "name") {
            record = record.with_metadata("name", name);
        }
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Runs the dispatch runtime snapshot resume for connection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn dispatch_runtime_snapshot_resume_for_connection(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        snapshots: &SnapshotRepository,
        connection: &mut ControlConnectionState,
        caller_client_id: &crate::ids::ClientId,
    ) -> Result<String> {
        let params = request
            .params
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("snapshot/resume requires a params object"))?;
        let _idempotency_key = runtime_json_string_field(params, "idempotency_key")
            .ok_or_else(|| MezError::invalid_args("snapshot/resume requires idempotency_key"))?;
        let snapshot_id = runtime_json_string_field(params, "snapshot_id")
            .ok_or_else(|| MezError::invalid_args("snapshot/resume requires snapshot_id"))?;
        let payload = snapshots.inspect_payload(&snapshot_id)?;
        self.require_snapshot_resume_hooks_allow(&payload)?;
        let _ = connection;
        let resume_plan = payload.resume_plan();
        self.apply_runtime_snapshot_resume_for_connection(
            &snapshot_id,
            payload,
            resume_plan,
            caller_client_id,
        )
    }

    /// Runs the dispatch runtime snapshot resume for connection async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn dispatch_runtime_snapshot_resume_for_connection_async(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        snapshots: &SnapshotRepository,
        connection: &mut ControlConnectionState,
        caller_client_id: &crate::ids::ClientId,
    ) -> Result<String> {
        let _ = connection;
        let params = request
            .params
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("snapshot/resume requires a params object"))?;
        let _idempotency_key = runtime_json_string_field(params, "idempotency_key")
            .ok_or_else(|| MezError::invalid_args("snapshot/resume requires idempotency_key"))?;
        let snapshot_id = runtime_json_string_field(params, "snapshot_id")
            .ok_or_else(|| MezError::invalid_args("snapshot/resume requires snapshot_id"))?;
        let payload = snapshots.inspect_payload_async(&snapshot_id).await?;
        self.require_snapshot_resume_hooks_allow(&payload)?;
        let resume_plan = payload.resume_plan();
        self.apply_runtime_snapshot_resume_for_connection(
            &snapshot_id,
            payload,
            resume_plan,
            caller_client_id,
        )
    }

    /// Runs the apply runtime snapshot resume for connection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_runtime_snapshot_resume_for_connection(
        &mut self,
        snapshot_id: &str,
        payload: crate::snapshot::SessionSnapshotPayload,
        resume_plan: crate::snapshot::LayoutLoadPlan,
        caller_client_id: &crate::ids::ClientId,
    ) -> Result<String> {
        let terminated_panes =
            self.pane_processes.tracked_pane_ids().len() + self.async_owned_pane_processes.len();
        let replaced_pane_ids = self
            .session
            .windows()
            .iter()
            .flat_map(|window| window.panes().iter().map(|pane| pane.id.to_string()))
            .collect::<Vec<_>>();
        self.active_copy_modes.clear();
        self.pane_screens.clear();
        self.pane_transaction_osc_screens.clear();
        self.pane_transaction_osc_pending.clear();
        self.pane_exit_records.clear();
        self.pane_closing.clear();
        self.pane_transcript_refs.clear();
        self.agent_shell_store = AgentShellStore::default();
        self.agent_turn_ledger = AgentTurnLedger::new(false);
        self.agent_turn_contexts.clear();
        self.agent_turn_executions.clear();
        self.agent_turn_pending_steering.clear();
        self.agent_turn_failure_feedback_attempts.clear();
        self.agent_turn_shell_dispatch_history.clear();
        self.agent_turn_network_action_history.clear();
        self.agent_copy_outputs.clear();
        self.agent_modified_files.clear();
        self.agent_prompt_inputs.clear();
        self.agent_turn_model_profiles.clear();
        self.pending_agent_provider_tasks.clear();
        self.claimed_agent_provider_tasks.clear();
        self.agent_scheduler = AgentScheduler::with_default_limit();
        self.subagent_task_routes.clear();
        self.joined_subagent_dependencies.clear();
        self.subagent_lineage.clear();
        self.subagent_window_ids.clear();
        self.pending_terminal_subagent_pane_closes.clear();
        self.subagent_scope_declarations.clear();
        self.subagent_scopes = ScopeRegistry::default();
        self.blocked_agent_approval_refs.clear();
        self.running_shell_transactions.clear();
        self.shell_transaction_require_start_markers.clear();
        self.shell_transaction_started_markers.clear();
        self.pane_readiness_states.clear();
        self.pane_readiness_overrides = PaneReadinessOverrideStore::default();
        self.blocked_approvals = Default::default();
        self.session_approvals = Default::default();

        self.session
            .replace_layout_from_snapshot_payload(&payload)?;
        self.session.state = crate::session::SessionState::Running;
        let restored_at = current_unix_seconds();
        self.window_created_at_unix_seconds = self
            .session
            .windows()
            .iter()
            .map(|window| {
                (
                    window.id.to_string(),
                    window.created_at_unix_seconds.unwrap_or(restored_at),
                )
            })
            .collect();
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        let restarted_panes = self.restart_restored_pane_processes(None)?.len();
        let replaced_pane_id_refs = replaced_pane_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        self.stop_active_pane_pipes_for(replaced_pane_id_refs.as_slice());
        self.append_lifecycle_event(
            EventKind::SnapshotChanged,
            format!(
                r#"{{"method":"snapshot/resume","snapshot_id":"{}","resumed":true,"terminated_panes":{},"restarted_panes":{},"seeded_terminal_screens":0,"interrupted_agent_turns":0}}"#,
                json_escape(snapshot_id),
                terminated_panes,
                restarted_panes
            ),
        )?;
        Ok(format!(
            r#"{{"session":{},"resumed":true,"resume_plan":{},"limitations":{},"terminated_panes":{},"restarted_panes":{},"seeded_terminal_screens":0,"interrupted_agent_turns":0,"primary_client_id":"{}"}}"#,
            self.runtime_session_state_json(),
            runtime_snapshot_resume_plan_json(&resume_plan),
            runtime_string_array_json(&resume_plan.limitations),
            terminated_panes,
            restarted_panes,
            json_escape(caller_client_id.as_str())
        ))
    }

    /// Runs the dispatch runtime approval request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_approval_request(
        &mut self,
        body: &str,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &crate::ids::ClientId,
    ) -> String {
        let cache_key = if request.method == "approval/decide" {
            let params = request.params.as_deref().unwrap_or("{}");
            let Some(idempotency_key) = runtime_json_string_field(params, "idempotency_key") else {
                return runtime_json_rpc_error(
                    &request.id,
                    crate::error::MezErrorKind::InvalidArgs,
                    "mutating control request requires idempotency_key",
                );
            };
            let cache_key = format!("{caller_client_id}:{idempotency_key}");
            match self.control_idempotency.cached_response(
                &cache_key,
                request.method.as_str(),
                &request.params,
            ) {
                Ok(Some(response)) => return response,
                Ok(None) => Some(cache_key),
                Err(error) => {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            }
        } else {
            None
        };

        if request.method == "approval/decide" {
            let params = request.params.as_deref().unwrap_or("{}");
            let approval_id = runtime_json_string_field(params, "approval_id")
                .unwrap_or_else(|| "unknown".to_string());
            let decision = runtime_json_string_field(params, "decision")
                .unwrap_or_else(|| "unknown".to_string());
            if let Some(block) = match self.run_configured_pre_action_hooks(
                HookEvent::PermissionDecision,
                &runtime_permission_decision_hook_payload(&approval_id, &decision),
            ) {
                Ok(block) => block,
                Err(error) => {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            } {
                return runtime_json_rpc_error(
                    &request.id,
                    crate::error::MezErrorKind::Forbidden,
                    &format!(
                        "permission decision blocked by hook `{}`: {}",
                        block.hook_id, block.message
                    ),
                );
            }
        }

        let response = if let Some(audit_log) = self.audit_log.as_mut() {
            dispatch_control_request_with_approvals_and_audit(
                body,
                &mut self.session,
                caller_client_id,
                &mut self.blocked_approvals,
                audit_log,
            )
        } else {
            dispatch_control_request_with_approvals(
                body,
                &mut self.session,
                caller_client_id,
                &mut self.blocked_approvals,
            )
        };
        if response.contains(r#""result""#) && request.method == "approval/decide" {
            let params = request.params.as_deref().unwrap_or("{}");
            let approval_id = runtime_json_string_field(params, "approval_id")
                .unwrap_or_else(|| "unknown".to_string());
            let decision = runtime_json_string_field(params, "decision")
                .unwrap_or_else(|| "unknown".to_string());
            let decision_kind = runtime_approval_decision_name_to_kind(&decision);
            let requested_scope = match approval_decide_scope_persistence(params) {
                Ok(scope) => scope,
                Err(error) => {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            };
            let decided_approval = self.blocked_approvals.get(&approval_id).cloned();
            if let Some(rule_decision) = (match decision_kind {
                Some(ApprovalDecision::Approve) => Some(RuleDecision::Allow),
                Some(ApprovalDecision::Disapprove) => Some(RuleDecision::Forbid),
                Some(ApprovalDecision::Redirect) | None => None,
            }) && let Some(approval) = decided_approval.as_ref()
                && approval.action_kind == "shell_command"
                && matches!(
                    requested_scope,
                    Some(
                        ApprovalDecisionScopePersistence::Session
                            | ApprovalDecisionScopePersistence::Project
                            | ApprovalDecisionScopePersistence::Global
                    )
                )
            {
                match self.persist_shell_approval_rule(
                    approval,
                    requested_scope
                        .expect("requested_scope is Some for persisted approval decision"),
                    rule_decision,
                ) {
                    Ok(_) => {}
                    Err(error) => {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                }
            }
            let mut resumed_actions = 0usize;
            if matches!(decision_kind, Some(ApprovalDecision::Approve))
                && let Some(approval) = decided_approval.as_ref()
            {
                match self.resume_approved_blocked_agent_action(
                    &approval_id,
                    approval,
                    caller_client_id,
                ) {
                    Ok(Some(count)) => resumed_actions = count,
                    Ok(None) => {}
                    Err(error) => {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                }
            }
            if matches!(
                decision_kind,
                Some(ApprovalDecision::Disapprove | ApprovalDecision::Redirect)
            ) && let Some(approval) = decided_approval.as_ref()
            {
                match self.settle_decided_blocked_agent_action(&approval_id, approval) {
                    Ok(Some(count)) => resumed_actions = count,
                    Ok(None) => {}
                    Err(error) => {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                }
                if let Err(error) = self
                    .session
                    .select_pane_global(caller_client_id, &approval.pane_id)
                {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
                if let Err(error) = self.enter_agent_mode_for_pane(&approval.pane_id) {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            }
            if let Err(error) = self.append_primary_lifecycle_event(
                EventKind::ApprovalChanged,
                format!(
                    r#"{{"approval_id":"{}","decision":"{}","state":"decided","agent_actions_resumed":{}}}"#,
                    json_escape(&approval_id),
                    json_escape(&decision),
                    resumed_actions
                ),
            ) {
                return runtime_json_rpc_error(&request.id, error.kind(), error.message());
            }
        }
        if let Some(cache_key) = cache_key {
            self.control_idempotency.remember_response(
                cache_key,
                request.method.clone(),
                request.params.clone(),
                response.clone(),
            );
        }
        response
    }

    /// Runs the persist shell approval rule operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn persist_shell_approval_rule(
        &mut self,
        approval: &BlockedApprovalRequest,
        persistence: ApprovalDecisionScopePersistence,
        decision: RuleDecision,
    ) -> Result<()> {
        let normalized = normalize_exact_command_text(&approval.action_summary, false);
        let scope = match persistence {
            ApprovalDecisionScopePersistence::Once => return Ok(()),
            ApprovalDecisionScopePersistence::Session => CommandRuleScope::Session,
            ApprovalDecisionScopePersistence::Project => CommandRuleScope::Project,
            ApprovalDecisionScopePersistence::Global => CommandRuleScope::User,
        };
        let rule = CommandRule::new_exact_sha256(
            &normalized,
            DEFAULT_COMMAND_SHELL_CLASSIFICATION,
            decision,
        )?
        .with_scope(scope)
        .with_justification(format!(
            "approval {} for pane {}",
            approval.id, approval.pane_id
        ));
        if matches!(persistence, ApprovalDecisionScopePersistence::Project) {
            self.persist_project_shell_approval_rule(approval, &rule)?;
        } else {
            self.permission_policy.add_rule(rule);
        }
        Ok(())
    }

    /// Runs the persist project shell approval rule operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn persist_project_shell_approval_rule(
        &mut self,
        approval: &BlockedApprovalRequest,
        rule: &CommandRule,
    ) -> Result<()> {
        let project_root = self.project_root_for_approval(approval);
        if let Some(record) = self
            .project_trust_store
            .as_ref()
            .and_then(|store| store.get(&project_root))
        {
            match record.state {
                TrustDecision::Trusted => {}
                TrustDecision::Pending => {
                    return Err(MezError::conflict(
                        "project approval persistence is blocked until project trust is decided",
                    ));
                }
                TrustDecision::Rejected | TrustDecision::Revoked => {
                    return Err(MezError::forbidden(
                        "project approval persistence requires a trusted project root",
                    ));
                }
            }
        }
        let config_path = project_root.join(".mezzanine/config.toml");
        let parent = config_path.parent().ok_or_else(|| {
            MezError::invalid_args(format!(
                "project config target {} has no parent directory",
                config_path.display()
            ))
        })?;
        let text = self.project_config_text_for_update(&config_path)?;
        let updated = append_project_command_rule_text(&text, rule)?;
        if self.defer_project_config_writes {
            self.deferred_project_config_writes
                .push(DeferredProjectConfigWrite {
                    path: config_path.clone(),
                    text: updated.clone(),
                });
        } else {
            fs::create_dir_all(parent)?;
            fs::write(&config_path, updated.clone())?;
        }
        self.upsert_project_config_layer(config_path, updated, project_root)
    }

    /// Runs the project config text for update operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn project_config_text_for_update(&self, config_path: &Path) -> Result<String> {
        if let Some(layer) = self.config_layers.iter().rev().find(|layer| {
            layer
                .path
                .as_ref()
                .is_some_and(|layer_path| paths_equivalent(layer_path, config_path))
        }) {
            return Ok(layer.text.clone());
        }
        match fs::read_to_string(config_path) {
            Ok(text) => Ok(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(crate::config::DEFAULT_PROJECT_CONFIG_TOML.to_string())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Runs the project root for approval operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn project_root_for_approval(&self, approval: &BlockedApprovalRequest) -> PathBuf {
        self.pane_current_working_directory(&approval.pane_id)
            .map(|path| discover_project_root(&path))
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|path| discover_project_root(&path))
            })
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Runs the upsert project config layer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn upsert_project_config_layer(
        &mut self,
        path: PathBuf,
        text: String,
        project_root: PathBuf,
    ) -> Result<()> {
        let trusted = self
            .project_trust_store
            .as_ref()
            .and_then(|store| store.get(&project_root))
            .is_none_or(|record| record.state == TrustDecision::Trusted);
        if let Some(layer) = self.config_layers.iter_mut().find(|layer| {
            layer
                .path
                .as_ref()
                .is_some_and(|layer_path| paths_equivalent(layer_path, &path))
        }) {
            layer.format = ConfigFormat::Toml;
            layer.scope = ConfigScope::ProjectOverlay;
            layer.trusted = trusted;
            layer.text = text;
        } else {
            self.config_layers.push(ConfigLayer {
                name: "project".to_string(),
                path: Some(path),
                format: ConfigFormat::Toml,
                scope: ConfigScope::ProjectOverlay,
                trusted,
                text,
            });
        }
        self.apply_runtime_config_layers()?;
        Ok(())
    }

    /// Runs the dispatch runtime mutating request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_mutating_request(
        &mut self,
        request: crate::control::JsonRpcRequest,
        caller_client_id: &crate::ids::ClientId,
    ) -> String {
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
        let cache_key = format!("{caller_client_id}:{idempotency_key}");
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
            caller_client_id,
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

    /// Runs the dispatch runtime mutating result operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_mutating_result(
        &mut self,
        method: &str,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        match method {
            "window/create" => self.dispatch_runtime_window_create(primary_client_id, params),
            "pane/create" => self.dispatch_runtime_pane_create(primary_client_id, params),
            "pane/resize" => self.dispatch_runtime_pane_resize(primary_client_id, params),
            "pane/swap" => self.dispatch_runtime_pane_swap(primary_client_id, params),
            "pane/break" => self.dispatch_runtime_pane_break(primary_client_id, params),
            "pane/join" | "pane/move" => self.dispatch_runtime_pane_join(primary_client_id, params),
            "pane/close" => self.dispatch_runtime_pane_close(primary_client_id, params),
            "window/close" => self.dispatch_runtime_window_close(primary_client_id, params),
            "session/kill" => self.dispatch_runtime_session_kill(primary_client_id, params),
            "observer/approve" => self.dispatch_runtime_observer_approve(primary_client_id, params),
            "observer/reject" => self.dispatch_runtime_observer_reject(primary_client_id, params),
            "observer/revoke" => self.dispatch_runtime_observer_revoke(primary_client_id, params),
            "terminal/step" => self.dispatch_runtime_terminal_step(primary_client_id, params),
            "terminal/command" => self.dispatch_runtime_terminal_command(primary_client_id, params),
            "agent/shell/command" => {
                self.dispatch_runtime_agent_shell_command(primary_client_id, params)
            }
            "agent/spawn" => self.dispatch_runtime_agent_spawn(primary_client_id, params),
            "project/trust/decide" | "project/trust/revoke" => {
                self.dispatch_runtime_project_trust_mutation(method, primary_client_id, params)
            }
            "mcp/retry" => self.dispatch_runtime_mcp_retry(params),
            _ => Err(MezError::invalid_state(
                "runtime control method was filtered incorrectly",
            )),
        }
    }

    /// Runs the dispatch runtime mcp retry operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_mcp_retry(&mut self, params: &str) -> Result<String> {
        let server_id = runtime_json_string_field(params, "server_id")
            .or_else(|| runtime_json_string_field(params, "id"))
            .ok_or_else(|| MezError::invalid_args("mcp/retry requires server_id"))?;
        let report = self.retry_runtime_mcp_server(&server_id)?;
        self.append_lifecycle_event(
            EventKind::McpServerChanged,
            runtime_mcp_retry_event_payload("control:mcp/retry", &report),
        )?;
        Ok(runtime_mcp_retry_result_json(&report))
    }

    /// Runs the dispatch runtime window create operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_window_create(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let name = runtime_json_string_field(params, "name").unwrap_or_else(|| "shell".to_string());
        let select = runtime_json_bool_field(params, "select").unwrap_or(true);
        let command = runtime_json_creation_command(params)?;
        let start_directory = runtime_json_start_directory(params)?;
        let started = self.create_window_with_pane_process_with_options(
            primary_client_id,
            name,
            select,
            command.as_deref(),
            start_directory.as_deref(),
            None,
        )?;
        self.runtime_started_pane_result_json(&started, true)
    }

    /// Runs the dispatch runtime pane create operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_create(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        if let Some(target) = pane_target_checked_resolved(&self.session, params)? {
            self.session.select_pane(primary_client_id, &target)?;
        }
        let split =
            runtime_json_string_field(params, "split").unwrap_or_else(|| "vertical".to_string());
        let select = runtime_json_bool_field(params, "select").unwrap_or(true);
        let command = runtime_json_creation_command(params)?;
        let start_directory = runtime_json_start_directory(params)?;
        let requested_size = runtime_json_optional_size_field(params, "size")?;
        if split == "window" {
            let started = self.create_window_with_pane_process_with_options(
                primary_client_id,
                "shell",
                select,
                command.as_deref(),
                start_directory.as_deref(),
                requested_size,
            )?;
            return self.runtime_started_pane_result_json(&started, false);
        }
        let direction = runtime_split_direction(&split)?;
        let started = self.split_pane_with_process_with_options(
            primary_client_id,
            direction,
            select,
            command.as_deref(),
            start_directory.as_deref(),
            requested_size,
        )?;
        self.runtime_started_pane_result_json(&started, false)
    }

    /// Runs the dispatch runtime pane resize operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_resize(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let target = pane_target_checked_resolved(&self.session, params)?;
        let spec = runtime_json_size(params)?;
        let update = self.resize_pane_pty_with_spec(primary_client_id, target.as_deref(), spec)?;
        self.runtime_pane_resize_result_json(&update)
    }

    /// Runs the dispatch runtime pane swap operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_swap(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let source = source_pane_target_checked_resolved(&self.session, params)?;
        let destination = destination_target_checked_resolved(&self.session, params)?
            .ok_or_else(|| MezError::invalid_args("pane/swap requires destination"))?;
        let updates =
            self.swap_panes_and_sync_pty_sizes(primary_client_id, source.as_deref(), &destination)?;
        let layout = self.runtime_active_layout_state_json()?;
        Ok(format!(
            r#"{{"layout":{layout},"synced_panes":{}}}"#,
            updates.len()
        ))
    }

    /// Runs the dispatch runtime pane break operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_break(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let target = pane_target_checked_resolved(&self.session, params)?;
        let name = runtime_json_string_field(params, "name");
        let (window_id, updates) =
            self.break_pane_and_sync_pty_sizes(primary_client_id, target.as_deref(), name, true)?;
        let window = self
            .session
            .windows()
            .iter()
            .find(|window| window.id.as_str() == window_id.as_str())
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "created window not found",
                )
            })?;
        let pane = window.active_pane();
        Ok(format!(
            r#"{{"window":{},"pane":{},"synced_panes":{}}}"#,
            self.runtime_window_state_json(window),
            self.runtime_control_pane_state_json(window, pane),
            updates.len()
        ))
    }

    /// Runs the dispatch runtime pane join operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_join(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let source = source_pane_target_checked_resolved(&self.session, params)?;
        let destination = destination_target_checked_resolved(&self.session, params)?
            .ok_or_else(|| MezError::invalid_args("pane/join requires destination"))?;
        let direction = runtime_json_string_field(params, "position")
            .as_deref()
            .map(runtime_split_direction)
            .transpose()?
            .unwrap_or(SplitDirection::Vertical);
        let (pane_id, updates) = self.join_pane_and_sync_pty_sizes(
            primary_client_id,
            source.as_deref(),
            &destination,
            direction,
            true,
        )?;
        let (window, pane) = runtime_pane_by_id(&self.session, pane_id.as_str())?;
        Ok(format!(
            r#"{{"pane":{},"layout":{},"synced_panes":{}}}"#,
            self.runtime_control_pane_state_json(window, pane),
            layout_state_json(window),
            updates.len()
        ))
    }

    /// Runs the dispatch runtime pane close operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_close(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let force = runtime_json_bool_field(params, "force").unwrap_or(false);
        let target = pane_target_checked_resolved(&self.session, params)?;
        let descriptor = match target.as_deref() {
            Some(target) => self.find_pane_descriptor(target).ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
            })?,
            None => self.active_window_pane_descriptor(None)?,
        };
        let (_, pane) = runtime_pane_by_id(&self.session, descriptor.pane_id.as_str())?;
        if force || !pane.live {
            self.fail_agent_turns_for_pane_shutdown(
                &[descriptor.pane_id.to_string()],
                "pane closed",
            )?;
        }
        let removed = self
            .session
            .kill_pane(primary_client_id, target.as_deref(), force)?;
        let terminated = if let Some(pane) = removed {
            let pane_id = pane.id.to_string();
            let _ = self.stop_active_pane_pipe(pane.id.as_str());
            let terminated = usize::from(self.terminate_runtime_pane_process(&pane_id, force)?);
            self.cleanup_removed_pane_runtime_state(&pane_id);
            terminated
        } else {
            0
        };
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        self.append_pane_close_event(
            descriptor.pane_id.as_str(),
            descriptor.window_id.as_str(),
            terminated,
            self.session.windows().is_empty(),
        )?;
        Ok(format!(
            r#"{{"closed":true,"terminated_panes":{},"session_empty":{}}}"#,
            terminated,
            self.session.windows().is_empty()
        ))
    }

    /// Runs the dispatch runtime window close operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_window_close(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let force = runtime_json_bool_field(params, "force").unwrap_or(false);
        let target = window_target_checked_resolved(&self.session, params)?;
        let window = if let Some(target) = target.as_deref() {
            self.session
                .windows()
                .iter()
                .find(|window| window.id.as_str() == target)
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "window not found")
                })?
        } else {
            self.session
                .active_window()
                .ok_or_else(|| MezError::invalid_state("session has no active window"))?
        };
        let panes_have_live_process = window.panes().iter().any(|pane| pane.live);
        let pane_ids = window
            .panes()
            .iter()
            .map(|pane| pane.id.to_string())
            .collect::<Vec<_>>();
        if force || !panes_have_live_process {
            self.fail_agent_turns_for_pane_shutdown(&pane_ids, "window closed")?;
        }
        let removed = self
            .session
            .kill_window(primary_client_id, target.as_deref(), force)?;
        let pane_ids = removed
            .panes()
            .iter()
            .map(|pane| pane.id.to_string())
            .collect::<Vec<_>>();
        let pane_id_refs = pane_ids.iter().map(String::as_str).collect::<Vec<_>>();
        self.stop_active_pane_pipes_for(pane_id_refs.as_slice());
        let terminated = self.terminate_runtime_pane_processes(pane_id_refs, force)?;
        for pane_id in &pane_ids {
            self.cleanup_removed_pane_runtime_state(pane_id);
        }
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        self.append_window_close_event(
            removed.id.as_str(),
            terminated,
            self.session.windows().is_empty(),
        )?;
        Ok(format!(
            r#"{{"closed":true,"terminated_panes":{},"session_empty":{}}}"#,
            terminated,
            self.session.windows().is_empty()
        ))
    }

    /// Runs the dispatch runtime session kill operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_session_kill(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let force = runtime_json_bool_field(params, "force").unwrap_or(false);
        self.kill_session(primary_client_id, force)?;
        Ok(format!(
            r#"{{"killed":true,"session_id":"{}"}}"#,
            json_escape(self.session.id.as_str())
        ))
    }

    /// Runs the dispatch runtime observer approve operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_observer_approve(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let observer_id =
            runtime_json_string_field(params, "observer_request_id").ok_or_else(|| {
                MezError::invalid_args("observer/approve requires observer_request_id")
            })?;
        self.approve_observer_with_runtime_cutoff(primary_client_id, &observer_id)?;
        self.append_lifecycle_event(
            EventKind::ObserverDecided,
            format!(
                r#"{{"observer_request_id":"{}","decision":"approved"}}"#,
                json_escape(&observer_id)
            ),
        )?;
        runtime_append_observer_decision_audit(
            self,
            primary_client_id,
            "observer_request",
            &observer_id,
            "approved",
        )?;
        Ok(format!(
            r#"{{"observer":{}}}"#,
            observer_json(&self.session, &observer_id)?
        ))
    }

    /// Runs the dispatch runtime observer reject operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_observer_reject(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let observer_id =
            runtime_json_string_field(params, "observer_request_id").ok_or_else(|| {
                MezError::invalid_args("observer/reject requires observer_request_id")
            })?;
        let reason = runtime_json_string_field(params, "reason");
        self.session
            .reject_observer_target_with_reason(primary_client_id, &observer_id, reason)?;
        self.append_lifecycle_event(
            EventKind::ObserverDecided,
            format!(
                r#"{{"observer_request_id":"{}","decision":"rejected"}}"#,
                json_escape(&observer_id)
            ),
        )?;
        runtime_append_observer_decision_audit(
            self,
            primary_client_id,
            "observer_request",
            &observer_id,
            "rejected",
        )?;
        Ok(format!(
            r#"{{"observer":{}}}"#,
            observer_json(&self.session, &observer_id)?
        ))
    }

    /// Runs the dispatch runtime observer revoke operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_observer_revoke(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let client_id = runtime_json_string_field(params, "client_id")
            .ok_or_else(|| MezError::invalid_args("observer/revoke requires client_id"))?;
        let reason = runtime_json_string_field(params, "reason");
        self.session
            .revoke_observer_client_with_reason(primary_client_id, &client_id, reason)?;
        self.append_lifecycle_event(
            EventKind::ObserverDecided,
            format!(
                r#"{{"client_id":"{}","decision":"revoked"}}"#,
                json_escape(&client_id)
            ),
        )?;
        runtime_append_observer_decision_audit(
            self,
            primary_client_id,
            "client",
            &client_id,
            "revoked",
        )?;
        Ok(r#"{"revoked":true}"#.to_string())
    }

    /// Runs the dispatch runtime terminal step operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_terminal_step(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let input = runtime_json_input_bytes(params)?;
        let render = runtime_json_bool_field(params, "render").unwrap_or(true);
        let client_size =
            runtime_json_optional_client_size(params)?.unwrap_or(self.session.authoritative_size);
        if client_size != self.session.authoritative_size {
            self.resize_attached_primary_terminal(primary_client_id, client_size)?;
        }
        let terminal_config =
            self.terminal_client_loop_config(TerminalClientLoopConfig::default())?;
        let prompt_active = if input.is_empty() {
            false
        } else {
            self.render_client_view_with_resolved_config(
                ClientViewRole::Primary,
                client_size,
                &terminal_config,
            )?
            .is_some_and(|view| view.primary_prompt_active)
        };
        let actions = if prompt_active {
            vec![TerminalClientLoopAction::ForwardToPane(input.clone())]
        } else {
            route_client_input_actions(&input, &terminal_config)?
        };
        let step = AttachedTerminalClientStepPlan {
            actions,
            output_lines: Vec::new(),
            output_line_style_spans: Vec::new(),
            input_hangup: false,
            output_hangup: false,
            error_roles: Vec::new(),
        };
        let application = self.apply_attached_terminal_step_plan(primary_client_id, &step)?;
        let view = if render {
            self.render_client_view_with_resolved_config(
                ClientViewRole::Primary,
                client_size,
                &terminal_config,
            )?
        } else {
            None
        };
        Ok(runtime_terminal_step_result_json(
            input.len(),
            &application,
            view.as_ref(),
        ))
    }

    /// Runs the dispatch runtime terminal view operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_terminal_view(
        &self,
        caller_client_id: &crate::ids::ClientId,
        params: Option<&str>,
    ) -> Result<String> {
        let client = self
            .session
            .clients()
            .iter()
            .find(|client| client.id == *caller_client_id)
            .ok_or_else(|| MezError::forbidden("unknown control client"))?;
        if !matches!(client.state, ClientState::Attached | ClientState::Pending) {
            return Err(MezError::forbidden("control client is not attached"));
        }
        let role = match client.role {
            ClientRole::Primary => ClientViewRole::Primary,
            ClientRole::PendingObserver => ClientViewRole::PendingObserver,
            ClientRole::Observer => ClientViewRole::Observer,
            ClientRole::Agent | ClientRole::Automation => {
                return Err(MezError::forbidden(
                    "client role is not authorized for terminal view",
                ));
            }
        };
        let client_size = match params {
            Some(params) => runtime_json_optional_client_size(params)?
                .unwrap_or(self.session.authoritative_size),
            None => self.session.authoritative_size,
        };
        let terminal_config =
            self.terminal_client_loop_config(TerminalClientLoopConfig::default())?;
        let mut view =
            self.render_client_view_with_resolved_config(role, client_size, &terminal_config)?;
        if let (Some(params), Some(view)) = (params, view.as_mut())
            && let Some((row, column)) = runtime_json_optional_view_offset(params)?
        {
            crate::terminal::apply_client_view_offset(view, row, column);
        }
        let view_json = view
            .as_ref()
            .map(rendered_client_view_json)
            .unwrap_or_else(|| "null".to_string());
        Ok(format!(r#"{{"view":{view_json}}}"#))
    }

    /// Dispatches runtime-owned agent shell visibility changes.
    ///
    /// The shared control layer mutates persisted agent shell state. The live
    /// runtime layers pane-local side effects on top of that state change so
    /// showing agent mode enters the scoped child shell and hiding agent mode
    /// leaves it when no turn still needs it.
    pub(super) fn dispatch_runtime_agent_shell_visibility_request(
        &mut self,
        body: &str,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &crate::ids::ClientId,
    ) -> String {
        let pane_id = match self.runtime_agent_shell_visibility_target_pane_id(request) {
            Ok(pane_id) => pane_id,
            Err(error) => {
                return runtime_json_rpc_error(&request.id, error.kind(), error.message());
            }
        };
        let response = dispatch_control_request_for_client_with_agent_state(
            body,
            &mut self.session,
            caller_client_id,
            None,
            &mut self.agent_shell_store,
            &self.agent_turn_ledger,
        );
        if response.contains(r#""error""#) {
            return response;
        }
        let side_effect = if request.method == "agent/shell/show" {
            self.enter_agent_mode_for_pane(&pane_id)
                .and_then(|_| self.clear_agent_shell_terminal_view(&pane_id).map(|_| ()))
        } else {
            self.request_agent_shell_exit_for_pane(&pane_id).map(|_| ())
        }
        .and_then(|()| self.sync_tracked_pty_sizes().map(|_| ()));
        match side_effect {
            Ok(()) => response,
            Err(error) => runtime_json_rpc_error(&request.id, error.kind(), error.message()),
        }
    }

    /// Resolves the pane affected by an `agent/shell/show` or
    /// `agent/shell/hide` request before live side effects are applied.
    fn runtime_agent_shell_visibility_target_pane_id(
        &self,
        request: &crate::control::JsonRpcRequest,
    ) -> Result<String> {
        let params = request.params.as_deref().ok_or_else(|| {
            MezError::invalid_args(format!("{} requires a params object", request.method))
        })?;
        pane_target_checked_resolved(&self.session, params)?
            .map(Ok)
            .unwrap_or_else(|| self.active_pane_id())
    }

    /// Runs the dispatch runtime terminal command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_terminal_command(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let input = runtime_json_string_field(params, "input")
            .ok_or_else(|| MezError::invalid_args("terminal/command requires input"))?;
        self.execute_terminal_command(primary_client_id, &input)
    }

    /// Runs the dispatch runtime agent shell command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_agent_shell_command(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let input = runtime_json_string_field(params, "input")
            .ok_or_else(|| MezError::invalid_args("agent/shell/command requires input"))?;
        self.execute_agent_shell_command(primary_client_id, &input)
    }

    /// Ensures a runtime-created agent identity exists in the local MMP service.
    ///
    /// Agent ids are opaque MMP identities. When the id follows the runtime
    /// `agent-%pane` convention, the identity is enriched with pane and window
    /// metadata so discovery can connect the agent to its terminal surface.
    fn ensure_runtime_message_identity(
        &mut self,
        agent_id: &str,
        pane_id: Option<PaneId>,
        role: &str,
        capabilities: &[&str],
        now_ms: u64,
    ) -> Result<SenderIdentity> {
        let agent_id = AgentId::opaque(agent_id.to_string())
            .ok_or_else(|| MezError::invalid_args("agent id is invalid for MMP"))?;
        let pane_id = pane_id.or_else(|| pane_id_from_runtime_agent_id(agent_id.as_str()));
        let window_id = pane_id
            .as_ref()
            .and_then(|pane_id| self.find_pane_descriptor(pane_id.as_str()))
            .map(|descriptor| descriptor.window_id);
        self.message_service.ensure_agent_identity(
            SenderIdentity {
                agent_id,
                pane_id,
                window_id,
                role: Some(role.to_string()),
                capabilities: capabilities
                    .iter()
                    .map(|capability| (*capability).to_string())
                    .collect(),
            },
            now_ms,
        )
    }

    /// Runs the dispatch runtime pane capture operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_capture(
        &mut self,
        body: &str,
        request_id: &str,
        caller_client_id: &crate::ids::ClientId,
    ) -> String {
        if self.session.primary_client_id() != Some(caller_client_id) {
            return runtime_json_rpc_error(
                request_id,
                crate::error::MezErrorKind::Forbidden,
                "operation requires the primary client",
            );
        }
        let captures = self.pane_capture_sources();
        dispatch_control_request_with_captures(body, &mut self.session, caller_client_id, &captures)
    }

    /// Runs the pane capture sources operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn pane_capture_sources(&self) -> Vec<PaneCaptureSource> {
        self.pane_screens
            .iter()
            .map(|(pane_id, screen)| {
                let history_styled_lines = screen.history().styled_lines().collect::<Vec<_>>();
                let primary_pid = self.primary_pid_for_live_pane_process(pane_id);
                let process_state = primary_pid.map(|_| "running").unwrap_or_else(|| {
                    match runtime_pane_by_id(&self.session, pane_id) {
                        Ok((_, pane)) if pane.live => "starting",
                        Ok((_, _)) => "exited",
                        Err(_) => "unknown",
                    }
                });
                PaneCaptureSource {
                    pane_id: pane_id.clone(),
                    visible_lines: screen.visible_lines(),
                    visible_line_style_spans: screen
                        .visible_styled_lines()
                        .into_iter()
                        .map(|line| line.style_spans)
                        .collect(),
                    history_lines: history_styled_lines
                        .iter()
                        .map(|line| line.text.clone())
                        .collect(),
                    history_line_style_spans: history_styled_lines
                        .into_iter()
                        .map(|line| line.style_spans)
                        .collect(),
                    alternate_screen_active: screen.alternate_screen_active(),
                    truncated: false,
                    primary_pid,
                    process_state: Some(process_state.to_string()),
                    exit_status: self
                        .pane_exit_records
                        .get(pane_id)
                        .map(|record| record.exit_status),
                }
            })
            .collect()
    }
    /// Runs the require attachable operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn require_attachable(&self) -> Result<()> {
        match self.lifecycle_state {
            RuntimeLifecycleState::Running | RuntimeLifecycleState::Detached => Ok(()),
            RuntimeLifecycleState::Stopping => {
                Err(MezError::invalid_state("runtime service is stopping"))
            }
            RuntimeLifecycleState::Killed => Err(MezError::invalid_state(
                "runtime service has already been killed",
            )),
            RuntimeLifecycleState::Failed => Err(MezError::invalid_state(
                "runtime service is in a failed lifecycle state",
            )),
        }
    }

    /// Runs the require live operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn require_live(&self) -> Result<()> {
        match self.lifecycle_state {
            RuntimeLifecycleState::Running | RuntimeLifecycleState::Detached => Ok(()),
            RuntimeLifecycleState::Stopping => {
                Err(MezError::invalid_state("runtime service is stopping"))
            }
            RuntimeLifecycleState::Killed => Err(MezError::invalid_state(
                "runtime service has already been killed",
            )),
            RuntimeLifecycleState::Failed => Err(MezError::invalid_state(
                "runtime service is in a failed lifecycle state",
            )),
        }
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

/// Runs the append project command rule text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn append_project_command_rule_text(text: &str, rule: &CommandRule) -> Result<String> {
    let (digest_hex, shell_classification) = match &rule.rule_match {
        RuleMatch::ExactSha256 {
            digest_hex,
            shell_classification,
        } => (digest_hex, shell_classification),
        RuleMatch::Prefix | RuleMatch::Exact => {
            return Err(MezError::invalid_args(
                "project approval persistence requires an exact_sha256 command rule",
            ));
        }
    };
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid project TOML config: {error}")))?;
    if document.as_table().get("version").is_some() {
        document.as_table_mut().insert(
            "version",
            toml_edit::value(crate::config::CURRENT_CONFIG_SCHEMA_VERSION as i64),
        );
    } else {
        let text = if text.trim().is_empty() {
            format!(
                "version = {}\n",
                crate::config::CURRENT_CONFIG_SCHEMA_VERSION
            )
        } else if text.ends_with('\n') {
            format!(
                "version = {}\n{text}",
                crate::config::CURRENT_CONFIG_SCHEMA_VERSION
            )
        } else {
            format!(
                "version = {}\n{text}\n",
                crate::config::CURRENT_CONFIG_SCHEMA_VERSION
            )
        };
        document = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| MezError::config(format!("invalid project TOML config: {error}")))?;
    }
    let root = document.as_table_mut();
    if root.get("permissions").is_none() {
        root.insert(
            "permissions",
            toml_edit::Item::Table(toml_edit::Table::new()),
        );
    }
    let permissions = root
        .get_mut("permissions")
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| MezError::config("project config permissions must be a table"))?;
    if let Some(item) = permissions.get("command_rules") {
        let replace_empty_array = matches!(item, toml_edit::Item::Value(value) if value.as_array().is_some_and(|array| array.is_empty()));
        if replace_empty_array {
            permissions.remove("command_rules");
        }
    }
    if permissions.get("command_rules").is_none() {
        permissions.insert(
            "command_rules",
            toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    let rules = permissions
        .get_mut("command_rules")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
        .ok_or_else(|| {
            MezError::config("project config permissions.command_rules must be an array of tables")
        })?;
    let mut pattern = toml_edit::Array::default();
    pattern.push(digest_hex.as_str());
    let mut table = toml_edit::Table::new();
    table.insert("pattern", toml_edit::value(pattern));
    table.insert(
        "decision",
        toml_edit::value(project_rule_decision_name(rule.decision)),
    );
    table.insert(
        "scope",
        toml_edit::value(project_rule_scope_name(rule.scope)),
    );
    table.insert("match", toml_edit::value("exact_sha256"));
    table.insert("exact_sha256", toml_edit::value(digest_hex.as_str()));
    table.insert(
        "shell_classification",
        toml_edit::value(shell_classification.as_str()),
    );
    if let Some(justification) = &rule.justification {
        table.insert("justification", toml_edit::value(justification.as_str()));
    }
    rules.push(table);
    let updated = document.to_string();
    let validation =
        validate_config_text(ConfigFormat::Toml, &updated, ConfigScope::ProjectOverlay);
    if !validation.valid {
        let summary = validation
            .diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.path, diagnostic.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(MezError::config(format!(
            "project command rule persistence produced invalid config: {summary}"
        )));
    }
    Ok(updated)
}

/// Runs the project rule decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn project_rule_decision_name(decision: RuleDecision) -> &'static str {
    match decision {
        RuleDecision::Forbid => "deny",
        RuleDecision::Prompt => "prompt",
        RuleDecision::Allow => "allow",
    }
}

/// Runs the project rule scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn project_rule_scope_name(scope: CommandRuleScope) -> &'static str {
    match scope {
        CommandRuleScope::BuiltIn => "built-in",
        CommandRuleScope::Session => "session",
        CommandRuleScope::Project => "project",
        CommandRuleScope::User => "user",
        CommandRuleScope::Managed => "managed",
    }
}

/// Runs the runtime snapshot config scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_config_scope_name(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Primary => "primary",
        ConfigScope::ProjectOverlay => "project-overlay",
        ConfigScope::LiveOverride => "live-override",
    }
}

/// Runs the runtime snapshot frame position name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_frame_position_name(position: TerminalFramePosition) -> &'static str {
    match position {
        TerminalFramePosition::Top => "top",
        TerminalFramePosition::Bottom => "bottom",
    }
}

/// Runs the runtime snapshot frame style name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_frame_style_name(style: TerminalFrameStyle) -> &'static str {
    match style {
        TerminalFrameStyle::Default => "default",
        TerminalFrameStyle::Bold => "bold",
        TerminalFrameStyle::Underline => "underline",
        TerminalFrameStyle::Inverse => "inverse",
    }
}

/// Runs the runtime snapshot approval grant operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_approval_grant(grant: &ApprovalGrant) -> SnapshotApprovalGrantMetadata {
    SnapshotApprovalGrantMetadata {
        id: grant.id.clone(),
        command_prefix: grant.command_prefix.clone(),
        scope: runtime_snapshot_approval_scope_name(grant.scope).to_string(),
        decision: runtime_snapshot_approval_decision_name(grant.decision).to_string(),
    }
}

/// Runs the runtime snapshot approval request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_approval_request(
    request: &BlockedApprovalRequest,
) -> SnapshotApprovalRequestMetadata {
    SnapshotApprovalRequestMetadata {
        id: request.id.clone(),
        requesting_agent_id: request.requesting_agent_id.clone(),
        pane_id: request.pane_id.clone(),
        parent_agent_chain: request.parent_agent_chain.clone(),
        action_kind: request.action_kind.clone(),
        action_summary: request.action_summary.clone(),
        declared_effects: request.declared_effects.clone(),
        matched_rules: request.matched_rules.clone(),
        read_scopes: request.read_scopes.clone(),
        write_scopes: request.write_scopes.clone(),
        created_at_unix_seconds: request.created_at_unix_seconds,
        decided_at_unix_seconds: request.decided_at_unix_seconds,
        decided_by_client_id: request.decided_by_client_id.clone(),
        state: runtime_snapshot_blocked_approval_state_name(request.state).to_string(),
        decision: request
            .decision
            .map(runtime_snapshot_approval_decision_name)
            .map(ToOwned::to_owned),
        redirect_instruction: request.redirect_instruction.clone(),
    }
}

/// Runs the runtime snapshot mcp server state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_mcp_server_state(
    server: &crate::mcp::McpServerState,
) -> SnapshotMcpServerState {
    SnapshotMcpServerState {
        id: server.configured.id.clone(),
        name: server.configured.name.clone(),
        kind: runtime_snapshot_mcp_kind_name(server.configured.kind).to_string(),
        enabled: server.configured.enabled,
        status: runtime_snapshot_mcp_status_name(server.status).to_string(),
        last_checked_at_unix_seconds: server.last_checked_at_unix_seconds,
        blacklist_reason: server.blacklist_reason.clone(),
        external_capability: runtime_snapshot_mcp_external_capability(
            &server.configured.external_capability,
        ),
        tools: server.tools.iter().map(runtime_snapshot_mcp_tool).collect(),
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

/// Runs the runtime snapshot mcp external capability operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_mcp_external_capability(
    capability: &McpExternalCapability,
) -> SnapshotMcpExternalCapability {
    SnapshotMcpExternalCapability {
        mutates_filesystem_outside_shell: capability.mutates_filesystem_outside_shell,
        executes_processes_outside_shell: capability.executes_processes_outside_shell,
        accesses_credentials_outside_shell: capability.accesses_credentials_outside_shell,
        purpose: capability.purpose.clone(),
    }
}

/// Runs the runtime snapshot mcp tool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_mcp_tool(tool: &McpToolState) -> SnapshotMcpToolState {
    SnapshotMcpToolState {
        server_id: tool.server_id.clone(),
        name: tool.name.clone(),
        available: tool.available,
        blacklisted: tool.blacklisted,
        permission_required: tool.permission_required,
        effects: runtime_snapshot_mcp_effects(tool.effects),
        approval: runtime_snapshot_mcp_approval_name(tool.approval).to_string(),
        description: tool.description.clone(),
        input_schema_json: tool.input_schema_json.clone(),
    }
}

/// Runs the runtime snapshot mcp effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_mcp_effects(effects: McpToolEffects) -> SnapshotMcpToolEffects {
    SnapshotMcpToolEffects {
        reads_filesystem: effects.reads_filesystem,
        mutates_filesystem: effects.mutates_filesystem,
        executes_processes: effects.executes_processes,
        accesses_credentials: effects.accesses_credentials,
        uses_network: effects.uses_network,
        has_side_effects: effects.has_side_effects,
    }
}

/// Runs the runtime snapshot mcp kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_mcp_kind_name(kind: McpServerKind) -> &'static str {
    match kind {
        McpServerKind::Stdio => "stdio",
        McpServerKind::Http => "streamable_http",
    }
}

/// Runs the runtime snapshot mcp status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_mcp_status_name(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Configured => "configured",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

/// Runs the runtime snapshot mcp approval name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_mcp_approval_name(approval: McpApprovalSetting) -> &'static str {
    match approval {
        McpApprovalSetting::Inherit => "inherit",
        McpApprovalSetting::Prompt => "prompt",
        McpApprovalSetting::Allow => "allow",
        McpApprovalSetting::Deny => "deny",
    }
}

/// Runs the runtime mutating response is cacheable operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_mutating_response_is_cacheable(method: &str) -> bool {
    method != "terminal/step"
}

/// Runs the runtime snapshot approval scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_approval_scope_name(scope: ApprovalScope) -> &'static str {
    match scope {
        ApprovalScope::Session => "session",
        ApprovalScope::Global => "global",
    }
}

/// Runs the runtime snapshot approval decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_approval_decision_name(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Approve => "approve",
        ApprovalDecision::Disapprove => "disapprove",
        ApprovalDecision::Redirect => "redirect",
    }
}

/// Runs the runtime snapshot blocked approval state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_blocked_approval_state_name(state: BlockedApprovalState) -> &'static str {
    match state {
        BlockedApprovalState::Pending => "pending",
        BlockedApprovalState::Approved => "approved",
        BlockedApprovalState::Disapproved => "disapproved",
        BlockedApprovalState::Redirected => "redirected",
    }
}

/// Runs the runtime snapshot agent visibility name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_snapshot_agent_visibility_name(visibility: AgentShellVisibility) -> &'static str {
    match visibility {
        AgentShellVisibility::Hidden => "hidden",
        AgentShellVisibility::Visible => "visible",
        AgentShellVisibility::HidePendingTaskCompletion => "hide-pending-task-completion",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
