//! Provider-neutral immutable-context continuity diagnostics.
//!
//! This module fingerprints only lifecycle metadata and cryptographic digests;
//! it never retains prompt text. Runtime adapters can compare consecutive
//! finalized contexts to distinguish expected turn, compaction, and provider
//! transitions from an unexpected rewrite of settled chronology.

use sha2::{Digest, Sha256};

use crate::{AgentContext, ContextBlock, ContextPlacement, ContextSourceKind};

/// Sensitive-content-free digest of one immutable context block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImmutableContextBlockDigest {
    /// Explicit lifecycle placement of the block.
    pub placement: ContextPlacement,
    /// Context provenance retained for diagnostic classification.
    pub source: ContextSourceKind,
    /// Canonical encoded byte length before hashing.
    pub bytes: usize,
    /// Best-effort model token estimate for this block.
    pub token_estimate: usize,
    /// SHA-256 of the canonical block encoding.
    pub sha256: String,
}

/// Sensitive-content-free snapshot of one finalized provider-bound context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextContinuitySnapshot {
    /// Provider selected for the request.
    pub provider: String,
    /// Model selected for the request.
    pub model: String,
    /// Logical runtime turn that owns the request.
    pub turn_id: String,
    /// Ordered stable and conversation block digests.
    pub immutable_blocks: Vec<ImmutableContextBlockDigest>,
    /// Best-effort token estimate for stable and conversation material.
    pub immutable_token_estimate: usize,
    /// Best-effort token estimate for regenerated ephemeral material.
    pub volatile_token_estimate: usize,
    /// Canonical byte length of the immutable projection.
    pub stable_projection_bytes: usize,
    /// SHA-256 of the complete immutable projection.
    pub stable_projection_sha256: String,
    /// Whether the current immutable projection contains a compaction epoch.
    pub contains_compaction_epoch: bool,
}

/// Classified reason for the latest context-continuity transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextContinuityBreakReason {
    /// No earlier comparable request exists or a new logical turn began.
    NewTurn,
    /// Immutable chronology was intentionally replaced by a compaction epoch.
    Compaction,
    /// The request moved to a different provider.
    ProviderSwitch,
    /// The request moved to another model within the same provider.
    ModelSwitch,
    /// Immutable chronology only retained its prefix and appended new blocks.
    AppendOnly,
    /// Settled chronology changed without an expected lifecycle explanation.
    UnexpectedRewrite,
}

impl ContextContinuityBreakReason {
    /// Returns the stable status and trace spelling for this classification.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NewTurn => "new_turn",
            Self::Compaction => "compaction",
            Self::ProviderSwitch => "provider_switch",
            Self::ModelSwitch => "model_switch",
            Self::AppendOnly => "append_only",
            Self::UnexpectedRewrite => "unexpected_rewrite",
        }
    }
}

/// Comparison result for one provider-bound context and its predecessor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextContinuityDiagnostics {
    /// Current sensitive-content-free context snapshot.
    pub snapshot: ContextContinuitySnapshot,
    /// Number of identical immutable blocks at the front.
    pub common_immutable_prefix_blocks: usize,
    /// Best-effort tokens covered by the identical immutable block prefix.
    pub common_immutable_prefix_tokens: usize,
    /// Whether all prior immutable blocks remain byte-identical in order.
    pub immutable_append_only: bool,
    /// Lifecycle classification for the transition.
    pub break_reason: ContextContinuityBreakReason,
}

