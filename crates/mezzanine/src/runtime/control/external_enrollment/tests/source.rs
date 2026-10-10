//! Real native source namespace lookup beside ordinary lifecycle admission.
//!
//! Registry recreation models lost daemon-owned ephemeral registration state,
//! not a cross-process resume/fork. Actual producer/root witnesses remain live;
//! durable mapping never replaces their native qualification or enables usage.

use super::*;

/// A locked source store produces no durable namespace through the bounded
/// worker lookup. Expiring the captured admission after genuine observation
/// still rejects settlement and releases its reservation; no mapping or failed
/// lookup can substitute for the actor's original deadline/lifetime authority.
#[tokio::test(flavor = "current_thread")]
async fn external_source_store_contention_and_expired_settlement_grant_no_authority() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store =
        crate::storage::token_usage::TokenUsageStore::new(fixture.directory.join("usage.sqlite"));
    store
        .initialize(crate::runtime::current_unix_seconds())
        .unwrap();
    let lock = rusqlite::Connection::open(store.path()).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE;").unwrap();
    fixture.service.set_token_usage_store(store);
    let mut work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok(), "{observed:?}");
    assert_eq!(work.source_owner.get(), Some(&None));
    lock.execute_batch("ROLLBACK;").unwrap();
    work.deadline = Instant::now();
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
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
}

/// Registry recreation and a different short-lived helper retain the native
/// creator's namespace. Public selector/freshness and usage limitations remain
/// unchanged; neither helper identity nor stored mapping is admission proof.
#[tokio::test(flavor = "current_thread")]
async fn external_source_curated_creator_recreation_preserves_owner() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            fixture.directory.join("usage.sqlite"),
        ));
    let (mut first_socket, first_connection) =
        super::helper_presentation::child(&mut fixture).await;
    let first = super::curated_admission::enroll(&mut fixture, &first_connection).await;
    assert_eq!(first["result"]["registered"], true);
    let owner = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .accounting_owner
        .clone();
    super::helper_presentation::release(&mut first_socket, &first_connection).await;
    *fixture.service.control.external_agents_mut() = Default::default();
    let (mut next_socket, next_connection) = super::helper_presentation::child(&mut fixture).await;
    assert_ne!(
        first_connection.unix_origin().unwrap().identity.process_id,
        next_connection.unix_origin().unwrap().identity.process_id
    );
    let next = super::curated_admission::enroll(&mut fixture, &next_connection).await;
    assert_eq!(next["result"]["registered"], true);
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .accounting_owner,
        owner
    );
    assert!(next["result"].get("launch_token").is_none());
    assert_eq!(next["result"]["usage"], "unavailable-source-continuity");
    super::helper_presentation::release(&mut next_socket, &next_connection).await;
}

/// Resolves through the real native worker and actor ingress, never supplying a
/// PID/namespace key or pretending an injected success established ancestry.
async fn enroll(fixture: &mut Fixture) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    serde_json::from_str(&fixture.service.complete_external_enrollment(
        work,
        observed,
        &fixture.connection,
    ))
    .unwrap()
}

/// Losing ephemeral registry/observer state does not allocate a new accounting
/// namespace for the exact live native source. Public handles do rotate, and
/// accounting stays unavailable until ordered observation continuity is wired.
#[tokio::test(flavor = "current_thread")]
async fn external_source_native_registry_recreation_preserves_owner() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store =
        crate::storage::token_usage::TokenUsageStore::new(fixture.directory.join("usage.sqlite"));
    fixture.service.set_token_usage_store(store);
    let first = enroll(&mut fixture).await;
    assert_eq!(first["result"]["registered"], true);
    assert_eq!(first["result"]["usage"], "unavailable-source-continuity");
    let original = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .accounting_owner
        .clone();
    *fixture.service.control.external_agents_mut() = Default::default();
    let second = enroll(&mut fixture).await;
    assert_eq!(second["result"]["registered"], true);
    assert_ne!(
        second["result"]["launch_token"],
        first["result"]["launch_token"]
    );
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .accounting_owner,
        original
    );
    assert_eq!(second["result"]["usage"], "unavailable-source-continuity");
}

/// Ledger failure does not break a qualified observational producer or pretend
/// the fallback random namespace is durable. No source row or usage delta is
/// published, and the existing unavailable-continuity contract remains exact.
#[tokio::test(flavor = "current_thread")]
async fn external_source_failed_storage_keeps_lifecycle_neutral() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let blocked = fixture.directory.join("not-a-directory");
    std::fs::write(&blocked, b"preserve").unwrap();
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            blocked.join("usage.sqlite"),
        ));
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    assert_eq!(work.source_owner.get(), Some(&None));
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert_eq!(reply["result"]["registered"], true);
    assert_eq!(reply["result"]["usage"], "unavailable-source-continuity");
    assert_eq!(std::fs::read(blocked).unwrap(), b"preserve");
}
