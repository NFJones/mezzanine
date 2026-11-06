//! Real receipt settlement, legacy migration and source-mutation boundaries.
//! Fingerprint lookup grants no cursor, model membership or ordinary append ACK.

use super::*;
use crate::storage::transcript::{TranscriptAppendBatch, TranscriptRowOrigins};

const CID: &str = "row-image-owner";

struct Fixture {
    root: PathBuf,
    store: AgentTranscriptStore,
}
impl Fixture {
    fn new() -> Self {
        let root = temp_root("row-images");
        Self {
            store: AgentTranscriptStore::new(&root),
            root,
        }
    }
    fn batch(&self, first: u64) -> TranscriptAppendBatch {
        TranscriptAppendBatch::owned_rows(
            (first..first + 2)
                .map(|sequence| {
                    let mut row = entry(CID, sequence, TranscriptRole::Assistant);
                    row.content = "equal 雪\nrow\t\\\0".into();
                    row
                })
                .collect(),
        )
        .unwrap()
    }
    fn index(&self) -> Connection {
        Connection::open(self.root.join(CID).join("row-origins.sqlite3")).unwrap()
    }
    fn lookup(&self, rows: &[TranscriptEntry]) -> crate::Result<Option<TranscriptRowOrigins>> {
        let _lease = self.store.acquire_conversation_lock(CID)?;
        self.store.indexed_row_origins_locked(CID, rows)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Full-field equality is independent of repeated visible text and survives a
/// cold store handle. A changed role/producer/time cannot borrow an original ID.
#[test]
fn row_images_bind_every_original_field_after_actual_receipt_settlement() {
    let fixture = Fixture::new();
    let batch = fixture.batch(1);
    fixture
        .store
        .commit_accounted_transcript_batch(&batch, 1)
        .unwrap();
    let expected = batch.row_accounting().unwrap().origins().clone();
    assert_eq!(
        fixture.lookup(batch.entries()).unwrap(),
        Some(expected.clone())
    );
    let cold = AgentTranscriptStore::new(&fixture.root);
    {
        let _lease = cold.acquire_conversation_lock(CID).unwrap();
        assert_eq!(
            cold.indexed_row_origins_locked(CID, batch.entries())
                .unwrap(),
            Some(expected)
        );
        assert!(cold.initial_row_accounting_locked(CID).unwrap().is_none());
    }
    for field in 0..9 {
        let mut row = batch.entries()[0].clone();
        match field {
            0 => row.conversation_id = "foreign".into(),
            1 => row.sequence = 2,
            2 => row.created_at_unix_seconds += 1,
            3 => row.role = TranscriptRole::User,
            4 => row.turn_id = "other-turn".into(),
            5 => row.agent_id = "other-agent".into(),
            6 => row.pane_id = "other-pane".into(),
            7 => row.content.push('!'),
            _ => row.content = row.content.replace('\0', "literal null"),
        }
        assert!(fixture.lookup(&[row]).is_err(), "field={field}");
    }
    let raw = entry(CID, 3, TranscriptRole::Assistant);
    fixture.store.append(&raw).unwrap();
    assert!(fixture.lookup(&[raw]).unwrap().is_none());
    assert_eq!(
        fixture
            .index()
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        4
    );
}

/// Old persisted placement/cost/initial schemas acquire no guessed fingerprints
/// during lookup. A new real receipt migrates only its actual rows, leaving old
/// unindexed rows explicitly unavailable for a future bounded source proof.
#[test]
fn row_images_migrate_old_schemas_only_through_original_append_ownership() {
    for version in 1..=2 {
        let fixture = Fixture::new();
        let first = fixture.batch(1);
        fixture
            .store
            .commit_accounted_transcript_batch(&first, 1)
            .unwrap();
        let index = fixture.index();
        index
            .execute_batch("DROP TABLE row_digests; DROP TABLE row_digest_meta;")
            .unwrap();
        if version < 3 {
            index
                .execute_batch("DROP TABLE initial_inheritance;")
                .unwrap();
        }
        if version == 1 {
            index.execute_batch("DROP TABLE row_costs;").unwrap();
        }
        index.pragma_update(None, "user_version", version).unwrap();
        drop(index);
        let before = fs::read(fixture.root.join(CID).join("row-origins.sqlite3")).unwrap();
        assert!(fixture.lookup(first.entries()).unwrap().is_none());
        assert_eq!(
            fs::read(fixture.root.join(CID).join("row-origins.sqlite3")).unwrap(),
            before
        );
        let next = fixture.batch(3);
        fixture
            .store
            .commit_accounted_transcript_batch(&next, 2)
            .unwrap();
        assert_eq!(
            fixture.lookup(next.entries()).unwrap(),
            Some(next.row_accounting().unwrap().origins().clone())
        );
        assert!(fixture.lookup(first.entries()).unwrap().is_none());
        assert!(fixture.store.pending_append_receipts().unwrap().is_empty());
    }
    // Version 3 owns a real initial marker; schema migration must preserve its
    // requiredness instead of treating a subsequently missing marker as absence.
    let fixture = Fixture::new();
    let original = fixture.batch(1);
    let initial = TranscriptAppendBatch::rows(original.entries().to_vec())
        .unwrap()
        .with_initial_row_accounting(
            crate::storage::transcript::InitialTranscriptRowAccounting::new(
                original.row_accounting().unwrap().origins().clone(),
                BTreeMap::new(),
            ),
        )
        .unwrap();
    fixture
        .store
        .accept_transcript_append_receipt(&initial, 1)
        .unwrap();
    fixture
        .store
        .settle_transcript_append_receipt(&initial, 1)
        .unwrap();
    assert_eq!(
        fixture
            .index()
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        3
    );
    assert!(fixture.lookup(initial.entries()).unwrap().is_none());
    fixture
        .store
        .commit_accounted_transcript_batch(&fixture.batch(3), 2)
        .unwrap();
    {
        let _lease = fixture.store.acquire_conversation_lock(CID).unwrap();
        assert_eq!(
            fixture
                .store
                .initial_row_accounting_locked(CID)
                .unwrap()
                .as_ref(),
            initial.initial_row_accounting()
        );
    }
    fixture
        .index()
        .execute_batch("DELETE FROM initial_inheritance;")
        .unwrap();
    assert!(fixture.lookup(initial.entries()).is_err());
}

/// Corruption remains local and preserves the exact accepted recovery receipt.
/// A missing first index row cannot conceal a damaged later selected entry.
#[test]
fn row_images_refuse_corruption_without_retiring_original_receipt() {
    for fault in 0..7 {
        let fixture = Fixture::new();
        let batch = fixture.batch(1);
        fixture
            .store
            .commit_accounted_transcript_batch(&batch, 1)
            .unwrap();
        fixture
            .store
            .accept_transcript_append_receipt(&batch, 2)
            .unwrap();
        let receipt_path = fs::read_dir(fixture.root.join(".append-receipts"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let receipt = fs::read(&receipt_path).unwrap();
        let index = fixture.index();
        match fault {
            0 => {
                index
                    .execute(
                        "UPDATE row_digests SET digest=?1 WHERE sequence='2'",
                        ["A".repeat(64)],
                    )
                    .unwrap();
            }
            1 => {
                index
                    .execute(
                        "UPDATE row_digests SET digest=?1 WHERE sequence='2'",
                        ["0".repeat(64)],
                    )
                    .unwrap();
            }
            2 => {
                index
                    .execute(
                        "UPDATE row_digests SET origin=?1 WHERE sequence='2'",
                        [String::from(mez_agent::ContextBlockOccurrenceId::allocate())],
                    )
                    .unwrap();
            }
            3 => {
                index.execute_batch("DROP TABLE row_digests;").unwrap();
            }
            4 => {
                index
                    .execute_batch("DELETE FROM row_digests WHERE sequence='1';")
                    .unwrap();
                index
                    .execute(
                        "UPDATE row_digests SET digest=?1 WHERE sequence='2'",
                        ["0".repeat(64)],
                    )
                    .unwrap();
            }
            5 => {
                index.execute_batch("DROP TABLE row_digest_meta;").unwrap();
            }
            _ => {
                index
                    .execute_batch("UPDATE row_digest_meta SET initial_required=1;")
                    .unwrap();
            }
        }
        drop(index);
        assert!(fixture.lookup(batch.entries()).is_err(), "fault={fault}");
        assert!(
            fixture
                .store
                .settle_transcript_append_receipt(&batch, 2)
                .is_err(),
            "fault={fault}"
        );
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt);
        assert_eq!(fixture.store.inspect(CID).unwrap(), batch.entries());
        let independent = TranscriptAppendBatch::owned_rows(vec![entry(
            "independent-images",
            1,
            TranscriptRole::User,
        )])
        .unwrap();
        fixture
            .store
            .commit_accounted_transcript_batch(&independent, 1)
            .unwrap();
    }
}

/// Successful raw CAS and survivor resequencing preserve occurrence IDs while
/// invalidating the obsolete full-row image. Repeated text cannot relend it to a
/// new physical position; a later original append receives its own proof.
#[test]
fn row_images_follow_real_edit_and_resequence_owners() {
    let fixture = Fixture::new();
    let batch = fixture.batch(1);
    fixture
        .store
        .commit_accounted_transcript_batch(&batch, 1)
        .unwrap();
    let original = &batch.entries()[0];
    fixture
        .store
        .compare_and_swap_entry_content(
            CID,
            1,
            &original.pane_id,
            &fixture.store.transcript_entry_revision(original).unwrap(),
            "edited actual source".into(),
        )
        .unwrap();
    let edited = fixture.store.inspect(CID).unwrap();
    assert_eq!(edited[0].content, "edited actual source");
    assert!(fixture.lookup(&edited[..1]).unwrap().is_none());
    assert_eq!(
        fixture.lookup(&edited[1..]).unwrap().unwrap()[&2],
        batch.row_accounting().unwrap().origins()[&2]
    );
    assert!(fixture.store.delete_entry(CID, 1).unwrap());
    let survivors = fixture.store.inspect(CID).unwrap();
    assert_eq!(survivors.len(), 1);
    assert!(fixture.lookup(&survivors).unwrap().is_none());
    let next = fixture.batch(2);
    fixture
        .store
        .commit_accounted_transcript_batch(&next, 3)
        .unwrap();
    assert_eq!(
        fixture.lookup(next.entries()).unwrap(),
        Some(next.row_accounting().unwrap().origins().clone())
    );
}

/// A real saved fork preserves the parent's occurrences while rebinding physical
/// conversation fields. It cannot copy parent row-image proof or invent initial
/// authority merely because its parent's accounting schema is version four.
#[test]
fn row_images_do_not_rebind_source_fingerprints_to_a_saved_fork() {
    let fixture = Fixture::new();
    let batch = fixture.batch(1);
    fixture
        .store
        .commit_accounted_transcript_batch(&batch, 1)
        .unwrap();
    fixture.store.fork(CID, "image-child", 99).unwrap();
    let child = fixture.store.inspect("image-child").unwrap();
    assert_eq!(child.len(), batch.entries().len());
    let _lease = fixture
        .store
        .acquire_conversation_lock("image-child")
        .unwrap();
    assert!(
        fixture
            .store
            .indexed_row_origins_locked("image-child", &child)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .store
            .transcript_row_origins_locked("image-child", &child)
            .unwrap(),
        *batch.row_accounting().unwrap().origins()
    );
    drop(_lease);
    assert_eq!(
        fixture.lookup(batch.entries()).unwrap(),
        Some(batch.row_accounting().unwrap().origins().clone())
    );
}
