//! Initial typed roots use the existing append journal through every uncertain
//! publication phase. Neither a live source copy nor recovery settles the
//! parent's ordinary source; each child receives independent original raw rows.

use super::*;
use crate::storage::transcript::{InitialTranscriptRowAccounting, TranscriptAppendBatch};
use mez_agent::transcript::TranscriptContextRecord;

fn child_batch(fixture: &Fixture, target: &str, retained: bool) -> TranscriptAppendBatch {
    let mut owner = source();
    let frozen = plan(&owner);
    if retained {
        owner
            .append_fact("tail", "original unarchived raw source")
            .unwrap();
    }
    let proposal = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(None),
            "conversation-a",
            None,
            &owner,
            frozen,
            ReplayHeadPublication::Summary("original typed head".into()),
        )
        .unwrap();
    let commit = fixture.store.publish_replay_epoch(&proposal).unwrap();
    let snapshot = commit.snapshot(&fixture.store, &proposal).unwrap();
    let (_, loaded) = fixture.load();
    let source = snapshot.fork_source(&loaded, &[]).unwrap();
    initial_batch(&source, target)
}

fn initial_batch(source: &ReplayForkSource, target: &str) -> TranscriptAppendBatch {
    let mut origins = BTreeMap::new();
    let mut records = BTreeMap::new();
    let rows = source
        .rows()
        .iter()
        .cloned()
        .map(|mut row| {
            row.conversation_id = target.into();
            let record = TranscriptContextRecord::from_transcript_content(&row.content)
                .unwrap()
                .unwrap()
                .record();
            origins.insert(row.sequence, record.occurrence_id());
            records.insert(row.sequence, record);
            row
        })
        .collect();
    TranscriptAppendBatch::initial_replay(source.for_target(target).unwrap(), rows)
        .unwrap()
        .with_initial_row_accounting(InitialTranscriptRowAccounting::new(origins, records))
        .unwrap()
}

#[test]
fn typed_initial_receipt_preserves_nonempty_raw_rows_and_head_only_roots() {
    for retained in [false, true] {
        let fixture = Fixture::new();
        let batch = child_batch(&fixture, "child", retained);
        assert_eq!(batch.entries().len(), usize::from(retained));
        let expected = batch.initial_replay_epoch().unwrap().owner().unwrap();
        fixture
            .store
            .accept_transcript_append_receipt(&batch, 1)
            .unwrap();
        fixture
            .store
            .settle_transcript_append_receipt(&batch, 1)
            .unwrap();
        fixture
            .store
            .settle_transcript_append_receipt(&batch, 1)
            .unwrap();
        assert_eq!(fixture.store.inspect("child").unwrap(), batch.entries());
        fixture.store.delete("conversation-a").unwrap();
        let (identity, actual) = fixture
            .store
            .load_replay_epoch("child", prefix())
            .unwrap()
            .unwrap();
        assert_eq!(
            identity,
            batch
                .initial_replay_epoch()
                .unwrap()
                .pending_snapshot(&fixture.store)
                .unwrap()
                .identity
        );
        assert_eq!(actual.head(), expected.head());
        assert_eq!(
            actual.context().chronology(),
            expected.context().chronology()
        );
    }
}

