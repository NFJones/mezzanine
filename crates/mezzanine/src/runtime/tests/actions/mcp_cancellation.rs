//! Real MCP lease cancellation and distinct-call reconnection regressions.
//!
//! A server can perform effects before a call is cancelled. Its connection must
//! be retired, availability must become truthful, and only explicitly new work
//! may use a fresh discovered connection. The interrupted call is not replayed.

use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeSessionActor, execute_approved_external_action,
};

/// Queues a single fixture MCP action through production actor settlement.
async fn claim_fixture_call(
    service: &mut RuntimeSessionService,
    turn_id: &str,
    action_id: &str,
    message: &str,
) -> crate::runtime::RuntimeApprovedExternalActionDispatch {
    super::mcp::grant_fixture_mcp_tool_for_turn(service, turn_id);
    service.remove_pending_agent_provider_task(turn_id);
    let turn = service.agent_turn_ledger().turn(turn_id).unwrap().clone();
    let action = mez_agent::AgentAction {
        id: action_id.to_string(),
        payload: mez_agent::AgentActionPayload::McpCall {
            server: "fixture".to_string(),
            tool: "echo".to_string(),
            arguments_json: serde_json::json!({"message": message}).to_string(),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "MCP cancellation fixture".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "exercise one distinct call".to_string(),
                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![mez_agent::ActionResult::running(
            &turn,
            &action,
            Vec::new(),
            None,
        )],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .apply_agent_provider_completed_event(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            turn_id,
            execution,
        )
        .await
        .unwrap();
    service
        .claim_approved_external_action(turn_id, action_id)
        .unwrap()
        .unwrap()
}

/// Cancels a real blocked stdio tools/call after its effect barrier, then runs
/// a distinct new call through a freshly discovered transport. The old call
/// must occur exactly once and retain unknown-effect/no-replay evidence.
#[tokio::test]
async fn runtime_mcp_inflight_cancellation_allows_distinct_new_call() {
    let root = temp_root("mcp-inflight-cancellation");
    fs::create_dir_all(&root).unwrap();
    let calls = root.join("calls");
    let ready = root.join("ready");
    let script = runtime_mcp_fixture_script(false).replace(
        "  *'\"method\":\"tools/call\"'*)\n",
        &format!("  *'\"method\":\"tools/call\"'*)\nprintf '%s\\n' \"$line\" >> '{}'\ncase \"$line\" in *stall*) printf ready > '{}'; while :; do sleep 0.01; done;; esac\n", calls.display(), ready.display()),
    );
    let mut service = test_runtime_service();
    service.replace_config_layers_async(vec![ConfigLayer {
        name: "primary".to_string(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: format!("[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [\"-c\", {}]\napproval = \"allow\"\ntool_timeout_ms = 5000\n", toml_string(&script)),
    }]).await.unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "call @fixture once")
        .unwrap();
    let dispatch = claim_fixture_call(&mut service, "turn-1", "cancelled-call", "stall").await;
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let worker_handle = handle.clone();
    let worker = async move {
        execute_approved_external_action(worker_handle, dispatch)
            .await
            .unwrap()
    };
    let cancel = async {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !ready.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(ready.exists(), "MCP request did not reach effect barrier");
        handle
            .execute_agent_shell_command(primary.clone(), "/stop".to_string())
            .await
            .unwrap();
    };
    let client = async {
        let (result, ()) = tokio::join!(worker, cancel);
        assert!(result.is_none());
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        tokio::join!(client, actor.run())
    })
    .await
    .unwrap();
    let mut service = exit.service;
    assert_eq!(
        service.mcp_registry().list_servers()[0].status,
        mez_agent::mcp::McpServerStatus::Configured
    );
    assert!(!service.agent_provider_task_is_pending("turn-1"));
    let work = service.prepare_runtime_mcp_discovery_work().unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    service.apply_agent_provider_preparation(outcome).unwrap();
    assert_eq!(
        service.mcp_registry().list_servers()[0].status,
        mez_agent::mcp::McpServerStatus::Available
    );
    service
        .execute_agent_shell_command(&primary, "make a distinct new @fixture call")
        .unwrap();
    let context = runtime_prepared_context_for_turn(&service, "turn-2");
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains("action_interrupted_unknown_effects"))
    );
    let dispatch = claim_fixture_call(&mut service, "turn-2", "new-call", "new-work").await;
    let mcp = dispatch.mcp.unwrap();
    let mut transports = crate::runtime::RuntimeMcpTransportSet::default();
    transports.insert("fixture".to_string(), mcp.transport);
    let response = transports
        .call_tool_async(&mcp.plan, &mcp.environment, mcp.auth_store.as_ref())
        .await
        .unwrap();
    assert!(!response.is_error);
    let observed = fs::read_to_string(&calls).unwrap();
    assert_eq!(
        observed.lines().count(),
        2,
        "interrupted call was replayed: {observed}"
    );
    assert_eq!(observed.matches("stall").count(), 1);
    assert_eq!(observed.matches("new-work").count(), 1);
    service.terminate_all_pane_processes().unwrap();
    drop(transports);
    fs::remove_dir_all(root).unwrap();
}
