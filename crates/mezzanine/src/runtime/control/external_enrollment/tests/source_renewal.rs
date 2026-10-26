//! Paced maintenance -> existing persistence lane -> exact actor publication.

use super::source_activation_fixture::accepted_source;
use super::*;

/// A real owned blocking-task panic joins as failure without exposing panic
/// content or restarting the same immutable attempt. Native/store I/O never ran;
/// actor failure settlement preserves the live binding and original Accepted2.
#[tokio::test(flavor = "current_thread")]
async fn external_source_paced_renewal_join_failure_never_replays_attempt() {
    let Some((mut fixture, store, publication, _ready)) = accepted_source().await else {
        return;
    };
    let _observer = crate::runtime::UnixOriginStream::new(
        &mut fixture.socket,
        Some(publication.origin.clone()),
    );
    publication.source_preparation.make_renewal_due_for_tests();
    fixture.service.renew_connected_external_observers();
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("missing renewal");
    };
    work.panic_before_io_for_tests();
    assert!(!work.execute_persistence().await);
    let repeat = work.clone();
    assert!(
        tokio::task::spawn_blocking(move || repeat.execute())
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        Some(false)
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
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| !binding.retired)
    );
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT revision FROM external_usage_source_activations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
}

/// Ordinary live observer maintenance schedules exactly one due source, keeps
/// actor/I/O separate and requires a real joined result before publication.
/// Immediate ticks and duplicate callbacks cannot mint another CAS or allowance.
#[tokio::test(flavor = "current_thread")]
async fn external_source_paced_renewal_maintenance_and_publication_are_exact() {
    let Some((mut fixture, store, publication, _ready)) = accepted_source().await else {
        return;
    };
    let _observer = crate::runtime::UnixOriginStream::new(
        &mut fixture.socket,
        Some(publication.origin.clone()),
    );
    fixture.service.renew_connected_external_observers();
    assert!(
        fixture
            .service
            .persistence
            .take_token_usage_effects()
            .is_empty(),
        "initial non-due source was renewed"
    );
    publication.source_preparation.make_renewal_due_for_tests();
    fixture.service.renew_connected_external_observers();
    let mut effects = fixture.service.persistence.take_token_usage_effects();
    assert_eq!(
        effects.len(),
        1,
        "due maintenance did not enqueue one exact renewal"
    );
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } =
        effects.pop().unwrap()
    else {
        panic!("wrong persistence family");
    };
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .len(),
        1
    );
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        None,
        "supplied event fabricated a worker join"
    );
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .len(),
        1
    );
    fixture.service.renew_connected_external_observers();
    assert!(
        fixture
            .service
            .persistence
            .take_token_usage_effects()
            .is_empty()
    );
    assert!(work.execute_persistence().await);
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        Some(true)
    );
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        None
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
    drop(work);
    fixture.service.renew_connected_external_observers();
    assert!(
        fixture
            .service
            .persistence
            .take_token_usage_effects()
            .is_empty(),
        "completion replenished pacing"
    );
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT revision FROM external_usage_source_activations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        3
    );
}

/// Reused numeric admission IDs after registry replacement cannot consume or
/// publish another incarnation. The opaque original scope precedes ID removal.
#[tokio::test(flavor = "current_thread")]
async fn external_source_paced_renewal_registry_aba_is_fenced() {
    let Some((mut fixture, _store, publication, _ready)) = accepted_source().await else {
        return;
    };
    let _observer = crate::runtime::UnixOriginStream::new(
        &mut fixture.socket,
        Some(publication.origin.clone()),
    );
    publication.source_preparation.make_renewal_due_for_tests();
    fixture.service.renew_connected_external_observers();
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("missing renewal");
    };
    assert!(work.execute_persistence().await);
    let admissions = &mut fixture.service.control.external_agents_mut().enrollments;
    admissions.incarnation = Arc::new(());
    // Retain the same numeric ID to model a new registry's reused reservation.
    assert_eq!(admissions.pending.len(), 1);
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        None
    );
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .len(),
        1
    );
}

/// Real logical retirement after the physical CAS but before actor publication
/// suppresses success, then the existing exact retirement effect tombstones4.
#[tokio::test(flavor = "current_thread")]
async fn external_source_paced_renewal_retirement_after_io_refuses_publication() {
    let Some((mut fixture, store, publication, _ready)) = accepted_source().await else {
        return;
    };
    let _observer = crate::runtime::UnixOriginStream::new(
        &mut fixture.socket,
        Some(publication.origin.clone()),
    );
    publication.source_preparation.make_renewal_due_for_tests();
    fixture.service.renew_connected_external_observers();
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("missing renewal");
    };
    assert!(work.execute_persistence().await);
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        None
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
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRetirement { work: retirement } =
        fixture
            .service
            .persistence
            .take_token_usage_effects()
            .pop()
            .unwrap()
    else {
        panic!("missing retirement");
    };
    tokio::task::spawn_blocking(move || retirement.execute())
        .await
        .unwrap()
        .unwrap();
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT state,revision FROM external_usage_source_activations",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (2, 4)
    );
}

