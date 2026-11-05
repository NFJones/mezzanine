//! Stable snapshot adoption must precede allocation and preserve live authority.
//!
//! These tests use the same indexed-history plus named-slot input split as
//! product replay. The counting witness detects estimates that are constructed
//! and immediately discarded even when the resulting records look correct.

use super::*;
use crate::ContextPlacement;

/// Actual producer keys survive reorder/removal and preserve original costs
/// without estimates; legacy positional snapshots migrate only at their exact
/// source position, and ambiguous explicit associations fail before allocation.
#[test]
fn producer_keyed_stable_import_preserves_reordered_slots_without_recount() {
    let sources = vec![
        (0, StableContextSlotId::new("producer/first").unwrap()),
        (1, StableContextSlotId::new("producer/second").unwrap()),
    ];
    let blocks = vec![
        guidance("first", "equal 雪"),
        guidance("second", "equal 雪"),
    ];
    let original =
        AgentContext::import_durable_blocks_with_stable_sources(blocks.clone(), &[], &[], &sources)
            .unwrap();
    let saved = original.stable_block_records();
    let before = ESTIMATE_COMPUTATIONS.with(|count| count.get());
    let reversed = AgentContext::import_durable_blocks_with_stable_sources(
        vec![blocks[1].clone(), blocks[0].clone()],
        &[],
        &saved,
        &[(0, sources[1].1.clone()), (1, sources[0].1.clone())],
    )
    .unwrap();
    assert_eq!(reversed.stable_slots()[0], original.stable_slots()[1]);
    assert_eq!(reversed.stable_slots()[1], original.stable_slots()[0]);
    let survivor = AgentContext::import_durable_blocks_with_stable_sources(
        vec![blocks[1].clone()],
        &[],
        &saved,
        &[(0, sources[1].1.clone())],
    )
    .unwrap();
    assert_eq!(survivor.stable_slots()[0], original.stable_slots()[1]);
    assert_eq!(ESTIMATE_COMPUTATIONS.with(|count| count.get()), before);
    for invalid in [
        vec![(2, sources[0].1.clone())],
        vec![(0, sources[0].1.clone()), (0, sources[1].1.clone())],
        vec![(0, sources[0].1.clone()), (1, sources[0].1.clone())],
    ] {
        assert!(
            AgentContext::import_durable_blocks_with_stable_sources(
                blocks.clone(),
                &[],
                &saved,
                &invalid
            )
            .is_err()
        );
    }
    assert_eq!(ESTIMATE_COMPUTATIONS.with(|count| count.get()), before);
    let legacy = AgentContext::import_durable_blocks(blocks.clone()).unwrap();
    let before = ESTIMATE_COMPUTATIONS.with(|count| count.get());
    let migrated = AgentContext::import_durable_blocks_with_stable_sources(
        blocks,
        &[],
        &legacy.stable_block_records(),
        &sources,
    )
    .unwrap();
    assert_eq!(
        migrated.stable_slots()[0].record(),
        legacy.stable_slots()[0].record()
    );
    assert_eq!(
        migrated.stable_slots()[1].record(),
        legacy.stable_slots()[1].record()
    );
    assert_eq!(ESTIMATE_COMPUTATIONS.with(|count| count.get()), before);
    assert_eq!(migrated.stable_slots()[0].slot_id(), &sources[0].1);
}

/// Builds current trusted guidance with a stable structural slot identity.
fn guidance(label: &str, content: &str) -> ContextBlock {
    ContextBlock {
        source: ContextSourceKind::ProjectGuidance,
        placement: ContextPlacement::StablePrefix,
        label: label.into(),
        content: content.into(),
    }
}

