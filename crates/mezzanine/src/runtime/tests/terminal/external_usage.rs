//! Production external accounting ingress and checkpoint projection regressions.
//!
//! The actor authenticates immutable registration provenance, workers commit
//! SQLite outside actor ownership, and replies acknowledge only durable facts.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
use crate::runtime::current_unix_seconds;
use crate::storage::token_usage::TokenUsageStore;

/// External-only status remains observational and must not allocate a native
/// conversation merely to inspect usage or reset its pane-view baseline.
#[test]
fn runtime_harness_status_external_only_does_not_create_native_session() {
    let (mut service, connection, params, _) = fixture();
    let request = crate::control::parse_json_rpc_request(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params,
        })
        .to_string(),
    )
    .unwrap();
    let work = service
        .prepare_external_usage(&request, &connection)
        .unwrap();
    let commit = work.store.ingest_external(&work.report, work.now).unwrap();
    service.complete_external_usage(work, Ok(commit));
    let client = service.session().layout_owner_client_id().unwrap().clone();
    let response = service
        .execute_agent_shell_command(&client, "/status")
        .unwrap();
    assert!(response.contains("codex"), "{response}");
    assert!(service.agent_shell_store().get("%1").is_none());
    let report = service
        .prepare_status_report(
            &client,
            "%1",
            mez_agent::slash::StatusOptions {
                scope: mez_agent::slash::StatusScope::Overall,
                extended: true,
            },
        )
        .unwrap();
    assert!(service.status_report_is_current(&report));
    assert!(report.render().unwrap().contains("1-Day Token Usage"));
    service
        .dispatch_deferred_agent_shell_command(&client, "%1", "status", "/status --extended")
        .unwrap();
    let body = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(body.contains("codex"), "{body}");
    assert!(service.agent_shell_store().get("%1").is_none());
    let reset = service
        .execute_agent_shell_command(&client, "/reset-status")
        .unwrap();
    assert!(reset.contains("changed=true"), "{reset}");
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        0
    );
    assert_eq!(
        service
            .external_token_usage(None)
            .values()
            .next()
            .unwrap()
            .input_tokens,
        10
    );
    let replay = service
        .prepare_external_usage(&request, &connection)
        .unwrap();
    let commit = replay
        .store
        .ingest_external(&replay.report, replay.now)
        .unwrap();
    service.complete_external_usage(replay, Ok(commit));
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        0
    );
    assert!(service.agent_shell_store().get("%1").is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Overall status keeps same-model native/external expense separate, retains
/// unknown reasoning coverage, and includes both sources in durable windows.
#[test]
fn runtime_harness_status_separates_same_model_and_unknown_reasoning() {
    let (mut service, connection, mut params, _) = fixture();
    params["counters"]
        .as_object_mut()
        .unwrap()
        .remove("reasoning_tokens");
    let request = crate::control::parse_json_rpc_request(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params,
        })
        .to_string(),
    )
    .unwrap();
    let work = service
        .prepare_external_usage(&request, &connection)
        .unwrap();
    let result = work.store.ingest_external(&work.report, work.now).unwrap();
    service.complete_external_usage(work, Ok(result));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let profile = runtime_model_profile("provider", "model");
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        mez_agent::ModelTokenUsage {
            input_tokens: 20,
            output_tokens: 8,
            reasoning_tokens: 2,
            ..Default::default()
        },
        Default::default(),
        Some(&profile),
    );
    let client = service.session().layout_owner_client_id().unwrap().clone();
    let report = service
        .prepare_status_report(
            &client,
            "%1",
            mez_agent::slash::StatusOptions {
                scope: mez_agent::slash::StatusScope::Overall,
                extended: true,
            },
        )
        .unwrap()
        .render()
        .unwrap();
    assert!(
        report.contains("| Harness | Provider | Model |"),
        "{report}"
    );
    assert!(
        report.contains("| codex | provider | model | 7 | 3 | 4 | unknown |"),
        "{report}"
    );
    assert!(
        report.contains("| mez | provider | model | 20 | unknown | 8 | 2 |"),
        "{report}"
    );
    assert!(
        report
            .split("### 1-Day Token Usage")
            .nth(1)
            .unwrap()
            .contains("| codex |"),
        "{report}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Reads only committed external deltas, independent of the native status reader.
fn external_ledger_input(store: &TokenUsageStore) -> i64 {
    rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COALESCE(SUM(input_tokens),0) FROM token_usage_events WHERE event_source='external'",
        [], |row| row.get(0),
    ).unwrap()
}

