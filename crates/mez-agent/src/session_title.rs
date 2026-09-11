//! Bounded provider-backed session-title generation contracts.
//!
//! One conversation may ask for a short model-authored display title through a
//! dedicated side-channel interaction. This module owns only the provider
//! request shape, its input bounds, and the interaction-kind contract. The
//! response never becomes conversation content: product code sanitizes the raw
//! text under the shared title bounds and stores it as display state.
//!
//! The request is built fresh from already-published, bounded inputs, exposes a
//! response-only `say`-only surface, carries a small output budget, and starts
//! no prompt-cache lineage, so it cannot move cache lineage, compact context,
//! create a turn, or advance turn state.

use crate::{
    AllowedActionSet, ContextPlacement, ContextSourceKind, ModelInteractionKind, ModelMessage,
    ModelMessageRole, ModelProfile, ModelRequest,
};

/// Hard provider output-token cap for one session-title request.
///
/// A display title is bounded far below this value, so the cap only protects
/// against a model that rambles instead of answering.
pub const SESSION_TITLE_MAX_OUTPUT_TOKENS: usize = 24;

/// Maximum objective bytes included in one session-title request.
pub const SESSION_TITLE_OBJECTIVE_MAX_BYTES: usize = 1_024;

/// Maximum first-prompt bytes included in one session-title request.
pub const SESSION_TITLE_FIRST_PROMPT_MAX_BYTES: usize = 512;

/// Maximum summary-line bytes included in one session-title request.
pub const SESSION_TITLE_SUMMARY_MAX_BYTES: usize = 256;

/// Stable instruction sent as the only system message of a title request.
pub const SESSION_TITLE_INSTRUCTION: &str = "Write one very short sentence of three to eight words that describes what this session is about. Use sentence case, no quotes, no markup, and no trailing period. Return only the sentence and never call a tool.";

/// Marker appended when one bounded input had to be truncated.
const SESSION_TITLE_TRUNCATION_MARKER: &str = " [truncated]";

/// Bounded, already-published inputs for one session-title request.
///
/// Every value is optional: the request stays well-formed and bounded when no
/// input exists, and the caller separately decides whether generating a title
/// is worth a provider call at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionTitleGenerationInputs<'a> {
    /// Published conversation objective, when one is available.
    pub objective: Option<&'a str>,
    /// First user prompt of the conversation, when one is available.
    pub first_prompt: Option<&'a str>,
    /// At most one bounded summary line of conversation context.
    pub summary_line: Option<&'a str>,
}

impl SessionTitleGenerationInputs<'_> {
    /// Reports whether any bounded input can seed a title request.
    pub fn has_input(&self) -> bool {
        [self.objective, self.first_prompt, self.summary_line]
            .into_iter()
            .flatten()
            .any(|value| !value.trim().is_empty())
    }
}

