//! Agent actions implementation.
//!
//! This module owns the agent actions boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::AsyncModelProvider;
#[cfg(test)]
use super::ModelProvider;
use super::shell::{SHELL_OUTPUT_BASE64_BEGIN_MARKER, SHELL_OUTPUT_BASE64_END_MARKER};
use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentCapability, AgentContext,
    AgentTranscriptStore, AgentTurnLedger, AgentTurnRecord, AgentTurnState, AllowedAction,
    AllowedActionSet, ContextSourceKind, DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS, EnvironmentSignature,
    MaapBatch, MarkerToken, McpPromptTool, McpToolCallPlan, McpToolCallResponse, MezError,
    ModelInteractionKind, ModelMessage, ModelMessageRole, ModelProfile, ModelRequest,
    ModelResponse, ModelTokenUsage, Path, PathScopes, PermissionPolicy, Result, RuleDecision,
    SayStatus, SessionApprovalStore, ShellTransaction, ShellTransactionOutputTransport,
    ToolDiscoveryCache, ToolInventory, TranscriptEntry, TranscriptRole,
    action_content_blocks_from_json_or_text, action_text_content_blocks, assemble_model_request,
    constrain_skill_actions_for_loaded_context, json_escape, local_action_plan,
    local_action_summary, network_action_plan, network_action_structured_content_json,
    network_action_summary, provider_error_invites_retry, provider_error_is_context_limit_exceeded,
    provider_error_is_output_limit_exceeded, string_array_json, tool_discovery_script,
};
use crate::subagent::SubagentScopeDeclaration;
use base64::Engine;

// Shell/MCP executors, action execution, and transcript persistence.

/// Maximum number of ephemeral provider retries after a MAAP validation error.
///
/// The retry instruction is appended only to a cloned request and is never
/// returned in `AgentTurnExecution.request`, keeping repair diagnostics out of
/// durable transcripts and future model context when the corrected response is
/// valid.
const MAAP_REPAIR_ATTEMPT_LIMIT: usize = 2;

/// Maximum previous-response bytes included in one ephemeral MAAP repair prompt.
const MAAP_REPAIR_RAW_TEXT_LIMIT_BYTES: usize = 12 * 1024;
/// Maximum previous-response bytes included in a terminal failure summary prompt.
const FAILURE_SUMMARY_RAW_TEXT_LIMIT_BYTES: usize = 8 * 1024;
/// Maximum non-executing capability negotiations before a turn fails closed.
const CAPABILITY_REQUEST_ATTEMPT_LIMIT: usize = 3;
/// Maximum action-result content bytes included in one model-facing context
/// block before native truncation metadata is appended.
const MODEL_ACTION_RESULT_CONTENT_LIMIT_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Carries shell execution request state for this subsystem.
///
/// The fields are kept explicit so callers can inspect and move structured
/// runtime data without parsing display text.
pub struct ShellExecutionRequest {
    /// Structured `action_id` value carried by this API type.
    pub action_id: String,
    /// Structured `transaction` value carried by this API type.
    pub transaction: ShellTransaction,
    /// Structured `timeout_ms` value carried by this API type.
    pub timeout_ms: Option<u64>,
    /// Structured `interactive` value carried by this API type.
    pub interactive: bool,
    /// Structured `stateful` value carried by this API type.
    pub stateful: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Carries shell execution output state for this subsystem.
///
/// The fields are kept explicit so callers can inspect and move structured
/// runtime data without parsing display text.
pub struct ShellExecutionOutput {
    /// Structured `exit_code` value carried by this API type.
    pub exit_code: Option<i32>,
    /// Structured `stdout` value carried by this API type.
    pub stdout: String,
    /// Structured `stderr` value carried by this API type.
    pub stderr: String,
    /// Structured `timed_out` value carried by this API type.
    pub timed_out: bool,
    /// Structured `interrupted` value carried by this API type.
    pub interrupted: bool,
}

/// Defines the `PaneShellExecutor` behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary used
/// by higher-level runtime code.
pub trait PaneShellExecutor {
    /// Runs the execute shell operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn execute_shell(&mut self, request: &ShellExecutionRequest) -> Result<ShellExecutionOutput>;
}

/// Defines the `McpActionExecutor` behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary used
/// by higher-level runtime code.
pub trait McpActionExecutor {
    /// Runs the execute mcp call operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn execute_mcp_call(&mut self, plan: &McpToolCallPlan) -> Result<McpToolCallResponse>;
}

#[allow(async_fn_in_trait)]
/// Defines the `AsyncMcpActionExecutor` behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary used
/// by higher-level runtime code.
pub trait AsyncMcpActionExecutor {
    /// Runs the execute mcp call async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn execute_mcp_call_async(
        &mut self,
        plan: &McpToolCallPlan,
    ) -> Result<McpToolCallResponse>;
}

/// Executes the `execute_shell_action_through_pane` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn execute_shell_action_through_pane(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    marker: MarkerToken,
    shell_path: &Path,
    executor: &mut impl PaneShellExecutor,
) -> Result<ActionResult> {
    let Some(plan) = local_action_plan(action)? else {
        return Err(MezError::invalid_args(
            "pane shell execution requires a shell-backed action",
        ));
    };
    let transaction = ShellTransaction::new(
        marker.clone(),
        &turn.turn_id,
        &turn.agent_id,
        &turn.pane_id,
        shell_path,
        &plan.command,
    )?
    .with_output_transport(ShellTransactionOutputTransport::Base64);
    let request = ShellExecutionRequest {
        action_id: action.id.clone(),
        transaction,
        timeout_ms: plan.timeout_ms,
        interactive: plan.interactive,
        stateful: plan.stateful,
    };
    let output = postprocess_semantic_shell_output(action, executor.execute_shell(&request)?)?;
    shell_output_to_action_result(turn, action, output, marker)
}

/// Applies native success-output shaping for shell-backed semantic actions.
///
/// Pane-side semantic commands stay limited to small shell primitives. Line
/// slicing, truncation notices, and generated change previews are applied here
/// after the pane shell returns its bounded output.
pub fn postprocess_shell_action_success_output(
    action: &AgentAction,
    stdout: String,
) -> Result<String> {
    let output = ShellExecutionOutput {
        exit_code: Some(0),
        stdout,
        stderr: String::new(),
        timed_out: false,
        interrupted: false,
    };
    postprocess_semantic_shell_output(action, output).map(|output| output.stdout)
}

/// Decodes shell-output transport blocks emitted by non-stateful transactions.
///
/// # Parameters
/// - `stdout`: Bounded raw PTY observation for one shell transaction.
pub fn decode_shell_output_transport(stdout: &str) -> String {
    decode_base64_transport_output(
        stdout,
        SHELL_OUTPUT_BASE64_BEGIN_MARKER,
        SHELL_OUTPUT_BASE64_END_MARKER,
        "shell output",
    )
}

/// Builds compact action-result content for a plain model-authored shell command.
///
/// # Parameters
/// - `output`: The command stdout/stderr already decoded for model context.
/// - `exit_code`: The observed process exit code, when one was observed.
/// - `timed_out`: Whether the command timed out before a process exit.
/// - `interrupted`: Whether the command was interrupted by the runtime.
pub fn shell_command_result_content(
    output: &str,
    exit_code: Option<i32>,
    timed_out: bool,
    interrupted: bool,
) -> Vec<String> {
    if !output.trim().is_empty() {
        return vec![output.to_string()];
    }
    let status = if timed_out {
        "shell command timed out".to_string()
    } else if interrupted {
        "shell command was interrupted".to_string()
    } else if let Some(exit_code) = exit_code {
        format!("shell command exited with status {exit_code}")
    } else {
        "shell command finished without an exit status".to_string()
    };
    vec![status]
}

fn postprocess_semantic_shell_output(
    action: &AgentAction,
    mut output: ShellExecutionOutput,
) -> Result<ShellExecutionOutput> {
    output.stdout = decode_shell_output_transport(&output.stdout);
    if output.exit_code != Some(0) || output.timed_out || output.interrupted {
        return Ok(output);
    }
    if let AgentActionPayload::ApplyPatch { patch, .. } = &action.payload {
        ensure_success_preview(&mut output, patch_change_preview(patch));
    }
    Ok(output)
}

fn decode_base64_transport_output(stdout: &str, begin: &str, end: &str, label: &str) -> String {
    if !stdout.contains(begin) {
        return stdout.to_string();
    }
    let normalized = stdout.replace("\r\n", "\n").replace('\r', "\n");
    let mut decoded = String::new();
    let mut block = String::new();
    let mut outside_block = String::new();
    let mut in_block = false;
    for line in normalized.split_inclusive('\n') {
        let marker_candidate = line.trim_end_matches('\n');
        if marker_candidate == begin {
            append_non_wrapper_transport_text(&mut decoded, &outside_block);
            outside_block.clear();
            in_block = true;
            block.clear();
            continue;
        }
        if marker_candidate == end {
            decoded.push_str(&decode_base64_transport_block(&block, label, false));
            in_block = false;
            block.clear();
            continue;
        }
        if in_block {
            block.push_str(marker_candidate.trim());
        } else {
            outside_block.push_str(line);
        }
    }
    if in_block {
        decoded.push_str(&decode_base64_transport_block(&block, label, true));
        decoded.push_str(&format!(
            "[mez: {label} base64 transport ended before end marker]\n"
        ));
    } else {
        append_non_wrapper_transport_text(&mut decoded, &outside_block);
    }
    decoded
}

fn append_non_wrapper_transport_text(decoded: &mut String, text: &str) {
    for line in text.split_inclusive('\n') {
        let (line, had_newline) = line
            .strip_suffix('\n')
            .map(|line| (line, true))
            .unwrap_or((line, false));
        let trimmed = line.trim();
        if trimmed.is_empty()
            || shell_output_line_is_mezzanine_wrapper(trimmed)
            || shell_output_line_is_mezzanine_transport_scaffold(trimmed)
        {
            continue;
        }
        decoded.push_str(line);
        if had_newline {
            decoded.push('\n');
        }
    }
}

fn shell_output_line_is_mezzanine_transport_scaffold(trimmed: &str) -> bool {
    matches!(
        trimmed,
        "{" | "}" | "done" | "-" | "C" | "I" | "o" | "SY" | "TC" | "PS0"
    )
}

fn decode_base64_transport_block(block: &str, label: &str, partial: bool) -> String {
    let mut cleaned = block
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    if partial {
        let full_quartets = cleaned.len() - (cleaned.len() % 4);
        cleaned.truncate(full_quartets);
    }
    if cleaned.is_empty() {
        return String::new();
    }
    match base64::engine::general_purpose::STANDARD.decode(cleaned.as_bytes()) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                let bytes = error.into_bytes();
                let mut text = format!(
                    "[mez: {label} contained non-UTF-8 bytes; decoded {} bytes with replacement characters]\n",
                    bytes.len()
                );
                text.push_str(&String::from_utf8_lossy(&bytes));
                text
            }
        },
        Err(error) => format!("[mez: failed to decode {label} base64 transport: {error}]\n"),
    }
}

fn ensure_success_preview(output: &mut ShellExecutionOutput, preview: String) {
    if output.stdout.trim().is_empty() {
        output.stdout = preview;
    }
}

fn patch_change_preview(patch: &str) -> String {
    const MAX_PREVIEW_LINES: usize = 160;
    let mut lines = vec!["diff -- apply patch".to_string()];
    for line in patch.lines().take(MAX_PREVIEW_LINES) {
        lines.push(line.to_string());
    }
    let total_lines = patch.lines().count();
    if total_lines > MAX_PREVIEW_LINES {
        lines.push(format!(
            "[mez: diff truncated; {} lines omitted]",
            total_lines - MAX_PREVIEW_LINES
        ));
    }
    lines.join("\n") + "\n"
}

/// Executes the `execute_mcp_action_through_runtime` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn execute_mcp_action_through_runtime(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    plan: &McpToolCallPlan,
    executor: &mut impl McpActionExecutor,
) -> Result<ActionResult> {
    let AgentActionPayload::McpCall {
        server,
        tool,
        arguments_json,
    } = &action.payload
    else {
        return Err(MezError::invalid_args(
            "MCP execution requires an mcp_call action",
        ));
    };
    if plan.server_id != *server
        || plan.tool_name != *tool
        || plan.arguments_json.trim() != arguments_json.trim()
    {
        return Err(MezError::invalid_args(
            "MCP execution plan does not match the action payload",
        ));
    }

    let response = executor.execute_mcp_call(plan)?;
    mcp_response_to_action_result(turn, action, plan, response)
}

