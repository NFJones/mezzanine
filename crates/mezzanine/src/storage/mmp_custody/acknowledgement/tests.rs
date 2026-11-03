//! Real private-store ACK composition tests with the actual lower linear owner.
//! Offers exercise the lower prerequisite; these fixtures do not claim native
//! context durability or finished runtime ingress/supervisor integration.

use super::*;
use mez_agent::messaging::{
    Envelope, MessageAcceptanceDecodeLimits, MessageAcceptanceWriteOutcome,
    MessageAcknowledgementAttempt, MessageAcknowledgementJoin, MessageAcknowledgementTicket,
    MessageAcknowledgementWriteOutcome, MessageScope, MessageService, Recipient,
};

/// Supplies the lower owner's existing independent acceptance ceilings.
fn decode_limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 16 * 1024 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}

/// An existing content-addressed ACK and valid original source cannot qualify
/// a positive receipt after the store's persisted identity, owner or batch
/// framing changes. The retry must not bypass transaction state validation.
#[test]
fn existing_acknowledgement_rechecks_persisted_store_state() {
    let mut admitted = Vec::new();
    for fault in ["id", "owner", "batch"] {
        let fixture = Fixture::new();
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let (_service, _ticket, write) = prepared(&mut store);
        let plan = store.acknowledgement_plan().unwrap();
        store.acknowledgement_port(&plan).commit(&write).unwrap();
        match fault {
            "id" => {
                let mut id = store.store_id.clone();
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
        if store.acknowledgement_port(&plan).commit(&write).is_ok() {
            admitted.push(fault);
        }
    }
    assert!(
        admitted.is_empty(),
        "changed state qualified ACK: {admitted:?}"
    );
}

/// A real committed ACK cannot be reconciled if its store generation has
/// rolled behind the original plan, even when the ACK, sources and batch have
/// valid framing. Exact retries at the committed generation, later historical
/// duplicates and an already-present ACK at a fresh plan remain valid.
#[test]
fn existing_acknowledgement_rejects_generation_rollback_but_keeps_history() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (_service, _ticket, write) = prepared(&mut store);
    let plan = store.acknowledgement_plan().unwrap();
    assert!(plan.generation > 1);
    store.acknowledgement_port(&plan).commit(&write).unwrap();
    let committed_generation = plan.generation + 1;
    store
        .connection
        .execute(
            "UPDATE state SET generation=?1",
            [i64::try_from(plan.generation - 1).unwrap()],
        )
        .unwrap();
    assert!(store.acknowledgement_port(&plan).commit(&write).is_err());
    store
        .connection
        .execute(
            "UPDATE state SET generation=?1",
            [i64::try_from(committed_generation).unwrap()],
        )
        .unwrap();
    store.acknowledgement_port(&plan).commit(&write).unwrap();
    store
        .commit(
            committed_generation,
            &[CustodyWrite {
                key: "unrelated",
                previous: None,
                value: Some(b"later"),
            }],
        )
        .unwrap();
    store.acknowledgement_port(&plan).commit(&write).unwrap();
    let fresh_plan = store.acknowledgement_plan().unwrap();
    store
        .acknowledgement_port(&fresh_plan)
        .commit(&write)
        .unwrap();
}

/// An exact ACK can already exist when a later plan is captured. If an
/// unrelated valid batch then advances the store once, that historical ACK
/// remains a valid duplicate even though it is not the most recent batch.
#[test]
fn existing_acknowledgement_survives_one_later_unrelated_batch() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (_service, _ticket, write) = prepared(&mut store);
    let original_plan = store.acknowledgement_plan().unwrap();
    store
        .acknowledgement_port(&original_plan)
        .commit(&write)
        .unwrap();
    let later_plan = store.acknowledgement_plan().unwrap();
    store
        .commit(
            later_plan.generation,
            &[CustodyWrite {
                key: "unrelated",
                previous: None,
                value: Some(b"later"),
            }],
        )
        .unwrap();
    store
        .acknowledgement_port(&later_plan)
        .commit(&write)
        .unwrap();
}