/// Builds and compares provider-neutral continuity diagnostics.
pub fn context_continuity_diagnostics(
    context: &AgentContext,
    provider: &str,
    model: &str,
    turn_id: &str,
    previous: Option<&ContextContinuitySnapshot>,
) -> ContextContinuityDiagnostics {
    let snapshot = context_continuity_snapshot(context, provider, model, turn_id);
    let Some(previous) = previous else {
        return ContextContinuityDiagnostics {
            snapshot,
            common_immutable_prefix_blocks: 0,
            common_immutable_prefix_tokens: 0,
            immutable_append_only: false,
            break_reason: ContextContinuityBreakReason::NewTurn,
        };
    };
    let common_immutable_prefix_blocks = previous
        .immutable_blocks
        .iter()
        .zip(&snapshot.immutable_blocks)
        .take_while(|(previous, current)| previous.sha256 == current.sha256)
        .count();
    let common_immutable_prefix_tokens = previous
        .immutable_blocks
        .iter()
        .take(common_immutable_prefix_blocks)
        .fold(0usize, |total, block| {
            total.saturating_add(block.token_estimate)
        });
    let immutable_append_only = common_immutable_prefix_blocks == previous.immutable_blocks.len()
        && snapshot.immutable_blocks.len() >= previous.immutable_blocks.len();
    let break_reason = if previous.provider != snapshot.provider {
        ContextContinuityBreakReason::ProviderSwitch
    } else if previous.model != snapshot.model {
        ContextContinuityBreakReason::ModelSwitch
    } else if !immutable_append_only && snapshot.contains_compaction_epoch {
        ContextContinuityBreakReason::Compaction
    } else if previous.turn_id != snapshot.turn_id {
        ContextContinuityBreakReason::NewTurn
    } else if immutable_append_only {
        ContextContinuityBreakReason::AppendOnly
    } else {
        ContextContinuityBreakReason::UnexpectedRewrite
    };
    ContextContinuityDiagnostics {
        snapshot,
        common_immutable_prefix_blocks,
        common_immutable_prefix_tokens,
        immutable_append_only,
        break_reason,
    }
}

/// Builds a sensitive-content-free context continuity snapshot.
pub fn context_continuity_snapshot(
    context: &AgentContext,
    provider: &str,
    model: &str,
    turn_id: &str,
) -> ContextContinuitySnapshot {
    let mut immutable_blocks = Vec::new();
    let mut immutable_projection = Vec::new();
    let mut immutable_token_estimate = 0usize;
    let mut volatile_token_estimate = 0usize;
    let mut contains_compaction_epoch = false;
    for block in &context.blocks {
        let token_estimate = context_block_token_estimate(block);
        if block.placement == ContextPlacement::EphemeralTail {
            volatile_token_estimate = volatile_token_estimate.saturating_add(token_estimate);
            continue;
        }
        let canonical = canonical_context_block_bytes(block);
        immutable_projection.extend_from_slice(&(canonical.len() as u64).to_be_bytes());
        immutable_projection.extend_from_slice(&canonical);
        immutable_token_estimate = immutable_token_estimate.saturating_add(token_estimate);
        contains_compaction_epoch |= context_block_is_compaction_epoch(block);
        immutable_blocks.push(ImmutableContextBlockDigest {
            placement: block.placement,
            source: block.source,
            bytes: canonical.len(),
            token_estimate,
            sha256: sha256_hex(&canonical),
        });
    }
    ContextContinuitySnapshot {
        provider: provider.to_string(),
        model: model.to_string(),
        turn_id: turn_id.to_string(),
        immutable_blocks,
        immutable_token_estimate,
        volatile_token_estimate,
        stable_projection_bytes: immutable_projection.len(),
        stable_projection_sha256: sha256_hex(&immutable_projection),
        contains_compaction_epoch,
    }
}

/// Returns a deterministic best-effort token estimate for one rendered block.
fn context_block_token_estimate(block: &ContextBlock) -> usize {
    format!("[{}]\n{}", block.label, block.content)
        .split_whitespace()
        .count()
}

/// Encodes one block with explicit lengths so concatenation is unambiguous.
fn canonical_context_block_bytes(block: &ContextBlock) -> Vec<u8> {
    let placement = format!("{:?}", block.placement);
    let source = format!("{:?}", block.source);
    let mut encoded = Vec::new();
    for field in [
        placement.as_bytes(),
        source.as_bytes(),
        block.label.as_bytes(),
        block.content.as_bytes(),
    ] {
        encoded.extend_from_slice(&(field.len() as u64).to_be_bytes());
        encoded.extend_from_slice(field);
    }
    encoded
}

