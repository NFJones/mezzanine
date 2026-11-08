//! Exact reference rendering for a durable interrupted-turn event.
//!
//! Producers, replay and immutable source qualification share these bytes. The
//! result carries no task admission, execution closure or tool authority.

use crate::{ContextBlock, ContextPlacement, ContextSourceKind};

/// Renders original prompt, interruption reason and settled observations as one
/// non-executable runtime reference. Inputs retain exact Unicode and newlines;
/// empty observations get the established explicit no-result text. This pure
/// rendering neither restores instructions nor acknowledges any external effect.
pub fn interrupted_turn_context_block(
    prompt: &str,
    reason: &str,
    evidence: &[String],
) -> ContextBlock {
    let evidence = if evidence.is_empty() {
        "no action result was available when the turn stopped".to_owned()
    } else {
        evidence.join("\n")
    };
    ContextBlock {
        source: ContextSourceKind::RuntimeHint,
        placement: ContextPlacement::ConversationAppend,
        label: "interrupted turn context".to_owned(),
        content: format!(
            "The prior turn was interrupted before completion. Continue or redirect that work using its original user intent:\n{prompt}\n\nreason={reason}\nobserved action state:\n{evidence}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rendering changes would invalidate original records even if every owner
    /// changed together. Preserve the established bytes and literal role.
    #[test]
    fn interrupted_reference_preserves_original_semantic_bytes() {
        let block = interrupted_turn_context_block(
            "prompt 雪\nnext",
            "cancelled",
            &[
                "first observed result".into(),
                "second observed result".into(),
            ],
        );
        assert_eq!(block.source, ContextSourceKind::RuntimeHint);
        assert_eq!(block.placement, ContextPlacement::ConversationAppend);
        assert_eq!(block.label, "interrupted turn context");
        assert_eq!(
            block.content,
            "The prior turn was interrupted before completion. Continue or redirect that work using its original user intent:\nprompt 雪\nnext\n\nreason=cancelled\nobserved action state:\nfirst observed result\nsecond observed result"
        );
        let empty = interrupted_turn_context_block("prompt", "stopped", &[]);
        assert!(empty.content.ends_with(
            "observed action state:\nno action result was available when the turn stopped"
        ));
    }
}
