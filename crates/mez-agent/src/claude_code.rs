//! Provider-independent Claude Code CLI policy.
//!
//! This module owns deterministic Claude Code request policy that does not
//! require process execution, filesystem access, credentials, or product error
//! projection. The root adapter retains subprocess invocation and process-local
//! session locking around the stable identities produced here.

use crate::{
    ModelRequest, ProviderRequestAssemblyError, ProviderRequestAssemblyResult,
    maap_action_batch_schema,
};
use sha2::Digest;

/// Corrective instruction used after Claude Code returns malformed MAAP output.
pub const CLAUDE_CODE_MAAP_RETRY_INSTRUCTION: &str = "Your previous response was invalid for Mezzanine because it did not satisfy the required structured output contract. Return only one validated Mezzanine MAAP action batch that matches the provided JSON schema, with no surrounding prose.";
/// Corrective instruction used after Claude Code returns an empty response.
pub const CLAUDE_CODE_EMPTY_OUTPUT_RETRY_INSTRUCTION: &str = "Your previous response was empty. Return only one validated Mezzanine MAAP action batch that matches the provided JSON schema, with no surrounding prose.";

/// Builds the Claude system prompt passed through the dedicated CLI channel.
pub fn claude_code_system_prompt(
    request: &ModelRequest,
    retry_instruction: Option<&str>,
) -> String {
    let mut prompt = String::new();
    append_claude_code_instruction_framing(&mut prompt, request, retry_instruction);
    if request.interaction_kind == crate::ModelInteractionKind::AutoSizing {
        prompt.push_str("Claude Code internal router boundary:\n");
        prompt.push_str("This turn is a hidden preflight classification step for Mezzanine's internal auto-sizing router, not a user-visible assistant response. Do not answer the user's task, continue the conversation, call native tools, or emit MAAP actions. When Mezzanine provides a JSON schema, use StructuredOutput only as a carrier for the router decision object.\n");
        prompt.push_str("Output contract:\n");
        prompt.push_str("Return exactly one JSON object matching the requested schema with version, size, reasoning_effort, confidence, and rationale. Do not include prose, markdown, code fences, or task-completion text before or after that JSON object.\n");
    } else {
        prompt.push_str("Claude Code direct-tool boundary:\n");
        prompt.push_str("Perform all requested operations through Mezzanine MAAP actions only. Do not call Claude Code native tools for local files, commands, web, MCP, subagents, config, memory, issue operations, or task delegation. Use only the response channel Mezzanine requested for this turn. When a MAAP schema is present, the only Claude Code tool Mezzanine may allow is StructuredOutput, and it is only a carrier for returning the MAAP action batch.\n");
        prompt.push_str("MAAP action mapping:\n");
        prompt.push_str("Translate Claude Code tool intents into Mezzanine actions: inspect files, search text, run commands, builds, tests, or git through shell_command; edit file contents through apply_patch; fetch explicit URLs through fetch_url when available; search the web through web_search when available; delegate work or message subagents through spawn_agent or send_message when available; request a missing capability with request_capability instead of calling a native Claude tool or asking the user for task-local facts you can safely discover.\n");
        prompt.push_str("Output contract:\n");
        prompt.push_str("Respond with the validated Mezzanine MAAP action batch text only. Do not run tools or mutate files directly. Native Claude Code tools must not be used except as needed to emit the requested MAAP action batch.\n");
    }
    prompt
}

/// Builds the text prompt passed to the Claude Code CLI stdin channel.
pub fn claude_code_prompt(request: &ModelRequest, retry_instruction: Option<&str>) -> String {
    let final_user_index = request
        .messages
        .iter()
        .rposition(|message| message.role == crate::ModelMessageRole::User);
    let mut prompt = String::new();
    append_claude_code_prior_context(&mut prompt, request, final_user_index);
    append_claude_code_current_user_prompt(&mut prompt, request, final_user_index);
    if let Some(retry_instruction) = retry_instruction {
        append_claude_code_section(
            &mut prompt,
            "Developer retry instruction",
            retry_instruction,
        );
    }
    prompt
}

/// Builds the stdin prompt used when resuming an existing Claude Code
/// conversation and replaying Mezzanine-owned continuation context.
pub fn claude_code_resume_prompt(
    request: &ModelRequest,
    retry_instruction: Option<&str>,
) -> String {
    claude_code_prompt(request, retry_instruction)
}