/// Builds one fresh, response-only provider request for a conversation title.
///
/// The returned request shares nothing with the live turn request: it is a new
/// [`ModelRequest`] with an empty turn identity, no prompt-cache lineage, no
/// tools, no MCP tools, and a small output budget.
pub fn session_title_request(
    model_profile: &ModelProfile,
    agent_id: &str,
    inputs: &SessionTitleGenerationInputs<'_>,
) -> ModelRequest {
    let mut lines = Vec::new();
    if let Some(objective) = inputs.objective {
        let objective = bounded_input_excerpt(objective, SESSION_TITLE_OBJECTIVE_MAX_BYTES);
        if !objective.is_empty() {
            lines.push(format!("objective: {objective}"));
        }
    }
    if let Some(first_prompt) = inputs.first_prompt {
        let first_prompt =
            bounded_input_excerpt(first_prompt, SESSION_TITLE_FIRST_PROMPT_MAX_BYTES);
        if !first_prompt.is_empty() {
            lines.push(format!("first_prompt: {first_prompt}"));
        }
    }
    if let Some(summary_line) = inputs.summary_line {
        let summary_line = bounded_input_excerpt(summary_line, SESSION_TITLE_SUMMARY_MAX_BYTES);
        if !summary_line.is_empty() {
            lines.push(format!("summary: {summary_line}"));
        }
    }
    let evidence = if lines.is_empty() {
        "no bounded conversation inputs are available".to_string()
    } else {
        lines.join("\n")
    };
    ModelRequest {
        provider: model_profile.provider.clone(),
        model: model_profile.model.clone(),
        model_capabilities: model_profile.model_capabilities.clone(),
        max_input_tokens: model_profile.max_input_tokens(),
        reasoning_effort: None,
        thinking_enabled: Some(false),
        latency_preference: model_profile.latency_preference.clone(),
        prompt_cache_retention: None,
        max_output_tokens: Some(SESSION_TITLE_MAX_OUTPUT_TOKENS),
        temperature: None,
        stop: None,
        prompt_cache_session_id: None,
        prompt_cache_lineage_id: None,
        turn_id: String::new(),
        agent_id: agent_id.to_string(),
        available_mcp_tools: Vec::new(),
        memory_actions_enabled: false,
        issue_actions_enabled: false,
        interaction_kind: ModelInteractionKind::SessionTitle,
        allowed_actions: AllowedActionSet::say_only(),
        messages: vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: ContextPlacement::StablePrefix,
                content: SESSION_TITLE_INSTRUCTION.to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Context,
                source: ContextSourceKind::RuntimeHint,
                placement: ContextPlacement::ConversationAppend,
                content: format!("[session title inputs]\n{evidence}"),
            },
        ]
        .into(),
    }
}

/// Returns one UTF-8-safe, byte-bounded excerpt of an untrusted input.
fn bounded_input_excerpt(value: &str, max_bytes: usize) -> String {
    let trimmed = value.trim();
    if trimmed.len() <= max_bytes {
        return trimmed.to_string();
    }
    let mut end = max_bytes;
    while !trimmed.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}{SESSION_TITLE_TRUNCATION_MARKER}",
        trimmed[..end].trim_end()
    )
}

#[cfg(test)]
mod tests {
    use super::{
        SESSION_TITLE_FIRST_PROMPT_MAX_BYTES, SESSION_TITLE_INSTRUCTION,
        SESSION_TITLE_MAX_OUTPUT_TOKENS, SESSION_TITLE_OBJECTIVE_MAX_BYTES,
        SESSION_TITLE_SUMMARY_MAX_BYTES, SessionTitleGenerationInputs, session_title_request,
    };
    use crate::{
        AllowedAction, AllowedActionSet, ContextPlacement, ContextSourceKind, ModelInteractionKind,
        ModelMessageRole, ModelProfile,
    };

