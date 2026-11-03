//! Actual SQLite interruption and corruption controls for restart inspection.

use super::super::test_fixture::{Fixture, original};
use super::*;
use mez_agent::messaging::MessageAcceptanceWriteOutcome;

/// Creates an actual interrupted source, using a SQLite trigger to fail before
/// a selected chunk insertion or the final commit marker transaction.
fn interrupted(fixture: &Fixture, trigger: &str) -> (u64, Header) {
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (_service, ticket, write) = original("x".repeat(150_000));
    let (header, _) = record_header(&write, store.limits.logical_bytes).unwrap();
    let sequence = ticket.sequence();
    let plan = store.acceptance_plan(sequence).unwrap();
    store.connection.execute_batch(trigger).unwrap();
    assert!(matches!(
        write.persist(&mut store.acceptance_port(&plan)),
        MessageAcceptanceWriteOutcome::Unknown(_)
    ));
    (sequence, header)
}

/// Reopening after an actual interrupted physical append preserves only the
/// checked dense prefix and exact last-batch evidence. Inspection itself does
/// not alter any rows or generation, even when all source chunks were written.
#[test]
fn reopened_stages_report_only_verified_local_prefix_evidence() {
    for (trigger, expected_chunks) in [
        (
            "CREATE TRIGGER stop_chunk BEFORE INSERT ON records WHEN NEW.key LIKE '%:chunk:%' BEGIN SELECT RAISE(ABORT,'stop'); END;",
            0,
        ),
        (
            "CREATE TRIGGER stop_chunk BEFORE INSERT ON records WHEN NEW.key LIKE '%:chunk:00000002' BEGIN SELECT RAISE(ABORT,'stop'); END;",
            2,
        ),
        (
            "CREATE TRIGGER stop_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'stop'); END;",
            3,
        ),
    ] {
        let fixture = Fixture::new();
        let (sequence, header) = interrupted(&fixture, trigger);
        let store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let before: (i64, i64) = store
            .connection
            .query_row(
                "SELECT generation,(SELECT COUNT(*) FROM records) FROM state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let evidence = store.inspect_unfinished_acceptance(sequence).unwrap();
        assert_eq!(evidence.store_id, store.store_id);
        assert_eq!(evidence.generation, before.0 as u64);
        assert_eq!(evidence.sequence, sequence);
        assert_eq!(evidence.encoded_bytes, header.encoded_bytes);
        assert_eq!(evidence.source_digest, header.digest);
        assert_eq!(evidence.total_chunks, header.chunks);
        assert_eq!(evidence.present_chunks, expected_chunks);
        let after: (i64, i64) = store
            .connection
            .query_row(
                "SELECT generation,(SELECT COUNT(*) FROM records) FROM state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(before, after);
    }
}

/// A committed marker is never described as an unfinished stage. If that real
/// marker is subsequently deleted without changing the generation or last
/// batch, its surviving marker-batch evidence still prevents misclassification.
#[test]
fn deleted_committed_marker_does_not_become_unfinished_stage() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let (_service, ticket, write) = original("x".repeat(150_000));
    let sequence = ticket.sequence();
    let plan = store.acceptance_plan(sequence).unwrap();
    assert!(matches!(
        write.persist(&mut store.acceptance_port(&plan)),
        MessageAcceptanceWriteOutcome::Committed(_)
    ));
    assert!(store.inspect_unfinished_acceptance(sequence).is_err());
    let marker = format!("a1:{sequence:016x}:commit");
    assert_eq!(
        store
            .connection
            .execute("DELETE FROM records WHERE key=?1", [&marker])
            .unwrap(),
        1
    );
    assert!(store.inspect_unfinished_acceptance(sequence).is_err());
    assert!(
        store
            .read(&format!("a1:{sequence:016x}:stage"))
            .unwrap()
            .is_some()
    );
}

/// Missing or corrupt prefix rows and unrelated last-batch state are errors,
/// never partial evidence that a later cleanup could treat as a valid source.
#[test]
fn unfinished_inspection_rejects_holes_corruption_and_changed_batch() {
    for fault in ["hole", "corrupt", "batch"] {
        let fixture = Fixture::new();
        let (sequence, _) = interrupted(
            &fixture,
            "CREATE TRIGGER stop_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'stop'); END;",
        );
        let store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        assert!(store.inspect_unfinished_acceptance(sequence).is_ok());
        match fault {
            "hole" => {
                let key = format!("a1:{sequence:016x}:chunk:00000000");
                store
                    .connection
                    .execute("DELETE FROM records WHERE key=?1", [&key])
                    .unwrap();
            }
            "corrupt" => {
                let key = format!("a1:{sequence:016x}:chunk:00000000");
                store
                    .connection
                    .execute("UPDATE records SET value=x'00' WHERE key=?1", [&key])
                    .unwrap();
            }
            "batch" => {
                store
                    .connection
                    .execute("UPDATE state SET batch=zeroblob(32)", [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            store.inspect_unfinished_acceptance(sequence).is_err(),
            "{fault}"
        );
    }
}

/// Even when a changed final chunk is given a matching row checksum and a
/// matching apparent last-append batch, its complete source digest must still
/// agree with the original stage before any descriptive evidence is returned.
#[test]
fn complete_prefix_must_match_the_declared_full_source_digest() {
    let fixture = Fixture::new();
    let (sequence, header) = interrupted(
        &fixture,
        "CREATE TRIGGER stop_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'stop'); END;",
    );
    let store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    assert!(store.inspect_unfinished_acceptance(sequence).is_ok());
    let key = chunk_key(&format!("a1:{sequence:016x}:"), header.chunks - 1);
    let mut changed = store.read(&key).unwrap().unwrap();
    changed[0] ^= 1;
    let digest = Sha256::digest(&changed);
    let batch = batch_digest(&[CustodyWrite {
        key: &key,
        previous: None,
        value: Some(&changed),
    }])
    .unwrap();
    store
        .connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            params![changed, digest.as_slice(), key],
        )
        .unwrap();
    store
        .connection
        .execute("UPDATE state SET batch=?1", [&batch])
        .unwrap();
    assert!(store.inspect_unfinished_acceptance(sequence).is_err());
}

/// An actual competing connection cannot commit while inspection retains its
/// SQLite snapshot. Once that snapshot ends, its later write changes the last
/// batch and the same staged prefix no longer qualifies for this inspection.
#[test]
fn unfinished_inspection_uses_one_snapshot_and_rejects_later_writer() {
    let fixture = Fixture::new();
    let (sequence, _) = interrupted(
        &fixture,
        "CREATE TRIGGER stop_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'stop'); END;",
    );
    let store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut other = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let transaction = store.connection.unchecked_transaction().unwrap();
    let evidence = store
        .inspect_unfinished_in_snapshot(&transaction, sequence)
        .unwrap();
    let write = CustodyWrite {
        key: "unrelated",
        previous: None,
        value: Some(b"later"),
    };
    assert!(other.commit(evidence.generation, &[write]).is_err());
    assert_eq!(
        store
            .inspect_unfinished_in_snapshot(&transaction, sequence)
            .unwrap()
            .generation,
        evidence.generation
    );
    transaction.commit().unwrap();
    let write = CustodyWrite {
        key: "unrelated",
        previous: None,
        value: Some(b"later"),
    };
    other.commit(evidence.generation, &[write]).unwrap();
    assert!(store.inspect_unfinished_acceptance(sequence).is_err());
}

/// Snapshot identity is a connection property, not an equal owner label or a
/// caller-supplied sequence. A transaction from a different real store must be
/// rejected before reading either store's staged source.
#[test]
fn unfinished_inspection_rejects_a_foreign_store_snapshot() {
    let fixture = Fixture::new();
    let (sequence, _) = interrupted(
        &fixture,
        "CREATE TRIGGER stop_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'stop'); END;",
    );
    let store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let other_fixture = Fixture::new();
    let other = MmpCustodyStore::open(&other_fixture.0, "session").unwrap();
    let transaction = other.connection.unchecked_transaction().unwrap();
    assert!(
        store
            .inspect_unfinished_in_snapshot(&transaction, sequence)
            .is_err()
    );
}
