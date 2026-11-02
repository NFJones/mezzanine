//! Real SQLite acceptance-port qualification with generated private stores.
//! These tests compose the production port with actual lower linear writes;
//! test-only full encoded buffers are independent comparison oracles.

use super::*;
use mez_agent::messaging::{
    MessageAcceptanceAttempt, MessageAcceptanceJoin, MessageAcceptanceWriteOutcome,
};

use super::test_fixture::{Fixture, original, stored_bytes};

/// Supplies the lower owner's current admission ceilings to the test decoder.
fn decode_limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 16 * 1024 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}

/// A reopened SQLite store supplies a real, heavily escaped accepted original
/// directly to the bounded typed decoder. The candidate emerges only after the
/// full original marker, chunks and digest verify; no delivery is installed.
#[test]
fn verified_store_decodes_exact_original_with_bounded_payload() {
    let fixture = Fixture::new();
    let payload = "\0\"\n\\".repeat(256 * 1024);
    let (_service, ticket, write) = original(payload.clone());
    let expected = serde_json::to_vec(&write).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    assert!(matches!(
        write.persist(&mut store.acceptance_port(&plan)),
        MessageAcceptanceWriteOutcome::Committed(_)
    ));
    drop(store);
    let store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (decoded, digest, bytes) = store
        .read_accepted_source(ticket.sequence(), decode_limits())
        .unwrap();
    assert_eq!(decoded.sequence(), ticket.sequence());
    assert_eq!(decoded.envelope().payload, payload);
    assert_eq!(bytes, expected.len());
    assert_eq!(digest, <[u8; 32]>::from(Sha256::digest(&expected)));
    assert!(!decoded.recipient_bindings().is_empty());
    let mut narrowed = decode_limits();
    narrowed.payload_bytes -= 1;
    assert!(
        store
            .read_accepted_source(ticket.sequence(), narrowed)
            .is_err()
    );
}

/// Even well-framed decoded bytes do not escape when a later source chunk has
/// changed: the final original digest must agree before the typed value returns.
#[test]
fn typed_source_is_withheld_when_committed_digest_changes() {
    let fixture = Fixture::new();
    let (_service, ticket, write) = original("original".repeat(40_000));
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.acceptance_port(&plan).commit(&write).unwrap();
    let key = chunk_key(&format!("a1:{:016x}:", ticket.sequence()), 1);
    let mut changed = store.read(&key).unwrap().unwrap();
    let position = changed.iter().position(|byte| *byte == b'o').unwrap();
    changed[position] = b'p';
    let checksum = Sha256::digest(&changed).to_vec();
    store
        .connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            rusqlite::params![changed, checksum, key],
        )
        .unwrap();
    let error = store
        .read_accepted_source(ticket.sequence(), decode_limits())
        .err()
        .unwrap();
    assert!(error.to_string().contains("digest changed"), "{error}");
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

/// A reopened store must provide the exact committed original as a bounded
/// stream, including escaped payload and frozen metadata. The sink observes at
/// most one storage chunk at a time and receives no independent authority.
#[test]
fn committed_original_streams_from_reopened_store_without_a_full_record_buffer() {
    /// Retains only a digest and byte counts, never the encoded source itself.
    struct MeasuringSink {
        hash: Sha256,
        bytes: usize,
        largest_write: usize,
    }
    impl Write for MeasuringSink {
        /// Hashes each borrowed chunk and records its largest actual write.
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.hash.update(bytes);
            self.bytes += bytes.len();
            self.largest_write = self.largest_write.max(bytes.len());
            Ok(bytes.len())
        }
        /// This sink has no buffered output.
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let fixture = Fixture::new();
    let (_service, ticket, write) = original("\0\"\n\\".repeat(256 * 1024));
    let expected = serde_json::to_vec(&write).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    assert!(matches!(
        write.persist(&mut store.acceptance_port(&plan)),
        MessageAcceptanceWriteOutcome::Committed(_)
    ));
    drop(store);
    let store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut sink = MeasuringSink {
        hash: Sha256::new(),
        bytes: 0,
        largest_write: 0,
    };
    let (digest, bytes) = store
        .stream_accepted_occurrence(ticket.sequence(), &mut sink)
        .unwrap();
    assert_eq!(bytes, expected.len());
    assert_eq!(sink.bytes, expected.len());
    assert!(sink.largest_write <= CHUNK_BYTES);
    assert_eq!(digest, <[u8; 32]>::from(Sha256::digest(&expected)));
    assert_eq!(digest, <[u8; 32]>::from(sink.hash.finalize()));
}

