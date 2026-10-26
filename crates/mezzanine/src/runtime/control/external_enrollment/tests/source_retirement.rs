//! Native source-lifetime retirement, separate from observer rotation.
//!
//! Actual generated Unix producers and successful actor registration establish
//! the source. No synthetic grant, parent capability or payload identity stands
//! in for native evidence. Database inspection checks inert metadata only.

use super::*;

/// Retirement may race the already-admitted optional acceptance phase. Logical
/// source retirement must latch before dropping proof, so a delayed worker
/// cannot newly accept it merely because its producer and socket remain live.
/// The actor itself must not perform the eventual SQLite tombstone operation.
#[tokio::test(flavor = "current_thread")]
async fn external_source_retirement_fences_pending_acceptance() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store = crate::storage::token_usage::TokenUsageStore::new(
        fixture.directory.join("retirement.sqlite"),
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
    let body =
        fixture
            .service
            .complete_external_enrollment(work.clone(), observed, &fixture.connection);
    let publication = fixture
        .service
        .begin_external_source_publication(work, body)
        .unwrap();
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    let slot = publication.source_preparation.clone();
    let io = slot.hold_io_for_tests();
    fixture.service.retire_external_agent_binding(digest);
    drop(io);
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT state,revision FROM external_usage_source_activations",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (0, 1),
        "actor retirement must do no SQLite I/O"
    );
    let native = publication.clone();
    let accepted = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(
        accepted.is_err(),
        "a retired source must not newly accept metadata"
    );
    let effects = fixture.service.persistence.take_token_usage_effects();
    assert_eq!(effects.len(), 1);
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRetirement { work } =
        effects.into_iter().next().unwrap()
    else {
        panic!("missing exact retirement work")
    };
    let native = work.clone();
    tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT state,revision FROM external_usage_source_activations",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (2, 2)
    );
    tokio::task::spawn_blocking(move || work.execute())
        .await
        .unwrap()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    assert!(
        fixture
            .service
            .persistence
            .take_token_usage_effects()
            .is_empty()
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

/// Establishes real native observation, actual registration and its separately
/// persisted acceptance. Only fixture-owned stores/selectors are inspected; the
/// private reply is never formatted as failure diagnostics.
async fn accepted_source() -> Option<(
    Fixture,
    crate::storage::token_usage::TokenUsageStore,
    ExternalEnrollmentWork,
    serde_json::Value,
)> {
    let mut fixture = fixture("hold").await?;
    let store = crate::storage::token_usage::TokenUsageStore::new(
        fixture.directory.join("retirement.sqlite"),
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
    let body =
        fixture
            .service
            .complete_external_enrollment(work.clone(), observed, &fixture.connection);
    let publication = fixture
        .service
        .begin_external_source_publication(work, body)
        .unwrap_or_else(|_| panic!("actual admission did not grant publication"));
    let native = publication.clone();
    let accepted = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(accepted.is_ok(), "{accepted:?}");
    let ready: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_source_publication(
            publication.clone(),
            accepted,
            &fixture.connection,
        ))
        .unwrap();
    assert_eq!(ready["result"]["registered"], true);
    Some((fixture, store, publication, ready["result"].clone()))
}

/// Required end replies need actual worker proof, not supplied success. Logical
/// closure precedes storage, a fresh identical request may obtain the original
/// committed tombstone, and duplicate completion cannot reuse a reservation.
#[tokio::test(flavor = "current_thread")]
async fn external_source_end_requires_worker_proof_and_exact_publication() {
    let Some((mut fixture, store, publication, ready)) = accepted_source().await else {
        return;
    };
    let end = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":41,"method":"agent/external/source-end","params":{"launch_token":ready["launch_token"],"generation":ready["generation"],"external_session_id":"session-a"}}).to_string()).unwrap();
    let slot = publication.source_preparation.clone();
    let guard = slot.hold_io_for_tests();
    let first = fixture
        .service
        .prepare_external_source_end(&end, &fixture.connection)
        .unwrap();
    drop(guard);
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(state(&database), (1, 2), "actor end performed SQLite work");
    let rejected: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_source_end(first, Ok(()), &fixture.connection),
    )
    .unwrap();
    assert!(
        rejected.get("error").is_some(),
        "supplied success became durable proof"
    );
    let work = fixture
        .service
        .prepare_external_source_end(&end, &fixture.connection)
        .unwrap();
    let duplicate = work.clone();
    let native = work.clone();
    let outcome = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    assert!(outcome.is_ok());
    let body: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_source_end(work, outcome, &fixture.connection),
    )
    .unwrap();
    assert_eq!(body["result"]["protocol"], "external-source-end/1");
    assert_eq!(body["result"]["durable"], true);
    assert_eq!(body["result"]["generation"], ready["generation"]);
    assert_eq!(state(&database), (2, 3));
    let again: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_source_end(duplicate, Ok(()), &fixture.connection),
    )
    .unwrap();
    assert!(again.get("error").is_some());
    assert_eq!(
        fixture.service.persistence.take_token_usage_effects().len(),
        1,
        "end replay queued another original retirement"
    );
}

