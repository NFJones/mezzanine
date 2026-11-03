//! Actual private-stage recovery tests; every refusal comes through the trusted
//! production port and every fault affects real SQLite transactions/postimages.

use super::super::test_fixture::{Fixture, original, stored_bytes};
use super::*;
use mez_agent::messaging::MessageAcceptanceWriteOutcome;

/// Uses a real marker transaction refusal after all private chunks commit.
fn fail_marker(store: &MmpCustodyStore) {
    store.connection.execute_batch("CREATE TRIGGER fail_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'fixture marker failure'); END;").unwrap();
}

/// Returns actual SQLite generation/count, not a historical attempt label.
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

/// Exact row absence is not noncommit evidence when the persisted allocation,
/// owner or generation/batch framing changed after the original plan. Each
/// mismatch must remain unknown rather than releasing the pending original.
#[test]
fn empty_recovery_rechecks_persisted_store_state_before_refusal() {
    let mut refused = Vec::new();
    for fault in ["id", "owner", "batch"] {
        let fixture = Fixture::new();
        let (_service, ticket, write) = original("short".into());
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let plan = store.acceptance_plan(ticket.sequence()).unwrap();
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
                    .execute("UPDATE state SET batch=zeroblob(32)", [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        if matches!(
            store.acceptance_recovery_port(&plan).commit(&write),
            Err(MessageAcceptanceStoreError::NotCommitted(_))
        ) {
            refused.push(fault);
        }
    }
    assert!(
        refused.is_empty(),
        "changed state qualified refusal: {refused:?}"
    );
}

/// An actually failed final marker leaves original private chunks; recovery
/// proves and cleans them into a bounded non-mail disposition before releasing
/// lower custody. Genuine later control work then fits without replaying a send.
#[test]
fn partial_original_recovery_releases_capacity_for_later_control_transaction() {
    let fixture = Fixture::new();
    let (mut service, ticket, write) = original("payload\0".repeat(40_000));
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    fail_marker(&store);
    let MessageAcceptanceWriteOutcome::Unknown(unknown) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected partial original")
    };
    let before = state(&store);
    assert!(before.1 > 3);
    let write = service.retry_acceptance_write(unknown).unwrap();
    let MessageAcceptanceWriteOutcome::NotCommitted(rejected) =
        write.persist(&mut store.acceptance_recovery_port(&plan))
    else {
        panic!("expected qualified noncommit")
    };
    assert_eq!(state(&store).1, 1);
    let key = format!("a1:{:016x}:stage", ticket.sequence());
    assert!(store.read(&key).unwrap().unwrap().starts_with(ABORT_MAGIC));
    assert!(service.has_unpublished_acceptance());
    service.release_rejected_acceptance(&rejected).unwrap();
    assert!(!service.has_unpublished_acceptance());
    let generation = state(&store).0 as u64;
    store
        .commit(
            generation,
            &[CustodyWrite {
                key: "ack-existing-original",
                previous: None,
                value: Some(b"canonical/application commit already known"),
            }],
        )
        .unwrap();
    assert_eq!(state(&store).1, 2);
    assert!(
        store
            .read(&format!("a1:{:016x}:commit", ticket.sequence()))
            .unwrap()
            .is_none()
    );
}

/// Recovery can never change a real positive commit to refusal. The actual
/// original and all frozen bytes remain unchanged and a second reconciliation
/// performs no additional transaction or generation advance.
#[test]
fn real_committed_original_wins_over_requested_failed_stage_release() {
    let fixture = Fixture::new();
    let (mut service, ticket, write) = original("original\0".repeat(40_000));
    let expected = serde_json::to_vec(&write).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    store.acceptance_port(&plan).commit(&write).unwrap();
    let before = state(&store);
    let MessageAcceptanceWriteOutcome::Committed(commit) =
        write.persist(&mut store.acceptance_recovery_port(&plan))
    else {
        panic!("must retain actual commit")
    };
    assert_eq!(state(&store), before);
    assert_eq!(stored_bytes(&store, ticket.sequence()), expected);
    assert!(service.publish_acceptance(&commit).unwrap().0.accepted);
    assert!(
        store
            .read(&format!("a1:{:016x}:abort", ticket.sequence()))
            .unwrap()
            .is_none()
    );
}

/// A real deletion failure after earlier tail deletes retains its immutable abort
/// descriptor. Reopening and retrying validates the exact remaining prefix and
/// resumes only cleanup; it never rebuilds chunks or appends an accepted marker.
#[test]
fn lost_cleanup_progress_reconciles_after_reopen_without_reaccepting_original() {
    let fixture = Fixture::new();
    let (mut service, ticket, write) = original("payload".repeat(60_000));
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    fail_marker(&store);
    let MessageAcceptanceWriteOutcome::Unknown(unknown) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected partial original")
    };
    let blocked = chunk_key(&format!("a1:{:016x}:", ticket.sequence()), 0);
    store.connection.execute_batch(&format!("CREATE TRIGGER fail_cleanup BEFORE DELETE ON records WHEN OLD.key='{blocked}' BEGIN SELECT RAISE(ABORT,'fixture cleanup failure'); END;")).unwrap();
    let write = service.retry_acceptance_write(unknown).unwrap();
    let MessageAcceptanceWriteOutcome::Unknown(unknown) =
        write.persist(&mut store.acceptance_recovery_port(&plan))
    else {
        panic!("expected retained cleanup")
    };
    assert_eq!(state(&store).1, 2); // remaining first chunk and abort stage
    drop(store);
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    store
        .connection
        .execute_batch("DROP TRIGGER fail_cleanup")
        .unwrap();
    let before = state(&store);
    let write = service.retry_acceptance_write(unknown).unwrap();
    let MessageAcceptanceWriteOutcome::NotCommitted(rejected) =
        write.persist(&mut store.acceptance_recovery_port(&plan))
    else {
        panic!("expected completed cleanup")
    };
    assert_eq!(state(&store), (before.0 + 1, 1));
    service.release_rejected_acceptance(&rejected).unwrap();
}