/// A real competing connection cannot change the accepted source between ACK
/// source validation and either a new ACK write or an existing-ACK receipt.
/// The deterministic hook runs inside the production transaction at that exact
/// boundary; the original source remains intact after the blocked attempt.
#[test]
fn acknowledgement_source_and_settlement_share_one_writer_fence() {
    use std::cell::Cell;

    for existing in [false, true] {
        let fixture = Fixture::new();
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let (_service, _ticket, write) = prepared(&mut store);
        let plan = store.acknowledgement_plan().unwrap();
        if existing {
            store.acknowledgement_port(&plan).commit(&write).unwrap();
        }
        let key = "a1:0000000000000001:chunk:00000000";
        let original = store.read(key).unwrap().unwrap();
        let blocked = Cell::new(false);
        let path = fixture.0.join("custody.sqlite");
        let probe = || {
            let writer = rusqlite::Connection::open(&path).unwrap();
            writer
                .busy_timeout(std::time::Duration::from_millis(100))
                .unwrap();
            blocked.set(matches!(
                writer.execute("UPDATE records SET value=X'00' WHERE key=?1", [key]),
                Err(rusqlite::Error::SqliteFailure(error, _))
                    if matches!(error.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            ));
        };
        store
            .acknowledgement_port_with_probe(&plan, &probe)
            .commit(&write)
            .unwrap();
        assert!(
            blocked.get(),
            "competing writer entered during existing={existing}"
        );
        assert_eq!(store.read(key).unwrap().unwrap(), original);
    }
}

/// A trigger that corrupts an accepted source while the ACK row is inserted
/// must not produce a positive ACK receipt. The whole transaction, including
/// the source mutation and ACK insertion, must roll back together.
#[test]
fn acknowledgement_rolls_back_if_its_write_changes_a_source() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (_service, _ticket, write) = prepared(&mut store);
    let plan = store.acknowledgement_plan().unwrap();
    let source_key = "a1:0000000000000001:chunk:00000000";
    let original = store.read(source_key).unwrap().unwrap();
    let mut encoded = Vec::new();
    write.write_record(&mut encoded).unwrap();
    let key = ack_key(&encoded);
    store.connection.execute_batch("CREATE TRIGGER corrupt_source AFTER INSERT ON records WHEN NEW.key LIKE 'k1:%' BEGIN UPDATE records SET value=X'00' WHERE key='a1:0000000000000001:chunk:00000000'; END;").unwrap();
    assert!(store.acknowledgement_port(&plan).commit(&write).is_err());
    assert_eq!(store.read(source_key).unwrap().unwrap(), original);
    assert!(store.read(&key).unwrap().is_none());
}

/// Reopening a real committed ACK retains its original recipient interval and
/// exact source decision. Reading verifies storage only and does not advance the
/// lower service cursor or establish a recovered native application commit.
#[test]
fn reopened_acknowledgement_reads_exact_source_without_advancing_cursor() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (service, _ticket, write) = prepared(&mut store);
    let recipient = write.recipient().clone();
    let mut bytes = Vec::new();
    write.write_record(&mut bytes).unwrap();
    let key = ack_key(&bytes);
    let plan = store.acknowledgement_plan().unwrap();
    assert!(matches!(
        write.persist(&mut store.acknowledgement_port(&plan)),
        MessageAcknowledgementWriteOutcome::Committed(_)
    ));
    drop(store);
    let store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let decoded = store
        .read_stored_acknowledgement(&key, decode_limits())
        .unwrap();
    assert_eq!(decoded.recipient, recipient.as_str());
    assert_eq!(decoded.registered_after_sequence, 0);
    assert_eq!(decoded.after_sequence, 0);
    assert_eq!(decoded.endpoint_sequence, 1);
    assert_eq!(decoded.observed_at_ms, 20);
    assert_eq!(decoded.decisions.len(), 1);
    assert_eq!(decoded.decisions[0].sequence, 1);
    assert_eq!(decoded.decisions[0].binding_index, 0);
    assert!(decoded.decisions[0].acknowledged);
    assert_eq!(service.subscription(&recipient).unwrap().last_sequence, 0);
}