/// Both actual framed actor ingress paths retain the reply while I/O is gated.
/// A second rejected request remains responsive, then real commit precedes the
/// durable acknowledgment. No parent-supplied capability replaces native proof.
#[tokio::test(flavor = "current_thread")]
async fn external_source_end_actor_frames_wait_for_commit_and_keep_control_live() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    for (snapshots, lose_reply) in [(false, false), (true, false), (false, true)] {
        let Some((mut fixture, store, _publication, ready)) = accepted_source().await else {
            return;
        };
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let settled = Arc::new(tokio::sync::Notify::new());
        fixture
            .service
            .control
            .external_agents_mut()
            .enrollments
            .settled = Some(settled.clone());
        fixture
            .service
            .control
            .external_agents_mut()
            .enrollments
            .worker_gate = Some((started.clone(), release.clone()));
        let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let mut owned = tokio::task::JoinSet::new();
        owned.spawn(actor.run());
        let body = serde_json::json!({"jsonrpc":"2.0","id":43,"method":"agent/external/source-end","params":{"launch_token":ready["launch_token"],"generation":ready["generation"],"external_session_id":"session-a"}}).to_string();
        let input = crate::control::encode_control_body(&body);
        let client = handle.clone();
        let connection = fixture.connection.clone();
        let repository = crate::storage::snapshot::SnapshotRepository::new(
            fixture.directory.join("end-snapshots"),
        );
        let pending = tokio::spawn(async move {
            if snapshots {
                client
                    .handle_control_input_for_connection_with_snapshots(
                        input, 8192, connection, repository,
                    )
                    .await
            } else {
                client
                    .handle_control_input_for_connection(input, 8192, connection)
                    .await
            }
        });
        started.notified().await;
        assert!(
            !pending.is_finished(),
            "durable reply preceded worker proof"
        );
        let database = rusqlite::Connection::open(store.path()).unwrap();
        assert_eq!(state(&database), (1, 2));
        let invalid = crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":44,"method":"agent/external/source-end","params":{}}"#,
        );
        let sibling = handle
            .handle_control_input_for_connection(invalid, 8192, fixture.connection.clone())
            .await
            .unwrap();
        let (body, _) = crate::control::decode_control_frame(&sibling.output, 8192).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&body)
                .unwrap()
                .get("error")
                .is_some()
        );
        assert!(!pending.is_finished());
        if lose_reply {
            // Receiver cancellation only, not a claimed process/socket crash.
            pending.abort();
            assert!(pending.await.unwrap_err().is_cancelled());
            release.notify_one();
            settled.notified().await;
            assert_eq!(state(&database), (2, 3));
            handle.shutdown().await.unwrap();
            fixture.service = owned.join_next().await.unwrap().unwrap().service;
            assert!(
                fixture
                    .service
                    .control
                    .external_agents()
                    .enrollments
                    .pending
                    .is_empty()
            );
            continue;
        }
        release.notify_one();
        let result = pending.await.unwrap().unwrap();
        assert!(!result.connection.initialized());
        let (body, consumed) = crate::control::decode_control_frame(&result.output, 8192).unwrap();
        assert_eq!(consumed, result.output.len());
        assert!(!body.contains(ready["launch_token"].as_str().unwrap()));
        let reply: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply["result"]["durable"], true);
        assert_eq!(state(&database), (2, 3));
        handle.shutdown().await.unwrap();
        fixture.service = owned.join_next().await.unwrap().unwrap().service;
    }
}

