//! Ordinary provider-request evidence preparation.
//!
//! Normal request assembly should preserve provider-visible history exactly as
//! it was observed. Silent reordering, deduplication, or summary rewriting
//! harms prompt-cache continuity because the next request no longer looks like
//! the previous request plus appended tail content. Compaction remains the
//! explicit path that may rewrite old history under pressure.

use super::ContextBlock;

/// Prepares context blocks for ordinary provider requests without rewriting
/// their observed order or content.
pub(super) fn prepare_model_context_blocks(blocks: Vec<ContextBlock>) -> Vec<ContextBlock> {
    blocks
}
