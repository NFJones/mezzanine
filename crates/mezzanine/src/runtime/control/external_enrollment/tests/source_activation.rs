//! Real native preparation and actor-qualified publication, not cold recovery.
//!
//! These generated producers use actual Unix writer and retained ancestry
//! evidence. Native observation and registration alone cannot mark a row Accepted;
//! the second worker needs the actual actor's sealed grant. Even Accepted remains
//! inert for cold recovery. No fake parent token or source identity is supplied.

use super::*;

/// The actual two-phase native acceptance must expose a closed, explicitly
/// versioned historical source receipt, not require copying test-only selectors
/// or private launch handles. It grants no cold/native authority by itself.
#[tokio::test(flavor = "current_thread")]
async fn external_source_activation_reply_contains_only_public_accepted_source_receipt() {
    let Some((_fixture, _store, publication, ready)) =
        super::source_activation_fixture::accepted_source().await
    else {
        return;
    };
    let receipt = ready
        .get("source_receipt")
        .expect("accepted native source receipt missing");
    let object = receipt.as_object().unwrap();
    assert_eq!(object.len(), 3);
    assert_eq!(receipt["protocol"], "external-source-activation/1");
    let id = receipt["activation_id"].as_str().unwrap();
    assert_eq!(id.len(), 64);
    assert!(
        id.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    assert_ne!(id, "0".repeat(64));
    assert_eq!(receipt["observer_generation"], "1");
    let (_, activation, _) = publication
        .source_preparation
        .get()
        .unwrap()
        .observer_scope_for_tests();
    assert_eq!(id, activation.public_id());
    assert!(
        !receipt
            .to_string()
            .contains(ready["launch_token"].as_str().unwrap())
    );
    assert!(
        ready.get("recovery").is_none(),
        "historical receipt invented recovery"
    );
}

/// An actual actor grant and prepared row are not an acceptance receipt. A
/// caller-supplied successful completion without executing the acceptance worker
/// preserves lifecycle-only registration and cannot expose accepted-source facts.
#[tokio::test(flavor = "current_thread")]
async fn external_source_activation_injected_success_cannot_create_public_receipt() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store =
        crate::storage::token_usage::TokenUsageStore::new(fixture.directory.join("receipt.sqlite"));
    fixture.service.set_token_usage_store(store.clone());
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    let body =
        fixture
            .service
            .complete_external_enrollment(work.clone(), observed, &fixture.connection);
    let publication = fixture
        .service
        .begin_external_source_publication(work, body)
        .unwrap();
    let reply: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_source_publication(
            publication,
            Ok(()),
            &fixture.connection,
        ))
        .unwrap();
    assert_eq!(reply["result"]["registered"], true);
    assert!(reply["result"].get("source_receipt").is_none());
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
}

/// A genuinely native observation and actual sealed actor acceptance persist
/// only the bounded versioned public fence. No supplied PID/parent capability
/// or storage-only fake stands in for the generated ordinary producer.
#[tokio::test(flavor = "current_thread")]
async fn external_source_activation_public_fence_requires_actual_acceptance() {
    let Some((_fixture, store, _publication, _ready)) =
        super::source_activation_fixture::accepted_source().await
    else {
        return;
    };
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(database.query_row("SELECT state,revision,typeof(observer_fence),length(observer_fence) FROM external_usage_source_activations", [], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?,row.get::<_,String>(2)?,row.get::<_,Option<i64>>(3)?))).unwrap(), (1,2,"blob".into(),Some(107)));
}

/// Replacing the reservation owner invalidates a late publication even when a
/// new owner reuses its numeric slot. Only synthetic quota metadata is inserted;
/// the original grant and source proof are from a genuine native registration.
/// No old reply, release or stored acceptance may authorize the replacement.
#[tokio::test(flavor = "current_thread")]
async fn external_source_activation_publication_owner_replacement_fences_aba() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store = crate::storage::token_usage::TokenUsageStore::new(
        fixture.directory.join("publication.sqlite"),
    );
    fixture.service.set_token_usage_store(store);
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    let body =
        fixture
            .service
            .complete_external_enrollment(work.clone(), observed, &fixture.connection);
    let publication = fixture
        .service
        .begin_external_source_publication(work, body)
        .unwrap();
    let native = publication.clone();
    let accepted = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(accepted.is_ok());
    let admission = publication.admission;
    fixture.service.control.external_agents_mut().enrollments = Default::default();
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .pending
        .insert(admission);
    let reply: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_source_publication(
            publication,
            accepted,
            &fixture.connection,
        ))
        .unwrap();
    assert!(reply.get("error").is_some());
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .contains(&admission),
        "old completion must not consume replacement-owner metadata"
    );
}

