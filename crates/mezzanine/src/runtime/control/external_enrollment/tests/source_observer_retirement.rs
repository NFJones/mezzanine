//! Genuine original retirement workers versus changed inert public-fence facts.
//!
//! The original actor grant/kernel producer is real. A stored successor is an
//! explicit storage-only race control, NOT proof of runtime handoff/cold admission.

use super::source_activation_fixture::accepted_source;

/// A real failed original retirement freezes G1 before SQL. After a stored-only
/// G2 advance, neither the bounded recovery CAS nor same-numbered G2 tombstone
/// can be adopted as the original worker's success. Original failure stays cached.
#[tokio::test(flavor = "current_thread")]
async fn external_source_old_observer_recovery_refuses_successor_and_tombstone() {
    let Some((mut fixture, store, publication, _ready)) = accepted_source().await else {
        return;
    };
    let slot = publication.source_preparation.clone();
    let (key, activation, first) = slot.get().unwrap().observer_scope_for_tests();
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database.execute_batch("CREATE TRIGGER reject_original_retire BEFORE UPDATE ON external_usage_source_activations BEGIN SELECT RAISE(ABORT,'observer retirement fixture refusal'); END;").unwrap();
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRetirement { work } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("missing retirement");
    };
    let native_slot = slot.clone();
    let original = tokio::task::spawn_blocking(move || {
        let _guard = native_slot.hold_io_for_tests();
        native_slot.get().unwrap().retire()
    })
    .await
    .unwrap();
    assert!(
        original
            .unwrap_err()
            .message()
            .contains("observer retirement fixture refusal")
    );
    assert!(slot.get().unwrap().recovery_outcome_for_tests().is_none());
    database
        .execute_batch("DROP TRIGGER reject_original_retire")
        .unwrap();
    let proposal = first
        .propose(first.generation(), "stored-successor", [7; 32])
        .unwrap();
    let (native_store, native_key, native_activation) =
        (store.clone(), key.clone(), activation.clone());
    let successor = tokio::task::spawn_blocking(move || {
        native_store.advance_external_source_observer(
            &native_key,
            &native_activation,
            2,
            &proposal,
            super::super::source_clock::native_boot_millis().unwrap(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    let retry = work.clone();
    assert!(
        tokio::task::spawn_blocking(move || retry.execute())
            .await
            .unwrap()
            .is_err()
    );
    assert!(
        slot.get().unwrap().recovery_outcome_for_tests().is_none(),
        "changed observer admitted recovery I/O"
    );
    assert_eq!(
        store.external_source_activation(&key).unwrap(),
        Some(successor.clone())
    );
    // Only storage metadata is advanced/retired here, not a fake actor handoff.
    let (native_store, native_key, native_activation) =
        (store.clone(), key.clone(), activation.clone());
    let tombstone = tokio::task::spawn_blocking(move || {
        native_store.retire_external_source_activation_with_observer(
            &native_key,
            &native_activation,
            2,
            successor.observer.as_ref(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(tombstone.revision, 3);
    assert!(
        tokio::task::spawn_blocking(move || work.execute_for_publication())
            .await
            .unwrap()
            .is_err(),
        "old reply accepted a foreign observer tombstone"
    );
    assert!(
        slot.get()
            .unwrap()
            .retirement_outcome_for_tests()
            .unwrap()
            .as_ref()
            .unwrap_err()
            .message()
            .contains("observer retirement fixture refusal")
    );
    assert!(slot.get().unwrap().recovery_outcome_for_tests().is_none());
    assert_eq!(
        store.external_source_activation(&key).unwrap(),
        Some(tombstone)
    );
}

/// A G1 source worker must not learn and retire a stored G2 observer merely
/// because source proposal and lifetime revision are unchanged. Logical old
/// binding retirement still happens, but its durable result stays unavailable.
#[tokio::test(flavor = "current_thread")]
async fn external_source_old_observer_retirement_refuses_successor_fence() {
    let Some((mut fixture, store, publication, _ready)) = accepted_source().await else {
        return;
    };
    let (key, activation, first) = publication
        .source_preparation
        .get()
        .unwrap()
        .observer_scope_for_tests();
    let proposal = first
        .propose(first.generation(), "stored-successor", [7; 32])
        .unwrap();
    let (native_store, native_key, native_activation) =
        (store.clone(), key.clone(), activation.clone());
    let successor = tokio::task::spawn_blocking(move || {
        native_store.advance_external_source_observer(
            &native_key,
            &native_activation,
            2,
            &proposal,
            super::super::source_clock::native_boot_millis().unwrap(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRetirement { work } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("missing original retirement worker");
    };
    let retired = tokio::task::spawn_blocking(move || work.execute())
        .await
        .unwrap();
    assert!(
        retired.is_err(),
        "old retirement adopted the successor public fence"
    );
    assert_eq!(
        store.external_source_activation(&key).unwrap(),
        Some(successor)
    );
}
