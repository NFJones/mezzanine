//! Real warm-observer handoff -> bounded native metadata work -> actor settlement.
//!
//! These controls use ordinary generated producer/Unix writer/ancestry evidence
//! and actual registration. No stored-only successor stands in for admission.
//! They qualify same-control handoff only, not cold-service reconstruction.

use super::source_activation_fixture::accepted_source;
use super::*;

/// A real SQL failure after genuine native handoff is optional metadata failure,
/// not new authority or permission to retry the same mutation. The pending scope
/// retains its original predecessor, so current logical retirement still closes
/// that exact row. Removing the fixture fault does not refresh the cached work.
#[tokio::test(flavor = "current_thread")]
async fn external_source_observer_failed_publication_retains_prior_scope_without_replay() {
    let Some((mut fixture, store, publication, ready)) = accepted_source().await else {
        return;
    };
    let _observer = crate::runtime::UnixOriginStream::new(
        &mut fixture.socket,
        Some(publication.origin.clone()),
    );
    let (key, _, _) = publication
        .source_preparation
        .get()
        .unwrap()
        .observer_scope_for_tests();
    let before = store.external_source_activation(&key).unwrap().unwrap();
    let mut replacement = request();
    let mut params: serde_json::Value =
        serde_json::from_str(replacement.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = serde_json::json!("actual-native-successor");
    params["predecessor_generation"] = ready["generation"].clone();
    replacement.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&replacement, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    let rotated: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert_eq!(rotated["result"]["registered"], true);
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("missing observer publication");
    };
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database.execute_batch("CREATE TRIGGER observer_fault BEFORE UPDATE ON external_usage_source_activations BEGIN SELECT RAISE(ABORT,'observer-publication-fixture-failure'); END;").unwrap();
    assert!(!work.execute_persistence().await);
    assert!(
        work.cached_failure_for_tests()
            .unwrap()
            .contains("observer-publication-fixture-failure")
    );
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        Some(false)
    );
    assert_eq!(
        store.external_source_activation(&key).unwrap(),
        Some(before.clone())
    );
    database
        .execute_batch("DROP TRIGGER observer_fault")
        .unwrap();
    assert!(
        !work.execute_persistence().await,
        "failed intent blindly replayed after fault removal"
    );
    assert_eq!(
        store.external_source_activation(&key).unwrap(),
        Some(before)
    );
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRetirement { work: end } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("missing retirement");
    };
    assert!(
        tokio::task::spawn_blocking(move || end.execute_for_publication())
            .await
            .unwrap()
            .is_ok()
    );
    let ended = store.external_source_activation(&key).unwrap().unwrap();
    assert_eq!(
        ended.state,
        crate::storage::token_usage::ExternalSourceActivationState::Retired
    );
    assert_eq!(ended.observer.unwrap().generation(), 1);
}

/// Genuine observer replacement must publish its G2 fence without extending
/// lifetime/revision, changing namespace or minting a second physical worker.
/// Current source retirement then consumes G2, while duplicate metadata outcome
/// remains historical and cannot settle another actor reservation.
#[tokio::test(flavor = "current_thread")]
async fn external_source_observer_native_handoff_publishes_and_retires_current_scope() {
    let Some((mut fixture, store, publication, ready)) = accepted_source().await else {
        return;
    };
    let _observer = crate::runtime::UnixOriginStream::new(
        &mut fixture.socket,
        Some(publication.origin.clone()),
    );
    let (key, _, _) = publication
        .source_preparation
        .get()
        .unwrap()
        .observer_scope_for_tests();
    let before = store.external_source_activation(&key).unwrap().unwrap();
    let mut replacement = request();
    let mut params: serde_json::Value =
        serde_json::from_str(replacement.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = serde_json::json!("actual-native-successor");
    params["predecessor_generation"] = ready["generation"].clone();
    replacement.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&replacement, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    let rotated: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert_eq!(rotated["result"]["registered"], true);
    let effects = fixture.service.persistence.take_token_usage_effects();
    assert_eq!(
        effects.len(),
        1,
        "actual handoff lost its bounded publication work"
    );
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } =
        effects.into_iter().next().unwrap()
    else {
        panic!("missing observer publication");
    };
    assert_eq!(
        store.external_source_activation(&key).unwrap().unwrap(),
        before
    );
    assert!(
        publication.prepare_source_renewal().is_none(),
        "handoff reset the original physical worker budget"
    );
    assert!(
        fixture
            .service
            .complete_external_source_renewal(&work)
            .is_none(),
        "caller fabricated native completion"
    );
    assert!(work.execute_persistence().await);
    let current = store.external_source_activation(&key).unwrap().unwrap();
    assert_eq!(current.observer.as_ref().unwrap().generation(), 2);
    assert_eq!(current.activation, before.activation);
    assert_eq!(current.revision, before.revision);
    assert_eq!(current.renewal, before.renewal);
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        Some(true)
    );
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        None
    );
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRetirement { work: end } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("missing current retirement");
    };
    assert!(
        tokio::task::spawn_blocking(move || end.execute_for_publication())
            .await
            .unwrap()
            .is_ok()
    );
    let ended = store.external_source_activation(&key).unwrap().unwrap();
    assert_eq!(
        ended.state,
        crate::storage::token_usage::ExternalSourceActivationState::Retired
    );
    assert_eq!(ended.observer, current.observer);
    assert_eq!(ended.revision, 3);
}

