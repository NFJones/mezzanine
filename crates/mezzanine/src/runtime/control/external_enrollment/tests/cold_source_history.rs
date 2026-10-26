//! Regression controls for required publication and complete durable history.
//! All authority uses the actual generated producer and Unix/ancestry proof.

use super::cold_source_recovery::enroll;
use super::source_activation_fixture::accepted_source;
use super::*;

/// A future predecessor guess is not the actor predecessor captured when native
/// history was marked. C premarks first against A, B supersedes it and publishes
/// complete history, then delayed C must not adopt from its old cached success.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_superseded_history_mark_cannot_authorize_delayed_adoption() {
    let Some((mut fixture, store, original, ready)) = accepted_source().await else {
        return;
    };
    let _observer =
        crate::runtime::UnixOriginStream::new(&mut fixture.socket, Some(original.origin.clone()));
    let (key, _, _) = original
        .source_preparation
        .get()
        .unwrap()
        .observer_scope_for_tests();
    let mut c = request();
    let mut params: serde_json::Value = serde_json::from_str(c.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = serde_json::json!("delayed-history-C");
    params["predecessor_generation"] = serde_json::json!(ready["generation"].as_u64().unwrap() + 1);
    c.params = Some(params.to_string());
    let delayed = fixture
        .service
        .prepare_external_enrollment(&c, &fixture.connection)
        .unwrap();
    let native = delayed.clone();
    let delayed_observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(delayed_observed.is_ok());
    let mut b = request();
    params["observer_instance"] = serde_json::json!("superseding-history-B");
    params["predecessor_generation"] = ready["generation"].clone();
    b.params = Some(params.to_string());
    let b_ready = enroll(&mut fixture.service, &fixture.connection, &b).await;
    assert_eq!(b_ready["result"]["registered"], true);
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("missing B publication");
    };
    assert!(work.execute_persistence().await);
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        Some(true)
    );
    drop(work);
    assert!(store.external_source_history_complete(&key).unwrap());
    let before = store.external_source_activation(&key).unwrap().unwrap();
    let refused: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_enrollment(
            delayed,
            delayed_observed,
            &fixture.connection,
        ))
        .unwrap();
    assert!(
        refused.get("error").is_some(),
        "superseded native history mark admitted a new identity after completeness was restored"
    );
    assert!(refused.get("result").is_none());
    assert_eq!(
        store.external_source_activation(&key).unwrap(),
        Some(before)
    );
    assert!(store.external_source_history_complete(&key).unwrap());
    assert!(fixture.service.control.external_agents().bindings.values().filter(|binding|!binding.retired).all(|binding|binding.enrollment.as_ref().unwrap().instance=="superseding-history-B"));
}

/// A real timer-queue refusal after native actor registration must discard only
/// the unexposed cold grant, retain original durable facts and allow same-intent
/// fresh qualification. Runs the actual actor completion method/full timer lane.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_actor_timer_refusal_cleans_unpublished_authority() {
    let Some((mut fixture, store, original, ready)) = accepted_source().await else {
        return;
    };
    let _observer =
        crate::runtime::UnixOriginStream::new(&mut fixture.socket, Some(original.origin.clone()));
    let (key, _, _) = original
        .source_preparation
        .get()
        .unwrap()
        .observer_scope_for_tests();
    let before = store.external_source_activation(&key).unwrap().unwrap();
    fixture.service.retire_all_external_source_anchors();
    fixture.service.control = crate::runtime::control::RuntimeControlComponent::new(
        crate::control::ControlIdempotencyCache::default(),
        mez_agent::messaging::MessageService::default(),
        None,
    );
    let mut cold = request();
    let mut params: serde_json::Value =
        serde_json::from_str(cold.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = serde_json::json!("timer-refusal-B");
    params["durable_source_resume"] =
        serde_json::json!({"receipt":ready["source_receipt"],"operation_id":"b".repeat(64)});
    cold.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&cold, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(observed.is_ok());
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (service, error) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::full_timer_lane_enrollment_for_tests(
            service,
            work,
            observed,
            fixture.connection.clone(),
        );
    fixture.service = service;
    assert!(
        error
            .unwrap()
            .message()
            .contains("side-effect queue is full")
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| binding.retired)
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
    assert_eq!(
        store.external_source_activation(&key).unwrap(),
        Some(before)
    );
    assert!(
        fixture
            .service
            .persistence
            .take_token_usage_effects()
            .is_empty()
    );
    let recovered = enroll(&mut fixture.service, &fixture.connection, &cold).await;
    assert_eq!(recovered["result"]["registered"], true);
}