/// A checksummed and correctly content-addressed ACK with a forged binding slot
/// cannot be adopted merely because its occurrence or recipient label matches.
/// It must still refer to the exact frozen binding of the verified original.
#[test]
fn stored_acknowledgement_rejects_forged_original_binding() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (_service, _ticket, write) = prepared(&mut store);
    let mut bytes = Vec::new();
    write.write_record(&mut bytes).unwrap();
    let offset = 48 + write.recipient().as_str().len();
    bytes[offset + 8..offset + 12].copy_from_slice(&1u32.to_le_bytes());
    let key = ack_key(&bytes);
    let generation = store.acknowledgement_plan().unwrap().generation;
    store
        .commit(
            generation,
            &[CustodyWrite {
                key: &key,
                previous: None,
                value: Some(&bytes),
            }],
        )
        .unwrap();
    let error = store
        .read_stored_acknowledgement(&key, decode_limits())
        .err()
        .unwrap();
    assert!(error.to_string().contains("binding is missing"), "{error}");
}

/// Changing an original source after ACK publication, even with a recomputed
/// per-row checksum, cannot yield a verified ACK. The acceptance marker's full
/// digest and the ACK's stored source digest both remain required.
#[test]
fn stored_acknowledgement_rejects_changed_original_source() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (_service, _ticket, write) = prepared(&mut store);
    let mut bytes = Vec::new();
    write.write_record(&mut bytes).unwrap();
    let key = ack_key(&bytes);
    let plan = store.acknowledgement_plan().unwrap();
    assert!(matches!(
        write.persist(&mut store.acknowledgement_port(&plan)),
        MessageAcknowledgementWriteOutcome::Committed(_)
    ));
    let chunk_key = "a1:0000000000000001:chunk:00000001";
    let mut changed = store.read(chunk_key).unwrap().unwrap();
    let index = changed.iter().position(|byte| *byte == b'o').unwrap();
    changed[index] = b'p';
    let checksum = Sha256::digest(&changed).to_vec();
    store
        .connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            rusqlite::params![changed, checksum, chunk_key],
        )
        .unwrap();
    let error = store
        .read_stored_acknowledgement(&key, decode_limits())
        .err()
        .unwrap();
    assert!(error.to_string().contains("digest changed"), "{error}");
}

