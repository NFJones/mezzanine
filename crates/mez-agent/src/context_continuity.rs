//! Provider-neutral immutable-context continuity diagnostics.
//!
//! This module fingerprints only lifecycle metadata and cryptographic digests;
//! it never retains prompt text. Runtime adapters can compare consecutive
//! finalized contexts to distinguish expected turn, compaction, and provider
//! transitions from an unexpected rewrite of settled chronology.

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    AgentContext, ContextBlock, ContextPlacement, ContextRetention, ContextSemanticKind,
    ContextSourceKind, ModelInteractionKind, ModelMessageRole, role_for_context_block,
};

/// Sensitive-content-free digest of one immutable context block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImmutableContextBlockDigest {
    /// Explicit lifecycle placement of the block.
    pub placement: ContextPlacement,
    /// Context provenance retained for diagnostic classification.
    pub source: ContextSourceKind,
    /// Model-facing meaning included in continuity identity.
    pub semantic_kind: ContextSemanticKind,
    /// Compaction/retention treatment included in continuity identity.
    pub retention: ContextRetention,
    /// Canonical encoded byte length before hashing.
    pub bytes: usize,
    /// Best-effort model token estimate for this block.
    pub token_estimate: usize,
    /// SHA-256 of the canonical block encoding.
    pub sha256: String,
}

/// Sensitive-content-free diagnostics for one canonical context block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBlockDiagnostics {
    /// Zero-based canonical block index.
    pub index: usize,
    /// Cache-lifecycle placement.
    pub placement: ContextPlacement,
    /// Model-facing semantic classification.
    pub semantic_kind: ContextSemanticKind,
    /// Retention and compaction treatment.
    pub retention: ContextRetention,
    /// Canonical provider-neutral role.
    pub canonical_role: String,
    /// Actual transport role/channel selected for the provider family.
    pub provider_role: String,
    /// Producer provenance.
    pub source: ContextSourceKind,
    /// Hash of source and label only, used as a stable block identity.
    pub block_identity_sha256: String,
    /// Canonical encoded byte length.
    pub bytes: usize,
    /// Best-effort token estimate.
    pub token_estimate: usize,
    /// Whether the block can participate in the reusable request prefix.
    pub reusable_prefix: bool,
    /// Whether the block is discarded after this provider request.
    pub request_local: bool,
    /// SHA-256 of the complete canonical block encoding.
    pub sha256: String,
}

/// Aggregate size and count for one semantic context category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSemanticAggregate {
    /// Semantic category represented by the aggregate.
    pub semantic_kind: ContextSemanticKind,
    /// Number of blocks in the category.
    pub blocks: usize,
    /// Canonical encoded bytes in the category.
    pub bytes: usize,
    /// Best-effort tokens in the category.
    pub token_estimate: usize,
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
    /// Ordered diagnostics for both durable and request-local blocks.
    pub blocks: Vec<ContextBlockDiagnostics>,
    /// Best-effort token estimate for stable and conversation material.
    pub immutable_token_estimate: usize,
    /// Best-effort token estimate for regenerated ephemeral material.
    pub volatile_token_estimate: usize,
    /// Canonical bytes in invariant stable-prefix blocks.
    pub stable_prefix_bytes: usize,
    /// Canonical bytes in immutable append-only chronology.
    pub append_only_bytes: usize,
    /// Canonical bytes in request-local live state.
    pub live_state_bytes: usize,
    /// Best-effort tokens in invariant stable-prefix blocks.
    pub stable_prefix_token_estimate: usize,
    /// Best-effort tokens in immutable append-only chronology.
    pub append_only_token_estimate: usize,
    /// Best-effort tokens in request-local live state.
    pub live_state_token_estimate: usize,
    /// Counts and sizes grouped by model-facing semantic category.
    pub semantic_aggregates: Vec<ContextSemanticAggregate>,
    /// First canonical block index outside the reusable prefix.
    pub first_volatile_block: Option<usize>,
    /// Number of blocks duplicating an earlier exact canonical digest.
    pub exact_duplicate_blocks: usize,
    /// Number of blocks duplicating normalized source/content with only
    /// whitespace or case differences.
    pub near_duplicate_blocks: usize,
    /// Canonical byte length of the immutable projection.
    pub stable_projection_bytes: usize,
    /// SHA-256 of the complete immutable projection.
    pub stable_projection_sha256: String,
    /// Hash of canonical blocks after provider-role projection metadata.
    pub provider_projection_sha256: String,
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
    /// Exceptional interaction mode that intentionally changes the stable
    /// request instruction profile, when applicable.
    pub expected_cache_break_reason: Option<String>,
}