/// An ordinary new observer cannot turn a tentative cold actor grant into warm
/// authority. Cover a native request captured BEFORE that grant and a later
/// prepare, then prove genuinely published cold authority can rotate normally.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_unpublished_grant_cannot_publish_an_ordinary_successor() {
    let Some((mut fixture, store, original, ready)) = accepted_source().await else {
        return;
    };
    let _observer =
        crate::runtime::UnixOriginStream::new(&mut fixture.socket, Some(original.origin.clone()));
    fixture.service.retire_all_external_source_anchors();
    fixture.service.control = crate::runtime::control::RuntimeControlComponent::new(
        crate::control::ControlIdempotencyCache::default(),
        mez_agent::messaging::MessageService::default(),
        None,
    );
    let mut cold = request();
    let mut params: serde_json::Value =
        serde_json::from_str(cold.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = serde_json::json!("pending-cold-B");
    params["durable_source_resume"] =
        serde_json::json!({"receipt":ready["source_receipt"],"operation_id":"b".repeat(64)});
    cold.params = Some(params.to_string());
    let mut ordinary = request();
    let mut ordinary_params: serde_json::Value =
        serde_json::from_str(ordinary.params.as_deref().unwrap()).unwrap();
    ordinary_params["observer_instance"] = serde_json::json!("ordinary-C");
    ordinary_params["predecessor_generation"] = serde_json::json!(1);
    ordinary.params = Some(ordinary_params.to_string());
    let raced = fixture
        .service
        .prepare_external_enrollment(&ordinary, &fixture.connection)
        .unwrap();
    let native = raced.clone();
    let raced_result = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(raced_result.is_ok());
    let work = fixture
        .service
        .prepare_external_enrollment(&cold, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(observed.is_ok());
    let body =
        fixture
            .service
            .complete_external_enrollment(work.clone(), observed, &fixture.connection);
    let pending: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(pending["result"]["registered"], true);
    let refused: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(raced, raced_result, &fixture.connection),
    )
    .unwrap();
    assert!(refused.get("error").is_some());
    assert!(refused.get("result").is_none());
    assert!(
        fixture
            .service
            .prepare_external_enrollment(&ordinary, &fixture.connection)
            .is_err()
    );
    let publication = fixture
        .service
        .begin_external_source_publication(work, body)
        .unwrap();
    let native = publication.clone();
    let committed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(committed.is_ok());
    let published: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_source_publication(
            publication,
            committed,
            &fixture.connection,
        ))
        .unwrap();
    assert_eq!(published["result"]["registered"], true);
    let rotated = enroll(&mut fixture.service, &fixture.connection, &ordinary).await;
    assert_eq!(rotated["result"]["registered"], true);
    assert_eq!(
        rusqlite::Connection::open(store.path())
            .unwrap()
            .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

/// A->B really admits B, but its optional observer write fails. C's successful
/// publication MUST include B, not just A/C, before history becomes cold-eligible.
/// After full control replacement old B refuses and fresh D genuinely succeeds.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_failed_warm_history_is_repaired_completely_or_refuses() {
    for repair in [false, true] {
        let Some((mut fixture, store, original, ready)) = accepted_source().await else {
            return;
        };
        let _observer = crate::runtime::UnixOriginStream::new(
            &mut fixture.socket,
            Some(original.origin.clone()),
        );
        let (key, _, _) = original
            .source_preparation
            .get()
            .unwrap()
            .observer_scope_for_tests();
        let mut b = request();
        let mut params: serde_json::Value =
            serde_json::from_str(b.params.as_deref().unwrap()).unwrap();
        params["observer_instance"] = serde_json::json!("missing-history-B");
        params["predecessor_generation"] = ready["generation"].clone();
        b.params = Some(params.to_string());
        let b_ready = enroll(&mut fixture.service, &fixture.connection, &b).await;
        assert_eq!(b_ready["result"]["registered"], true);
        assert!(!store.external_source_history_complete(&key).unwrap());
        let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = fixture
            .service
            .persistence
            .take_token_usage_effects()
            .pop()
            .unwrap()
        else {
            panic!("missing B publication");
        };
        let database = rusqlite::Connection::open(store.path()).unwrap();
        database.execute_batch("CREATE TRIGGER warm_history_fault BEFORE UPDATE OF observer_fence ON external_usage_source_activations BEGIN SELECT RAISE(ABORT,'warm-history-fixture-failure'); END;").unwrap();
        assert!(!work.execute_persistence().await);
        assert_eq!(
            fixture.service.complete_external_source_renewal(&work),
            Some(false)
        );
        drop(work);
        assert!(!store.external_source_history_complete(&key).unwrap());
        database
            .execute_batch("DROP TRIGGER warm_history_fault")
            .unwrap();
        if !repair {
            fixture.service.retire_all_external_source_anchors();
            fixture.service.control = crate::runtime::control::RuntimeControlComponent::new(
                crate::control::ControlIdempotencyCache::default(),
                mez_agent::messaging::MessageService::default(),
                None,
            );
            let mut cold = request();
            let mut params: serde_json::Value =
                serde_json::from_str(cold.params.as_deref().unwrap()).unwrap();
            params["observer_instance"] = serde_json::json!("fresh-but-history-unknown-D");
            params["durable_source_resume"] = serde_json::json!({"receipt":ready["source_receipt"],"operation_id":"d".repeat(64)});
            cold.params = Some(params.to_string());
            assert!(
                enroll(&mut fixture.service, &fixture.connection, &cold)
                    .await
                    .get("error")
                    .is_some()
            );
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
                    .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            continue;
        }
        params["observer_instance"] = serde_json::json!("successful-history-C");
        params["predecessor_generation"] = b_ready["result"]["generation"].clone();
        let mut c = request();
        c.params = Some(params.to_string());
        let c_ready = enroll(&mut fixture.service, &fixture.connection, &c).await;
        assert_eq!(c_ready["result"]["registered"], true);
        let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = fixture
            .service
            .persistence
            .take_token_usage_effects()
            .pop()
            .unwrap()
        else {
            panic!("missing C publication");
        };
        assert!(work.execute_persistence().await);
        assert_eq!(
            fixture.service.complete_external_source_renewal(&work),
            Some(true)
        );
        drop(work);
        assert!(store.external_source_history_complete(&key).unwrap());
        let fence = store
            .external_source_activation(&key)
            .unwrap()
            .unwrap()
            .observer
            .unwrap();
        assert!(fence.contains_instance("missing-history-B").unwrap());
        fixture.service.retire_all_external_source_anchors();
        fixture.service.control = crate::runtime::control::RuntimeControlComponent::new(
            crate::control::ControlIdempotencyCache::default(),
            mez_agent::messaging::MessageService::default(),
            None,
        );
        fixture
            .service
            .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
                store.path(),
            ));
        let mut cold = request();
        let mut params: serde_json::Value =
            serde_json::from_str(cold.params.as_deref().unwrap()).unwrap();
        params["observer_instance"] = serde_json::json!("missing-history-B");
        params["durable_source_resume"] = serde_json::json!({"receipt":{"protocol":"external-source-activation/1","activation_id":ready["source_receipt"]["activation_id"],"observer_generation":fence.generation().to_string()},"operation_id":"d".repeat(64)});
        cold.params = Some(params.to_string());
        assert!(
            enroll(&mut fixture.service, &fixture.connection, &cold)
                .await
                .get("error")
                .is_some()
        );
        params["observer_instance"] = serde_json::json!("fresh-history-D");
        cold.params = Some(params.to_string());
        let fresh = enroll(&mut fixture.service, &fixture.connection, &cold).await;
        assert_eq!(fresh["result"]["registered"], true);
        assert_eq!(fresh["result"]["recovery"]["kind"], "durable-native-source");
    }
}

