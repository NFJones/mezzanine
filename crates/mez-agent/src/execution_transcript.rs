//! Durable transcript projection for agent turn executions.
//!
//! This module owns conversion from one bounded model/action execution into
//! transcript entries. It keeps transcript persistence and assistant-history
//! shaping separate from the turn runner and shell/MCP action executors.

use crate::{
    ActionResult, AgentAction, AgentActionPayload, AgentTurnRecord, AgentTurnState,
    ContextSourceKind, MaapBatch, ModelMessage, ModelMessageRole, ModelRequest, ModelResponse,
    ModelTokenUsage, ModelTokenUsageKey, ProviderTranscriptEvent, TranscriptContractError,
    TranscriptEntry, TranscriptRole, action_result_transcript_content,
};

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
) -> Result<Vec<TranscriptEntry>, TranscriptContractError> {
    if first_sequence == 0 || created_at_unix_seconds == 0 {
        return Err(TranscriptContractError::new(
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
    for event in provider_transcript_entries_for_execution(execution) {
        entries.push(TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role: TranscriptRole::System,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content: event.to_transcript_content(),
        });
        sequence = sequence.saturating_add(1);
    }

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
/// persists for the execution. It preserves the complete model-authored causal
/// response: batch rationale, durable thought, action-local rationale, visible
/// conversational text, and bounded action summaries. Raw protocol JSON and
/// inline action payloads remain excluded.
pub fn assistant_context_content_for_execution(execution: &AgentTurnExecution) -> String {
    assistant_transcript_content(execution)
}

/// Returns durable request text for transcript storage.
///
/// Model requests are assembled from prompt scaffolding: system prompts,
/// environment details, prior transcript excerpts, action-result context, and
/// action feedback. Persisting that whole request recursively stores prior
/// transcript context inside the next transcript entry. Durable transcripts
/// therefore keep only exact direct-user events from the request; assistant
/// output and tool results are appended from the execution itself.
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

/// Returns canonical assistant transcript text without copying raw protocol
/// JSON or inline file payloads into long-lived transcript storage.
///
/// Rationale is part of the accepted assistant response and therefore part of
/// the causal history consumed by later provider calls. Presentation may hide
/// redundant rationale from the terminal, but transcript projection must not
/// discard or rewrite it. Closed execution groups remain eligible for atomic
/// compaction through the context lifecycle instead of being made incomplete
/// at this boundary.
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
    let mut lines = assistant_transcript_rationale_lines("rationale", &batch.rationale);
    lines.extend(assistant_transcript_durable_thinking_lines(batch));
    if !execution.response.raw_text.trim().is_empty()
        && !assistant_raw_text_looks_like_maap_payload(&execution.response.raw_text)
    {
        lines.push(execution.response.raw_text.clone());
    }
    lines.extend(assistant_action_transcript_lines(batch));
    if lines.is_empty() {
        EMPTY_ASSISTANT_TRANSCRIPT_CONTENT.to_string()
    } else {
        lines.join("\n")
    }
}

/// Prefixes each non-empty model-authored rationale line for causal replay.
fn assistant_transcript_rationale_lines(label: &str, text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| format!("{label}: {line}"))
        .collect()
}

/// Returns durable model-authored thinking notes as transcript-visible lines.
///
/// `thought` remains the explicit durable work-note channel. Rationale is also
/// retained as causal execution context, but it is labeled separately so later
/// providers can distinguish immediate intent from longer-lived learning.
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

/// Returns action-local rationale and bounded action summaries in provider
/// order.
///
/// `say` actions retain exact conversational text so later references such as
/// "do item 2" remain meaningful. Other action payloads are summarized to
/// avoid copying commands, patches, generated files, or request bodies into
/// model history.
fn assistant_action_transcript_lines(batch: &MaapBatch) -> Vec<String> {
    let mut lines = Vec::new();
    for action in &batch.actions {
        let rationale_label = format!("action rationale {} ({})", action.id, action.action_type());
        lines.extend(assistant_transcript_rationale_lines(
            &rationale_label,
            &action.rationale,
        ));
        match &action.payload {
            AgentActionPayload::Say { text, .. } => {
                let text = text.trim();
                if !text.is_empty() {
                    lines.push(text.to_string());
                }
            }
            AgentActionPayload::Abort { reason } => {
                lines.push(format!("aborted: {}", reason.trim()));
            }
            _ => lines.push(format!(
                "action {}: {}",
                action.id,
                assistant_transcript_action_summary(action)
            )),
        }
    }
    lines
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
            refresh,
        } => format!(
            "issue_query kind={} state={} text_bytes={} limit={} refresh={}",
            kind.as_deref().unwrap_or("any"),
            state.as_deref().unwrap_or("open"),
            text.as_deref().map(str::len).unwrap_or(0),
            limit
                .map(|value| value.to_string())
                .unwrap_or_else(|| "default".to_string()),
            refresh
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentTurnTrigger, AllowedActionSet, ModelInteractionKind, SayStatus};

    /// Builds the canonical turn identity used by transcript projection tests.
    fn turn() -> AgentTurnRecord {
        AgentTurnRecord {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "%1".to_string(),
            trigger: AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 100,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: AgentTurnState::Running,
            cooperation_mode: None,
            initial_capability: None,
        }
    }

    /// Builds one provider-independent request from explicit projected messages.
    fn request(messages: Vec<ModelMessage>) -> ModelRequest {
        ModelRequest {
            provider: "openai".to_string(),
            model: "default".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: ModelInteractionKind::ActionExecution,
            allowed_actions: AllowedActionSet::respond_only(),
            stop: None,
            messages,
        }
    }

    /// Builds a labeled provider-facing user message.
    fn message(source: ContextSourceKind, label: &str, content: &str) -> ModelMessage {
        ModelMessage {
            role: ModelMessageRole::User,
            source,
            placement: crate::ContextPlacement::ConversationAppend,
            content: format!("[{label}]\n{content}"),
        }
    }

    /// Builds one canonical execution for transcript projection tests.
    fn execution(
        messages: Vec<ModelMessage>,
        raw_text: impl Into<String>,
        action_batch: Option<MaapBatch>,
        provider_transcript_events: Vec<ProviderTranscriptEvent>,
        action_results: Vec<ActionResult>,
    ) -> AgentTurnExecution {
        AgentTurnExecution {
            request: request(messages),
            response: ModelResponse {
                provider: "openai".to_string(),
                model: "default".to_string(),
                raw_text: raw_text.into(),
                usage: ModelTokenUsage::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch,
                provider_transcript_events,
            },
            latest_response_usage: ModelTokenUsage::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results,
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        }
    }

    /// Builds one visible final response action.
    fn say_action(text: &str) -> AgentAction {
        AgentAction {
            id: "say-1".to_string(),
            rationale: "reply".to_string(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Final,
                text: text.to_string(),
                content_type: "text/plain".to_string(),
            },
        }
    }

    /// Builds one shell action used to identify a provider-native tool result.
    fn shell_action() -> AgentAction {
        AgentAction {
            id: "a1".to_string(),
            rationale: "inspect".to_string(),
            payload: AgentActionPayload::ShellCommand {
                summary: "Inspect the directory".to_string(),
                command: "pwd".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: None,
            },
        }
    }

    #[test]
    /// Verifies expanded skill context is not persisted as user transcript text.
    ///
    /// Skill bodies are execution-time workflow context. Durable transcripts
    /// keep the explicit invocation but omit injected skill content.
    fn turn_execution_transcript_omits_expanded_skill_request_context() {
        let execution = execution(
            vec![
                message(
                    ContextSourceKind::UserInstruction,
                    "explicit skill review",
                    "# Skill: review\n\nReview workflow.",
                ),
                message(
                    ContextSourceKind::RuntimeHint,
                    "explicit skill invocation review",
                    "skill=review",
                ),
                message(
                    ContextSourceKind::UserInstruction,
                    "user prompt",
                    "$review focus src/lib.rs",
                ),
            ],
            "Working on it.",
            None,
            Vec::new(),
            Vec::new(),
        );

        let entries =
            transcript_entries_for_execution("conv1", 1, 200, &turn(), &execution).unwrap();
        let users = entries
            .iter()
            .filter(|entry| entry.role == TranscriptRole::User)
            .collect::<Vec<_>>();

        assert_eq!(users.len(), 1, "{entries:?}");
        assert_eq!(users[0].content, "$review focus src/lib.rs");
        assert!(
            entries
                .iter()
                .all(|entry| !entry.content.contains("# Skill:"))
        );
    }

    #[test]
    /// Verifies transcript persistence does not recursively store prompt context.
    ///
    /// Prior transcript and action-result request messages must not be appended
    /// as new user records on every continuation.
    fn turn_execution_transcript_omits_recursive_request_context() {
        let execution = execution(
            vec![
                message(
                    ContextSourceKind::UserInstruction,
                    "user prompt",
                    "create test.txt",
                ),
                message(
                    ContextSourceKind::ActionResult,
                    "legacy passive terminal context",
                    "terminal prompt and previous output",
                ),
                message(
                    ContextSourceKind::Transcript,
                    "recent transcript for pane %1",
                    "recursive-payload",
                ),
            ],
            "Working on it.",
            None,
            Vec::new(),
            Vec::new(),
        );

        let entries =
            transcript_entries_for_execution("conv1", 1, 200, &turn(), &execution).unwrap();

        assert_eq!(entries[0].role, TranscriptRole::User);
        assert_eq!(entries[0].content, "create test.txt");
        assert!(
            entries
                .iter()
                .all(|entry| !entry.content.contains("recursive-payload"))
        );
        assert!(
            entries
                .iter()
                .all(|entry| entry.role != TranscriptRole::System)
        );
    }

    #[test]
    /// Verifies conversational `say` output is preserved as assistant history.
    ///
    /// Follow-up prompts can refer to numbered lists, so visible text must not
    /// be replaced by compact protocol summaries.
    fn turn_execution_transcript_preserves_visible_say_text() {
        let visible_text = "Suggested changes:\n1. Keep history role-aware.\n2. Preserve lists.";
        let batch = MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "reply".to_string(),
            thought: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            actions: vec![say_action(visible_text)],
            final_turn: true,
        };
        let execution = execution(
            vec![message(
                ContextSourceKind::UserInstruction,
                "user prompt",
                "list suggested changes",
            )],
            r#"{"actions":[{"type":"say"}]}"#,
            Some(batch),
            Vec::new(),
            Vec::new(),
        );

        let entries =
            transcript_entries_for_execution("conv1", 1, 200, &turn(), &execution).unwrap();
        let assistant = entries
            .iter()
            .find(|entry| entry.role == TranscriptRole::Assistant)
            .unwrap();

        assert!(assistant.content.contains("rationale: reply"));
        assert!(
            assistant
                .content
                .contains("action rationale say-1 (say): reply")
        );
        assert!(assistant.content.ends_with(visible_text));
        assert!(!assistant.content.contains("say text="));
    }

    #[test]
    /// Verifies provider-native replay metadata is durable but not visible.
    ///
    /// DeepSeek tool calls require hidden assistant and tool-result events for
    /// later replay without exposing provider JSON in visible transcript rows.
    fn turn_execution_transcript_stores_hidden_provider_native_tool_call_events() {
        let turn = turn();
        let action = shell_action();
        let result = ActionResult::running(
            &turn,
            &action,
            vec!["shell command accepted for pane execution".to_string()],
            None,
        );
        let execution = execution(
            vec![message(
                ContextSourceKind::UserInstruction,
                "user",
                "run pwd",
            )],
            "executing",
            None,
            vec![ProviderTranscriptEvent::DeepSeekAssistantToolCall {
                content: String::new(),
                reasoning_content: Some("I need command output.".to_string()),
                tool_calls: vec![serde_json::json!({
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "submit_maap_action_batch", "arguments": "{}"}
                })],
            }],
            vec![result],
        );

        let entries = transcript_entries_for_execution("conv1", 1, 200, &turn, &execution).unwrap();
        let assistant_index = entries
            .iter()
            .position(|entry| entry.role == TranscriptRole::Assistant)
            .unwrap();
        let first_native_index = entries
            .iter()
            .position(|entry| entry.role == TranscriptRole::System)
            .unwrap();
        let generic_result_index = entries
            .iter()
            .position(|entry| entry.role == TranscriptRole::Tool)
            .unwrap();
        assert!(assistant_index < first_native_index);
        assert!(first_native_index < generic_result_index);
        let hidden = entries
            .iter()
            .filter(|entry| entry.role == TranscriptRole::System)
            .map(|entry| ProviderTranscriptEvent::from_transcript_content(&entry.content).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(hidden.len(), 2);
        let ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id,
            content,
        } = &hidden[1]
        else {
            panic!("expected DeepSeek tool-result event");
        };
        assert_eq!(tool_call_id, "call_1");
        assert!(content.contains("[action_result a1 shell_command running]"));
        let visible = entries
            .iter()
            .filter(|entry| entry.role != TranscriptRole::System)
            .map(|entry| entry.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!visible.contains("reasoning_content"));
        assert!(!visible.contains("call_1"));
    }

    #[test]
    /// Verifies assistant transcript entries summarize MAAP action batches
    /// without retaining inline patch payloads from raw provider JSON.
    ///
    /// Durable history preserves rationale, explicit thought notes, and bounded
    /// action shape while omitting generated file bytes.
    fn turn_execution_transcript_summarizes_maap_action_batches() {
        let patch =
            "*** Begin Patch\n*** Add File: note.txt\n+large-inline-file-content\n*** End Patch";
        let action = AgentAction {
            id: "patch-1".to_string(),
            rationale: "write note file".to_string(),
            payload: AgentActionPayload::ApplyPatch {
                patch: patch.to_string(),
                strip: None,
            },
        };
        let batch = MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "transient batch rationale".to_string(),
            thought: Some("The patch summary belongs in future model context.".to_string()),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            actions: vec![action],
            final_turn: false,
        };
        let execution = execution(
            vec![message(
                ContextSourceKind::UserInstruction,
                "user",
                "create note.txt",
            )],
            format!(
                r#"{{"actions":[{{"patch":{}}}]}}"#,
                serde_json::json!(patch)
            ),
            Some(batch),
            Vec::new(),
            Vec::new(),
        );

        let entries =
            transcript_entries_for_execution("conv1", 1, 200, &turn(), &execution).unwrap();
        let assistant = entries
            .iter()
            .find(|entry| entry.role == TranscriptRole::Assistant)
            .unwrap();

        assert!(assistant.content.contains("thinking: The patch summary"));
        assert!(
            assistant
                .content
                .contains("rationale: transient batch rationale")
        );
        assert!(
            assistant
                .content
                .contains("action rationale patch-1 (apply_patch): write note file")
        );
        assert!(assistant.content.contains("apply_patch patch_bytes="));
        assert!(!assistant.content.contains("large-inline-file-content"));
    }

    #[test]
    /// Verifies conversational provider text cannot short-circuit structured
    /// rationale and action projection.
    ///
    /// Some adapters expose a compact conversational `raw_text` value beside a
    /// parsed MAAP batch. Both belong to the accepted response and must reach a
    /// later continuation without retaining the raw action envelope.
    fn assistant_context_preserves_raw_text_and_structured_rationale_together() {
        let batch = MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "Select issue iss-42 before inspecting its owner".to_string(),
            thought: Some("Active issue: iss-42".to_string()),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            actions: vec![shell_action()],
            final_turn: false,
        };
        let execution = execution(
            vec![message(
                ContextSourceKind::UserInstruction,
                "user",
                "fix the issue backlog",
            )],
            "I selected the highest-priority issue.",
            Some(batch),
            Vec::new(),
            Vec::new(),
        );

        let content = assistant_context_content_for_execution(&execution);

        assert!(content.contains("rationale: Select issue iss-42"));
        assert!(content.contains("thinking: Active issue: iss-42"));
        assert!(content.contains("I selected the highest-priority issue."));
        assert!(content.contains("action rationale a1 (shell_command): inspect"));
        assert!(content.contains("action a1: shell_command"));
        assert!(!content.contains("\"actions\""));
    }

    #[test]
    /// Verifies empty provider text still produces a durable assistant entry.
    ///
    /// Transcript validation forbids empty content, so projection synthesizes a
    /// bounded placeholder when no visible text or MAAP batch exists.
    fn turn_execution_transcript_synthesizes_placeholder_for_empty_assistant_response() {
        let execution = execution(
            vec![message(
                ContextSourceKind::UserInstruction,
                "user prompt",
                "respond with a MAAP action batch",
            )],
            String::new(),
            None,
            Vec::new(),
            Vec::new(),
        );

        let entries =
            transcript_entries_for_execution("conv1", 1, 200, &turn(), &execution).unwrap();
        let assistant = entries
            .iter()
            .find(|entry| entry.role == TranscriptRole::Assistant)
            .unwrap();

        assert_eq!(
            assistant.content,
            "[assistant response contained no visible content]"
        );
        assistant.validate().unwrap();
    }
}
