//! Immutable native-source observations beside legacy delta/cumulative streams.
//!
//! Synthetic native witnesses test storage only, not producer authorization.
//! No public ordinary reporting mode or historical-import permission is enabled.
//! Every observation, source-wide tombstone, stream checkpoint and expense delta
//! must commit atomically; changed IDs cannot manufacture another charge.

use super::*;
use mez_agent::ModelTokenUsageKey;

/// Both finite limits reject fresh IDs at their exact boundary; already-known
/// replay is checked before this owner. Impossible count combinations cannot
/// silently disable the guard or create an unbounded observation namespace.
#[test]
fn external_observation_capacity_guard_covers_source_and_global_bounds() {
    for (source, total, allowed) in [
        (0, 0, true),
        (65535, 65535, true),
        (1, 1048575, true),
        (65536, 65536, false),
        (1, 1048576, false),
        (-1, 0, false),
        (2, 1, false),
    ] {
        assert_eq!(
            super::external::require_observation_capacity(source, total).is_ok(),
            allowed
        );
    }
}

/// Concurrent first publication of the same immutable report commits one
/// expense and one complete evidence set. Other callers receive the durable
/// checkpoint rather than rebilling or falsely acknowledging partial state.
#[test]
fn external_observation_concurrent_first_publication_commits_once() {
    let (store, report) = fixture();
    let barrier = std::sync::Barrier::new(4);
    let commits = std::thread::scope(|scope| {
        let tasks = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    store.ingest_external(&report, 100).unwrap()
                })
            })
            .collect::<Vec<_>>();
        tasks
            .into_iter()
            .map(|task| task.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(commits.iter().filter(|commit| commit.applied).count(), 1);
    assert!(
        commits
            .iter()
            .all(|commit| commit.revision == 1 && commit.totals.input_tokens == 1)
    );
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
            1
        );
    }
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Alias adoption and its projection snapshot must publish from one writer
/// transaction. A post-commit read is deliberately shown unsafe by a fixture
/// trigger: it commits adoption before discovering corrupt group evidence.
/// The coherent API rejects and rolls back both trigger mutation and alias,
/// preserving the original receipt/checkpoint/expense for an exact retry.
#[test]
fn external_opencode_coherent_handoff_rolls_back_invalid_group() {
    for coherent in [false, true] {
        let (store, report) = fixture_for("opencode");
        let mut legacy = report.clone();
        legacy.mode = "delta".into();
        legacy.epoch = format!("opencode-message:{}", report.event_id);
        legacy.event_id = "completed:100001".into();
        let original = store.ingest_external(&legacy, 100).unwrap();
        let connection = store.open().unwrap();
        connection.execute_batch("CREATE TRIGGER corrupt_handoff_group AFTER INSERT ON external_usage_observations BEGIN UPDATE external_usage_streams SET model=model||char(0)||'corrupt'; END;").unwrap();
        if coherent {
            assert!(
                store
                    .ingest_external_opencode_handoff_with_group(&report, &legacy, 100)
                    .is_err()
            );
            assert_eq!(
                connection
                    .query_row(
                        "SELECT count(*) FROM external_usage_observations",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            assert_eq!(
                connection
                    .query_row("SELECT model FROM external_usage_streams", [], |row| row
                        .get::<_, String>(
                        0
                    ))
                    .unwrap(),
                report.model.model
            );
            connection
                .execute_batch("DROP TRIGGER corrupt_handoff_group")
                .unwrap();
            let (commit, group) = store
                .ingest_external_opencode_handoff_with_group(&report, &legacy, 100)
                .unwrap();
            assert_eq!(
                commit,
                ExternalUsageCommit {
                    applied: false,
                    ..original
                }
            );
            assert_eq!(group.members(), std::slice::from_ref(&commit));
            assert_eq!(group.totals(), commit.totals);
            let replay = store
                .ingest_external_opencode_handoff_with_group(&report, &legacy, 100)
                .unwrap();
            assert_eq!(replay, (commit, group));
        } else {
            store
                .ingest_external_opencode_handoff(&report, &legacy, 100)
                .unwrap();
            assert!(store.opencode_observation_group_snapshot(&report).is_err());
            assert_eq!(
                connection
                    .query_row(
                        "SELECT count(*) FROM external_usage_observations",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                1,
                "two calls cannot promise atomic publication"
            );
        }
        assert_eq!(
            connection
                .query_row(
                    "SELECT sum(input_tokens) FROM token_usage_events",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        drop(connection);
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }
}

/// Read-transaction groups include only same-owner aliased checkpoints, count a
/// shared generic checkpoint once, and keep project/model/category groups apart.
/// Unaliased legacy history is excluded rather than imported; reopen and replay
/// preserve the absolute snapshot without expense or identity writes. Synthetic
/// witnesses qualify storage only, never native admission or cache/reset repair.
#[test]
fn external_opencode_group_snapshot_preserves_unique_checkpoint_membership() {
    let (store, report) = fixture_for("opencode");
    assert!(
        store
            .opencode_observation_group_snapshot(&report)
            .unwrap()
            .is_none()
    );
    let mut legacy = report.clone();
    legacy.mode = "delta".into();
    legacy.epoch = format!("opencode-message:{}", report.event_id);
    legacy.event_id = "completed:100001".into();
    store.ingest_external(&legacy, 100).unwrap();
    assert!(
        store
            .opencode_observation_group_snapshot(&report)
            .unwrap()
            .is_none()
    );
    store
        .ingest_external_opencode_handoff(&report, &legacy, 100)
        .unwrap();
    for index in [2, 3] {
        let mut next = report.clone();
        next.event_id = message_id(index);
        store.ingest_external(&next, 100).unwrap();
    }
    for variant in 0..3 {
        let mut next = report.clone();
        next.event_id = message_id(10 + variant);
        match variant {
            0 => next.model.model = "other".into(),
            1 => next.counters.cached_input_tokens = None,
            _ => {
                next.project =
                    Some(AccountingProjectId::from_stored(new_token_usage_event_id()).unwrap())
            }
        }
        store.ingest_external(&next, 100).unwrap();
        assert_eq!(
            store
                .opencode_observation_group_snapshot(&next)
                .unwrap()
                .unwrap()
                .totals()
                .input_tokens,
            1
        );
    }
    let expected = store
        .opencode_observation_group_snapshot(&report)
        .unwrap()
        .unwrap();
    assert_eq!(expected.members().len(), 2);
    assert_eq!(expected.revision(), 3);
    assert_eq!(expected.totals().input_tokens, 3);
    assert_eq!(expected.totals().cached_input_tokens, Some(0));
    assert_eq!(expected.totals().reasoning_tokens, None);
    assert_eq!(
        expected.stream_id(),
        external_usage_stream_id(&report.owner, &external_observation_epoch(&report).unwrap())
            .unwrap()
    );
    let connection = store.open().unwrap();
    let before: i64 = connection
        .query_row(
            "SELECT count(*) FROM external_usage_observations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let reopened = TokenUsageStore::new(store.path());
    assert_eq!(
        reopened
            .opencode_observation_group_snapshot(&report)
            .unwrap()
            .unwrap(),
        expected
    );
    assert!(
        !reopened
            .ingest_external_opencode_handoff(&report, &legacy, 100)
            .unwrap()
            .applied
    );
    assert_eq!(
        reopened
            .opencode_observation_group_snapshot(&report)
            .unwrap()
            .unwrap(),
        expected
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        before
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        6
    );
    let mut foreign = report.clone();
    foreign.owner = new_token_usage_event_id();
    assert!(
        reopened
            .opencode_observation_group_snapshot(&foreign)
            .is_err()
    );
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Missing checkpoints and alias links to another owner's otherwise valid
/// checkpoint fail closed. Snapshot errors must not delete replay evidence or
/// add expense; no corrupt membership becomes an accounting authority.
#[test]
fn external_opencode_group_snapshot_rejects_unbound_checkpoint_evidence() {
    for missing in [false, true] {
        let (store, report) = fixture_for("opencode");
        let commit = store.ingest_external(&report, 100).unwrap();
        let connection = store.open().unwrap();
        if missing {
            connection
                .execute(
                    "DELETE FROM external_usage_streams WHERE id=?1",
                    [&commit.stream_id],
                )
                .unwrap();
        } else {
            let foreign = external_usage_stream_id(
                "another-owner",
                &external_observation_epoch(&report).unwrap(),
            )
            .unwrap();
            connection
                .execute(
                    "UPDATE external_usage_streams SET id=?1 WHERE id=?2",
                    rusqlite::params![foreign, commit.stream_id],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE external_usage_observations SET stream_id=?1 WHERE owner=?2",
                    rusqlite::params![foreign, report.owner],
                )
                .unwrap();
        }
        assert!(store.opencode_observation_group_snapshot(&report).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM external_usage_observations",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT sum(input_tokens) FROM token_usage_events",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        drop(connection);
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }
}

/// SQLite TEXT slicing must not hide embedded NUL suffixes or repair padded
/// stored model labels. Every corrupt source/alias/checkpoint field rejects
/// without adding expense. Bounded extraction must preserve raw byte identity,
/// including invalid UTF-8 and oversized metadata; these fixture mutations are
/// not native reporting or an authority to rewrite private accounting evidence.
#[test]
fn external_opencode_group_snapshot_rejects_raw_identity_corruption() {
    for field in [
        "source",
        "event",
        "stream",
        "harness",
        "provider",
        "model",
        "mode",
        "totals",
        "fingerprint",
        "padding",
        "utf8",
        "oversized",
    ] {
        let (store, report) = fixture_for("opencode");
        let commit = store.ingest_external(&report, 100).unwrap();
        let connection = store.open().unwrap();
        match field {
            "source" => {
                connection
                    .execute(
                        "UPDATE external_usage_sources SET harness=?1",
                        ["opencode\0wrong"],
                    )
                    .unwrap();
            }
            "event" => {
                connection
                    .execute(
                        "UPDATE external_usage_observations SET event_id=?1",
                        [format!("{}\0wrong", report.event_id)],
                    )
                    .unwrap();
            }
            "stream" => {
                let changed = format!("{}\0wrong", commit.stream_id);
                connection
                    .execute("UPDATE external_usage_streams SET id=?1", [&changed])
                    .unwrap();
                connection
                    .execute(
                        "UPDATE external_usage_observations SET stream_id=?1",
                        [&changed],
                    )
                    .unwrap();
            }
            "fingerprint" => {
                connection
                    .execute(
                        "UPDATE external_usage_observations SET fingerprint=fingerprint||?1",
                        ["\0wrong"],
                    )
                    .unwrap();
            }
            "totals" => {
                connection
                    .execute(
                        "UPDATE external_usage_streams SET totals=totals||?1",
                        ["\0wrong"],
                    )
                    .unwrap();
            }
            "padding" => {
                connection
                    .execute("UPDATE external_usage_streams SET model=?1", [" model "])
                    .unwrap();
            }
            "utf8" => {
                connection
                    .execute(
                        "UPDATE external_usage_streams SET model=CAST(x'ff' AS TEXT)",
                        [],
                    )
                    .unwrap();
            }
            "oversized" => {
                connection
                    .execute(
                        "UPDATE external_usage_streams SET model=?1",
                        ["a".repeat(129)],
                    )
                    .unwrap();
            }
            _ => {
                let value = match field {
                    "harness" => "opencode",
                    "provider" => "fixture",
                    "model" => "model",
                    _ => "observation",
                };
                connection
                    .execute(
                        &format!("UPDATE external_usage_streams SET {field}=?1"),
                        [format!("{value}\0wrong")],
                    )
                    .unwrap();
            }
        }
        assert!(
            store.opencode_observation_group_snapshot(&report).is_err(),
            "corrupt field {field} must not be canonicalized"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT sum(input_tokens) FROM token_usage_events",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        drop(connection);
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }
}

/// Finite membership accepts the exact boundary without counting a repeated
/// shared checkpoint multiple times, then refuses overflow. Checked aggregate
/// totals reject SQLite-range overflow across distinct valid legacy members.
/// Corruption is injected only into isolated private fixture state; reads never
/// repair, prune, charge, or reinterpret it as native authority.
#[test]
fn external_opencode_group_snapshot_bounds_membership_and_totals() {
    let (store, report) = fixture_for("opencode");
    let first = store.ingest_external(&report, 100).unwrap();
    let connection = store.open().unwrap();
    connection
        .execute(
            "WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<65535)
        INSERT INTO external_usage_observations(owner,event_id,fingerprint,stream_id)
        SELECT ?1,printf('%064x',n),printf('%064x',0),?2 FROM ids",
            rusqlite::params![report.owner, first.stream_id],
        )
        .unwrap();
    let snapshot = store
        .opencode_observation_group_snapshot(&report)
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.members().len(), 1);
    assert_eq!(snapshot.revision(), 1);
    assert_eq!(snapshot.totals().input_tokens, 1);
    connection.execute("INSERT INTO external_usage_observations(owner,event_id,fingerprint,stream_id) VALUES(?1,printf('%064x',65536),printf('%064x',0),?2)", rusqlite::params![report.owner, first.stream_id]).unwrap();
    assert_eq!(
        store
            .opencode_observation_group_snapshot(&report)
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::RateLimited
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();

    let (store, report) = fixture_for("opencode");
    for index in [1, 2] {
        let mut next = report.clone();
        next.event_id = message_id(index);
        let mut legacy = next.clone();
        legacy.mode = "delta".into();
        legacy.epoch = format!("opencode-message:{}", next.event_id);
        legacy.event_id = "completed:100001".into();
        store.ingest_external(&legacy, 100).unwrap();
        store
            .ingest_external_opencode_handoff(&next, &legacy, 100)
            .unwrap();
    }
    let connection = store.open().unwrap();
    let mut huge = report.counters;
    huge.input_tokens = i64::MAX as u64;
    connection
        .execute(
            "UPDATE external_usage_streams SET totals=?1",
            [serde_json::to_string(&huge).unwrap()],
        )
        .unwrap();
    assert!(store.opencode_observation_group_snapshot(&report).is_err());
    assert_eq!(
        connection
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    connection
        .execute(
            "UPDATE external_usage_streams SET totals=?1",
            [serde_json::to_string(&report.counters).unwrap()],
        )
        .unwrap();
    let mut generic = report.clone();
    generic.event_id = message_id(3);
    let commit = store.ingest_external(&generic, 100).unwrap();
    connection
        .execute(
            "UPDATE external_usage_streams SET revision=?1 WHERE id=?2",
            rusqlite::params![i64::MAX, commit.stream_id],
        )
        .unwrap();
    assert_eq!(
        store
            .opencode_observation_group_snapshot(&report)
            .unwrap_err()
            .message(),
        "OpenCode group revision exhausted"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        3
    );
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// A compiled OpenCode layout handoff must adopt its exact same-owner legacy
/// receipt without expense. Merely changing mode/epoch currently recharges the
/// message; the explicit path must also retain millisecond conflicts and reject
/// missing/foreign evidence while preserving receipts, history and totals.
#[test]
fn external_opencode_legacy_handoff_preserves_expense_and_conflicts() {
    let (store, report) = fixture_for("opencode");
    let mut legacy = report.clone();
    legacy.mode = "delta".into();
    legacy.epoch = format!("opencode-message:{}", report.event_id);
    legacy.event_id = "completed:100001".into();
    let original = store.ingest_external(&legacy, 100).unwrap();
    assert!(
        store.ingest_external(&report, 100).is_err(),
        "generic conversion must not bypass legacy evidence"
    );
    let adopted = store
        .ingest_external_opencode_handoff(&report, &legacy, 100)
        .unwrap();
    assert!(
        !adopted.applied,
        "layout handoff must not recharge committed legacy expense"
    );
    assert_eq!(
        adopted,
        ExternalUsageCommit {
            applied: false,
            ..original
        }
    );
    let reopened = TokenUsageStore::new(store.path());
    assert_eq!(
        reopened
            .ingest_external_opencode_handoff(&report, &legacy, 100)
            .unwrap(),
        adopted
    );
    assert!(!reopened.ingest_external(&legacy, 100).unwrap().applied);
    assert!(reopened.ingest_external(&report, 100).is_err());
    let mut changed = legacy.clone();
    changed.event_id = "completed:100002".into();
    assert!(
        reopened
            .ingest_external_opencode_handoff(&report, &changed, 100)
            .is_err(),
        "same-second completion changes must conflict"
    );
    for field in 0..3 {
        let mut next = report.clone();
        match field {
            0 => next.counters.input_tokens += 1,
            1 => next.model.model = "other".into(),
            _ => {
                next.project =
                    Some(AccountingProjectId::from_stored(new_token_usage_event_id()).unwrap())
            }
        }
        let mut old = legacy.clone();
        old.counters = next.counters;
        old.model = next.model.clone();
        old.project = next.project.clone();
        assert!(
            reopened
                .ingest_external_opencode_handoff(&next, &old, 100)
                .is_err()
        );
    }
    let mut absent = report.clone();
    absent.event_id = message_id(2);
    let mut old_absent = legacy.clone();
    old_absent.epoch = format!("opencode-message:{}", absent.event_id);
    assert!(
        reopened
            .ingest_external_opencode_handoff(&absent, &old_absent, 100)
            .is_err()
    );
    let mut foreign = legacy.clone();
    foreign.owner = new_token_usage_event_id();
    assert!(
        reopened
            .ingest_external_opencode_handoff(&report, &foreign, 100)
            .is_err()
    );
    let connection = reopened.open().unwrap();
    for table in [
        "external_usage_streams",
        "external_usage_receipts",
        "external_usage_observations",
        "token_usage_events",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Alias publication is atomic: an injected SQLite failure leaves no partial
/// adoption, and concurrent exact handoffs return one unchanged legacy checkpoint
/// without creating expense or replacing the preserved legacy replay receipt.
#[test]
fn external_opencode_legacy_handoff_rolls_back_and_serializes() {
    let (store, report) = fixture_for("opencode");
    let mut legacy = report.clone();
    legacy.mode = "delta".into();
    legacy.epoch = format!("opencode-message:{}", report.event_id);
    legacy.event_id = "completed:100001".into();
    store.ingest_external(&legacy, 100).unwrap();
    let connection = store.open().unwrap();
    connection.execute_batch("CREATE TRIGGER fail_handoff BEFORE INSERT ON external_usage_observations BEGIN SELECT RAISE(ABORT,'fixture handoff failure'); END;").unwrap();
    assert!(
        store
            .ingest_external_opencode_handoff(&report, &legacy, 100)
            .is_err()
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    connection
        .execute_batch("DROP TRIGGER fail_handoff;")
        .unwrap();
    drop(connection);
    let barrier = std::sync::Barrier::new(4);
    let commits = std::thread::scope(|scope| {
        let jobs = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    store
                        .ingest_external_opencode_handoff(&report, &legacy, 100)
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        jobs.into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(
        commits
            .iter()
            .all(|commit| !commit.applied && commit == &commits[0])
    );
    let connection = store.open().unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Schema5 upgrade retains the source owner and legacy stream/receipt payloads;
/// a colliding schema object rolls back migration without changing their expense.
#[test]
fn external_observation_schema6_migration_preserves_sources_and_legacy_replay() {
    for fail in [false, true] {
        let (store, mut legacy) = fixture();
        legacy.mode = "delta".into();
        store.ingest_external(&legacy, 100).unwrap();
        let connection = store.open().unwrap();
        connection
            .execute_batch("DROP TABLE external_usage_observations; DROP TABLE external_usage_requests; PRAGMA user_version=5;")
            .unwrap();
        if fail {
            connection
                .execute_batch("CREATE TABLE external_usage_observations(collision TEXT);")
                .unwrap();
        }
        let result = store.initialize(100);
        assert_eq!(result.is_err(), fail);
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            if fail { 5 } else { 7 }
        );
        assert_eq!(
            connection
                .query_row("SELECT owner FROM external_usage_sources", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
            legacy.owner
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT sum(input_tokens) FROM token_usage_events",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        if !fail {
            assert!(!store.ingest_external(&legacy, 100).unwrap().applied);
        }
        drop(connection);
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }
}

/// Explicit legacy handoff requires retained same-owner source and receipt
/// evidence, even when its checkpoint remains. Exhausted replay capacity must
/// reject a new alias without changing old expense/checkpoints, while an exact
/// already-adopted alias stays replayable at capacity. These are isolated ledger
/// fixtures, not proof of native admission, history import or producer activation.
#[test]
fn external_opencode_legacy_handoff_missing_evidence_and_capacity_conserve() {
    for scenario in ["unmapped", "missing-receipt", "capacity", "capacity-replay"] {
        let (store, report) = fixture_for("opencode");
        let mut legacy = report.clone();
        legacy.mode = "delta".into();
        legacy.epoch = format!("opencode-message:{}", report.event_id);
        legacy.event_id = "completed:100001".into();
        let original = store.ingest_external(&legacy, 100).unwrap();
        if scenario == "capacity-replay" {
            assert!(
                !store
                    .ingest_external_opencode_handoff(&report, &legacy, 100)
                    .unwrap()
                    .applied
            );
        }
        let connection = store.open().unwrap();
        match scenario {
            "unmapped" => {
                connection
                    .execute(
                        "DELETE FROM external_usage_sources WHERE owner=?1",
                        [&report.owner],
                    )
                    .unwrap();
            }
            "missing-receipt" => {
                connection
                    .execute(
                        "DELETE FROM external_usage_receipts WHERE stream_id=?1",
                        [&original.stream_id],
                    )
                    .unwrap();
            }
            _ => {
                let count = if scenario == "capacity-replay" {
                    65535
                } else {
                    65536
                };
                connection.execute("WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<?3)
                    INSERT INTO external_usage_observations(owner,event_id,fingerprint,stream_id)
                    SELECT ?1,printf('%064x',n),printf('%064x',0),?2 FROM ids", rusqlite::params![report.owner, original.stream_id, count]).unwrap();
            }
        }
        let before = connection
            .query_row(
                "SELECT count(*) FROM external_usage_observations WHERE owner=?1",
                [&report.owner],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let reopened = TokenUsageStore::new(store.path());
        let result = reopened.ingest_external_opencode_handoff(&report, &legacy, 100);
        if scenario == "capacity-replay" {
            assert_eq!(
                result.unwrap(),
                ExternalUsageCommit {
                    applied: false,
                    ..original.clone()
                }
            );
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                error.kind(),
                match scenario {
                    "unmapped" => crate::error::MezErrorKind::Forbidden,
                    "missing-receipt" => crate::error::MezErrorKind::Conflict,
                    _ => crate::error::MezErrorKind::RateLimited,
                }
            );
        }
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM external_usage_observations WHERE owner=?1",
                    [&report.owner],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            before
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT sum(input_tokens) FROM token_usage_events",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT revision FROM external_usage_streams WHERE id=?1",
                    [&original.stream_id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM external_usage_streams", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM external_usage_receipts WHERE stream_id=?1",
                    [&original.stream_id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            if scenario == "missing-receipt" { 0 } else { 1 }
        );
        drop(connection);
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }
}

/// Source-wide tombstone exhaustion fails explicitly without evicting existing
/// observation identity. Already committed replay remains inert at capacity;
/// plaintext/uppercase/short identifiers reject before changing any expense.
#[test]
fn external_observation_capacity_and_identity_format_fail_closed() {
    let (store, report) = fixture();
    let first = store.ingest_external(&report, 100).unwrap();
    let connection = store.open().unwrap();
    connection
        .execute(
            "WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<65535)
        INSERT INTO external_usage_observations(owner,event_id,fingerprint,stream_id)
        SELECT ?1,printf('%064x',n),printf('%064x',0),?2 FROM ids",
            rusqlite::params![report.owner, first.stream_id],
        )
        .unwrap();
    assert!(!store.ingest_external(&report, 100).unwrap().applied);
    let mut next = report.clone();
    next.event_id = message_id(2);
    assert!(store.ingest_external(&next, 100).is_err());
    for id in [
        "private raw identifier".into(),
        report.event_id.to_ascii_uppercase(),
        "a".repeat(63),
    ] {
        next.event_id = id;
        assert!(store.ingest_external(&next, 100).is_err());
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Stable content-free fixture identity, not an upstream callback text field.
fn message_id(index: u64) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(format!("fixture-message/1/{index}"))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Builds an isolated mapped source and one content-free normalized observation.
fn fixture() -> (TokenUsageStore, ExternalUsageReport) {
    fixture_for("pi")
}

/// Selects only a code-owned harness in an isolated synthetic mapped-source test.
fn fixture_for(harness: &str) -> (TokenUsageStore, ExternalUsageReport) {
    let store = TokenUsageStore::new(
        std::env::temp_dir()
            .join(format!("mez-observations-{}", new_token_usage_event_id()))
            .join("usage.sqlite"),
    );
    let key = ExternalSourceKey::from_native(
        harness,
        NativeSourceWitness {
            boot: "01234567-89ab-cdef-0123-456789abcdef",
            uid: 42,
            root: (100, 2),
            producer: (101, 100, 3),
            kind: NativeSourceKind::Socket,
            session: "fixture",
        },
    )
    .unwrap();
    let owner = store.external_source_owner(&key).unwrap();
    (
        store,
        ExternalUsageReport {
            owner,
            project: None,
            harness: harness.into(),
            epoch: "caller-epoch".into(),
            event_id: message_id(1),
            sequence: 1,
            mode: "observation".into(),
            baseline: false,
            observed_at: 100,
            model: ModelTokenUsageKey::new("fixture", "model"),
            counters: ExternalCounters {
                input_tokens: 1,
                cached_input_tokens: Some(0),
                ..Default::default()
            },
        },
    )
}

/// More than4096 distinct finalized observations share one model/category
/// stream, not a runtime stream per message. Reopen, reordered original delivery
/// and changed caller epoch/sequence replay cannot charge old observations again.
#[test]
fn external_observation_many_messages_use_one_stream_and_replay_once() {
    let (store, mut report) = fixture();
    for index in 0..4097 {
        report.event_id = message_id(index);
        report.epoch = format!("caller-message-{index}");
        let commit = store.ingest_external(&report, 100).unwrap();
        assert!(commit.applied);
        assert_eq!(commit.revision, index + 1);
    }
    let reopened = TokenUsageStore::new(store.path());
    report.event_id = message_id(0);
    report.epoch = "different-reconnect-layout".into();
    report.sequence = 999;
    let replay = reopened.ingest_external(&report, 100).unwrap();
    assert!(!replay.applied);
    assert_eq!(replay.revision, 4097);
    assert_eq!(replay.totals.input_tokens, 4097);
    let connection = store.open().unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM external_usage_streams", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        4097
    );
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Model/category changes create separate groups only for genuinely new IDs.
/// The same immutable observation cannot be rebilled under another group or
/// changed normalized payload; unknown counters remain distinct from zero.
#[test]
fn external_observation_identity_fences_model_category_and_counter_changes() {
    let (store, report) = fixture();
    let first = store.ingest_external(&report, 100).unwrap();
    for field in 0..5 {
        let mut changed = report.clone();
        match field {
            0 => changed.model.model = "other".into(),
            1 => changed.counters.cached_input_tokens = None,
            2 => changed.counters.input_tokens = 2,
            3 => changed.observed_at = 101,
            _ => {
                changed.project =
                    Some(AccountingProjectId::from_stored(new_token_usage_event_id()).unwrap())
            }
        }
        assert!(store.ingest_external(&changed, 101).is_err());
    }
    let mut wrong_harness = report.clone();
    wrong_harness.harness = "opencode".into();
    assert_eq!(
        store
            .ingest_external(&wrong_harness, 100)
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::Forbidden
    );
    let mut new_model = report.clone();
    new_model.event_id = message_id(5000);
    new_model.model.model = "other".into();
    assert_ne!(
        store.ingest_external(&new_model, 100).unwrap().stream_id,
        first.stream_id
    );
    let mut unknown = report.clone();
    unknown.event_id = message_id(5001);
    unknown.counters.cached_input_tokens = None;
    assert_ne!(
        store.ingest_external(&unknown, 100).unwrap().stream_id,
        first.stream_id
    );
    let mut project = report.clone();
    project.event_id = message_id(5002);
    project.project = Some(AccountingProjectId::from_stored(new_token_usage_event_id()).unwrap());
    assert_ne!(
        store.ingest_external(&project, 100).unwrap().stream_id,
        first.stream_id
    );
    let connection = store.open().unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        4
    );
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// A publication error rolls back the source-wide ID, receipt, checkpoint and
/// expense together. Retrying the same storage report after repair charges once,
/// without replaying any provider work or acknowledging an uncommitted expense.
#[test]
fn external_observation_publication_failure_rolls_back_all_evidence() {
    let (store, report) = fixture();
    let connection = store.open().unwrap();
    connection.execute_batch("CREATE TRIGGER reject_observation BEFORE INSERT ON token_usage_events BEGIN SELECT RAISE(ABORT,'fixture publication rejected'); END;").unwrap();
    assert!(store.ingest_external(&report, 100).is_err());
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
        .execute_batch("DROP TRIGGER reject_observation;")
        .unwrap();
    assert!(store.ingest_external(&report, 100).unwrap().applied);
    assert!(!store.ingest_external(&report, 100).unwrap().applied);
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Receipt/event retention cannot erase an observation's immutable tombstone.
/// Expired original delivery is outside the replay horizon; rewriting its time
/// does not turn a historical ID into new expense. Unmapped owners reject.
#[test]
fn external_observation_retention_keeps_ids_and_rejects_unmapped_sources() {
    let (store, report) = fixture();
    store.ingest_external(&report, 100).unwrap();
    let later = 100 + 92 * 86_400;
    store.initialize(later).unwrap();
    assert!(store.ingest_external(&report, later).is_err());
    let mut changed = report.clone();
    changed.observed_at = later;
    assert!(store.ingest_external(&changed, later).is_err());
    changed.owner = new_token_usage_event_id();
    assert!(store.ingest_external(&changed, later).is_err());
    let connection = store.open().unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}
