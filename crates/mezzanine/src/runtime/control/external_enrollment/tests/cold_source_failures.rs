//! Required cold claims fail closed without exposing tentative private grants.
//! Native proof is always genuine; only fixture SQL faults or quota/epoch
//! metadata are synthetic. No old task/provider/action effect is replayed.

use super::*;

/// A retained same-control source/retired fence must use its existing warm
/// authority and physical budget. Cold selectors cannot override it, even when
/// genuine stored acceptance and the exact same live producer are present.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_retained_same_control_owner_cannot_be_bypassed() {
    let Some((mut fixture, _store, _original, ready)) =
        super::source_activation_fixture::accepted_source().await
    else {
        return;
    };
    let mut request = request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = serde_json::json!("cold-bypass-instance");
    params["durable_source_resume"] =
        serde_json::json!({"receipt":ready["source_receipt"],"operation_id":"b".repeat(64)});
    request.params = Some(params.to_string());
    assert_eq!(
        fixture
            .service
            .prepare_external_enrollment(&request, &fixture.connection)
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::Conflict
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
}

/// Closed selectors reject unsafe representations before any metadata/native
/// reservation. Synthetic public IDs exercise grammar only, never native proof.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_malformed_selectors_reserve_nothing() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let valid = serde_json::json!({"receipt":{"protocol":"external-source-activation/1","activation_id":"a".repeat(64),"observer_generation":"1"},"operation_id":"b".repeat(64)});
    let mut invalid = vec![
        serde_json::Value::Null,
        serde_json::json!(false),
        serde_json::json!([]),
    ];
    for generation in [
        serde_json::json!(1),
        serde_json::json!("0"),
        serde_json::json!("01"),
        serde_json::json!("-1"),
        serde_json::json!("9223372036854775807"),
        serde_json::json!("18446744073709551616"),
    ] {
        let mut value = valid.clone();
        value["receipt"]["observer_generation"] = generation;
        invalid.push(value);
    }
    for id in ["A".repeat(64), "0".repeat(64), "x".repeat(43)] {
        let mut value = valid.clone();
        value["operation_id"] = serde_json::json!(id);
        invalid.push(value);
    }
    let mut extra = valid.clone();
    extra["receipt"]["owner"] = serde_json::json!("not-authority");
    invalid.push(extra);
    let mut extra = valid.clone();
    extra["force"] = serde_json::json!(true);
    invalid.push(extra);
    for value in invalid {
        let mut request = request();
        let mut params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
        params["durable_source_resume"] = value;
        request.params = Some(params.to_string());
        assert_eq!(
            fixture
                .service
                .prepare_external_enrollment(&request, &fixture.connection)
                .unwrap_err()
                .kind(),
            crate::error::MezErrorKind::InvalidArgs
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
                .is_empty()
        );
    }
    for mixed in ["source_resume", "predecessor_generation"] {
        let mut request = request();
        let mut params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
        params["durable_source_resume"] = valid.clone();
        params[mixed] = serde_json::Value::Null;
        request.params = Some(params.to_string());
        assert!(
            fixture
                .service
                .prepare_external_enrollment(&request, &fixture.connection)
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
    }
}

/// Genuine native observation alone writes Prepared but no accepted source.
/// Even an untrusted selector naming its exact public attempt cannot recover it
/// after control replacement; no second namespace or private binding appears.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_prepared_mapping_never_recovers_without_actual_acceptance() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store = crate::storage::token_usage::TokenUsageStore::new(
        fixture.directory.join("prepared.sqlite"),
    );
    fixture.service.set_token_usage_store(store.clone());
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    assert!(
        tokio::task::spawn_blocking(move || native.execute_enrollment())
            .await
            .unwrap()
            .is_ok()
    );
    let (_, activation, _) = work
        .source_preparation
        .get()
        .unwrap()
        .observer_scope_for_tests();
    fixture.service.control = crate::runtime::control::RuntimeControlComponent::new(
        crate::control::ControlIdempotencyCache::default(),
        mez_agent::messaging::MessageService::default(),
        None,
    );
    let mut request = request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["durable_source_resume"] = serde_json::json!({"receipt":{"protocol":"external-source-activation/1","activation_id":activation.public_id(),"observer_generation":"1"},"operation_id":"b".repeat(64)});
    request.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&request, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(observed.is_err());
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
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
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
}

