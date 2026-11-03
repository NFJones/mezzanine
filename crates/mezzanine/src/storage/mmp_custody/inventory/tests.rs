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
    let restart_empty = store.inspect_restart_inventory(limits()).unwrap();
    assert!(restart_empty.pending.is_none());
    assert!(restart_empty.inventory.occurrences.is_empty());
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
    let restart = store.inspect_restart_inventory(limits()).unwrap();
    assert!(restart.inventory.occurrences.is_empty());
    assert!(restart.inventory.acknowledgement_keys.is_empty());
    assert!(
        matches!(restart.pending, Some(PendingCustodyRef::Unfinished {
        sequence: 1, encoded_bytes: 1.., present_chunks, total_chunks,
    }) if present_chunks == total_chunks && total_chunks > 0)
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
    let mut legacy = original.clone();
    legacy[7] = b'1';
    let digest = Sha256::digest(&legacy).to_vec();
    store
        .connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            rusqlite::params![legacy, digest, key],
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
    let mut pending = PendingScan {
        enabled: true,
        found: None,
        batch: Vec::new(),
    };
    assert!(
        store
            .scan_inventory(&foreign, limits(), &mut pending)
            .is_err()
    );
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
/// must not yield strict inventory. A separate preflight may quarantine the
/// actual partial abort while validating the original, but must reject changed
/// descriptor counts, deletion evidence or batch. The abort enters strict
/// inventory only after its retained cleanup owner completes every deletion.
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
    let restart = store.inspect_restart_inventory(limits()).unwrap();
    assert_eq!(restart.inventory.occurrences.len(), 1);
    assert_eq!(restart.inventory.occurrences[0].sequence, 1);
    assert!(matches!(
        restart.pending,
        Some(PendingCustodyRef::Aborting {
            sequence: 2,
            encoded_bytes: 1..,
            present_chunks: 1,
            total_chunks: 2..,
        })
    ));
    let stage_key = "a1:0000000000000002:stage";
    let stage = store.read(stage_key).unwrap().unwrap();
    for offset in [34, 104] {
        let mut changed = stage.clone();
        changed[offset] ^= 1;
        let digest = Sha256::digest(&changed);
        store
            .connection
            .execute(
                "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
                rusqlite::params![&changed, digest.as_slice(), stage_key],
            )
            .unwrap();
        assert!(store.inspect_restart_inventory(limits()).is_err());
    }
    let digest = Sha256::digest(&stage);
    store
        .connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            rusqlite::params![&stage, digest.as_slice(), stage_key],
        )
        .unwrap();
    let batch: Vec<u8> = store
        .connection
        .query_row("SELECT batch FROM state WHERE singleton=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    store
        .connection
        .execute("UPDATE state SET batch=zeroblob(32)", [])
        .unwrap();
    assert!(store.inspect_restart_inventory(limits()).is_err());
    store
        .connection
        .execute("UPDATE state SET batch=?1", [&batch])
        .unwrap();
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

/// Restart inspection validates every committed source and stored ACK in the
/// same snapshot as an unresolved original. A real competing writer must be
/// blocked while that snapshot is pinned. A changed ACK with a valid row
/// checksum, unknown or additional unresolved rows, and corruption of an
/// unrelated accepted source must all withhold the entire result.
#[test]
fn restart_preflight_validates_all_accepted_sources_and_acknowledgements() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    assert_eq!(
        accept(&mut store, &mut service, &sender, &target.agent_id, "first"),
        1
    );
    service
        .receive_subscribed(&target.agent_id, 20, 10)
        .unwrap();
    let ack = service
        .prepare_acknowledgement(&target.agent_id, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = service.begin_acknowledgement_write(&ack).unwrap();
    let plan = store.acknowledgement_plan().unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) =
        write.persist(&mut store.acknowledgement_port(&plan))
    else {
        panic!("expected committed acknowledgement")
    };
    service.publish_acknowledgement(&commit).unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target.agent_id, "pending"),
            MessageScope::Session,
            30,
            4096,
            None,
        )
        .unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'fixture marker failure'); END;").unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    assert!(matches!(
        write.persist(&mut store.acceptance_port(&plan)),
        MessageAcceptanceWriteOutcome::Unknown(_)
    ));
    let result = store.inspect_restart_inventory(limits()).unwrap();
    assert_eq!(result.inventory.occurrences.len(), 1);
    assert_eq!(result.inventory.acknowledgement_keys.len(), 1);
    let ack_key = result.inventory.acknowledgement_keys[0].clone();
    assert!(matches!(
        result.pending,
        Some(PendingCustodyRef::Unfinished { sequence: 2, .. })
    ));
    let trusted_id = store.store_id.clone();
    let trusted_batch: Vec<u8> = store
        .connection
        .query_row("SELECT batch FROM state WHERE singleton=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    for fault in ["id", "owner", "batch"] {
        match fault {
            "id" => {
                let mut id = trusted_id.clone();
                id[0] ^= 1;
                store
                    .connection
                    .execute("UPDATE state SET store_id=?1", [&id])
                    .unwrap();
            }
            "owner" => {
                store
                    .connection
                    .execute("UPDATE state SET owner='foreign'", [])
                    .unwrap();
            }
            "batch" => {
                store
                    .connection
                    .execute("UPDATE state SET batch=X''", [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            store.inspect_restart_inventory(limits()).is_err(),
            "{fault}"
        );
        assert!(store.inspect_unfinished_acceptance(2).is_err(), "{fault}");
        assert!(store.read_accepted_source(1, limits()).is_err(), "{fault}");
        assert!(
            store
                .read_stored_acknowledgement(&ack_key, limits())
                .is_err(),
            "{fault}"
        );
        store
            .connection
            .execute(
                "UPDATE state SET owner='session',store_id=?1,batch=?2",
                rusqlite::params![&trusted_id, &trusted_batch],
            )
            .unwrap();
    }
    assert!(store.inspect_restart_inventory(limits()).is_ok());
    let ack = store.read(&ack_key).unwrap().unwrap();
    let mut changed_ack = ack.clone();
    changed_ack[0] ^= 1;
    let changed_digest = Sha256::digest(&changed_ack);
    store
        .connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            rusqlite::params![&changed_ack, changed_digest.as_slice(), &ack_key],
        )
        .unwrap();
    assert!(store.inspect_restart_inventory(limits()).is_err());
    let ack_digest = Sha256::digest(&ack);
    store
        .connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            rusqlite::params![&ack, ack_digest.as_slice(), &ack_key],
        )
        .unwrap();
    assert!(store.inspect_restart_inventory(limits()).is_ok());
    let transaction = store.connection.unchecked_transaction().unwrap();
    let mut pending = PendingScan {
        enabled: true,
        found: None,
        batch: Vec::new(),
    };
    let snapshot = store
        .scan_inventory(&transaction, limits(), &mut pending)
        .unwrap();
    assert_eq!(snapshot.acknowledgement_keys.len(), 1);
    assert!(matches!(
        pending.found,
        Some(PendingCustodyRef::Unfinished { sequence: 2, .. })
    ));
    let path = fixture.0.join("custody.sqlite");
    let writer = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .busy_timeout(std::time::Duration::from_millis(100))
            .unwrap();
        matches!(connection.execute("UPDATE records SET value=X'00' WHERE key='a1:0000000000000002:chunk:00000000'", []), Err(rusqlite::Error::SqliteFailure(error, _)) if matches!(error.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
    });
    assert!(writer.join().unwrap());
    transaction.commit().unwrap();

    // An unknown later namespace must withhold the entire preflight result.
    let unknown = b"unknown";
    let unknown_digest = Sha256::digest(unknown);
    store
        .connection
        .execute(
            "INSERT INTO records(key,value,digest) VALUES('z:unknown',?1,?2)",
            rusqlite::params![unknown.as_slice(), unknown_digest.as_slice()],
        )
        .unwrap();
    assert!(store.inspect_restart_inventory(limits()).is_err());
    store
        .connection
        .execute("DELETE FROM records WHERE key='z:unknown'", [])
        .unwrap();

    // A second unresolved namespace cannot be hidden behind a valid first one.
    let stage = store.read("a1:0000000000000002:stage").unwrap().unwrap();
    let stage_digest = Sha256::digest(&stage);
    store
        .connection
        .execute(
            "INSERT INTO records(key,value,digest) VALUES('a1:0000000000000003:stage',?1,?2)",
            rusqlite::params![&stage, stage_digest.as_slice()],
        )
        .unwrap();
    assert!(store.inspect_restart_inventory(limits()).is_err());
    store
        .connection
        .execute(
            "DELETE FROM records WHERE key='a1:0000000000000003:stage'",
            [],
        )
        .unwrap();
    assert!(store.inspect_restart_inventory(limits()).is_ok());
    store
        .connection
        .execute(
            "UPDATE records SET value=X'00' WHERE key='a1:0000000000000001:chunk:00000000'",
            [],
        )
        .unwrap();
    assert!(store.inspect_restart_inventory(limits()).is_err());
}

