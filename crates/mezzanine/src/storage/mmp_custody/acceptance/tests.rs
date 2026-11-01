//! Real SQLite acceptance-port qualification with generated private stores.
//! These tests compose the production port with actual lower linear writes;
//! test-only full encoded buffers are independent comparison oracles.

use super::*;
use mez_agent::messaging::{
    Envelope, MessageAcceptanceAttempt, MessageAcceptanceJoin, MessageAcceptanceWriteOutcome,
    MessageScope, MessageService, Recipient,
};

/// Owns only one generated private directory, removed on normal/unwind cleanup.
struct Fixture(std::path::PathBuf);
impl Fixture {
    /// Allocates an initially absent private store destination.
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "mez-mmp-stream-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }
}
impl Drop for Fixture {
    /// Removes only this fixture's generated directory.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Creates a real unpublished original with enough existing payload allowance.
fn original(
    payload: String,
) -> (
    MessageService,
    mez_agent::messaging::MessageAcceptanceTicket,
    MessageAcceptanceWrite,
) {
    let mut service = MessageService::with_limits(16, 4 * 1024 * 1024);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "original".into(),
        message_type: "send".into(),
        time: "original-time".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id),
        correlation_id: None,
        ttl_ms: Some(1234),
        content_type: "text/plain".into(),
        payload,
        extension_fields: vec![("frozen".into(), "{\"order\":7}".into())],
    };
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope,
            MessageScope::Session,
            77,
            4096,
            None,
        )
        .unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    (service, ticket, write)
}

/// Reads a complete test-only oracle while checking each production chunk's
/// individual bound. Production never collects these buffers into one Vec.
fn stored_bytes(store: &MmpCustodyStore, sequence: u64) -> Vec<u8> {
    let prefix = format!("a1:{sequence:016x}:");
    let marker = store.read(&format!("{prefix}commit")).unwrap().unwrap();
    let header: serde_json::Value = serde_json::from_slice(&marker).unwrap();
    let mut bytes = Vec::new();
    for index in 0..header["chunks"].as_u64().unwrap() as usize {
        let chunk = store.read(&chunk_key(&prefix, index)).unwrap().unwrap();
        assert!(chunk.len() <= CHUNK_BYTES);
        bytes.extend_from_slice(&chunk);
    }
    assert_eq!(
        bytes.len(),
        header["encoded_bytes"].as_u64().unwrap() as usize
    );
    bytes
}

/// Escaping expands a valid one-MiB payload far beyond one256KiB transaction.
/// The real port still preserves every original byte, TTL and frozen identity,
/// uses bounded rows, and only its completed marker qualifies lower publication.
#[test]
fn large_escaped_original_streams_to_bounded_transactions_before_publication() {
    let fixture = Fixture::new();
    let payload = "\0\"\n\\".repeat(256 * 1024);
    let (mut service, ticket, write) = original(payload);
    let expected = serde_json::to_vec(&write).unwrap();
    assert!(expected.len() > MAX_BATCH_BYTES * 4);
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected commit")
    };
    assert!(service.has_unpublished_acceptance());
    assert_eq!(stored_bytes(&store, ticket.sequence()), expected);
    let maximum: i64 = store
        .connection
        .query_row("SELECT MAX(length(value)) FROM records", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(maximum <= CHUNK_BYTES as i64);
    let generation: i64 = store
        .connection
        .query_row("SELECT generation FROM state", [], |row| row.get(0))
        .unwrap();
    assert!(generation > 4);
    assert_eq!(
        service.publish_acceptance(&commit).unwrap().0.sequence,
        ticket.sequence()
    );
    drop(store);
    let reopened = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    assert_eq!(stored_bytes(&reopened, ticket.sequence()), expected);
}

