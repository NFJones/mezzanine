//! Request-origin begin boundaries under actual ordinary producer provenance.
//! No provider work, PID override, inherited observer fd or primary role is used.
//! Stored metadata does not enable usage or substitute for live native authority.

use super::helper_presentation::enroll_producer;
use super::*;

/// Retiring the only current source removes reconstructible readiness but not
/// its durable origin. Reopening storage restores metadata without adopting
/// current project/time or creating cost, but grants no retired source readiness.
#[tokio::test(flavor = "current_thread")]
async fn external_request_readiness_retirement_preserves_durable_origin_restore() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let enrolled = enroll_producer(&mut fixture).await;
    let request = begin(&enrolled, &"a".repeat(64));
    let work = fixture
        .service
        .prepare_external_usage_begin(&request, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let first: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage_begin(work, result, &fixture.connection),
    )
    .unwrap();
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    let owner = fixture.service.control.external_agents().bindings[&digest]
        .accounting_owner
        .clone();
    fixture.service.retire_external_agent_binding(digest);
    assert!(
        fixture
            .service
            .control
            .external_usage_begins()
            .origins
            .is_empty()
    );
    assert!(
        fixture
            .service
            .prepare_external_usage_begin(&request, &fixture.connection)
            .is_err()
    );
    assert!(
        fixture
            .service
            .prepare_external_enrollment(&super::request(), &fixture.connection)
            .is_err()
    );
    let restored = crate::storage::token_usage::TokenUsageStore::new(&path)
        .begin_external_request(
            &owner,
            "pi",
            &"a".repeat(64),
            None,
            first["result"]["admitted_at"].as_u64().unwrap() + 1,
        )
        .unwrap();
    assert_eq!(
        restored.admitted_at,
        first["result"]["admitted_at"].as_u64().unwrap()
    );
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

/// Reconstructible readiness for abandoned sources cannot permanently block a
/// different live producer. At capacity a fresh marker and an original durable
/// retry still acknowledge correctly, with a bounded cache and zero expense.
#[tokio::test(flavor = "current_thread")]
async fn external_request_readiness_cache_exhaustion_preserves_fresh_and_durable_retry() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let enrolled = enroll_producer(&mut fixture).await;
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
    for index in 0..4096 {
        fixture
            .service
            .control
            .external_usage_begins_mut()
            .origins
            .insert(
                (format!("abandoned-{index}"), "f".repeat(64)),
                crate::storage::token_usage::ExternalRequestOrigin {
                    request_id: "f".repeat(64),
                    project: None,
                    admitted_at: 1,
                },
            );
    }
    let request = begin(&enrolled, &"a".repeat(64));
    let work = fixture
        .service
        .prepare_external_usage_begin(&request, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let first: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage_begin(work, result, &fixture.connection),
    )
    .unwrap();
    assert_eq!(first["result"]["accepted"], true);
    assert!(
        fixture
            .service
            .control
            .external_usage_begins()
            .origins
            .len()
            <= 4096
    );
    fixture
        .service
        .control
        .external_usage_begins_mut()
        .origins
        .remove(&(owner, "a".repeat(64)));
    let work = fixture
        .service
        .prepare_external_usage_begin(&request, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let retry: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage_begin(work, result, &fixture.connection),
    )
    .unwrap();
    assert_eq!(
        retry["result"]["admitted_at"],
        first["result"]["admitted_at"]
    );
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

