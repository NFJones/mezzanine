//! Sensitive-content-free OpenAI request continuity diagnostics.
//!
//! This module owns compact digests and comparisons for complete provider-
//! visible requests. It intentionally retains only byte counts, roles, and
//! hashes so runtime metrics can identify cache-affinity regressions without
//! retaining prompt content.

/// Sensitive-content-free digest of one provider-visible OpenAI input message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiRequestMessageDigest {
    /// Ordered provider input index.
    pub index: usize,
    /// Provider-visible message role.
    pub role: String,
    /// Canonical serialized byte count.
    pub bytes: usize,
    /// SHA-256 of the canonical serialized message.
    pub sha256: String,
}

/// Complete provider-visible OpenAI request snapshot without prompt contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiRequestContinuitySnapshot {
    /// Canonical serialized request byte count.
    pub request_bytes: usize,
    /// SHA-256 of all provider-visible cache-affecting request material.
    pub request_sha256: String,
    /// SHA-256 of the front-loaded instructions.
    pub instructions_sha256: String,
    /// SHA-256 of the response format.
    pub response_format_sha256: String,
    /// SHA-256 of the tool definitions.
    pub tools_sha256: String,
    /// SHA-256 of tool choice.
    pub tool_choice_sha256: String,
    /// SHA-256 of request controls outside messages and tools.
    pub request_control_sha256: String,
    /// Ordered digests of every provider-visible input message.
    pub messages: Vec<OpenAiRequestMessageDigest>,
}

/// First provider-visible divergence between two consecutive OpenAI requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiRequestContinuity {
    /// Stable divergence category, or `identical` when requests match exactly.
    pub category: String,
    /// First divergent provider input message index, when message content diverged.
    pub message_index: Option<usize>,
    /// Number of identical ordered input messages at the front.
    pub common_message_prefix: usize,
    /// Whether the current input messages only append to the previous sequence.
    pub messages_append_only: bool,
}

/// Compares two complete provider-visible OpenAI request snapshots.
pub fn compare_openai_request_continuity(
    previous: &OpenAiRequestContinuitySnapshot,
    current: &OpenAiRequestContinuitySnapshot,
) -> OpenAiRequestContinuity {
    let common_message_prefix = previous
        .messages
        .iter()
        .zip(&current.messages)
        .take_while(|(previous, current)| previous.sha256 == current.sha256)
        .count();
    let messages_append_only = common_message_prefix == previous.messages.len()
        && current.messages.len() >= previous.messages.len();
    let (category, message_index) = if previous.instructions_sha256 != current.instructions_sha256 {
        ("instructions", None)
    } else if previous.response_format_sha256 != current.response_format_sha256 {
        ("response_format", None)
    } else if previous.tools_sha256 != current.tools_sha256 {
        ("tools", None)
    } else if previous.tool_choice_sha256 != current.tool_choice_sha256 {
        ("tool_choice", None)
    } else if previous.request_control_sha256 != current.request_control_sha256 {
        ("request_control", None)
    } else if previous.messages != current.messages {
        ("messages", Some(common_message_prefix))
    } else {
        ("identical", None)
    };
    OpenAiRequestContinuity {
        category: category.to_string(),
        message_index,
        common_message_prefix,
        messages_append_only,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        OpenAiRequestContinuity, OpenAiRequestContinuitySnapshot, OpenAiRequestMessageDigest,
        compare_openai_request_continuity,
    };

    /// Verifies complete-request continuity reports the earliest cache-affecting divergence.
    ///
    /// The diagnostic must distinguish request-front changes from append-only
    /// transcript growth without retaining any provider-visible prompt text.
    #[test]
    fn openai_request_continuity_classifies_front_and_message_divergence() {
        let snapshot =
            |instructions: &str, tools: &str, messages: &[&str]| OpenAiRequestContinuitySnapshot {
                request_bytes: 128,
                request_sha256: "request".to_string(),
                instructions_sha256: instructions.to_string(),
                response_format_sha256: "format".to_string(),
                tools_sha256: tools.to_string(),
                tool_choice_sha256: "choice".to_string(),
                request_control_sha256: "control".to_string(),
                messages: messages
                    .iter()
                    .enumerate()
                    .map(|(index, digest)| OpenAiRequestMessageDigest {
                        index,
                        role: "user".to_string(),
                        bytes: digest.len(),
                        sha256: (*digest).to_string(),
                    })
                    .collect(),
            };
        let initial = snapshot("instructions-a", "tools-a", &["message-a"]);
        let appended = snapshot("instructions-a", "tools-a", &["message-a", "message-b"]);
        let replaced = snapshot("instructions-a", "tools-a", &["message-c"]);
        let changed_tools = snapshot("instructions-a", "tools-b", &["message-a"]);
        let changed_instructions = snapshot("instructions-b", "tools-a", &["message-a"]);

        assert_eq!(
            compare_openai_request_continuity(&initial, &appended),
            OpenAiRequestContinuity {
                category: "messages".to_string(),
                message_index: Some(1),
                common_message_prefix: 1,
                messages_append_only: true,
            }
        );
        assert_eq!(
            compare_openai_request_continuity(&initial, &replaced),
            OpenAiRequestContinuity {
                category: "messages".to_string(),
                message_index: Some(0),
                common_message_prefix: 0,
                messages_append_only: false,
            }
        );
        assert_eq!(
            compare_openai_request_continuity(&initial, &changed_tools).category,
            "tools"
        );
        assert_eq!(
            compare_openai_request_continuity(&initial, &changed_instructions).category,
            "instructions"
        );
    }
}