/// Aborting the final marker leaves a real dense private chunk prefix, no lower
/// acceptance, and an ambiguous linear original. Reopening and retrying the same
/// retained plan verifies existing bytes and publishes exactly one final marker.
#[test]
fn partial_original_after_marker_failure_reconciles_same_plan_after_reopen() {
    let fixture = Fixture::new();
    let (mut service, ticket, write) = original("original\n".repeat(40_000));
    let expected = serde_json::to_vec(&write).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'injected marker failure'); END;").unwrap();
    let MessageAcceptanceWriteOutcome::Unknown(unknown) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected ambiguous failure")
    };
    assert!(service.has_unpublished_acceptance());
    assert!(service.cancel_acceptance(&ticket).is_err());
    assert!(
        store
            .read(&format!("a1:{:016x}:commit", ticket.sequence()))
            .unwrap()
            .is_none()
    );
    let before: i64 = store
        .connection
        .query_row("SELECT generation FROM state", [], |row| row.get(0))
        .unwrap();
    assert!(before > 2);
    drop(store);
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    store
        .connection
        .execute_batch("DROP TRIGGER fail_marker")
        .unwrap();
    let write = service.retry_acceptance_write(unknown).unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected reconciled commit")
    };
    let after: i64 = store
        .connection
        .query_row("SELECT generation FROM state", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after, before + 1);
    assert_eq!(stored_bytes(&store, ticket.sequence()), expected);
    assert!(service.publish_acceptance(&commit).unwrap().0.accepted);
}

/// A positive saved marker cannot justify recreating missing or corrupt payload
/// chunks. Retry must fail closed without changing generation or repairing the
/// actual damaged postimage, even though the original full commit did succeed.
#[test]
fn committed_marker_never_reconstructs_deleted_or_corrupt_chunks() {
    for corrupt in [false, true] {
        let fixture = Fixture::new();
        let (_service, ticket, write) = original("original".repeat(40_000));
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let plan = store.acceptance_plan(ticket.sequence()).unwrap();
        store.acceptance_port(&plan).commit(&write).unwrap();
        let key = chunk_key(&format!("a1:{:016x}:", ticket.sequence()), 0);
        if corrupt {
            store
                .connection
                .execute("UPDATE records SET value=X'00' WHERE key=?1", [&key])
                .unwrap();
        } else {
            store
                .connection
                .execute("DELETE FROM records WHERE key=?1", [&key])
                .unwrap();
        }
        let before: i64 = store
            .connection
            .query_row("SELECT generation FROM state", [], |row| row.get(0))
            .unwrap();
        assert!(store.acceptance_port(&plan).commit(&write).is_err());
        let after: i64 = store
            .connection
            .query_row("SELECT generation FROM state", [], |row| row.get(0))
            .unwrap();
        assert_eq!(before, after);
        if corrupt {
            assert!(store.read(&key).is_err());
        } else {
            assert!(store.read(&key).unwrap().is_none());
        }
    }
}