/// Failed storage cannot publish durability or restore native authority; a
/// later request cannot renew an exhausted write allowance. Conversely, real
/// worker proof cannot publish into a changed observer binding generation.
#[tokio::test(flavor = "current_thread")]
async fn external_source_end_storage_failure_and_changed_binding_refuse_success() {
    for failed_store in [false, true] {
        let Some((mut fixture, store, publication, ready)) = accepted_source().await else {
            return;
        };
        let end = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":45,"method":"agent/external/source-end","params":{"launch_token":ready["launch_token"],"generation":ready["generation"],"external_session_id":"session-a"}}).to_string()).unwrap();
        let database = rusqlite::Connection::open(store.path()).unwrap();
        if failed_store {
            database.execute_batch("CREATE TRIGGER reject_end AFTER UPDATE ON external_usage_source_activations WHEN NEW.state=2 BEGIN SELECT RAISE(ABORT,'fixture-private-storage-marker'); END;").unwrap();
        }
        let work = fixture
            .service
            .prepare_external_source_end(&end, &fixture.connection)
            .unwrap();
        let native = work.clone();
        let result = tokio::task::spawn_blocking(move || native.execute())
            .await
            .unwrap();
        assert_eq!(result.is_err(), failed_store);
        assert!(
            fixture
                .service
                .control
                .external_agents()
                .bindings
                .values()
                .all(|binding| binding.retired)
        );
        if !failed_store {
            // Negative only: no new native observer is authorized by this edit.
            fixture
                .service
                .control
                .external_agents_mut()
                .bindings
                .values_mut()
                .next()
                .unwrap()
                .generation += 1;
        }
        let response =
            fixture
                .service
                .complete_external_source_end(work, result, &fixture.connection);
        if failed_store {
            assert!(
                publication
                    .source_preparation
                    .get()
                    .unwrap()
                    .retirement_outcome_for_tests()
                    .unwrap()
                    .as_ref()
                    .unwrap_err()
                    .message()
                    .contains("fixture-private-storage-marker"),
                "original private cause was not retained"
            );
            assert!(
                !response.contains("fixture-private-storage-marker"),
                "private storage diagnostic escaped into source-end reply"
            );
        }
        let reply: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(reply.get("error").is_some());
        assert!(
            fixture
                .service
                .control
                .external_agents()
                .enrollments
                .pending
                .is_empty()
        );
        if failed_store {
            database.execute_batch("DROP TRIGGER reject_end").unwrap();
            let retry = fixture
                .service
                .prepare_external_source_end(&end, &fixture.connection)
                .unwrap();
            let native = retry.clone();
            let result = tokio::task::spawn_blocking(move || native.execute())
                .await
                .unwrap();
            assert!(result.is_err());
            let reply: serde_json::Value = serde_json::from_str(
                &fixture
                    .service
                    .complete_external_source_end(retry, result, &fixture.connection),
            )
            .unwrap();
            assert!(reply.get("error").is_some());
            assert_eq!(
                state(&database),
                (1, 2),
                "new end request replenished recovery writes"
            );
        }
    }
}

/// Required replies share the existing finite admission pool. Exhaustion never
/// claims durability; invalidated reservations cannot consume replacement IDs.
#[tokio::test(flavor = "current_thread")]
async fn external_source_end_capacity_and_registry_fences_are_exact() {
    let Some((mut fixture, _, _, ready)) = accepted_source().await else {
        return;
    };
    let end = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":42,"method":"agent/external/source-end","params":{"launch_token":ready["launch_token"],"generation":ready["generation"],"external_session_id":"session-a"}}).to_string()).unwrap();
    // Capacity metadata only, never a substitute for actual native admission.
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .pending
        .extend((0..MAX_PENDING).map(|id| id as u64));
    assert!(
        fixture
            .service
            .prepare_external_source_end(&end, &fixture.connection)
            .is_err()
    );
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .pending
        .clear();
    let old = fixture
        .service
        .prepare_external_source_end(&end, &fixture.connection)
        .unwrap();
    let physical = old.clone();
    let old_id = fixture.service.control.external_agents().enrollments.next;
    fixture.service.invalidate_external_enrollment_admissions();
    assert!(
        fixture
            .service
            .prepare_external_source_end(&end, &fixture.connection)
            .is_err(),
        "registry reset released physical reply capacity"
    );
    // Reused numeric metadata is not replacement work or native authority.
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .pending
        .insert(old_id);
    let stale: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_source_end(old, Ok(()), &fixture.connection),
    )
    .unwrap();
    assert!(stale.get("error").is_some());
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
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .pending
        .clear();
    drop(physical);
    let current = fixture
        .service
        .prepare_external_source_end(&end, &fixture.connection)
        .unwrap();
    let native = current.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_source_end(current, result, &fixture.connection),
    )
    .unwrap();
    assert_eq!(body["result"]["durable"], true);
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