/// Executes the `execute_mcp_action_through_runtime_async` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub async fn execute_mcp_action_through_runtime_async(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    plan: &McpToolCallPlan,
    executor: &mut impl AsyncMcpActionExecutor,
) -> Result<ActionResult> {
    let AgentActionPayload::McpCall {
        server,
        tool,
        arguments_json,
    } = &action.payload
    else {
        return Err(MezError::invalid_args(
            "MCP execution requires an mcp_call action",
        ));
    };
    if plan.server_id != *server
        || plan.tool_name != *tool
        || plan.arguments_json.trim() != arguments_json.trim()
    {
        return Err(MezError::invalid_args(
            "MCP execution plan does not match the action payload",
        ));
    }

    let response = executor.execute_mcp_call_async(plan).await?;
    mcp_response_to_action_result(turn, action, plan, response)
}

/// Executes the `discover_tools_through_pane_shell` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn discover_tools_through_pane_shell(
    cache: &mut ToolDiscoveryCache,
    signature: EnvironmentSignature,
    turn: &AgentTurnRecord,
    marker: MarkerToken,
    shell_path: &Path,
    executor: &mut impl PaneShellExecutor,
) -> Result<ToolInventory> {
    if let Some(inventory) = cache.get(&signature) {
        return Ok(inventory.clone());
    }

    let transaction = ShellTransaction::new(
        marker,
        &turn.turn_id,
        &turn.agent_id,
        &turn.pane_id,
        shell_path,
        tool_discovery_script(),
    )?;
    let request = ShellExecutionRequest {
        action_id: format!("tool-discovery:{}", turn.turn_id),
        transaction,
        timeout_ms: Some(DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS),
        interactive: false,
        stateful: false,
    };
    let output = executor.execute_shell(&request)?;
    if output.timed_out {
        return Err(MezError::invalid_state("tool discovery timed out"));
    }
    if output.interrupted {
        return Err(MezError::invalid_state("tool discovery was interrupted"));
    }
    if output.exit_code != Some(0) {
        return Err(MezError::invalid_state(format!(
            "tool discovery failed: {}",
            output.stderr.trim()
        )));
    }

    let inventory = ToolInventory::parse_bootstrap_output(&output.stdout);
    cache.record(signature, inventory.clone());
    Ok(inventory)
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Carries agent turn execution state for this subsystem.
///
/// The fields are kept explicit so callers can inspect and move structured
/// runtime data without parsing display text.
pub struct AgentTurnExecution {
    /// Structured `request` value carried by this API type.
    pub request: ModelRequest,
    /// Structured `response` value carried by this API type.
    pub response: ModelResponse,
    /// Provider token usage from the latest model response in this execution.
    ///
    /// `response.usage` may carry cumulative usage across capability,
    /// execution, and repair provider calls. This field preserves the latest
    /// single provider response so UI context-window percentages describe the
    /// last prompt sent to the model instead of an accumulated turn total.
    pub latest_response_usage: ModelTokenUsage,
    /// Structured `action_results` value carried by this API type.
    pub action_results: Vec<ActionResult>,
    /// Structured `final_turn` value carried by this API type.
    pub final_turn: bool,
    /// Structured `terminal_state` value carried by this API type.
    pub terminal_state: AgentTurnState,
}

/// Executes the `transcript_entries_for_execution` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn transcript_entries_for_execution(
    conversation_id: &str,
    first_sequence: u64,
    created_at_unix_seconds: u64,
    turn: &AgentTurnRecord,
    execution: &AgentTurnExecution,
) -> Result<Vec<TranscriptEntry>> {
    if first_sequence == 0 || created_at_unix_seconds == 0 {
        return Err(MezError::invalid_args(
            "transcript sequence and creation time must be non-zero",
        ));
    }
    let mut sequence = first_sequence;
    let mut entries = Vec::new();
    for message in &execution.request.messages {
        let Some(content) = durable_request_transcript_content(message) else {
            continue;
        };
        entries.push(TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role: TranscriptRole::User,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content,
        });
        sequence = sequence.saturating_add(1);
    }
    entries.push(TranscriptEntry {
        conversation_id: conversation_id.to_string(),
        sequence,
        created_at_unix_seconds,
        role: TranscriptRole::Assistant,
        turn_id: turn.turn_id.clone(),
        agent_id: turn.agent_id.clone(),
        pane_id: turn.pane_id.clone(),
        content: assistant_transcript_content(execution),
    });
    sequence = sequence.saturating_add(1);

    for result in &execution.action_results {
        entries.push(TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role: TranscriptRole::Tool,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content: action_result_transcript_content(result),
        });
        sequence = sequence.saturating_add(1);
    }
    for entry in &entries {
        entry.validate()?;
    }
    Ok(entries)
}

/// Returns the assistant-history context produced by one model execution.
///
/// The returned text is the same assistant content durable transcript storage
/// would persist for the execution: visible `say` text is retained, non-visible
/// actions are summarized, and MAAP rationale text is preserved as `thinking:`
/// lines without retaining raw protocol JSON or inline file payloads.
pub fn assistant_context_content_for_execution(execution: &AgentTurnExecution) -> String {
    assistant_transcript_content(execution)
}

/// Returns durable request text for transcript storage.
///
/// Model requests are assembled from prompt scaffolding: system prompts,
/// environment details, prior transcript excerpts, action-result context, and
/// action feedback. Persisting that whole request recursively stores prior
/// transcript context inside the next transcript entry. Durable transcripts
/// therefore keep only the current user instruction; assistant output and tool
/// results are appended from the execution itself.
fn durable_request_transcript_content(message: &ModelMessage) -> Option<String> {
    if message.source != ContextSourceKind::UserInstruction
        || message.role != ModelMessageRole::User
    {
        return None;
    }
    if labeled_context_label(&message.content).is_some_and(transcript_label_is_expanded_skill) {
        return None;
    }
    let content = labeled_context_body(&message.content).unwrap_or(message.content.as_str());
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Returns the rendered context block label when one is present.
fn labeled_context_label(content: &str) -> Option<&str> {
    let after_label = content.strip_prefix('[')?;
    let label_end = after_label.find("]\n")?;
    Some(&after_label[..label_end])
}

/// Returns the body of a rendered context block when one is present.
fn labeled_context_body(content: &str) -> Option<&str> {
    let after_label = content.strip_prefix('[')?;
    let label_end = after_label.find("]\n")?;
    Some(&after_label[label_end + 2..])
}

/// Returns whether one rendered context label is an expanded skill payload.
fn transcript_label_is_expanded_skill(label: &str) -> bool {
    label.starts_with("explicit skill ") || label.starts_with("explicit skill invocation ")
}

/// Returns durable assistant transcript text without copying raw protocol JSON
/// or inline file payloads into long-lived transcript storage.
///
/// MAAP rationale text is persisted as compact `thinking:` lines because it is
/// the model-authored continuity thread behind the visible action sequence.
fn assistant_transcript_content(execution: &AgentTurnExecution) -> String {
    let Some(batch) = execution.response.action_batch.as_ref() else {
        return execution.response.raw_text.clone();
    };
    let mut thinking_lines = assistant_transcript_rationale_lines(batch);
    if !execution.response.raw_text.trim().is_empty()
        && !assistant_raw_text_looks_like_maap_payload(&execution.response.raw_text)
    {
        if thinking_lines.is_empty() {
            return execution.response.raw_text.clone();
        }
        thinking_lines.push(execution.response.raw_text.clone());
        return thinking_lines.join("\n");
    }
    if let Some(visible_text) = assistant_visible_action_transcript_content(batch) {
        if thinking_lines.is_empty() {
            return visible_text;
        }
        thinking_lines.push(visible_text);
        return thinking_lines.join("\n");
    }
    thinking_lines.push(format!(
        "[assistant emitted MAAP actions; action_count={}]",
        batch.actions.len()
    ));
    for action in &batch.actions {
        thinking_lines.push(format!("- {}", assistant_transcript_action_summary(action)));
    }
    thinking_lines.join("\n")
}

/// Returns model-authored rationale and thought text as transcript-visible
/// thinking lines.
///
/// Batch and action rationales are rendered as thinking messages in the pane
/// UI. Batch thoughts are hidden from normal-mode pane logs but still persisted
/// here so later turns can reference durable work notes without storing raw
/// MAAP payloads.
fn assistant_transcript_rationale_lines(batch: &MaapBatch) -> Vec<String> {
    let mut lines = Vec::new();
    if !batch.rationale.trim().is_empty() {
        lines.extend(assistant_transcript_thinking_lines(&batch.rationale));
    }
    if let Some(thought) = batch.thought.as_deref()
        && !thought.trim().is_empty()
    {
        lines.extend(assistant_transcript_thinking_lines(thought));
    }
    for action in &batch.actions {
        if !action.rationale.trim().is_empty() {
            lines.extend(assistant_transcript_thinking_lines(&action.rationale));
        }
    }
    lines
}

/// Prefixes each non-empty line of model-authored thinking text for durable
/// assistant transcript storage.
fn assistant_transcript_thinking_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| format!("thinking: {line}"))
        .collect()
}

/// Returns the user-visible assistant text carried by a MAAP action batch.
///
/// `say` actions are conversational output from the user's perspective.
/// Persisting only their compact action summaries breaks later references such
/// as "do item 2"; this helper preserves that text while still summarizing
/// non-conversational actions.
fn assistant_visible_action_transcript_content(batch: &MaapBatch) -> Option<String> {
    let mut visible_lines = Vec::new();
    let mut hidden_action_summaries = Vec::new();
    for action in &batch.actions {
        match &action.payload {
            AgentActionPayload::Say { text, .. } => visible_lines.push(text.trim().to_string()),
            AgentActionPayload::Abort { reason } => {
                visible_lines.push(format!("aborted: {}", reason.trim()));
            }
            _ => hidden_action_summaries.push(assistant_transcript_action_summary(action)),
        }
    }
    visible_lines.retain(|line| !line.is_empty());
    if visible_lines.is_empty() {
        return None;
    }
    if !hidden_action_summaries.is_empty() {
        visible_lines.push(format!(
            "[assistant also emitted MAAP actions; action_count={}]",
            hidden_action_summaries.len()
        ));
        visible_lines.extend(
            hidden_action_summaries
                .into_iter()
                .map(|summary| format!("- {summary}")),
        );
    }
    Some(visible_lines.join("\n"))
}