/// A real cumulative ACK can exclude an unoffered expired earlier occurrence
/// and acknowledge a later offered one. The stored reader must preserve both
/// decisions and validate expiry against the original clock and endpoint offer.
#[test]
fn stored_acknowledgement_preserves_expiry_exclusion_and_later_ack() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let recipient = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&recipient.agent_id).unwrap();
    for (id, ttl_ms) in [("expired", Some(5)), ("live", None)] {
        let envelope = Envelope {
            protocol: "mmp/1",
            id: id.into(),
            message_type: "send".into(),
            time: "fixture".into(),
            sender: sender.clone(),
            recipient: Recipient::Agent(recipient.agent_id.clone()),
            correlation_id: None,
            ttl_ms,
            content_type: "text/plain".into(),
            payload: id.into(),
            extension_fields: vec![],
        };
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                envelope,
                MessageScope::Session,
                10,
                4096,
                None,
            )
            .unwrap();
        let plan = store.acceptance_plan(ticket.sequence()).unwrap();
        let write = service.begin_acceptance_write(&ticket).unwrap();
        let MessageAcceptanceWriteOutcome::Committed(commit) =
            write.persist(&mut store.acceptance_port(&plan))
        else {
            panic!("acceptance must commit")
        };
        service.publish_acceptance(&commit).unwrap();
    }
    let offered = service
        .receive_subscribed(&recipient.agent_id, 20, 10)
        .unwrap();
    assert_eq!(offered.messages.len(), 1);
    assert_eq!(offered.messages[0].sequence, 2);
    let ticket = service
        .prepare_acknowledgement(&recipient.agent_id, 2, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    let mut bytes = Vec::new();
    write.write_record(&mut bytes).unwrap();
    let key = ack_key(&bytes);
    let plan = store.acknowledgement_plan().unwrap();
    assert!(matches!(
        write.persist(&mut store.acknowledgement_port(&plan)),
        MessageAcknowledgementWriteOutcome::Committed(_)
    ));
    let decoded = store
        .read_stored_acknowledgement(&key, decode_limits())
        .unwrap();
    assert_eq!(decoded.observed_at_ms, 20);
    assert_eq!(decoded.decisions.len(), 2);
    assert_eq!(decoded.decisions[0].sequence, 1);
    assert!(!decoded.decisions[0].acknowledged);
    assert_eq!(decoded.decisions[1].sequence, 2);
    assert!(decoded.decisions[1].acknowledged);
    let history = store.fold_custody_history(decode_limits()).unwrap();
    assert_eq!(history.inventory.occurrences.len(), 2);
    assert_eq!(history.observed_next_sequence, Some(3));
    assert_eq!(
        history.acknowledgements,
        vec![super::super::history::FoldedAcknowledgement {
            sequence: 2,
            binding_index: 0
        }]
    );
    assert_eq!(
        history.expiries,
        vec![super::super::history::FoldedExpiry {
            sequence: 1,
            observed_at_ms: 20
        }]
    );

    // Recompute both row checksum and content key for a forged earlier offer
    // clock; the original first message was not yet expired at that instant.
    bytes[32..40].copy_from_slice(&10u64.to_le_bytes());
    let forged_key = ack_key(&bytes);
    let generation = store.acknowledgement_plan().unwrap().generation;
    store
        .commit(
            generation,
            &[CustodyWrite {
                key: &forged_key,
                previous: None,
                value: Some(&bytes),
            }],
        )
        .unwrap();
    let error = store
        .read_stored_acknowledgement(&forged_key, decode_limits())
        .err()
        .unwrap();
    assert!(error.to_string().contains("expiry exclusion"), "{error}");
    assert!(store.fold_custody_history(decode_limits()).is_err());
}

