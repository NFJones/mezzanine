//! Real user-facing content CAS, physical row accounting and cold recovery.
//! Saved-fork setup supplies original archived raw occurrences through the
//! actual fork/append owners rather than manufacturing an index or append ACK.

use super::*;
use crate::storage::transcript::CompareAndSwapTranscriptEntryResult as Cas;
use mez_agent::transcript::{TranscriptContextEvent as Event, TranscriptContextRecord as Record};

pub(super) const CID: &str = "editable-child";

pub(super) fn saved_raw() -> (Fixture, mez_agent::TranscriptEntry) {
    let fixture = Fixture::new();
    let mut owner = source();
    let frozen = plan(&owner);
    owner
        .append_fact(
            "cut prefix",
            "original prefix to summarize before the editable tail",
        )
        .unwrap();
    owner
        .append_fact("tail", "original editable raw bytes")
        .unwrap();
    owner
        .append_fact("other", "original unrelated raw bytes")
        .unwrap();
    archive_retained_source(&fixture, &owner, frozen);
    let rows = fixture.store.inspect(CID).unwrap();
    assert_eq!(
        rows.len(),
        3,
        "actual initial append must archive the old prefix and both retained rows"
    );
    // Initial append settlement preserves its root's empty cut. Advance it
    // through a real model publication covering only the oldest raw fact,
    // leaving both editable facts INSIDE a committed physical cut.
    let (identity, owner) = load(&fixture);
    let frozen = match owner
        .prepare_compaction(ReplayTailPolicy::new(100_000, 100).unwrap())
        .unwrap()
    {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected oldest-unit progress, got {other:?}"),
    };
    assert_eq!(frozen.summary_input().len(), 2); // Prior head and one raw fact.
    assert_eq!(frozen.retained_tail().len(), 2);
    let snapshot = {
        let _lease = fixture.store.acquire_conversation_lock(CID).unwrap();
        fixture
            .store
            .replay_epoch_snapshot_locked(CID)
            .unwrap()
            .unwrap()
    };
    let proposal = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(Some(&snapshot)),
            CID,
            Some(identity),
            &owner,
            frozen,
            ReplayHeadPublication::Summary("head with an actual archived cut".into()),
        )
        .unwrap();
    let commit = fixture.store.publish_replay_epoch(&proposal).unwrap();
    assert_eq!(
        commit
            .snapshot(&fixture.store, &proposal)
            .unwrap()
            .covered_archive_frontier(),
        3
    );
    assert_eq!(load(&fixture).1.members().len(), 2);
    (fixture, rows[1].clone())
}

/// The initial fork owner actually appends its independent raw rows and
/// accounting. A saved copy of a memory-backed parent alone is not that ACK.
pub(super) fn archive_retained_source(
    fixture: &Fixture,
    owner: &ReplayEpochContext,
    frozen: ReplayCompactionPlan,
) {
    let proposal = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(None),
            "conversation-a",
            None,
            owner,
            frozen,
            ReplayHeadPublication::Summary("original model head".into()),
        )
        .unwrap();
    let commit = fixture.store.publish_replay_epoch(&proposal).unwrap();
    let snapshot = commit.snapshot(&fixture.store, &proposal).unwrap();
    let (_, loaded) = fixture.load();
    let source = snapshot.fork_source(&loaded, &[]).unwrap();
    let batch = source.initial_batch(CID, 100).unwrap();
    fixture
        .store
        .accept_transcript_append_receipt(&batch, 1)
        .unwrap();
    fixture
        .store
        .settle_transcript_append_receipt(&batch, 1)
        .unwrap();
}

fn proposed(row: &mez_agent::TranscriptEntry, content: &str) -> String {
    let record = Record::from_transcript_content(&row.content)
        .unwrap()
        .unwrap();
    let Event::PromptBoundary {
        source,
        event_sequence: Some(sequence),
        label,
        ..
    } = record.event()
    else {
        panic!("expected original fact");
    };
    let event =
        Event::prompt_boundary_with_event_sequence(*source, *sequence, label, content).unwrap();
    // An editor need not calculate cached costs. CAS must normalize them from
    // the exact new semantic bytes without changing the original occurrence.
    Record::new(
        event,
        record.record(),
        record.stable_records().map(<[_]>::to_vec),
    )
    .unwrap()
    .to_transcript_content()
    .unwrap()
}