/// The real native worker cannot seal actor acceptance. Successful actual
/// registration creates one grant, then a separate worker accepts once. Its
/// publication occupies the SAME finite pool as native admission, original
/// outcomes reconcile without writes, final settlement and duplicate callbacks
/// release at most their own slot, and Debug never exposes the private reply.
#[tokio::test(flavor = "current_thread")]
async fn external_source_activation_actor_grant_publication_is_exact_and_bounded() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store = crate::storage::token_usage::TokenUsageStore::new(
        fixture.directory.join("publication.sqlite"),
    );
    fixture.service.set_token_usage_store(store.clone());
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(observed.is_ok(), "{observed:?}");
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert!(
        database
            .query_row(
                "SELECT observer_fence IS NULL FROM external_usage_source_activations",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap(),
        "native observation invented accepted observer facts"
    );
    assert!(
        fixture
            .service
            .begin_external_source_publication(work.clone(), "unqualified".into())
            .is_err()
    );
    assert!(
        work.source_preparation
            .get()
            .unwrap()
            .admission
            .get()
            .is_none()
    );
    let body =
        fixture
            .service
            .complete_external_enrollment(work.clone(), observed, &fixture.connection);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["result"]["registered"], true);
    let publication = fixture
        .service
        .begin_external_source_publication(work.clone(), body)
        .unwrap();
    assert!(publication.is_source_publication());
    let old_native: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work.clone(), Ok(()), &fixture.connection),
    )
    .unwrap();
    assert!(old_native.get("error").is_some());
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .contains(&publication.admission)
    );
    assert!(
        !format!("{publication:?}").contains(parsed["result"]["launch_token"].as_str().unwrap())
    );
    // Synthetic quota reservations are NOT native admission witnesses. They
    // exercise metadata headroom only; the publication proof above is genuine.
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .pending
        .extend(100..131);
    assert_eq!(
        fixture
            .service
            .prepare_external_enrollment(&request(), &fixture.connection)
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::RateLimited
    );
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .pending
        .retain(|id| !(100..131).contains(id));
    let native = publication.clone();
    let accepted = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(accepted.is_ok(), "{accepted:?}");
    let native = publication.clone();
    assert!(
        tokio::task::spawn_blocking(move || native.execute_enrollment())
            .await
            .unwrap()
            .is_ok()
    );
    let reply: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_source_publication(
            publication.clone(),
            Err(MezError::conflict(
                "fixture outer completion acknowledgment lost",
            )),
            &fixture.connection,
        ))
        .unwrap();
    assert_eq!(reply["result"]["registered"], true);
    assert_eq!(
        reply["result"]["source_receipt"]["protocol"], "external-source-activation/1",
        "real accepted cache was replaced by outer completion feedback"
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
    let duplicate: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_source_publication(
            publication,
            Ok(()),
            &fixture.connection,
        ))
        .unwrap();
    assert!(duplicate.get("error").is_some());
    assert!(
        fixture
            .service
            .begin_external_source_publication(work, "duplicate".into())
            .is_err()
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
        (1, 2)
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

/// A real acceptance-write fault remains cached even after the fault is
/// removed. Optional metadata failure preserves lifecycle-only registration,
/// but cannot mark Prepared as Accepted or fabricate expense/recovery rights.
#[tokio::test(flavor = "current_thread")]
async fn external_source_activation_publication_failure_is_not_replayed() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store = crate::storage::token_usage::TokenUsageStore::new(
        fixture.directory.join("publication.sqlite"),
    );
    fixture.service.set_token_usage_store(store.clone());
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    let body =
        fixture
            .service
            .complete_external_enrollment(work.clone(), observed, &fixture.connection);
    let publication = fixture
        .service
        .begin_external_source_publication(work, body)
        .unwrap();
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database.execute_batch("CREATE TRIGGER refuse_acceptance AFTER UPDATE ON external_usage_source_activations BEGIN SELECT RAISE(ABORT,'fixture refusal'); END;").unwrap();
    let native = publication.clone();
    let accepted = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(accepted.is_err());
    database
        .execute_batch("DROP TRIGGER refuse_acceptance;")
        .unwrap();
    let native = publication.clone();
    assert!(
        tokio::task::spawn_blocking(move || native.execute_enrollment())
            .await
            .unwrap()
            .is_err()
    );
    let reply: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_source_publication(
            publication,
            accepted,
            &fixture.connection,
        ))
        .unwrap();
    assert_eq!(reply["result"]["registered"], true);
    assert_eq!(reply["result"]["usage"], "unavailable-source-continuity");
    assert!(
        reply["result"].get("source_receipt").is_none(),
        "failed acceptance fabricated a source receipt"
    );
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

/// A committed acceptance does not authorize a stale private reply. Final actor
/// deadline failure releases only that exact publication slot, leaves actual
/// historical acceptance intact and returns no handle or accounting authority.
#[tokio::test(flavor = "current_thread")]
async fn external_source_activation_publication_rechecks_after_commit() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store = crate::storage::token_usage::TokenUsageStore::new(
        fixture.directory.join("publication.sqlite"),
    );
    fixture.service.set_token_usage_store(store.clone());
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    let body =
        fixture
            .service
            .complete_external_enrollment(work.clone(), observed, &fixture.connection);
    let mut publication = fixture
        .service
        .begin_external_source_publication(work, body)
        .unwrap();
    let native = publication.clone();
    let accepted = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(accepted.is_ok());
    publication.deadline = Instant::now();
    let reply: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_source_publication(
            publication,
            accepted,
            &fixture.connection,
        ))
        .unwrap();
    assert!(reply.get("error").is_some());
    assert!(reply.get("result").is_none());
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
        (1, 2)
    );
}

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