/// A real stage whose first chunk could not be written is still represented as
/// one quarantined unfinished original. It is neither accepted mail nor a
/// finalized abort, and strict inventory must continue to reject it.
#[test]
fn restart_preflight_quarantines_a_stage_with_no_chunks() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target.agent_id, "pending"),
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_chunk BEFORE INSERT ON records WHEN NEW.key LIKE '%:chunk:%' BEGIN SELECT RAISE(ABORT,'fixture chunk failure'); END;").unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    assert!(matches!(
        write.persist(&mut store.acceptance_port(&plan)),
        MessageAcceptanceWriteOutcome::Unknown(_)
    ));
    assert!(store.inventory_custody(limits()).is_err());
    let result = store.inspect_restart_inventory(limits()).unwrap();
    assert!(result.inventory.occurrences.is_empty());
    assert!(result.inventory.aborted_sequences.is_empty());
    assert!(matches!(
        result.pending,
        Some(PendingCustodyRef::Unfinished {
            sequence: 1,
            encoded_bytes: 1..,
            present_chunks: 0,
            total_chunks: 1..,
        })
    ));
}

/// A cleanup interrupted before its first deletion leaves all original chunks
/// plus a durable v2 abort declaration. Restart preflight validates that exact
/// declaration batch and quarantines it without treating it as completed.
#[test]
fn restart_preflight_quarantines_abort_before_first_deletion() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target.agent_id, "pending"),
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
        panic!("expected staged original")
    };
    store.connection.execute_batch("CREATE TRIGGER fail_cleanup BEFORE DELETE ON records BEGIN SELECT RAISE(ABORT,'fixture cleanup failure'); END;").unwrap();
    let retry = service.retry_acceptance_write(unknown).unwrap();
    assert!(matches!(
        retry.persist(&mut store.acceptance_recovery_port(&plan)),
        MessageAcceptanceWriteOutcome::Unknown(_)
    ));
    assert!(store.inventory_custody(limits()).is_err());
    let result = store.inspect_restart_inventory(limits()).unwrap();
    assert!(result.inventory.occurrences.is_empty());
    assert!(result.inventory.aborted_sequences.is_empty());
    assert!(matches!(result.pending, Some(PendingCustodyRef::Aborting {
        sequence: 1, encoded_bytes: 1.., present_chunks, total_chunks,
    }) if present_chunks == total_chunks && total_chunks > 0));
}