pub(super) fn load(fixture: &Fixture) -> (ReplayEpochIdentity, ReplayEpochContext) {
    fixture
        .store
        .load_replay_epoch(CID, prefix())
        .unwrap()
        .unwrap()
}

pub(super) fn save(
    fixture: &Fixture,
    row: &mez_agent::TranscriptEntry,
    content: &str,
) -> crate::Result<Cas> {
    fixture.store.compare_and_swap_entry_content(
        CID,
        row.sequence,
        &row.pane_id,
        &fixture.store.transcript_entry_revision(row).unwrap(),
        proposed(row, content),
    )
}

/// Actual later arrivals retain their original IDs/accounting beyond the old
/// physical cut. The source receipt must settle before any edit, and the normal
/// bounded replay reader must still observe each edited arrival exactly once.
#[test]
fn raw_edit_keeps_later_arrivals_visible_and_preserves_pending_receipt_fence() {
    let (fixture, row) = saved_raw();
    let (before, mut owner) = load(&fixture);
    owner
        .append_fact("arrival", "original later raw source")
        .unwrap();
    let event = owner.context().chronology().last().unwrap();
    let semantic = Event::prompt_boundary_with_event_sequence(
        event.block().source,
        event.sequence().get(),
        event.block().label.clone(),
        event.block().content.clone(),
    )
    .unwrap();
    let mut arrival = row.clone();
    arrival.sequence = 4;
    arrival.content = Record::new(semantic, event.record(), None)
        .unwrap()
        .to_transcript_content()
        .unwrap();
    let mut audit = row.clone();
    audit.sequence = 5;
    audit.content = "ordinary non-model audit metadata".into();
    let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![
        arrival.clone(),
        audit.clone(),
    ])
    .unwrap();
    fixture
        .store
        .accept_transcript_append_receipt(&batch, 1)
        .unwrap();
    assert!(
        save(&fixture, &row, "cannot race the original append")
            .unwrap_err()
            .to_string()
            .contains("requires append receipts to settle")
    );
    assert_eq!(load(&fixture).0, before);
    assert_eq!(fixture.store.inspect(CID).unwrap().len(), 3);
    fixture
        .store
        .commit_accounted_transcript_batch(&batch, 1)
        .unwrap();
    let Cas::Updated(updated) = save(&fixture, &arrival, "revised later raw source 界").unwrap()
    else {
        panic!("expected arrival save");
    };
    assert!(matches!(
        fixture
            .store
            .compare_and_swap_entry_content(
                CID,
                audit.sequence,
                &audit.pane_id,
                &fixture.store.transcript_entry_revision(&audit).unwrap(),
                String::new()
            )
            .unwrap(),
        Cas::Updated(_)
    ));
    let snapshot = fixture
        .store
        .conversation_replay_snapshot(CID, &[], |_| {
            Some((
                crate::storage::transcript::ConversationTranscriptRead::All,
                true,
            ))
        })
        .unwrap();
    let replay = snapshot.replay.as_ref().unwrap();
    assert_eq!(replay.covered_archive_frontier(), 3);
    assert!(replay.row_is_authored_arrival(arrival.sequence));
    assert_eq!(
        snapshot
            .view
            .logical
            .iter()
            .filter(|entry| entry.sequence == arrival.sequence)
            .count(),
        1
    );
    assert_eq!(
        snapshot
            .view
            .logical
            .iter()
            .find(|entry| entry.sequence == arrival.sequence),
        Some(&updated)
    );
    assert_eq!(
        snapshot.row_origins.get(&arrival.sequence),
        Some(&event.record().occurrence_id())
    );
    let record = Record::from_transcript_content(&updated.content)
        .unwrap()
        .unwrap()
        .record();
    assert_eq!(snapshot.row_records.get(&arrival.sequence), Some(&record));
    assert_eq!(load(&fixture).0.epoch_revision, before.epoch_revision);
    assert_eq!(load(&fixture).0.revision, before.revision + 2);
}