/// Two actual recipient ACKs carry the same expired prefix at different
/// endpoint clocks. Folding must keep separate exact acknowledged slots while
/// taking the occurrence-wide maximum expiry clock, independent of key order.
#[test]
fn history_folds_multiple_recipient_acks_and_maximum_expiry_clock() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let first = service.register_agent(None, None, "first", vec![]);
    let second = service.register_agent(None, None, "second", vec![]);
    service.subscribe(&first.agent_id).unwrap();
    service.subscribe(&second.agent_id).unwrap();
    for (id, ttl_ms) in [("expired", Some(5)), ("live", None)] {
        let envelope = Envelope {
            protocol: "mmp/1",
            id: id.into(),
            message_type: "send".into(),
            time: "fixture".into(),
            sender: sender.clone(),
            recipient: Recipient::Session,
            correlation_id: None,
            ttl_ms,
            content_type: "text/plain".into(),
            payload: id.into(),
            extension_fields: vec![],
        };
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                envelope,
                MessageScope::Session,
                10,
                4096,
                None,
            )
            .unwrap();
        let plan = store.acceptance_plan(ticket.sequence()).unwrap();
        let write = service.begin_acceptance_write(&ticket).unwrap();
        let MessageAcceptanceWriteOutcome::Committed(commit) =
            write.persist(&mut store.acceptance_port(&plan))
        else {
            panic!("acceptance must commit")
        };
        service.publish_acceptance(&commit).unwrap();
    }
    for (recipient, now) in [(&first.agent_id, 20), (&second.agent_id, 30)] {
        let offered = service.receive_subscribed(recipient, now, 10).unwrap();
        assert_eq!(offered.messages.len(), 1);
        assert_eq!(offered.messages[0].sequence, 2);
    }
    for recipient in [&first.agent_id, &second.agent_id] {
        let ticket = service
            .prepare_acknowledgement(recipient, 2, 512 * 1024)
            .unwrap()
            .unwrap();
        let write = service.begin_acknowledgement_write(&ticket).unwrap();
        let plan = store.acknowledgement_plan().unwrap();
        let MessageAcknowledgementWriteOutcome::Committed(commit) =
            write.persist(&mut store.acknowledgement_port(&plan))
        else {
            panic!("acknowledgement must commit")
        };
        service.publish_acknowledgement(&commit).unwrap();
    }
    let (source, _, _) = store.read_accepted_source(2, decode_limits()).unwrap();
    let mut slots = [&first.agent_id, &second.agent_id]
        .into_iter()
        .map(|id| {
            u32::try_from(
                source
                    .recipient_bindings()
                    .iter()
                    .position(|binding| binding.recipient == id.as_str())
                    .unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    slots.sort_unstable();
    drop(service);
    drop(store);
    let store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let history = store.fold_custody_history(decode_limits()).unwrap();
    assert_eq!(history.inventory.acknowledgement_keys.len(), 2);
    assert_eq!(history.observed_next_sequence, Some(3));
    assert_eq!(
        history
            .acknowledgements
            .iter()
            .map(|entry| (entry.sequence, entry.binding_index))
            .collect::<Vec<_>>(),
        slots.into_iter().map(|slot| (2, slot)).collect::<Vec<_>>()
    );
    assert_eq!(
        history.expiries,
        vec![super::super::history::FoldedExpiry {
            sequence: 1,
            observed_at_ms: 30
        }]
    );
}

/// A row whose content address and row checksum have both been recomputed must
/// still reject a forged source digest, rather than treating key integrity as
/// evidence that its referenced original occurrence was the same message.
#[test]
fn stored_acknowledgement_rejects_forged_source_digest() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (_service, _ticket, write) = prepared(&mut store);
    let mut bytes = Vec::new();
    write.write_record(&mut bytes).unwrap();
    let offset = 48 + write.recipient().as_str().len();
    bytes[offset + 13] ^= 0x80;
    let key = ack_key(&bytes);
    let generation = store.acknowledgement_plan().unwrap().generation;
    store
        .commit(
            generation,
            &[CustodyWrite {
                key: &key,
                previous: None,
                value: Some(&bytes),
            }],
        )
        .unwrap();
    let error = store
        .read_stored_acknowledgement(&key, decode_limits())
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("source digest changed"),
        "{error}"
    );
}

/// ACK and source validation share one physical SQLite read snapshot. A real
/// competing connection cannot commit a changed original between those reads,
/// and a transaction belonging to another store cannot qualify the operation.
#[test]
fn stored_acknowledgement_and_original_share_exact_store_snapshot() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (_service, _ticket, write) = prepared(&mut store);
    let mut bytes = Vec::new();
    write.write_record(&mut bytes).unwrap();
    let key = ack_key(&bytes);
    let plan = store.acknowledgement_plan().unwrap();
    assert!(matches!(
        write.persist(&mut store.acknowledgement_port(&plan)),
        MessageAcknowledgementWriteOutcome::Committed(_)
    ));
    let transaction = store.connection.unchecked_transaction().unwrap();
    assert!(store.read(&key).unwrap().is_some());
    let path = fixture.0.join("custody.sqlite");
    let writer = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .busy_timeout(std::time::Duration::from_millis(100))
            .unwrap();
        let result = connection.execute(
            "UPDATE records SET value=X'00' WHERE key=?1",
            ["a1:0000000000000001:chunk:00000000"],
        );
        matches!(result, Err(rusqlite::Error::SqliteFailure(error, _)) if matches!(error.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
    });
    assert!(
        writer.join().unwrap(),
        "competing write committed inside the snapshot"
    );
    let decoded = store
        .read_stored_acknowledgement_in_snapshot(&transaction, &key, decode_limits())
        .unwrap();
    assert_eq!(decoded.endpoint_sequence, 1);
    transaction.commit().unwrap();

    let other_fixture = Fixture::new();
    let other = MmpCustodyStore::open(&other_fixture.0, "another-session").unwrap();
    let foreign = other.connection.unchecked_transaction().unwrap();
    let error = store
        .read_stored_acknowledgement_in_snapshot(&foreign, &key, decode_limits())
        .err()
        .unwrap();
    assert!(error.to_string().contains("another store"), "{error}");
}

