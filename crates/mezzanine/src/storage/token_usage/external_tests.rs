//! Durable external stream replay and cumulative baseline regression coverage.
//!
//! Receipts, normalized deltas and checkpoints must agree after reopening the
//! store. Tests use content-free counters rather than vendor payloads.

use super::*;
use mez_agent::ModelTokenUsageKey;

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