/// When a real cleanup stops after deleting several tail chunks, the surviving
/// descriptor must retain the exact previous-stage and deleted-chunk preimages
/// needed to independently reproduce the last atomic batch after process loss.
/// The retained source below is a test oracle, not a restart prerequisite.
#[test]
fn interrupted_cleanup_retains_a_reconstructable_last_deletion_batch() {
    let fixture = Fixture::new();
    let (mut service, ticket, write) = original("payload".repeat(60_000));
    let encoded = serde_json::to_vec(&write).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    fail_marker(&store);
    let MessageAcceptanceWriteOutcome::Unknown(unknown) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected partial original")
    };
    let prefix = format!("a1:{:016x}:", ticket.sequence());
    let blocked = chunk_key(&prefix, 0);
    store.connection.execute_batch(&format!("CREATE TRIGGER fail_cleanup BEFORE DELETE ON records WHEN OLD.key='{blocked}' BEGIN SELECT RAISE(ABORT,'fixture cleanup failure'); END;")).unwrap();
    let write = service.retry_acceptance_write(unknown).unwrap();
    assert!(matches!(
        write.persist(&mut store.acceptance_recovery_port(&plan)),
        MessageAcceptanceWriteOutcome::Unknown(_)
    ));
    let stage_key = format!("{prefix}stage");
    let stage = store.read(&stage_key).unwrap().unwrap();
    assert!(stage.starts_with(b"MMPABT02"));
    let abort = Abort::decode(&stage).unwrap();
    assert_eq!(abort.remaining, 1);
    assert!(abort.chunks > abort.remaining);
    let index = usize::from(abort.remaining);
    let start = index * CHUNK_BYTES;
    let end = (start + CHUNK_BYTES).min(encoded.len());
    assert_eq!(
        abort.last_deleted_digest,
        <[u8; 32]>::from(Sha256::digest(&encoded[start..end]))
    );
    let (_, _, batch, _) = position(&store, &prefix).unwrap();
    verify_abort_batch(&prefix, &stage, &abort, &batch).unwrap();
}