/// Owns one generated private directory, removed on normal/unwind cleanup.
struct Fixture(std::path::PathBuf);
impl Fixture {
    /// Allocates an initially absent generated store destination.
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "mez-mmp-ack-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }
}
impl Drop for Fixture {
    /// Removes only this fixture's generated private files.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Persists actual accepted source, adopts it into the sole lower owner, genuinely
/// offers it and prepares an unpublished ACK. No memory-only source qualifies.
fn prepared(
    store: &mut MmpCustodyStore,
) -> (
    MessageService,
    MessageAcknowledgementTicket,
    MessageAcknowledgementWrite,
) {
    let mut service = MessageService::with_limits(8, 1024 * 1024);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "original".into(),
        message_type: "send".into(),
        time: "original".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "original\0payload".repeat(20_000),
        extension_fields: vec![],
    };
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope,
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected source commit")
    };
    service.publish_acceptance(&commit).unwrap();
    service
        .receive_subscribed(&target.agent_id, 20, 10)
        .unwrap();
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    (service, ticket, write)
}

/// Reads actual SQLite generation/record count as an independent commit oracle.
fn state(store: &MmpCustodyStore) -> (i64, i64) {
    store
        .connection
        .query_row(
            "SELECT generation,(SELECT COUNT(*) FROM records) FROM state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

/// Actual ACK storage must precede lower cursor/slot adoption. The immutable
/// binary row contains metadata only, fits one bounded transaction and preserves
/// every accepted source row until later explicit compaction, not ACK side effects.
#[test]
fn acknowledgement_commits_one_bounded_record_before_original_cursor_adoption() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (mut service, _ticket, write) = prepared(&mut store);
    let recipient = write.recipient().clone();
    let mut bytes = Vec::new();
    write.write_record(&mut bytes).unwrap();
    assert_eq!(bytes.len(), write.encoded_bytes());
    assert!(bytes.len() < MAX_BATCH_BYTES);
    assert!(bytes.starts_with(b"MMPACK01"));
    assert!(!bytes.windows(8).any(|bytes| bytes == b"original"));
    let plan = store.acknowledgement_plan().unwrap();
    let before = state(&store);
    let MessageAcknowledgementWriteOutcome::Committed(commit) =
        write.persist(&mut store.acknowledgement_port(&plan))
    else {
        panic!("expected ACK commit")
    };
    assert_eq!(state(&store), (before.0 + 1, before.1 + 1));
    assert_eq!(service.subscription(&recipient).unwrap().last_sequence, 0);
    assert_eq!(store.read(&ack_key(&bytes)).unwrap().unwrap(), bytes);
    assert_eq!(
        service
            .publish_acknowledgement(&commit)
            .unwrap()
            .last_sequence,
        1
    );
    assert_eq!(service.subscription(&recipient).unwrap().last_sequence, 1);
    assert_eq!(
        service
            .publish_acknowledgement(&commit)
            .unwrap()
            .last_sequence,
        1
    );
}

/// A real worker commits the ACK and panics before returning its receipt. Only
/// the actual observed join qualifies original-input recovery; reopened storage
/// reconciles identical bytes and original sources without another generation.
#[test]
fn actual_postcommit_worker_loss_reconciles_same_ack_after_reopen_once() {
    /// Qualifies exactly the worker escrow after its real thread join completed.
    struct Joined {
        attempt: MessageAcknowledgementAttempt,
        observed: bool,
    }
    impl MessageAcknowledgementJoin for Joined {
        /// Never treats a generic error or matching recipient ID as join evidence.
        fn qualify_joined(
            &mut self,
            attempt: &MessageAcknowledgementAttempt,
        ) -> mez_agent::messaging::Result<()> {
            if self.observed && self.attempt.same_attempt(attempt) {
                Ok(())
            } else {
                Err(MessageError::invalid_state(
                    "actual original ACK worker not joined",
                ))
            }
        }
    }
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (mut service, _ticket, write) = prepared(&mut store);
    let attempt = write.attempt();
    let plan = store.acknowledgement_plan().unwrap();
    let worker_plan = plan.clone();
    let worker = std::thread::spawn(move || {
        assert!(matches!(
            write.persist(&mut store.acknowledgement_port(&worker_plan)),
            MessageAcknowledgementWriteOutcome::Committed(_)
        ));
        panic!("injected loss after actual ACK commit");
    });
    let observed = worker.join().is_err();
    assert!(observed);
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let before = state(&store);
    let mut joined = Joined {
        attempt: attempt.clone(),
        observed,
    };
    let write = service
        .recover_joined_acknowledgement_write(&attempt, &mut joined)
        .unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) =
        write.persist(&mut store.acknowledgement_port(&plan))
    else {
        panic!("expected exact reconciliation")
    };
    assert_eq!(state(&store), before);
    service.publish_acknowledgement(&commit).unwrap();
}

