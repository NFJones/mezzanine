//! Real private SQLite inventory controls over lower-owned acceptance and ACKs.

use std::path::PathBuf;

use mez_agent::messaging::{
    Envelope, MessageAcceptanceDecodeLimits, MessageAcceptanceWriteOutcome,
    MessageAcknowledgementWriteOutcome, MessageScope, MessageService, Recipient,
};

use super::*;

/// Owns one generated test-only private directory and its store.
struct Fixture(PathBuf);
impl Fixture {
    /// Selects a fresh path without reading or mutating user session data.
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "mez-mmp-inventory-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }
}
impl Drop for Fixture {
    /// Removes only the fixture's generated path after the test finishes.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Supplies the lower owner's existing finite payload and metadata policies.
fn limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 16 * 1024 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}

/// Creates one ordinary envelope under an actual registered sender and target.
fn envelope(
    sender: &mez_agent::messaging::SenderIdentity,
    recipient: &mez_core::ids::AgentId,
    id: &str,
) -> Envelope {
    Envelope {
        protocol: "mmp/1",
        id: id.into(),
        message_type: "send".into(),
        time: "fixture".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(recipient.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: id.repeat(40_000),
        extension_fields: vec![],
    }
}

/// Persists and publishes one exact original through the real lower/store port.
fn accept(
    store: &mut MmpCustodyStore,
    service: &mut MessageService,
    sender: &mez_agent::messaging::SenderIdentity,
    recipient: &mez_core::ids::AgentId,
    id: &str,
) -> u64 {
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(sender, recipient, id),
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let sequence = ticket.sequence();
    let plan = store.acceptance_plan(sequence).unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("fixture acceptance must commit")
    };
    service.publish_acceptance(&commit).unwrap();
    sequence
}

/// A reopened mixed store inventories actual accepted sources and an ACK while
/// preserving a legitimate cancelled sequence gap. Only compact references are
/// returned; neither the live subscription nor native application state moves.
#[test]
fn inventory_preserves_committed_originals_ack_and_cancelled_gap() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let empty = store.inventory_custody(limits()).unwrap();
    assert_eq!(empty.generation, 0);
    assert!(
        empty.occurrences.is_empty()
            && empty.acknowledgement_keys.is_empty()
            && empty.aborted_sequences.is_empty()
    );
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let cancelled = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target.agent_id, "cancelled"),
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    assert_eq!(cancelled.sequence(), 1);
    service.cancel_acceptance(&cancelled).unwrap();
    assert_eq!(
        accept(&mut store, &mut service, &sender, &target.agent_id, "first"),
        2
    );
    assert_eq!(
        accept(
            &mut store,
            &mut service,
            &sender,
            &target.agent_id,
            "second"
        ),
        3
    );
    let offered = service
        .receive_subscribed(&target.agent_id, 20, 10)
        .unwrap();
    assert_eq!(offered.messages.len(), 2);
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, 3, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    let plan = store.acknowledgement_plan().unwrap();
    assert!(matches!(
        write.persist(&mut store.acknowledgement_port(&plan)),
        MessageAcknowledgementWriteOutcome::Committed(_)
    ));
    drop(store);
    let store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.store_id, store.store_id);
    assert!(inventory.generation > 0);
    assert_eq!(
        inventory
            .occurrences
            .iter()
            .map(|item| item.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(
        inventory
            .occurrences
            .iter()
            .all(|item| item.recipient_count == 1
                && item.encoded_bytes > 64 * 1024
                && item.digest != [0; 32])
    );
    assert_eq!(inventory.acknowledgement_keys.len(), 1);
    assert!(inventory.aborted_sequences.is_empty());
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
}

/// An actual failed marker leaves private staged chunks and is never inventoried
/// as accepted. After qualified cleanup, its real abort disposition is tracked
/// separately and later committed mail remains visible without closing the gap.
#[test]
fn inventory_refuses_unsettled_stage_then_records_real_finalized_abort() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target.agent_id, "staged"),
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'fixture marker failure'); END;").unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    let MessageAcceptanceWriteOutcome::Unknown(unknown) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected staged source")
    };
    let error = store.inventory_custody(limits()).err().unwrap();
    assert!(
        error.to_string().contains("requires reconciliation"),
        "{error}"
    );
    let retry = service.retry_acceptance_write(unknown).unwrap();
    let MessageAcceptanceWriteOutcome::NotCommitted(rejected) =
        retry.persist(&mut store.acceptance_recovery_port(&plan))
    else {
        panic!("expected finalized abort")
    };
    service.release_rejected_acceptance(&rejected).unwrap();
    store
        .connection
        .execute_batch("DROP TRIGGER fail_marker")
        .unwrap();
    assert_eq!(
        accept(&mut store, &mut service, &sender, &target.agent_id, "later"),
        2
    );
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.aborted_sequences, vec![1]);
    assert_eq!(
        inventory
            .occurrences
            .iter()
            .map(|item| item.sequence)
            .collect::<Vec<_>>(),
        vec![2]
    );
    // A syntactically present abort is not enough: its original generation
    // equation must remain valid even when the row checksum was recomputed.
    let key = "a1:0000000000000001:stage";
    let original = store.read(key).unwrap().unwrap();
    let mut padded = original.clone();
    padded.resize(1024, 0);
    let digest = Sha256::digest(&padded).to_vec();
    store
        .connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            rusqlite::params![padded, digest, key],
        )
        .unwrap();
    assert!(store.inventory_custody(limits()).is_err());
    let mut changed = original;
    changed[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
    let digest = Sha256::digest(&changed).to_vec();
    store
        .connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            rusqlite::params![changed, digest, key],
        )
        .unwrap();
    assert!(store.inventory_custody(limits()).is_err());
}

