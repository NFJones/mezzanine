//! Legacy-launch canonical accounting and schema13 semantic-fence regressions.
//!
//! Storage tests supply normalized fixture reports, never native evidence or
//! runtime authorization. Real legacy authorization is exercised by runtime
//! normal-admission tests. Existing rows are compared across migration/failure
//! to protect conservation and to keep legacy aliases out of native authority.

use super::*;
use crate::integrations::bootstrap::opencode::CompletedObservation;
use rusqlite::Connection;

/// Schema12 rows survive the version-only13 fence and subsequent empty14 alias
/// migration exactly, then a scoped
/// legacy adoption adds no expense. Native observation/snapshot APIs still reject
/// that owner because legacy receipt possession never creates native provenance.
#[test]
fn legacy_launch_schema13_migration_preserves_rows_and_native_refusal() {
    let (store, legacy) = fixture();
    store.ingest_external(&legacy, 100).unwrap();
    let sql = Connection::open(store.path()).unwrap();
    sql.execute_batch("DROP TABLE external_copilot_calls; PRAGMA user_version=12;")
        .unwrap();
    let before = contents(&sql);
    store.initialize(100).unwrap();
    assert_eq!(contents(&sql), before);
    assert_eq!(
        sql.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        super::store::SCHEMA_VERSION
    );
    let pair = CompletedObservation::from_legacy_report(&legacy)
        .unwrap()
        .unwrap();
    let (commit, group) = store
        .ingest_admitted_legacy_opencode_with_group(pair.observation(), pair.legacy(), 100)
        .unwrap();
    assert!(!commit.applied);
    assert_eq!(group.totals().input_tokens, 10);
    assert!(store.ingest_external(pair.observation(), 100).is_err());
    assert!(
        store
            .opencode_observation_group_snapshot(pair.observation())
            .is_err()
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM external_usage_sources", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        sql.query_row(
            "SELECT sum(input_tokens) FROM token_usage_events",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        10
    );
}

/// A refused migration writer cannot advance the semantic fence or change old
/// expense. Releasing the real SQLite writer allows the same unchanged rows to
/// migrate, without a fake successful callback or a replacement namespace.
#[test]
fn legacy_launch_schema13_migration_writer_refusal_preserves_version_and_expense() {
    let (store, legacy) = fixture();
    store.ingest_external(&legacy, 100).unwrap();
    let sql = Connection::open(store.path()).unwrap();
    sql.execute_batch(
        "DROP TABLE external_copilot_calls; PRAGMA user_version=12; BEGIN IMMEDIATE;",
    )
    .unwrap();
    let before = contents(&sql);
    assert!(store.initialize(100).is_err());
    assert_eq!(
        sql.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        12
    );
    assert_eq!(contents(&sql), before);
    sql.execute_batch("ROLLBACK;").unwrap();
    store.initialize(100).unwrap();
    assert_eq!(contents(&sql), before);
}

/// Even the separately scoped launch writer must roll back fresh expense,
/// checkpoint updates, receipts and aliases if coherent group validation finds
/// corrupted prior ownership evidence. Native-map absence is no validation bypass.
#[test]
fn legacy_launch_invalid_group_rolls_back_new_expense_and_aliases() {
    let (store, legacy) = fixture();
    let pair = CompletedObservation::from_legacy_report(&legacy)
        .unwrap()
        .unwrap();
    assert!(
        store
            .ingest_admitted_legacy_opencode_with_group(pair.observation(), pair.legacy(), 100)
            .unwrap()
            .0
            .applied
    );
    let sql = Connection::open(store.path()).unwrap();
    sql.execute(
        "UPDATE external_usage_observations SET stream_id=?1",
        ["f".repeat(64)],
    )
    .unwrap();
    let before = contents(&sql);
    let mut next = legacy;
    next.epoch = format!("opencode-message:{}", "b".repeat(64));
    let pair = CompletedObservation::from_legacy_report(&next)
        .unwrap()
        .unwrap();
    assert!(
        store
            .ingest_admitted_legacy_opencode_with_group(pair.observation(), pair.legacy(), 100)
            .is_err()
    );
    assert_eq!(contents(&sql), before);
}

/// Private isolated store and exact old delta metadata. A server-issued-shaped
/// fixture owner is not a native witness and no source mapping is inserted.
fn fixture() -> (TokenUsageStore, ExternalUsageReport) {
    let store = TokenUsageStore::new(
        std::env::temp_dir()
            .join(format!("mez-legacy-launch-{}", new_token_usage_event_id()))
            .join("usage.sqlite"),
    );
    store.initialize(100).unwrap();
    let legacy = ExternalUsageReport {
        owner: new_token_usage_event_id(),
        project: None,
        harness: "opencode".into(),
        epoch: format!("opencode-message:{}", "a".repeat(64)),
        event_id: "completed:100001".into(),
        sequence: 1,
        mode: "delta".into(),
        baseline: false,
        observed_at: 100,
        model: mez_agent::ModelTokenUsageKey::new("fixture", "model"),
        counters: ExternalCounters {
            input_tokens: 10,
            output_tokens: 3,
            ..Default::default()
        },
    };
    (store, legacy)
}

/// Reads all fixture-owned accounting/provenance rows in stable order. This
/// helper never edits data or fabricates source/admission evidence.
fn contents(sql: &Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    [
        "token_usage_events",
        "external_usage_streams",
        "external_usage_receipts",
        "external_usage_observations",
        "external_usage_sources",
        "external_usage_requests",
        "external_usage_view_resets",
        "external_usage_source_activations",
    ]
    .into_iter()
    .map(|table| {
        let mut statement = sql
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
            .unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    })
    .collect()
}