/// A precommit SQLite refusal leaves both ACK cursor and its input unresolved.
/// Removing the real fault lets the same original bytes commit after reopening;
/// error text never becomes an ACK or permission to repeat native input effects.
#[test]
fn real_ack_write_failure_retains_original_until_storage_recovers() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (mut service, ticket, write) = prepared(&mut store);
    let recipient = write.recipient().clone();
    let plan = store.acknowledgement_plan().unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_ack BEFORE INSERT ON records WHEN NEW.key LIKE 'k1:%' BEGIN SELECT RAISE(ABORT,'fixture ACK failure'); END;").unwrap();
    let before = state(&store);
    let MessageAcknowledgementWriteOutcome::Unknown(unknown) =
        write.persist(&mut store.acknowledgement_port(&plan))
    else {
        panic!("expected uncertain store failure")
    };
    assert_eq!(state(&store), before);
    assert_eq!(service.subscription(&recipient).unwrap().last_sequence, 0);
    assert!(service.cancel_acknowledgement(&ticket).is_err());
    drop(store);
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    store
        .connection
        .execute_batch("DROP TRIGGER fail_ack")
        .unwrap();
    let write = service.retry_acknowledgement_write(unknown).unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) =
        write.persist(&mut store.acknowledgement_port(&plan))
    else {
        panic!("expected recovered commit")
    };
    assert_eq!(state(&store), (before.0 + 1, before.1 + 1));
    service.publish_acknowledgement(&commit).unwrap();
}

/// An existing saved ACK cannot excuse missing/corrupt accepted-source chunks or
/// a corrupt actual ACK record. Every reconciliation must fail without rebuilding
/// source custody, repairing postimages or advancing another store generation.
#[test]
fn damaged_ack_or_original_payload_never_qualifies_saved_commit_label() {
    for source in [false, true] {
        let fixture = Fixture::new();
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let (_service, _ticket, write) = prepared(&mut store);
        let plan = store.acknowledgement_plan().unwrap();
        let mut bytes = Vec::new();
        write.write_record(&mut bytes).unwrap();
        store.acknowledgement_port(&plan).commit(&write).unwrap();
        let key = if source {
            "a1:0000000000000001:chunk:00000000".to_string()
        } else {
            ack_key(&bytes)
        };
        let mut changed = store.read(&key).unwrap().unwrap();
        changed[0] ^= 1;
        let digest = Sha256::digest(&changed);
        store
            .connection
            .execute(
                "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
                params![&changed, digest.as_slice(), &key],
            )
            .unwrap();
        let before = state(&store);
        assert!(matches!(
            write.persist(&mut store.acknowledgement_port(&plan)),
            MessageAcknowledgementWriteOutcome::Unknown(_)
        ));
        assert_eq!(state(&store), before);
        assert_eq!(store.read(&key).unwrap().unwrap(), changed);
    }
}

