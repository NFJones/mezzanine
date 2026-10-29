//! Actual failures after acquiring the expense writer remain terminal.
//!
//! Generated SQLite schema damage and a real rejecting COMMIT hook test the
//! owning transaction, not an injected broad error label. Failed writes consume
//! no request link/tombstone/receipt/checkpoint/delta, and no fixture result can
//! manufacture precommit scheduling evidence or durable acknowledgment.

use super::*;
use crate::storage::token_usage::request_tests::{fixture, report};

/// Real COMMIT-hook refusal happens after all ledger statements, then SQLite
/// rolls the transaction back. The invocation remains terminal/unknown to the
/// scheduler. A later ordinary telemetry retry of the exact sealed report can
/// commit once; no automatic replay or second expense ledger is introduced.
#[test]
fn external_attempt_actual_commit_refusal_is_terminal_and_atomic() {
    let (store, owner, marker) = fixture();
    store
        .begin_external_request(&owner, "pi", &marker, None, 100)
        .unwrap();
    let frozen = store
        .freeze_external_request_report(&report(&owner), &marker)
        .unwrap();
    let mut witness = ExternalUsageAdmissionWitness {
        abort_commit: true,
        ..Default::default()
    };
    let result = store.ingest_external_request_with_admission(&frozen, 101, &mut witness);
    assert!(result.is_err(), "real rejecting COMMIT hook did not run");
    assert!(witness.refusal.is_none());
    assert!(matches!(
        witness.finish(result),
        ExternalUsageAttempt::Completed(Err(_))
    ));
    let connection = store.open().unwrap();
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
    assert!(store.ingest_external_request(&frozen, 101).unwrap().applied);
    assert!(!store.ingest_external_request(&frozen, 101).unwrap().applied);
    assert_eq!(
        connection
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// A real SELECT failure after BEGIN does not inherit acquisition eligibility.
/// This deliberately damaged generated database can neither charge nor publish
/// completion, even though its native mapping/request rows still exist. It is
/// discarded only as an owned test fixture, never repaired by deleting history.
#[test]
fn external_attempt_post_writer_read_failure_is_terminal_without_expense() {
    let (store, owner, marker) = fixture();
    store
        .begin_external_request(&owner, "pi", &marker, None, 100)
        .unwrap();
    let frozen = store
        .freeze_external_request_report(&report(&owner), &marker)
        .unwrap();
    let connection = store.open().unwrap();
    connection
        .execute_batch("DROP TABLE external_usage_observations")
        .unwrap();
    let mut witness = ExternalUsageAdmissionWitness::default();
    let result = store.ingest_external_request_with_admission(&frozen, 101, &mut witness);
    assert!(result.is_err());
    assert!(witness.refusal.is_none());
    assert!(matches!(
        witness.finish(result),
        ExternalUsageAttempt::Completed(Err(_))
    ));
    for table in [
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
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// The canonical terminal alias uses the same real COMMIT as expense/checkpoint
/// publication. A SQLite commit-hook veto rolls back all statements and cannot
/// produce a durable acknowledgment or precommit scheduling permission.
#[test]
fn copilot_terminal_real_commit_failure_rolls_back_alias_and_expense() {
    use super::super::copilot_terminal_tests::{cleanup, counts, fixture, report};
    let (store, a, _) = fixture();
    let call = "4".repeat(64);
    let input = report(&a, 42, &call, 100);
    let mut witness = ExternalUsageAdmissionWitness {
        abort_commit: true,
        ..Default::default()
    };
    assert!(
        store
            .ingest_copilot_terminal_with_admission(&input, 42, &call, 101, &mut witness,)
            .is_err()
    );
    assert!(witness.refusal.is_none());
    assert_eq!(counts(&store), [0, 0, 0, 0, 0, 0]);
    assert!(
        store
            .ingest_copilot_terminal(&input, 42, &call, 101)
            .unwrap()
            .applied()
    );
    assert_eq!(counts(&store), [1, 1, 1, 1, 1, 0]);
    cleanup(store);
}
