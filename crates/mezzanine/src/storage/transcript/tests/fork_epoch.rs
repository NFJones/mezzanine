//! Fork publication preserves exact active history without changing its parent.
//!
//! Synthetic archives cover selective anchors, prefix boundaries, crash-safe
//! epoch publication, and fail-closed source validation. No actions are replayed.

use super::*;
use crate::storage::transcript::{AgentCompactionEpoch, AgentCompactionRange};

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
