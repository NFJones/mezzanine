//! Durable compaction-summary identity shared by replay producers and consumers.
//!
//! The product replay path persists its prefix summary with a memory UUID label
//! derived from the conversation's compaction identity. Consumers of the shared
//! classifier must agree on that identity, and an ordinary memory must never be
//! promoted to a summary epoch just because it carries a UUID label.

use super::super::{ContextBlock, ContextSourceKind, context_block_is_compaction_summary};

/// Builds the exact durable replay summary label the product producer emits.
fn durable_replay_summary_label(conversation_id: &str) -> String {
    format!(
        "memory {} (conversation)",
        crate::memory::canonical_memory_uuid(&format!("compact-{conversation_id}"))
    )
}

/// A replay-persisted compaction summary carries explicit prefix provenance
/// alongside the unchanged durable UUID label. The actual product producer is
/// exercised separately; ordinary memories with that label shape stay neutral.
#[test]
fn durable_replay_compaction_summary_is_recognized() {
    let block = ContextBlock::reference_event(
        ContextSourceKind::CompactionPrefix,
        durable_replay_summary_label("conversation-1"),
        "persisted prefix summary",
    );
    assert!(
        context_block_is_compaction_summary(&block),
        "durable replay label was not recognized: {}",
        block.label
    );
}

/// An ordinary UUID memory must never become a summary epoch.
#[test]
fn ordinary_uuid_memory_labels_are_not_compaction_summaries() {
    for label in [
        format!("memory {} (pinned note)", crate::memory::new_memory_uuid()),
        format!("memory {} (conversation)", crate::memory::new_memory_uuid()),
    ] {
        let block = ContextBlock::reference_event(ContextSourceKind::Memory, label.clone(), "note");
        assert!(
            !context_block_is_compaction_summary(&block),
            "ordinary memory was treated as a summary: {label}"
        );
    }
}

/// Compatible legacy summary labels must stay recognized while that gap is closed.
#[test]
fn legacy_compaction_summary_labels_remain_recognized() {
    for label in [
        "context compaction summary",
        "conversation compaction notice",
        "memory compact-legacy",
    ] {
        let block = ContextBlock::reference_event(ContextSourceKind::Memory, label, "summary");
        assert!(
            context_block_is_compaction_summary(&block),
            "legacy summary label was lost: {label}"
        );
    }
}

/// Explicit prefix and selective identities must survive provider projection
/// while preserving the legacy Memory framing, trust and reference semantics.
/// Ordinary UUID messages remain non-summaries even with identical content.
#[test]
fn typed_summary_identity_survives_projection_without_changing_visible_bytes() {
    use crate::{ModelMessage, ModelMessageRole, model_message_is_compaction_summary};

    let ordinary = ContextBlock::reference_event(
        ContextSourceKind::Memory,
        durable_replay_summary_label("projection-conversation"),
        "unchanged summary bytes",
    );
    for source in [
        ContextSourceKind::CompactionPrefix,
        ContextSourceKind::CompactionSelective,
    ] {
        let mut summary = ordinary.clone();
        summary.source = source;
        assert!(context_block_is_compaction_summary(&summary));
        assert_eq!(summary.semantic_kind(), ordinary.semantic_kind());
        assert_eq!(summary.retention(), ordinary.retention());
        assert_eq!(summary.trust_domain(), ordinary.trust_domain());
        assert_eq!(
            crate::model_context_block_header(&summary),
            crate::model_context_block_header(&ordinary)
        );
        let mut message = ModelMessage {
            role: ModelMessageRole::Context,
            source,
            placement: summary.placement,
            content: format!(
                "{}{}",
                crate::model_context_block_header(&summary),
                summary.content
            ),
        };
        assert!(model_message_is_compaction_summary(&message));
        message.source = ContextSourceKind::Memory;
        assert!(!model_message_is_compaction_summary(&message));
        message.source = ContextSourceKind::TranscriptAssistant;
        assert!(!model_message_is_compaction_summary(&message));
    }
}