/// Capacity limits new unique streams, not retries or updates of accepted ones.
/// Concurrent reservations for the same stream must share one projection slot.
#[test]
fn runtime_external_usage_capacity_preserves_existing_stream_admission() {
    let (mut service, connection, params, _) = fixture();
    service.set_external_usage_stream_limit_for_tests(1);
    let request = |params: &serde_json::Value| {
        crate::control::parse_json_rpc_request(
        &serde_json::json!({"jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params}).to_string(),
    ).unwrap()
    };
    let first = service
        .prepare_external_usage(&request(&params), &connection)
        .unwrap();
    let duplicate = service
        .prepare_external_usage(&request(&params), &connection)
        .expect("same pending stream must not reserve another slot");
    let commit = first
        .store
        .ingest_external(&first.report, first.now)
        .unwrap();
    service.complete_external_usage(first, Ok(commit));
    let replay = duplicate
        .store
        .ingest_external(&duplicate.report, duplicate.now)
        .unwrap();
    service.complete_external_usage(duplicate, Ok(replay));
    let retry = service
        .prepare_external_usage(&request(&params), &connection)
        .expect("existing stream replay must be admitted at capacity");
    let mut next = params.clone();
    next["event_id"] = serde_json::json!("next");
    next["sequence"] = serde_json::json!(2);
    let update = service
        .prepare_external_usage(&request(&next), &connection)
        .expect("existing stream update must be admitted at capacity");
    let mut new_stream = params.clone();
    new_stream["epoch"] = serde_json::json!("new-epoch");
    assert!(
        service
            .prepare_external_usage(&request(&new_stream), &connection)
            .is_err()
    );
    for work in [retry, update] {
        let result = work.store.ingest_external(&work.report, work.now);
        service.complete_external_usage(work, result);
    }
    assert_eq!(
        service
            .external_token_usage(None)
            .values()
            .next()
            .unwrap()
            .input_tokens,
        20
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Creates a registered, capability-only telemetry owner and private usage store.
fn fixture() -> (
    RuntimeSessionService,
    ControlConnectionState,
    serde_json::Value,
    TokenUsageStore,
) {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let store = TokenUsageStore::new(temp_root("external-usage-ingress").join("usage.sqlite"));
    service.set_token_usage_store(store.clone());
    let launch: serde_json::Value = serde_json::from_str(&service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"launch","method":"agent/external/launch","params":{"pane_id":"%1","harness":"codex","version":"fixture"}}"#, &primary,
    )).unwrap();
    let mut connection = ControlConnectionState::new(true, false);
    connection
        .bind_authenticated_peer(crate::control::AuthenticatedPeer::unix_user(
            crate::runtime::current_effective_uid(),
        ))
        .unwrap();
    let registration = serde_json::json!({"launch_token":launch["result"]["launch_token"],"generation":launch["result"]["generation"],
        "external_session_id":"usage-run","display_name":"Usage Fixture"});
    let response = service.dispatch_runtime_control_body_for_connection(&serde_json::json!({
        "jsonrpc":"2.0","id":"register","method":"agent/external/register","params":registration,
    }).to_string(), &mut connection);
    assert!(
        serde_json::from_str::<serde_json::Value>(&response)
            .unwrap()
            .get("result")
            .is_some()
    );
    let report = serde_json::json!({"launch_token":launch["result"]["launch_token"],"generation":launch["result"]["generation"],
        "external_session_id":"usage-run","epoch":"epoch-one","event_id":"first","sequence":1,"mode":"delta",
        "observed_at":current_unix_seconds(),"provider":"provider","model":"model",
        "counters":{"input_tokens":10,"output_tokens":4,"reasoning_tokens":2,"cached_input_tokens":3}});
    (service, connection, report, store)
}

/// Duplicate RPC replies recover an absolute checkpoint without double charging.
/// Changed counters under the same event ID fail after durable commit, while
/// the hook remains uninitialized and the actor still serves lifecycle requests.
/// Dropping the request waiter while storage is gated must not cancel accepted
/// accounting or hold the actor. Identical retry recovers the committed checkpoint.
#[tokio::test(flavor = "current_thread")]
async fn runtime_external_usage_worker_gate_preserves_actor_and_lost_reply() {
    let (mut service, connection, params, store) = fixture();
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    service.gate_external_usage_worker_for_tests(started.clone(), release.clone());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let actor_task = tokio::spawn(actor.run());
    let input = crate::control::encode_control_body(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params,
        })
        .to_string(),
    );
    let first_handle = handle.clone();
    let first_input = input.clone();
    let first_connection = connection.clone();
    let first = tokio::spawn(async move {
        first_handle
            .handle_control_input_for_connection(first_input, 8192, first_connection)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), handle.lifecycle_state())
            .await
            .unwrap()
            .unwrap(),
        RuntimeLifecycleState::Running
    );
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    release.notify_one();
    let retry_handle = handle.clone();
    let retry = tokio::spawn(async move {
        retry_handle
            .handle_control_input_for_connection(input, 8192, connection)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    release.notify_one();
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), retry)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&response.output, 8192).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["result"]["durable"],
        true,
        "external usage retry response: {body}"
    );
    handle.shutdown().await.unwrap();
    let mut exit = actor_task.await.unwrap();
    assert_eq!(
        exit.service
            .external_token_usage(None)
            .values()
            .next()
            .unwrap()
            .input_tokens,
        10
    );
    assert_eq!(external_ledger_input(&store), 10);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Duplicate RPC replies recover an absolute checkpoint without double charging.