/// A changed store allocation, last-batch digest or deletion preimage cannot
/// qualify a retry as cleanup progress, even when a forged descriptor has both
/// a valid row checksum and a matching recomputed batch. The retained original
/// must prevent each fault from deleting the real remaining stage or chunk.
#[test]
fn cleanup_retry_rejects_changed_last_batch_or_deletion_evidence() {
    for fault in ["batch", "deleted-digest", "store-id", "consistent-forgery"] {
        let fixture = Fixture::new();
        let (mut service, ticket, write) = original("payload".repeat(60_000));
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let plan = store.acceptance_plan(ticket.sequence()).unwrap();
        fail_marker(&store);
        let MessageAcceptanceWriteOutcome::Unknown(unknown) =
            write.persist(&mut store.acceptance_port(&plan))
        else {
            panic!("expected partial original")
        };
        let prefix = format!("a1:{:016x}:", ticket.sequence());
        let blocked = chunk_key(&prefix, 0);
        store.connection.execute_batch(&format!("CREATE TRIGGER fail_cleanup BEFORE DELETE ON records WHEN OLD.key='{blocked}' BEGIN SELECT RAISE(ABORT,'fixture cleanup failure'); END;")).unwrap();
        let write = service.retry_acceptance_write(unknown).unwrap();
        let MessageAcceptanceWriteOutcome::Unknown(unknown) =
            write.persist(&mut store.acceptance_recovery_port(&plan))
        else {
            panic!("expected interrupted cleanup")
        };
        store
            .connection
            .execute_batch("DROP TRIGGER fail_cleanup")
            .unwrap();
        let stage_key = format!("{prefix}stage");
        if fault == "batch" {
            store
                .connection
                .execute("UPDATE state SET batch=zeroblob(32)", [])
                .unwrap();
        } else if fault == "store-id" {
            let mut changed = store.store_id.clone();
            changed[0] ^= 1;
            store
                .connection
                .execute("UPDATE state SET store_id=?1", [&changed])
                .unwrap();
        } else {
            let mut stage = store.read(&stage_key).unwrap().unwrap();
            stage[104] ^= 1;
            let digest = Sha256::digest(&stage);
            store
                .connection
                .execute(
                    "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
                    params![&stage, digest.as_slice(), &stage_key],
                )
                .unwrap();
            if fault == "consistent-forgery" {
                let abort = Abort::decode(&stage).unwrap();
                let deleted = chunk_key(&prefix, usize::from(abort.remaining));
                let batch = batch_digest(&[
                    CustodyWrite {
                        key: &stage_key,
                        previous: Some(&abort.previous_stage_digest),
                        value: Some(&stage),
                    },
                    CustodyWrite {
                        key: &deleted,
                        previous: Some(&abort.last_deleted_digest),
                        value: None,
                    },
                ])
                .unwrap();
                store
                    .connection
                    .execute("UPDATE state SET batch=?1", [&batch])
                    .unwrap();
            }
        }
        let before = state(&store);
        let stage = store.read(&stage_key).unwrap().unwrap();
        let chunk = store.read(&blocked).unwrap().unwrap();
        let write = service.retry_acceptance_write(unknown).unwrap();
        assert!(
            matches!(
                write.persist(&mut store.acceptance_recovery_port(&plan)),
                MessageAcceptanceWriteOutcome::Unknown(_)
            ),
            "{fault}"
        );
        assert_eq!(state(&store), before);
        assert_eq!(store.read(&stage_key).unwrap().unwrap(), stage);
        assert_eq!(store.read(&blocked).unwrap().unwrap(), chunk);
    }
}

/// The smallest possible canonical version1 stage header remains large enough
/// for the fixed version2 descriptor, so accepting a genuine source never
/// depends on enlarging the preallocated stage cell during cleanup.
#[test]
fn smallest_version1_header_can_hold_the_version2_abort_descriptor() {
    let header = Header {
        record_version: RECORD_VERSION,
        sequence: 1,
        encoded_bytes: 1,
        chunks: 1,
        digest: [0; 32],
    };
    assert!(serde_json::to_vec(&header).unwrap().len() >= ABORT_BYTES);
}

/// A single short source chunk is hashed through its actual final byte, not a
/// full chunk-sized buffer. A lost completed refusal can then reconcile the
/// same descriptor without another deletion or generation increment.
#[test]
fn completed_short_chunk_cleanup_reconciles_without_another_write() {
    let fixture = Fixture::new();
    let (_service, ticket, write) = original("short".into());
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    let (header, _) = record_header(&write, store.limits.logical_bytes).unwrap();
    assert_eq!(header.chunks, 1);
    assert!(header.encoded_bytes < CHUNK_BYTES);
    fail_marker(&store);
    assert!(store.acceptance_port(&plan).commit(&write).is_err());
    assert!(matches!(
        store.acceptance_recovery_port(&plan).commit(&write),
        Err(MessageAcceptanceStoreError::NotCommitted(_))
    ));
    let before = state(&store);
    assert!(matches!(
        store.acceptance_recovery_port(&plan).commit(&write),
        Err(MessageAcceptanceStoreError::NotCommitted(_))
    ));
    assert_eq!(state(&store), before);
}

