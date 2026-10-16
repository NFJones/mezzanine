//! Fork publication preserves exact active history without changing its parent.
//!
//! Synthetic archives cover selective anchors, prefix boundaries, crash-safe
//! epoch publication, and fail-closed source validation. No actions are replayed.

use super::*;
use crate::storage::transcript::{AgentCompactionEpoch, AgentCompactionRange};

/// The existing active-delete recovery intent hides a provisional fork from
/// exact lookup, catalog rebuild and model replay even after a crash. Restart
/// cleans only that provisional target and leaves parent bytes untouched.
#[test]
fn fork_epoch_pending_publication_is_hidden_and_recovered() {
    use crate::storage::transcript::ConversationTranscriptRead;
    let root = temp_root("fork-pending-publication");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("source", 1, TranscriptRole::User))
        .unwrap();
    store
        .save_compaction_epoch("source", 1, "source summary")
        .unwrap();
    let before = source_bytes(&root);
    store.begin_fork_publication("target").unwrap();
    store
        .append(&entry("target", 1, TranscriptRole::User))
        .unwrap();
    assert!(store.saved_session("target").unwrap().is_none());
    assert!(
        store
            .list()
            .unwrap()
            .iter()
            .all(|summary| summary.conversation_id != "target")
    );
    assert!(
        store
            .conversation_transcript_view("target", ConversationTranscriptRead::All, true, &[])
            .is_err()
    );
    assert!(
        store
            .publish_terminal_compaction_handoffs("target")
            .is_err()
    );
    assert!(
        store
            .catalog_migration_candidates()
            .unwrap()
            .iter()
            .all(|candidate| candidate.summary.conversation_id != "target")
    );
    // Rebuilding enumerates retained files before the existing recovery owner
    // compensates pending target payloads; neither step may expose the fork.
    store.rebuild_catalog(100).unwrap();
    assert!(store.saved_session("target").unwrap().is_none());
    let reopened = AgentTranscriptStore::new(root.clone());
    reopened.recover_archive_transactions().unwrap();
    assert!(!root.join("target").exists());
    assert_eq!(source_bytes(&root), before);
    fs::remove_dir_all(root).unwrap();
}

/// Failed provisional-journal publication and its blocked compensation must
/// report both failures. A file at the recovery-directory name produces a
/// portable filesystem error without privilege changes or injected authority.
#[test]
fn fork_epoch_cleanup_failure_retains_original_and_secondary_diagnostics() {
    let root = temp_root("fork-secondary-cleanup");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("source", 1, TranscriptRole::User))
        .unwrap();
    store
        .save_compaction_epoch("source", 1, "source summary")
        .unwrap();
    let before = source_bytes(&root);
    fs::write(root.join(".archive-recovery"), "blocked journal directory").unwrap();
    let error = store.fork("source", "target", 99).unwrap_err();
    assert!(
        error.to_string().contains("conversation fork failed"),
        "{error}"
    );
    assert!(
        error.to_string().contains("target cleanup failed"),
        "{error}"
    );
    assert_eq!(source_bytes(&root), before);
    fs::remove_dir_all(root).unwrap();
}