/// Changed counters under the same event ID fail after durable commit, while
/// the hook remains uninitialized and the actor still serves lifecycle requests.
#[tokio::test(flavor = "current_thread")]
async fn runtime_external_usage_acknowledges_durable_replay_without_double_charge() {
    let (service, connection, params, store) = fixture();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let client =
        async {
            for expected in [true, false] {
                let request = crate::control::encode_control_body(&serde_json::json!({
                "jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params,
            }).to_string());
                let response = handle
                    .handle_control_input_for_connection(request, 8192, connection.clone())
                    .await
                    .unwrap();
                let (body, _) =
                    crate::control::decode_control_frame(&response.output, 8192).unwrap();
                let value: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(value["result"]["durable"], true, "{value}");
                assert_eq!(value["result"]["applied"], expected, "{value}");
                assert!(!response.connection.initialized());
            }
            let mut changed = params.clone();
            changed["counters"]["input_tokens"] = serde_json::json!(11);
            let request = crate::control::encode_control_body(
                &serde_json::json!({
                    "jsonrpc":"2.0","id":"changed","method":"agent/external/usage","params":changed,
                })
                .to_string(),
            );
            let response = handle
                .handle_control_input_for_connection(request, 8192, connection.clone())
                .await
                .unwrap();
            let (body, _) = crate::control::decode_control_frame(&response.output, 8192).unwrap();
            assert!(
                serde_json::from_str::<serde_json::Value>(&body)
                    .unwrap()
                    .get("error")
                    .is_some()
            );
            assert_eq!(
                handle.lifecycle_state().await.unwrap(),
                RuntimeLifecycleState::Running
            );
            handle.shutdown().await.unwrap();
        };
    let ((), mut exit) = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        tokio::join!(client, actor.run())
    })
    .await
    .unwrap();
    let totals = exit.service.external_token_usage(None);
    assert_eq!(totals.values().next().unwrap().input_tokens, 10);
    assert_eq!(external_ledger_input(&store), 10);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Commit-before-projection loss is repaired by identical replay. Pane reset
/// affects only the view baseline, so a later report adds its new increment,
/// and out-of-order completion cannot overwrite a newer checkpoint.
#[test]
fn runtime_external_usage_checkpoint_recovery_and_reset_conserve_totals() {
    let (mut service, connection, params, _) = fixture();
    let request = |params: &serde_json::Value| {
        crate::control::parse_json_rpc_request(
            &serde_json::json!({
                "jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params,
            })
            .to_string(),
        )
        .unwrap()
    };
    let work = service
        .prepare_external_usage(&request(&params), &connection)
        .unwrap();
    let first = work.store.ingest_external(&work.report, work.now).unwrap();
    let replay = work.store.ingest_external(&work.report, work.now).unwrap();
    service.complete_external_usage(work.clone(), Ok(replay));
    service.complete_external_usage(work, Ok(first));
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        10
    );
    service.reset_agent_token_usage_for_pane("%1");
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        0
    );
    let mut next = params.clone();
    next["event_id"] = serde_json::json!("next");
    next["sequence"] = serde_json::json!(2);
    let work = service
        .prepare_external_usage(&request(&next), &connection)
        .unwrap();
    let result = work.store.ingest_external(&work.report, work.now).unwrap();
    let mut third = next.clone();
    third["event_id"] = serde_json::json!("third");
    third["sequence"] = serde_json::json!(3);
    let later = service
        .prepare_external_usage(&request(&third), &connection)
        .unwrap();
    let later_result = later
        .store
        .ingest_external(&later.report, later.now)
        .unwrap();
    // Two distinct admissions/revisions complete in reverse order.
    service.complete_external_usage(later, Ok(later_result));
    service.complete_external_usage(work, Ok(result));
    assert_eq!(
        service
            .external_token_usage(None)
            .values()
            .next()
            .unwrap()
            .input_tokens,
        30
    );
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        20
    );
    service.terminate_all_pane_processes().unwrap();
}