/// Detects provider text that is the MAAP envelope itself rather than
/// conversational assistant output. Such text can contain inline file content
/// and should be summarized before it enters durable transcript storage.
fn assistant_raw_text_looks_like_maap_payload(value: &str) -> bool {
    let mut candidate = value.trim();
    for marker in ["\nmaap_validation_error:", "\nprovider_error:"] {
        if let Some((prefix, _)) = candidate.split_once(marker) {
            candidate = prefix.trim();
        }
    }
    if candidate.starts_with("```")
        && let Some((_, rest)) = candidate.split_once('\n')
        && let Some((body, _)) = rest.rsplit_once("```")
    {
        candidate = body.trim();
    }
    if candidate.starts_with(r#"{"actions""#)
        || (candidate.starts_with(r#"{"rationale""#) && candidate.contains(r#""actions""#))
        || candidate.starts_with(r#"{"action_batch""#)
        || (candidate.contains(r#""protocol":"maap/1""#) && candidate.contains(r#""actions""#))
    {
        return true;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(candidate) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    object
        .get("actions")
        .is_some_and(serde_json::Value::is_array)
        || object.contains_key("action_batch")
        || (object
            .get("protocol")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|protocol| protocol == "maap/1")
            && object.contains_key("actions"))
}

/// Summarizes one MAAP action for transcript storage while omitting full
/// command bodies and inline file content.
fn assistant_transcript_action_summary(action: &AgentAction) -> String {
    match &action.payload {
        AgentActionPayload::Say { text, .. } => {
            format!("say text={}", bounded_transcript_field(text))
        }
        AgentActionPayload::RequestCapability { capability, reason } => format!(
            "request_capability capability={} reason={}",
            capability.as_str(),
            bounded_transcript_field(reason)
        ),
        AgentActionPayload::RequestSkills => "request_skills".to_string(),
        AgentActionPayload::CallSkill {
            name,
            additional_context,
        } => format!(
            "call_skill name={} additional_context_bytes={}",
            bounded_transcript_field(name),
            additional_context.as_deref().map(str::len).unwrap_or(0)
        ),
        AgentActionPayload::ShellCommand {
            summary, command, ..
        } => format!(
            "shell_command summary={} command_bytes={}",
            bounded_transcript_field(summary),
            command.len()
        ),
        AgentActionPayload::ApplyPatch { patch, .. } => {
            format!("apply_patch patch_bytes={}", patch.len())
        }
        AgentActionPayload::WebSearch { query, .. } => {
            format!("web_search query={}", bounded_transcript_field(query))
        }
        AgentActionPayload::FetchUrl { url, .. } => {
            format!("fetch_url url={}", bounded_transcript_field(url))
        }
        AgentActionPayload::SendMessage {
            recipient, payload, ..
        } => format!(
            "send_message recipient={} payload_bytes={}",
            bounded_transcript_field(recipient),
            payload.len()
        ),
        AgentActionPayload::SpawnAgent {
            role, task_prompt, ..
        } => format!(
            "spawn_agent role={} task_bytes={}",
            bounded_transcript_field(role),
            task_prompt.len()
        ),
        AgentActionPayload::ConfigChange {
            setting_path,
            operation,
            value,
        } => format!(
            "config_change operation={} setting={} value_bytes={}",
            bounded_transcript_field(operation),
            bounded_transcript_field(setting_path),
            value.as_deref().map(str::len).unwrap_or(0)
        ),
        AgentActionPayload::McpCall {
            server,
            tool,
            arguments_json,
        } => format!(
            "mcp_call tool={}/{} argument_bytes={}",
            bounded_transcript_field(server),
            bounded_transcript_field(tool),
            arguments_json.len()
        ),
        AgentActionPayload::Complete => "complete".to_string(),
        AgentActionPayload::Abort { reason } => {
            format!("abort reason={}", bounded_transcript_field(reason))
        }
    }
}

/// Keeps transcript action summaries compact when action labels or paths are
/// unusually long.
fn bounded_transcript_field(value: &str) -> String {
    const MAX_TRANSCRIPT_FIELD_CHARS: usize = 160;
    let mut text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return "(empty)".to_string();
    }
    for (chars, (index, _)) in text.char_indices().enumerate() {
        if chars == MAX_TRANSCRIPT_FIELD_CHARS {
            text.truncate(index);
            text.push_str("...");
            break;
        }
    }
    text
}

/// Append a completed bounded turn execution to the durable transcript store.
pub fn persist_turn_execution_transcript(
    store: &AgentTranscriptStore,
    conversation_id: &str,
    created_at_unix_seconds: u64,
    turn: &AgentTurnRecord,
    execution: &AgentTurnExecution,
) -> Result<Vec<TranscriptEntry>> {
    let first_sequence = next_transcript_sequence(store, conversation_id)?;
    let entries = transcript_entries_for_execution(
        conversation_id,
        first_sequence,
        created_at_unix_seconds,
        turn,
        execution,
    )?;
    for entry in &entries {
        store.append(entry)?;
    }
    Ok(entries)
}

/// Executes the `next_transcript_sequence` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn next_transcript_sequence(
    store: &AgentTranscriptStore,
    conversation_id: &str,
) -> Result<u64> {
    match store.next_sequence(conversation_id) {
        Ok(sequence) => Ok(sequence),
        Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Ok(1),
        Err(error) => Err(error),
    }
}

/// Executes the `action_result_transcript_content` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn action_result_transcript_content(result: &ActionResult) -> String {
    let mut content = format!(
        "action_id={} action_type={} status={:?}",
        result.action_id, result.action_type, result.status
    );
    if matches!(result.action_type, "request_skills" | "call_skill") {
        if let Some(summary) = skill_action_result_transcript_summary(result) {
            content.push_str("\nskill_action_summary:\n");
            content.push_str(&summary);
        }
        if let Some(error) = &result.error {
            content.push_str("\nerror:");
            content.push_str(&error.code);
            content.push(' ');
            content.push_str(&error.message);
        }
        return content;
    }
    if !result.content.is_empty() {
        content.push_str("\ncontent:\n");
        content.push_str(&result.content_text());
    }
    if let Some(data) = &result.structured_content_json {
        content.push_str("\nstructured_content:\n");
        content.push_str(data);
    }
    if let Some(error) = &result.error {
        content.push_str("\nerror:");
        content.push_str(&error.code);
        content.push(' ');
        content.push_str(&error.message);
    }
    content
}

/// Builds a compact durable summary for non-effecting skill actions.
///
/// Skill result bodies can contain complete `SKILL.md` documents or catalogs.
/// Durable transcript storage keeps only metadata that helps audit what
/// happened without allowing those workflow instructions to become future
/// model prompt context.
fn skill_action_result_transcript_summary(result: &ActionResult) -> Option<String> {
    match result.action_type {
        "request_skills" => skill_catalog_result_transcript_summary(result),
        "call_skill" => called_skill_result_transcript_summary(result),
        _ => None,
    }
}

/// Summarizes a skill-catalog action result without copying descriptions.
fn skill_catalog_result_transcript_summary(result: &ActionResult) -> Option<String> {
    let data = result.structured_content_json.as_deref()?;
    let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let skills = value
        .get("skills")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let diagnostics = value
        .get("diagnostics")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let names = skills
        .iter()
        .filter_map(|skill| skill.get("name").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    let mut lines = vec![format!(
        "skills={} diagnostics={}",
        names.len(),
        diagnostics
    )];
    if !names.is_empty() {
        lines.push(format!("names={}", names.join(",")));
    }
    Some(lines.join("\n"))
}

/// Summarizes a loaded skill result without copying the skill body.
fn called_skill_result_transcript_summary(result: &ActionResult) -> Option<String> {
    let data = result.structured_content_json.as_deref()?;
    let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let object = value.as_object()?;
    let mut fields = Vec::new();
    for key in [
        "name",
        "source",
        "path",
        "skill_bytes",
        "additional_context_bytes",
    ] {
        let Some(value) = object.get(key) else {
            continue;
        };
        if let Some(text) = json_scalar_context_text(value) {
            fields.push(format!("{key}={text}"));
        }
    }
    (!fields.is_empty()).then(|| fields.join("\n"))
}

/// Executes the `action_result_context_content` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn action_result_context_content(result: &ActionResult) -> String {
    let mut lines = vec![format!(
        "[action_result {} {} {}]",
        result.action_id,
        result.action_type,
        action_status_context_name(result.status)
    )];
    if let Some(error) = &result.error {
        lines.push(format!("error: {} {}", error.code, error.message));
        if let Some(data) = error
            .data_json
            .as_deref()
            .and_then(model_error_json_text_for_context)
        {
            lines.push(format!("error_data: {data}"));
        }
    }
    if action_result_has_shell_observation(result) {
        append_shell_action_result_context(result, &mut lines);
    } else {
        append_action_result_content_text(result, &mut lines);
        if let Some(data) = result
            .structured_content_json
            .as_deref()
            .and_then(model_structured_json_text_for_context)
        {
            lines.push(format!("data: {data}"));
        }
    }
    lines.join("\n")
}

/// Returns true when a result carries pane shell transaction observation data.
fn action_result_has_shell_observation(result: &ActionResult) -> bool {
    result
        .structured_content_json
        .as_deref()
        .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .and_then(|value| value.as_object().cloned())
        .is_some_and(|object| {
            object.contains_key("command") && object.contains_key("terminal_observation")
        })
}

/// Returns the compact lowercase status name used in model-facing result
/// context.
fn action_status_context_name(status: ActionStatus) -> &'static str {
    match status {
        ActionStatus::Rejected => "rejected",
        ActionStatus::Blocked => "blocked",
        ActionStatus::Denied => "denied",
        ActionStatus::Running => "running",
        ActionStatus::Succeeded => "succeeded",
        ActionStatus::Failed => "failed",
        ActionStatus::Cancelled => "cancelled",
        ActionStatus::TimedOut => "timed_out",
        ActionStatus::Interrupted => "interrupted",
    }
}

/// Appends compact shell-result context for the next provider turn.
fn append_shell_action_result_context(result: &ActionResult, lines: &mut Vec<String>) {
    let structured = result
        .structured_content_json
        .as_deref()
        .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok());
    let structured_object = structured.as_ref().and_then(serde_json::Value::as_object);
    if let Some(command) = structured_object
        .and_then(|object| object.get("command"))
        .and_then(serde_json::Value::as_str)
        .filter(|command| !command.trim().is_empty())
    {
        lines.push(format!("command: {command}"));
    }
    let terminal_observation = structured_object
        .and_then(|object| object.get("terminal_observation"))
        .and_then(serde_json::Value::as_object);
    if let Some(observation) = terminal_observation {
        append_json_scalar_line(lines, "exit_code", observation.get("exit_code"));
        append_json_scalar_line(lines, "signal", observation.get("signal"));
        append_true_bool_line(lines, "timed_out", observation.get("timed_out"));
        append_true_bool_line(lines, "interrupted", observation.get("interrupted"));
        append_true_bool_line(
            lines,
            "output_truncated",
            observation.get("output_truncated"),
        );
    }
    let output = shell_action_result_output_for_context(result, terminal_observation);
    if !output.trim().is_empty() {
        lines.push("output:".to_string());
        let command = structured_object
            .and_then(|object| object.get("command"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        lines.push(compact_shell_output_for_context(&output, command));
    }
    if structured.is_some() {
        return;
    }
    append_action_result_content_text(result, lines);
}

/// Removes Mezzanine-owned shell wrapper echo from model-facing output when the
/// runtime observation still contains shell repaint or wrapper lines.
fn compact_shell_output_for_context(output: &str, command: &str) -> String {
    let command = command.trim();
    let mut cleaned = String::new();
    let normalized = output.replace("\r\n", "\n").replace('\r', "\n");
    for line in normalized.split_inclusive('\n') {
        let (line, had_newline) = line
            .strip_suffix('\n')
            .map(|line| (line, true))
            .unwrap_or((line, false));
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if had_newline && !cleaned.is_empty() && !cleaned.ends_with('\n') {
                cleaned.push('\n');
            }
            continue;
        }
        if !command.is_empty() && shell_output_line_is_echoed_command(line, command) {
            continue;
        }
        if shell_output_line_is_mezzanine_wrapper(trimmed) {
            continue;
        }
        cleaned.push_str(line);
        if had_newline {
            cleaned.push('\n');
        }
    }
    cleaned
}

/// Returns true when a line is known Mezzanine wrapper traffic rather than user
/// command output.
fn shell_output_line_is_mezzanine_wrapper(trimmed: &str) -> bool {
    [
        "MEZ_MARKER_TOKEN",
        "MEZ_TURN",
        "MEZ_AGENT",
        "MEZ_PANE",
        "MEZ_STATUS",
        "MEZ_STTY_STATE",
        "MEZ_RESTORE_",
        "MEZ_HISTORY_",
        "HISTFILE=/dev/null",
        "MEZ_COMMAND_",
        "MEZ_OUTPUT_FILE",
        "__MEZ_SHELL_OUTPUT_BASE64_",
        "mez_marker=",
        "printf '\\033]133",
        "env -u MEZ_MARKER_TOKEN",
        "__mez_tx_",
        "unset -f __mez_tx_",
        "stty -",
        "unset MEZ_",
        "set +o history",
        "set -o history",
        "history -d",
    ]
    .iter()
    .any(|marker| trimmed.contains(marker))
}

/// Returns true when a line is the shell echo of the executed command.
fn shell_output_line_is_echoed_command(line: &str, command: &str) -> bool {
    let mut remaining = line.trim_start();
    if remaining.trim() == command {
        return true;
    }
    loop {
        if let Some(next) = remaining.strip_prefix("$ ") {
            remaining = next.trim_start();
            if remaining.trim() == command {
                return true;
            }
            continue;
        }
        if let Some(next) = remaining.strip_prefix("> ") {
            remaining = next.trim_start();
            if remaining.trim() == command {
                return true;
            }
            continue;
        }
        return false;
    }
}

/// Appends non-empty model-readable result text.
fn append_action_result_content_text(result: &ActionResult, lines: &mut Vec<String>) {
    let mut content = result.content_text();
    if !content.trim().is_empty() {
        if truncate_string_to_max_bytes(&mut content, MODEL_ACTION_RESULT_CONTENT_LIMIT_BYTES) {
            append_truncation_notice(&mut content, MODEL_ACTION_RESULT_CONTENT_LIMIT_BYTES);
        }
        lines.push("content:".to_string());
        lines.push(content);
    }
}

/// Truncates one UTF-8 string to the requested byte ceiling.
///
/// # Parameters
/// - `text`: The string to truncate in place.
/// - `max_bytes`: The maximum retained byte length.
fn truncate_string_to_max_bytes(text: &mut String, max_bytes: u64) -> bool {
    let Ok(limit) = usize::try_from(max_bytes) else {
        return false;
    };
    if text.len() <= limit {
        return false;
    }
    let mut boundary = limit;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    true
}

/// Appends a compact truncation notice to model-readable action content.
///
/// # Parameters
/// - `text`: The string receiving the notice.
/// - `max_bytes`: The byte ceiling that caused truncation.
fn append_truncation_notice(text: &mut String, max_bytes: u64) {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "[mez: action result content truncated after {max_bytes} bytes]"
    ));
}

