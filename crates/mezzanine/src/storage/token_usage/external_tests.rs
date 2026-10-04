//! Durable external stream replay and cumulative baseline regression coverage.
//!
//! Receipts, normalized deltas and checkpoints must agree after reopening the
//! store. Tests use content-free counters rather than vendor payloads.

use super::*;
use mez_agent::ModelTokenUsageKey;

/// Retirement blocks fresh and replay admission, not historical decoding.
/// A seeded old ledger keeps expense, optional coverage and replay tombstones;
/// Gemini-named providers/models under other harnesses remain accepted.
#[test]
fn external_retired_gemini_preserves_history_and_other_providers() {
    let (fresh, mut rejected) = fixture();
    rejected.harness = "gemini".into();
    for (mode, baseline) in [
        ("delta", false),
        ("cumulative", false),
        ("cumulative", true),
    ] {
        rejected.mode = mode.into();
        rejected.baseline = baseline;
        assert!(fresh.ingest_external(&rejected, 100).is_err());
    }
    assert!(!fresh.path().exists());
    let (store, mut report) = fixture();
    report.project = Some(AccountingProjectId::from_stored(new_token_usage_event_id()).unwrap());
    report.model = ModelTokenUsageKey::new("google", "gemini-fixture");
    store.ingest_external(&report, 100).unwrap();
    let connection = rusqlite::Connection::open(store.path()).unwrap();
    connection.execute_batch("UPDATE token_usage_events SET harness='gemini'; UPDATE external_usage_streams SET harness='gemini';").unwrap();
    drop(connection);
    report.harness = "gemini".into();
    let reopened = TokenUsageStore::new(store.path());
    let before = reopened
        .history_snapshot(100, &[1], &TokenHistoryScope::default())
        .unwrap();
    for (mode, baseline) in [
        ("delta", false),
        ("cumulative", false),
        ("cumulative", true),
    ] {
        report.mode = mode.into();
        report.baseline = baseline;
        assert!(reopened.ingest_external(&report, 100).is_err());
    }
    let after = reopened
        .history_snapshot(100, &[1], &TokenHistoryScope::default())
        .unwrap();
    assert_eq!(before.windows, after.windows);
    let (key, usage) = after.windows[&1].iter().next().unwrap();
    assert_eq!(key.harness, "gemini");
    assert_eq!(key.project, report.project);
    assert_eq!(usage.usage.input_tokens, 10);
    assert_eq!(usage.usage.cached_input_tokens, Some(3));
    let connection = rusqlite::Connection::open(store.path()).unwrap();
    for table in ["external_usage_streams", "external_usage_receipts"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }
    report.harness = "opencode".into();
    report.epoch = "other-source".into();
    report.mode = "delta".into();
    report.baseline = false;
    assert!(reopened.ingest_external(&report, 100).unwrap().applied);
    report.harness = "codex".into();
    report.epoch = "gemini-provider".into();
    report.model.provider = "gemini".into();
    assert!(reopened.ingest_external(&report, 100).unwrap().applied);
    let native = TokenUsageEvent {
        id: "native-gemini-model".into(),
        project: report.project.clone(),
        observed_at_unix_seconds: 100,
        model: report.model.clone(),
        usage: mez_agent::ModelTokenUsage {
            input_tokens: 7,
            ..Default::default()
        },
    };
    reopened.append(&native).unwrap();
    assert_eq!(
        reopened.aggregate_windows(100, &[1]).unwrap()[&1][&report.model].input_tokens,
        7
    );
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Current-schema connections must remain readable while another connection
/// owns a WAL writer. Schema inspection is not a migration and must not acquire
/// a second writer lock merely to read committed accounting state.
#[test]
fn external_current_schema_open_does_not_require_writer_admission() {
    let (store, report) = fixture();
    store.ingest_external(&report, 100).unwrap();
    let blocker = rusqlite::Connection::open(store.path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let snapshot = store
        .history_snapshot(100, &[1], &TokenHistoryScope::default())
        .unwrap();
    assert_eq!(
        snapshot.windows[&1]
            .values()
            .next()
            .unwrap()
            .usage
            .input_tokens,
        report.counters.input_tokens
    );
    blocker.execute_batch("ROLLBACK;").unwrap();
    assert!(!store.ingest_external(&report, 100).unwrap().applied);
}

/// Persistent setup contention fails within a finite budget before any usage
/// transaction begins. An explicit retry after the lock clears commits once;
/// the setup retry loop must never duplicate a checkpoint or delta.
#[test]
fn external_wal_setup_contention_is_bounded_and_retry_safe() {
    let (store, report) = fixture();
    std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    let blocker = rusqlite::Connection::open(store.path()).unwrap();
    blocker
        .execute_batch("CREATE TABLE fixture(value INTEGER); BEGIN EXCLUSIVE;")
        .unwrap();
    let started = std::time::Instant::now();
    let error = store.ingest_external(&report, 100).unwrap_err();
    assert!(error.message().contains("WAL initialization"), "{error}");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    blocker.execute_batch("ROLLBACK;").unwrap();
    assert!(store.ingest_external(&report, 100).unwrap().applied);
    assert!(!store.ingest_external(&report, 100).unwrap().applied);
}

/// Concurrent identical reports opening a fresh store must settle one durable
/// delta and one replay, including schema/WAL initialization. Both workers own
/// separate connections and begin together, as lost-reply ingress permits.
#[test]
fn external_concurrent_fresh_store_replay_conserves_delta() {
    for _ in 0..20 {
        let (store, report) = fixture();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers = (0..2)
            .map(|_| {
                let store = store.clone();
                let report = report.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.ingest_external(&report, 100)
                })
            })
            .collect::<Vec<_>>();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|commit| commit.applied).count(), 1);
        assert_eq!(results[0].totals, results[1].totals);
    }
}