/// Observer handoff preserves the original source receipt and its finite permit.
/// A stale old handle cannot retire the successor. Current source retirement
/// queues exactly once and the actual actor persistence service writes Retired3,
/// without changing any expense or reusing registration credentials. Normal and
/// lost original/recovery receipt cases each COMMIT once. A real original SQLite
/// rollback followed by restored storage permits one exact-predecessor recovery;
/// both outcomes and the original store remain fixed. Unknown effects never
/// replay, and a separately lost recovery receipt uses only positive read proof.
#[tokio::test(flavor = "current_thread")]
async fn external_source_retirement_survives_rotation_and_uses_persistence_worker() {
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeSessionActor, AsyncRuntimeSideEffectServiceConfig,
        run_async_persistence_side_effect_service,
    };
    for mode in [
        "normal",
        "lost-ack",
        "precommit-restored",
        "precommit-restored-lost-ack",
    ] {
        let Some((mut fixture, store, publication, ready)) = accepted_source().await else {
            return;
        };
        let old_digest = *fixture
            .service
            .control
            .external_agents()
            .bindings
            .keys()
            .next()
            .unwrap();
        let mut replacement = request();
        let mut params: serde_json::Value =
            serde_json::from_str(replacement.params.as_deref().unwrap()).unwrap();
        params["observer_instance"] = serde_json::json!("fixture-instance-successor");
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
        assert_ne!(rotated["result"]["generation"], ready["generation"]);
        fixture.service.retire_external_agent_binding(old_digest);
        assert!(
            fixture
                .service
                .persistence
                .take_token_usage_effects()
                .is_empty()
        );
        let database = rusqlite::Connection::open(store.path()).unwrap();
        assert_eq!(state(&database), (1, 2));
        let digest = *fixture
            .service
            .control
            .external_agents()
            .bindings
            .keys()
            .next()
            .unwrap();
        assert_ne!(digest, old_digest);
        let replacement_store = crate::storage::token_usage::TokenUsageStore::new(
            fixture.directory.join("replacement.sqlite"),
        );
        fixture
            .service
            .set_token_usage_store(replacement_store.clone());
        database.execute_batch("CREATE TABLE retirement_write_count (id INTEGER); CREATE TRIGGER count_retirement_write AFTER UPDATE ON external_usage_source_activations WHEN NEW.state=2 BEGIN INSERT INTO retirement_write_count VALUES(1); END;").unwrap();
        if mode.contains("lost-ack") {
            publication
                .source_preparation
                .get()
                .unwrap()
                .lose_retirement_ack_for_tests();
        }
        fixture.service.retire_external_agent_binding(digest);
        fixture.service.retire_all_external_source_anchors();
        let effects = fixture.service.persistence.take_token_usage_effects();
        assert_eq!(effects.len(), 1);
        assert_eq!(state(&database), (1, 2), "actor did not touch SQLite");
        let crate::runtime::RuntimeSideEffect::PersistExternalSourceRetirement { work: duplicate } =
            &effects[0]
        else {
            panic!("missing retirement work")
        };
        let duplicate = duplicate.clone();
        assert_eq!(duplicate.path(), store.path());
        if mode.starts_with("precommit-restored") {
            database.execute_batch("CREATE TRIGGER reject_original_retirement AFTER UPDATE ON external_usage_source_activations WHEN NEW.state=2 BEGIN SELECT RAISE(ABORT,'fixture first retirement failed'); END;").unwrap();
            let original = publication.clone();
            let failure = tokio::task::spawn_blocking(move || {
                let _guard = original.source_preparation.hold_io_for_tests();
                original.source_preparation.get().unwrap().retire()
            })
            .await
            .unwrap();
            assert!(failure.is_err());
            assert_eq!(state(&database), (1, 2));
            database
                .execute_batch("DROP TRIGGER reject_original_retirement")
                .unwrap();
        }
        let (handle, actor) = AsyncRuntimeSessionActor::new(
            RuntimeServiceFixture::new().build(),
            AsyncRuntimeActorConfig::default(),
        )
        .unwrap();
        let mut owned = tokio::task::JoinSet::new();
        owned.spawn(actor.run());
        handle.queue_runtime_side_effects(effects).await.unwrap();
        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        handle.shutdown().await.unwrap();
        owned.join_next().await.unwrap().unwrap();
        assert_eq!(state(&database), (2, 3));
        assert!(
            !replacement_store.path().exists(),
            "retirement retargeted the current store"
        );
        assert_eq!(
            publication
                .source_preparation
                .get()
                .unwrap()
                .retirement_outcome_for_tests()
                .unwrap()
                .is_err(),
            mode != "normal",
            "reconciliation must not overwrite the original write result"
        );
        let recovery = publication
            .source_preparation
            .get()
            .unwrap()
            .recovery_outcome_for_tests();
        assert_eq!(recovery.is_some(), mode.starts_with("precommit-restored"));
        if let Some(result) = recovery {
            assert_eq!(result.is_err(), mode.ends_with("lost-ack"));
        }
        let after_recovery = duplicate.clone();
        tokio::task::spawn_blocking(move || duplicate.execute())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM retirement_write_count", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1,
            "lost receipt must not cause a second write"
        );
        if mode.starts_with("precommit-restored") {
            // Negative only: after recovery committed, a fixture-only rollback
            // to the old row cannot renew the exhausted write budget or let a
            // cached recovery success override contradictory current evidence.
            database
                .execute_batch("UPDATE external_usage_source_activations SET state=1,revision=2")
                .unwrap();
            let outcome = tokio::task::spawn_blocking(move || after_recovery.execute())
                .await
                .unwrap();
            assert!(
                outcome.is_err(),
                "cached recovery must not override a contradictory predecessor"
            );
            assert_eq!(state(&database), (1, 2), "recovery budget was replenished");
        }
        let native = publication.clone();
        assert!(
            tokio::task::spawn_blocking(move || native.execute_enrollment())
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

/// Changed evidence must be rejected BEFORE spending a fresh recovery attempt.
/// Real native admission and a real original SQLite rollback establish custody;
/// fixture-only row mutations are negative controls, never forged authority.
/// Each negative leaves both its row and the still-unused recovery cell intact.
#[tokio::test(flavor = "current_thread")]
async fn external_source_retirement_changed_proof_cannot_spend_fresh_recovery() {
    let Some((mut fixture, store, publication, _)) = accepted_source().await else {
        return;
    };
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database.execute_batch("CREATE TRIGGER reject_original_retirement AFTER UPDATE ON external_usage_source_activations WHEN NEW.state=2 BEGIN SELECT RAISE(ABORT,'fixture first retirement failed'); END;").unwrap();
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    let effects = fixture.service.persistence.take_token_usage_effects();
    assert_eq!(effects.len(), 1);
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRetirement { work } =
        effects.into_iter().next().unwrap()
    else {
        panic!("missing sealed retirement")
    };
    let original = publication.clone();
    let failure = tokio::task::spawn_blocking(move || {
        let _guard = original.source_preparation.hold_io_for_tests();
        original.source_preparation.get().unwrap().retire()
    })
    .await
    .unwrap()
    .unwrap_err()
    .message()
    .to_owned();
    database.execute_batch("DROP TRIGGER reject_original_retirement; CREATE TABLE original_activation AS SELECT * FROM external_usage_source_activations;").unwrap();
    for changed in [
        "UPDATE external_usage_source_activations SET state=0,revision=2",
        "UPDATE external_usage_source_activations SET state=2,revision=2",
        "UPDATE external_usage_source_activations SET attempt=X'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'",
        "DELETE FROM external_usage_source_activations",
    ] {
        database.execute_batch(changed).unwrap();
        let before: (i64, Option<i64>, Option<i64>) = database
            .query_row(
                "SELECT count(*),min(state),min(revision) FROM external_usage_source_activations",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let native = work.clone();
        let result = tokio::task::spawn_blocking(move || native.execute())
            .await
            .unwrap();
        assert_eq!(result.unwrap_err().message(), failure);
        assert!(
            publication
                .source_preparation
                .get()
                .unwrap()
                .recovery_outcome_for_tests()
                .is_none(),
            "changed proof spent recovery allowance"
        );
        assert_eq!(database.query_row("SELECT count(*),min(state),min(revision) FROM external_usage_source_activations", [], |row| Ok((row.get::<_, i64>(0)?,row.get::<_, Option<i64>>(1)?,row.get::<_, Option<i64>>(2)?))).unwrap(), before, "changed proof was mutated");
        database.execute_batch("DELETE FROM external_usage_source_activations; INSERT INTO external_usage_source_activations SELECT * FROM original_activation;").unwrap();
    }
    assert_eq!(state(&database), (1, 2));
}

/// A real SQLite failure leaves Accepted2 unchanged and retains the original
/// error after the one recovery also fails. Removing the fault cannot replenish
/// that bound or claim a tombstone. Wrong stage/revision, changed or absent data
/// cannot authorize another write. Logical closure still fences acceptance.
#[tokio::test(flavor = "current_thread")]
async fn external_source_retirement_failure_is_retained_without_write_replay() {
    let Some((mut fixture, store, publication, _)) = accepted_source().await else {
        return;
    };
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database.execute_batch("CREATE TRIGGER reject_retirement AFTER UPDATE ON external_usage_source_activations WHEN NEW.state=2 BEGIN SELECT RAISE(ABORT,'fixture retirement fault'); END;").unwrap();
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    let effects = fixture.service.persistence.take_token_usage_effects();
    assert_eq!(effects.len(), 1);
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRetirement { work } =
        effects.into_iter().next().unwrap()
    else {
        panic!("missing retirement")
    };
    let native = work.clone();
    let failure = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    assert!(failure.is_err());
    assert!(
        publication
            .source_preparation
            .get()
            .unwrap()
            .recovery_outcome_for_tests()
            .unwrap()
            .is_err(),
        "permanent failure must exhaust its one recovery"
    );
    assert_eq!(state(&database), (1, 2));
    database
        .execute_batch("DROP TRIGGER reject_retirement")
        .unwrap();
    let retained = work.clone();
    let again = tokio::task::spawn_blocking(move || work.execute())
        .await
        .unwrap();
    let original = failure.unwrap_err().message().to_owned();
    assert_eq!(again.unwrap_err().message(), original);
    assert_eq!(state(&database), (1, 2));
    database
        .execute_batch(
            "CREATE TABLE original_activation AS SELECT * FROM external_usage_source_activations;",
        )
        .unwrap();
    for changed in [
        "UPDATE external_usage_source_activations SET state=2,revision=2",
        "UPDATE external_usage_source_activations SET state=0,revision=2",
        "UPDATE external_usage_source_activations SET state=2,revision=3,attempt=X'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'",
        "DELETE FROM external_usage_source_activations",
    ] {
        database.execute_batch(changed).unwrap();
        let native = retained.clone();
        let result = tokio::task::spawn_blocking(move || native.execute())
            .await
            .unwrap();
        assert_eq!(
            result.unwrap_err().message(),
            original,
            "changed or missing tombstone must preserve original failure"
        );
        database.execute_batch("DELETE FROM external_usage_source_activations; INSERT INTO external_usage_source_activations SELECT * FROM original_activation;").unwrap();
    }
    let native = publication.clone();
    assert!(
        tokio::task::spawn_blocking(move || native.execute_enrollment())
            .await
            .unwrap()
            .is_err()
    );
    fixture.service.retire_all_external_source_anchors();
    assert!(
        fixture
            .service
            .persistence
            .take_token_usage_effects()
            .is_empty()
    );
}

/// Replacing control authority is not proof that the native source ended. It
/// cancels old acceptance and removes old anchor custody, but must not queue a
/// permanent tombstone or erase an already committed historical acceptance.
/// Cold admission remains unavailable; this test grants no restored rights.
#[tokio::test(flavor = "current_thread")]
async fn external_source_retirement_does_not_confuse_control_invalidation_with_native_end() {
    let Some((mut fixture, store, publication, _)) = accepted_source().await else {
        return;
    };
    fixture.service.retire_unbound_external_message_identities();
    assert!(
        fixture
            .service
            .persistence
            .take_token_usage_effects()
            .is_empty()
    );
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(state(&database), (1, 2));
    let native = publication.clone();
    assert!(
        tokio::task::spawn_blocking(move || native.execute_enrollment())
            .await
            .unwrap()
            .is_err()
    );
}

/// Synthetic reservations exercise only finite metadata headroom. A genuine
/// native producer still receives independently qualified lifecycle service,
/// but cannot prepare or publish optional activation metadata over the cap.
/// No counter reservation is used as a native witness or acceptance grant.
#[tokio::test(flavor = "current_thread")]
async fn external_source_retirement_metadata_exhaustion_does_not_expand_or_forge_receipts() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store = crate::storage::token_usage::TokenUsageStore::new(
        fixture.directory.join("capacity.sqlite"),
    );
    fixture.service.set_token_usage_store(store.clone());
    let held = (0..super::super::super::external_agents::MAX_BINDINGS)
        .map(|_| {
            fixture
                .service
                .control
                .external_sources()
                .reserve_activation_metadata()
        })
        .collect::<Vec<_>>();
    assert!(held.iter().all(|slot| slot.has_reservation()));
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    assert!(!work.source_preparation.has_reservation());
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(observed.is_ok(), "{observed:?}");
    let body =
        fixture
            .service
            .complete_external_enrollment(work.clone(), observed, &fixture.connection);
    let reply: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(reply["result"]["registered"], true);
    assert!(work.source_preparation.get().is_none());
    assert!(
        fixture
            .service
            .begin_external_source_publication(work, body)
            .is_err()
    );
    let database = rusqlite::Connection::open(store.path()).unwrap();
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
    drop(held);
    assert!(
        fixture
            .service
            .control
            .external_sources()
            .reserve_activation_metadata()
            .has_reservation()
    );
}

/// The pre-existing lease fence is a logical authority end, not native death.
/// A real admitted source remains physically alive while test-owned control
/// references are removed and its lease is expired. Actual reconciliation must
/// queue a tombstone once; neither native liveness nor stale metadata extends
/// that expired authority. No native witness or death evidence is fabricated.
#[tokio::test(flavor = "current_thread")]
async fn external_source_retirement_expired_observer_lease_is_not_native_death() {
    let Some((mut fixture, store, publication, _)) = accepted_source().await else {
        return;
    };
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    assert!(publication.origin.is_live());
    assert!(
        fixture
            .service
            .pane_process_identity_is_current(&publication.pane_id, &publication.process)
    );
    let binding = fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .get_mut(&digest)
        .unwrap();
    binding.expires = 0;
    binding.enrollment.as_mut().unwrap().observers.clear();
    fixture
        .service
        .reconcile_external_agent_registration(digest);
    assert!(fixture.service.control.external_agents().bindings[&digest].retired);
    assert!(publication.origin.is_live());
    assert!(
        fixture
            .service
            .pane_process_identity_is_current(&publication.pane_id, &publication.process)
    );
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(state(&database), (1, 2));
    let effects = fixture.service.persistence.take_token_usage_effects();
    assert_eq!(effects.len(), 1);
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRetirement { work } =
        effects.into_iter().next().unwrap()
    else {
        panic!("missing logical retirement")
    };
    tokio::task::spawn_blocking(move || work.execute())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&database), (2, 3));
    assert!(
        publication.origin.is_live(),
        "tombstone is not native death evidence"
    );
}

/// Inspects fixed-size inert state/revision only, never private reply contents.
fn state(database: &rusqlite::Connection) -> (i64, i64) {
    database
        .query_row(
            "SELECT state,revision FROM external_usage_source_activations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}