/// Selects the shell output text worth returning to the model.
fn shell_action_result_output_for_context(
    result: &ActionResult,
    terminal_observation: Option<&serde_json::Map<String, serde_json::Value>>,
) -> String {
    let content = result.content_text();
    if !content.trim().is_empty() && !shell_result_content_is_generic_status(&content) {
        return content;
    }
    terminal_observation
        .and_then(|observation| observation.get("combined_output_preview"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Returns true when shell result content only restates status already carried
/// by the compact header and observation fields.
fn shell_result_content_is_generic_status(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed == "shell command accepted for pane execution"
        || trimmed.starts_with("shell command exited with status ")
        || trimmed == "shell command timed out"
        || trimmed == "shell command was interrupted"
}

/// Appends a scalar JSON field using a compact `key: value` representation.
fn append_json_scalar_line(
    lines: &mut Vec<String>,
    label: &str,
    value: Option<&serde_json::Value>,
) {
    let Some(value) = value else {
        return;
    };
    if value.is_null() {
        return;
    }
    if let Some(text) = json_scalar_context_text(value) {
        lines.push(format!("{label}: {text}"));
    }
}

/// Appends a Boolean field only when true.
fn append_true_bool_line(lines: &mut Vec<String>, label: &str, value: Option<&serde_json::Value>) {
    if value.and_then(serde_json::Value::as_bool) == Some(true) {
        lines.push(format!("{label}: true"));
    }
}

/// Formats scalar JSON values for compact context.
fn json_scalar_context_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

/// Produces model-facing error data after pruning shell/audit internals.
fn model_error_json_text_for_context(value: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(value).ok()?;
    let compact =
        compact_json_value_for_context_with_pruning(&parsed, model_error_json_audit_keys())?;
    serde_json::to_string(&compact).ok()
}

/// Produces model-facing structured result data after pruning audit fields.
fn model_structured_json_text_for_context(value: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(value).ok()?;
    let compact =
        compact_json_value_for_context_with_pruning(&parsed, model_structured_json_audit_keys())?;
    serde_json::to_string(&compact).ok()
}

/// Removes fields that do not add model-usable information and drops keys
/// reserved for audit/debug surfaces.
fn compact_json_value_for_context_with_pruning(
    value: &serde_json::Value,
    pruned_keys: &[&str],
) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(value) if value.is_empty() => None,
        serde_json::Value::Array(values) => {
            let values = values
                .iter()
                .filter_map(|value| compact_json_value_for_context_with_pruning(value, pruned_keys))
                .collect::<Vec<_>>();
            if values.is_empty() {
                None
            } else {
                Some(serde_json::Value::Array(values))
            }
        }
        serde_json::Value::Object(object) => {
            let object = object
                .iter()
                .filter(|(key, _)| !pruned_keys.contains(&key.as_str()))
                .filter_map(|(key, value)| {
                    compact_json_value_for_context_with_pruning(value, pruned_keys)
                        .map(|value| (key.clone(), value))
                })
                .collect::<serde_json::Map<_, _>>();
            if object.is_empty() {
                None
            } else {
                Some(serde_json::Value::Object(object))
            }
        }
        other => Some(other.clone()),
    }
}

/// Audit/debug fields that should never be replayed as model result context.
fn model_structured_json_audit_keys() -> &'static [&'static str] {
    &[
        "approval",
        "matched_rules",
        "sent_to_pane",
        "stateful",
        "policy_command",
        "summary",
        "terminal_observation",
        "generated_command_elided",
        "generated_command_bytes",
    ]
}

/// Error data fields that are useful for audit but encourage prompt bloat or
/// automatic command replay when included in model context.
fn model_error_json_audit_keys() -> &'static [&'static str] {
    &["command"]
}

/// Carries agent turn runner state for this subsystem.
///
/// The fields are kept explicit so callers can inspect and move structured
/// runtime data without parsing display text.
pub struct AgentTurnRunner<'a, P> {
    /// Structured `provider` value carried by this API type.
    pub provider: &'a P,
    /// Structured `model_profile` value carried by this API type.
    pub model_profile: ModelProfile,
    /// Structured `permissions` value carried by this API type.
    pub permissions: &'a PermissionPolicy,
    /// Structured `approvals` value carried by this API type.
    pub approvals: &'a SessionApprovalStore,
    /// Structured `path_scopes` value carried by this API type.
    pub path_scopes: Option<&'a PathScopes>,
    /// Structured `subagent_scope` value carried by this API type.
    pub subagent_scope: Option<&'a SubagentScopeDeclaration>,
    /// Structured `available_mcp_servers` value carried by this API type.
    pub available_mcp_servers: Vec<String>,
    /// Structured `available_mcp_tools` value carried by this API type.
    pub available_mcp_tools: &'a [McpPromptTool],
}

#[cfg(test)]
impl<'a, P: ModelProvider> AgentTurnRunner<'a, P> {
    /// Executes the `run_turn` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub fn run_turn(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: AgentContext,
    ) -> Result<AgentTurnExecution> {
        ledger.start_turn(turn.clone())?;
        let mut request = assemble_model_request(&self.model_profile, &turn, &context)?;
        request.available_mcp_tools = self.available_mcp_tools.to_vec();
        let mut repair_attempts = 0usize;
        let mut capability_attempts = 0usize;
        let mut response_request: ModelRequest;
        let mut durable_response_request = request.clone();
        let mut cumulative_usage = ModelTokenUsage::default();
        let mut latest_response_usage;
        let mut latest_quota_usage = Vec::new();
        let mut response = loop {
            response_request = request.clone();
            let response = match self.provider.send_request(&request) {
                Ok(response) => response,
                Err(error)
                    if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT
                        && maap_provider_error_is_repairable(&error) =>
                {
                    repair_attempts = repair_attempts.saturating_add(1);
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        error.provider_raw_text().unwrap_or(""),
                        repair_attempts,
                    );
                    continue;
                }
                Err(error) => {
                    if provider_error_should_retry_without_summary(&error) {
                        return Err(error);
                    }
                    if let Some(execution) = summarize_provider_failure_execution(
                        self.provider,
                        &turn,
                        &response_request,
                        &error,
                    ) {
                        ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                        return Ok(execution);
                    }
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    return Err(error);
                }
            };
            latest_response_usage = response.usage;
            cumulative_usage.add_assign(latest_response_usage);
            if !response.quota_usage.is_empty() {
                latest_quota_usage = response.quota_usage.clone();
            }
            if response.provider != self.provider.provider_id() {
                let error = MezError::invalid_state(
                    "model provider response identity does not match the selected provider",
                );
                if let Some(execution) = summarize_controller_failure_execution(
                    self.provider,
                    &turn,
                    &response_request,
                    FailureSummaryInput {
                        failed_response: response.clone(),
                        error: &error,
                        scope: FailureSummaryScope {
                            stage: "provider_identity",
                            available_mcp_servers: &self.available_mcp_servers,
                            available_mcp_tools: self.available_mcp_tools,
                        },
                    },
                ) {
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    return Ok(execution);
                }
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                return Err(error);
            }
            if response_request.interaction_kind != ModelInteractionKind::Repair {
                durable_response_request = response_request.clone();
            }
            let Some(batch) = &response.action_batch else {
                break response;
            };
            if let Err(error) = validate_batch_allowed_actions(batch, &request) {
                if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT {
                    repair_attempts = repair_attempts.saturating_add(1);
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        &response.raw_text,
                        repair_attempts,
                    );
                    continue;
                }
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                let mut response = response;
                response.usage = cumulative_usage;
                response.quota_usage = latest_quota_usage;
                return Ok(failed_maap_validation_execution_with_summary(
                    self.provider,
                    &turn,
                    durable_response_request,
                    response,
                    latest_response_usage,
                    &error,
                    FailureSummaryScope {
                        stage: "allowed_actions",
                        available_mcp_servers: &self.available_mcp_servers,
                        available_mcp_tools: self.available_mcp_tools,
                    },
                ));
            }
            if let Err(error) =
                batch.validate(&turn, &self.available_mcp_servers, self.available_mcp_tools)
            {
                if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT {
                    repair_attempts = repair_attempts.saturating_add(1);
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        &response.raw_text,
                        repair_attempts,
                    );
                    continue;
                }
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                let mut response = response;
                response.usage = cumulative_usage;
                response.quota_usage = latest_quota_usage;
                return Ok(failed_maap_validation_execution_with_summary(
                    self.provider,
                    &turn,
                    durable_response_request,
                    response,
                    latest_response_usage,
                    &error,
                    FailureSummaryScope {
                        stage: "maap_validation",
                        available_mcp_servers: &self.available_mcp_servers,
                        available_mcp_tools: self.available_mcp_tools,
                    },
                ));
            }
            let capability_request = match capability_requests_from_batch(batch) {
                Ok(capability_request) => capability_request,
                Err(error) => {
                    if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT {
                        repair_attempts = repair_attempts.saturating_add(1);
                        request = maap_repair_request(
                            &response_request,
                            error.message(),
                            &response.raw_text,
                            repair_attempts,
                        );
                        continue;
                    }
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    let mut response = response;
                    response.usage = cumulative_usage;
                    response.quota_usage = latest_quota_usage;
                    return Ok(failed_maap_validation_execution_with_summary(
                        self.provider,
                        &turn,
                        durable_response_request,
                        response,
                        latest_response_usage,
                        &error,
                        FailureSummaryScope {
                            stage: "capability_negotiation",
                            available_mcp_servers: &self.available_mcp_servers,
                            available_mcp_tools: self.available_mcp_tools,
                        },
                    ));
                }
            };
            if let Some(capability_request) = capability_request {
                if capability_attempts >= CAPABILITY_REQUEST_ATTEMPT_LIMIT {
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    let mut response = response;
                    response.usage = cumulative_usage;
                    response.quota_usage = latest_quota_usage;
                    return Ok(failed_capability_request_execution(
                        response_request,
                        response,
                        latest_response_usage,
                        "capability_request_limit",
                        "model exceeded capability request limit before emitting executable or user-facing output",
                    ));
                }
                capability_attempts = capability_attempts.saturating_add(1);
                request = capability_continuation_request(&response_request, &capability_request);
                repair_attempts = 0;
                continue;
            }
            break response;
        };
        response.usage = cumulative_usage;
        response.quota_usage = latest_quota_usage;

        let Some(batch) = &response.action_batch else {
            ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
            return Ok(AgentTurnExecution {
                request: durable_response_request,
                response,
                latest_response_usage,
                action_results: Vec::new(),
                final_turn: true,
                terminal_state: AgentTurnState::Failed,
            });
        };

        let final_turn = batch.final_turn;
        let mut action_results = Vec::with_capacity(batch.actions.len());
        for action in &batch.actions {
            action_results.push(self.plan_action_result(&turn, action)?);
        }
        let terminal_state = turn_state_from_action_results(&action_results, final_turn);
        if terminal_state != AgentTurnState::Running {
            ledger.finish_turn(&turn.turn_id, terminal_state)?;
        }

        Ok(AgentTurnExecution {
            request: durable_response_request,
            response,
            latest_response_usage,
            action_results,
            final_turn,
            terminal_state,
        })
    }

    /// Executes the `run_turn_with_shell_executor` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub fn run_turn_with_shell_executor<M>(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: AgentContext,
        shell_path: &Path,
        executor: &mut impl PaneShellExecutor,
        mut marker_for_action: M,
    ) -> Result<AgentTurnExecution>
    where
        M: FnMut(&AgentAction) -> Result<MarkerToken>,
    {
        let mut execution = self.run_turn(ledger, turn.clone(), context)?;
        if execution.terminal_state != AgentTurnState::Running {
            return Ok(execution);
        }

        let Some(batch) = &execution.response.action_batch else {
            return Ok(execution);
        };
        for result in &mut execution.action_results {
            if result.status != ActionStatus::Running {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == result.action_id)
                .ok_or_else(|| {
                    MezError::invalid_state("running shell result does not match an action")
                })?;
            if local_action_plan(action)?.is_none() {
                continue;
            }
            let marker = marker_for_action(action)?;
            *result =
                execute_shell_action_through_pane(&turn, action, marker, shell_path, executor)?;
        }

        execution.terminal_state =
            turn_state_from_action_results(&execution.action_results, execution.final_turn);
        if execution.terminal_state != AgentTurnState::Running {
            ledger.finish_turn(&turn.turn_id, execution.terminal_state)?;
        }
        Ok(execution)
    }

    /// Executes the `run_turn_with_mcp_executor` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub fn run_turn_with_mcp_executor<F>(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: AgentContext,
        executor: &mut impl McpActionExecutor,
        mut plan_for_action: F,
    ) -> Result<AgentTurnExecution>
    where
        F: FnMut(&AgentAction) -> Result<McpToolCallPlan>,
    {
        let mut execution = self.run_turn(ledger, turn.clone(), context)?;
        if execution.terminal_state != AgentTurnState::Running {
            return Ok(execution);
        }

        let Some(batch) = &execution.response.action_batch else {
            return Ok(execution);
        };
        for result in &mut execution.action_results {
            if result.status != ActionStatus::Running || result.action_type != "mcp_call" {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == result.action_id)
                .ok_or_else(|| {
                    MezError::invalid_state("running MCP result does not match an action")
                })?;
            let plan = plan_for_action(action)?;
            *result = execute_mcp_action_through_runtime(&turn, action, &plan, executor)?;
        }

        execution.terminal_state =
            turn_state_from_action_results(&execution.action_results, execution.final_turn);
        if execution.terminal_state != AgentTurnState::Running {
            ledger.finish_turn(&turn.turn_id, execution.terminal_state)?;
        }
        Ok(execution)
    }
}