/// The current pending scope must close safely on either side of its actual
/// SQL commit, before final actor metadata acknowledgment. Precommit retirement
/// prevents the queued mutation; postcommit retirement sees its own sealed G2
/// receipt, even though final publication is stale. Neither ordering resurrects
/// the source or learns a foreign database scope.
#[tokio::test(flavor = "current_thread")]
async fn external_source_observer_retirement_fences_pending_and_committed_publication() {
    for commit_first in [false, true] {
        let Some((mut fixture, store, publication, ready)) = accepted_source().await else {
            return;
        };
        let _observer = crate::runtime::UnixOriginStream::new(
            &mut fixture.socket,
            Some(publication.origin.clone()),
        );
        let (key, _, _) = publication
            .source_preparation
            .get()
            .unwrap()
            .observer_scope_for_tests();
        let mut replacement = request();
        let mut params: serde_json::Value =
            serde_json::from_str(replacement.params.as_deref().unwrap()).unwrap();
        params["observer_instance"] = serde_json::json!("actual-native-successor");
        params["predecessor_generation"] = ready["generation"].clone();
        replacement.params = Some(params.to_string());
        let work = fixture
            .service
            .prepare_external_enrollment(&replacement, &fixture.connection)
            .unwrap();
        let native = work.clone();
        let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
            .await
            .unwrap();
        let rotated: serde_json::Value = serde_json::from_str(
            &fixture
                .service
                .complete_external_enrollment(work, observed, &fixture.connection),
        )
        .unwrap();
        assert_eq!(rotated["result"]["registered"], true);
        let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = fixture
            .service
            .persistence
            .take_token_usage_effects()
            .pop()
            .unwrap()
        else {
            panic!("missing observer publication");
        };
        if commit_first {
            assert!(work.execute_persistence().await);
        }
        let digest = *fixture
            .service
            .control
            .external_agents()
            .bindings
            .keys()
            .next()
            .unwrap();
        fixture.service.retire_external_agent_binding(digest);
        let crate::runtime::RuntimeSideEffect::PersistExternalSourceRetirement { work: end } =
            fixture
                .service
                .persistence
                .take_token_usage_effects()
                .pop()
                .unwrap()
        else {
            panic!("missing retirement");
        };
        assert!(
            tokio::task::spawn_blocking(move || end.execute_for_publication())
                .await
                .unwrap()
                .is_ok()
        );
        if !commit_first {
            assert!(!work.execute_persistence().await);
        }
        assert_eq!(
            fixture.service.complete_external_source_renewal(&work),
            None
        );
        let ended = store.external_source_activation(&key).unwrap().unwrap();
        assert_eq!(
            ended.state,
            crate::storage::token_usage::ExternalSourceActivationState::Retired
        );
        assert_eq!(
            ended.observer.unwrap().generation(),
            if commit_first { 2 } else { 1 }
        );
        assert_eq!(ended.revision, 3);
    }
}