/// The original product import order created fresh stable estimates and then
/// replaced them from saved snapshots. Valid unchanged stable and chronological
/// inputs must instead reach their constructors with original records already
/// selected, with no estimation calls and no change to the trusted source.
#[test]
fn stable_snapshots_are_restored_before_any_estimate_is_constructed() {
    let mut original = AgentContext::import_durable_blocks(vec![
        guidance("repository", "equal guidance 雪 {}"),
        guidance("directory", "equal guidance 雪 {}"),
    ])
    .unwrap();
    original
        .append_peer_message_event("peer", "untrusted source")
        .unwrap();
    let stable = original.stable_block_records();
    let records = original
        .block_records()
        .into_iter()
        .skip(2)
        .collect::<Vec<_>>();
    let before = ESTIMATE_COMPUTATIONS.with(|count| count.get());
    let restored = AgentContext::import_durable_blocks_with_records(
        original.blocks().to_vec(),
        &records,
        &stable,
    )
    .unwrap();
    assert_eq!(restored, original);
    assert_eq!(
        ESTIMATE_COMPUTATIONS.with(|count| count.get()),
        before,
        "valid stable snapshots must not allocate estimates before adoption"
    );
}

/// Changed trusted guidance repairs only its own slot; a subsequent import of
/// its repaired snapshot is free of estimation. Saved fingerprints cannot
/// replace the current source. Stale counts and methods likewise repair once.
#[test]
fn stable_snapshot_revisions_recompute_only_the_changed_slot() {
    let original = AgentContext::import_durable_blocks(vec![
        guidance("repository", "original instructions"),
        guidance("directory", "unchanged instructions"),
    ])
    .unwrap();
    let stable = original.stable_block_records();
    let mut blocks = original.blocks().to_vec();
    blocks[0].content = "current trusted instructions 雪".repeat(80);
    let expected = StableContextBlock::from_compatibility_block(blocks[0].clone(), 0).unwrap();
    let before = ESTIMATE_COMPUTATIONS.with(|count| count.get());
    let changed =
        AgentContext::import_durable_blocks_with_records(blocks.clone(), &[], &stable).unwrap();
    assert_eq!(ESTIMATE_COMPUTATIONS.with(|count| count.get()), before + 1);
    assert_eq!(
        changed.stable_slots()[0].record().occurrence_id(),
        stable[0].record.occurrence_id()
    );
    assert_eq!(changed.stable_slots()[0].block(), &blocks[0]);
    assert_eq!(
        changed.stable_slots()[0].source_fingerprint(),
        expected.source_fingerprint()
    );
    assert_ne!(
        changed.stable_block_records()[0].source_fingerprint,
        stable[0].source_fingerprint
    );
    assert_eq!(changed.stable_slots()[1], original.stable_slots()[1]);
    let current = changed.stable_block_records();
    let before = ESTIMATE_COMPUTATIONS.with(|count| count.get());
    let repeated = AgentContext::import_durable_blocks_with_records(blocks, &[], &current).unwrap();
    assert_eq!(changed, repeated);
    assert_eq!(ESTIMATE_COMPUTATIONS.with(|count| count.get()), before);
    for field in [
        "counting_method",
        "framed_input_tokens",
        "native_input_tokens",
    ] {
        let mut encoded = serde_json::to_value(&stable).unwrap();
        encoded[0]["record"][field] = 999_999.into();
        let stale: Vec<StableContextBlockRecord> = serde_json::from_value(encoded).unwrap();
        let before = ESTIMATE_COMPUTATIONS.with(|count| count.get());
        let repaired = AgentContext::import_durable_blocks_with_records(
            original.blocks().to_vec(),
            &[],
            &stale,
        )
        .unwrap();
        assert_eq!(repaired, original, "{field}");
        assert_eq!(
            ESTIMATE_COMPUTATIONS.with(|count| count.get()),
            before + 1,
            "{field}"
        );
    }
}

