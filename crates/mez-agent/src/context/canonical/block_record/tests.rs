//! Occurrence/revision and reusable-estimate controls on the actual canonical
//! owner. Transcript persistence and provider serialization have separate owners.

use super::*;
use crate::{ContextSourceKind, provider_text_input_token_estimate};

/// Equal content is two occurrences. A content revision preserves the original
/// identity and refreshes estimates; valid serialized caches restore without a
/// counting call, while modified costs or methods recompute from real source.
#[test]
fn occurrence_identity_and_checked_cost_cache_have_independent_lifetimes() {
    let block = ContextBlock::reference_event(ContextSourceKind::PeerMessage, "peer", "é😀{}\n");
    let first = ContextBlockRecord::allocate(&block).unwrap();
    let second = ContextBlockRecord::allocate(&block).unwrap();
    assert_ne!(first.occurrence_id(), second.occurrence_id());
    assert_eq!(
        first.estimated_input_tokens(),
        provider_text_input_token_estimate(&format!(
            "{}{}",
            model_context_block_header(&block),
            block.content
        ))
    );
    assert_eq!(
        first.estimated_native_input_tokens(),
        provider_text_input_token_estimate(&block.content)
    );
    let snapshot = serde_json::to_value(first).unwrap();
    let before = ESTIMATE_COMPUTATIONS.with(|count| count.get());
    let restored: ContextBlockRecord = serde_json::from_value(snapshot.clone()).unwrap();
    assert_eq!(restored.restored(&block).unwrap(), first);
    assert_eq!(ESTIMATE_COMPUTATIONS.with(|count| count.get()), before);
    for field in [
        "framed_input_tokens",
        "native_input_tokens",
        "counting_method",
    ] {
        let mut stale = snapshot.clone();
        stale[field] = 999_999.into();
        let stale: ContextBlockRecord = serde_json::from_value(stale).unwrap();
        assert_eq!(stale.restored(&block).unwrap(), first);
    }
    let changed = ContextBlock::reference_event(ContextSourceKind::PeerMessage, "peer", "new text");
    let revised = first.revised(&changed).unwrap();
    assert_eq!(first.occurrence_id(), revised.occurrence_id());
    assert_ne!(first, revised);
    let mut future = snapshot;
    future["format_version"] = 2.into();
    let future: ContextBlockRecord = serde_json::from_value(future).unwrap();
    assert!(future.restored(&block).is_err());
}

/// Projection rebuilding, validation and copied-owner edits reuse existing
/// records. Importing valid structural snapshots also avoids a discard/recount
/// pass; duplicate origins or ambiguous indices cannot partially mutate source.
#[test]
fn unchanged_canonical_projections_and_record_import_do_not_recount_source() {
    let mut context = AgentContext::empty();
    for _ in 0..4 {
        context
            .append_peer_message_event("same label", "same text")
            .unwrap();
    }
    let records = context.block_records();
    let before = ESTIMATE_COMPUTATIONS.with(|count| count.get());
    for _ in 0..8 {
        context.retain_blocks(|_| true).unwrap();
        context.validate_durable().unwrap();
    }
    let restored =
        AgentContext::import_durable_blocks_with_records(context.blocks().to_vec(), &records)
            .unwrap();
    assert_eq!(restored.block_records(), records);
    assert_eq!(ESTIMATE_COMPUTATIONS.with(|count| count.get()), before);
    let original = context.clone();
    assert!(
        context
            .restore_block_records(&[(0, records[0].1), (1, records[0].1)])
            .is_err()
    );
    assert_eq!(context, original);
    assert!(
        context
            .restore_block_records(&[(1, records[1].1), (0, records[0].1)])
            .is_err()
    );
    assert_eq!(context, original);
}

/// Local import order can change independently of immutable origin. Compaction
/// creates a new summary even at a reused sequence anchor; the retained raw tail
/// and later arrivals keep their exact original occurrence and cached costs.
#[test]
fn imported_tail_origins_survive_local_resequencing_and_summary_replacement() {
    let mut history = AgentContext::empty();
    for _ in 0..4 {
        history
            .append_peer_message_event("peer", "equal source")
            .unwrap();
    }
    let mut context = AgentContext::empty();
    context
        .append_peer_message_event("earlier", "different prefix")
        .unwrap();
    context.append_imported_history(&history).unwrap();
    assert_eq!(
        context.chronology()[1].record(),
        history.chronology()[0].record()
    );
    assert_ne!(
        context.chronology()[1].sequence(),
        history.chronology()[0].sequence()
    );
    let old = context.chronology()[0].record().occurrence_id();
    let anchor = context.chronology()[0].sequence();
    let mut heads = std::collections::BTreeSet::new();
    for round in 0..3 {
        let tail = context.chronology().last().unwrap().record();
        let count = context.chronology().len();
        context
            .compact_execution_ranges_into_summary(
                std::iter::once(0..count - 1).collect(),
                ContextBlock::reference_event(
                    ContextSourceKind::CompactionPrefix,
                    "summary",
                    format!("epoch {round}"),
                ),
            )
            .unwrap();
        assert_eq!(context.chronology()[0].sequence(), anchor);
        let head = context.chronology()[0].record().occurrence_id();
        assert_ne!(head, old);
        assert!(heads.insert(head));
        assert_eq!(context.chronology().last().unwrap().record(), tail);
        if round < 2 {
            context
                .append_peer_message_event("arrival", "new source")
                .unwrap();
            assert_ne!(
                context
                    .chronology()
                    .last()
                    .unwrap()
                    .record()
                    .occurrence_id(),
                tail.occurrence_id()
            );
        }
    }
}

/// Legacy migration uses actual archived position plus structural projection,
/// not bytes, pane labels or new context order. Persistent IDs have one strict
/// spelling; nil/alternate spellings cannot create aliases in membership maps.
#[test]
fn legacy_archive_identity_and_persistent_spelling_are_explicit() {
    let first = ContextBlockOccurrenceId::for_legacy_archive_row("conversation", 7, 0).unwrap();
    assert_eq!(
        first,
        ContextBlockOccurrenceId::for_legacy_archive_row("conversation", 7, 0).unwrap()
    );
    assert_ne!(
        first,
        ContextBlockOccurrenceId::for_legacy_archive_row("conversation", 8, 0).unwrap()
    );
    assert_ne!(
        first,
        ContextBlockOccurrenceId::for_legacy_archive_row("conversation", 7, 1).unwrap()
    );
    assert!(ContextBlockOccurrenceId::for_legacy_archive_row("conversation", 0, 0).is_err());
    let encoded: String = first.into();
    assert_eq!(
        ContextBlockOccurrenceId::try_from(encoded.clone()).unwrap(),
        first
    );
    assert!(ContextBlockOccurrenceId::try_from(encoded.to_uppercase()).is_err());
    assert!(
        ContextBlockOccurrenceId::try_from("00000000-0000-0000-0000-000000000000".to_owned())
            .is_err()
    );
}