/// Empty physical audit history does not erase a committed typed summary.
/// Exercise the real saved-fork transaction twice and remove both ancestors.
#[test]
fn head_only_typed_source_supports_nested_saved_forks() {
    let fixture = Fixture::new();
    let batch = child_batch(&fixture, "head-only", false);
    fixture
        .store
        .accept_transcript_append_receipt(&batch, 1)
        .unwrap();
    fixture
        .store
        .settle_transcript_append_receipt(&batch, 1)
        .unwrap();
    assert!(fixture.store.inspect("head-only").unwrap().is_empty());
    let expected = batch.initial_replay_epoch().unwrap().owner().unwrap();
    assert!(
        fixture
            .store
            .fork_owned_cancellable("head-only", "unsettled-row-claim", 123, &[], 1)
            .is_err()
    );
    assert!(!fixture.root.join("unsettled-row-claim").exists());
    for (parent, child) in [("head-only", "nested-head"), ("nested-head", "last-head")] {
        fixture.store.fork(parent, child, 123).unwrap();
        assert!(fixture.store.inspect(child).unwrap().is_empty());
        let (_, actual) = fixture
            .store
            .load_replay_epoch(child, prefix())
            .unwrap()
            .unwrap();
        assert_eq!(
            actual.context().chronology(),
            expected.context().chronology()
        );
        assert_eq!(actual.head().unwrap().conversation().as_str(), child);
        fixture.store.delete(parent).unwrap();
    }
    fixture.store.delete("conversation-a").unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("last-head", prefix())
            .unwrap()
            .is_some()
    );
}

/// The oldest-unit progress rule may cover an old task's prelude while its
/// original user root remains raw. That prelude is backing, never model tail.
fn partial_task_source(fixture: &Fixture) -> (ReplayEpochSnapshot, ReplayEpochContext) {
    let owner = partial_task_owner();
    publish_partial_task_source(fixture, &owner)
}

fn publish_partial_task_source(
    fixture: &Fixture,
    owner: &ReplayEpochContext,
) -> (ReplayEpochSnapshot, ReplayEpochContext) {
    let plan = partial_task_plan(owner);
    let publication = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(None),
            "conversation-a",
            None,
            owner,
            plan,
            ReplayHeadPublication::Summary("only the old prelude was summarized".into()),
        )
        .unwrap();
    let commit = fixture.store.publish_replay_epoch(&publication).unwrap();
    (
        commit.snapshot(&fixture.store, &publication).unwrap(),
        publication.prepared.candidate().clone(),
    )
}

fn partial_task_owner() -> ReplayEpochContext {
    let mut owner = ReplayEpochContext::new(
        ReplayConversationId::new("conversation-a").unwrap(),
        prefix(),
    )
    .unwrap();
    owner
        .admit_instruction_task(
            vec![
                ContextBlock::user_event("old task prelude", "covered original task boundary"),
                ContextBlock::user_event("user prompt", "retained original user root"),
            ],
            &[],
            &[],
            1,
        )
        .unwrap();
    owner
        .admit_instruction_task(
            vec![ContextBlock::user_event(
                "user prompt",
                "current exact task",
            )],
            &[],
            &[],
            0,
        )
        .unwrap();
    owner
}

fn partial_task_plan(owner: &ReplayEpochContext) -> ReplayCompactionPlan {
    let plan = match owner
        .prepare_compaction(ReplayTailPolicy::new(100_000, 100).unwrap())
        .unwrap()
    {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected original oldest-unit progress: {other:?}"),
    };
    assert_eq!(plan.summary_input().len(), 1);
    plan
}

fn assert_partial_task_child(
    fixture: &Fixture,
    source: &ReplayForkSource,
    owner: &ReplayEpochContext,
) {
    let batch = initial_batch(source, "partial-task-child");
    assert_eq!(batch.entries().len(), 2);
    assert!(
        !batch
            .entries()
            .iter()
            .any(|row| row.content.contains("covered original task boundary"))
    );
    fixture
        .store
        .accept_transcript_append_receipt(&batch, 1)
        .unwrap();
    fixture
        .store
        .settle_transcript_append_receipt(&batch, 1)
        .unwrap();
    fixture.store.delete("conversation-a").unwrap();
    let (_, cold) = fixture
        .store
        .load_replay_epoch("partial-task-child", prefix())
        .unwrap()
        .unwrap();
    assert_eq!(cold.context().chronology(), owner.context().chronology());
    let value = serde_json::to_value(batch.initial_replay_epoch().unwrap()).unwrap();
    let boundary = value["payloads"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, content)| content.as_str() == Some("covered original task boundary"))
        .unwrap()
        .0;
    fs::remove_file(
        fixture
            .root
            .join("partial-task-child/epoch-payloads")
            .join(boundary),
    )
    .unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("partial-task-child", prefix())
            .is_err()
    );
}