/// Equal session labels cannot supply a different store allocation. Future source
/// framing, even with a valid generic row checksum, cannot qualify ACK acceptance
/// or make an unknown record format silently behave like the current version.
#[test]
fn foreign_store_and_future_original_record_are_rejected_before_ack_write() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (_service, _ticket, write) = prepared(&mut store);
    let plan = store.acknowledgement_plan().unwrap();
    let foreign_fixture = Fixture::new();
    let mut foreign = MmpCustodyStore::open(&foreign_fixture.0, "session").unwrap();
    assert!(foreign.acknowledgement_port(&plan).commit(&write).is_err());
    assert_eq!(state(&foreign), (0, 0));
    let key = "a1:0000000000000001:commit";
    let mut header: serde_json::Value =
        serde_json::from_slice(&store.read(key).unwrap().unwrap()).unwrap();
    header["record_version"] = serde_json::json!(2);
    let bytes = serde_json::to_vec(&header).unwrap();
    let digest = Sha256::digest(&bytes);
    store
        .connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            params![&bytes, digest.as_slice(), key],
        )
        .unwrap();
    let before = state(&store);
    assert!(matches!(
        write.persist(&mut store.acknowledgement_port(&plan)),
        MessageAcknowledgementWriteOutcome::Unknown(_)
    ));
    assert_eq!(state(&store), before);
}

/// A lower memory-only acceptance is insufficient for a durable ACK claim. The
/// product port refuses it before adding any ACK record, preserving both cursor
/// and owned uncertainty instead of concealing an unpersisted ingress path.
#[test]
fn memory_only_source_cannot_qualify_durable_acknowledgement() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "memory-only".into(),
        message_type: "send".into(),
        time: "fixture".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "original".into(),
        extension_fields: vec![],
    };
    let sequence = service
        .accept_at_with_scope(&sender.agent_id, envelope, MessageScope::Session, 1)
        .unwrap()
        .sequence;
    service.receive_subscribed(&target.agent_id, 2, 1).unwrap();
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, sequence, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    let plan = store.acknowledgement_plan().unwrap();
    assert!(matches!(
        write.persist(&mut store.acknowledgement_port(&plan)),
        MessageAcknowledgementWriteOutcome::Unknown(_)
    ));
    assert_eq!(state(&store), (0, 0));
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
}

/// Matching addresses, floors, occurrence ID and sequence cannot substitute a
/// different payload for the durable original. Worker-computed fingerprints bind
/// the complete frozen source without hashing/copying payload on the actor.
#[test]
fn lookalike_memory_source_with_changed_payload_cannot_ack_different_durable_original() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (service, _ticket, _original) = prepared(&mut store);
    let mut snapshot = service.snapshot_state();
    snapshot.retained_messages[0].envelope.payload =
        "different content with identical public identity".into();
    snapshot.accepted_messages[0].envelope.payload =
        snapshot.retained_messages[0].envelope.payload.clone();
    let mut foreign = MessageService::from_snapshot_state(&snapshot).unwrap();
    let recipient =
        mez_core::ids::AgentId::opaque(snapshot.subscriptions[0].recipient.clone()).unwrap();
    let ticket = foreign
        .prepare_acknowledgement(&recipient, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = foreign.begin_acknowledgement_write(&ticket).unwrap();
    let plan = store.acknowledgement_plan().unwrap();
    let before = state(&store);
    assert!(matches!(
        write.persist(&mut store.acknowledgement_port(&plan)),
        MessageAcknowledgementWriteOutcome::Unknown(_)
    ));
    assert_eq!(state(&store), before);
    assert_eq!(foreign.subscription(&recipient).unwrap().last_sequence, 0);
}
