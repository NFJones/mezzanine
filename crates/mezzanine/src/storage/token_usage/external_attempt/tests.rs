//! Exact pretransaction refusal qualification, separate from native authority.
//!
//! Synthetic SQLite errors test the private disposition grammar; real database
//! contention and trigger failures test the actual ledger return sites. None
//! of these fixtures enroll a producer or turn storage possession into an ACK.

use super::*;
use crate::storage::token_usage::request_tests::{fixture, report};

/// Builds one generated SQLite diagnostic without provider or callback content.
fn sqlite(code: i32) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
}

/// Only exact base BUSY/LOCKED at two allowed boundaries qualify. Extended
/// recovery/snapshot/shared-cache variants and permission/corrupt/constraint
/// failures remain terminal. A broad Io diagnostic alone never enables retry.
#[test]
fn external_attempt_qualifies_only_exact_base_precommit_codes() {
    for phase in [
        ExternalPrecommitPhase::WalSetup,
        ExternalPrecommitPhase::LedgerBeginImmediate,
    ] {
        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let mut witness = ExternalUsageAdmissionWitness::default();
            witness.refused(phase, &sqlite(code));
            let ExternalUsageAttempt::PrecommitRefused { refusal, .. } =
                witness.finish(Err(MezError::from(sqlite(code))))
            else {
                panic!("base precommit refusal was not qualified");
            };
            assert_eq!(refusal.phase, phase);
            assert_eq!(refusal.extended_code, code);
            assert!(refusal.is_explicit_precommit());
        }
        for code in [
            rusqlite::ffi::SQLITE_BUSY_RECOVERY,
            rusqlite::ffi::SQLITE_BUSY_SNAPSHOT,
            rusqlite::ffi::SQLITE_LOCKED_SHAREDCACHE,
            rusqlite::ffi::SQLITE_IOERR,
            rusqlite::ffi::SQLITE_CORRUPT,
            rusqlite::ffi::SQLITE_CONSTRAINT,
            rusqlite::ffi::SQLITE_PERM,
        ] {
            let mut witness = ExternalUsageAdmissionWitness::default();
            witness.refused(phase, &sqlite(code));
            assert!(matches!(
                witness.finish(Err(MezError::from(sqlite(code)))),
                ExternalUsageAttempt::Completed(Err(_))
            ));
        }
    }
    // Grammar only: broad Io + an unmarked invocation cannot schedule retry.
    // Actual schema/read/write/COMMIT sites have separate database controls.
    assert!(matches!(
        ExternalUsageAdmissionWitness::default()
            .finish(Err(MezError::from(sqlite(rusqlite::ffi::SQLITE_BUSY)))),
        ExternalUsageAttempt::Completed(Err(_))
    ));
}

/// Witnesses belong to one invocation only. A later unrelated error clears
/// qualification; success cannot request retry, and the next invocation starts
/// clean. Intermediate WAL refusal followed by setup success is never recorded.
#[test]
fn external_attempt_witness_is_local_and_success_never_retries() {
    let mut witness = ExternalUsageAdmissionWitness::default();
    witness.refused(
        ExternalPrecommitPhase::WalSetup,
        &sqlite(rusqlite::ffi::SQLITE_BUSY),
    );
    witness.refused(
        ExternalPrecommitPhase::WalSetup,
        &sqlite(rusqlite::ffi::SQLITE_IOERR),
    );
    assert!(matches!(
        witness.finish(Err(MezError::from(sqlite(rusqlite::ffi::SQLITE_IOERR)))),
        ExternalUsageAttempt::Completed(Err(_))
    ));
    let mut witness = ExternalUsageAdmissionWitness::default();
    witness.refused(
        ExternalPrecommitPhase::WalSetup,
        &sqlite(rusqlite::ffi::SQLITE_BUSY),
    );
    let commit = ExternalUsageCommit {
        stream_id: "a".repeat(64),
        revision: 1,
        totals: Default::default(),
        applied: false,
    };
    assert!(matches!(
        witness.finish(Ok(commit)),
        ExternalUsageAttempt::Completed(Ok(_))
    ));
    assert!(matches!(
        ExternalUsageAdmissionWitness::default()
            .finish(Err(MezError::from(sqlite(rusqlite::ffi::SQLITE_BUSY)))),
        ExternalUsageAttempt::Completed(Err(_))
    ));
}