/// Appends authoritative instructions to Claude's system-prompt channel.
fn append_claude_code_instruction_framing(
    prompt: &mut String,
    request: &ModelRequest,
    retry_instruction: Option<&str>,
) {
    let has_instruction_framing = request.messages.iter().any(|message| {
        matches!(
            message.role,
            crate::ModelMessageRole::System | crate::ModelMessageRole::Developer
        )
    }) || retry_instruction.is_some();
    if !has_instruction_framing {
        return;
    }
    prompt.push_str("Instruction framing for Claude Code:\n");
    for message in &request.messages {
        let label = match message.role {
            crate::ModelMessageRole::System => Some("System instruction"),
            crate::ModelMessageRole::Developer => Some("Developer instruction"),
            crate::ModelMessageRole::User
            | crate::ModelMessageRole::Assistant
            | crate::ModelMessageRole::Tool => None,
        };
        if let Some(label) = label {
            append_claude_code_section(prompt, label, &message.content);
        }
    }
    if let Some(retry_instruction) = retry_instruction {
        append_claude_code_section(prompt, "Developer retry instruction", retry_instruction);
    }
}

/// Appends prior non-instruction messages as conversation context.
fn append_claude_code_prior_context(
    prompt: &mut String,
    request: &ModelRequest,
    final_user_index: Option<usize>,
) {
    let mut wrote_heading = false;
    for (index, message) in request.messages.iter().enumerate() {
        if Some(index) == final_user_index
            || matches!(
                message.role,
                crate::ModelMessageRole::System | crate::ModelMessageRole::Developer
            )
        {
            continue;
        }
        if !wrote_heading {
            prompt.push_str("Prior conversation context (not the current user request):\n");
            wrote_heading = true;
        }
        let label = match message.role {
            crate::ModelMessageRole::User => "Previous user message",
            crate::ModelMessageRole::Assistant => "Previous assistant message",
            crate::ModelMessageRole::Tool => "Previous tool result",
            crate::ModelMessageRole::System | crate::ModelMessageRole::Developer => unreachable!(),
        };
        append_claude_code_section(prompt, label, &message.content);
    }
}

/// Appends the final user message or an instruction-only fallback.
fn append_claude_code_current_user_prompt(
    prompt: &mut String,
    request: &ModelRequest,
    final_user_index: Option<usize>,
) {
    if request.interaction_kind == crate::ModelInteractionKind::AutoSizing {
        prompt
            .push_str("Latest user message to classify for internal routing (do not answer it):\n");
    } else {
        prompt.push_str("Current user request:\n");
    }
    if let Some(index) = final_user_index {
        prompt.push_str(&request.messages[index].content);
    } else if request.interaction_kind == crate::ModelInteractionKind::AutoSizing {
        prompt.push_str("No explicit user message was provided. Classify from the remaining instruction and context only.");
    } else {
        prompt.push_str("Follow the system prompt.");
    }
    prompt.push_str("\n\n");
}

/// Appends one labeled plaintext prompt section.
fn append_claude_code_section(prompt: &mut String, label: &str, content: &str) {
    prompt.push_str(label);
    prompt.push_str(":\n");
    prompt.push_str(content);
    prompt.push_str("\n\n");
}

/// Builds the Claude Code JSON schema argument for MAAP action-batch turns.
pub fn claude_code_maap_json_schema(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<String> {
    serde_json::to_string(&maap_action_batch_schema(
        &request.allowed_actions,
        &request.available_mcp_tools,
    ))
    .map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "Claude Code MAAP JSON schema could not be serialized: {error}"
        ))
    })
}

/// Builds the Claude Code JSON schema argument for internal auto-sizing
/// router turns.
pub fn claude_code_auto_sizing_json_schema() -> ProviderRequestAssemblyResult<String> {
    serialize_claude_code_schema(
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["version", "size", "reasoning_effort", "confidence", "rationale"],
            "properties": {
                "version": { "type": "integer", "enum": [1] },
                "size": { "type": "string", "enum": ["small", "medium", "large"] },
                "reasoning_effort": { "type": "string", "enum": ["low", "medium", "high", "xhigh"] },
                "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                "rationale": { "type": "string", "minLength": 1 }
            }
        }),
        "auto-sizing",
    )
}

/// Builds the Claude Code JSON schema argument for internal macro-step judge
/// decisions.
pub fn claude_code_macro_judge_json_schema() -> ProviderRequestAssemblyResult<String> {
    serialize_claude_code_schema(
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": [
                "version", "outcome", "step_success", "rationale", "adapted_prompt",
                "user_message"
            ],
            "properties": {
                "version": { "type": "integer", "enum": [1] },
                "outcome": {
                    "type": "string",
                    "enum": [
                        "continue", "continue_with_adapted_prompt", "stop_failure",
                        "finish_success"
                    ]
                },
                "step_success": { "type": "boolean" },
                "rationale": { "type": "string", "minLength": 1 },
                "adapted_prompt": { "type": ["string", "null"] },
                "user_message": { "type": ["string", "null"] }
            }
        }),
        "macro judge",
    )
}