/// A genuine persistent source submits a ready marker then one finalized
/// normalized observation through production framed workers. Restoring the same
/// begin after reply loss preserves origin and returns a durable inert replay;
/// unknown markers and forbidden attribution fields create no expense.
#[tokio::test(flavor = "current_thread")]
async fn external_request_begin_end_actor_durable_replay_and_marker_scope() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let enrolled = enroll_producer(&mut fixture).await;
    let params = serde_json::json!({"launch_token":enrolled["result"]["launch_token"],"generation":enrolled["result"]["generation"],"external_session_id":"session-a","request_id":"a".repeat(64)});
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    let begin_body=serde_json::json!({"jsonrpc":"2.0","id":"begin-live","method":"agent/external/usage-begin","params":params}).to_string();
    let first = handle
        .handle_control_input_for_connection(
            crate::control::encode_control_body(&begin_body),
            8192,
            fixture.connection.clone(),
        )
        .await
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&first.output, 8192).unwrap();
    let started: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(started["result"]["accepted"], true);
    let mut end_params = params.clone();
    end_params["event_id"] = serde_json::json!("b".repeat(64));
    end_params["observed_at"] = serde_json::json!(crate::runtime::current_unix_seconds());
    end_params["provider"] = serde_json::json!("fixture");
    end_params["model"] = serde_json::json!("model");
    end_params["counters"] =
        serde_json::json!({"input_tokens":10,"output_tokens":3,"cached_input_tokens":0});
    let end_body=serde_json::json!({"jsonrpc":"2.0","id":"finish-live","method":"agent/external/usage-end","params":end_params}).to_string();
    let first = handle
        .handle_control_input_for_connection(
            crate::control::encode_control_body(&end_body),
            8192,
            fixture.connection.clone(),
        )
        .await
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&first.output, 8192).unwrap();
    let committed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(committed["result"]["durable"], true, "{committed}");
    assert_eq!(committed["result"]["applied"], true);
    assert!(!first.connection.initialized());
    let missing = handle
        .handle_control_input_for_connection(
            crate::control::encode_control_body(&end_body),
            8192,
            fixture.connection.clone(),
        )
        .await
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&missing.output, 8192).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&body)
            .unwrap()
            .get("error")
            .is_some()
    );
    let restored = handle
        .handle_control_input_for_connection(
            crate::control::encode_control_body(&begin_body),
            8192,
            fixture.connection.clone(),
        )
        .await
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&restored.output, 8192).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["result"]["admitted_at"],
        started["result"]["admitted_at"]
    );
    let replay = handle
        .handle_control_input_for_connection(
            crate::control::encode_control_body(&end_body),
            8192,
            fixture.connection.clone(),
        )
        .await
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&replay.output, 8192).unwrap();
    let replay: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(replay["result"]["durable"], true, "{replay}");
    assert_eq!(replay["result"]["applied"], false);
    end_params["project_id"] = serde_json::json!("forbidden");
    let forbidden=serde_json::json!({"jsonrpc":"2.0","id":"bad-origin","method":"agent/external/usage-end","params":end_params}).to_string();
    let reply = handle
        .handle_control_input_for_connection(
            crate::control::encode_control_body(&forbidden),
            8192,
            fixture.connection.clone(),
        )
        .await
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&reply.output, 8192).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&body)
            .unwrap()
            .get("error")
            .is_some()
    );
    handle.shutdown().await.unwrap();
    let mut exit = task.await.unwrap();
    assert_eq!(
        exit.service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_eq!(
        exit.service
            .external_token_usage(Some("%1"))
            .values()
            .map(|usage| usage.input_tokens)
            .sum::<u64>(),
        10
    );
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Production framed actor ingress accepts only one bounded begin body, runs
/// metadata I/O off actor and returns typed readiness without initialization.
/// Oversized/batched bodies reserve nothing; repeat delivery preserves one row.
#[tokio::test(flavor = "current_thread")]
async fn external_request_begin_actor_frame_boundary_and_retry_are_exact() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let enrolled = enroll_producer(&mut fixture).await;
    let request = begin(&enrolled, &"a".repeat(64));
    let body=serde_json::json!({"jsonrpc":"2.0","id":"wire-begin","method":"agent/external/usage-begin","params":serde_json::from_str::<serde_json::Value>(request.params.as_deref().unwrap()).unwrap()}).to_string();
    let exact = format!("{body}{}", " ".repeat(8192 - body.len()));
    let oversized = crate::control::encode_control_body(&format!("{exact} "));
    let mut batched = crate::control::encode_control_body(&body);
    batched.extend_from_slice(&crate::control::encode_control_body(&body));
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let actor_task = tokio::spawn(actor.run());
    for frame in [oversized, batched] {
        let reply = handle
            .handle_control_input_for_connection(frame, 16384, fixture.connection.clone())
            .await
            .unwrap();
        let (text, _) = crate::control::decode_control_frame(&reply.output, 16384).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&text)
                .unwrap()
                .get("error")
                .is_some()
        );
    }
    for _ in 0..2 {
        let reply = handle
            .handle_control_input_for_connection(
                crate::control::encode_control_body(&exact),
                16384,
                fixture.connection.clone(),
            )
            .await
            .unwrap();
        assert!(!reply.connection.initialized());
        let (text, _) = crate::control::decode_control_frame(&reply.output, 16384).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["result"]["accepted"], true, "{value}");
        assert_eq!(value["result"]["controls"], serde_json::json!([]));
    }
    handle.shutdown().await.unwrap();
    let mut exit = actor_task.await.unwrap();
    assert_eq!(exit.service.external_usage_begin_pending_for_tests(), 0);
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Native source cwd wins over stale pane hints; deepest denial stays explicitly
/// unattributed. A begin marker stores the first qualified request boundary, not
/// registration-time project state or a guessed provider execution origin.
#[tokio::test(flavor = "current_thread")]
async fn external_request_begin_native_cwd_and_deepest_trust_choose_original_origin() {
    use crate::security::project::{ProjectTrustStore, TrustDecision};
    for denied in [false, true] {
        let root = std::env::temp_dir().join(format!(
            "mez-request-origin-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        let a = root.join("a");
        let b = root.join("b");
        let deep = a.join("deep");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let start = if denied { &deep } else { &a };
        let Some(mut fixture) = fixture_with_start_directory("hold", Some(start)).await else {
            std::fs::remove_dir_all(root).unwrap();
            continue;
        };
        let store = crate::storage::token_usage::TokenUsageStore::new(
            fixture.directory.join("usage.sqlite"),
        );
        let mut trust = ProjectTrustStore::default();
        trust
            .decide(a.clone(), TrustDecision::Trusted, None)
            .unwrap();
        trust
            .decide(b.clone(), TrustDecision::Trusted, None)
            .unwrap();
        if denied {
            trust.decide(deep, TrustDecision::Rejected, None).unwrap();
        }
        let inventory = store
            .prepare_accounting_projects(&trust.records().cloned().collect::<Vec<_>>())
            .unwrap();
        let a = std::fs::canonicalize(a).unwrap();
        let expected = inventory
            .iter()
            .find(|row| row.root == a)
            .unwrap()
            .id
            .clone();
        fixture.service.set_project_trust_store(trust, None);
        fixture.service.set_token_usage_store(store.clone());
        fixture
            .service
            .persistence
            .install_accounting_projects(&store, Some(inventory));
        fixture.service.set_pane_current_working_directory("%1", b);
        let enrolled = enroll_producer(&mut fixture).await;
        let request = begin(&enrolled, &"a".repeat(64));
        let work = fixture
            .service
            .prepare_external_usage_begin(&request, &fixture.connection)
            .unwrap();
        let native = work.clone();
        let result = tokio::task::spawn_blocking(move || native.execute())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.project, if denied { None } else { expected });
        let reply: serde_json::Value = serde_json::from_str(
            &fixture
                .service
                .complete_external_usage_begin(work, Ok(result), &fixture.connection),
        )
        .unwrap();
        assert_eq!(reply["result"]["accepted"], true);
        assert_eq!(
            reply["result"]["origin"],
            if denied { "unattributed" } else { "project" }
        );
        drop(fixture);
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Constructs only published private-source fields and an opaque marker.
fn begin(enrolled: &serde_json::Value, marker: &str) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"begin","method":"agent/external/usage-begin","params":{
        "launch_token":enrolled["result"]["launch_token"],"generation":enrolled["result"]["generation"],"external_session_id":"session-a","request_id":marker,
    }}).to_string()).unwrap()
}