/// An actual initialized ledger writer refuses BEGIN IMMEDIATE with raw BUSY;
/// no receipt/checkpoint/link/expense is changed. After releasing the exact
/// writer, a fresh witness commits the unchanged original report once.
#[test]
fn external_attempt_actual_ledger_begin_refusal_has_phase_and_no_effects() {
    let (store, owner, marker) = fixture();
    store
        .begin_external_request(&owner, "pi", &marker, None, 100)
        .unwrap();
    let frozen = store
        .freeze_external_request_report(&report(&owner), &marker)
        .unwrap();
    let connection = store.open().unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut witness = ExternalUsageAdmissionWitness::default();
    let result = store.ingest_external_request_with_admission(&frozen, 101, &mut witness);
    let ExternalUsageAttempt::PrecommitRefused { refusal, .. } = witness.finish(result) else {
        panic!("actual ledger writer refusal was not qualified");
    };
    assert_eq!(refusal.phase, ExternalPrecommitPhase::LedgerBeginImmediate);
    assert_eq!(refusal.extended_code, rusqlite::ffi::SQLITE_BUSY);
    for table in [
        "external_usage_observations",
        "external_usage_receipts",
        "external_usage_streams",
        "token_usage_events",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert!(
        connection
            .query_row(
                "SELECT observation_id FROM external_usage_requests",
                [],
                |row| row.get::<_, Option<String>>(0)
            )
            .unwrap()
            .is_none()
    );
    connection.execute_batch("ROLLBACK").unwrap();
    let mut witness = ExternalUsageAdmissionWitness::default();
    let result = store.ingest_external_request_with_admission(&frozen, 101, &mut witness);
    assert!(
        matches!(witness.finish(result), ExternalUsageAttempt::Completed(Ok(commit)) if commit.applied)
    );
    assert!(!store.ingest_external_request(&frozen, 101).unwrap().applied);
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Real statement failure after writer entry is terminal and atomic, not a
/// precommit refusal. A subsequent unchanged report may still commit through
/// the ordinary API after the fixture removes its own rejecting trigger.
#[test]
fn external_attempt_statement_failure_is_terminal_and_rolls_back() {
    let (store, owner, marker) = fixture();
    store
        .begin_external_request(&owner, "pi", &marker, None, 100)
        .unwrap();
    let frozen = store
        .freeze_external_request_report(&report(&owner), &marker)
        .unwrap();
    let connection = store.open().unwrap();
    connection.execute_batch("CREATE TRIGGER reject_attempt BEFORE INSERT ON token_usage_events BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let mut witness = ExternalUsageAdmissionWitness::default();
    let result = store.ingest_external_request_with_admission(&frozen, 101, &mut witness);
    assert!(matches!(
        witness.finish(result),
        ExternalUsageAttempt::Completed(Err(_))
    ));
    for table in [
        "external_usage_observations",
        "external_usage_receipts",
        "external_usage_streams",
        "token_usage_events",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    connection
        .execute_batch("DROP TRIGGER reject_attempt")
        .unwrap();
    assert!(store.ingest_external_request(&frozen, 101).unwrap().applied);
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Future schema and unavailable markers do not expose a retryable admission
/// even with mapped source metadata. The current version and original report
/// remain unmodified; no migration or historical import is manufactured.
#[test]
fn external_attempt_future_schema_and_missing_origin_are_terminal() {
    let (store, owner, marker) = fixture();
    store
        .begin_external_request(&owner, "pi", &marker, None, 100)
        .unwrap();
    let frozen = store
        .freeze_external_request_report(&report(&owner), &marker)
        .unwrap();
    let connection = store.open().unwrap();
    connection.execute_batch("PRAGMA user_version=999").unwrap();
    let mut witness = ExternalUsageAdmissionWitness::default();
    let result = store.ingest_external_request_with_admission(&frozen, 101, &mut witness);
    assert!(matches!(
        witness.finish(result),
        ExternalUsageAttempt::Completed(Err(_))
    ));
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        999
    );
    connection.execute_batch("PRAGMA user_version=7").unwrap();
    let mut witness = ExternalUsageAdmissionWitness::default();
    let missing = store.freeze_external_request_report_with_admission(
        &report(&owner),
        &"f".repeat(64),
        &mut witness,
    );
    assert!(missing.is_err());
    assert!(matches!(
        witness.finish(missing.map(|_| unreachable!("unknown marker cannot freeze"))),
        ExternalUsageAttempt::Completed(Err(_))
    ));
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// A real exclusive rollback-journal writer refuses WAL setup before expense
/// begins. The raw phase/code remains available only for this failed invocation;
/// after release, successful WAL setup followed by missing-origin rejection
/// clears even an intentionally reused witness, rather than retrying a terminal
/// metadata error because an earlier invocation was busy.
#[test]
fn external_attempt_wal_refusal_then_terminal_origin_has_no_stale_witness() {
    let (store, owner, marker) = fixture();
    store
        .begin_external_request(&owner, "pi", &marker, None, 100)
        .unwrap();
    let connection = store.open().unwrap();
    connection
        .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
        .unwrap();
    let mut witness = ExternalUsageAdmissionWitness::default();
    let result =
        store.freeze_external_request_report_with_admission(&report(&owner), &marker, &mut witness);
    assert!(result.is_err());
    assert_eq!(
        witness.refusal,
        Some(ExternalPrecommitRefusal {
            phase: ExternalPrecommitPhase::WalSetup,
            extended_code: rusqlite::ffi::SQLITE_BUSY
        })
    );
    connection.execute_batch("ROLLBACK").unwrap();
    let missing = store.freeze_external_request_report_with_admission(
        &report(&owner),
        &"f".repeat(64),
        &mut witness,
    );
    assert!(missing.is_err());
    assert!(matches!(
        witness.finish(missing.map(|_| unreachable!("missing origin"))),
        ExternalUsageAttempt::Completed(Err(_))
    ));
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Schema writer acquisition is not the expense ledger's BEGIN. Even the same
/// real SQLite BUSY caused by the same held writer remains terminal when a
/// generated outdated-version fixture needs migration. No migration statement
/// executes; restoring the fixture's original version preserves all rows.
#[test]
fn external_attempt_schema_writer_busy_is_terminal_not_ledger_refusal() {
    let (store, owner, marker) = fixture();
    store
        .begin_external_request(&owner, "pi", &marker, None, 100)
        .unwrap();
    let frozen = store
        .freeze_external_request_report(&report(&owner), &marker)
        .unwrap();
    let connection = store.open().unwrap();
    connection
        .execute_batch("PRAGMA user_version=6; BEGIN IMMEDIATE;")
        .unwrap();
    let mut witness = ExternalUsageAdmissionWitness::default();
    let result = store.ingest_external_request_with_admission(&frozen, 101, &mut witness);
    assert!(matches!(
        witness.finish(result),
        ExternalUsageAttempt::Completed(Err(_))
    ));
    connection
        .execute_batch("ROLLBACK; PRAGMA user_version=7;")
        .unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(store.ingest_external_request(&frozen, 101).unwrap().applied);
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}