impl<'a, P> AgentTurnRunner<'a, P> {
    /// Executes the `plan_action_result` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub(super) fn plan_action_result(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        let local_plan = local_action_plan(action)?;
        let network_plan = network_action_plan(action)?;
        match &action.payload {
            AgentActionPayload::Say {
                status,
                text,
                content_type,
            } => Ok(ActionResult::succeeded(
                turn,
                action,
                vec![text.clone()],
                Some(say_structured_content_json(*status, content_type, text)),
            )),
            AgentActionPayload::RequestCapability { .. } => Err(MezError::invalid_state(
                "request_capability reached executable action planning",
            )),
            AgentActionPayload::RequestSkills => Ok(ActionResult::running(
                turn,
                action,
                vec!["skill catalog accepted for runtime lookup".to_string()],
                Some(r#"{"state":"pending_runtime_skill_lookup"}"#.to_string()),
            )),
            AgentActionPayload::CallSkill { name, .. } => Ok(ActionResult::running(
                turn,
                action,
                vec![format!("skill {name} accepted for runtime loading")],
                Some(format!(
                    r#"{{"state":"pending_runtime_skill_load","name":"{}"}}"#,
                    json_escape(name)
                )),
            )),
            _ if local_plan.is_some() => {
                let Some(plan) = local_plan.as_ref() else {
                    return Err(MezError::invalid_state(
                        "local action plan was unavailable after local action match",
                    ));
                };
                if self.permissions.approval_policy
                    != crate::permissions::ApprovalPolicy::FullAccess
                    && let Some(scope) = self.subagent_scope
                    && let Some(message) = scope.shell_command_violation(&plan.policy_command)?
                {
                    return ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Denied,
                        "subagent_scope_violation",
                        message,
                    );
                }
                match self
                    .permissions
                    .evaluate_shell_command_with_approvals_scoped(
                        &plan.policy_command,
                        self.approvals,
                        self.path_scopes,
                    ) {
                    RuleDecision::Allow => Ok(ActionResult::running(
                        turn,
                        action,
                        vec!["local action accepted for pane execution".to_string()],
                        Some(shell_command_structured_content_json(
                            action,
                            false,
                            serde_json::Value::Null,
                            &[],
                            serde_json::json!({"state":"pending_dispatch"}),
                        )?),
                    )),
                    RuleDecision::Prompt
                        if self.permissions.approval_policy
                            == crate::permissions::ApprovalPolicy::AutoAllow
                            && action_supports_auto_allow(action) =>
                    {
                        let reason = action_auto_allow_reason(action);
                        Ok(ActionResult::running(
                            turn,
                            action,
                            vec![
                                "local action auto-allowed by model assessment".to_string(),
                                reason,
                            ],
                            Some(shell_command_structured_content_json(
                                action,
                                false,
                                auto_allow_approval_json(action, action.action_type()),
                                &[],
                                serde_json::json!({"state":"pending_dispatch"}),
                            )?),
                        ))
                    }
                    RuleDecision::Prompt => Ok(ActionResult::blocked(
                        turn,
                        action,
                        vec!["approval required before executing local action".to_string()],
                        shell_command_structured_content_json(
                            action,
                            false,
                            serde_json::json!({
                                "state": "pending",
                                "kind": action.action_type(),
                                "action_id": action.id.as_str(),
                                "command": plan.policy_command.as_str()
                            }),
                            &[],
                            serde_json::json!({"state":"pending_approval"}),
                        )?,
                    )),
                    RuleDecision::Forbid => ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Denied,
                        "policy_forbidden",
                        "local action denied by permission policy",
                    ),
                }
            }
            _ if network_plan.is_some() => {
                let Some(plan) = network_plan.as_ref() else {
                    return Err(MezError::invalid_state(
                        "network action plan was unavailable after network action match",
                    ));
                };
                match self
                    .permissions
                    .evaluate_shell_command_with_approvals_scoped(
                        &plan.policy_command,
                        self.approvals,
                        self.path_scopes,
                    ) {
                    RuleDecision::Allow => Ok(ActionResult::running(
                        turn,
                        action,
                        vec!["network action accepted for runtime execution".to_string()],
                        Some(network_action_structured_content_json(
                            action,
                            serde_json::Value::Null,
                            serde_json::json!({"state":"pending_runtime_network"}),
                        )?),
                    )),
                    RuleDecision::Prompt
                        if self.permissions.approval_policy
                            == crate::permissions::ApprovalPolicy::AutoAllow
                            && action_supports_auto_allow(action) =>
                    {
                        let reason = action_auto_allow_reason(action);
                        Ok(ActionResult::running(
                            turn,
                            action,
                            vec![
                                "network action auto-allowed by model assessment".to_string(),
                                reason,
                            ],
                            Some(network_action_structured_content_json(
                                action,
                                auto_allow_approval_json(action, action.action_type()),
                                serde_json::json!({"state":"pending_runtime_network"}),
                            )?),
                        ))
                    }
                    RuleDecision::Prompt => Ok(ActionResult::blocked(
                        turn,
                        action,
                        vec!["approval required before executing network action".to_string()],
                        network_action_structured_content_json(
                            action,
                            serde_json::json!({
                                "state": "pending",
                                "kind": action.action_type(),
                                "action_id": action.id.as_str(),
                                "policy_command": plan.policy_command.as_str()
                            }),
                            serde_json::json!({"state":"pending_approval"}),
                        )?,
                    )),
                    RuleDecision::Forbid => ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Denied,
                        "policy_forbidden",
                        "network action denied by permission policy",
                    ),
                }
            }
            AgentActionPayload::SendMessage {
                recipient,
                content_type,
                payload,
            } => Ok(ActionResult::running(
                turn,
                action,
                vec!["message accepted for local delivery".to_string()],
                Some(format!(
                    r#"{{"recipient":"{}","content_type":"{}","bytes":{},"message_id":null,"delivery_status":"pending_runtime_delivery","protocol_error":null}}"#,
                    json_escape(recipient),
                    json_escape(content_type),
                    payload.len()
                )),
            )),
            AgentActionPayload::SpawnAgent {
                role,
                placement,
                cooperation_mode,
                read_scopes,
                write_scopes,
                task_prompt,
            } => Ok(ActionResult::running(
                turn,
                action,
                vec!["subagent spawn accepted for control endpoint placement".to_string()],
                Some(format!(
                    r#"{{"role":"{}","placement":"{}","cooperation_mode":"{}","read_scopes":{},"write_scopes":{},"prompt_bytes":{}}}"#,
                    json_escape(role),
                    json_escape(placement),
                    json_escape(cooperation_mode),
                    string_array_json(read_scopes),
                    string_array_json(write_scopes),
                    task_prompt.len()
                )),
            )),
            AgentActionPayload::ConfigChange {
                setting_path,
                operation,
                ..
            } => {
                let policy_allowed = action_prompt_gate_satisfied_by_policy(self.permissions);
                let auto_allowed = !policy_allowed
                    && self.permissions.approval_policy
                        == crate::permissions::ApprovalPolicy::AutoAllow
                    && action_supports_auto_allow(action);
                if !policy_allowed && !auto_allowed {
                    return Ok(ActionResult::blocked(
                        turn,
                        action,
                        vec!["approval required before applying configuration change".to_string()],
                        format!(
                            r#"{{"approval":{{"state":"pending","kind":"config_change","path":"{}","operation":"{}","required_command":"/approve"}},"setting_path":"{}","operation":"{}","validation":{{"status":"pending_primary_approval"}},"applied_layer":null,"persistence":{{"requested":true,"completed":false,"scope":"user"}}}}"#,
                            json_escape(setting_path),
                            json_escape(operation),
                            json_escape(setting_path),
                            json_escape(operation)
                        ),
                    ));
                }
                let approval = if auto_allowed {
                    auto_allow_approval_json(action, "config_change")
                } else {
                    action_policy_approval_json(action, "config_change", self.permissions)
                };
                Ok(ActionResult::running(
                    turn,
                    action,
                    vec!["configuration change accepted for runtime application".to_string()],
                    Some(
                        serde_json::json!({
                            "approval": approval,
                            "setting_path": setting_path,
                            "operation": operation,
                            "validation": {"status": "pending_runtime_config_change"},
                            "applied_layer": null,
                            "persistence": {
                                "requested": true,
                                "completed": false,
                                "scope": "user"
                            }
                        })
                        .to_string(),
                    ),
                ))
            }
            AgentActionPayload::McpCall {
                server,
                tool,
                arguments_json,
            } => {
                let approval_required = self.mcp_tool_requires_approval(server, tool);
                let policy_allowed =
                    approval_required && action_prompt_gate_satisfied_by_policy(self.permissions);
                let auto_allowed = approval_required
                    && !policy_allowed
                    && self.permissions.approval_policy
                        == crate::permissions::ApprovalPolicy::AutoAllow
                    && action_supports_auto_allow(action);
                if approval_required && !policy_allowed && !auto_allowed {
                    return Ok(ActionResult::blocked(
                        turn,
                        action,
                        vec!["approval required before executing MCP tool call".to_string()],
                        format!(
                            r#"{{"approval":{{"state":"pending","kind":"mcp_call","action_id":"{}","server":"{}","tool":"{}"}}}}"#,
                            json_escape(&action.id),
                            json_escape(server),
                            json_escape(tool)
                        ),
                    ));
                }
                let auto_allow_reason = action_auto_allow_reason(action);
                Ok(ActionResult::running(
                    turn,
                    action,
                    if auto_allowed {
                        vec![
                            "mcp call auto-allowed by model assessment".to_string(),
                            auto_allow_reason,
                        ]
                    } else if approval_required {
                        vec!["mcp call accepted by approval policy".to_string()]
                    } else {
                        vec!["mcp call accepted for external-integration execution".to_string()]
                    },
                    Some(format!(
                        r#"{{"server":"{}","tool":"{}","arguments":{},"approval_required":{},"approval":{}}}"#,
                        json_escape(server),
                        json_escape(tool),
                        arguments_json,
                        approval_required,
                        if auto_allowed {
                            auto_allow_approval_json(action, "mcp_call").to_string()
                        } else if approval_required {
                            action_policy_approval_json(action, "mcp_call", self.permissions)
                                .to_string()
                        } else {
                            "null".to_string()
                        }
                    )),
                ))
            }
            AgentActionPayload::Complete => Ok(ActionResult::succeeded(
                turn,
                action,
                vec!["turn complete".to_string()],
                Some(r#"{"complete":true}"#.to_string()),
            )),
            AgentActionPayload::Abort { reason } => ActionResult::failed(
                turn,
                action,
                ActionStatus::Cancelled,
                "agent_aborted",
                reason,
            ),
            _ => Err(MezError::invalid_state(
                "shell-backed action was not planned before action-result planning",
            )),
        }
    }

    /// Executes the `mcp_tool_requires_approval` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub(super) fn mcp_tool_requires_approval(&self, server: &str, tool: &str) -> bool {
        self.available_mcp_tools
            .iter()
            .find(|available| available.server_id == server && available.tool_name == tool)
            .map(|available| available.approval_required)
            .unwrap_or(true)
    }
}