/// Unknown keys, extra chunks and changed checksummed originals cannot produce a
/// partial successful inventory. The caller receives an error and no references.
#[test]
fn inventory_rejects_unknown_extra_and_corrupt_rows() {
    for corruption in ["unknown", "extra", "source"] {
        let fixture = Fixture::new();
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "sender", vec![]);
        let target = service.register_agent(None, None, "target", vec![]);
        assert_eq!(
            accept(
                &mut store,
                &mut service,
                &sender,
                &target.agent_id,
                "original"
            ),
            1
        );
        match corruption {
            "unknown" | "extra" => {
                let key = if corruption == "unknown" {
                    "unknown-key"
                } else {
                    "a1:0000000000000001:chunk:ffffffff"
                };
                let generation: i64 = store
                    .connection
                    .query_row(
                        "SELECT generation FROM state WHERE singleton=1",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                let generation = u64::try_from(generation).unwrap();
                store
                    .commit(
                        generation,
                        &[CustodyWrite {
                            key,
                            previous: None,
                            value: Some(b"changed"),
                        }],
                    )
                    .unwrap();
            }
            _ => {
                let key = "a1:0000000000000001:chunk:00000000";
                let mut bytes = store.read(key).unwrap().unwrap();
                let index = bytes.iter().position(|byte| *byte == b'o').unwrap();
                bytes[index] = b'p';
                let digest = Sha256::digest(&bytes).to_vec();
                store
                    .connection
                    .execute(
                        "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
                        rusqlite::params![bytes, digest, key],
                    )
                    .unwrap();
            }
        }
        assert!(store.inventory_custody(limits()).is_err(), "{corruption}");
    }
}

/// Inventory cannot mix two committed SQLite revisions or use a different
/// store's transaction. A competing writer is blocked by the pinned snapshot.
#[test]
fn inventory_uses_one_actual_same_store_snapshot() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    accept(
        &mut store,
        &mut service,
        &sender,
        &target.agent_id,
        "original",
    );
    let transaction = store.connection.unchecked_transaction().unwrap();
    assert!(store.read("a1:0000000000000001:stage").unwrap().is_some());
    let path = fixture.0.join("custody.sqlite");
    let writer = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .busy_timeout(std::time::Duration::from_millis(100))
            .unwrap();
        matches!(connection.execute("UPDATE records SET value=X'00' WHERE key=?1", ["a1:0000000000000001:chunk:00000000"]), Err(rusqlite::Error::SqliteFailure(error, _)) if matches!(error.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
    });
    assert!(writer.join().unwrap());
    assert_eq!(
        store
            .inventory_in_snapshot(&transaction, limits())
            .unwrap()
            .occurrences
            .len(),
        1
    );
    transaction.commit().unwrap();
    let other_fixture = Fixture::new();
    let other = MmpCustodyStore::open(&other_fixture.0, "another-session").unwrap();
    let foreign = other.connection.unchecked_transaction().unwrap();
    assert!(store.inventory_in_snapshot(&foreign, limits()).is_err());
}

