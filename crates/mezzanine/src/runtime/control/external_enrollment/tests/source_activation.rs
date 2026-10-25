//! Real native worker preparation, distinct from accepted activation/recovery.
//!
//! These generated producers use actual Unix writer and retained ancestry
//! evidence. A Prepared database row must remain inert before AND after current
//! lifecycle publication; no fake parent token or source identity is supplied.

use super::*;

/// Genuine native observation prepares exactly one source receipt off actor.
/// Neither that write nor ordinary registration can manufacture Accepted state,
/// an expense, or permission for full-control replacement to revive the source.
#[tokio::test(flavor = "current_thread")]
async fn external_source_activation_native_worker_prepares_but_never_accepts() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store =
        crate::storage::token_usage::TokenUsageStore::new(fixture.directory.join("usage.sqlite"));
    fixture.service.set_token_usage_store(store.clone());
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok(), "{observed:?}");
    assert!(work.source_owner.get().unwrap().is_some());
    assert!(
        work.source_preparation
            .get()
            .unwrap()
            .outcome_for_tests()
            .unwrap()
            .is_ok()
    );
    let identity = work.origin.reobserve().unwrap();
    let fingerprint = work
        .ancestry
        .get()
        .unwrap()
        .continuity_digest(work.origin.uid(), identity)
        .unwrap();
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT ancestry FROM external_usage_source_activations",
                [],
                |row| row.get::<_, Vec<u8>>(0)
            )
            .unwrap(),
        fingerprint
    );
    assert_eq!(database.query_row("SELECT count(*) FROM external_usage_source_activations WHERE state=0 AND revision=1", [],
        |row| row.get::<_, i64>(0)).unwrap(), 1, "genuine native work must prepare an inert receipt");
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert_eq!(reply["result"]["registered"], true, "{reply}");
    assert_eq!(
        database
            .query_row(
                "SELECT state,revision FROM external_usage_source_activations",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (0, 1)
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

/// Abandonment after genuine preparation leaves Prepared, not Accepted. Actor
/// expiry still rejects publication and releases its exact reservation; a real
/// durable namespace plus prepared metadata cannot resurrect a missing owner.
#[tokio::test(flavor = "current_thread")]
async fn external_source_activation_abandoned_native_preparation_is_inert() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store =
        crate::storage::token_usage::TokenUsageStore::new(fixture.directory.join("usage.sqlite"));
    fixture.service.set_token_usage_store(store.clone());
    let mut work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok(), "{observed:?}");
    assert!(
        work.source_preparation
            .get()
            .unwrap()
            .outcome_for_tests()
            .unwrap()
            .is_ok()
    );
    work.deadline = Instant::now();
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some(), "{reply}");
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT state,revision FROM external_usage_source_activations",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (0, 1)
    );
    let next = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = next.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok(), "{observed:?}");
    assert_eq!(next.source_created.get(), Some(&false));
    assert!(
        next.source_preparation.get().is_none(),
        "existing mappings must not mint another attempt"
    );
    let rejected: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(next, observed, &fixture.connection),
    )
    .unwrap();
    assert!(rejected.get("error").is_some(), "{rejected}");
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_source_activations WHERE state<>0",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

/// An actual SQLite preparation failure is retained with the original native
/// work, never turned into accepted evidence or replayed with a new nonce.
/// Existing independently qualified lifecycle admission remains usable; optional
/// preparation is not a new reporting permission or source-allocation fallback.
#[tokio::test(flavor = "current_thread")]
async fn external_source_activation_native_write_failure_keeps_original_outcome() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store =
        crate::storage::token_usage::TokenUsageStore::new(fixture.directory.join("usage.sqlite"));
    store
        .initialize(crate::runtime::current_unix_seconds())
        .unwrap();
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database
        .execute_batch(
            "CREATE TRIGGER refuse_preparation AFTER INSERT ON external_usage_source_activations
        BEGIN SELECT RAISE(ABORT,'fixture preparation refusal'); END;",
        )
        .unwrap();
    fixture.service.set_token_usage_store(store);
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok(), "{observed:?}");
    let prepared = work.source_preparation.get().unwrap();
    assert!(prepared.outcome_for_tests().unwrap().is_err());
    assert!(work.source_owner.get().unwrap().is_some());
    database
        .execute_batch("DROP TRIGGER refuse_preparation;")
        .unwrap();
    assert!(
        prepared.outcome_for_tests().unwrap().is_err(),
        "removing the fault cannot invent a retry or success"
    );
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert_eq!(reply["result"]["registered"], true, "{reply}");
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_source_activations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