/// A cold source needs its own actual commit and current final publication,
/// unlike optional first-admission metadata. Failure must not tombstone another
/// winner or remove a replacement registry's coincident reservation.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_required_publication_failures_do_not_grant_or_retire_foreign_scope() {
    for fault in [
        "unexecuted",
        "sql",
        "registry",
        "store",
        "retired",
        "headroom",
    ] {
        let Some((mut fixture, store, original, ready)) =
            super::source_activation_fixture::accepted_source().await
        else {
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
        let mut request = request();
        let mut params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
        params["observer_instance"] = serde_json::json!("cold-fault-instance");
        params["durable_source_resume"] =
            serde_json::json!({"receipt":ready["source_receipt"],"operation_id":"b".repeat(64)});
        request.params = Some(params.to_string());
        let work = fixture
            .service
            .prepare_external_enrollment(&request, &fixture.connection)
            .unwrap();
        let native = work.clone();
        let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
            .await
            .unwrap();
        assert!(observed.is_ok());
        let body = fixture.service.complete_external_enrollment(
            work.clone(),
            observed,
            &fixture.connection,
        );
        let digest = *fixture
            .service
            .control
            .external_agents()
            .bindings
            .keys()
            .next()
            .unwrap();
        if fault == "headroom" {
            fixture
                .service
                .control
                .external_agents_mut()
                .enrollments
                .pending
                .extend(100..132);
            let reply: serde_json::Value = serde_json::from_str(
                &fixture
                    .service
                    .begin_external_source_publication(work, body)
                    .unwrap_err(),
            )
            .unwrap();
            assert!(reply.get("error").is_some());
            assert!(fixture.service.control.external_agents().bindings[&digest].retired);
            let retained = store.external_source_activation(&key).unwrap().unwrap();
            assert_eq!(retained.observer.unwrap().generation(), 1);
            assert_eq!(
                retained.state,
                crate::storage::token_usage::ExternalSourceActivationState::Accepted
            );
            assert!(
                fixture
                    .service
                    .persistence
                    .take_token_usage_effects()
                    .is_empty()
            );
            assert_eq!(
                fixture
                    .service
                    .control
                    .external_agents()
                    .enrollments
                    .pending
                    .len(),
                32
            );
            continue;
        }
        let publication = fixture
            .service
            .begin_external_source_publication(work, body)
            .unwrap();
        let database = rusqlite::Connection::open(store.path()).unwrap();
        if fault == "sql" {
            database.execute_batch("CREATE TRIGGER refuse_cold_claim BEFORE UPDATE ON external_usage_source_activations BEGIN SELECT RAISE(ABORT,'cold-claim-fixture-failure'); END;").unwrap();
        }
        if fault == "retired" {
            fixture.service.retire_external_agent_binding(digest);
            let crate::runtime::RuntimeSideEffect::PersistExternalSourceRetirement { work } =
                fixture
                    .service
                    .persistence
                    .take_token_usage_effects()
                    .pop()
                    .unwrap()
            else {
                panic!("missing exact retirement");
            };
            assert!(
                tokio::task::spawn_blocking(move || work.execute_for_publication())
                    .await
                    .unwrap()
                    .is_ok()
            );
        }
        let result = if fault == "unexecuted" {
            Ok(())
        } else {
            let native = publication.clone();
            tokio::task::spawn_blocking(move || native.execute_enrollment())
                .await
                .unwrap()
        };
        if ["sql", "retired"].contains(&fault) {
            assert!(result.is_err());
        }
        if ["registry", "store"].contains(&fault) {
            assert!(result.is_ok());
        }
        if fault == "registry" {
            fixture.service.control.external_agents_mut().enrollments = Default::default();
            fixture
                .service
                .control
                .external_agents_mut()
                .enrollments
                .pending
                .insert(publication.admission);
        }
        if fault == "store" {
            fixture.service.set_token_usage_store(
                crate::storage::token_usage::TokenUsageStore::new(
                    fixture.directory.join("replacement.sqlite"),
                ),
            );
        }
        let reply: serde_json::Value =
            serde_json::from_str(&fixture.service.complete_external_source_publication(
                publication.clone(),
                result,
                &fixture.connection,
            ))
            .unwrap();
        assert!(
            reply.get("error").is_some(),
            "failed required cold claim leaked ready reply"
        );
        assert!(reply.get("result").is_none());
        let retained = store.external_source_activation(&key).unwrap().unwrap();
        if fault == "registry" {
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
                !fixture.service.control.external_agents().bindings[&digest].retired,
                "stale cleanup mutated replacement registry"
            );
        } else {
            assert!(
                fixture
                    .service
                    .control
                    .external_agents()
                    .enrollments
                    .pending
                    .is_empty()
            );
            assert!(fixture.service.control.external_agents().bindings[&digest].retired);
        }
        if fault == "retired" {
            assert_eq!(
                retained.state,
                crate::storage::token_usage::ExternalSourceActivationState::Retired
            );
            assert_eq!(retained.observer.unwrap().generation(), 1);
        } else {
            assert_eq!(
                retained.state,
                crate::storage::token_usage::ExternalSourceActivationState::Accepted,
                "unexposed failed grant wrote a tombstone"
            );
            assert_eq!(
                retained.observer.unwrap().generation(),
                if ["registry", "store"].contains(&fault) {
                    2
                } else {
                    1
                }
            );
        }
        if fault == "sql" {
            database
                .execute_batch("DROP TRIGGER refuse_cold_claim")
                .unwrap();
            let native = publication.clone();
            assert!(
                tokio::task::spawn_blocking(move || native.execute_enrollment())
                    .await
                    .unwrap()
                    .is_err()
            );
            assert_eq!(
                store
                    .external_source_activation(&key)
                    .unwrap()
                    .unwrap()
                    .observer
                    .unwrap()
                    .generation(),
                1,
                "failed immutable claim replayed"
            );
        }
        assert!(
            fixture
                .service
                .persistence
                .take_token_usage_effects()
                .is_empty(),
            "cleanup queued a foreign durable end"
        );
    }
}

