//! Real persisted origin/resequence crash boundaries, including local refusal.

use super::*;
use mez_agent::TranscriptRole;

/// Keeps recovery tests independent of catalog setup and runtime actor state.
fn fixture(label: &str) -> (AgentTranscriptStore, Vec<TranscriptEntry>) {
    let store = AgentTranscriptStore::new(std::env::temp_dir().join(format!(
        "mez-row-origins-{label}-{}",
        String::from(ContextBlockOccurrenceId::allocate())
    )));
    let rows = (1..=3)
        .map(|sequence| TranscriptEntry {
            conversation_id: "source".into(),
            sequence,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "turn".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: "same literal body 雪".into(),
        })
        .collect::<Vec<_>>();
    for row in &rows {
        store.append(row).unwrap();
    }
    (store, rows)
}

/// Both sides of the raw rename recover the exact surviving IDs. Equal bodies
/// cannot substitute for the original row's structural position. Subsequent
/// appends receive a namespace that cannot reuse the deleted occurrence.
#[test]
fn interrupted_resequence_recovers_old_or_new_exact_image() {
    for renamed in [false, true] {
        let (store, rows) = fixture(if renamed { "new" } else { "old" });
        let lease = store.acquire_conversation_lock("source").unwrap();
        let original = store
            .transcript_row_origins_locked("source", &rows)
            .unwrap();
        let mut retained = rows[1..].to_vec();
        for (index, row) in retained.iter_mut().enumerate() {
            row.sequence = index as u64 + 1;
        }
        let expected = BTreeMap::from([(1, original[&2]), (2, original[&3])]);
        let raw = store.existing_transcript_path_for("source").unwrap();
        store
            .prepare_row_origins_rewrite_locked("source", &raw, &retained, &expected)
            .unwrap();
        if renamed {
            let mut bytes = String::new();
            for row in &retained {
                bytes.push_str(&encode_transcript_entry(row).unwrap());
                bytes.push('\n');
            }
            fs::write(&raw, bytes).unwrap();
        }
        drop(lease);
        let reopened = AgentTranscriptStore::new(store.root());
        let lease = reopened.acquire_conversation_lock("source").unwrap();
        let actual = reopened
            .transcript_row_origins_locked("source", if renamed { &retained } else { &rows })
            .unwrap();
        assert_eq!(actual, if renamed { expected } else { original });
        drop(lease);
        fs::remove_dir_all(store.root()).unwrap();
    }
}