impl<'a, P: AsyncModelProvider> AgentTurnRunner<'a, P> {
    /// Executes the `run_turn_async` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub async fn run_turn_async(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: AgentContext,
    ) -> Result<AgentTurnExecution> {
        ledger.start_turn(turn.clone())?;
        let mut request = assemble_model_request(&self.model_profile, &turn, &context)?;
        request.available_mcp_tools = self.available_mcp_tools.to_vec();
        let mut repair_attempts = 0usize;
        let mut capability_attempts = 0usize;
        let mut response_request: ModelRequest;
        let mut durable_response_request = request.clone();
        let mut cumulative_usage = ModelTokenUsage::default();
        let mut latest_response_usage;
        let mut latest_quota_usage = Vec::new();
        let mut response = loop {
            response_request = request.clone();
            let response = match self.provider.send_request_async(&request).await {
                Ok(response) => response,
                Err(error)
                    if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT
                        && maap_provider_error_is_repairable(&error) =>
                {
                    repair_attempts = repair_attempts.saturating_add(1);
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        error.provider_raw_text().unwrap_or(""),
                        repair_attempts,
                    );
                    continue;
                }
                Err(error) => {
                    if provider_error_should_retry_without_summary(&error) {
                        return Err(error);
                    }
                    if let Some(execution) = summarize_provider_failure_execution_async(
                        self.provider,
                        &turn,
                        &response_request,
                        &error,
                    )
                    .await
                    {
                        ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                        return Ok(execution);
                    }
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    return Err(error);
                }
            };
            latest_response_usage = response.usage;
            cumulative_usage.add_assign(latest_response_usage);
            if !response.quota_usage.is_empty() {
                latest_quota_usage = response.quota_usage.clone();
            }
            if response.provider != self.provider.provider_id() {
                let error = MezError::invalid_state(
                    "model provider response identity does not match the selected provider",
                );
                if let Some(execution) = summarize_controller_failure_execution_async(
                    self.provider,
                    &turn,
                    &response_request,
                    FailureSummaryInput {
                        failed_response: response.clone(),
                        error: &error,
                        scope: FailureSummaryScope {
                            stage: "provider_identity",
                            available_mcp_servers: &self.available_mcp_servers,
                            available_mcp_tools: self.available_mcp_tools,
                        },
                    },
                )
                .await
                {
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    return Ok(execution);
                }
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                return Err(error);
            }
            if response_request.interaction_kind != ModelInteractionKind::Repair {
                durable_response_request = response_request.clone();
            }
            let Some(batch) = &response.action_batch else {
                break response;
            };
            if let Err(error) = validate_batch_allowed_actions(batch, &request) {
                if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT {
                    repair_attempts = repair_attempts.saturating_add(1);
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        &response.raw_text,
                        repair_attempts,
                    );
                    continue;
                }
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                let mut response = response;
                response.usage = cumulative_usage;
                response.quota_usage = latest_quota_usage;
                return Ok(failed_maap_validation_execution_with_summary_async(
                    self.provider,
                    &turn,
                    durable_response_request,
                    response,
                    latest_response_usage,
                    &error,
                    FailureSummaryScope {
                        stage: "allowed_actions",
                        available_mcp_servers: &self.available_mcp_servers,
                        available_mcp_tools: self.available_mcp_tools,
                    },
                )
                .await);
            }
            if let Err(error) =
                batch.validate(&turn, &self.available_mcp_servers, self.available_mcp_tools)
            {
                if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT {
                    repair_attempts = repair_attempts.saturating_add(1);
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        &response.raw_text,
                        repair_attempts,
                    );
                    continue;
                }
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                let mut response = response;
                response.usage = cumulative_usage;
                response.quota_usage = latest_quota_usage;
                return Ok(failed_maap_validation_execution_with_summary_async(
                    self.provider,
                    &turn,
                    durable_response_request,
                    response,
                    latest_response_usage,
                    &error,
                    FailureSummaryScope {
                        stage: "maap_validation",
                        available_mcp_servers: &self.available_mcp_servers,
                        available_mcp_tools: self.available_mcp_tools,
                    },
                )
                .await);
            }
            let capability_request = match capability_requests_from_batch(batch) {
                Ok(capability_request) => capability_request,
                Err(error) => {
                    if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT {
                        repair_attempts = repair_attempts.saturating_add(1);
                        request = maap_repair_request(
                            &response_request,
                            error.message(),
                            &response.raw_text,
                            repair_attempts,
                        );
                        continue;
                    }
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    let mut response = response;
                    response.usage = cumulative_usage;
                    response.quota_usage = latest_quota_usage;
                    return Ok(failed_maap_validation_execution_with_summary_async(
                        self.provider,
                        &turn,
                        durable_response_request,
                        response,
                        latest_response_usage,
                        &error,
                        FailureSummaryScope {
                            stage: "capability_negotiation",
                            available_mcp_servers: &self.available_mcp_servers,
                            available_mcp_tools: self.available_mcp_tools,
                        },
                    )
                    .await);
                }
            };
            if let Some(capability_request) = capability_request {
                if capability_attempts >= CAPABILITY_REQUEST_ATTEMPT_LIMIT {
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    let mut response = response;
                    response.usage = cumulative_usage;
                    response.quota_usage = latest_quota_usage;
                    return Ok(failed_capability_request_execution(
                        response_request,
                        response,
                        latest_response_usage,
                        "capability_request_limit",
                        "model exceeded capability request limit before emitting executable or user-facing output",
                    ));
                }
                capability_attempts = capability_attempts.saturating_add(1);
                request = capability_continuation_request(&response_request, &capability_request);
                repair_attempts = 0;
                continue;
            }
            break response;
        };
        response.usage = cumulative_usage;
        response.quota_usage = latest_quota_usage;

        let Some(batch) = &response.action_batch else {
            ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
            return Ok(AgentTurnExecution {
                request: durable_response_request,
                response,
                latest_response_usage,
                action_results: Vec::new(),
                final_turn: true,
                terminal_state: AgentTurnState::Failed,
            });
        };

        let final_turn = batch.final_turn;
        let mut action_results = Vec::with_capacity(batch.actions.len());
        for action in &batch.actions {
            action_results.push(self.plan_action_result(&turn, action)?);
        }
        let terminal_state = turn_state_from_action_results(&action_results, final_turn);
        if terminal_state != AgentTurnState::Running {
            ledger.finish_turn(&turn.turn_id, terminal_state)?;
        }

        Ok(AgentTurnExecution {
            request: durable_response_request,
            response,
            latest_response_usage,
            action_results,
            final_turn,
            terminal_state,
        })
    }
}

/// Validates that the provider emitted only actions exposed in the active
/// request schema.
fn validate_batch_allowed_actions(batch: &super::MaapBatch, request: &ModelRequest) -> Result<()> {
    for action in &batch.actions {
        if matches!(action.payload, AgentActionPayload::Complete) {
            continue;
        }
        let action_type = action.action_type();
        let Some(allowed_action) = AllowedAction::from_action_type(action_type) else {
            return Err(MezError::invalid_args(format!(
                "maap action type {action_type} is not part of the provider action surface"
            )));
        };
        if !request.allowed_actions.contains(allowed_action) {
            return Err(MezError::invalid_args(format!(
                "maap action type {action_type} is not allowed during {} interaction",
                request.interaction_kind.as_str()
            )));
        }
    }
    Ok(())
}

/// One model-requested coarse capability and its task-specific reason.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CapabilityRequest {
    /// Requested coarse capability.
    capability: AgentCapability,
    /// Model-authored reason for exposing the capability.
    reason: String,
}

/// Extracts capability requests from a non-executing capability negotiation.
///
/// A provider schema may allow the model to include a visible `say` alongside
/// one or more `request_capability` actions during the initial capability
/// decision or later action execution. Treat the batch as one combined
/// capability decision, but reject mixed executable or blocking work so the
/// controller can update the action surface before any effects run.
fn capability_requests_from_batch(
    batch: &super::MaapBatch,
) -> Result<Option<Vec<CapabilityRequest>>> {
    let mut requests = Vec::new();
    let mut incompatible_actions = Vec::new();
    for action in &batch.actions {
        match &action.payload {
            AgentActionPayload::RequestCapability { capability, reason } => {
                requests.push(CapabilityRequest {
                    capability: *capability,
                    reason: reason.clone(),
                });
            }
            AgentActionPayload::Say { .. } => {}
            _ => incompatible_actions.push(action.action_type().to_string()),
        }
    }
    if requests.is_empty() {
        return Ok(None);
    }
    if !incompatible_actions.is_empty() {
        let action_list = incompatible_actions.join(",");
        return Err(MezError::invalid_args(format!(
            "request_capability may only be combined with say actions; incompatible actions: {action_list}"
        )));
    }
    Ok(Some(requests))
}

/// Builds the next provider request after a non-executing capability request.
fn capability_continuation_request(
    previous_request: &ModelRequest,
    requests: &[CapabilityRequest],
) -> ModelRequest {
    let decisions = requests
        .iter()
        .map(|request| {
            (
                request,
                capability_decision(previous_request, request.capability),
            )
        })
        .collect::<Vec<_>>();
    let carried_execution_surface =
        previous_request.interaction_kind == ModelInteractionKind::ActionExecution;
    let mut allowed_actions = if carried_execution_surface {
        previous_request.allowed_actions.clone()
    } else {
        AllowedActionSet::action_execution_base()
    };
    for (_, decision) in &decisions {
        if decision.granted {
            allowed_actions.extend_set(&decision.allowed_actions);
        }
    }
    let granted_any = decisions.iter().any(|(_, decision)| decision.granted);
    let mut request = previous_request.clone();
    request.interaction_kind = if granted_any || carried_execution_surface {
        ModelInteractionKind::ActionExecution
    } else {
        ModelInteractionKind::CapabilityDecision
    };
    request.allowed_actions = if granted_any || carried_execution_surface {
        allowed_actions
    } else {
        AllowedActionSet::capability_decision()
    };
    constrain_skill_actions_for_loaded_context(&mut request);
    let content = if decisions.len() == 1 {
        let (capability_request, decision) = &decisions[0];
        format!(
            "[capability {}]\ncapability={}\nreason={}\ncontroller_reason={}\nallowed_actions={}",
            if decision.granted {
                "granted"
            } else {
                "denied"
            },
            capability_request.capability.as_str(),
            capability_request.reason.as_str(),
            decision.reason.as_str(),
            request.allowed_actions.action_type_names().join(",")
        )
    } else {
        let mut lines = vec!["[capability decisions]".to_string()];
        for (capability_request, decision) in &decisions {
            lines.push(format!(
                "- capability={} decision={} reason={} controller_reason={}",
                capability_request.capability.as_str(),
                if decision.granted {
                    "granted"
                } else {
                    "denied"
                },
                capability_request.reason.as_str(),
                decision.reason.as_str()
            ));
        }
        lines.push(format!(
            "allowed_actions={}",
            request.allowed_actions.action_type_names().join(",")
        ));
        lines.join("\n")
    };
    request.messages.push(ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::DeveloperInstruction,
        content,
    });
    request
}

/// Controller decision for a requested capability.
struct CapabilityDecision {
    /// Whether the capability was granted.
    granted: bool,
    /// Action set for the next request.
    allowed_actions: AllowedActionSet,
    /// Deterministic controller reason.
    reason: String,
}

/// Grants or denies a coarse capability with deterministic policy checks.
///
/// This deliberately does not try to solve the task or validate the eventual
/// concrete action target. Target validation belongs to the action parser,
/// permission layer, and action executor after the capability-specific action
/// surface has been exposed.
fn capability_decision(request: &ModelRequest, capability: AgentCapability) -> CapabilityDecision {
    match capability {
        AgentCapability::Mcp if request.available_mcp_tools.is_empty() => CapabilityDecision {
            granted: false,
            allowed_actions: AllowedActionSet::capability_decision(),
            reason: "mcp capability requires at least one available MCP tool in runtime context"
                .to_string(),
        },
        _ => CapabilityDecision {
            granted: true,
            allowed_actions: AllowedActionSet::for_capability(capability),
            reason: "capability is permitted by deterministic action-surface rules".to_string(),
        },
    }
}