/// Identical saves leave the storage parent untouched; validly encoded framing
/// and identity substitutions refuse before modifying raw source or authority.
#[test]
fn raw_edit_noop_and_invalid_semantic_changes_leave_source_unchanged() {
    let (fixture, row) = saved_raw();
    let identity = load(&fixture).0;
    assert!(matches!(
        fixture
            .store
            .compare_and_swap_entry_content(
                CID,
                row.sequence,
                &row.pane_id,
                &fixture.store.transcript_entry_revision(&row).unwrap(),
                row.content.clone()
            )
            .unwrap(),
        Cas::Updated(_)
    ));
    assert_eq!(load(&fixture).0, identity);
    let original = Record::from_transcript_content(&row.content)
        .unwrap()
        .unwrap();
    let mut incorrect = load(&fixture).1.context().chronology()[1].block().clone();
    incorrect.content = "different bytes used only for a stale cost".repeat(20);
    let stale = Record::new(
        original.event().clone(),
        original.record().revised(&incorrect).unwrap(),
        original.stable_records().map(<[_]>::to_vec),
    )
    .unwrap()
    .to_transcript_content()
    .unwrap();
    let outcome = fixture
        .store
        .compare_and_swap_entry_content(
            CID,
            row.sequence,
            &row.pane_id,
            &fixture.store.transcript_entry_revision(&row).unwrap(),
            stale,
        )
        .unwrap();
    assert!(matches!(outcome, Cas::Updated(updated) if updated == row));
    assert_eq!(load(&fixture).0, identity);
    assert!(
        !fixture
            .root
            .join(CID)
            .join(".raw-content-edit.tsv")
            .exists()
    );
    let Event::PromptBoundary {
        source,
        event_sequence: Some(sequence),
        content,
        ..
    } = original.event()
    else {
        panic!("expected fact");
    };
    let changed = Event::prompt_boundary_with_event_sequence(
        *source,
        *sequence,
        "different framing",
        content,
    )
    .unwrap();
    let framing = Record::new(changed, original.record(), None)
        .unwrap()
        .to_transcript_content()
        .unwrap();
    let other = fixture.store.inspect(CID).unwrap().last().unwrap().clone();
    let other_record = Record::from_transcript_content(&other.content)
        .unwrap()
        .unwrap()
        .record();
    let identity_change = Record::new(original.event().clone(), other_record, None)
        .unwrap()
        .to_transcript_content()
        .unwrap();
    let manifest = fs::read(fixture.root.join(CID).join(MANIFEST_NAME)).unwrap();
    let raw_path = fixture.store.existing_transcript_path_for(CID).unwrap();
    let raw = fs::read(&raw_path).unwrap();
    for content in [framing, identity_change] {
        assert!(
            fixture
                .store
                .compare_and_swap_entry_content(
                    CID,
                    row.sequence,
                    &row.pane_id,
                    &fixture.store.transcript_entry_revision(&row).unwrap(),
                    content
                )
                .is_err()
        );
        assert_eq!(
            fs::read(fixture.root.join(CID).join(MANIFEST_NAME)).unwrap(),
            manifest
        );
        assert_eq!(fs::read(&raw_path).unwrap(), raw);
        assert_eq!(load(&fixture).0, identity);
    }
}

