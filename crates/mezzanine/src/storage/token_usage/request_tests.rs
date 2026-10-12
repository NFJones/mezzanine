//! Source-scoped request origins conserve first provenance independently of
//! observer lifetime. Synthetic source witnesses exercise storage, not native
//! authorization. Markers store opaque identifiers only and never usage delta.

use super::*;

/// Invalid combined cache coverage must fail before a native request acquires
/// any completion link or ledger evidence. Repairing only normalized telemetry
/// under the identical original marker/report then commits once after reopen.
#[test]
fn external_request_cache_conservation_rejects_without_completion() {
    let (store, owner, marker) = fixture();
    store
        .begin_external_request(&owner, "pi", &marker, None, 100)
        .unwrap();
    let mut input = report(&owner);
    input.counters.cached_input_tokens = Some(6);
    input.counters.cache_write_input_tokens = Some(5);
    assert!(
        store
            .freeze_external_request_report(&input, &marker)
            .is_err()
    );
    let connection = store.open().unwrap();
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
    for table in [
        "token_usage_events",
        "external_usage_observations",
        "external_usage_receipts",
        "external_usage_streams",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    input.counters.cache_write_input_tokens = Some(4);
    let frozen = store
        .freeze_external_request_report(&input, &marker)
        .unwrap();
    assert!(store.ingest_external_request(&frozen, 101).unwrap().applied);
    let reopened = TokenUsageStore::new(store.path());
    assert!(
        !reopened
            .ingest_external_request(&frozen, 101)
            .unwrap()
            .applied
    );
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

/// Normalized report fixture; its supplied project is not trusted request
/// provenance and must be replaced by the marker's original stored origin.
fn report(owner: &str) -> ExternalUsageReport {
    ExternalUsageReport {
        owner: owner.into(),
        project: None,
        harness: "pi".into(),
        epoch: "caller-epoch".into(),
        event_id: "b".repeat(64),
        sequence: 1,
        mode: "observation".into(),
        baseline: false,
        observed_at: 101,
        model: mez_agent::ModelTokenUsageKey::new("fixture", "model"),
        counters: ExternalCounters {
            input_tokens: 10,
            output_tokens: 3,
            ..Default::default()
        },
    }
}

/// A completed observation uses the original marker's project, never current
/// registration/cwd or supplied origin. Same-report reopen replay is inert;
/// another request/observation and an unbound storage call cannot detach it.
#[test]
fn external_request_observation_preserves_origin_and_single_completion() {
    let (store, owner, marker) = fixture();
    let a = AccountingProjectId::from_stored(new_token_usage_event_id()).unwrap();
    let b = AccountingProjectId::from_stored(new_token_usage_event_id()).unwrap();
    store
        .begin_external_request(&owner, "pi", &marker, Some(&a), 100)
        .unwrap();
    let mut input = report(&owner);
    input.project = Some(b);
    let frozen = store
        .freeze_external_request_report(&input, &marker)
        .unwrap();
    assert_eq!(frozen.report().project.as_ref(), Some(&a));
    assert!(store.ingest_external_request(&frozen, 101).unwrap().applied);
    let reopened = TokenUsageStore::new(store.path());
    input.epoch = "changed-layout".into();
    input.sequence = 999;
    let replay = reopened
        .freeze_external_request_report(&input, &marker)
        .unwrap();
    assert!(
        !reopened
            .ingest_external_request(&replay, 101)
            .unwrap()
            .applied
    );
    assert!(store.ingest_external(replay.report(), 101).is_err());
    let another = "c".repeat(64);
    store
        .begin_external_request(&owner, "pi", &another, Some(&a), 100)
        .unwrap();
    let changed = store
        .freeze_external_request_report(&input, &another)
        .unwrap();
    assert!(store.ingest_external_request(&changed, 101).is_err());
    input.event_id = "d".repeat(64);
    let changed = store
        .freeze_external_request_report(&input, &marker)
        .unwrap();
    assert!(store.ingest_external_request(&changed, 101).is_err());
    let connection = store.open().unwrap();
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
    assert_eq!(
        connection
            .query_row("SELECT project_id FROM token_usage_events", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        a.as_str()
    );
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Expense publication failure rolls back request completion together with all
/// accounting evidence. Missing/foreign/predated markers cannot import history;
/// retry is telemetry-only and the same admitted report charges exactly once.
#[test]
fn external_request_observation_failure_and_marker_scope_are_atomic() {
    let (store, owner, marker) = fixture();
    store
        .begin_external_request(&owner, "pi", &marker, None, 100)
        .unwrap();
    let input = report(&owner);
    assert!(
        store
            .freeze_external_request_report(&input, &"f".repeat(64))
            .is_err()
    );
    let mut foreign = input.clone();
    foreign.owner = new_token_usage_event_id();
    assert!(
        store
            .freeze_external_request_report(&foreign, &marker)
            .is_err()
    );
    let mut old = input.clone();
    old.observed_at = 99;
    assert!(store.freeze_external_request_report(&old, &marker).is_err());
    let frozen = store
        .freeze_external_request_report(&input, &marker)
        .unwrap();
    let connection = store.open().unwrap();
    connection.execute_batch("CREATE TRIGGER reject_request_expense BEFORE INSERT ON token_usage_events BEGIN SELECT RAISE(ABORT,'fixture rejected'); END;").unwrap();
    assert!(store.ingest_external_request(&frozen, 101).is_err());
    let completed: Option<String> = connection
        .query_row(
            "SELECT observation_id FROM external_usage_requests",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(completed.is_none());
    for table in [
        "token_usage_events",
        "external_usage_observations",
        "external_usage_receipts",
        "external_usage_streams",
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
        .execute_batch("DROP TRIGGER reject_request_expense;")
        .unwrap();
    assert!(store.ingest_external_request(&frozen, 101).unwrap().applied);
    assert!(!store.ingest_external_request(&frozen, 101).unwrap().applied);
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Isolates one native source namespace and opaque request selector.
fn fixture() -> (TokenUsageStore, String, String) {
    let store = TokenUsageStore::new(
        std::env::temp_dir()
            .join(format!("mez-request-{}", new_token_usage_event_id()))
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
    (store, owner, "a".repeat(64))
}

/// Reopen/retry cannot repartition a request after cwd/trust/observer changes.
/// Explicitly unattributed initial requests remain unattributed rather than
/// adopting a later known project. Marker persistence emits no expense.
#[test]
fn external_request_first_origin_and_time_survive_reopen_and_retry() {
    for attributed in [false, true] {
        let (store, owner, marker) = fixture();
        let a = AccountingProjectId::from_stored(new_token_usage_event_id()).unwrap();
        let b = AccountingProjectId::from_stored(new_token_usage_event_id()).unwrap();
        let first = store
            .begin_external_request(&owner, "pi", &marker, attributed.then_some(&a), 100)
            .unwrap();
        let reopened = TokenUsageStore::new(store.path());
        assert_eq!(
            reopened
                .begin_external_request(&owner, "pi", &marker, Some(&b), 101)
                .unwrap(),
            first
        );
        assert_eq!(first.admitted_at, 100);
        assert_eq!(first.project.as_ref(), attributed.then_some(&a));
        let connection = store.open().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                    row.get::<_, i64>(0)
                })
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
}

/// Unknown owners, wrong harness, malformed IDs and invalid clocks cannot create
/// metadata under a qualified source. Existing rows are never changed by them.
#[test]
fn external_request_source_and_marker_validation_preserve_existing_rows() {
    let (store, owner, marker) = fixture();
    let first = store
        .begin_external_request(&owner, "pi", &marker, None, 100)
        .unwrap();
    for (source, harness, id, time) in [
        (owner.as_str(), "opencode", marker.as_str(), 100),
        ("unknown", "pi", marker.as_str(), 100),
        (owner.as_str(), "pi", "private content", 100),
        (owner.as_str(), "pi", marker.as_str(), 0),
    ] {
        assert!(
            store
                .begin_external_request(source, harness, id, None, time)
                .is_err()
        );
    }
    assert_eq!(
        store
            .begin_external_request(&owner, "pi", &marker, None, 200)
            .unwrap(),
        first
    );
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// Simultaneous first captures publish exactly one origin/time; a failed insert
/// leaves no marker or fake readiness, and a same-marker storage retry succeeds.
#[test]
fn external_request_concurrent_first_capture_and_publication_failure_are_atomic() {
    let (store, owner, marker) = fixture();
    let barrier = std::sync::Barrier::new(4);
    let origins = std::thread::scope(|scope| {
        let tasks = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    store
                        .begin_external_request(&owner, "pi", &marker, None, 100)
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        tasks
            .into_iter()
            .map(|task| task.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(origins.iter().all(|origin| origin == &origins[0]));
    let connection = store.open().unwrap();
    connection.execute_batch("CREATE TRIGGER reject_request BEFORE INSERT ON external_usage_requests BEGIN SELECT RAISE(ABORT,'fixture rejected'); END;").unwrap();
    let next = "b".repeat(64);
    assert!(
        store
            .begin_external_request(&owner, "pi", &next, None, 100)
            .is_err()
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    connection
        .execute_batch("DROP TRIGGER reject_request;")
        .unwrap();
    assert!(
        store
            .begin_external_request(&owner, "pi", &next, None, 100)
            .is_ok()
    );
    drop(connection);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}