#[test]
fn durable_copy_preserves_source_only_task_boundary() {
    let fixture = Fixture::new();
    let (snapshot, owner) = partial_task_source(&fixture);
    let source = snapshot.fork_source(&owner, &[]).unwrap();
    assert_partial_task_child(&fixture, &source, &owner);
}

#[test]
fn memory_copy_preserves_source_only_task_boundary() {
    let fixture = Fixture::new();
    let (snapshot, owner) = partial_task_source(&fixture);
    let capture = ReplayMemorySource::capture(&owner, None, Some(&snapshot), None, &[]).unwrap();
    let source = capture.fork_source(&owner).unwrap();
    assert_partial_task_child(&fixture, &source, &owner);
}

/// A genuinely unsaved rewrite has no store from which to recover the removed
/// prelude. The before-rewrite memory capture owns those original bytes.
#[test]
fn memory_copy_preserves_unsaved_source_only_task_boundary() {
    let fixture = Fixture::new();
    let owner = partial_task_owner();
    let original = origin();
    let before =
        ReplayMemorySource::capture(&owner, None, None, Some((&original, 0)), &[]).unwrap();
    let prepared = owner
        .prepare_compaction_publication(
            partial_task_plan(&owner),
            ReplayHeadPublication::Summary("memory-only partial task head".into()),
        )
        .unwrap();
    let owner = prepared.candidate();
    let after =
        ReplayMemorySource::capture(owner, Some(&before), None, Some((&original, 0)), &[]).unwrap();
    let batch = initial_batch(&after.fork_source(owner).unwrap(), "unsaved-partial-child");
    assert!(!fixture.root.join("conversation-a").exists());
    assert_eq!(batch.entries().len(), 2);
    assert!(
        !batch
            .entries()
            .iter()
            .any(|row| row.content.contains("covered original task boundary"))
    );
    fixture
        .store
        .accept_transcript_append_receipt(&batch, 1)
        .unwrap();
    fixture
        .store
        .settle_transcript_append_receipt(&batch, 1)
        .unwrap();
    let (_, cold) = fixture
        .store
        .load_replay_epoch("unsaved-partial-child", prefix())
        .unwrap()
        .unwrap();
    assert_eq!(cold.context().chronology(), owner.context().chronology());
    assert!(!fixture.root.join("conversation-a").exists());
}

/// A required boundary may already be indexed behind the cut. Saved forks must
/// copy its source-only bytes as well as the current checkpoint and audit rows.
#[test]
fn saved_copy_preserves_archived_source_only_task_boundary() {
    let fixture = Fixture::new();
    let owner = partial_task_owner();
    let items = escrow::extend(&[], &owner, &origin(), &[]).unwrap();
    let rows = items
        .iter()
        .zip(owner.context().chronology())
        .enumerate()
        .map(|(index, (item, event))| {
            let mut row = item
                .copy_row(event.block().content.clone(), "conversation-a")
                .unwrap();
            row.sequence = index as u64 + 1;
            row
        })
        .collect();
    let batch = TranscriptAppendBatch::owned_rows(rows).unwrap();
    fixture
        .store
        .commit_accounted_transcript_batch(&batch, 1)
        .unwrap();
    let (snapshot, expected) = publish_partial_task_source(&fixture, &owner);
    assert_eq!(snapshot.covered_archive_frontier(), 3);
    fixture
        .store
        .fork("conversation-a", "archived-boundary-child", 123)
        .unwrap();
    fixture.store.delete("conversation-a").unwrap();
    let (_, cold) = fixture
        .store
        .load_replay_epoch("archived-boundary-child", prefix())
        .unwrap()
        .unwrap();
    assert_eq!(cold.context().chronology(), expected.context().chronology());
    assert!(
        !cold
            .context()
            .chronology()
            .iter()
            .any(|event| event.block().content == "covered original task boundary")
    );
    fixture
        .store
        .fork(
            "archived-boundary-child",
            "archived-boundary-grandchild",
            456,
        )
        .unwrap();
    fixture.store.delete("archived-boundary-child").unwrap();
    assert_eq!(
        fixture
            .store
            .load_replay_epoch("archived-boundary-grandchild", prefix())
            .unwrap()
            .unwrap()
            .1
            .context()
            .chronology(),
        expected.context().chronology()
    );
}