/// A nonempty committed store cannot have generation zero, and one generic
/// transaction cannot create more than its bounded batch of rows. Corrupting
/// only the state generation must not yield otherwise valid-looking references.
#[test]
fn inventory_rejects_generation_too_low_for_actual_rows() {
    for large in [false, true] {
        let fixture = Fixture::new();
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "sender", vec![]);
        let target = service.register_agent(None, None, "target", vec![]);
        let mut message = envelope(&sender, &target.agent_id, "original");
        if large {
            message.payload = "\0".repeat(1024 * 1024);
        }
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                message,
                MessageScope::Session,
                10,
                4096,
                None,
            )
            .unwrap();
        let plan = store.acceptance_plan(ticket.sequence()).unwrap();
        let write = service.begin_acceptance_write(&ticket).unwrap();
        assert!(matches!(
            write.persist(&mut store.acceptance_port(&plan)),
            MessageAcceptanceWriteOutcome::Committed(_)
        ));
        assert_eq!(
            store.inventory_custody(limits()).unwrap().occurrences.len(),
            1
        );
        let rows: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
            .unwrap();
        if large {
            assert!(rows > MAX_BATCH_RECORDS as i64);
            store
                .connection
                .execute("UPDATE state SET generation=1 WHERE singleton=1", [])
                .unwrap();
        } else {
            store
                .connection
                .execute(
                    "UPDATE state SET generation=0,batch=X'' WHERE singleton=1",
                    [],
                )
                .unwrap();
        }
        assert!(
            store.inventory_custody(limits()).is_err(),
            "large={large}, rows={rows}"
        );
    }
}

/// The generation check must not assume one row per transaction: the generic
/// store legally writes several canonical rows in one bounded batch. A real
/// original copied as exact rows into one batch remains a descriptive source.
#[test]
fn inventory_accepts_valid_original_written_in_one_generic_batch() {
    let source_fixture = Fixture::new();
    let mut source = MmpCustodyStore::open(&source_fixture.0, "source").unwrap();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    assert_eq!(
        accept(&mut source, &mut service, &sender, &target.agent_id, "a"),
        1
    );
    let keys = [
        "a1:0000000000000001:stage",
        "a1:0000000000000001:chunk:00000000",
        "a1:0000000000000001:commit",
    ];
    let values = keys
        .iter()
        .map(|key| source.read(key).unwrap().unwrap())
        .collect::<Vec<_>>();
    let writes = keys
        .iter()
        .zip(values.iter())
        .map(|(key, value)| CustodyWrite {
            key,
            previous: None,
            value: Some(value),
        })
        .collect::<Vec<_>>();
    let destination_fixture = Fixture::new();
    let mut destination = MmpCustodyStore::open(&destination_fixture.0, "destination").unwrap();
    assert_eq!(destination.commit(0, &writes).unwrap().generation(), 1);
    let inventory = destination.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.generation, 1);
    assert_eq!(inventory.occurrences.len(), 1);
    assert_eq!(inventory.occurrences[0].sequence, 1);
}

/// A crash-like cleanup interruption after another committed original exists
/// must not yield even that earlier partial inventory. The aborted source is
/// listed only after its real retained cleanup owner completes every deletion.
#[test]
fn partially_cleaned_abort_withholds_earlier_inventory_until_reconciled() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    assert_eq!(
        accept(&mut store, &mut service, &sender, &target.agent_id, "first"),
        1
    );
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target.agent_id, "interrupted"),
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'fixture marker failure'); END;").unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    let MessageAcceptanceWriteOutcome::Unknown(unknown) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected staged source")
    };
    store.connection.execute_batch("CREATE TRIGGER fail_cleanup BEFORE DELETE ON records WHEN OLD.key='a1:0000000000000002:chunk:00000000' BEGIN SELECT RAISE(ABORT,'fixture cleanup failure'); END;").unwrap();
    let retry = service.retry_acceptance_write(unknown).unwrap();
    let MessageAcceptanceWriteOutcome::Unknown(unknown) =
        retry.persist(&mut store.acceptance_recovery_port(&plan))
    else {
        panic!("expected interrupted cleanup")
    };
    let error = store.inventory_custody(limits()).err().unwrap();
    assert!(
        error.to_string().contains("requires reconciliation"),
        "{error}"
    );
    store
        .connection
        .execute_batch("DROP TRIGGER fail_cleanup")
        .unwrap();
    let retry = service.retry_acceptance_write(unknown).unwrap();
    let MessageAcceptanceWriteOutcome::NotCommitted(rejected) =
        retry.persist(&mut store.acceptance_recovery_port(&plan))
    else {
        panic!("expected completed cleanup")
    };
    service.release_rejected_acceptance(&rejected).unwrap();
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(
        inventory
            .occurrences
            .iter()
            .map(|item| item.sequence)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(inventory.aborted_sequences, vec![2]);
}