/// A source-free restart declaration must preserve an unrelated committed
/// occurrence and its actual acknowledgement. The resulting unresolved abort
/// remains quarantined, and neither prior acceptance nor ACK is regenerated.
#[test]
fn restart_declaration_preserves_committed_source_and_acknowledgement() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    assert_eq!(
        accept(&mut store, &mut service, &sender, &target.agent_id, "first"),
        1
    );
    service
        .receive_subscribed(&target.agent_id, 20, 10)
        .unwrap();
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    let plan = store.acknowledgement_plan().unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) =
        write.persist(&mut store.acknowledgement_port(&plan))
    else {
        panic!("expected committed acknowledgement")
    };
    service.publish_acknowledgement(&commit).unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target.agent_id, "pending"),
            MessageScope::Session,
            30,
            4096,
            None,
        )
        .unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'fixture marker failure'); END;").unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    assert!(matches!(
        write.persist(&mut store.acceptance_port(&plan)),
        MessageAcceptanceWriteOutcome::Unknown(_)
    ));
    store
        .connection
        .execute_batch("DROP TRIGGER fail_marker")
        .unwrap();
    let before = store.inspect_restart_inventory(limits()).unwrap();
    let source_digest = before.inventory.occurrences[0].digest;
    let ack_key = before.inventory.acknowledgement_keys[0].clone();
    let ack = store.read(&ack_key).unwrap().unwrap();
    drop(service);
    drop(store);
    let owner = super::super::ownership::RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    store.connection.execute_batch("CREATE TRIGGER delete_ack AFTER UPDATE OF value ON records WHEN NEW.key='a1:0000000000000002:stage' BEGIN DELETE FROM records WHERE key LIKE 'k1:%'; END;").unwrap();
    let mut owned = owner.bind_store(&mut store).unwrap();
    assert!(owned.declare_restart_cleanup(limits()).is_err());
    assert_eq!(
        owned.store_mut().read(&ack_key).unwrap().as_ref(),
        Some(&ack)
    );
    assert_eq!(
        owned
            .store_mut()
            .inspect_restart_inventory(limits())
            .unwrap()
            .inventory
            .generation,
        before.inventory.generation
    );
    owned
        .store_mut()
        .connection
        .execute_batch("DROP TRIGGER delete_ack")
        .unwrap();
    assert_eq!(owned.declare_restart_cleanup(limits()).unwrap(), Some(2));
    let after = owned
        .store_mut()
        .inspect_restart_inventory(limits())
        .unwrap();
    assert_eq!(after.inventory.occurrences.len(), 1);
    assert_eq!(after.inventory.occurrences[0].digest, source_digest);
    assert_eq!(after.inventory.acknowledgement_keys, vec![ack_key.clone()]);
    assert_eq!(owned.store_mut().read(&ack_key).unwrap(), Some(ack));
    assert!(matches!(
        after.pending,
        Some(PendingCustodyRef::Aborting { sequence: 2, .. })
    ));
}