/// Real native source plus durable mapping is mandatory. Native worker capture
/// returns typed readiness with no client/control/usage grant and no expense.
#[tokio::test(flavor = "current_thread")]
async fn external_request_begin_native_source_persists_without_usage_or_client_rights() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let enrolled = enroll_producer(&mut fixture).await;
    let request = begin(&enrolled, &"a".repeat(64));
    let work = fixture
        .service
        .prepare_external_usage_begin(&request, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage_begin(work, result, &fixture.connection),
    )
    .unwrap();
    assert_eq!(reply["result"]["protocol"], "external-request/1");
    assert_eq!(reply["result"]["accepted"], true);
    assert_eq!(reply["result"]["controls"], serde_json::json!([]));
    assert!(!fixture.connection.initialized());
    assert_eq!(fixture.service.external_usage_begin_pending_for_tests(), 0);
    let connection = rusqlite::Connection::open(path).unwrap();
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
    assert_eq!(enrolled["result"]["usage"], "unavailable-source-continuity");
}

/// Fake success without worker publication cannot acknowledge durable readiness.
/// Metadata/credential/session/generation mismatches reject before reservations;
/// expired settlement consumes only its original reservation and grants nothing.
#[tokio::test(flavor = "current_thread")]
async fn external_request_begin_rejects_unpublished_and_mismatched_or_expired_work() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            fixture.directory.join("usage.sqlite"),
        ));
    let enrolled = enroll_producer(&mut fixture).await;
    let request = begin(&enrolled, &"a".repeat(64));
    let work = fixture
        .service
        .prepare_external_usage_begin(&request, &fixture.connection)
        .unwrap();
    let fake = crate::storage::token_usage::ExternalRequestOrigin {
        request_id: "a".repeat(64),
        project: None,
        admitted_at: 100,
    };
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage_begin(work, Ok(fake), &fixture.connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    for field in [
        "generation",
        "external_session_id",
        "request_id",
        "project_id",
    ] {
        let mut candidate = request.clone();
        let mut params: serde_json::Value =
            serde_json::from_str(candidate.params.as_deref().unwrap()).unwrap();
        params[field] = if field == "generation" {
            serde_json::json!(999)
        } else {
            serde_json::json!("wrong")
        };
        candidate.params = Some(params.to_string());
        assert!(
            fixture
                .service
                .prepare_external_usage_begin(&candidate, &fixture.connection)
                .is_err()
        );
        assert_eq!(fixture.service.external_usage_begin_pending_for_tests(), 0);
    }
    let mut work = fixture
        .service
        .prepare_external_usage_begin(&request, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    work.expire_for_tests();
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage_begin(work, result, &fixture.connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    assert_eq!(fixture.service.external_usage_begin_pending_for_tests(), 0);
}
