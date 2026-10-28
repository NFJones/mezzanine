//! Compacted fork sources carry immutable authority, rows and logical cutoffs.
//!
//! Actual `/resume` establishes parent counters. Worker-source checks never
//! substitute model block counts or invented raw rows for archival evidence.
//! Child publication is a separate owner and is not qualified by these tests.

use super::*;
use crate::storage::transcript::{AgentCompactionEpoch, AgentCompactionRange};

use super::fork_fixtures::{CONVERSATION, resumed_source, row};

/// A nonempty summary remains source work when no raw tail survives. A parent
/// with a retained tail must carry the same complete prefix authority too.
#[test]
fn runtime_fork_source_preserves_resumed_prefix_with_and_without_raw_tail() {
    for prefix in [1, 5] {
        let (service, store, _) = resumed_source(5, prefix);
        let epoch = store.compaction_epoch(CONVERSATION).unwrap();
        let work = service
            .prepare_subagent_fork_read_work("agent-%1")
            .unwrap()
            .expect("nonempty prefix requires source work even with zero raw rows");
        let snapshot = work.execute().unwrap();
        assert_eq!(snapshot.source_high_water, 5);
        assert_eq!(snapshot.compaction_epoch, epoch);
        assert_eq!(snapshot.entries.len() as u64, 5 - prefix);
    }
}

/// A worker freezes the existing prefix and only ranges wholly before its
/// captured logical cutoff. Later raw rows and their summaries cannot enter
/// that source, and a subsequent epoch edit cannot mutate the owned snapshot.
#[test]
fn runtime_fork_source_freezes_selective_authority_at_captured_cutoff() {
    let (service, store, _) = resumed_source(3, 1);
    let work = service
        .prepare_subagent_fork_read_work("agent-%1")
        .unwrap()
        .unwrap();
    for sequence in 4..=5 {
        store.append(&row(sequence)).unwrap();
    }
    let ranges = vec![
        AgentCompactionRange {
            first_sequence: 2,
            through_sequence: 3,
            summary: "FIRST_RANGE".into(),
        },
        AgentCompactionRange {
            first_sequence: 4,
            through_sequence: 5,
            summary: "LATER_RANGE".into(),
        },
    ];
    store
        .save_compaction_ranges(CONVERSATION, 1, "FROZEN_PREFIX", ranges.clone())
        .unwrap();
    let snapshot = work.execute().unwrap();
    let expected = AgentCompactionEpoch {
        version: 2,
        conversation_id: CONVERSATION.into(),
        through_sequence: 1,
        summary: "FROZEN_PREFIX".into(),
        ranges: vec![ranges[0].clone()],
    };
    assert_eq!(snapshot.compaction_epoch, Some(expected.clone()));
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|r| r.sequence)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    store
        .save_compaction_ranges(CONVERSATION, 1, "REPLACEMENT_PREFIX", ranges)
        .unwrap();
    assert_eq!(snapshot.compaction_epoch, Some(expected));
    work.check_owner(&service).unwrap();
}

/// A summary spanning across a captured source cutoff cannot be truncated into
/// a smaller authority claim. Reject before any child or provider work exists.
#[test]
fn runtime_fork_source_rejects_selective_range_bisecting_captured_cutoff() {
    let (service, store, _) = resumed_source(2, 1);
    let work = service
        .prepare_subagent_fork_read_work("agent-%1")
        .unwrap()
        .unwrap();
    store.append(&row(3)).unwrap();
    store
        .save_compaction_ranges(
            CONVERSATION,
            1,
            "FROZEN_PREFIX",
            vec![AgentCompactionRange {
                first_sequence: 2,
                through_sequence: 3,
                summary: "CROSSES_CUTOFF".into(),
            }],
        )
        .unwrap();
    assert!(
        work.execute().is_err(),
        "a complete summary cannot stand for a partial source"
    );
}

/// An epoch published beyond the admitted frontier cannot replace missing
/// frozen history, even when the later archive itself is valid and contiguous.
#[test]
fn runtime_fork_source_rejects_prefix_ahead_of_captured_cutoff() {
    let (service, store, _) = resumed_source(2, 1);
    let work = service
        .prepare_subagent_fork_read_work("agent-%1")
        .unwrap()
        .unwrap();
    store.append(&row(3)).unwrap();
    store
        .save_compaction_epoch(CONVERSATION, 3, "TOO_NEW_PREFIX")
        .unwrap();
    assert!(work.execute().is_err());
}

/// A zero raw tail is not permission to treat a missing required archive as an
/// empty conversation. The retained summary remains tied to checked history.
#[test]
fn runtime_fork_source_zero_raw_tail_requires_its_archive() {
    let (service, store, _) = resumed_source(5, 5);
    std::fs::remove_file(store.transcript_path(CONVERSATION).unwrap()).unwrap();
    let work = service
        .prepare_subagent_fork_read_work("agent-%1")
        .unwrap()
        .expect("zero raw tail must still qualify its durable prefix");
    let error = work.execute().unwrap_err();
    assert!(
        error
            .message()
            .contains("required transcript archive is missing"),
        "{error}"
    );
}

/// A legitimate zero-boundary summary has an empty checked archive, not an
/// invented raw row. It still needs a source worker and carries full authority.
#[test]
fn runtime_fork_source_preserves_zero_boundary_summary_without_inventing_rows() {
    let store = AgentTranscriptStore::new(temp_root("fork-source-empty-inherited"));
    store
        .save_compaction_epoch(CONVERSATION, 0, "INHERITED_ONLY")
        .unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", CONVERSATION, 0)
        .unwrap();
    let work = service
        .prepare_subagent_fork_read_work("agent-%1")
        .unwrap()
        .unwrap();
    let snapshot = work.execute().unwrap();
    assert_eq!(snapshot.source_high_water, 0);
    assert!(snapshot.entries.is_empty());
    assert_eq!(
        snapshot.compaction_epoch,
        store.compaction_epoch(CONVERSATION).unwrap()
    );
}

/// Empty parents remain valid with or without storage. Only a configured store
/// needs qualification for a possible inherited zero-boundary summary; an
/// absent store cannot justify nonempty logical history.
#[test]
fn runtime_fork_source_empty_controls_do_not_require_nonexistent_history() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    assert!(
        service
            .prepare_subagent_fork_read_work("agent-%1")
            .unwrap()
            .is_none()
    );
    let store = AgentTranscriptStore::new(temp_root("fork-source-empty-control"));
    service.set_agent_transcript_store(store);
    let snapshot = service
        .prepare_subagent_fork_read_work("agent-%1")
        .unwrap()
        .unwrap()
        .execute()
        .unwrap();
    assert!(snapshot.entries.is_empty());
    assert!(snapshot.compaction_epoch.is_none());
    assert_eq!(snapshot.source_high_water, 0);

    let mut without_store = test_runtime_service();
    without_store
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    without_store
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();
    assert!(
        without_store
            .prepare_subagent_fork_read_work("agent-%1")
            .is_err()
    );
}

/// Captured data from an old store cannot be accepted after the actor switches
/// transcript roots, even if conversation, counters and model lineage match.
#[test]
fn runtime_fork_source_rejects_replaced_store_at_adoption() {
    let (mut service, _, _) = resumed_source(3, 1);
    let work = service
        .prepare_subagent_fork_read_work("agent-%1")
        .unwrap()
        .unwrap();
    let snapshot = work.execute().unwrap();
    assert!(snapshot.compaction_epoch.is_some());
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
        "fork-source-replacement",
    )));
    assert!(work.check_owner(&service).is_err());
}