#[test]
fn typed_initial_receipt_recovers_exact_root_after_each_publication_fault() {
    for phase in [
        Phase::BeforePayloadSync,
        Phase::BeforeSourceReceiptSync,
        Phase::BeforeManifestSwap,
        Phase::AfterManifestSwap,
        Phase::BeforeRequiredMarker,
        Phase::BeforeAcknowledgment,
    ] {
        let fixture = Fixture::new();
        let batch = child_batch(&fixture, "child", true);
        let parent = fs::read(fixture.root.join("conversation-a").join(MANIFEST_NAME)).unwrap();
        fixture.store.inject_replay_epoch_fault(phase, 1, false);
        assert!(
            fixture
                .store
                .accept_transcript_append_receipt(&batch, 1)
                .is_err(),
            "{phase:?}"
        );
        assert!(
            fixture
                .store
                .has_unsettled_append_receipts("child")
                .unwrap()
        );
        assert_eq!(
            fs::read(fixture.root.join("conversation-a").join(MANIFEST_NAME)).unwrap(),
            parent
        );
        // A new store has no live fixture owner: the actual retained initial
        // receipt must recover its publication and preserve its original ID.
        let reopened = AgentTranscriptStore::new(fixture.root.clone());
        reopened.recover_append_receipts().unwrap();
        assert!(!reopened.has_unsettled_append_receipts("child").unwrap());
        let (identity, _) = reopened
            .load_replay_epoch("child", prefix())
            .unwrap()
            .unwrap();
        assert_eq!(
            identity,
            batch
                .initial_replay_epoch()
                .unwrap()
                .pending_snapshot(&reopened)
                .unwrap()
                .identity
        );
        assert_eq!(reopened.inspect("child").unwrap(), batch.entries());
    }
}

#[test]
fn typed_initial_receipt_refuses_changed_rows_and_unrelated_root_adoption() {
    let fixture = Fixture::new();
    let batch = child_batch(&fixture, "child", true);
    let mut rows = batch.entries().to_vec();
    rows[0].turn_id = "different-original-turn".into();
    assert!(
        TranscriptAppendBatch::initial_replay(batch.initial_replay_epoch().unwrap().clone(), rows)
            .is_err()
    );
    assert!(
        TranscriptAppendBatch::initial_replay(
            batch.initial_replay_epoch().unwrap().clone(),
            Vec::new()
        )
        .is_err()
    );
    let unowned = TranscriptAppendBatch::rows(batch.entries().to_vec()).unwrap();
    fixture
        .store
        .accept_transcript_append_receipt(&unowned, 9)
        .unwrap();
    // Ordinary admission journals only. Its actual writer must append before
    // settlement; those legacy rows still cannot grant typed initial ownership.
    fixture.store.append_many(batch.entries()).unwrap();
    fixture
        .store
        .settle_transcript_append_receipt(&unowned, 9)
        .unwrap();
    assert!(
        fixture
            .store
            .accept_transcript_append_receipt(&batch, 1)
            .is_err()
    );
    assert!(
        fixture
            .store
            .load_replay_epoch("child", prefix())
            .unwrap()
            .is_none()
    );
}