/// Deleting a complete unrelated committed namespace from an update trigger
/// leaves a superficially valid sequence gap. Restart declaration must detect
/// the omitted original and roll back the stage update and all trigger effects.
#[test]
fn restart_declaration_rolls_back_omitted_committed_original() {
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
            envelope(&sender, &target.agent_id, "pending"),
            MessageScope::Session,
            30,
            4096,
            None,
        )
        .unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'fixture marker failure'); END;").unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    assert!(matches!(
        write.persist(&mut store.acceptance_port(&plan)),
        MessageAcceptanceWriteOutcome::Unknown(_)
    ));
    store
        .connection
        .execute_batch("DROP TRIGGER fail_marker")
        .unwrap();
    let before = store.inspect_restart_inventory(limits()).unwrap();
    let source_digest = before.inventory.occurrences[0].digest;
    store.connection.execute_batch("CREATE TRIGGER delete_original AFTER UPDATE OF value ON records WHEN NEW.key='a1:0000000000000002:stage' BEGIN DELETE FROM records WHERE key LIKE 'a1:0000000000000001:%'; END;").unwrap();
    let owner = super::super::ownership::RootOwnership::acquire(&fixture.0).unwrap();
    let mut owned = owner.bind_store(&mut store).unwrap();
    assert!(
        owned.declare_restart_cleanup(limits()).is_err(),
        "deleting a committed original must roll back"
    );
    let after = owned
        .store_mut()
        .inspect_restart_inventory(limits())
        .unwrap();
    assert_eq!(after.inventory.generation, before.inventory.generation);
    assert_eq!(after.inventory.occurrences.len(), 1);
    assert_eq!(after.inventory.occurrences[0].digest, source_digest);
    assert!(matches!(
        after.pending,
        Some(PendingCustodyRef::Unfinished { sequence: 2, .. })
    ));
}

/// A finalized abort remains sequence evidence even though it is not mail.
/// A trigger that removes it while declaring a later stage must roll back both
/// effects, rather than silently allowing an allocator-relevant gap.
#[test]
fn restart_declaration_rolls_back_omitted_abort_tombstone() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target.agent_id, "aborted"),
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_chunk BEFORE INSERT ON records WHEN NEW.key LIKE '%:chunk:%' BEGIN SELECT RAISE(ABORT,'fixture chunk failure'); END;").unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    let MessageAcceptanceWriteOutcome::Unknown(unknown) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected interrupted original")
    };
    store
        .connection
        .execute_batch("DROP TRIGGER fail_chunk")
        .unwrap();
    let retry = service.retry_acceptance_write(unknown).unwrap();
    let MessageAcceptanceWriteOutcome::NotCommitted(rejected) =
        retry.persist(&mut store.acceptance_recovery_port(&plan))
    else {
        panic!("expected finalized abort")
    };
    service.release_rejected_acceptance(&rejected).unwrap();
    let tombstone = store.read("a1:0000000000000001:stage").unwrap().unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target.agent_id, "pending"),
            MessageScope::Session,
            30,
            4096,
            None,
        )
        .unwrap();
    assert_eq!(ticket.sequence(), 2);
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'fixture marker failure'); END;").unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    assert!(matches!(
        write.persist(&mut store.acceptance_port(&plan)),
        MessageAcceptanceWriteOutcome::Unknown(_)
    ));
    store.connection.execute_batch("DROP TRIGGER fail_marker; CREATE TRIGGER delete_abort AFTER UPDATE OF value ON records WHEN NEW.key='a1:0000000000000002:stage' BEGIN DELETE FROM records WHERE key='a1:0000000000000001:stage'; END;").unwrap();
    let before = store.inspect_restart_inventory(limits()).unwrap();
    assert_eq!(before.inventory.aborted_sequences, vec![1]);
    let owner = super::super::ownership::RootOwnership::acquire(&fixture.0).unwrap();
    let mut owned = owner.bind_store(&mut store).unwrap();
    assert!(owned.declare_restart_cleanup(limits()).is_err());
    assert_eq!(
        owned.store_mut().read("a1:0000000000000001:stage").unwrap(),
        Some(tombstone)
    );
    let after = owned
        .store_mut()
        .inspect_restart_inventory(limits())
        .unwrap();
    assert_eq!(after.inventory.generation, before.inventory.generation);
    assert!(matches!(
        after.pending,
        Some(PendingCustodyRef::Unfinished { sequence: 2, .. })
    ));
}