/// Unknown/stale public source selectors and another native session cannot
/// allocate a substitute accounting namespace or silently enter initial mode.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_wrong_receipt_session_legacy_and_retired_refuse_without_allocation() {
    for fault in [
        "activation",
        "generation",
        "session",
        "legacy",
        "retired",
        "expired",
        "initial-noop",
    ] {
        let Some((mut fixture, store, original, ready)) =
            super::source_activation_fixture::accepted_source().await
        else {
            return;
        };
        if fault == "legacy" {
            rusqlite::Connection::open(store.path())
                .unwrap()
                .execute_batch("UPDATE external_usage_source_activations SET observer_fence=NULL")
                .unwrap();
        }
        if fault == "expired" {
            // Valid but expired fixture metadata, not a changed host clock or
            // a forged native lifetime. The actual producer remains alive.
            rusqlite::Connection::open(store.path()).unwrap().execute_batch(
                "UPDATE external_usage_source_activations SET prepared_at_boot_ms=1,expires_at_boot_ms=2"
            ).unwrap();
        }
        if fault == "retired" {
            let work = original.source_preparation.retire().unwrap();
            assert!(
                tokio::task::spawn_blocking(move || work.execute_for_publication())
                    .await
                    .unwrap()
                    .is_ok()
            );
        }
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
        let mut request = request();
        let mut params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
        params["observer_instance"] = serde_json::json!("cold-negative-instance");
        params["durable_source_resume"] =
            serde_json::json!({"receipt":ready["source_receipt"],"operation_id":"b".repeat(64)});
        match fault {
            "activation" => {
                params["durable_source_resume"]["receipt"]["activation_id"] =
                    serde_json::json!("f".repeat(64))
            }
            "generation" => {
                params["durable_source_resume"]["receipt"]["observer_generation"] =
                    serde_json::json!("2")
            }
            "session" => {
                params["external_session_id"] = serde_json::json!("different-source-session")
            }
            "initial-noop" => {
                params["observer_instance"] = ready["observer_instance"].clone();
                params["durable_source_resume"]["operation_id"] =
                    ready["source_receipt"]["activation_id"].clone();
            }
            _ => {}
        }
        request.params = Some(params.to_string());
        let work = fixture
            .service
            .prepare_external_enrollment(&request, &fixture.connection)
            .unwrap();
        let native = work.clone();
        let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
            .await
            .unwrap();
        assert!(observed.is_err());
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
        let database = rusqlite::Connection::open(store.path()).unwrap();
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
