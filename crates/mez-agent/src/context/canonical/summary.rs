//! Summary provenance shared by canonical planning and provider projections.
//!
//! Prefix and selective summaries carry explicit source identities through
//! context copies and model messages. Their labels, content, reference role,
//! trust and framing remain unchanged. Only enumerated legacy Memory labels
//! retain compatibility recognition; an ordinary UUID memory never acquires
//! summary identity from its spelling. This classification grants no execution,
//! storage or permission authority and does not imply a new compaction occurred.

use super::{ContextBlock, ContextSourceKind, ModelMessage};

impl ContextSourceKind {
    /// Reports explicit controller-owned prefix or selective summary provenance.
    /// Legacy labels require the block/message classifiers below.
    pub fn is_compaction_summary(self) -> bool {
        matches!(self, Self::CompactionPrefix | Self::CompactionSelective)
    }

    /// Reports the neutral memory family, including explicitly owned summaries.
    /// This is a presentation/category predicate, never summary authorization.
    pub fn is_memory(self) -> bool {
        self == Self::Memory || self.is_compaction_summary()
    }
}

/// Recognizes explicit summary provenance or a compatible legacy Memory label.
/// UUID labels alone cannot distinguish ordinary memory from durable summaries.
pub fn context_block_is_compaction_summary(block: &ContextBlock) -> bool {
    block.source.is_compaction_summary()
        || (block.source == ContextSourceKind::Memory && legacy_summary_label(&block.label))
}

/// Applies the same summary identity to a projected provider message. Explicit
/// provenance survives framing; legacy Memory messages require the exact
/// bracketed first-line label emitted by canonical projection.
pub fn model_message_is_compaction_summary(message: &ModelMessage) -> bool {
    message.source.is_compaction_summary()
        || (message.source == ContextSourceKind::Memory
            && message
                .content
                .strip_prefix('[')
                .and_then(|content| content.split_once("]\n"))
                .is_some_and(|(label, _)| legacy_summary_label(label)))
}

/// Keeps historical recognition narrowly limited to established summary labels.
fn legacy_summary_label(label: &str) -> bool {
    label == "context compaction summary"
        || label == "conversation compaction notice"
        || label.starts_with("memory compact-")
}
