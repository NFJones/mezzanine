//! Product-independent MMP message action lowering and approval identity.
//!
//! This module lowers one model-planned `send_message` action into the
//! permission-facing pseudo command evaluated by product message-recipient
//! rules plus the bounded, redacted payload identity carried by a resumable
//! send approval. It performs no delivery and owns no runtime state.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::{AgentAction, AgentActionPayload, shell_quote};

/// Maximum bytes of message text included in one send-approval preview.
pub const MESSAGE_APPROVAL_PREVIEW_BYTES: usize = 200;

/// Lowered MMP message action used for permission planning and presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageActionPlan {
    /// Recipient exactly as the model supplied it.
    pub recipient: String,
    /// User-facing summary of the message action.
    pub summary: String,
    /// Shell-shaped pseudo command evaluated by product recipient rules.
    ///
    /// The recipient is shell-quoted so a hostile recipient string cannot add
    /// candidates, operators, or redirects to the analyzed command.
    pub policy_command: String,
}

/// Lowers one `send_message` action into its message action plan.
pub fn message_action_plan(action: &AgentAction) -> Option<MessageActionPlan> {
    let AgentActionPayload::SendMessage { recipient, .. } = &action.payload else {
        return None;
    };
    Some(MessageActionPlan {
        recipient: recipient.clone(),
        summary: format!(
            "I’ll send a message to `{}`.",
            message_payload_preview(recipient)
        ),
        policy_command: message_action_policy_command(recipient),
    })
}

/// Returns the permission-facing pseudo command for one message recipient.
pub fn message_action_policy_command(recipient: &str) -> String {
    format!("send_message {}", shell_quote(recipient))
}

/// Returns the stable payload identity digest binding one send approval.
///
/// The digest covers the content type and the full payload, so an approval is
/// only resumable while both remain identical.
pub fn message_payload_digest(content_type: &str, payload: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"mez-send-message-sha256-v1\0");
    hasher.update(content_type.as_bytes());
    hasher.update([0u8]);
    hasher.update(payload.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

/// Returns the bounded, redacted preview carried by one send approval.
///
/// Message text is untrusted peer data: whitespace and control characters are
/// collapsed, the value is truncated on a UTF-8 boundary to
/// [`MESSAGE_APPROVAL_PREVIEW_BYTES`], and a truncation marker is appended so a
/// reviewer can see that more text exists without reading it.
pub fn message_payload_preview(payload: &str) -> String {
    const MARKER: &str = "...";
    let limit = MESSAGE_APPROVAL_PREVIEW_BYTES.saturating_sub(MARKER.len());
    let mut preview = String::with_capacity(payload.len().min(limit));
    let mut pending_space = false;
    let mut truncated = false;
    for character in payload.chars() {
        if character.is_whitespace() || character.is_control() {
            pending_space = !preview.is_empty();
            continue;
        }
        if pending_space {
            if preview.len() + 1 + character.len_utf8() > limit {
                truncated = true;
                break;
            }
            preview.push(' ');
            pending_space = false;
        }
        if preview.len() + character.len_utf8() > limit {
            truncated = true;
            break;
        }
        preview.push(character);
    }
    if truncated {
        preview.push_str(MARKER);
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentAction;

    /// Builds one message action for message-action lowering tests.
    fn message_action(recipient: &str, payload: &str) -> AgentAction {
        AgentAction {
            id: "message-1".to_string(),
            payload: AgentActionPayload::SendMessage {
                recipient: recipient.to_string(),
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: payload.to_string(),
                correlation_id: None,
            },
        }
    }

    /// Verifies message lowering keeps the recipient and quotes the permission
    /// pseudo command so hostile recipient text cannot add shell structure.
    #[test]
    fn message_action_plan_quotes_recipient_for_policy() {
        let action = message_action("role:reviewer; rm -rf /tmp/x", "hello");
        let plan = message_action_plan(&action).expect("message plan");

        assert_eq!(plan.recipient, "role:reviewer; rm -rf /tmp/x");
        assert_eq!(
            plan.policy_command,
            "send_message 'role:reviewer; rm -rf /tmp/x'"
        );
        assert!(plan.summary.contains("role:reviewer"));
    }

    /// Verifies approval previews are bounded, control-free, and truncation
    /// marked while the payload digest still covers the full payload.
    #[test]
    fn message_approval_identity_is_bounded_and_payload_bound() {
        let oversized = "é".repeat(MESSAGE_APPROVAL_PREVIEW_BYTES);
        let preview = message_payload_preview(&oversized);
        assert!(preview.len() <= MESSAGE_APPROVAL_PREVIEW_BYTES);
        assert!(preview.ends_with("..."));

        let controlled = message_payload_preview("line one\n\tline two\u{0}");
        assert_eq!(controlled, "line one line two");

        let first = message_payload_digest("text/plain; charset=utf-8", "payload");
        let second = message_payload_digest("text/plain; charset=utf-8", "payload ");
        assert_eq!(first.len(), 64);
        assert_ne!(first, second);
    }
}
