//! Real publication and history worker with a large original accounted archive.
//! The decode counter measures physical TSV work, not helper invocation counts.

use super::*;
use crate::runtime::control::context::{
    RuntimeAgentHistoryEpochInputs, RuntimeAgentHistoryEpochWork,
    execute_runtime_agent_history_epoch_work,
};
use crate::storage::transcript::{TranscriptAppendBatch, replay_epoch::ReplaySourceOrigin};
use mez_agent::{
    ContextConversationAppend, ContextExecutionGroupId, ReplayCompactionPreparation,
    ReplayConversationId, ReplayEpochContext, ReplayHeadPublication, ReplayTailPolicy,
};

const CID: &str = "cursor-history";

struct Fixture {
    root: Directory,
    store: AgentTranscriptStore,
    owner: ReplayEpochContext,
    batch: TranscriptAppendBatch,
    new_source: mez_agent::ConversationEvent,
}

fn origin() -> ReplaySourceOrigin {
    ReplaySourceOrigin::new("original-turn", "original-agent", "original-pane").unwrap()
}

fn append_closed(owner: &mut ReplayEpochContext, name: &str) {
    let group = ContextExecutionGroupId::new(name).unwrap();
    let sequence = owner
        .append_execution_events([ContextConversationAppend::assistant(
            "source",
            format!("exact {name}"),
            group.clone(),
        )])
        .unwrap()[0];
    let handle = owner.capture_execution_owner(&group, sequence).unwrap();
    owner.finish_execution_group(&handle).unwrap();
}

fn plan(owner: &ReplayEpochContext) -> mez_agent::ReplayCompactionPlan {
    match owner
        .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
        .unwrap()
    {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected source, got {other:?}"),
    }
}

fn source_row(event: &mez_agent::ConversationEvent, sequence: u64) -> TranscriptEntry {
    let block = event.block();
    TranscriptEntry {
        conversation_id: CID.into(),
        sequence,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "original-turn".into(),
        agent_id: "original-agent".into(),
        pane_id: "original-pane".into(),
        content: TranscriptContextRecord::new(
            TranscriptContextEvent::execution_block_with_metadata(
                block.source,
                &block.label,
                &block.content,
                event.execution_group_id().unwrap().clone(),
                1,
                None,
            )
            .unwrap(),
            event.record(),
            None,
        )
        .unwrap()
        .to_transcript_content()
        .unwrap(),
    }
}