/// A trigger that rejects the first tail deletion fires after the stage update
/// has been attempted in the same transaction. SQLite must roll both changes
/// back, leaving the initial declaration and its exact batch recoverable after
/// reopening; retry then deletes the original chunks and settles once.
#[test]
fn failed_tail_delete_rolls_back_the_paired_descriptor_update() {
    let fixture = Fixture::new();
    let (mut service, ticket, write) = original("payload".repeat(40_000));
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    fail_marker(&store);
    let MessageAcceptanceWriteOutcome::Unknown(unknown) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected partial original")
    };
    store.connection.execute_batch("CREATE TRIGGER fail_cleanup BEFORE DELETE ON records BEGIN SELECT RAISE(ABORT,'fixture cleanup failure'); END;").unwrap();
    let write = service.retry_acceptance_write(unknown).unwrap();
    let MessageAcceptanceWriteOutcome::Unknown(unknown) =
        write.persist(&mut store.acceptance_recovery_port(&plan))
    else {
        panic!("expected rejected atomic deletion")
    };
    let prefix = format!("a1:{:016x}:", ticket.sequence());
    let stage = store.read(&format!("{prefix}stage")).unwrap().unwrap();
    let abort = Abort::decode(&stage).unwrap();
    assert!(abort.chunks > 0);
    assert_eq!(abort.remaining, abort.chunks);
    let (generation, count, batch, _) = position(&store, &prefix).unwrap();
    assert_eq!(generation, abort.declared_generation);
    assert_eq!(count, usize::from(abort.chunks) + 1);
    verify_abort_batch(&prefix, &stage, &abort, &batch).unwrap();
    drop(store);
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    store
        .connection
        .execute_batch("DROP TRIGGER fail_cleanup")
        .unwrap();
    let write = service.retry_acceptance_write(unknown).unwrap();
    let MessageAcceptanceWriteOutcome::NotCommitted(rejected) =
        write.persist(&mut store.acceptance_recovery_port(&plan))
    else {
        panic!("expected qualified cleanup after reopening")
    };
    service.release_rejected_acceptance(&rejected).unwrap();
    assert_eq!(state(&store).1, 1);
}

/// Missing/corrupt actual private chunks cannot be interpreted as harmless
/// cleanup progress. Generation and checksums must agree before any deletion;
/// removing a committed marker likewise cannot turn accepted mail into refusal.
#[test]
fn damaged_or_missing_original_rows_never_qualify_noncommit_or_get_repaired() {
    for kind in 0..3 {
        let fixture = Fixture::new();
        let (_service, ticket, write) = original("payload".repeat(40_000));
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let plan = store.acceptance_plan(ticket.sequence()).unwrap();
        if kind == 2 {
            store.acceptance_port(&plan).commit(&write).unwrap();
        } else {
            fail_marker(&store);
            assert!(store.acceptance_port(&plan).commit(&write).is_err());
        }
        let prefix = format!("a1:{:016x}:", ticket.sequence());
        let key = if kind == 2 {
            format!("{prefix}commit")
        } else {
            chunk_key(&prefix, 0)
        };
        if kind == 0 {
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
        let before = state(&store);
        assert!(matches!(
            write.persist(&mut store.acceptance_recovery_port(&plan)),
            MessageAcceptanceWriteOutcome::Unknown(_)
        ));
        assert_eq!(state(&store), before);
        assert!(store.read(&format!("{prefix}abort")).unwrap().is_none());
    }
}

/// A real tiny SQLite page ceiling refuses a large chunk after its stage header
/// committed. Equal-sized replacement of the private header can still settle
/// that unaccepted original and permit a later bounded control write at the cap.
#[test]
fn actual_page_full_partial_stage_can_be_qualified_and_released() {
    let fixture = Fixture::new();
    let (mut service, ticket, write) = original("payload".repeat(40_000));
    let mut store = MmpCustodyStore::open_with_limits(
        &fixture.0,
        "session",
        CustodyStoreLimits {
            database_pages: 4,
            ..CustodyStoreLimits::default()
        },
    )
    .unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    let MessageAcceptanceWriteOutcome::Unknown(unknown) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("expected actual SQLite page-full")
    };
    assert_eq!(state(&store), (1, 1));
    let pages_before: i64 = store
        .connection
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .unwrap();
    assert_eq!(pages_before, 4);
    let write = service.retry_acceptance_write(unknown).unwrap();
    let MessageAcceptanceWriteOutcome::NotCommitted(rejected) =
        write.persist(&mut store.acceptance_recovery_port(&plan))
    else {
        panic!("expected qualified release at page cap")
    };
    assert_eq!(state(&store), (2, 1));
    let pages_after: i64 = store
        .connection
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .unwrap();
    assert_eq!(pages_after, pages_before);
    service.release_rejected_acceptance(&rejected).unwrap();
    store
        .commit(
            2,
            &[CustodyWrite {
                key: "ack-existing",
                previous: None,
                value: Some(b"settled"),
            }],
        )
        .unwrap();
}

