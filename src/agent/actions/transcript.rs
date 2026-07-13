//! Durable transcript projection for agent turn executions.
//!
//! This module owns conversion from one bounded model/action execution into
//! transcript entries. It keeps transcript persistence and assistant-history
//! shaping separate from the turn runner and shell/MCP action executors.

use super::super::{
    ActionResult, AgentAction, AgentActionPayload, AgentTranscriptEntry, AgentTranscriptRole,
    AgentTurnRecord, AgentTurnState, ContextSourceKind, MaapBatch, MezError, ModelMessage,
    ModelMessageRole, ModelRequest, ModelResponse, ModelTokenUsage, ModelTokenUsageKey,
    ProviderTranscriptEvent, Result, TranscriptPersistence,
};
use super::action_result_transcript_content;

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
    /// Provider token usage for auxiliary model calls made before the main
    /// turn request.
    ///
    /// Automatic routing/model-sizing requests use a provider model but are not
    /// part of the user-visible assistant response. Keeping them separate lets
    /// runtime status and metrics account for their cost under the router model
    /// without attributing those tokens to the selected execution model.
    pub routing_token_usage_by_model:
        std::collections::BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
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
) -> Result<Vec<AgentTranscriptEntry>> {
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
        entries.push(AgentTranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role: AgentTranscriptRole::User,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content,
        });
        sequence = sequence.saturating_add(1);
    }
    for event in provider_transcript_entries_for_execution(execution) {
        entries.push(AgentTranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role: AgentTranscriptRole::System,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content: event.to_transcript_content(),
        });
        sequence = sequence.saturating_add(1);
    }
    entries.push(AgentTranscriptEntry {
        conversation_id: conversation_id.to_string(),
        sequence,
        created_at_unix_seconds,
        role: AgentTranscriptRole::Assistant,
        turn_id: turn.turn_id.clone(),
        agent_id: turn.agent_id.clone(),
        pane_id: turn.pane_id.clone(),
        content: assistant_transcript_content(execution),
    });
    sequence = sequence.saturating_add(1);

    for result in &execution.action_results {
        entries.push(AgentTranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role: AgentTranscriptRole::Tool,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content: action_result_transcript_content(result),
        });
        sequence = sequence.saturating_add(1);
    }
    for entry in &entries {
        entry
            .validate()
            .map_err(|error| MezError::invalid_args(error.to_string()))?;
    }
    Ok(entries)
}

/// Builds hidden provider-native transcript entries for future provider replay.
fn provider_transcript_entries_for_execution(
    execution: &AgentTurnExecution,
) -> Vec<ProviderTranscriptEvent> {
    let mut events = Vec::new();
    for event in &execution.response.provider_transcript_events {
        events.push(event.clone());
        for tool_call_id in event.deepseek_tool_call_ids() {
            events.push(ProviderTranscriptEvent::DeepSeekToolResult {
                tool_call_id,
                content: provider_tool_result_content_for_execution(execution),
            });
        }
    }
    events
}

/// Returns compact provider-facing tool output for hidden native replay.
fn provider_tool_result_content_for_execution(execution: &AgentTurnExecution) -> String {
    let content = execution
        .action_results
        .iter()
        .map(action_result_transcript_content)
        .collect::<Vec<_>>()
        .join("\n\n");
    if content.trim().is_empty() {
        "Mezzanine accepted the provider tool call without additional action output.".to_string()
    } else {
        content
    }
}

/// Returns the assistant-history context produced by one model execution.
///
/// The returned text is the same assistant content durable transcript storage
/// would persist for the execution: visible `say` text is retained, non-visible
/// actions are summarized, and only explicit durable `thought` notes are
/// preserved as `thinking:` lines without retaining raw protocol JSON or inline
/// file payloads.
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
/// Routine batch/action rationale is intentionally omitted from durable
/// assistant history because it is usually immediate execution intent such as
/// "read exact anchors" or "check test lines" that causes future requests to
/// over-weight investigation churn. Only explicit durable `thought` notes are
/// persisted as `thinking:` lines.
const EMPTY_ASSISTANT_TRANSCRIPT_CONTENT: &str =
    "[assistant response contained no visible content]";