/// Shared native/publication capacity refuses admission before pacing changes;
/// restored capacity admits the same genuine source without sleeps or new input.
#[tokio::test(flavor = "current_thread")]
async fn external_source_paced_renewal_capacity_and_observer_guards_are_exact() {
    let Some((mut fixture, _store, publication, _ready)) = accepted_source().await else {
        return;
    };
    publication.source_preparation.make_renewal_due_for_tests();
    fixture.service.renew_connected_external_observers();
    assert!(
        fixture
            .service
            .persistence
            .take_token_usage_effects()
            .is_empty(),
        "PID alone supplied observer authority"
    );
    // Idle maintenance intentionally pruned that disconnected association. A
    // fresh genuine admission models capacity return, not a forged reconnect.
    let Some((mut fixture, _store, publication, _ready)) = accepted_source().await else {
        return;
    };
    publication.source_preparation.make_renewal_due_for_tests();
    let _observer = crate::runtime::UnixOriginStream::new(
        &mut fixture.socket,
        Some(publication.origin.clone()),
    );
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .pending
        .extend(10_000..10_000 + MAX_PENDING as u64);
    fixture.service.renew_connected_external_observers();
    assert!(
        fixture
            .service
            .persistence
            .take_token_usage_effects()
            .is_empty()
    );
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .pending
        .clear();
    fixture.service.renew_connected_external_observers();
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("capacity return lost renewal");
    };
    assert!(work.execute_persistence().await);
    fixture.service.persistence.record_token_usage_write_gap();
    fixture
        .service
        .apply_persistence_transition(
            crate::runtime::PersistenceEvent::ExternalSourceRenewalCompleted { work },
        )
        .unwrap();
    assert!(
        fixture
            .service
            .persistence
            .token_usage_health_error()
            .is_some(),
        "metadata success erased prior accounting loss evidence"
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

/// Changing the configured repository after a committed worker cannot publish
/// into that replacement or schedule more old-target renewals. No store write
/// is retargeted; the original historical fact remains only in its old store.
#[tokio::test(flavor = "current_thread")]
async fn external_source_paced_renewal_store_change_refuses_publication() {
    let Some((mut fixture, store, publication, _ready)) = accepted_source().await else {
        return;
    };
    let _observer = crate::runtime::UnixOriginStream::new(
        &mut fixture.socket,
        Some(publication.origin.clone()),
    );
    publication.source_preparation.make_renewal_due_for_tests();
    fixture.service.renew_connected_external_observers();
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("missing renewal");
    };
    assert!(work.execute_persistence().await);
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            fixture.directory.join("replacement.sqlite"),
        ));
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        None
    );
    drop(work);
    publication.source_preparation.make_renewal_due_for_tests();
    fixture.service.renew_connected_external_observers();
    assert!(
        fixture
            .service
            .persistence
            .take_token_usage_effects()
            .is_empty()
    );
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT revision FROM external_usage_source_activations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        3
    );
}

/// Genuine SQLite failure remains an optional metadata gap, not a lifecycle
/// retirement, false success or immediate retry loop. A join is still required.
#[tokio::test(flavor = "current_thread")]
async fn external_source_paced_renewal_sql_failure_preserves_live_binding() {
    let Some((mut fixture, store, publication, _ready)) = accepted_source().await else {
        return;
    };
    let _observer = crate::runtime::UnixOriginStream::new(
        &mut fixture.socket,
        Some(publication.origin.clone()),
    );
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database.execute_batch("CREATE TRIGGER reject_paced_renewal BEFORE UPDATE ON external_usage_source_activations BEGIN SELECT RAISE(FAIL,'paced fixture refusal'); END;").unwrap();
    publication.source_preparation.make_renewal_due_for_tests();
    fixture.service.renew_connected_external_observers();
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("missing renewal");
    };
    assert!(!work.execute_persistence().await);
    assert!(
        work.cached_failure_for_tests()
            .is_some_and(|error| error.contains("paced fixture refusal")),
        "worker never reached the SQLite fault"
    );
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        Some(false)
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| !binding.retired)
    );
    assert_eq!(
        database
            .query_row(
                "SELECT revision FROM external_usage_source_activations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    fixture.service.renew_connected_external_observers();
    assert!(
        fixture
            .service
            .persistence
            .take_token_usage_effects()
            .is_empty()
    );
}