/// Full encoded row count and logical costs are checked before the first stage
/// transaction. Tiny policies cannot retain an orphan prefix on known capacity
/// failure, and the lower issued original remains explicitly unsettled.
#[test]
fn complete_record_capacity_is_checked_before_first_private_write() {
    for limits in [
        CustodyStoreLimits {
            records: 3,
            ..CustodyStoreLimits::default()
        },
        CustodyStoreLimits {
            logical_bytes: 4096,
            ..CustodyStoreLimits::default()
        },
    ] {
        let fixture = Fixture::new();
        let (service, ticket, write) = original("payload".repeat(40_000));
        let mut store = MmpCustodyStore::open_with_limits(&fixture.0, "session", limits).unwrap();
        let plan = store.acceptance_plan(ticket.sequence()).unwrap();
        assert!(matches!(
            write.persist(&mut store.acceptance_port(&plan)),
            MessageAcceptanceWriteOutcome::Unknown(_)
        ));
        assert!(service.has_unpublished_acceptance());
        let state: (i64, i64) = store
            .connection
            .query_row(
                "SELECT generation,(SELECT COUNT(*) FROM records) FROM state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, (0, 0));
    }
}

/// Equal session labels and sequence numbers do not qualify a replacement store
/// allocation or changed original content. Neither refusal may alter existing
/// committed records, and a superseded plan cannot ignore unrelated store work.
#[test]
fn foreign_store_changed_input_and_interleaved_generation_are_rejected() {
    let first = Fixture::new();
    let second = Fixture::new();
    let (_service, ticket, write) = original("first".into());
    let mut store = MmpCustodyStore::open(&first.0, "same-session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    let mut foreign = MmpCustodyStore::open(&second.0, "same-session").unwrap();
    assert!(foreign.acceptance_port(&plan).commit(&write).is_err());
    store.acceptance_port(&plan).commit(&write).unwrap();
    let original_bytes = stored_bytes(&store, ticket.sequence());
    let (_other, _other_ticket, changed) = original("changed".into());
    assert!(store.acceptance_port(&plan).commit(&changed).is_err());
    assert_eq!(stored_bytes(&store, ticket.sequence()), original_bytes);
    let generation: i64 = store
        .connection
        .query_row("SELECT generation FROM state", [], |row| row.get(0))
        .unwrap();
    store
        .commit(
            generation as u64,
            &[CustodyWrite {
                key: "unrelated",
                previous: None,
                value: Some(b"later"),
            }],
        )
        .unwrap();
    assert!(store.acceptance_port(&plan).commit(&write).is_err());
    assert_eq!(stored_bytes(&store, ticket.sequence()), original_bytes);
}

/// The actual worker commits every chunk/marker and then panics before returning
/// its opaque completion. A real observed join qualifies retry of the retained
/// original plan after reopen, with no extra disk generation or lower send.
#[test]
fn actual_worker_loss_after_marker_reconciles_original_once_without_new_disk_write() {
    /// Holds the exact escrow and actual join observation from this test's worker.
    struct Joined {
        expected: MessageAcceptanceAttempt,
        observed: bool,
    }
    impl MessageAcceptanceJoin for Joined {
        /// Checks the original attempt only after the owned thread was joined.
        fn qualify_joined(
            &mut self,
            attempt: &MessageAcceptanceAttempt,
        ) -> mez_agent::messaging::Result<()> {
            if self.observed && self.expected.same_attempt(attempt) {
                Ok(())
            } else {
                Err(MessageError::invalid_state("not the actual joined worker"))
            }
        }
    }
    let fixture = Fixture::new();
    let (mut service, ticket, write) = original("original\0".repeat(40_000));
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    let worker_plan = plan.clone();
    let attempt = service.acceptance_write_attempt(&ticket).unwrap();
    let worker = std::thread::spawn(move || {
        assert!(matches!(
            write.persist(&mut store.acceptance_port(&worker_plan)),
            MessageAcceptanceWriteOutcome::Committed(_)
        ));
        panic!("injected actual lost completion after marker commit");
    });
    let observed = worker.join().is_err();
    assert!(observed);
    assert!(service.has_unpublished_acceptance());
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let before: i64 = store
        .connection
        .query_row("SELECT generation FROM state", [], |row| row.get(0))
        .unwrap();
    let mut joined = Joined {
        expected: attempt.clone(),
        observed,
    };
    let write = service
        .recover_joined_acceptance_write(&attempt, &mut joined)
        .unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected reconciled commit")
    };
    let after: i64 = store
        .connection
        .query_row("SELECT generation FROM state", [], |row| row.get(0))
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        service.publish_acceptance(&commit).unwrap().0.sequence,
        ticket.sequence()
    );
    assert_eq!(
        service.publish_acceptance(&commit).unwrap().0.sequence,
        ticket.sequence()
    );
}

/// Measured actual SQLite logical row charges provide an independent boundary
/// oracle: an identical original fits the exact observed budget and one byte less
/// rejects before its first stage write. No mirrored size formula is used here.
#[test]
fn exact_observed_encoded_budget_fits_and_one_byte_less_writes_nothing() {
    let fixture = Fixture::new();
    let (_service, ticket, write) = original("bounded\0".repeat(10_000));
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.acceptance_port(&plan).commit(&write).unwrap();
    let (records,bytes):(i64,i64)=store.connection.query_row("SELECT COUNT(*),SUM(length(CAST(key AS BLOB))+length(value)+length(digest)+64) FROM records",[],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
    for delta in [0, 1] {
        let target = Fixture::new();
        let limits = CustodyStoreLimits {
            records: records as usize,
            logical_bytes: bytes as usize - delta,
            ..CustodyStoreLimits::default()
        };
        let mut target = MmpCustodyStore::open_with_limits(&target.0, "session", limits).unwrap();
        let plan = target.acceptance_plan(ticket.sequence()).unwrap();
        let result = target.acceptance_port(&plan).commit(&write);
        if delta == 0 {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err());
            let rows: i64 = target
                .connection
                .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
                .unwrap();
            assert_eq!(rows, 0);
        }
    }
}

/// A plan near SQLite's generation maximum must reserve room for its entire
/// stage/chunk/marker sequence. Exhaustion cannot strand a newly written prefix
/// that could never reach a qualifying final marker.
#[test]
fn generation_headroom_is_reserved_before_staging_any_original() {
    let fixture = Fixture::new();
    let (_service, ticket, write) = original("short".into());
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    store
        .connection
        .execute("UPDATE state SET generation=?1", [i64::MAX - 1])
        .unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    assert!(store.acceptance_port(&plan).commit(&write).is_err());
    let rows: i64 = store
        .connection
        .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}