/// Returns whether one immutable block denotes a completed compaction epoch.
fn context_block_is_compaction_epoch(block: &ContextBlock) -> bool {
    block.label == "conversation compaction notice"
        || (block.source == ContextSourceKind::Memory
            && block.content.starts_with("[context compacted]"))
}

/// Returns a lower-case SHA-256 digest.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one context block for continuity regression scenarios.
    fn block(
        placement: ContextPlacement,
        source: ContextSourceKind,
        label: &str,
        content: &str,
    ) -> ContextBlock {
        ContextBlock {
            placement,
            source,
            label: label.to_string(),
            content: content.to_string(),
        }
    }

    /// Verifies append-only chronology preserves the complete immutable prefix.
    #[test]
    fn context_continuity_reports_append_only_immutable_growth() {
        let initial = AgentContext::new(vec![block(
            ContextPlacement::StablePrefix,
            ContextSourceKind::System,
            "system",
            "follow policy",
        )])
        .unwrap();
        let previous = context_continuity_snapshot(&initial, "openai", "gpt", "turn-1");
        let appended = AgentContext::new(vec![
            initial.blocks[0].clone(),
            block(
                ContextPlacement::ConversationAppend,
                ContextSourceKind::TranscriptUser,
                "user",
                "run tests",
            ),
            block(
                ContextPlacement::EphemeralTail,
                ContextSourceKind::RuntimeHint,
                "runtime",
                "running",
            ),
        ])
        .unwrap();

        let diagnostics =
            context_continuity_diagnostics(&appended, "openai", "gpt", "turn-1", Some(&previous));

        assert_eq!(
            diagnostics.break_reason,
            ContextContinuityBreakReason::AppendOnly
        );
        assert!(diagnostics.immutable_append_only);
        assert_eq!(diagnostics.common_immutable_prefix_blocks, 1);
        assert!(diagnostics.snapshot.immutable_token_estimate > 0);
        assert!(diagnostics.snapshot.volatile_token_estimate > 0);
        assert_eq!(diagnostics.snapshot.stable_projection_sha256.len(), 64);
    }

    /// Verifies expected lifecycle changes are distinguished from rewrites.
    #[test]
    fn context_continuity_classifies_compaction_switches_and_rewrites() {
        let initial = AgentContext::new(vec![block(
            ContextPlacement::ConversationAppend,
            ContextSourceKind::TranscriptUser,
            "history",
            "original chronology",
        )])
        .unwrap();
        let previous = context_continuity_snapshot(&initial, "openai", "gpt-a", "turn-1");
        let compacted = AgentContext::new(vec![block(
            ContextPlacement::ConversationAppend,
            ContextSourceKind::Memory,
            "summary",
            "[context compacted] durable summary",
        )])
        .unwrap();
        assert_eq!(
            context_continuity_diagnostics(
                &compacted,
                "openai",
                "gpt-a",
                "turn-1",
                Some(&previous),
            )
            .break_reason,
            ContextContinuityBreakReason::Compaction
        );
        assert_eq!(
            context_continuity_diagnostics(
                &initial,
                "deepseek",
                "chat",
                "turn-1",
                Some(&previous),
            )
            .break_reason,
            ContextContinuityBreakReason::ProviderSwitch
        );
        assert_eq!(
            context_continuity_diagnostics(&initial, "openai", "gpt-b", "turn-1", Some(&previous),)
                .break_reason,
            ContextContinuityBreakReason::ModelSwitch
        );
        let rewritten = AgentContext::new(vec![block(
            ContextPlacement::ConversationAppend,
            ContextSourceKind::TranscriptUser,
            "history",
            "rewritten chronology",
        )])
        .unwrap();
        assert_eq!(
            context_continuity_diagnostics(
                &rewritten,
                "openai",
                "gpt-a",
                "turn-1",
                Some(&previous),
            )
            .break_reason,
            ContextContinuityBreakReason::UnexpectedRewrite
        );
    }
}