/// Builds a failed execution for capability-negotiation failures that happen
/// before executable actions are available.
fn failed_capability_request_execution(
    request: ModelRequest,
    mut response: ModelResponse,
    latest_response_usage: ModelTokenUsage,
    code: &str,
    message: &str,
) -> AgentTurnExecution {
    let turn_id = request.turn_id.clone();
    let agent_id = request.agent_id.clone();
    let original_batch = response.action_batch.as_ref();
    let mut actions = original_batch
        .map(|batch| {
            batch
                .actions
                .iter()
                .filter(|action| {
                    matches!(action.payload, AgentActionPayload::RequestCapability { .. })
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if actions.is_empty() {
        actions.push(AgentAction {
            id: "capability-request".to_string(),
            rationale: message.to_string(),
            payload: AgentActionPayload::RequestCapability {
                capability: AgentCapability::Shell,
                reason: message.to_string(),
            },
        });
    }
    let terminal_batch = MaapBatch {
        protocol: original_batch
            .map(|batch| batch.protocol.clone())
            .unwrap_or_else(|| "maap/1".to_string()),
        rationale: original_batch
            .map(|batch| batch.rationale.clone())
            .filter(|rationale| !rationale.trim().is_empty())
            .unwrap_or_else(|| "capability request failed before execution".to_string()),
        thought: original_batch.and_then(|batch| batch.thought.clone()),
        turn_id: turn_id.clone(),
        agent_id: agent_id.clone(),
        actions,
        final_turn: true,
    };
    let action_results = terminal_batch
        .actions
        .iter()
        .map(|action| ActionResult {
            protocol: "maap/1".to_string(),
            turn_id: turn_id.clone(),
            agent_id: agent_id.clone(),
            action_id: action.id.clone(),
            action_type: action.action_type(),
            status: ActionStatus::Failed,
            content: action_text_content_blocks(vec![message.to_string()]),
            structured_content_json: Some(format!(
                r#"{{"kind":"request_capability","status":"failed","code":"{}","message":"{}"}}"#,
                json_escape(code),
                json_escape(message)
            )),
            is_error: true,
            error: Some(super::ActionError {
                code: code.to_string(),
                message: message.to_string(),
                data_json: None,
            }),
        })
        .collect();
    response.action_batch = Some(terminal_batch);
    AgentTurnExecution {
        request,
        response,
        latest_response_usage,
        action_results,
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    }
}

/// Builds an ephemeral provider retry request that asks the model to repair its
/// previous MAAP response without adding the repair instruction to transcript
/// state or future model context.
fn maap_repair_request(
    original_request: &ModelRequest,
    error_message: &str,
    raw_text: &str,
    attempt: usize,
) -> ModelRequest {
    let mut request = original_request.clone();
    request.interaction_kind = ModelInteractionKind::Repair;
    request.messages.push(ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::Configuration,
        content: format!(
            "[ephemeral maap repair]\n\
             The previous provider response failed Mezzanine MAAP validation before any action was executed. \
             Return exactly one corrected maap/1 action batch for the same turn_id={} and agent_id={}. \
             Do not mention this repair instruction to the user. \
             This repair instruction is not durable transcript or future-turn context.\n\
             attempt={attempt} validation_error={}\n\
             previous_response_excerpt:\n{}",
            original_request.turn_id,
            original_request.agent_id,
            error_message,
            maap_repair_raw_text_excerpt(raw_text)
        ),
    });
    request
}

/// Returns a bounded UTF-8-safe excerpt of invalid provider MAAP output for an
/// ephemeral repair retry.
fn maap_repair_raw_text_excerpt(raw_text: &str) -> String {
    if raw_text.len() <= MAAP_REPAIR_RAW_TEXT_LIMIT_BYTES {
        return raw_text.to_string();
    }
    let mut end = MAAP_REPAIR_RAW_TEXT_LIMIT_BYTES;
    while !raw_text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}\n[truncated: original_bytes={}]",
        &raw_text[..end],
        raw_text.len()
    )
}

/// Reports whether a provider error came from malformed model MAAP output that
/// can be repaired by asking the same model to re-emit the action batch.
fn maap_provider_error_is_repairable(error: &MezError) -> bool {
    error.provider_raw_text().is_some()
        && error
            .message()
            .starts_with("provider MAAP output is malformed:")
}

/// Builds the terminal failed execution for a provider error when a final model
/// summary could not be obtained.
fn failed_provider_error_execution(
    request: ModelRequest,
    provider_id: &str,
    model: &str,
    error: &MezError,
) -> AgentTurnExecution {
    AgentTurnExecution {
        request,
        response: ModelResponse {
            provider: provider_id.to_string(),
            model: model.to_string(),
            raw_text: provider_error_raw_text(error),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: None,
        },
        latest_response_usage: Default::default(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    }
}

/// Formats provider error detail for durable failed-turn output.
fn provider_error_raw_text(error: &MezError) -> String {
    match error.provider_raw_text() {
        Some(raw_text) => format!("{raw_text}\nprovider_error: {error}"),
        None => format!("provider_error: {error}"),
    }
}

/// Reports whether a provider error should remain visible to runtime retry
/// handling instead of being converted into a terminal failure summary.
fn provider_error_should_retry_without_summary(error: &MezError) -> bool {
    if provider_error_is_context_limit_exceeded(error.message(), error.provider_failure_json()) {
        return true;
    }
    if provider_error_is_output_limit_exceeded(error.message(), error.provider_failure_json()) {
        return true;
    }
    if let Some(status_code) = provider_failure_status_code(error.provider_failure_json()) {
        if status_code == 400
            && (error.message().contains("Unsupported") || error.message().contains("unsupported"))
        {
            return false;
        }
        return status_code == 429 || (500..=599).contains(&status_code);
    }
    if error.kind() == crate::error::MezErrorKind::Io {
        return true;
    }
    if error.kind() != crate::error::MezErrorKind::InvalidState {
        return false;
    }
    let message = error.message();
    message.contains("provider HTTP request failed")
        || message.contains("provider HTTP response read failed")
        || provider_error_invites_retry(message, error.provider_failure_json())
}

/// Extracts an HTTP status code from provider failure diagnostics.
fn provider_failure_status_code(provider_failure_json: Option<&str>) -> Option<u16> {
    let value: serde_json::Value = serde_json::from_str(provider_failure_json?).ok()?;
    let status_code = value.get("status_code")?.as_u64()?;
    u16::try_from(status_code).ok()
}

/// Builds a response-only model request for final failure characterization.
fn failure_summary_request(
    previous_request: &ModelRequest,
    stage: &str,
    error: &MezError,
    failed_response_raw_text: &str,
) -> ModelRequest {
    let mut request = previous_request.clone();
    request.interaction_kind = ModelInteractionKind::ActionExecution;
    request.allowed_actions = AllowedActionSet::say_only();
    request.messages.push(ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::Configuration,
        content: format!(
            "[controller failure summary]\n\
             Mezzanine has already failed this turn at the controller/provider boundary. \
             Return exactly one say action with status final that briefly characterizes the failure for the user. \
             Do not request capabilities, call tools, retry work, or claim the original task succeeded. \
             Name the failure class and the most useful next diagnostic step.\n\
             stage={stage}\n\
             error_kind={:?} error_message={}\n\
             failed_response_excerpt:\n{}",
            error.kind(),
            error.message(),
            failure_summary_raw_text_excerpt(failed_response_raw_text)
        ),
    });
    request
}

/// Returns a bounded UTF-8-safe excerpt for terminal failure summary prompts.
fn failure_summary_raw_text_excerpt(raw_text: &str) -> String {
    if raw_text.len() <= FAILURE_SUMMARY_RAW_TEXT_LIMIT_BYTES {
        return raw_text.to_string();
    }
    let mut end = FAILURE_SUMMARY_RAW_TEXT_LIMIT_BYTES;
    while !raw_text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}\n[truncated: original_bytes={}]",
        &raw_text[..end],
        raw_text.len()
    )
}

/// Validation surface needed to accept a final failure-summary response.
#[derive(Clone, Copy)]
struct FailureSummaryScope<'a> {
    /// Failure stage reported to the model.
    stage: &'a str,
    /// MCP servers available to the failed interaction.
    available_mcp_servers: &'a [String],
    /// MCP tools available to the failed interaction.
    available_mcp_tools: &'a [McpPromptTool],
}

/// Data needed to ask the model for a final failure summary.
struct FailureSummaryInput<'a> {
    /// Failed response being characterized.
    failed_response: ModelResponse,
    /// Controller/provider error being characterized.
    error: &'a MezError,
    /// Summary validation and stage context.
    scope: FailureSummaryScope<'a>,
}

/// Converts a valid summary response into a terminal failed execution.
fn failure_summary_execution_from_response(
    turn: &AgentTurnRecord,
    request: ModelRequest,
    failed_response_raw_text: &str,
    mut response: ModelResponse,
    scope: FailureSummaryScope<'_>,
) -> Result<AgentTurnExecution> {
    let batch = response.action_batch.as_ref().ok_or_else(|| {
        MezError::invalid_args("failure summary response must include a say action batch")
    })?;
    validate_batch_allowed_actions(batch, &request)?;
    batch.validate(turn, scope.available_mcp_servers, scope.available_mcp_tools)?;
    if batch.actions.is_empty()
        || batch
            .actions
            .iter()
            .any(|action| !matches!(action.payload, AgentActionPayload::Say { .. }))
    {
        return Err(MezError::invalid_args(
            "failure summary response must contain only say actions",
        ));
    }
    let mut terminal_batch = batch.clone();
    terminal_batch.final_turn = true;
    for action in &mut terminal_batch.actions {
        if let AgentActionPayload::Say { status, .. } = &mut action.payload {
            *status = SayStatus::Final;
        }
    }
    let action_results = terminal_batch
        .actions
        .iter()
        .map(|action| match &action.payload {
            AgentActionPayload::Say {
                status,
                text,
                content_type,
            } => Ok(ActionResult::succeeded(
                turn,
                action,
                vec![text.clone()],
                Some(say_structured_content_json(*status, content_type, text)),
            )),
            _ => Err(MezError::invalid_args(
                "failure summary response must contain only say actions",
            )),
        })
        .collect::<Result<Vec<_>>>()?;
    response.raw_text = format!(
        "{}\ncontroller_failure_summary:\n{}",
        failed_response_raw_text, response.raw_text
    );
    response.action_batch = Some(terminal_batch);
    let latest_response_usage = response.usage;
    Ok(AgentTurnExecution {
        request,
        response,
        latest_response_usage,
        action_results,
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    })
}

/// Attempts one response-only provider call to summarize a provider failure.
#[cfg(test)]
fn summarize_provider_failure_execution<P: ModelProvider>(
    provider: &P,
    turn: &AgentTurnRecord,
    previous_request: &ModelRequest,
    error: &MezError,
) -> Option<AgentTurnExecution> {
    let failed = failed_provider_error_execution(
        previous_request.clone(),
        provider.provider_id(),
        &previous_request.model,
        error,
    );
    summarize_controller_failure_execution(
        provider,
        turn,
        previous_request,
        FailureSummaryInput {
            failed_response: failed.response.clone(),
            error,
            scope: FailureSummaryScope {
                stage: "provider_error",
                available_mcp_servers: &[],
                available_mcp_tools: &[],
            },
        },
    )
}

/// Attempts one response-only provider call to summarize a controller failure.
#[cfg(test)]
fn summarize_controller_failure_execution<P: ModelProvider>(
    provider: &P,
    turn: &AgentTurnRecord,
    previous_request: &ModelRequest,
    input: FailureSummaryInput<'_>,
) -> Option<AgentTurnExecution> {
    let request = failure_summary_request(
        previous_request,
        input.scope.stage,
        input.error,
        &input.failed_response.raw_text,
    );
    let response = provider.send_request(&request).ok()?;
    if response.provider != provider.provider_id() {
        return None;
    }
    failure_summary_execution_from_response(
        turn,
        request,
        &input.failed_response.raw_text,
        response,
        input.scope,
    )
    .ok()
}