/// Legacy status queries must not merge external same-model expense or display
/// missing external reasoning as a known zero while harness-aware status is pending.
#[test]
fn external_usage_is_isolated_from_legacy_status_history_reader() {
    let (store, mut report) = fixture();
    report.observed_at = 90;
    report.counters.reasoning_tokens = None;
    store.ingest_external(&report, 100).unwrap();
    assert!(store.aggregate_windows(100, &[1]).unwrap()[&1].is_empty());
    assert_eq!(store.oldest_observed_at(100).unwrap(), None);
    let native = TokenUsageEvent {
        id: "native".to_string(),
        project: None,
        observed_at_unix_seconds: 100,
        model: report.model.clone(),
        usage: mez_agent::ModelTokenUsage {
            input_tokens: 7,
            output_tokens: 3,
            reasoning_tokens: 1,
            ..Default::default()
        },
    };
    store.append(&native).unwrap();
    assert_eq!(
        store.aggregate_windows(100, &[1]).unwrap()[&1][&report.model],
        native.usage
    );
    assert_eq!(store.oldest_observed_at(100).unwrap(), Some(100));
    let connection = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT reasoning_known FROM token_usage_events WHERE harness='codex'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

/// Receipt pruning cannot turn an old delta into new expense. Future timestamps,
/// sequence gaps and invalid inclusive subsets fail without advancing checkpoints.
#[test]
fn external_replay_pruning_and_invalid_reports_preserve_checkpoint() {
    let (store, report) = fixture();
    let first = store.ingest_external(&report, 100).unwrap();
    let mut gap = report.clone();
    gap.sequence = 3;
    gap.event_id = "gap".to_string();
    assert!(store.ingest_external(&gap, 100).is_err());
    gap.sequence = 2;
    gap.observed_at = 101;
    assert!(store.ingest_external(&gap, 100).is_err());
    gap.observed_at = 100;
    gap.counters.reasoning_tokens = Some(5);
    assert!(store.ingest_external(&gap, 100).is_err());
    // Advance retention through normal ingestion, without session restart.
    let mut fresh = report.clone();
    fresh.epoch = "fresh-epoch".to_string();
    fresh.observed_at = 100 + 92 * 86_400;
    store.ingest_external(&fresh, fresh.observed_at).unwrap();
    assert!(store.ingest_external(&report, 100 + 92 * 86_400).is_err());
    let mut replay = report.clone();
    replay.event_id = "old-sequence-with-new-time".to_string();
    replay.observed_at = 100 + 92 * 86_400;
    assert!(store.ingest_external(&replay, replay.observed_at).is_err());
    let connection = rusqlite::Connection::open(store.path()).unwrap();
    let revision: i64 = connection
        .query_row(
            "SELECT revision FROM external_usage_streams WHERE id=?1",
            [&first.stream_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision, 1);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM external_usage_receipts", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
}

/// An insertion error after checkpoint and receipt preparation rolls back all
/// three facts together. Retrying the same report after repair charges once.
#[test]
fn external_transaction_failure_rolls_back_checkpoint_receipt_and_delta() {
    let (store, report) = fixture();
    store.initialize(100).unwrap();
    let connection = rusqlite::Connection::open(store.path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_external_delta BEFORE INSERT ON token_usage_events BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
    assert!(store.ingest_external(&report, 100).is_err());
    for table in [
        "external_usage_streams",
        "external_usage_receipts",
        "token_usage_events",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    connection
        .execute_batch("DROP TRIGGER reject_external_delta;")
        .unwrap();
    assert!(store.ingest_external(&report, 100).unwrap().applied);
    assert!(!store.ingest_external(&report, 100).unwrap().applied);
}

/// Populated v1 rows migrate under writer ownership with exact unknown-cache
/// fidelity and default native harness identity; failure leaves version and
/// original schema unchanged rather than publishing a partial upgrade.
#[test]
fn external_schema_migration_preserves_legacy_rows_and_rolls_back_failure() {
    for fail in [false, true] {
        let (store, _) = fixture();
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        let connection = rusqlite::Connection::open(store.path()).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE token_usage_events (
            id TEXT PRIMARY KEY NOT NULL, observed_at INTEGER NOT NULL,
            provider TEXT NOT NULL, model TEXT NOT NULL, input_tokens INTEGER NOT NULL,
            output_tokens INTEGER NOT NULL, reasoning_tokens INTEGER NOT NULL,
            cached_input_tokens INTEGER NULL, cache_write_input_tokens INTEGER NULL);
            INSERT INTO token_usage_events VALUES('legacy',100,'provider','model',10,4,2,NULL,0);
            PRAGMA user_version=1;",
            )
            .unwrap();
        if fail {
            connection
                .execute_batch("CREATE TABLE external_usage_streams(id TEXT);")
                .unwrap();
            assert!(store.initialize(100).is_err());
            assert_eq!(
                connection
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert!(
                connection
                    .prepare("SELECT harness FROM token_usage_events")
                    .is_err()
            );
            connection
                .execute_batch("DROP TABLE external_usage_streams;")
                .unwrap();
        }
        store.initialize(100).unwrap();
        store.initialize(100).unwrap();
        let row = connection.query_row("SELECT harness,input_tokens,cached_input_tokens,cache_write_input_tokens FROM token_usage_events WHERE id='legacy'", [], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, Option<i64>>(2)?, row.get::<_, Option<i64>>(3)?))).unwrap();
        assert_eq!(row, ("mez".to_string(), 10, None, Some(0)));
    }
}

/// Creates an isolated durable store and normalized delta fixture.
fn fixture() -> (TokenUsageStore, ExternalUsageReport) {
    let store = TokenUsageStore::new(
        std::env::temp_dir()
            .join(format!("mez-external-usage-{}", new_token_usage_event_id()))
            .join("usage.sqlite"),
    );
    let report = ExternalUsageReport {
        owner: "server-owner".to_string(),
        project: None,
        harness: "codex".to_string(),
        epoch: "epoch-one".to_string(),
        event_id: "event-one".to_string(),
        sequence: 1,
        mode: "delta".to_string(),
        baseline: false,
        observed_at: 100,
        model: ModelTokenUsageKey::new("provider", "model"),
        counters: ExternalCounters {
            input_tokens: 10,
            output_tokens: 4,
            reasoning_tokens: Some(2),
            cached_input_tokens: Some(3),
            cache_write_input_tokens: None,
        },
    };
    (store, report)
}

/// Identical delta replay applies nothing after reopen; conflicting reused IDs,
/// stale delta sequences and mode changes do not alter committed totals.
#[test]
fn external_delta_replay_conflict_and_epoch_mode_are_durable() {
    let (store, report) = fixture();
    let first = store.ingest_external(&report, 100).unwrap();
    assert!(first.applied);
    let replay = store.clone().ingest_external(&report, 100).unwrap();
    assert!(!replay.applied);
    assert_eq!(first.totals, replay.totals);
    let mut changed = report.clone();
    changed.counters.input_tokens += 1;
    assert!(store.ingest_external(&changed, 100).is_err());
    changed.event_id = "different-old-id".to_string();
    assert!(store.ingest_external(&changed, 100).is_err());
    changed.sequence = 2;
    changed.mode = "cumulative".to_string();
    assert!(store.ingest_external(&changed, 100).is_err());
    assert_eq!(
        store
            .ingest_external(&report, 100)
            .unwrap()
            .totals
            .input_tokens,
        10
    );
}

/// Cumulative attachment records an uncharged baseline, then commits only the
/// difference. Regression and optional-category changes require a new epoch.
#[test]
fn external_cumulative_baseline_and_unknown_categories_are_conserved() {
    let (store, mut report) = fixture();
    report.mode = "cumulative".to_string();
    assert!(store.ingest_external(&report, 100).is_err());
    report.baseline = true;
    assert!(
        store
            .ingest_external(&report, 100)
            .unwrap()
            .totals
            .normalized()
            .is_zero()
    );
    report.baseline = false;
    report.sequence = 2;
    report.event_id = "event-two".to_string();
    report.counters.input_tokens = 15;
    report.counters.output_tokens = 6;
    report.counters.reasoning_tokens = Some(3);
    report.counters.cached_input_tokens = Some(4);
    let commit = store.ingest_external(&report, 100).unwrap();
    assert_eq!(commit.totals.input_tokens, 5);
    assert_eq!(commit.totals.cached_input_tokens, Some(1));
    let mut bad = report.clone();
    bad.sequence = 3;
    bad.event_id = "bad".to_string();
    bad.counters.input_tokens = 14;
    assert!(store.ingest_external(&bad, 100).is_err());
    bad.counters = report.counters;
    bad.counters.reasoning_tokens = None;
    assert!(store.ingest_external(&bad, 100).is_err());
    assert_eq!(
        store.ingest_external(&report, 100).unwrap().totals,
        commit.totals
    );
}