    /// Builds one deterministic profile for request-shape assertions.
    fn profile() -> ModelProfile {
        ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-5".to_string(),
            ..Default::default()
        }
    }

    /// Verifies the request is response-only, tool-free, and cheap.
    #[test]
    fn title_request_is_response_only_and_bounded() {
        let request = session_title_request(
            &profile(),
            "agent-pane-1",
            &SessionTitleGenerationInputs {
                objective: Some("Inspect the backlog"),
                first_prompt: Some("look at the open issues"),
                summary_line: Some("summary line"),
            },
        );
        assert_eq!(request.interaction_kind, ModelInteractionKind::SessionTitle);
        assert_eq!(request.interaction_kind.as_str(), "session_title");
        assert!(!request.interaction_kind.expects_maap_batch());
        assert!(!request.interaction_kind.expects_structured_json());
        assert_eq!(request.allowed_actions, AllowedActionSet::say_only());
        assert_eq!(
            request
                .allowed_actions
                .actions
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![AllowedAction::Say]
        );
        assert_eq!(
            request.max_output_tokens,
            Some(SESSION_TITLE_MAX_OUTPUT_TOKENS)
        );
        assert!(request.available_mcp_tools.is_empty());
        assert!(!request.memory_actions_enabled);
        assert!(!request.issue_actions_enabled);
        assert!(request.temperature.is_none());
        assert!(request.stop.is_none());
        assert_eq!(request.provider, "openai");
        assert_eq!(request.model, "gpt-5");
        assert_eq!(request.agent_id, "agent-pane-1");
    }

    /// Verifies the request starts no turn and no prompt-cache lineage.
    #[test]
    fn title_request_starts_no_turn_and_no_cache_lineage() {
        let request = session_title_request(
            &profile(),
            "agent-pane-1",
            &SessionTitleGenerationInputs {
                objective: Some("Inspect the backlog"),
                ..Default::default()
            },
        );
        assert!(request.turn_id.is_empty());
        assert!(request.prompt_cache_lineage_id.is_none());
        assert!(request.prompt_cache_session_id.is_none());
        assert!(request.prompt_cache_retention.is_none());
        assert_eq!(request.thinking_enabled, Some(false));
        assert!(request.reasoning_effort.is_none());
    }

    /// Verifies the request carries exactly the instruction plus bounded input.
    #[test]
    fn title_request_carries_the_instruction_and_bounded_input() {
        let request = session_title_request(
            &profile(),
            "agent-pane-1",
            &SessionTitleGenerationInputs {
                objective: Some("  Inspect the backlog  "),
                first_prompt: Some("look at the open issues"),
                summary_line: Some("one summary"),
            },
        );
        let messages = request.messages.iter().collect::<Vec<_>>();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, ModelMessageRole::System);
        assert_eq!(messages[0].source, ContextSourceKind::System);
        assert_eq!(messages[0].placement, ContextPlacement::StablePrefix);
        assert_eq!(messages[0].content, SESSION_TITLE_INSTRUCTION);
        assert_eq!(messages[1].role, ModelMessageRole::Context);
        assert_eq!(messages[1].placement, ContextPlacement::ConversationAppend);
        assert_eq!(
            messages[1].content,
            "[session title inputs]\nobjective: Inspect the backlog\n\
             first_prompt: look at the open issues\nsummary: one summary"
        );
    }

    /// Verifies every input is byte-bounded before it reaches the provider.
    #[test]
    fn title_request_bounds_every_input() {
        let request = session_title_request(
            &profile(),
            "agent-pane-1",
            &SessionTitleGenerationInputs {
                objective: Some(&"objective ".repeat(400)),
                first_prompt: Some(&"prompt ".repeat(400)),
                summary_line: Some(&"summary ".repeat(400)),
            },
        );
        let content = request
            .messages
            .iter()
            .last()
            .map(|message| message.content.clone())
            .expect("title request carries bounded input");
        assert!(content.contains("[truncated]"), "{content}");
        let bound = SESSION_TITLE_OBJECTIVE_MAX_BYTES
            + SESSION_TITLE_FIRST_PROMPT_MAX_BYTES
            + SESSION_TITLE_SUMMARY_MAX_BYTES
            + 128;
        assert!(
            content.len() <= bound,
            "bounded input exceeded: {}",
            content.len()
        );
    }

    /// Verifies a title request without inputs is still well-formed.
    #[test]
    fn title_request_without_inputs_is_well_formed() {
        let inputs = SessionTitleGenerationInputs::default();
        assert!(!inputs.has_input());
        let request = session_title_request(&profile(), "agent-pane-1", &inputs);
        assert_eq!(request.interaction_kind, ModelInteractionKind::SessionTitle);
        let content = request
            .messages
            .iter()
            .last()
            .map(|message| message.content.clone())
            .expect("title request carries context");
        assert!(content.contains("no bounded conversation inputs"));
    }

    /// Verifies blank-only inputs never count as usable generation input.
    #[test]
    fn blank_inputs_do_not_count_as_input() {
        assert!(
            !SessionTitleGenerationInputs {
                objective: Some("   \t"),
                ..Default::default()
            }
            .has_input()
        );
        assert!(
            SessionTitleGenerationInputs {
                first_prompt: Some("real prompt"),
                ..Default::default()
            }
            .has_input()
        );
    }
}
