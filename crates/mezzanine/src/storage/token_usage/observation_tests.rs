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
            .execute_batch("DROP TABLE external_usage_observations; PRAGMA user_version=5;")
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
            if fail { 5 } else { 6 }
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
    let store = TokenUsageStore::new(
        std::env::temp_dir()
            .join(format!("mez-observations-{}", new_token_usage_event_id()))
            .join("usage.sqlite"),
    );
    let key = ExternalSourceKey::from_native(
        "pi",
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
            harness: "pi".into(),
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