/// Ordinary cold replay must repair the exact retained initial owner itself;
/// requiring an explicit global restart scan would expose partial raw history.
#[test]
fn typed_initial_receipt_cold_read_recovers_the_exact_initial_owner() {
    for phase in [
        Phase::BeforePayloadSync,
        Phase::BeforeSourceReceiptSync,
        Phase::BeforeManifestSwap,
        Phase::AfterManifestSwap,
        Phase::BeforeRequiredMarker,
        Phase::BeforeAcknowledgment,
    ] {
        let fixture = Fixture::new();
        let batch = child_batch(&fixture, "child", true);
        fixture.store.inject_replay_epoch_fault(phase, 1, false);
        assert!(
            fixture
                .store
                .accept_transcript_append_receipt(&batch, 1)
                .is_err()
        );
        let reopened = AgentTranscriptStore::new(fixture.root.clone());
        let snapshot = reopened
            .conversation_replay_snapshot("child", &[], |_| {
                Some((
                    crate::storage::transcript::ConversationTranscriptRead::All,
                    true,
                ))
            })
            .unwrap();
        let replay = snapshot
            .replay
            .expect("cold read must recover the actual typed head");
        assert_eq!(
            replay.identity,
            batch
                .initial_replay_epoch()
                .unwrap()
                .pending_snapshot(&reopened)
                .unwrap()
                .identity
        );
        assert!(!reopened.has_unsettled_append_receipts("child").unwrap());
    }
}

/// Losing a required initial journal must not turn partial copied raw rows
/// into an apparently complete conversation with its summary head omitted.
#[test]
fn typed_initial_receipt_missing_owner_refuses_partial_raw_history() {
    let fixture = Fixture::new();
    let batch = child_batch(&fixture, "child", true);
    fixture
        .store
        .inject_replay_epoch_fault(Phase::BeforePayloadSync, 1, false);
    assert!(
        fixture
            .store
            .accept_transcript_append_receipt(&batch, 1)
            .is_err()
    );
    fs::remove_file(fixture.root.join(".append-receipts").join(format!(
        "child-{:020}-{:020}.json",
        batch.first_sequence(),
        1
    )))
    .unwrap();
    let reopened = AgentTranscriptStore::new(fixture.root.clone());
    assert!(
        reopened
            .conversation_replay_snapshot("child", &[], |_| {
                Some((
                    crate::storage::transcript::ConversationTranscriptRead::All,
                    true,
                ))
            })
            .is_err(),
        "missing original initial authority must not expose raw-only replay"
    );
}

/// The initial owner is an actual child append, so its committed rows must
/// supply images for later bounded replay without borrowing a parent ACK.
#[test]
fn typed_initial_receipt_indexes_raw_source_for_the_next_epoch() {
    let fixture = Fixture::new();
    let batch = child_batch(&fixture, "child", true);
    fixture
        .store
        .accept_transcript_append_receipt(&batch, 1)
        .unwrap();
    fixture
        .store
        .settle_transcript_append_receipt(&batch, 1)
        .unwrap();
    let snapshot = fixture
        .store
        .conversation_replay_snapshot("child", &[], |_| {
            Some((
                crate::storage::transcript::ConversationTranscriptRead::All,
                true,
            ))
        })
        .unwrap()
        .replay
        .unwrap();
    let (_, owner) = fixture
        .store
        .load_replay_epoch("child", prefix())
        .unwrap()
        .unwrap();
    let next = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(Some(&snapshot)),
            "child",
            Some(snapshot.identity().clone()),
            &owner,
            plan(&owner),
            ReplayHeadPublication::Summary("next child head".into()),
        )
        .unwrap();
    let committed = fixture.store.publish_replay_epoch(&next).unwrap();
    assert_eq!(
        committed
            .snapshot(&fixture.store, &next)
            .unwrap()
            .covered_archive_frontier(),
        batch.entries().len() as u64,
        "actual initial rows must advance the physical archive cut"
    );
}