/// A retained row INSIDE the physical cut remains editable. The same head,
/// epoch, occurrence and other event survive; only selected content/cost, full
/// row fingerprint, cut and storage revision change. Repeat saves keep one base.
#[test]
fn raw_edit_updates_archive_accounting_and_cold_head_without_new_epoch() {
    let (fixture, original) = saved_raw();
    let (old_identity, old) = load(&fixture);
    let mut expected_rows = fixture.store.inspect(CID).unwrap();
    let Cas::Updated(updated) = save(
        &fixture,
        &original,
        "longer revised UTF-8 界 source".repeat(20).as_str(),
    )
    .unwrap() else {
        panic!("expected save");
    };
    let (identity, current) = load(&fixture);
    assert_eq!(identity.epoch_revision, old_identity.epoch_revision);
    assert_eq!(identity.revision, old_identity.revision + 1);
    assert_eq!(current.head(), old.head());
    assert_eq!(current.members(), old.members());
    assert_eq!(
        current.context().chronology()[0],
        old.context().chronology()[0]
    );
    assert_eq!(
        current.context().chronology()[2],
        old.context().chronology()[2]
    );
    expected_rows[original.sequence as usize - 1] = updated.clone();
    assert_eq!(fixture.store.inspect(CID).unwrap(), expected_rows);
    let record = Record::from_transcript_content(&updated.content)
        .unwrap()
        .unwrap()
        .record();
    assert_eq!(record, current.context().chronology()[1].record());
    let origin = record.occurrence_id();
    {
        let _lease = fixture.store.acquire_conversation_lock(CID).unwrap();
        let image = fixture
            .store
            .indexed_row_reference_locked(CID, original.sequence)
            .unwrap()
            .unwrap();
        assert_eq!(image.origin, origin);
        assert_eq!(
            image.digest,
            fixture.store.transcript_entry_revision(&updated).unwrap()
        );
        let costs = fixture
            .store
            .transcript_row_records_locked(CID, &BTreeMap::from([(original.sequence, origin)]))
            .unwrap();
        assert_eq!(costs.get(&original.sequence), Some(&record));
    }
    let first = fs::read(fixture.root.join(CID).join(MANIFEST_NAME)).unwrap();
    let first: Envelope = serde_json::from_slice(&first).unwrap();
    let Cas::Updated(_) = save(&fixture, &updated, "second exact save").unwrap() else {
        panic!("expected second save");
    };
    let second: Envelope =
        serde_json::from_slice(&fs::read(fixture.root.join(CID).join(MANIFEST_NAME)).unwrap())
            .unwrap();
    assert_eq!(
        second.manifest.raw_revision_origin,
        first.manifest.raw_revision_origin
    );
    assert_eq!(load(&fixture).0.revision, old_identity.revision + 2);
    assert_eq!(
        load(&fixture).1.context().chronology()[1].block().content,
        "second exact save"
    );
}

/// Every real durability seam preserves the old state before intent, or
/// recovers the exact new result after intent. Retry with an obsolete row CAS
/// never allocates another head/publication or overwrites a newer edit.
#[test]
fn raw_edit_recovers_exact_result_at_each_durability_boundary() {
    for phase in [
        Phase::BeforePayloadSync,
        Phase::BeforeSourceReceiptSync,
        Phase::BeforeRawEditIntent,
        Phase::AfterRawEditIntent,
        Phase::AfterRawEditArchive,
        Phase::AfterRawEditIndex,
        Phase::AfterRawEditManifest,
        Phase::BeforeRawEditAcknowledgment,
    ] {
        let (fixture, row) = saved_raw();
        let (old_identity, old) = load(&fixture);
        fixture.store.inject_replay_epoch_fault(phase, 1, false);
        assert!(
            save(&fixture, &row, "recovered exact edited source").is_err(),
            "{phase:?}"
        );
        if phase == Phase::AfterRawEditIntent {
            fixture.store.fork(CID, "fork-recovered-edit", 123).unwrap();
            let (_, copied) = fixture
                .store
                .load_replay_epoch("fork-recovered-edit", prefix())
                .unwrap()
                .unwrap();
            assert_eq!(
                copied.context().chronology()[1].block().content,
                "recovered exact edited source"
            );
        }
        let (identity, current) = load(&fixture);
        let before_intent = matches!(
            phase,
            Phase::BeforePayloadSync | Phase::BeforeSourceReceiptSync | Phase::BeforeRawEditIntent
        );
        if before_intent {
            assert_eq!(identity, old_identity);
            assert_eq!(current.context().chronology(), old.context().chronology());
            assert!(matches!(
                save(&fixture, &row, "recovered exact edited source").unwrap(),
                Cas::Updated(_)
            ));
        } else {
            assert_eq!(identity.revision, old_identity.revision + 1);
            assert_eq!(
                current.context().chronology()[1].block().content,
                "recovered exact edited source"
            );
            assert!(matches!(
                save(&fixture, &row, "recovered exact edited source").unwrap(),
                Cas::Stale { .. }
            ));
            assert_eq!(load(&fixture).0, identity);
        }
        let (identity, current) = load(&fixture);
        assert_eq!(identity.epoch_revision, old_identity.epoch_revision);
        assert_eq!(current.head(), old.head());
        assert_eq!(
            current.context().chronology()[0],
            old.context().chronology()[0]
        );
        assert_eq!(
            current.context().chronology()[2],
            old.context().chronology()[2]
        );
        assert!(
            !fixture
                .root
                .join(CID)
                .join(".raw-content-edit.json")
                .exists()
        );
    }
}