/// Snapshots cannot resurrect removed instructions or identify a new slot by
/// equal bytes. A surviving slot reuses its record, while a new slot receives
/// exactly one new occurrence and estimate.
#[test]
fn stable_snapshot_import_keeps_removed_slots_absent_and_new_slots_distinct() {
    let original =
        AgentContext::import_durable_blocks(vec![guidance("repository", "equal instructions")])
            .unwrap();
    let stable = original.stable_block_records();
    let before = ESTIMATE_COMPUTATIONS.with(|count| count.get());
    let removed =
        AgentContext::import_durable_blocks_with_records(Vec::new(), &[], &stable).unwrap();
    assert!(removed.blocks().is_empty());
    assert_eq!(ESTIMATE_COMPUTATIONS.with(|count| count.get()), before);
    let added = AgentContext::import_durable_blocks_with_records(
        vec![
            guidance("repository", "equal instructions"),
            guidance("new directory", "equal instructions"),
        ],
        &[],
        &stable,
    )
    .unwrap();
    assert_eq!(ESTIMATE_COMPUTATIONS.with(|count| count.get()), before + 1);
    assert_eq!(added.stable_slots()[0], original.stable_slots()[0]);
    assert_ne!(
        added.stable_slots()[0].record().occurrence_id(),
        added.stable_slots()[1].record().occurrence_id()
    );
}

/// All named formats validate before any estimates, even for removed slots.
/// Duplicate slot names and aliased/contradictory ownership cannot be adopted;
/// indexed and named records may overlap only when their claims agree.
#[test]
fn stable_snapshot_import_rejects_malformed_duplicate_and_conflicting_owners() {
    let mut original = AgentContext::import_durable_blocks(vec![
        guidance("repository", "source"),
        guidance("directory", "source"),
    ])
    .unwrap();
    original
        .append_peer_message_event("peer", "source")
        .unwrap();
    let stable = original.stable_block_records();
    let records = original
        .block_records()
        .into_iter()
        .skip(2)
        .collect::<Vec<_>>();
    for field in ["slot_id", "source_fingerprint", "format_version"] {
        let mut encoded = serde_json::to_value(&stable).unwrap();
        match field {
            "slot_id" => encoded[0][field] = "".into(),
            "source_fingerprint" => encoded[0][field] = "not-a-fingerprint".into(),
            _ => encoded[0]["record"][field] = 99.into(),
        }
        let malformed: Vec<StableContextBlockRecord> = serde_json::from_value(encoded).unwrap();
        let before = ESTIMATE_COMPUTATIONS.with(|count| count.get());
        assert!(
            AgentContext::import_durable_blocks_with_records(
                original.blocks().to_vec(),
                &records,
                &malformed
            )
            .is_err(),
            "{field}"
        );
        assert!(
            AgentContext::import_durable_blocks_with_records(Vec::new(), &[], &malformed).is_err(),
            "removed {field}"
        );
        assert_eq!(ESTIMATE_COMPUTATIONS.with(|count| count.get()), before);
    }
    let duplicated = vec![stable[0].clone(), stable[0].clone()];
    assert!(
        AgentContext::import_durable_blocks_with_records(Vec::new(), &[], &duplicated).is_err()
    );
    for foreign in [
        stable[1].record.occurrence_id(),
        records[0].1.occurrence_id(),
    ] {
        let mut aliased = stable.clone();
        aliased[0].record.occurrence_id = foreign;
        assert!(
            AgentContext::import_durable_blocks_with_records(
                original.blocks().to_vec(),
                &records,
                &aliased
            )
            .is_err()
        );
    }
    let all_records = original.block_records();
    let before = ESTIMATE_COMPUTATIONS.with(|count| count.get());
    let restored = AgentContext::import_durable_blocks_with_records(
        original.blocks().to_vec(),
        &all_records,
        &stable,
    )
    .unwrap();
    assert_eq!(restored, original);
    assert_eq!(ESTIMATE_COMPUTATIONS.with(|count| count.get()), before);
    let mut conflict = all_records;
    conflict[0].1.occurrence_id = ContextBlockOccurrenceId::allocate();
    assert!(
        AgentContext::import_durable_blocks_with_records(
            original.blocks().to_vec(),
            &conflict,
            &stable
        )
        .is_err()
    );
    assert_eq!(restored, original);
}
