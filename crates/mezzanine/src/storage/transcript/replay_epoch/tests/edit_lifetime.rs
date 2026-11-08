//! Required edit backing survives uncertain completion and conversation
//! lifecycle operations. Every scenario uses real save/fork/retention/archive
//! owners; removing a required synthetic file exercises corruption handling,
//! never authorizes a production per-payload garbage collector.

use super::raw_edits::{CID, load, save, saved_raw};
use super::*;
use crate::storage::transcript::{
    CompareAndSwapTranscriptEntryResult as Cas, SavedSessionRetentionPolicy,
};
use mez_agent::transcript::TranscriptContextRecord;

fn saved(
    fixture: &Fixture,
    row: &mez_agent::TranscriptEntry,
    content: &str,
) -> mez_agent::TranscriptEntry {
    let Cas::Updated(row) = save(fixture, row, content).unwrap() else {
        panic!("expected actual content save");
    };
    row
}

fn manifest(fixture: &Fixture) -> serde_json::Value {
    serde_json::from_slice(&fs::read(fixture.root.join(CID).join(MANIFEST_NAME)).unwrap()).unwrap()
}

/// A repeated edit has a distinct operation parent and original head base.
/// Both receipts, the replacement receipt and required old/new current bytes
/// must remain available before AND after the raw rename. Missing source blocks
/// model replay, explicit delete/archive and retention without losing the
/// recovery intent; restoring exact bytes completes one original replacement.
#[test]
fn raw_edit_missing_required_backing_fences_recovery_and_lifecycle_cleanup() {
    for phase in [Phase::AfterRawEditIntent, Phase::AfterRawEditArchive] {
        for role in ["original", "replacement", "base", "base_raw", "new_raw"] {
            let (fixture, row) = saved_raw();
            let first = saved(&fixture, &row, "first actual durable edit");
            let (identity, before) = load(&fixture);
            fixture.store.inject_replay_epoch_fault(phase, 1, false);
            assert!(save(&fixture, &first, "exact second edit awaiting recovery").is_err());
            let directory = fixture.root.join(CID);
            let intent_path = directory.join(".raw-content-edit.json");
            let intent_bytes = fs::read(&intent_path).unwrap();
            let intent: serde_json::Value = serde_json::from_slice(&intent_bytes).unwrap();
            let intent = &intent["intent"];
            let payloads = directory.join("epoch-payloads");
            let replacement: serde_json::Value = serde_json::from_slice(
                &fs::read(payloads.join(intent["replacement"]["sha256"].as_str().unwrap()))
                    .unwrap(),
            )
            .unwrap();
            let base = &replacement["manifest"]["raw_revision_origin"]["base"];
            assert_ne!(base["sha256"], intent["original"]["sha256"]);
            let base_envelope: serde_json::Value = serde_json::from_slice(
                &fs::read(payloads.join(base["sha256"].as_str().unwrap())).unwrap(),
            )
            .unwrap();
            let id = serde_json::to_value(
                TranscriptContextRecord::from_transcript_content(&row.content)
                    .unwrap()
                    .unwrap()
                    .record()
                    .occurrence_id(),
            )
            .unwrap();
            let raw_reference = |envelope: &serde_json::Value| {
                envelope["manifest"]["checkpoint"]["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|event| event["record"]["occurrence_id"] == id)
                    .unwrap()["block"]["content"]
                    .clone()
            };
            let reference = match role {
                "original" | "replacement" => intent[role].clone(),
                "base" => base.clone(),
                "base_raw" => raw_reference(&base_envelope),
                "new_raw" => raw_reference(&replacement),
                _ => unreachable!(),
            };
            let path = payloads.join(reference["sha256"].as_str().unwrap());
            let required = fs::read(&path).unwrap();
            let permissions = fs::metadata(&path).unwrap().permissions();
            fs::remove_file(&path).unwrap();
            let raw_path = fixture.store.existing_transcript_path_for(CID).unwrap();
            let archive = fs::read(&raw_path).unwrap();
            let authority = fs::read(directory.join(MANIFEST_NAME)).unwrap();
            assert!(
                fixture.store.load_replay_epoch(CID, prefix()).is_err(),
                "{phase:?}/{role}"
            );
            assert!(
                fixture.store.delete(CID).is_err(),
                "delete {phase:?}/{role}"
            );
            assert!(
                fixture.store.archive_session(CID, 200_000).is_err(),
                "archive {phase:?}/{role}"
            );
            let mut retention = fixture.store.clone();
            retention
                .set_saved_session_retention_policy(SavedSessionRetentionPolicy {
                    max_active_sessions: 100,
                    retention_days: 1,
                })
                .unwrap();
            let report = retention
                .enforce_saved_session_retention(
                    200_000,
                    &BTreeSet::from(["conversation-a".into()]),
                )
                .unwrap();
            assert!(
                report
                    .failures
                    .iter()
                    .any(|failure| failure.conversation_id == CID),
                "retention {phase:?}/{role}"
            );
            assert!(!report.deleted_conversation_ids.iter().any(|id| id == CID));
            assert_eq!(fs::read(&intent_path).unwrap(), intent_bytes);
            assert_eq!(fs::read(&raw_path).unwrap(), archive);
            assert_eq!(fs::read(directory.join(MANIFEST_NAME)).unwrap(), authority);
            fs::write(&path, required).unwrap();
            fs::set_permissions(path, permissions).unwrap();
            let (recovered_identity, recovered) = load(&fixture);
            assert_eq!(recovered_identity.revision, identity.revision + 1);
            assert_eq!(recovered.revision(), before.revision());
            assert_eq!(recovered.head(), before.head());
            assert_eq!(recovered.members(), before.members());
            assert_eq!(
                recovered.context().chronology()[1].block().content,
                "exact second edit awaiting recovery"
            );
            assert!(!intent_path.exists());
        }
    }
}

/// The flat original-head proof remains required while that head is active,
/// whereas an intermediate edit receipt is provenance rather than a replay
/// dependency after operation settlement. Forks back only their selected body
/// and remain readable even when required proof is missing in the parent.
#[test]
fn raw_edit_settled_parent_and_independent_fork_have_distinct_required_backing() {
    let (fixture, row) = saved_raw();
    let first = saved(&fixture, &row, "first durable member image");
    let intermediate = fs::read(fixture.root.join(CID).join(MANIFEST_NAME)).unwrap();
    saved(&fixture, &first, "latest durable member image");
    let (identity, current) = load(&fixture);
    let payloads = fixture.root.join(CID).join("epoch-payloads");
    fs::remove_file(payloads.join(digest(&intermediate))).unwrap();
    assert_eq!(load(&fixture).0, identity);
    fixture
        .store
        .fork(CID, "independent-edit-child", 200)
        .unwrap();
    let current_manifest = manifest(&fixture);
    let base = &current_manifest["manifest"]["raw_revision_origin"]["base"];
    let path = payloads.join(base["sha256"].as_str().unwrap());
    let bytes = fs::read(&path).unwrap();
    let permissions = fs::metadata(&path).unwrap().permissions();
    fs::remove_file(&path).unwrap();
    assert!(fixture.store.load_replay_epoch(CID, prefix()).is_err());
    let (_, child) = fixture
        .store
        .load_replay_epoch("independent-edit-child", prefix())
        .unwrap()
        .unwrap();
    assert_eq!(child.context().chronology(), current.context().chronology());
    assert_eq!(child.members(), current.members());
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(path, permissions).unwrap();
    fixture.store.delete(CID).unwrap();
    fixture.store.delete("conversation-a").unwrap();
    let (_, reopened) = fixture
        .store
        .load_replay_epoch("independent-edit-child", prefix())
        .unwrap()
        .unwrap();
    assert_eq!(
        reopened.context().chronology(),
        child.context().chronology()
    );
}

/// Whole-conversation cleanup may remove its own backing, never a child's
/// independent payloads. Archival must preserve the full edited authority and
/// its original proof through actual pack/extract/restore before deletion.
#[test]
fn raw_edit_fork_survives_parent_retention_and_archive_lifecycle() {
    for archive in [false, true] {
        let (fixture, row) = saved_raw();
        let first = saved(&fixture, &row, "first lifecycle edit");
        saved(&fixture, &first, "current lifecycle edit");
        let (identity, current) = load(&fixture);
        fixture.store.fork(CID, "lifecycle-child", 200).unwrap();
        fixture.store.delete("conversation-a").unwrap();
        if archive {
            fixture.store.archive_session(CID, 300).unwrap();
            assert!(!fixture.root.join(CID).exists());
            assert!(
                fixture
                    .store
                    .load_replay_epoch("lifecycle-child", prefix())
                    .unwrap()
                    .is_some()
            );
            fixture.store.restore_archived_session(CID).unwrap();
            assert_eq!(load(&fixture).0, identity);
            assert_eq!(
                load(&fixture).1.context().chronology(),
                current.context().chronology()
            );
            fixture.store.archive_session(CID, 400).unwrap();
            assert!(fixture.store.delete_archived_session(CID).unwrap());
        } else {
            let mut retention = fixture.store.clone();
            retention
                .set_saved_session_retention_policy(SavedSessionRetentionPolicy {
                    max_active_sessions: 100,
                    retention_days: 1,
                })
                .unwrap();
            let report = retention
                .enforce_saved_session_retention(
                    200_000,
                    &BTreeSet::from(["lifecycle-child".into()]),
                )
                .unwrap();
            assert!(report.failures.is_empty(), "{report:?}");
            assert_eq!(report.deleted_conversation_ids, vec![CID]);
        }
        assert!(!fixture.root.join(CID).exists());
        let (_, child) = fixture
            .store
            .load_replay_epoch("lifecycle-child", prefix())
            .unwrap()
            .unwrap();
        assert_eq!(child.context().chronology(), current.context().chronology());
        assert_eq!(child.members(), current.members());
        assert_eq!(
            child.head().unwrap().conversation().as_str(),
            "lifecycle-child"
        );
    }
}