/// No stored original and an unchanged starting generation qualify noncommit
/// even when the input could not fit the tiny policy. Foreign store identities
/// and unrelated generation progression do not qualify that absence evidence.
#[test]
fn empty_original_absence_is_exactly_fenced_before_qualifying_refusal() {
    let fixture = Fixture::new();
    let (_service, ticket, write) = original("payload".repeat(40_000));
    let mut store = MmpCustodyStore::open_with_limits(
        &fixture.0,
        "session",
        CustodyStoreLimits {
            logical_bytes: 1000,
            ..CustodyStoreLimits::default()
        },
    )
    .unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    let foreign_fixture = Fixture::new();
    let mut foreign = MmpCustodyStore::open(&foreign_fixture.0, "session").unwrap();
    assert!(
        foreign
            .acceptance_recovery_port(&plan)
            .commit(&write)
            .is_err()
    );
    assert!(matches!(
        store.acceptance_recovery_port(&plan).commit(&write),
        Err(MessageAcceptanceStoreError::NotCommitted(_))
    ));
    assert_eq!(state(&store), (0, 0));
    store
        .commit(
            0,
            &[CustodyWrite {
                key: "unrelated",
                previous: None,
                value: Some(b"changed"),
            }],
        )
        .unwrap();
    assert!(matches!(
        write.persist(&mut store.acceptance_recovery_port(&plan)),
        MessageAcceptanceWriteOutcome::Unknown(_)
    ));
}

/// Valid SQLite checksums do not make a future abort format, nonzero reserved
/// padding or changed original-header hash trustworthy. Each malformed terminal
/// disposition must fail closed without deletion, rewriting or acceptance.
#[test]
fn future_or_malformed_abort_frames_fail_closed_even_with_valid_row_checksum() {
    for kind in 0..4 {
        let fixture = Fixture::new();
        let (_service, ticket, write) = original("payload".repeat(40_000));
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let plan = store.acceptance_plan(ticket.sequence()).unwrap();
        fail_marker(&store);
        assert!(store.acceptance_port(&plan).commit(&write).is_err());
        assert!(matches!(
            store.acceptance_recovery_port(&plan).commit(&write),
            Err(MessageAcceptanceStoreError::NotCommitted(_))
        ));
        let key = format!("a1:{:016x}:stage", ticket.sequence());
        let mut bytes = store.read(&key).unwrap().unwrap();
        match kind {
            0 => bytes[7] = b'3',
            1 => bytes[ABORT_BYTES] = 1,
            2 => bytes[48] ^= 1,
            _ => bytes[7] = b'1',
        }
        let hash = Sha256::digest(&bytes);
        store
            .connection
            .execute(
                "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
                params![&bytes, hash.as_slice(), &key],
            )
            .unwrap();
        let before = state(&store);
        assert!(matches!(
            write.persist(&mut store.acceptance_recovery_port(&plan)),
            MessageAcceptanceWriteOutcome::Unknown(_)
        ));
        assert_eq!(state(&store), before);
        assert_eq!(store.read(&key).unwrap().unwrap(), bytes);
    }
}

/// A durable abort remains bound to its original content. Changed input cannot
/// consume that fact, and removing its actual tombstone cannot become permission
/// to regenerate the record or qualify a fresh absence at a later generation.
#[test]
fn changed_original_and_missing_terminal_tombstone_cannot_qualify_refusal() {
    let fixture = Fixture::new();
    let (_service, ticket, write) = original("original".repeat(40_000));
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    fail_marker(&store);
    assert!(store.acceptance_port(&plan).commit(&write).is_err());
    assert!(matches!(
        store.acceptance_recovery_port(&plan).commit(&write),
        Err(MessageAcceptanceStoreError::NotCommitted(_))
    ));
    let before = state(&store);
    let (_other, _other_ticket, changed) = original("changed".repeat(40_000));
    assert!(matches!(
        changed.persist(&mut store.acceptance_recovery_port(&plan)),
        MessageAcceptanceWriteOutcome::Unknown(_)
    ));
    assert_eq!(state(&store), before);
    let key = format!("a1:{:016x}:stage", ticket.sequence());
    store
        .connection
        .execute("DELETE FROM records WHERE key=?1", [&key])
        .unwrap();
    let damaged = state(&store);
    assert!(matches!(
        write.persist(&mut store.acceptance_recovery_port(&plan)),
        MessageAcceptanceWriteOutcome::Unknown(_)
    ));
    assert_eq!(state(&store), damaged);
    assert!(store.read(&key).unwrap().is_none());
}