/// Cooperating appends and compaction publications race repeated forks. Each
/// fork must contain a contiguous source prefix with an applicable captured
/// epoch; source work after capture cannot leak into an already-created target.
/// Two creators racing the same target must produce one winner without cleanup
/// deleting that winner. A barrier starts real workers, without sleeps or CAS
/// claims against unsupported arbitrary external writers.
#[test]
fn fork_epoch_concurrent_capture_and_exclusive_target_creation() {
    let root = temp_root("fork-concurrent-epoch");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("source", 1, TranscriptRole::User))
        .unwrap();
    store
        .save_compaction_epoch("source", 1, "summary-1")
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let writer = {
        let store = store.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            for sequence in 2..=16 {
                store
                    .append(&entry("source", sequence, TranscriptRole::User))
                    .unwrap();
                store
                    .save_compaction_epoch("source", sequence, &format!("summary-{sequence}"))
                    .unwrap();
            }
        })
    };
    barrier.wait();
    for index in 0..8 {
        let target = format!("target-{index}");
        store.fork("source", &target, 99).unwrap();
        let rows = store.inspect(&target).unwrap();
        let epoch = store.compaction_epoch(&target).unwrap().unwrap();
        assert!(epoch.through_sequence <= rows.len() as u64);
        assert_eq!(epoch.summary, format!("summary-{}", epoch.through_sequence));
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(row.sequence, index as u64 + 1);
            assert_eq!(row.content, format!("content {}", row.sequence));
        }
    }
    writer.join().unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let creators = (0..2)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                store.fork("source", "exclusive", 100)
            })
        })
        .collect::<Vec<_>>();
    let results = creators
        .into_iter()
        .map(|task| task.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    assert_eq!(store.inspect("exclusive").unwrap().len(), 16);
    assert_eq!(
        store
            .compaction_epoch("exclusive")
            .unwrap()
            .unwrap()
            .through_sequence,
        16
    );
    fs::remove_dir_all(root).unwrap();
}

/// Pending terminal witnesses with a nonempty prefix baseline reconcile only on
/// the target. Already-covered witnesses remain idempotent, while foreign or
/// stale baseline evidence fails closed and compensates only the target.
#[test]
fn fork_epoch_terminal_handoffs_translate_and_reconcile_only_target() {
    use crate::storage::transcript::compaction_handoff::{
        TerminalCompactionHandoff, TerminalCompactionReplacement,
    };
    for state in ["pending", "covered", "foreign", "stale"] {
        let root = temp_root("fork-terminal-handoff");
        let store = AgentTranscriptStore::new(root.clone());
        let rows = rows();
        store.append_many(&rows).unwrap();
        store
            .save_compaction_epoch("source", 1, "prefix model summary")
            .unwrap();
        let mut baseline = store.compaction_epoch("source").unwrap();
        if state == "foreign" {
            baseline.as_mut().unwrap().conversation_id = "foreign".into();
        }
        let mut witness = entry("source", 10, TranscriptRole::System);
        witness.content = TerminalCompactionHandoff {
            baseline,
            replacements: vec![TerminalCompactionReplacement {
                sources: rows[1..3].iter().map(|row| row.content.clone()).collect(),
                summary: "accepted model handoff summary".into(),
            }],
        }
        .to_content()
        .unwrap();
        // Single append deliberately leaves the witness unreconciled, as after
        // an interrupted append-receipt handoff; it does not execute actions.
        store.append(&witness).unwrap();
        if state == "covered" {
            store
                .publish_terminal_compaction_handoffs("source")
                .unwrap();
        } else if state == "stale" {
            store
                .save_compaction_epoch("source", 1, "newer independent summary")
                .unwrap();
        }
        let before = source_bytes(&root);
        let result = store.fork("source", "target", 99);
        if matches!(state, "foreign" | "stale") {
            assert!(result.is_err(), "{state}");
            assert!(!root.join("target").exists(), "{state}");
        } else {
            result.unwrap();
            let epoch = store.compaction_epoch("target").unwrap().unwrap();
            assert_eq!(epoch.conversation_id, "target");
            assert_eq!(epoch.through_sequence, 1);
            assert_eq!(epoch.summary, "prefix model summary");
            assert_eq!(
                epoch.ranges,
                vec![AgentCompactionRange {
                    first_sequence: 2,
                    through_sequence: 3,
                    summary: "accepted model handoff summary".into(),
                }]
            );
            store
                .publish_terminal_compaction_handoffs("target")
                .unwrap();
            assert_eq!(store.compaction_epoch("target").unwrap(), Some(epoch));
            assert_eq!(store.inspect("target").unwrap()[1].content, rows[1].content);
        }
        assert_eq!(source_bytes(&root), before, "{state}");
        fs::remove_dir_all(root).unwrap();
    }
}