/// A committed marker cannot validate a changed later chunk, even if its row
/// checksum was recomputed. A partial sink prefix must be discarded by the
/// caller; a missing original must fail before emitting any bytes at all.
#[test]
fn acceptance_stream_rejects_missing_or_changed_original_without_certifying_prefix() {
    let fixture = Fixture::new();
    let (_service, ticket, write) = original("original".repeat(40_000));
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.acceptance_port(&plan).commit(&write).unwrap();
    let key = chunk_key(&format!("a1:{:016x}:", ticket.sequence()), 1);
    let mut changed = store.read(&key).unwrap().unwrap();
    changed[0] ^= 1;
    let checksum = Sha256::digest(&changed).to_vec();
    store
        .connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            rusqlite::params![changed, checksum, key],
        )
        .unwrap();
    let mut quarantined = Vec::new();
    assert!(
        store
            .stream_accepted_occurrence(ticket.sequence(), &mut quarantined)
            .is_err()
    );
    assert!(!quarantined.is_empty());
    let decoded = store.with_verified_accepted_occurrence(ticket.sequence(), |reader| {
        serde_json::from_reader::<_, serde_json::Value>(reader)
            .map_err(|error| MezError::invalid_state(error.to_string()))
    });
    assert!(
        decoded.is_err(),
        "a decodable changed body is not a verified source"
    );
    let mut missing = Vec::new();
    assert!(
        store
            .stream_accepted_occurrence(ticket.sequence() + 1, &mut missing)
            .is_err()
    );
    assert!(missing.is_empty());
}

/// A failing sink must return a contextual error without changing the store or
/// consuming its source. A later reader can still verify the exact original.
#[test]
fn acceptance_stream_sink_failure_preserves_original_for_retry() {
    /// Fails the first actual write, independently of the SQLite reader.
    struct FailingSink;
    impl Write for FailingSink {
        /// Reports an actual sink error without accepting any bytes.
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("injected sink failure"))
        }
        /// This sink has no buffered output.
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let fixture = Fixture::new();
    let (_service, ticket, write) = original("original".repeat(40_000));
    let expected = serde_json::to_vec(&write).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.acceptance_port(&plan).commit(&write).unwrap();
    let before: i64 = store
        .connection
        .query_row("SELECT generation FROM state", [], |row| row.get(0))
        .unwrap();
    let error = store
        .stream_accepted_occurrence(ticket.sequence(), &mut FailingSink)
        .unwrap_err();
    assert!(error.to_string().contains("injected sink failure"));
    let mut recovered = Vec::new();
    let (_, length) = store
        .stream_accepted_occurrence(ticket.sequence(), &mut recovered)
        .unwrap();
    assert_eq!(length, expected.len());
    assert_eq!(recovered, expected);
    let after: i64 = store
        .connection
        .query_row("SELECT generation FROM state", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after, before);
}

/// A concurrent connection cannot commit a changed chunk between source reads.
/// The actual reader holds one SQLite snapshot through the final digest check,
/// so the sink cannot receive a mixture of two committed store revisions.
#[test]
fn acceptance_stream_holds_one_snapshot_against_concurrent_write() {
    /// Coordinates the other connection after the first source chunk is read.
    struct CoordinatingSink {
        started: Option<std::sync::mpsc::Sender<()>>,
        completed: std::sync::mpsc::Receiver<bool>,
        bytes: usize,
    }
    impl Write for CoordinatingSink {
        /// Waits only for the competing transaction's bounded result once.
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(started) = self.started.take() {
                started.send(()).unwrap();
                assert!(
                    self.completed
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap(),
                    "concurrent writer committed during acceptance read"
                );
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        /// This sink has no buffered output.
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let fixture = Fixture::new();
    let (_service, ticket, write) = original("original".repeat(40_000));
    let expected_len = serde_json::to_vec(&write).unwrap().len();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.acceptance_port(&plan).commit(&write).unwrap();
    let path = fixture.0.join("custody.sqlite");
    let key = chunk_key(&format!("a1:{:016x}:", ticket.sequence()), 1);
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .busy_timeout(std::time::Duration::from_millis(100))
            .unwrap();
        start_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let result = connection.execute("UPDATE records SET value=X'00' WHERE key=?1", [&key]);
        let locked = matches!(
            result,
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(error.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
        );
        done_tx.send(locked).unwrap();
    });
    let mut sink = CoordinatingSink {
        started: Some(start_tx),
        completed: done_rx,
        bytes: 0,
    };
    let (_, bytes) = store
        .stream_accepted_occurrence(ticket.sequence(), &mut sink)
        .unwrap();
    writer.join().unwrap();
    assert_eq!(bytes, expected_len);
    assert_eq!(sink.bytes, expected_len);
}

/// A decoder may consume the original directly without staging a second full
/// encoded copy, but its result is returned only after complete source checks.
/// An early successful callback cannot qualify an unread suffix as verified.
#[test]
fn acceptance_decoder_requires_complete_verified_input() {
    let fixture = Fixture::new();
    let (_service, ticket, write) = original("original".repeat(40_000));
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.acceptance_port(&plan).commit(&write).unwrap();
    let (decoded, _, _) = store
        .with_verified_accepted_occurrence(ticket.sequence(), |reader| {
            serde_json::from_reader::<_, serde_json::Value>(reader)
                .map_err(|error| MezError::invalid_state(error.to_string()))
        })
        .unwrap();
    assert_eq!(decoded["sequence"], ticket.sequence());
    assert_eq!(decoded["envelope"]["id"], "original");
    assert_eq!(
        decoded["envelope"]["payload"].as_str().unwrap().len(),
        320_000
    );
    let error = store
        .with_verified_accepted_occurrence(ticket.sequence(), |reader| {
            let mut first = [0; 1];
            reader.read_exact(&mut first).map_err(MezError::from)?;
            Ok(first)
        })
        .unwrap_err();
    assert!(error.to_string().contains("did not consume"));
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