/// Failed pre-admission storage cannot leave an unrecorded warm identity, even
/// if a caller fabricates outer Ok. The old current private handle survives.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_history_fence_failure_cannot_admit_new_warm_identity() {
    let Some((mut fixture, store, original, ready)) = accepted_source().await else {
        return;
    };
    let _observer =
        crate::runtime::UnixOriginStream::new(&mut fixture.socket, Some(original.origin.clone()));
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database.execute_batch("CREATE TRIGGER refuse_history_intent BEFORE UPDATE OF observer_history_gap ON external_usage_source_activations BEGIN SELECT RAISE(ABORT,'history-intent-fixture-refusal'); END;").unwrap();
    let mut successor = request();
    let mut params: serde_json::Value =
        serde_json::from_str(successor.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = serde_json::json!("unrecorded-warm-B");
    params["predecessor_generation"] = ready["generation"].clone();
    successor.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&successor, &fixture.connection)
        .unwrap();
    let native = work.clone();
    assert!(
        tokio::task::spawn_blocking(move || native.execute_enrollment())
            .await
            .unwrap()
            .is_err()
    );
    let refused: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, Ok(()), &fixture.connection),
    )
    .unwrap();
    assert!(refused.get("error").is_some());
    assert!(refused.get("result").is_none());
    let current = enroll(&mut fixture.service, &fixture.connection, &request()).await;
    assert_eq!(current["result"]["launch_token"], ready["launch_token"]);
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| !binding.retired)
    );
}
