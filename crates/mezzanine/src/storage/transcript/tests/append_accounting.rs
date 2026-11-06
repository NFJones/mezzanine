//! Ordinary receipt identity, single-lease commit and interrupted publication.
//!
//! These tests use real archives and original receipt payloads. Equal raw bytes
//! cannot substitute another producer's metadata, and recovery never overwrites
//! conflicting placement or mutates an unrelated archive before preflight.

use super::*;
use crate::storage::transcript::{ConversationTranscriptRead, TranscriptAppendBatch};
use mez_agent::{ContextBlock, ContextBlockRecord, ContextSourceKind};
use std::{sync::mpsc, time::Duration};

/// Produces equal Unicode/code bodies as independent original raw occurrences.
fn batch(cid: &str) -> TranscriptAppendBatch {
    let rows = (1..=2)
        .map(|sequence| {
            let mut row = entry(cid, sequence, TranscriptRole::Assistant);
            row.content = "equal 雪 dense {\"rows\":[1,2,3]}".into();
            row
        })
        .collect();
    TranscriptAppendBatch::owned_rows(rows).unwrap()
}

/// Captures exact journal evidence without interpreting it as source authority.
fn journals(root: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut paths = fs::read_dir(root.join(".append-receipts"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

/// The v6 domain binds the original proposal, not just equal physical rows.
/// Exact retries remain journal-only until commit; late completion requires
/// exact raw bytes and original published placement before it can retire.
#[test]
fn ordinary_accounted_receipt_binds_original_proposal_through_commit_and_retry() {
    let root = temp_root("ordinary-accounted-codec");
    let store = AgentTranscriptStore::new(&root);
    let original = batch("accounted-ordinary");
    let origins = original.row_accounting().unwrap().origins().clone();
    assert_ne!(origins[&1], origins[&2]);
    store
        .accept_transcript_append_receipt(&original, 7)
        .unwrap();
    assert!(
        !store
            .transcript_path(original.conversation_id())
            .unwrap()
            .exists()
    );
    let retained = journals(&root);
    assert_eq!(retained.len(), 1);
    let encoded: serde_json::Value = serde_json::from_slice(&retained[0].1).unwrap();
    assert_eq!(encoded["version"], 6);
    assert_eq!(encoded["row_accounting"]["version"], 1);
    assert!(encoded.get("compaction_epoch").is_none());
    assert!(encoded.get("initial_row_accounting").is_none());
    store
        .accept_transcript_append_receipt(&original.clone(), 7)
        .unwrap();
    assert_eq!(journals(&root), retained);
    let foreign = TranscriptAppendBatch::owned_rows(original.entries().to_vec()).unwrap();
    assert!(store.accept_transcript_append_receipt(&foreign, 7).is_err());
    assert_eq!(journals(&root), retained);
    assert!(
        store
            .settle_transcript_append_receipt(&original, 7)
            .is_err()
    );
    assert_eq!(journals(&root), retained);
    assert!(
        store
            .commit_accounted_transcript_batch(&original, 7)
            .unwrap()
            > 0
    );
    assert!(store.pending_append_receipts().unwrap().is_empty());
    let raw = fs::read(store.transcript_path(original.conversation_id()).unwrap()).unwrap();
    // A future bounded replay can verify a lagging original proposal without
    // trusting text or an occurrence alone: the existing accounting owner must
    // durably bind every persisted field to that same original occurrence.
    let index = rusqlite::Connection::open(
        root.join(original.conversation_id())
            .join("row-origins.sqlite3"),
    )
    .unwrap();
    for row in original.entries() {
        let (origin, digest): (String, String) = index
            .query_row(
                "SELECT origin, digest FROM row_digests WHERE sequence=?1",
                [row.sequence.to_string()],
                |value| Ok((value.get(0)?, value.get(1)?)),
            )
            .unwrap();
        assert_eq!(origin, String::from(origins[&row.sequence]));
        assert_eq!(digest, store.transcript_entry_revision(row).unwrap());
    }
    drop(index);
    assert_eq!(
        store.inspect(original.conversation_id()).unwrap(),
        original.entries()
    );
    assert_eq!(
        store
            .commit_accounted_transcript_batch(&original.clone(), 7)
            .unwrap(),
        0
    );
    assert_eq!(
        fs::read(store.transcript_path(original.conversation_id()).unwrap()).unwrap(),
        raw
    );
    let cold = AgentTranscriptStore::new(&root)
        .conversation_replay_snapshot(original.conversation_id(), &[], |_| {
            Some((ConversationTranscriptRead::All, true))
        })
        .unwrap();
    assert_eq!(cold.row_origins, origins);
    assert!(
        store
            .commit_accounted_transcript_batch(&foreign, 8)
            .is_err()
    );
    assert!(store.pending_append_receipts().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// Tampering with a well-shaped original proposal must invalidate its digest,
/// including descriptive costs. Unsupported shapes must likewise preserve the
/// damaged journal and refuse recovery before an earlier unrelated write.
#[test]
fn ordinary_accounted_receipt_rejects_tampering_before_unrelated_recovery() {
    for fault in ["origin", "cost", "version", "unknown-field", "initial"] {
        let root = temp_root("ordinary-accounted-tamper");
        let store = AgentTranscriptStore::new(&root);
        let proposed = batch("zzz-tampered");
        let origin = proposed.row_accounting().unwrap().origins()[&1];
        let record = ContextBlockRecord::for_occurrence(
            origin,
            &ContextBlock::reference_event(
                ContextSourceKind::TranscriptAssistant,
                "source",
                "rendered source",
            ),
        )
        .unwrap();
        let mut accounting = serde_json::to_value(proposed.row_accounting().unwrap()).unwrap();
        accounting["records"]["1"] = serde_json::to_value(record).unwrap();
        let original = TranscriptAppendBatch::rows(proposed.entries().to_vec())
            .unwrap()
            .with_row_accounting(serde_json::from_value(accounting).unwrap())
            .unwrap();
        store
            .accept_transcript_append_receipt(&original, 1)
            .unwrap();
        let (path, bytes) = journals(&root).pop().unwrap();
        let mut encoded: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        match fault {
            "origin" => {
                let replacement = mez_agent::ContextBlockOccurrenceId::allocate();
                let value = serde_json::to_value(replacement).unwrap();
                encoded["row_accounting"]["origins"]["1"] = value.clone();
                encoded["row_accounting"]["records"]["1"]["occurrence_id"] = value;
            }
            "cost" => {
                let cost = &mut encoded["row_accounting"]["records"]["1"]["framed_input_tokens"];
                *cost = (cost.as_u64().unwrap() + 1).into();
            }
            "version" => encoded["row_accounting"]["version"] = 99.into(),
            "unknown-field" => encoded["row_accounting"]["future_authority"] = true.into(),
            _ => encoded["initial_row_accounting"] = encoded["row_accounting"].clone(),
        }
        fs::write(&path, serde_json::to_vec(&encoded).unwrap()).unwrap();
        // Admit the independent owner first so its valid journal exists even
        // while the selected conversation's damaged receipt remains isolated.
        let earlier = entry("000-earlier", 1, TranscriptRole::User);
        store
            .accept_append_receipt(std::slice::from_ref(&earlier), 1)
            .unwrap();
        let retained = journals(&root);
        assert!(store.pending_append_receipts().is_err(), "{fault}");
        assert!(store.recover_append_receipts().is_err(), "{fault}");
        assert_eq!(journals(&root), retained, "{fault}");
        for cid in [original.conversation_id(), &earlier.conversation_id] {
            assert!(!store.transcript_path(cid).unwrap().exists(), "{fault}");
        }
        let independent = entry("independent-healthy", 1, TranscriptRole::User);
        store.append(&independent).unwrap();
        assert_eq!(
            store.inspect(&independent.conversation_id).unwrap(),
            vec![independent]
        );
        fs::remove_dir_all(root).unwrap();
    }
}

/// An accepted journal owns the raw/map crash window. Both a partial raw prefix
/// and complete raw rows with no overrides recover under the replay's existing
/// lease, without deadlock or draining another conversation's valid receipt.
#[test]
fn ordinary_accounted_cold_replay_recovers_partial_publication_under_one_lease() {
    for whole_raw in [false, true] {
        let root = temp_root("ordinary-accounted-partial");
        let store = AgentTranscriptStore::new(&root);
        let original = batch("original-partial");
        store
            .accept_transcript_append_receipt(&original, 1)
            .unwrap();
        let unrelated = entry("unrelated-pending", 1, TranscriptRole::User);
        store
            .accept_append_receipt(std::slice::from_ref(&unrelated), 1)
            .unwrap();
        store
            .append_many(if whole_raw {
                original.entries()
            } else {
                &original.entries()[..1]
            })
            .unwrap();
        let prefix = fs::read(store.transcript_path(original.conversation_id()).unwrap()).unwrap();
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn({
            let cold = AgentTranscriptStore::new(&root);
            move || {
                tx.send(
                    cold.conversation_replay_snapshot("original-partial", &[], |_| {
                        Some((ConversationTranscriptRead::All, true))
                    }),
                )
                .unwrap()
            }
        });
        let replay = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("single-lease replay must finish")
            .unwrap();
        worker.join().unwrap();
        assert_eq!(replay.view.committed, original.entries());
        assert_eq!(
            replay.row_origins,
            *original.row_accounting().unwrap().origins()
        );
        assert!(
            fs::read(store.transcript_path(original.conversation_id()).unwrap())
                .unwrap()
                .starts_with(&prefix)
        );
        assert_eq!(
            store.pending_append_receipts().unwrap(),
            vec![vec![unrelated.clone()]]
        );
        assert!(
            !store
                .transcript_path(&unrelated.conversation_id)
                .unwrap()
                .exists()
        );
        fs::remove_dir_all(root).unwrap();
    }
}

/// Fresh proposals cannot adopt already committed unaccounted history merely
/// because all raw fields match. Genuine legacy archive identity stays intact.
#[test]
fn ordinary_accounted_admission_refuses_equal_committed_legacy_adoption() {
    let root = temp_root("ordinary-accounted-legacy-adoption");
    let store = AgentTranscriptStore::new(&root);
    let original = batch("legacy-committed");
    store.append_many(original.entries()).unwrap();
    let raw = fs::read(store.transcript_path(original.conversation_id()).unwrap()).unwrap();
    let before = store
        .conversation_replay_snapshot(original.conversation_id(), &[], |_| {
            Some((ConversationTranscriptRead::All, true))
        })
        .unwrap();
    assert_ne!(
        before.row_origins,
        *original.row_accounting().unwrap().origins()
    );
    assert!(
        store
            .accept_transcript_append_receipt(&original, 1)
            .is_err()
    );
    assert!(store.pending_append_receipts().unwrap().is_empty());
    let after = store
        .conversation_replay_snapshot(original.conversation_id(), &[], |_| {
            Some((ConversationTranscriptRead::All, true))
        })
        .unwrap();
    assert_eq!(before.row_origins, after.row_origins);
    assert_eq!(
        fs::read(store.transcript_path(original.conversation_id()).unwrap()).unwrap(),
        raw
    );
    fs::remove_dir_all(root).unwrap();
}

/// Global recovery must preflight immutable placement and descriptive cache
/// integrity before an earlier valid receipt can write unrelated raw history.
/// Corruption remains for diagnosis while independent ordinary I/O still works.
#[test]
fn ordinary_accounted_recovery_preflights_conflicts_before_unrelated_mutation() {
    for fault in [
        "placement",
        "cache-identity",
        "cache-format",
        "schema",
        "missing",
    ] {
        let root = temp_root("ordinary-accounted-global-preflight");
        let store = AgentTranscriptStore::new(&root);
        let original = batch("zzz-accounted");
        store
            .commit_accounted_transcript_batch(&original, 1)
            .unwrap();
        store
            .accept_transcript_append_receipt(&original, 1)
            .unwrap();
        let earlier = entry("000-earlier", 1, TranscriptRole::User);
        store
            .accept_append_receipt(std::slice::from_ref(&earlier), 1)
            .unwrap();
        let database = root.join("zzz-accounted/row-origins.sqlite3");
        let foreign = ContextBlockRecord::allocate(&ContextBlock::reference_event(
            ContextSourceKind::TranscriptAssistant,
            "foreign",
            "foreign source",
        ))
        .unwrap();
        if fault == "missing" {
            fs::remove_file(&database).unwrap();
        } else {
            let db = Connection::open(&database).unwrap();
            match fault {
                "placement" => {
                    db.execute(
                        "UPDATE origins SET origin=?1 WHERE sequence='2'",
                        [String::from(foreign.occurrence_id())],
                    )
                    .unwrap();
                }
                "schema" => {
                    db.execute_batch("DROP TABLE row_costs;").unwrap();
                }
                _ => {
                    let origin = original.row_accounting().unwrap().origins()[&2];
                    let record = if fault == "cache-identity" {
                        foreign
                    } else {
                        ContextBlockRecord::for_occurrence(
                            origin,
                            &ContextBlock::reference_event(
                                ContextSourceKind::TranscriptAssistant,
                                "cached",
                                "source",
                            ),
                        )
                        .unwrap()
                    };
                    let mut encoded = serde_json::to_value(record).unwrap();
                    if fault == "cache-format" {
                        encoded["format_version"] = 99.into();
                    }
                    db.execute(
                        "INSERT INTO row_costs VALUES (?1, ?2)",
                        rusqlite::params![String::from(origin), encoded.to_string()],
                    )
                    .unwrap();
                }
            }
        }
        let retained = journals(&root);
        let data = fs::read(&database).ok();
        let raw = fs::read(store.transcript_path(original.conversation_id()).unwrap()).unwrap();
        assert!(store.recover_append_receipts().is_err(), "{fault}");
        assert_eq!(journals(&root), retained, "{fault}");
        assert_eq!(fs::read(&database).ok(), data, "{fault}");
        assert_eq!(
            fs::read(store.transcript_path(original.conversation_id()).unwrap()).unwrap(),
            raw,
            "{fault}"
        );
        assert!(
            !store
                .transcript_path(&earlier.conversation_id)
                .unwrap()
                .exists(),
            "{fault}"
        );
        let independent = entry("independent-append", 1, TranscriptRole::User);
        store.append(&independent).unwrap();
        assert_eq!(
            store.inspect(&independent.conversation_id).unwrap(),
            vec![independent]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