/// Attempts one response-only provider call to summarize a provider failure.
async fn summarize_provider_failure_execution_async<P: AsyncModelProvider>(
    provider: &P,
    turn: &AgentTurnRecord,
    previous_request: &ModelRequest,
    error: &MezError,
) -> Option<AgentTurnExecution> {
    let failed = failed_provider_error_execution(
        previous_request.clone(),
        provider.provider_id(),
        &previous_request.model,
        error,
    );
    summarize_controller_failure_execution_async(
        provider,
        turn,
        previous_request,
        FailureSummaryInput {
            failed_response: failed.response.clone(),
            error,
            scope: FailureSummaryScope {
                stage: "provider_error",
                available_mcp_servers: &[],
                available_mcp_tools: &[],
            },
        },
    )
    .await
}

/// Attempts one response-only provider call to summarize a controller failure.
async fn summarize_controller_failure_execution_async<P: AsyncModelProvider>(
    provider: &P,
    turn: &AgentTurnRecord,
    previous_request: &ModelRequest,
    input: FailureSummaryInput<'_>,
) -> Option<AgentTurnExecution> {
    let request = failure_summary_request(
        previous_request,
        input.scope.stage,
        input.error,
        &input.failed_response.raw_text,
    );
    let response = provider.send_request_async(&request).await.ok()?;
    if response.provider != provider.provider_id() {
        return None;
    }
    failure_summary_execution_from_response(
        turn,
        request,
        &input.failed_response.raw_text,
        response,
        input.scope,
    )
    .ok()
}

/// Builds the terminal failed execution for a MAAP response that remained
/// invalid after all ephemeral repair attempts were exhausted.
fn failed_maap_validation_execution(
    request: ModelRequest,
    mut response: ModelResponse,
    latest_response_usage: ModelTokenUsage,
    error: &MezError,
) -> AgentTurnExecution {
    response.raw_text = format!("{}\nmaap_validation_error: {}", response.raw_text, error);
    AgentTurnExecution {
        request,
        response,
        latest_response_usage,
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    }
}

/// Builds a terminal failed MAAP validation execution, asking the model for one
/// final user-facing characterization when possible.
#[cfg(test)]
fn failed_maap_validation_execution_with_summary<P: ModelProvider>(
    provider: &P,
    turn: &AgentTurnRecord,
    request: ModelRequest,
    response: ModelResponse,
    latest_response_usage: ModelTokenUsage,
    error: &MezError,
    scope: FailureSummaryScope<'_>,
) -> AgentTurnExecution {
    let failed =
        failed_maap_validation_execution(request.clone(), response, latest_response_usage, error);
    summarize_controller_failure_execution(
        provider,
        turn,
        &request,
        FailureSummaryInput {
            failed_response: failed.response.clone(),
            error,
            scope,
        },
    )
    .unwrap_or(failed)
}

/// Builds a terminal failed MAAP validation execution, asking the model for one
/// final user-facing characterization when possible.
async fn failed_maap_validation_execution_with_summary_async<P: AsyncModelProvider>(
    provider: &P,
    turn: &AgentTurnRecord,
    request: ModelRequest,
    response: ModelResponse,
    latest_response_usage: ModelTokenUsage,
    error: &MezError,
    scope: FailureSummaryScope<'_>,
) -> AgentTurnExecution {
    let failed =
        failed_maap_validation_execution(request.clone(), response, latest_response_usage, error);
    summarize_controller_failure_execution_async(
        provider,
        turn,
        &request,
        FailureSummaryInput {
            failed_response: failed.response.clone(),
            error,
            scope,
        },
    )
    .await
    .unwrap_or(failed)
}

/// Executes the `action_supports_auto_allow` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn action_supports_auto_allow(action: &AgentAction) -> bool {
    !action_auto_allow_reason(action).trim().is_empty()
}

/// Returns the most concise model-authored reason available for auto-allow
/// decisions after compact MAAP omitted the formerly mandatory rationale.
fn action_auto_allow_reason(action: &AgentAction) -> String {
    if !action.rationale.trim().is_empty() {
        return action.rationale.clone();
    }
    if let Ok(Some(summary)) = local_action_summary(action)
        && !summary.trim().is_empty()
    {
        return summary;
    }
    if let Ok(Some(summary)) = network_action_summary(action)
        && !summary.trim().is_empty()
    {
        return summary;
    }
    match &action.payload {
        AgentActionPayload::Say { text, .. } => text.clone(),
        AgentActionPayload::Abort { reason } => reason.clone(),
        AgentActionPayload::CallSkill { name, .. } => format!("load skill {name}"),
        AgentActionPayload::RequestSkills => "request available skills".to_string(),
        _ => String::new(),
    }
}

/// Returns true when the active runtime policy resolves a fresh approval
/// prompt without user interaction.
fn action_prompt_gate_satisfied_by_policy(permissions: &PermissionPolicy) -> bool {
    permissions.approval_bypass()
        || permissions.approval_policy == crate::permissions::ApprovalPolicy::FullAccess
}

/// Builds structured approval metadata for actions accepted by policy rather
/// than by an explicit blocked-approval decision.
fn action_policy_approval_json(
    action: &AgentAction,
    kind: &str,
    permissions: &PermissionPolicy,
) -> serde_json::Value {
    let state = if permissions.approval_bypass() {
        "bypassed"
    } else {
        "full_access"
    };
    serde_json::json!({
        "state": state,
        "kind": kind,
        "action_id": action.id.as_str()
    })
}

/// Executes the `auto_allow_approval_json` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn auto_allow_approval_json(
    action: &AgentAction,
    action_kind: &str,
) -> serde_json::Value {
    serde_json::json!({
        "state": "auto_allowed",
        "kind": action_kind,
        "action_id": action.id.as_str(),
        "reason": action_auto_allow_reason(action)
    })
}

/// Executes the `turn_state_from_action_results` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn turn_state_from_action_results(
    results: &[ActionResult],
    final_turn: bool,
) -> AgentTurnState {
    if results
        .iter()
        .any(|result| result.status == ActionStatus::Blocked)
    {
        AgentTurnState::Blocked
    } else if results.iter().any(|result| result.is_error) {
        AgentTurnState::Failed
    } else if results
        .iter()
        .any(|result| result.status == ActionStatus::Running)
    {
        AgentTurnState::Running
    } else if final_turn || results_are_display_only_completion(results) {
        AgentTurnState::Completed
    } else {
        AgentTurnState::Running
    }
}

/// Reports whether action results represent an explicit display-only
/// completion.
///
/// Empty result sets are not completions. Treating them as such through
/// vacuous `all(...)` semantics can mask missing provider output or missing
/// action planning as a settled turn.
fn results_are_display_only_completion(results: &[ActionResult]) -> bool {
    !results.is_empty() && results.iter().all(result_is_display_only)
}

/// Executes the `result_is_display_only` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn result_is_display_only(result: &ActionResult) -> bool {
    matches!(result.action_type, "complete")
}

/// Builds the structured result payload for a `say` action.
fn say_structured_content_json(status: SayStatus, content_type: &str, text: &str) -> String {
    format!(
        r#"{{"kind":"say","status":"{}","content_type":"{}","text":"{}"}}"#,
        status.as_str(),
        json_escape(content_type),
        json_escape(text),
    )
}

/// Executes the `shell_command_structured_content_json` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn shell_command_structured_content_json(
    action: &AgentAction,
    sent_to_pane: bool,
    approval: serde_json::Value,
    matched_rules: &[String],
    terminal_observation: serde_json::Value,
) -> Result<String> {
    let Some(plan) = local_action_plan(action)? else {
        return Err(MezError::invalid_args(
            "shell structured content requires a shell-backed action",
        ));
    };
    let generated_command_elided =
        !matches!(action.payload, AgentActionPayload::ShellCommand { .. });
    let command = if generated_command_elided {
        plan.policy_command.clone()
    } else {
        plan.command.clone()
    };
    let value = serde_json::json!({
        "kind": action.action_type(),
        "summary": plan.summary,
        "command": command,
        "generated_command_elided": generated_command_elided,
        "generated_command_bytes": if generated_command_elided { Some(plan.command.len()) } else { None },
        "sent_to_pane": sent_to_pane,
        "stateful": plan.stateful,
        "approval": approval,
        "matched_rules": matched_rules,
        "terminal_observation": terminal_observation
    });
    serde_json::to_string(&value).map_err(|error| {
        MezError::invalid_state(format!("shell structured content encoding failed: {error}"))
    })
}

/// Executes the `shell_output_to_action_result` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn shell_output_to_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    output: ShellExecutionOutput,
    marker: MarkerToken,
) -> Result<ActionResult> {
    if local_action_plan(action)?.is_none() {
        return Err(MezError::invalid_args(
            "shell output requires a shell-backed action",
        ));
    }
    let combined_output_bytes = output.stdout.len().saturating_add(output.stderr.len());
    let signal: Option<i32> = if output.interrupted {
        Some(2) // SIGINT
    } else if let Some(ec) = output.exit_code {
        if ec > 128 && ec < 256 {
            Some(ec - 128)
        } else {
            None
        }
    } else {
        None
    };
    let structured = shell_command_structured_content_json(
        action,
        true,
        serde_json::Value::Null,
        &[],
        serde_json::json!({
            "source": "executor",
            "stream": "pty_combined",
            "marker": marker.as_str(),
            "exit_code": output.exit_code,
            "signal": signal,
            "timed_out": output.timed_out,
            "interrupted": output.interrupted,
            "combined_output_bytes": combined_output_bytes,
            "output_truncated": false
        }),
    )?;
    if output.timed_out {
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::TimedOut,
            "shell_timeout",
            "shell command timed out",
        )?;
        result.structured_content_json = Some(structured);
        return Ok(result);
    }
    if output.interrupted {
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Interrupted,
            "shell_interrupted",
            "shell command was interrupted",
        )?;
        result.structured_content_json = Some(structured);
        return Ok(result);
    }
    let mut combined_output = String::new();
    if !output.stdout.is_empty() {
        combined_output.push_str(&output.stdout);
    }
    if !output.stderr.is_empty() {
        combined_output.push_str(&output.stderr);
    }
    let mut content = Vec::new();
    if !combined_output.is_empty() {
        content.push(combined_output);
    }
    if matches!(action.payload, AgentActionPayload::ShellCommand { .. }) {
        return Ok(ActionResult::succeeded(
            turn,
            action,
            shell_command_result_content(
                content.first().map(String::as_str).unwrap_or_default(),
                output.exit_code,
                output.timed_out,
                output.interrupted,
            ),
            Some(structured),
        ));
    }
    if output.exit_code == Some(0) {
        Ok(ActionResult::succeeded(
            turn,
            action,
            content,
            Some(structured),
        ))
    } else {
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Failed,
            "shell_exit_nonzero",
            "shell command exited with non-zero status",
        )?;
        result.content = action_text_content_blocks(content);
        result.structured_content_json = Some(structured);
        Ok(result)
    }
}

/// Executes the `mcp_response_to_action_result` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn mcp_response_to_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    plan: &McpToolCallPlan,
    response: McpToolCallResponse,
) -> Result<ActionResult> {
    let content_json = response.content_json.clone();
    let structured_payload = format!(
        r#"{{"server":"{}","tool":"{}","content":{},"structured_content":{},"is_error":{}}}"#,
        json_escape(&plan.server_id),
        json_escape(&plan.tool_name),
        content_json,
        response
            .structured_content_json
            .as_deref()
            .unwrap_or("null"),
        response.is_error
    );
    let content = action_content_blocks_from_json_or_text(&response.content_json);
    if response.is_error {
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Failed,
            "mcp_tool_error",
            "MCP tool returned an error",
        )?;
        result.content = content;
        result.structured_content_json = Some(structured_payload);
        Ok(result)
    } else {
        let mut result =
            ActionResult::succeeded(turn, action, Vec::new(), Some(structured_payload));
        result.content = content;
        Ok(result)
    }
}

/// Executes the `role_for_source` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn role_for_source(source: ContextSourceKind) -> ModelMessageRole {
    match source {
        ContextSourceKind::System => ModelMessageRole::System,
        ContextSourceKind::DeveloperInstruction
        | ContextSourceKind::Policy
        | ContextSourceKind::Configuration => ModelMessageRole::Developer,
        ContextSourceKind::ActionResult | ContextSourceKind::TranscriptTool => {
            ModelMessageRole::Tool
        }
        ContextSourceKind::TranscriptAssistant => ModelMessageRole::Assistant,
        ContextSourceKind::UserInstruction
        | ContextSourceKind::LocalMessage
        | ContextSourceKind::ProjectGuidance
        | ContextSourceKind::Memory
        | ContextSourceKind::Transcript
        | ContextSourceKind::TranscriptUser => ModelMessageRole::User,
    }
}