impl Fixture {
    fn new() -> Self {
        let root = Directory::new();
        let store = AgentTranscriptStore::new(root.0.join("transcripts"));
        let mut owner = ReplayEpochContext::new(
            ReplayConversationId::new(CID).unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        for index in 0..32 {
            append_closed(&mut owner, &format!("old-{index}"));
        }
        let mut rows = owner
            .context()
            .chronology()
            .iter()
            .enumerate()
            .map(|(index, event)| source_row(event, index as u64 + 1))
            .collect::<Vec<_>>();
        for _ in 0..256 {
            let mut row = rows[0].clone();
            row.sequence = rows.len() as u64 + 1;
            row.content = "ordinary zero-model audit".into();
            rows.push(row);
        }
        let batch = TranscriptAppendBatch::owned_rows(rows).unwrap();
        let first = store
            .prepare_replay_epoch(
                origin().capture(None),
                CID,
                None,
                &owner,
                plan(&owner),
                ReplayHeadPublication::Summary("first head".into()),
            )
            .unwrap();
        let ack = store.publish_replay_epoch(&first).unwrap();
        let mut body = owner.into_owned_context();
        ack.adopt(&store, &first, &mut body).unwrap();
        owner =
            ReplayEpochContext::from_owned_context(ReplayConversationId::new(CID).unwrap(), body)
                .unwrap();
        store.commit_accounted_transcript_batch(&batch, 1).unwrap();
        append_closed(&mut owner, "new-source");
        let new_source = owner.context().chronology().last().unwrap().clone();
        let second = store
            .prepare_replay_epoch(
                origin().capture(None),
                CID,
                Some(first.identity()),
                &owner,
                plan(&owner),
                ReplayHeadPublication::Summary("current head".into()),
            )
            .unwrap();
        let ack = store.publish_replay_epoch(&second).unwrap();
        let mut body = owner.into_owned_context();
        ack.adopt(&store, &second, &mut body).unwrap();
        owner =
            ReplayEpochContext::from_owned_context(ReplayConversationId::new(CID).unwrap(), body)
                .unwrap();
        Self {
            root,
            store,
            owner,
            batch,
            new_source,
        }
    }

    fn directory(&self) -> PathBuf {
        self.store.root().join(CID)
    }

    fn manifest(&self) -> serde_json::Value {
        serde_json::from_slice(
            &std::fs::read(self.directory().join("compaction-epoch.json")).unwrap(),
        )
        .unwrap()
    }

    fn publish(&mut self, frozen: mez_agent::ReplayCompactionPlan) {
        let (parent, _) = self
            .store
            .load_replay_epoch(CID, AgentContext::empty())
            .unwrap()
            .unwrap();
        let proposal = self
            .store
            .prepare_replay_epoch(
                origin().capture(None),
                CID,
                Some(parent),
                &self.owner,
                frozen,
                ReplayHeadPublication::Summary("third head".into()),
            )
            .unwrap();
        let ack = self.store.publish_replay_epoch(&proposal).unwrap();
        let mut body = self.owner.clone().into_owned_context();
        ack.adopt(&self.store, &proposal, &mut body).unwrap();
        self.owner =
            ReplayEpochContext::from_owned_context(ReplayConversationId::new(CID).unwrap(), body)
                .unwrap();
    }

    fn work(&self) -> RuntimeAgentHistoryEpochWork {
        RuntimeAgentHistoryEpochWork {
            pending_capture_error: None,
            store: self.store.clone(),
            inputs: RuntimeAgentHistoryEpochInputs {
                pane_id: "cold-other-pane".into(),
                conversation_id: CID.into(),
                ephemeral_source_entries: None,
                active_entries: Some(0),
                pending_entries: Default::default(),
            },
        }
    }
}

/// A durable head represents the whole indexed prefix. A cold history read and
/// lagging original pending capture must use bounded source, preserve exact head
/// identity and emit no covered raw members, without acknowledging that batch.
#[test]
fn replay_cursor_omits_large_indexed_prefix_through_actual_history_worker() {
    let fixture = Fixture::new();
    for pending in [false, true] {
        let mut work = fixture.work();
        work.store = AgentTranscriptStore::new(fixture.root.0.join("transcripts"));
        if pending {
            work.inputs.pending_entries =
                crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(
                    &fixture.batch,
                )
                .unwrap();
        }
        let before = crate::storage::transcript::transcript_decode_count();
        let history = execute_runtime_agent_history_epoch_work(work).unwrap();
        let decoded = crate::storage::transcript::transcript_decode_count() - before;
        assert!(
            decoded <= 4,
            "decoded {decoded} rows for an already published archive prefix"
        );
        assert_eq!(history.blocks.len(), 1);
        assert_eq!(
            history
                .records
                .import_context(history.blocks)
                .unwrap()
                .chronology(),
            fixture.owner.context().chronology()
        );
    }
}

/// Already complete original accounting needs no replay repair. History leaves
/// the original receipt with its actual owner instead of inventing an ACK from
/// the cut; explicit settlement still checks the actual physical archive.
#[test]
fn replay_cursor_keeps_original_receipt_without_decoding_archived_prefix() {
    let fixture = Fixture::new();
    fixture
        .store
        .accept_transcript_append_receipt(&fixture.batch, 1)
        .unwrap();
    let receipt_input = crate::storage::transcript::transcript_decode_count();
    assert_eq!(fixture.store.pending_append_receipts().unwrap().len(), 1);
    assert_eq!(
        crate::storage::transcript::transcript_decode_count(),
        receipt_input
    );
    let path = fixture.directory().join("history.tsv");
    let original = std::fs::read(&path).unwrap();
    for cold in [false, true] {
        let mut work = fixture.work();
        if cold {
            work.store = AgentTranscriptStore::new(fixture.store.root());
        }
        let before = crate::storage::transcript::transcript_decode_count();
        let history = execute_runtime_agent_history_epoch_work(work).unwrap();
        let decoded = crate::storage::transcript::transcript_decode_count() - before;
        assert_eq!(
            history
                .records
                .import_context(history.blocks)
                .unwrap()
                .chronology(),
            fixture.owner.context().chronology()
        );
        assert_eq!(
            fixture.store.pending_append_receipts().unwrap(),
            vec![fixture.batch.entries().to_vec()]
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(
            decoded <= 4,
            "decoded {decoded} rows with a fully archived original receipt"
        );
    }
    fixture
        .store
        .settle_transcript_append_receipt(&fixture.batch, 1)
        .unwrap();
    assert!(fixture.store.pending_append_receipts().unwrap().is_empty());
}

/// A real accepted but unappended tail is outside the cut. The actual worker
/// must finish its original accounting and physical settlement before replay,
/// preserving source and avoiding a duplicate current model occurrence.
#[test]
fn replay_cursor_recovers_uncovered_original_receipt_before_history() {
    let fixture = Fixture::new();
    let batch =
        TranscriptAppendBatch::owned_rows(vec![source_row(&fixture.new_source, 289)]).unwrap();
    fixture
        .store
        .accept_transcript_append_receipt(&batch, 2)
        .unwrap();
    assert_eq!(fixture.store.pending_append_receipts().unwrap().len(), 1);
    let history = execute_runtime_agent_history_epoch_work(fixture.work()).unwrap();
    assert_eq!(
        history
            .records
            .import_context(history.blocks)
            .unwrap()
            .chronology(),
        fixture.owner.context().chronology()
    );
    assert!(fixture.store.pending_append_receipts().unwrap().is_empty());
    let physical_before = crate::storage::transcript::transcript_decode_count();
    let durable = fixture.store.inspect(CID).unwrap();
    assert_eq!(durable.len(), 289);
    assert!(
        crate::storage::transcript::transcript_decode_count() - physical_before
            >= durable.len() as u64
    );
    assert_eq!(durable.last(), batch.entries().last());
}

/// Mixed receipt domains keep existing recovery rather than qualifying an
/// unaccounted owner from a different batch's original-accounting proof.
#[test]
fn replay_cursor_mixed_receipts_keep_existing_recovery_path() {
    let fixture = Fixture::new();
    fixture
        .store
        .accept_transcript_append_receipt(&fixture.batch, 1)
        .unwrap();
    let mut row = fixture.batch.entries()[0].clone();
    row.sequence = 289;
    row.content = "unaccounted audit source".into();
    let unaccounted = TranscriptAppendBatch::rows(vec![row]).unwrap();
    fixture
        .store
        .accept_transcript_append_receipt(&unaccounted, 2)
        .unwrap();
    let history = execute_runtime_agent_history_epoch_work(fixture.work()).unwrap();
    assert_eq!(
        history
            .records
            .import_context(history.blocks)
            .unwrap()
            .chronology(),
        fixture.owner.context().chronology()
    );
    assert_eq!(
        fixture.store.pending_append_receipts().unwrap(),
        vec![unaccounted.entries().to_vec()]
    );
}

/// Bounded replay does not certify arbitrary old raw bytes or remove the only
/// original receipt. A real settlement rejects changed source, retains its
/// receipt, and succeeds only after the exact original archive is restored.
#[test]
fn replay_cursor_retained_receipt_requires_actual_source_before_ack() {
    let fixture = Fixture::new();
    fixture
        .store
        .accept_transcript_append_receipt(&fixture.batch, 1)
        .unwrap();
    let receipt = std::fs::read_dir(fixture.store.root().join(".append-receipts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let original_receipt = std::fs::read(&receipt).unwrap();
    let path = fixture.directory().join("history.tsv");
    let original = std::fs::read_to_string(&path).unwrap();
    let changed = original.replacen("exact old-0", "EXACT OLD-0", 1);
    assert_ne!(original, changed);
    std::fs::write(&path, changed).unwrap();
    let before = crate::storage::transcript::transcript_decode_count();
    let history = execute_runtime_agent_history_epoch_work(fixture.work()).unwrap();
    assert!(crate::storage::transcript::transcript_decode_count() - before <= 4);
    assert_eq!(
        history
            .records
            .import_context(history.blocks)
            .unwrap()
            .chronology(),
        fixture.owner.context().chronology()
    );
    assert_eq!(std::fs::read(&receipt).unwrap(), original_receipt);
    assert!(
        fixture
            .store
            .settle_transcript_append_receipt(&fixture.batch, 1)
            .is_err()
    );
    assert_eq!(std::fs::read(&receipt).unwrap(), original_receipt);
    std::fs::write(path, original).unwrap();
    fixture
        .store
        .settle_transcript_append_receipt(&fixture.batch, 1)
        .unwrap();
    assert!(fixture.store.pending_append_receipts().unwrap().is_empty());
}

/// A complete cut does not cover missing required source images or costs. The
/// read fails locally and leaves actual receipt recovery evidence unchanged.
#[test]
fn replay_cursor_keeps_receipt_on_required_image_or_accounting_damage() {
    for image in [false, true] {
        let fixture = Fixture::new();
        fixture
            .store
            .accept_transcript_append_receipt(&fixture.batch, 1)
            .unwrap();
        let receipt = std::fs::read_dir(fixture.store.root().join(".append-receipts"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let original_receipt = std::fs::read(&receipt).unwrap();
        let connection =
            rusqlite::Connection::open(fixture.directory().join("row-origins.sqlite3")).unwrap();
        connection
            .execute(
                if image {
                    "DELETE FROM row_digests WHERE sequence = '1'"
                } else {
                    "DELETE FROM row_costs"
                },
                [],
            )
            .unwrap();
        drop(connection);
        assert!(
            execute_runtime_agent_history_epoch_work(fixture.work()).is_err(),
            "image={image}"
        );
        assert_eq!(std::fs::read(receipt).unwrap(), original_receipt);
    }
}

/// Only an actual indexed archive cut retires old backing. The new source is
/// still unsettled, so deleting that required payload must fail explicitly.
#[test]
fn replay_cursor_retires_covered_backing_but_requires_unsettled_originals() {
    let fixture = Fixture::new();
    let manifest = fixture.manifest();
    assert_eq!(manifest["manifest"]["version"], 6);
    assert_eq!(manifest["manifest"]["archive_cut"]["through_sequence"], 288);
    assert_eq!(
        manifest["manifest"]["source_escrow"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let payloads = fixture.directory().join("epoch-payloads");
    let find = |content: &[u8]| {
        std::fs::read_dir(&payloads)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| std::fs::read(path).unwrap() == content)
            .unwrap()
    };
    std::fs::remove_file(find(b"exact old-0")).unwrap();
    let history = execute_runtime_agent_history_epoch_work(fixture.work()).unwrap();
    assert_eq!(history.blocks.len(), 1);
    std::fs::remove_file(find(b"exact new-source")).unwrap();
    assert!(execute_runtime_agent_history_epoch_work(fixture.work()).is_err());
}

/// Late ordinary settlement of the covered original is zero model input. A
/// genuinely later user preserves its original record and reserved sequence.
#[test]
fn replay_cursor_admits_tail_once_with_original_user_identity() {
    let mut fixture = Fixture::new();
    let task = fixture.owner.begin_instruction_task().unwrap();
    fixture
        .owner
        .append_instruction(
            &task,
            ContextBlock::user_event("user", "exact later intent"),
        )
        .unwrap();
    let user = fixture.owner.context().chronology().last().unwrap();
    let mut row = source_row(&fixture.new_source, 290);
    row.turn_id = "later-turn".into();
    row.content = TranscriptContextRecord::new(
        TranscriptContextEvent::user_event(
            user.sequence().get(),
            &user.block().label,
            &user.block().content,
        )
        .unwrap(),
        user.record(),
        None,
    )
    .unwrap()
    .to_transcript_content()
    .unwrap();
    let batch =
        TranscriptAppendBatch::owned_rows(vec![source_row(&fixture.new_source, 289), row]).unwrap();
    fixture
        .store
        .commit_accounted_transcript_batch(&batch, 2)
        .unwrap();
    let mut work = fixture.work();
    work.inputs.pending_entries =
        crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch).unwrap();
    let before = crate::storage::transcript::transcript_decode_count();
    let history = execute_runtime_agent_history_epoch_work(work).unwrap();
    assert!(crate::storage::transcript::transcript_decode_count() - before <= 6);
    let restored = history.records.import_context(history.blocks).unwrap();
    assert_eq!(restored.chronology(), fixture.owner.context().chronology());
    assert_eq!(
        restored.chronology()[1].retention(),
        mez_agent::ContextRetention::Exact
    );
}

/// Covered pending rows need both exact physical images and the captured batch's
/// original IDs. Reconstructing identical audit text cannot manufacture them.
#[test]
fn replay_cursor_refuses_forged_or_reallocated_old_pending_source() {
    let fixture = Fixture::new();
    for changed in [false, true] {
        let mut rows = fixture.batch.entries().to_vec();
        if changed {
            rows[0].turn_id = "forged-turn".into();
        }
        let batch = TranscriptAppendBatch::owned_rows(rows).unwrap();
        let mut work = fixture.work();
        work.inputs.pending_entries =
            crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch)
                .unwrap();
        assert!(execute_runtime_agent_history_epoch_work(work).is_err());
    }
}

/// A raw import cannot resurrect an occurrence whose original indexed placement
/// was already incorporated. The absent retired payload is irrelevant to refusal.
#[test]
fn replay_cursor_refuses_retired_occurrence_reentry() {
    let fixture = Fixture::new();
    let mut row = fixture.batch.entries()[0].clone();
    row.sequence = 289;
    fixture.store.append(&row).unwrap();
    assert!(execute_runtime_agent_history_epoch_work(fixture.work()).is_err());
}

/// An unappended original pending proposal cannot move a retired occurrence
/// beyond the cut and gain a new model lifetime from its still-valid authored ID.
#[test]
fn replay_cursor_refuses_pending_retired_occurrence_reentry() {
    let fixture = Fixture::new();
    let mut row = fixture.batch.entries()[0].clone();
    row.sequence = 289;
    let batch = TranscriptAppendBatch::owned_rows(vec![row]).unwrap();
    let mut work = fixture.work();
    work.inputs.pending_entries =
        crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch).unwrap();
    assert!(execute_runtime_agent_history_epoch_work(work).is_err());
}

/// The original placement survives selective digest loss. That missing required
/// image must not turn an old occurrence into a fresh durable or pending arrival.
#[test]
fn replay_cursor_requires_interior_original_image_for_reentry_guard() {
    for pending in [false, true] {
        let fixture = Fixture::new();
        let connection =
            rusqlite::Connection::open(fixture.directory().join("row-origins.sqlite3")).unwrap();
        connection
            .execute("DELETE FROM row_digests WHERE sequence = '1'", [])
            .unwrap();
        drop(connection);
        let mut row = fixture.batch.entries()[0].clone();
        row.sequence = 289;
        let mut work = fixture.work();
        if pending {
            let batch = TranscriptAppendBatch::owned_rows(vec![row]).unwrap();
            work.inputs.pending_entries =
                crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch)
                    .unwrap();
        } else {
            fixture.store.append(&row).unwrap();
        }
        assert!(
            execute_runtime_agent_history_epoch_work(work).is_err(),
            "pending={pending}"
        );
    }
}

/// A later placement outside the cut may lack an image without claiming that
/// it is archived. Real source escrow remains required and a later publication
/// must retain that backing rather than inventing index proof or advancing.
#[test]
fn replay_cursor_keeps_unindexed_tail_outside_cut_without_inventing_proof() {
    let mut fixture = Fixture::new();
    let batch =
        TranscriptAppendBatch::owned_rows(vec![source_row(&fixture.new_source, 289)]).unwrap();
    fixture
        .store
        .commit_accounted_transcript_batch(&batch, 2)
        .unwrap();
    let connection =
        rusqlite::Connection::open(fixture.directory().join("row-origins.sqlite3")).unwrap();
    connection
        .execute("DELETE FROM row_digests WHERE sequence = '289'", [])
        .unwrap();
    drop(connection);
    let history = execute_runtime_agent_history_epoch_work(fixture.work()).unwrap();
    assert_eq!(
        history
            .records
            .import_context(history.blocks)
            .unwrap()
            .chronology(),
        fixture.owner.context().chronology()
    );
    append_closed(&mut fixture.owner, "third-source");
    let frozen = plan(&fixture.owner);
    fixture.publish(frozen);
    let manifest = fixture.manifest();
    assert_eq!(manifest["manifest"]["archive_cut"]["through_sequence"], 288);
    assert_eq!(
        manifest["manifest"]["source_escrow"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let history = execute_runtime_agent_history_epoch_work(fixture.work()).unwrap();
    assert_eq!(
        history
            .records
            .import_context(history.blocks)
            .unwrap()
            .chronology(),
        fixture.owner.context().chronology()
    );
}

/// A damaged required cut never falls back to a lifetime scan or an empty head.
#[test]
fn replay_cursor_refuses_truncation_boundary_and_required_index_damage() {
    for damage in 0..3 {
        let fixture = Fixture::new();
        let path = fixture.directory().join("history.tsv");
        match damage {
            0 => std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_len(1)
                .unwrap(),
            1 => {
                let mut bytes = std::fs::read(&path).unwrap();
                let last = bytes.len() - 2;
                bytes[last] ^= 1;
                std::fs::write(path, bytes).unwrap();
            }
            _ => {
                let connection =
                    rusqlite::Connection::open(fixture.directory().join("row-origins.sqlite3"))
                        .unwrap();
                connection
                    .execute("DELETE FROM row_digests WHERE sequence = '288'", [])
                    .unwrap();
            }
        }
        assert!(execute_runtime_agent_history_epoch_work(fixture.work()).is_err());
    }
}

/// A post-plan raw member remains exact even when its physical archive row is
/// inside the next cut. Archive progress does not choose model membership.
#[test]
fn replay_cursor_keeps_retained_raw_tail_inside_physical_cut() {
    let mut fixture = Fixture::new();
    append_closed(&mut fixture.owner, "covered-next");
    let covered = fixture.owner.context().chronology().last().unwrap().clone();
    let frozen = plan(&fixture.owner);
    append_closed(&mut fixture.owner, "retained-next");
    let retained = fixture.owner.context().chronology().last().unwrap().clone();
    let batch = TranscriptAppendBatch::owned_rows(vec![
        source_row(&fixture.new_source, 289),
        source_row(&covered, 290),
        source_row(&retained, 291),
    ])
    .unwrap();
    fixture
        .store
        .commit_accounted_transcript_batch(&batch, 2)
        .unwrap();
    fixture.publish(frozen);
    assert_eq!(
        fixture.manifest()["manifest"]["archive_cut"]["through_sequence"],
        291
    );
    let before = crate::storage::transcript::transcript_decode_count();
    let history = execute_runtime_agent_history_epoch_work(fixture.work()).unwrap();
    assert!(crate::storage::transcript::transcript_decode_count() - before <= 4);
    let restored = history.records.import_context(history.blocks).unwrap();
    assert_eq!(restored.chronology(), fixture.owner.context().chronology());
    assert_eq!(restored.chronology().last(), Some(&retained));
}

/// Ordinary reads deliberately verify only the boundary plus current backing.
/// Publishing a new cut must additionally reject altered older prefix bytes,
/// before replacing the old authority or acknowledging any new proposal.
#[test]
fn replay_cursor_rechecks_full_frozen_prefix_before_publication() {
    let mut fixture = Fixture::new();
    append_closed(&mut fixture.owner, "third-source");
    let (parent, _) = fixture
        .store
        .load_replay_epoch(CID, AgentContext::empty())
        .unwrap()
        .unwrap();
    let prepared = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(None),
            CID,
            Some(parent.clone()),
            &fixture.owner,
            plan(&fixture.owner),
            ReplayHeadPublication::Summary("must not publish".into()),
        )
        .unwrap();
    let path = fixture.directory().join("history.tsv");
    let text = std::fs::read_to_string(&path).unwrap();
    let changed = text.replacen("exact old-0", "EXACT OLD-0", 1);
    assert_ne!(changed, text);
    assert_eq!(changed.len(), text.len());
    std::fs::write(path, changed).unwrap();
    assert!(execute_runtime_agent_history_epoch_work(fixture.work()).is_ok());
    assert!(fixture.store.publish_replay_epoch(&prepared).is_err());
    assert_eq!(
        fixture
            .store
            .load_replay_epoch(CID, AgentContext::empty())
            .unwrap()
            .unwrap()
            .0,
        parent
    );
}