/// Historical no-epoch forks retain raw history unchanged, while repeated prefix
/// compactions capture only the latest summary and never absorb later parent
/// appends. Empty active tails remain backed by the complete copied archive.
#[test]
fn fork_epoch_latest_prefix_empty_tail_and_legacy_no_epoch() {
    for boundary in [None, Some(0), Some(5), Some(9)] {
        let root = temp_root("fork-prefix-variants");
        let store = AgentTranscriptStore::new(root.clone());
        store.append_many(&rows()).unwrap();
        if let Some(boundary) = boundary {
            store
                .save_compaction_epoch("source", 0, "older model summary")
                .unwrap();
            store
                .save_compaction_epoch("source", boundary, "latest model summary")
                .unwrap();
        }
        let before = source_bytes(&root);
        store.fork("source", "target", 99).unwrap();
        assert_eq!(source_bytes(&root), before);
        let target_epoch = store.compaction_epoch("target").unwrap();
        assert_eq!(
            target_epoch.as_ref().map(|epoch| epoch.through_sequence),
            boundary
        );
        if let Some(epoch) = target_epoch {
            assert_eq!(epoch.summary, "latest model summary");
        }
        store
            .append(&entry("source", 10, TranscriptRole::User))
            .unwrap();
        assert_eq!(store.inspect("target").unwrap().len(), 9);
        fs::remove_dir_all(root).unwrap();
    }
}

/// Creates a valid complete typed group at each selected sequence, with raw
/// user barriers in the uncovered gaps and distinct stable occurrence identity.
fn rows() -> Vec<TranscriptEntry> {
    (1..=9)
        .map(|sequence| {
            let mut row = entry("source", sequence, TranscriptRole::User);
            if matches!(sequence, 2..=3 | 6..=7) {
                row.role = TranscriptRole::System;
                row.content = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                    mez_agent::ContextSourceKind::TranscriptAssistant,
                    "answer",
                    format!("typed-{sequence}"),
                    mez_agent::ContextExecutionGroupId::new(if sequence < 4 {
                        "first"
                    } else {
                        "second"
                    })
                    .unwrap(),
                    if sequence < 4 {
                        sequence - 1
                    } else {
                        sequence - 5
                    },
                    None,
                )
                .unwrap()
                .to_transcript_content();
            }
            row
        })
        .collect()
}

/// Captures authoritative source files, excluding unrelated shared catalog and
/// lock files that target creation legitimately changes.
fn source_bytes(root: &std::path::Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fs::read_dir(root.join("source"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (path.file_name().unwrap().into(), fs::read(path).unwrap())
        })
        .collect()
}

/// Both prefix-zero selective history and combined prefix/selective history
/// retain inclusive anchors, exact gaps, fresh target identity and independently
/// appendable archives after reopening. No terminal handoff reconstruction is
/// available in this fixture: only the authoritative epoch proves the summary.
#[test]
fn fork_epoch_preserves_selective_projection_and_independent_append() {
    for prefix in [0, 1] {
        let root = temp_root("fork-selective-epoch");
        let store = AgentTranscriptStore::new(root.clone());
        let rows = rows();
        store.append_many(&rows).unwrap();
        let epoch = AgentCompactionEpoch {
            version: 2,
            conversation_id: "source".into(),
            through_sequence: prefix,
            summary: if prefix == 0 {
                String::new()
            } else {
                "prefix model summary".into()
            },
            ranges: vec![
                AgentCompactionRange {
                    first_sequence: 2,
                    through_sequence: 3,
                    summary: "first model summary".into(),
                },
                AgentCompactionRange {
                    first_sequence: 6,
                    through_sequence: 7,
                    summary: "second model summary".into(),
                },
            ],
        };
        let proof = rows
            .iter()
            .filter(|row| matches!(row.sequence, 2..=3 | 6..=7))
            .cloned()
            .collect::<Vec<_>>();
        store
            .save_compaction_ranges_with_proof(epoch.clone(), &proof)
            .unwrap();
        let before = source_bytes(&root);
        store.fork("source", "target", 99).unwrap();
        let reopened = AgentTranscriptStore::new(root.clone());
        let mut expected = epoch;
        expected.conversation_id = "target".into();
        assert_eq!(reopened.compaction_epoch("target").unwrap(), Some(expected));
        let copied = reopened.inspect("target").unwrap();
        for (original, copied) in rows.iter().zip(&copied) {
            let mut expected = original.clone();
            expected.conversation_id = "target".into();
            expected.created_at_unix_seconds = 99;
            assert_eq!(*copied, expected);
        }
        assert_eq!(source_bytes(&root), before);
        let mut later = entry("source", 10, TranscriptRole::User);
        store.append(&later).unwrap();
        later.conversation_id = "target".into();
        later.content = "independent target append".into();
        reopened.append(&later).unwrap();
        assert_ne!(
            store.inspect("source").unwrap()[9].content,
            reopened.inspect("target").unwrap()[9].content
        );
        fs::remove_dir_all(root).unwrap();
    }
}