fn assistant_transcript_content(execution: &AgentTurnExecution) -> String {
    let Some(batch) = execution.response.action_batch.as_ref() else {
        return if execution.response.raw_text.trim().is_empty() {
            EMPTY_ASSISTANT_TRANSCRIPT_CONTENT.to_string()
        } else {
            execution.response.raw_text.clone()
        };
    };
    let mut thinking_lines = assistant_transcript_durable_thinking_lines(batch);
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

/// Returns durable model-authored thinking notes as transcript-visible lines.
///
/// The explicit `thought` field is the only durable assistant work-note
/// channel. Batch/action rationale remains available in runtime logs and action
/// results for the current turn, but it is not replayed into future assistant
/// context because it tends to encode transient execution intent rather than
/// stable decisions.
fn assistant_transcript_durable_thinking_lines(batch: &MaapBatch) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(thought) = batch.thought.as_deref()
        && !thought.trim().is_empty()
    {
        lines.extend(assistant_transcript_thinking_lines(thought));
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
        AgentActionPayload::MemorySearch { query, limit } => format!(
            "memory_search query={} limit={}",
            bounded_transcript_field(query),
            limit
                .map(|value| value.to_string())
                .unwrap_or_else(|| "default".to_string())
        ),
        AgentActionPayload::MemoryStore {
            kind,
            content,
            keywords,
            ..
        } => format!(
            "memory_store kind={} content_bytes={} keyword_count={}",
            bounded_transcript_field(kind),
            content.len(),
            keywords.len()
        ),
        AgentActionPayload::IssueAdd {
            kind,
            title,
            body,
            notes,
            depends_on,
        } => format!(
            "issue_add kind={} title={} body_bytes={} notes_bytes={} depends_on_count={}",
            bounded_transcript_field(kind),
            bounded_transcript_field(title),
            body.as_deref().map(str::len).unwrap_or(0),
            notes.as_deref().map(str::len).unwrap_or(0),
            depends_on.len()
        ),
        AgentActionPayload::IssueUpdate {
            id,
            state,
            body,
            notes,
            clear_body,
            clear_notes,
            ..
        } => format!(
            "issue_update id={} state={} body_bytes={} notes_bytes={} clear_body={} clear_notes={}",
            bounded_transcript_field(id),
            state.as_deref().unwrap_or("unchanged"),
            body.as_deref().map(str::len).unwrap_or(0),
            notes.as_deref().map(str::len).unwrap_or(0),
            clear_body,
            clear_notes
        ),
        AgentActionPayload::IssueQuery {
            kind,
            state,
            text,
            limit,
        } => format!(
            "issue_query kind={} state={} text_bytes={} limit={}",
            kind.as_deref().unwrap_or("any"),
            state.as_deref().unwrap_or("open"),
            text.as_deref().map(str::len).unwrap_or(0),
            limit
                .map(|value| value.to_string())
                .unwrap_or_else(|| "default".to_string())
        ),
        AgentActionPayload::IssueDelete { id } => {
            format!("issue_delete id={}", bounded_transcript_field(id))
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
pub fn persist_turn_execution_transcript<P>(
    store: &P,
    conversation_id: &str,
    created_at_unix_seconds: u64,
    turn: &AgentTurnRecord,
    execution: &AgentTurnExecution,
) -> Result<Vec<AgentTranscriptEntry>>
where
    P: TranscriptPersistence<Error = MezError>,
{
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
pub fn next_transcript_sequence<P>(store: &P, conversation_id: &str) -> Result<u64>
where
    P: TranscriptPersistence<Error = MezError>,
{
    Ok(store.next_sequence(conversation_id)?.unwrap_or(1))
}