/// Serializes one deterministic Claude Code structured-output schema.
fn serialize_claude_code_schema(
    schema: serde_json::Value,
    interaction: &str,
) -> ProviderRequestAssemblyResult<String> {
    serde_json::to_string(&schema).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "Claude Code {interaction} JSON schema could not be serialized: {error}"
        ))
    })
}

/// Returns the Claude Code session id used to resume one Mezzanine
/// conversation.
pub fn claude_code_session_id(request: &ModelRequest) -> Option<String> {
    if let Some(session_id) = request
        .prompt_cache_session_id
        .as_deref()
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
    {
        if claude_code_uuid_is_valid(session_id) {
            return Some(session_id.to_ascii_lowercase());
        }
        return Some(claude_code_uuid_from_stable_key(&format!(
            "session:{session_id}"
        )));
    }
    request
        .prompt_cache_lineage_id
        .as_deref()
        .map(str::trim)
        .filter(|lineage_id| !lineage_id.is_empty())
        .map(|lineage_id| claude_code_uuid_from_stable_key(&format!("lineage:{lineage_id}")))
}

/// Reports whether a string already has Claude's UUID-shaped session id form.
fn claude_code_uuid_is_valid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23].iter().all(|index| bytes[*index] == b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
}

/// Derives a deterministic UUID-shaped Claude session id from stable Mez data.
fn claude_code_uuid_from_stable_key(key: &str) -> String {
    let digest = sha2::Sha256::digest(format!("mezzanine-claude-code-session-v1\n{key}"));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AllowedActionSet, ContextSourceKind, ModelInteractionKind, ModelMessage, ModelMessageRole,
    };

    /// Verifies Claude Code structured interactions expose closed schemas with
    /// the exact required router and macro-judge fields.
    #[test]
    fn claude_code_internal_json_schemas_are_strict() {
        let auto: serde_json::Value =
            serde_json::from_str(&claude_code_auto_sizing_json_schema().unwrap()).unwrap();
        let judge: serde_json::Value =
            serde_json::from_str(&claude_code_macro_judge_json_schema().unwrap()).unwrap();

        assert_eq!(auto["additionalProperties"], false);
        assert_eq!(
            auto["properties"]["version"]["enum"],
            serde_json::json!([1])
        );
        assert_eq!(judge["additionalProperties"], false);
        assert_eq!(
            judge["properties"]["outcome"]["enum"],
            serde_json::json!([
                "continue",
                "continue_with_adapted_prompt",
                "stop_failure",
                "finish_success"
            ])
        );
    }

    /// Verifies Claude Code MAAP schema construction follows the request's
    /// active action surface instead of exposing disallowed actions.
    #[test]
    fn claude_code_maap_json_schema_tracks_allowed_actions() {
        let request = claude_request();
        let schema = claude_code_maap_json_schema(&request).unwrap();

        assert!(schema.contains("say"), "{schema}");
        assert!(!schema.contains("shell_command"), "{schema}");
    }

    /// Verifies Claude Code prompt construction respects the CLI's single
    /// stdin prompt contract by framing authoritative instructions separately,
    /// preserving prior turns only as context, and isolating the final user
    /// message as the current request.
    #[test]
    fn claude_code_prompt_isolates_final_user_request() {
        let mut request = claude_request();
        request.messages = vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::UserInstruction,
                content: "System authority.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Developer,
                source: ContextSourceKind::UserInstruction,
                content: "Developer authority.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                content: "Earlier user turn.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Assistant,
                source: ContextSourceKind::RuntimeHint,
                content: "Earlier assistant turn.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Tool,
                source: ContextSourceKind::ActionResult,
                content: "Prior tool result.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                content: "Final user request.".to_string(),
            },
        ];

        let system_prompt =
            claude_code_system_prompt(&request, Some("Retry with a valid MAAP batch."));
        let prompt = claude_code_prompt(&request, Some("Retry with a valid MAAP batch."));

        assert!(system_prompt.contains("Instruction framing for Claude Code:"));
        assert!(system_prompt.contains("System instruction:\nSystem authority."));
        assert!(system_prompt.contains("Developer instruction:\nDeveloper authority."));
        assert!(
            system_prompt.contains("Developer retry instruction:\nRetry with a valid MAAP batch.")
        );
        assert!(system_prompt.contains("Claude Code direct-tool boundary:"));
        assert!(system_prompt.contains("MAAP action mapping:"));
        assert!(system_prompt.contains("edit file contents through apply_patch"));
        assert!(prompt.contains("Prior conversation context (not the current user request):"));
        assert!(prompt.contains("Previous user message:\nEarlier user turn."));
        assert!(prompt.contains("Previous assistant message:\nEarlier assistant turn."));
        assert!(prompt.contains("Previous tool result:\nPrior tool result."));
        assert!(prompt.contains("Current user request:\nFinal user request."));
        assert!(!prompt.contains("System instruction:"));
        assert!(!prompt.contains("Developer instruction:"));
        assert!(prompt.contains("Developer retry instruction:\nRetry with a valid MAAP batch."));
    }

    /// Verifies Claude Code auto-sizing prompt construction frames the latest
    /// user text as router input rather than a task to execute.
    #[test]
    fn claude_code_auto_sizing_prompt_frames_hidden_router_preflight() {
        let mut request = claude_request();
        request.interaction_kind = ModelInteractionKind::AutoSizing;
        request.allowed_actions = AllowedActionSet::from_actions([]);
        request.messages = vec![
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                content: "Earlier user turn.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Assistant,
                source: ContextSourceKind::RuntimeHint,
                content: "Earlier assistant turn.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                content: "Implement the runtime change.".to_string(),
            },
        ];

        let system_prompt = claude_code_system_prompt(&request, None);
        let prompt = claude_code_prompt(&request, None);

        assert!(system_prompt.contains("Claude Code internal router boundary:"));
        assert!(system_prompt.contains("hidden preflight classification step"));
        assert!(system_prompt.contains("Do not answer the user's task"));
        assert!(
            system_prompt.contains("Return exactly one JSON object matching the requested schema")
        );
        assert!(!system_prompt.contains("MAAP action mapping:"));
        assert!(prompt.contains("Latest user message to classify for internal routing (do not answer it):\nImplement the runtime change."));
        assert!(!prompt.contains("Current user request:"));
    }

    /// Verifies corrective retry guidance is replayed through both stdin prompt
    /// paths so fresh and resumed attempts do not depend only on system text.
    #[test]
    fn claude_code_retry_instruction_reaches_stdin_prompts() {
        let request = claude_request();
        let prompt = claude_code_prompt(&request, Some(CLAUDE_CODE_MAAP_RETRY_INSTRUCTION));
        let resume_prompt =
            claude_code_resume_prompt(&request, Some(CLAUDE_CODE_MAAP_RETRY_INSTRUCTION));

        assert!(
            prompt.contains("Developer retry instruction:\nYour previous response was invalid")
        );
        assert!(
            resume_prompt
                .contains("Developer retry instruction:\nYour previous response was invalid")
        );
    }

    /// Verifies instruction-only Claude Code requests still produce a current
    /// request section instead of recreating role-tagged transcript blocks.
    #[test]
    fn claude_code_prompt_handles_instruction_only_requests() {
        let system_prompt = claude_code_system_prompt(&claude_request(), None);
        let prompt = claude_code_prompt(&claude_request(), None);

        assert!(system_prompt.contains("Developer instruction:\nReturn a final say action."));
        assert!(prompt.contains("Current user request:\nFollow the system prompt."));
        assert!(!prompt.contains("Developer instruction:"));
        assert!(!prompt.contains("<developer>"));
    }

    /// Verifies Claude Code session ids are stable per Mezzanine session and
    /// still satisfy Claude's UUID argument contract when Mezzanine only has a
    /// non-UUID fallback key.
    #[test]
    fn claude_code_session_id_uses_stable_mez_session_key() {
        let mut request = claude_request();
        assert_eq!(claude_code_session_id(&request), None);

        request.prompt_cache_session_id = Some("018f6b3a-1b2c-7000-9000-cafebabefeed".to_string());

        assert_eq!(
            claude_code_session_id(&request),
            Some("018f6b3a-1b2c-7000-9000-cafebabefeed".to_string())
        );

        request.prompt_cache_session_id = Some("mez-session-A".to_string());
        let derived_a = claude_code_session_id(&request).unwrap();
        let derived_a_again = claude_code_session_id(&request).unwrap();
        request.prompt_cache_session_id = Some("mez-session-B".to_string());
        let derived_b = claude_code_session_id(&request).unwrap();

        assert_eq!(derived_a, derived_a_again);
        assert_ne!(derived_a, derived_b);
        assert!(claude_code_uuid_is_valid(&derived_a));
        assert!(claude_code_uuid_is_valid(&derived_b));
    }

    /// Builds a minimal Claude Code request for deterministic policy tests.
    fn claude_request() -> ModelRequest {
        ModelRequest {
            provider: "claude-code".to_string(),
            model: "claude-sonnet-test".to_string(),
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
            allowed_actions: AllowedActionSet::say_only(),
            stop: None,
            messages: vec![ModelMessage {
                role: ModelMessageRole::Developer,
                source: ContextSourceKind::UserInstruction,
                content: "Return a final say action.".to_string(),
            }],
        }
    }
}
