//! Exact physical append preparation/reconciliation without native authority.

use super::*;
use mez_agent::transcript::TranscriptRole;

mod receipts;

/// Owns only an isolated generated physical transcript destination.
struct Fixture {
    root: PathBuf,
    store: AgentTranscriptStore,
    row: TranscriptEntry,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mez-native-input-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let store = AgentTranscriptStore::new(&root);
        let mut row = TranscriptEntry {
            conversation_id: "original".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "first".into(),
            agent_id: "receiver".into(),
            pane_id: "pane".into(),
            content: "original prefix".into(),
        };
        store.append(&row).unwrap();
        row.turn_id = "actual-input-owner".into();
        row.content = "exact future native row\0with embedded\nnewline".into();
        Self { root, store, row }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// An actual interrupted own append is completed from the exact retained bytes;
/// a second retry requalifies the same physical row without allocating another.
#[test]
fn native_transcript_completes_exact_partial_own_row_without_duplicate_append() {
    let mut fixture = Fixture::new();
    let lease = fixture
        .store
        .native_input_lease("original", false, None)
        .unwrap();
    let plan = lease.prepare(&mut fixture.row).unwrap();
    let encoded = encoded_row(&fixture.row).unwrap();
    let mut file = OpenOptions::new().append(true).open(&lease.path).unwrap();
    file.write_all(&encoded[..encoded.len() / 2]).unwrap();
    file.sync_all().unwrap();
    assert!(lease.seal(&plan).is_err());
    lease.append(&plan, &fixture.row).unwrap();
    assert_eq!(lease.seal(&plan).unwrap(), fixture.row);
    lease.append(&plan, &fixture.row).unwrap();
    assert_eq!(fixture.store.inspect("original").unwrap().len(), 2);
}

/// A changed original prefix and a foreign partial suffix are both conflicts;
/// neither grants permission to repair different content or advance a sequence.
#[test]
fn native_transcript_refuses_changed_prefix_and_foreign_partial_suffix() {
    let mut fixture = Fixture::new();
    let lease = fixture
        .store
        .native_input_lease("original", false, None)
        .unwrap();
    let plan = lease.prepare(&mut fixture.row).unwrap();
    let mut file = OpenOptions::new().append(true).open(&lease.path).unwrap();
    file.write_all(b"foreign partial bytes").unwrap();
    assert!(lease.append(&plan, &fixture.row).is_err());
    file.set_len(plan.prefix_bytes).unwrap();
    let mut writer = OpenOptions::new().write(true).open(&lease.path).unwrap();
    writer.write_all(b"changed!").unwrap();
    assert!(lease.append(&plan, &fixture.row).is_err());
}

/// Recovery requires the original physical history. A missing archive must not
/// be replaced by an empty file merely while opening a pending preparation.
#[test]
fn native_transcript_existing_lease_does_not_create_missing_archive() {
    let fixture = Fixture::new();
    let history = fixture.root.join("original/history.tsv");
    fs::remove_file(&history).unwrap();
    assert!(
        fixture
            .store
            .native_input_lease("original", false, None)
            .is_err()
    );
    assert!(!history.exists());
}