/// A damaged staged or active archive cannot be recertified from the manifest.
/// Keep the exact intent and recover only after the original image is restored.
#[test]
fn raw_edit_recovery_retains_intent_for_unknown_archive_images() {
    for staged in [false, true] {
        let (fixture, row) = saved_raw();
        fixture
            .store
            .inject_replay_epoch_fault(Phase::AfterRawEditIntent, 1, false);
        assert!(save(&fixture, &row, "recover only original staged bytes").is_err());
        let directory = fixture.root.join(CID);
        let path = if staged {
            directory.join(".raw-content-edit.tsv")
        } else {
            fixture.store.existing_transcript_path_for(CID).unwrap()
        };
        let bytes = fs::read(&path).unwrap();
        let intent = fs::read(directory.join(".raw-content-edit.json")).unwrap();
        fs::write(&path, b"substituted unrelated archive\n").unwrap();
        assert!(fixture.store.load_replay_epoch(CID, prefix()).is_err());
        assert_eq!(
            fs::read(directory.join(".raw-content-edit.json")).unwrap(),
            intent
        );
        fs::write(path, bytes).unwrap();
        assert_eq!(
            load(&fixture).1.context().chronology()[1].block().content,
            "recover only original staged bytes"
        );
    }
}

/// Covered protection and sequence deletion use the typed owner, while valid
/// pane/revision failures leave editable source unchanged. A genuine subsequent
/// compaction reads the revised row/index/cut without a legacy decoder fallback.
#[test]
fn raw_edit_preserves_cas_and_protects_source_after_next_compaction() {
    let (fixture, row) = saved_raw();
    let before = load(&fixture).0;
    let revision = fixture.store.transcript_entry_revision(&row).unwrap();
    assert!(matches!(
        fixture
            .store
            .compare_and_swap_entry_content(
                CID,
                row.sequence,
                "wrong-pane",
                &revision,
                proposed(&row, "unused")
            )
            .unwrap(),
        Cas::WrongPane
    ));
    assert!(matches!(
        fixture
            .store
            .compare_and_swap_entry_content(
                CID,
                row.sequence,
                &row.pane_id,
                &"0".repeat(64),
                proposed(&row, "unused")
            )
            .unwrap(),
        Cas::Stale { .. }
    ));
    assert_eq!(load(&fixture).0, before);
    let Cas::Updated(row) = save(&fixture, &row, "edited before next model rewrite").unwrap()
    else {
        panic!("expected edit");
    };
    let (identity, owner) = load(&fixture);
    let snapshot = {
        let _lease = fixture.store.acquire_conversation_lock(CID).unwrap();
        fixture
            .store
            .replay_epoch_snapshot_locked(CID)
            .unwrap()
            .unwrap()
    };
    let next = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(Some(&snapshot)),
            CID,
            Some(identity),
            &owner,
            plan(&owner),
            ReplayHeadPublication::Summary("next model-authored head".into()),
        )
        .unwrap();
    fixture.store.publish_replay_epoch(&next).unwrap();
    let path = fixture.root.join(CID).join(MANIFEST_NAME);
    let manifest = fs::read(&path).unwrap();
    let raw = fs::read(fixture.store.existing_transcript_path_for(CID).unwrap()).unwrap();
    let error = save(&fixture, &row, "covered source cannot change").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("already represented by a compaction epoch"),
        "{error}"
    );
    assert!(
        fixture
            .store
            .delete_entry(CID, 1)
            .unwrap_err()
            .to_string()
            .contains("cannot renumber")
    );
    assert_eq!(fs::read(path).unwrap(), manifest);
    assert_eq!(
        fs::read(fixture.store.existing_transcript_path_for(CID).unwrap()).unwrap(),
        raw
    );
}