/// Builds and compares provider-neutral continuity diagnostics.
pub fn context_continuity_diagnostics(
    context: &AgentContext,
    provider: &str,
    model: &str,
    turn_id: &str,
    previous: Option<&ContextContinuitySnapshot>,
) -> ContextContinuityDiagnostics {
    context_continuity_diagnostics_for_interaction(
        context,
        provider,
        model,
        turn_id,
        previous,
        ModelInteractionKind::CapabilityDecision,
    )
}

/// Builds continuity diagnostics and labels intentional exceptional-mode cache
/// breaks selected by controller-owned request state.
pub fn context_continuity_diagnostics_for_interaction(
    context: &AgentContext,
    provider: &str,
    model: &str,
    turn_id: &str,
    previous: Option<&ContextContinuitySnapshot>,
    interaction_kind: ModelInteractionKind,
) -> ContextContinuityDiagnostics {
    let snapshot = context_continuity_snapshot(context, provider, model, turn_id);
    let expected_cache_break_reason = interaction_kind
        .expected_cache_break_reason()
        .map(ToString::to_string);
    let Some(previous) = previous else {
        return ContextContinuityDiagnostics {
            snapshot,
            common_immutable_prefix_blocks: 0,
            common_immutable_prefix_tokens: 0,
            immutable_append_only: false,
            break_reason: ContextContinuityBreakReason::NewTurn,
            expected_cache_break_reason,
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
        expected_cache_break_reason,
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
    let mut block_diagnostics = Vec::new();
    let mut immutable_projection = Vec::new();
    let mut provider_projection = Vec::new();
    let mut immutable_token_estimate = 0usize;
    let mut volatile_token_estimate = 0usize;
    let mut stable_prefix_bytes = 0usize;
    let mut append_only_bytes = 0usize;
    let mut live_state_bytes = 0usize;
    let mut stable_prefix_token_estimate = 0usize;
    let mut append_only_token_estimate = 0usize;
    let mut live_state_token_estimate = 0usize;
    let mut semantic_totals = BTreeMap::<String, (ContextSemanticKind, usize, usize, usize)>::new();
    let mut first_volatile_block = None;
    let mut exact_digests = BTreeSet::new();
    let mut near_digests = BTreeSet::new();
    let mut exact_duplicate_blocks = 0usize;
    let mut near_duplicate_blocks = 0usize;
    let mut contains_compaction_epoch = false;
    for (index, block) in context.blocks.iter().enumerate() {
        let token_estimate = context_block_token_estimate(block);
        let canonical = canonical_context_block_bytes(block);
        let digest = sha256_hex(&canonical);
        let near_digest = normalized_context_block_sha256(block);
        if !exact_digests.insert(digest.clone()) {
            exact_duplicate_blocks = exact_duplicate_blocks.saturating_add(1);
        }
        if !near_digests.insert(near_digest) {
            near_duplicate_blocks = near_duplicate_blocks.saturating_add(1);
        }
        let semantic = block.semantic_kind();
        let semantic_key = format!("{semantic:?}");
        let entry = semantic_totals
            .entry(semantic_key)
            .or_insert((semantic, 0, 0, 0));
        entry.1 = entry.1.saturating_add(1);
        entry.2 = entry.2.saturating_add(canonical.len());
        entry.3 = entry.3.saturating_add(token_estimate);
        let canonical_role = role_for_context_block(block);
        let provider_role = projected_context_role(provider, canonical_role);
        provider_projection.extend_from_slice(&(canonical.len() as u64).to_be_bytes());
        provider_projection.extend_from_slice(&canonical);
        provider_projection.extend_from_slice(&(provider_role.len() as u64).to_be_bytes());
        provider_projection.extend_from_slice(provider_role.as_bytes());
        let request_local = block.placement == ContextPlacement::EphemeralTail;
        if request_local {
            first_volatile_block.get_or_insert(index);
            volatile_token_estimate = volatile_token_estimate.saturating_add(token_estimate);
            live_state_bytes = live_state_bytes.saturating_add(canonical.len());
            live_state_token_estimate = live_state_token_estimate.saturating_add(token_estimate);
        } else {
            immutable_projection.extend_from_slice(&(canonical.len() as u64).to_be_bytes());
            immutable_projection.extend_from_slice(&canonical);
            immutable_token_estimate = immutable_token_estimate.saturating_add(token_estimate);
            contains_compaction_epoch |= context_block_is_compaction_epoch(block);
            match block.placement {
                ContextPlacement::StablePrefix => {
                    stable_prefix_bytes = stable_prefix_bytes.saturating_add(canonical.len());
                    stable_prefix_token_estimate =
                        stable_prefix_token_estimate.saturating_add(token_estimate);
                }
                ContextPlacement::ConversationAppend => {
                    append_only_bytes = append_only_bytes.saturating_add(canonical.len());
                    append_only_token_estimate =
                        append_only_token_estimate.saturating_add(token_estimate);
                }
                ContextPlacement::EphemeralTail => unreachable!(),
            }
            immutable_blocks.push(ImmutableContextBlockDigest {
                placement: block.placement,
                source: block.source,
                semantic_kind: semantic,
                retention: block.retention(),
                bytes: canonical.len(),
                token_estimate,
                sha256: digest.clone(),
            });
        }
        block_diagnostics.push(ContextBlockDiagnostics {
            index,
            placement: block.placement,
            semantic_kind: semantic,
            retention: block.retention(),
            canonical_role: model_message_role_name(canonical_role).to_string(),
            provider_role: provider_role.to_string(),
            source: block.source,
            block_identity_sha256: context_block_identity_sha256(block),
            bytes: canonical.len(),
            token_estimate,
            reusable_prefix: block.stable_prefix_eligible(),
            request_local,
            sha256: digest,
        });
    }
    let semantic_aggregates = semantic_totals
        .into_values()
        .map(
            |(semantic_kind, blocks, bytes, token_estimate)| ContextSemanticAggregate {
                semantic_kind,
                blocks,
                bytes,
                token_estimate,
            },
        )
        .collect();
    ContextContinuitySnapshot {
        provider: provider.to_string(),
        model: model.to_string(),
        turn_id: turn_id.to_string(),
        immutable_blocks,
        blocks: block_diagnostics,
        immutable_token_estimate,
        volatile_token_estimate,
        stable_prefix_bytes,
        append_only_bytes,
        live_state_bytes,
        stable_prefix_token_estimate,
        append_only_token_estimate,
        live_state_token_estimate,
        semantic_aggregates,
        first_volatile_block,
        exact_duplicate_blocks,
        near_duplicate_blocks,
        stable_projection_bytes: immutable_projection.len(),
        stable_projection_sha256: sha256_hex(&immutable_projection),
        provider_projection_sha256: sha256_hex(&provider_projection),
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
    let semantic = format!("{:?}", block.semantic_kind());
    let retention = format!("{:?}", block.retention());
    let canonical_role = model_message_role_name(role_for_context_block(block));
    let mut encoded = Vec::new();
    for field in [
        placement.as_bytes(),
        source.as_bytes(),
        semantic.as_bytes(),
        retention.as_bytes(),
        canonical_role.as_bytes(),
        block.label.as_bytes(),
        block.content.as_bytes(),
    ] {
        encoded.extend_from_slice(&(field.len() as u64).to_be_bytes());
        encoded.extend_from_slice(field);
    }
    encoded
}

/// Hashes stable block identity without retaining label text.
fn context_block_identity_sha256(block: &ContextBlock) -> String {
    let mut identity = format!("{:?}", block.source).into_bytes();
    identity.extend_from_slice(&(block.label.len() as u64).to_be_bytes());
    identity.extend_from_slice(block.label.as_bytes());
    sha256_hex(&identity)
}

/// Hashes normalized context to detect case/whitespace-only duplication.
fn normalized_context_block_sha256(block: &ContextBlock) -> String {
    let normalized = block
        .content
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    let material = format!("{:?}\n{normalized}", block.source);
    sha256_hex(material.as_bytes())
}

/// Returns the stable spelling of one canonical provider-neutral role.
fn model_message_role_name(role: ModelMessageRole) -> &'static str {
    match role {
        ModelMessageRole::System => "system",
        ModelMessageRole::Developer => "developer",
        ModelMessageRole::User => "user",
        ModelMessageRole::Assistant => "assistant",
        ModelMessageRole::Tool => "tool",
        ModelMessageRole::Context => "context",
    }
}

/// Returns the provider transport role or channel selected for one canonical
/// role without retaining message text.
fn projected_context_role(provider: &str, role: ModelMessageRole) -> &'static str {
    match provider {
        "openai" => match role {
            ModelMessageRole::System | ModelMessageRole::Developer => "instructions",
            ModelMessageRole::User => "user",
            ModelMessageRole::Assistant => "assistant",
            ModelMessageRole::Tool => "user_evidence_wrapper",
            ModelMessageRole::Context => "developer_neutral_wrapper",
        },
        "anthropic" => match role {
            ModelMessageRole::System | ModelMessageRole::Developer => "system",
            ModelMessageRole::Assistant => "assistant",
            ModelMessageRole::User | ModelMessageRole::Tool => "user",
            ModelMessageRole::Context => "user_neutral_wrapper",
        },
        "deepseek" => match role {
            ModelMessageRole::System => "system",
            ModelMessageRole::User => "user",
            ModelMessageRole::Assistant => "assistant",
            ModelMessageRole::Developer | ModelMessageRole::Tool | ModelMessageRole::Context => {
                "user_neutral_wrapper"
            }
        },
        "claude-code" => match role {
            ModelMessageRole::System | ModelMessageRole::Developer => "system_prompt",
            ModelMessageRole::User => "stdin_user_section",
            ModelMessageRole::Assistant => "stdin_assistant_section",
            ModelMessageRole::Tool => "stdin_tool_section",
            ModelMessageRole::Context => "stdin_neutral_context_section",
        },
        _ => match role {
            ModelMessageRole::System => "system",
            ModelMessageRole::Developer => "developer_or_system",
            ModelMessageRole::User => "user",
            ModelMessageRole::Assistant => "assistant",
            ModelMessageRole::Tool => "tool",
            ModelMessageRole::Context => "developer_or_system_neutral_wrapper",
        },
    }
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
        assert_eq!(diagnostics.snapshot.first_volatile_block, Some(2));
        assert!(diagnostics.snapshot.stable_prefix_bytes > 0);
        assert!(diagnostics.snapshot.append_only_bytes > 0);
        assert!(diagnostics.snapshot.live_state_bytes > 0);
        assert_eq!(diagnostics.snapshot.blocks[1].canonical_role, "user");
        assert_eq!(diagnostics.snapshot.blocks[2].canonical_role, "context");
        assert_eq!(
            diagnostics.snapshot.blocks[2].provider_role,
            "developer_neutral_wrapper"
        );
        assert_eq!(diagnostics.snapshot.stable_projection_sha256.len(), 64);
        assert_eq!(diagnostics.snapshot.provider_projection_sha256.len(), 64);
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

    /// Verifies semantic and retention changes participate in continuity
    /// identity and exceptional request profiles explain intentional breaks.
    #[test]
    fn context_continuity_fingerprints_semantics_and_labels_exceptional_modes() {
        let context = AgentContext::new(vec![
            ContextBlock::user_event("user prompt", "run tests"),
            ContextBlock::live_state(ContextSourceKind::RuntimeHint, "runtime", "cwd=/repo"),
            ContextBlock::live_state(ContextSourceKind::RuntimeHint, "runtime copy", "CWD=/repo"),
        ])
        .unwrap();

        let diagnostics = context_continuity_diagnostics_for_interaction(
            &context,
            "anthropic",
            "claude",
            "turn-1",
            None,
            ModelInteractionKind::OutputLimitRetry,
        );

        assert_eq!(
            diagnostics.expected_cache_break_reason.as_deref(),
            Some("output_limit_retry")
        );
        assert_eq!(diagnostics.snapshot.near_duplicate_blocks, 1);
        assert_eq!(
            diagnostics.snapshot.blocks[0].retention,
            ContextRetention::Exact
        );
        assert_eq!(
            diagnostics.snapshot.blocks[1].retention,
            ContextRetention::RequestLocal
        );
        assert_eq!(
            diagnostics.snapshot.blocks[1].provider_role,
            "user_neutral_wrapper"
        );
        assert!(
            diagnostics.snapshot.immutable_blocks[0]
                .sha256
                .contains(|character: char| character.is_ascii_hexdigit())
        );
    }
}
