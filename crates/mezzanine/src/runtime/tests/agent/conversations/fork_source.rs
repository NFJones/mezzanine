//! Compacted fork sources carry immutable authority, rows and logical cutoffs.
//!
//! Actual `/resume` establishes parent counters. Worker-source checks never
//! substitute model block counts or invented raw rows for archival evidence.
//! Child publication is a separate owner and is not qualified by these tests.

use super::*;
use crate::storage::transcript::{AgentCompactionEpoch, AgentCompactionRange};

const CONVERSATION: &str = "018f6b3a-1b2c-7000-9000-cafebabe0011";

/// Builds independently resumable rows, with complete typed groups at 2..3 and
/// 4..5 so tests can publish valid selective ranges after source admission.
fn row(sequence: u64) -> TranscriptEntry {
    let covered = (2..=5).contains(&sequence);
    let content = if covered {
        let (group, ordinal) = if sequence <= 3 {
            ("fork-source-first", sequence - 1)
        } else {
            ("fork-source-later", sequence - 3)
        };
        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::TranscriptAssistant,
            "assistant",
            format!("raw source {sequence}"),
            mez_agent::ContextExecutionGroupId::new(group).unwrap(),
            ordinal,
            None,
        )
        .unwrap()
        .to_transcript_content()
    } else {
        format!("raw source {sequence}")
    };
    TranscriptEntry {
        conversation_id: CONVERSATION.into(),
        sequence,
        created_at_unix_seconds: sequence,
        role: if covered {
            TranscriptRole::System
        } else {
            TranscriptRole::Assistant
        },
        turn_id: if covered {
            format!("group-{}", (sequence - 2) / 2)
        } else {
            format!("turn-{sequence}")
        },
        agent_id: "agent-%9".into(),
        pane_id: "%9".into(),
        content,
    }
}

/// Runs the actual deferred resume command against an existing prefix. Returns
/// the store for controlled later cooperating appends and epoch publications.
fn resumed_source(rows: u64, prefix: u64) -> (RuntimeSessionService, AgentTranscriptStore) {
    let store = AgentTranscriptStore::new(temp_root("fork-source-authority"));
    for sequence in 1..=rows {
        store.append(&row(sequence)).unwrap();
    }
    store
        .save_compaction_epoch(CONVERSATION, prefix, "FROZEN_PREFIX")
        .unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, &format!("/resume {CONVERSATION}"))
        .unwrap();
    let result = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(result.contains("resumed=true"), "{result}");
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.transcript_high_water, rows);
    assert_eq!(session.transcript_entries, rows - prefix);
    (service, store)
}

/// A nonempty summary remains source work when no raw tail survives. A parent
/// with a retained tail must carry the same complete prefix authority too.
#[test]
fn runtime_fork_source_preserves_resumed_prefix_with_and_without_raw_tail() {
    for prefix in [1, 5] {
        let (service, store) = resumed_source(5, prefix);
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
    let (service, store) = resumed_source(3, 1);
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
    let (service, store) = resumed_source(2, 1);
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
    let (service, store) = resumed_source(2, 1);
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
    let (service, store) = resumed_source(5, 5);
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
    let (mut service, _) = resumed_source(3, 1);
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