/// Missing/corrupt required epochs and invalid complete archives must fail
/// before a target can be exposed; none may become a legacy raw-history fork.
#[test]
fn fork_epoch_rejects_required_missing_corrupt_or_invalid_source() {
    for failure in ["missing", "corrupt", "gap", "foreign", "unterminated"] {
        let root = temp_root("fork-invalid-source");
        let store = AgentTranscriptStore::new(root.clone());
        store
            .append(&entry("source", 1, TranscriptRole::User))
            .unwrap();
        store.save_compaction_epoch("source", 1, "summary").unwrap();
        let epoch = root.join("source/compaction-epoch.json");
        match failure {
            "missing" => fs::remove_file(epoch).unwrap(),
            "corrupt" => fs::write(epoch, "invalid epoch").unwrap(),
            "gap" | "foreign" => {
                let mut changed = entry(
                    "source",
                    if failure == "gap" { 2 } else { 1 },
                    TranscriptRole::User,
                );
                if failure == "foreign" {
                    changed.conversation_id = "foreign".into();
                }
                fs::write(
                    store.transcript_path("source").unwrap(),
                    format!("{}\n", encode_transcript_entry(&changed).unwrap()),
                )
                .unwrap();
            }
            _ => {
                fs::write(
                    store.transcript_path("source").unwrap(),
                    encode_transcript_entry(&entry("source", 1, TranscriptRole::User)).unwrap(),
                )
                .unwrap();
            }
        }
        let before = source_bytes(&root);
        assert!(store.fork("source", "target", 99).is_err(), "{failure}");
        assert!(!root.join("target").exists(), "{failure}");
        assert_eq!(source_bytes(&root), before, "{failure}");
        fs::remove_dir_all(root).unwrap();
    }
}

/// Failures during target archive append or either side of atomic epoch rename
/// compensate only the newly owned target. The source's exact bytes survive,
/// and reopening cannot discover a partial raw-only fork as a saved session.
#[test]
fn fork_epoch_target_publication_failures_remove_only_new_target() {
    for failure in ["append", "epoch", "rename", "marker"] {
        let root = temp_root("fork-epoch-failure");
        let store = AgentTranscriptStore::new(root.clone());
        store
            .append(&entry("source", 1, TranscriptRole::User))
            .unwrap();
        store
            .save_compaction_epoch("source", 1, "model summary")
            .unwrap();
        let before = source_bytes(&root);
        match failure {
            "append" => store.fail_transcript_append_before_sync(),
            "epoch" => store.fail_next_compaction_epoch_write(),
            "rename" => store.fail_next_compaction_epoch_before_rename(),
            _ => store.fail_next_compaction_epoch_before_marker(),
        }
        assert!(store.fork("source", "target", 99).is_err(), "{failure}");
        assert_eq!(source_bytes(&root), before, "{failure}");
        assert!(!root.join("target").exists(), "{failure}");
        assert!(
            AgentTranscriptStore::new(root.clone())
                .saved_session("target")
                .unwrap()
                .is_none()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