/// Missing or corrupt identity state is never repaired by assigning replacement
/// IDs. Failure stays local: another conversation can still append and replay.
#[test]
fn required_origin_loss_or_corruption_refuses_only_affected_conversation() {
    for corrupt in [false, true] {
        let (store, rows) = fixture(if corrupt { "corrupt" } else { "missing" });
        let lease = store.acquire_conversation_lock("source").unwrap();
        store
            .transcript_row_origins_locked("source", &rows)
            .unwrap();
        drop(lease);
        let path = store.root().join("source").join(FILE_NAME);
        if corrupt {
            fs::write(&path, b"invalid sqlite").unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
        assert!(store.acquire_conversation_lock("source").is_err());
        let mut independent = rows[0].clone();
        independent.conversation_id = "unaffected".into();
        store.append(&independent).unwrap();
        assert_eq!(store.inspect("unaffected").unwrap(), [independent]);
        fs::remove_dir_all(store.root()).unwrap();
    }
}

/// An unexpected third raw image cannot select either pending mapping. The
/// retained recovery intent makes the refusal repeatable without destroying
/// evidence, while restoring the actual old image permits bounded recovery.
#[test]
fn changed_resequence_image_retains_recovery_evidence() {
    let (store, rows) = fixture("mismatch");
    let lease = store.acquire_conversation_lock("source").unwrap();
    let origins = store
        .transcript_row_origins_locked("source", &rows)
        .unwrap();
    let raw = store.existing_transcript_path_for("source").unwrap();
    let old = fs::read(&raw).unwrap();
    store
        .prepare_row_origins_rewrite_locked("source", &raw, &[], &BTreeMap::new())
        .unwrap();
    fs::write(&raw, b"unexpected changed source").unwrap();
    drop(lease);
    assert!(store.acquire_conversation_lock("source").is_err());
    assert!(store.acquire_conversation_lock("source").is_err());
    fs::write(&raw, old).unwrap();
    let lease = store.acquire_conversation_lock("source").unwrap();
    assert_eq!(
        store
            .transcript_row_origins_locked("source", &rows)
            .unwrap(),
        origins
    );
    drop(lease);
    fs::remove_dir_all(store.root()).unwrap();
}

/// An accepted append still owns its sequence before bytes appear. Resequencing
/// cannot invalidate that original payload or consume its receipt on the side.
#[test]
fn resequence_waits_for_original_append_receipt_owner() {
    let (store, rows) = fixture("pending-receipt");
    let mut pending = rows[0].clone();
    pending.sequence = 4;
    store
        .accept_append_receipt(std::slice::from_ref(&pending), 1)
        .unwrap();
    let raw = fs::read(store.transcript_path("source").unwrap()).unwrap();
    assert!(store.delete_entry("source", 1).is_err());
    assert_eq!(
        fs::read(store.transcript_path("source").unwrap()).unwrap(),
        raw
    );
    assert!(store.has_unsettled_append_receipts("source").unwrap());
    store.recover_append_receipts().unwrap();
    assert_eq!(store.inspect("source").unwrap().last(), Some(&pending));
    assert!(store.delete_entry("source", 1).unwrap());
    fs::remove_dir_all(store.root()).unwrap();
}

/// Authored records and explicit edits share a single immutable row occurrence.
/// A cache/source revision may change, but replacing its ID is rejected; removing
/// the typed envelope still leaves the original ID durably pinned in the ledger.
#[test]
fn authored_edits_cannot_replace_or_lose_original_occurrence() {
    let (store, rows) = fixture("authored-edit");
    let block = mez_agent::ContextBlock::reference_event(
        mez_agent::ContextSourceKind::TranscriptAssistant,
        "assistant",
        "body",
    );
    let record = mez_agent::ContextBlockRecord::allocate(&block).unwrap();
    let event = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
        block.source,
        &block.label,
        &block.content,
        mez_agent::ContextExecutionGroupId::new("group").unwrap(),
        1,
        None,
    )
    .unwrap();
    let encode = |record| {
        TranscriptContextRecord::new(event.clone(), record, None)
            .unwrap()
            .to_transcript_content()
            .unwrap()
    };
    let mut row = rows[0].clone();
    row.sequence = 4;
    row.role = TranscriptRole::System;
    row.content = encode(record);
    store.append(&row).unwrap();
    let revision = store.transcript_entry_revision(&row).unwrap();
    assert!(
        store
            .compare_and_swap_entry_content(
                "source",
                4,
                "%1",
                &revision,
                encode(mez_agent::ContextBlockRecord::allocate(&block).unwrap())
            )
            .is_err()
    );
    assert_eq!(store.inspect("source").unwrap().last(), Some(&row));
    store
        .compare_and_swap_entry_content("source", 4, "%1", &revision, "plain system audit".into())
        .unwrap();
    let lease = store.acquire_conversation_lock("source").unwrap();
    let updated = store.inspect("source").unwrap();
    let origins = store
        .transcript_row_origins_locked("source", &updated)
        .unwrap();
    assert_eq!(origins[&4], record.occurrence_id());
    drop(lease);
    fs::remove_dir_all(store.root()).unwrap();
}

/// A literal reserved marker in a user row cannot supply an authored UUID or
/// counting claim. Import preserves its role and bytes even beside typed rows.
#[test]
fn literal_user_accounting_marker_remains_ordinary_source() {
    let (store, rows) = fixture("literal-marker");
    let mut row = rows[0].clone();
    row.sequence = 4;
    row.content = "[mez-transcript-context-record/v1]\n{invalid metadata}".into();
    store.append(&row).unwrap();
    let lease = store.acquire_conversation_lock("source").unwrap();
    let origins = store
        .transcript_row_origins_locked("source", std::slice::from_ref(&row))
        .unwrap();
    assert_eq!(
        origins[&4],
        ContextBlockOccurrenceId::for_legacy_archive_row("source", 4, 0).unwrap()
    );
    assert_eq!(store.inspect("source").unwrap().last(), Some(&row));
    drop(lease);
    fs::remove_dir_all(store.root()).unwrap();
}

