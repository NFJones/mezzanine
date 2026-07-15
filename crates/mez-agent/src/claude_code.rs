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
    use crate::{AllowedActionSet, ModelInteractionKind};

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
            messages: Vec::new(),
        }
    }
}