/// Genuine pre-projection layouts import once under a replay lease, including
/// split root-level raw TSVs. Every supported metadata version keeps exact raw
/// bytes/roles and original distinct origins, then cold replay reuses them.
#[test]
fn metadata_versions_one_through_four_import_original_raw_origins() {
    for version in 1..=4 {
        let (store, rows) = fixture("old-metadata");
        let directory = store.root().join("source");
        fs::remove_file(directory.join(FILE_NAME)).unwrap();
        fs::write(
            directory.join("metadata.json"),
            serde_json::json!({"version": version, "conversation_kind": "root"}).to_string(),
        )
        .unwrap();
        let legacy = store.root().join("source.tsv");
        fs::rename(directory.join("history.tsv"), &legacy).unwrap();
        let raw = fs::read(&legacy).unwrap();
        let snapshot = store
            .conversation_replay_snapshot("source", &[], |_| {
                Some((super::super::ConversationTranscriptRead::All, true))
            })
            .unwrap();
        assert_eq!(snapshot.view.logical, rows);
        for row in &rows {
            assert_eq!(
                snapshot.row_origins[&row.sequence],
                ContextBlockOccurrenceId::for_legacy_archive_row("source", row.sequence, 0)
                    .unwrap()
            );
        }
        assert_eq!(
            fs::read(store.existing_transcript_path_for("source").unwrap()).unwrap(),
            raw
        );
        let metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("metadata.json")).unwrap()).unwrap();
        assert_eq!(metadata["version"], 5);
        assert_eq!(metadata["row_origins_required"], true);
        let reopened = AgentTranscriptStore::new(store.root());
        let next = reopened
            .conversation_replay_snapshot("source", &[], |_| {
                Some((super::super::ConversationTranscriptRead::All, true))
            })
            .unwrap();
        assert_eq!(next.row_origins, snapshot.row_origins);
        fs::remove_dir_all(store.root()).unwrap();
    }
}

/// Resequenced overrides and the fresh append namespace are authoritative
/// conversation payload. Archive/restore must retain both exactly; rebuilding
/// discovery metadata must not regenerate or discard original occurrences.
#[test]
fn archive_restore_retains_resequenced_origin_payload() {
    let (store, rows) = fixture("archive");
    let lease = store.acquire_conversation_lock("source").unwrap();
    let original = store
        .transcript_row_origins_locked("source", &rows)
        .unwrap();
    drop(lease);
    store.delete_entry("source", 1).unwrap();
    store.archive_session("source", 100).unwrap();
    store.restore_archived_session("source").unwrap();
    let reopened = AgentTranscriptStore::new(store.root());
    let lease = reopened.acquire_conversation_lock("source").unwrap();
    let retained = reopened.inspect("source").unwrap();
    let origins = reopened
        .transcript_row_origins_locked("source", &retained)
        .unwrap();
    assert_eq!(
        origins,
        BTreeMap::from([(1, original[&2]), (2, original[&3])])
    );
    drop(lease);
    fs::remove_dir_all(store.root()).unwrap();
}

/// Future persistence formats must not be treated as an empty migration and
/// reassigned current IDs. Refusal leaves the future database bytes untouched.
#[test]
fn unknown_row_origin_schema_is_preserved_and_refused() {
    let (store, _) = fixture("future-format");
    let path = store.root().join("source").join(FILE_NAME);
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("PRAGMA user_version=99;").unwrap();
    drop(connection);
    let before = fs::read(&path).unwrap();
    let error = store.acquire_conversation_lock("source").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported transcript row origins version")
    );
    assert_eq!(fs::read(path).unwrap(), before);
    fs::remove_dir_all(store.root()).unwrap();
}
