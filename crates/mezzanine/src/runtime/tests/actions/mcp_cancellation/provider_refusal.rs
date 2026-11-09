//! A real stdio tools/call may finish before its actor receives the result.
//! Its generated server records one call; refusal must retain the exact result
//! and leased transport, with no duplicate call or canonical adoption.

use super::*;

#[test]
fn runtime_provider_failure_refusal_retains_issued_mcp_outcome() {
    let mut body = heap_pinned_refusal_case();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(body.as_mut());
}

#[inline(never)]
fn heap_pinned_refusal_case() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
    Box::pin(refusal_case())
}

async fn refusal_case() {
    let root = temp_root("mcp-provider-failure-refusal");
    fs::create_dir_all(&root).unwrap();
    let calls = root.join("calls");
    let script = runtime_mcp_fixture_script(false).replace(
        "  *'\"method\":\"tools/call\"'*)\n",
        &format!(
            "  *'\"method\":\"tools/call\"'*)\nprintf '%s\\n' \"$line\" >> '{}'\n",
            calls.display()
        ),
    );
    let mut service = test_runtime_service();
    service.replace_config_layers_async(vec![ConfigLayer {
        name: "primary".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: format!("[mcp_servers.fixture]\ncommand=\"/bin/sh\"\nargs=[\"-c\", {}]\napproval=\"allow\"\ntool_timeout_ms=5000\n", toml_string(&script)),
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
    let dispatch =
        claim_fixture_call(&mut service, "turn-1", "retained-call", "original-work").await;
    let turn = dispatch.turn;
    let action = dispatch.action;
    let attempt = dispatch.attempt;
    let mcp = dispatch.mcp.unwrap();
    let mut transports = crate::runtime::RuntimeMcpTransportSet::default();
    transports.insert("fixture".into(), mcp.transport);
    let response = transports
        .call_tool_async(&mcp.plan, &mcp.environment, mcp.auth_store.as_ref())
        .await
        .unwrap();
    assert!(!response.is_error);
    assert_eq!(fs::read_to_string(&calls).unwrap().lines().count(), 1);
    let request = mez_agent::McpExecutionRequest::from(&mcp.plan);
    let result =
        mez_agent::mcp_response_to_action_result(&turn, &action, &request, response.into())
            .unwrap();
    let returned_transport = transports.take("fixture").unwrap();
    let execution = service.agent_turn_executions()[&turn.turn_id].clone();
    let context = service.agent_turn_contexts()[&turn.turn_id].clone();
    service
        .fail_configured_agent_provider_task(
            &turn.turn_id,
            &MezError::from(mez_agent::AgentContextError::new(
                "request context refused after MCP effect",
            )),
        )
        .unwrap();
    let outcome = || crate::runtime::RuntimeApprovedExternalActionOutcome {
        turn_id: turn.turn_id.clone(),
        action_id: action.id.clone(),
        attempt: attempt.clone(),
        result: Ok(result.clone()),
        mcp_transport: None,
    };
    // Return the exact completed lease with the original result. A lost reply
    // carries the same result/attempt, never another transport execution.
    let mut original = outcome();
    original.mcp_transport = Some(("fixture".into(), returned_transport));
    let applied = service.complete_approved_external_action(original);
    let retained = service.retained_external_result_for_tests(&turn.turn_id, &action.id, &attempt);
    let duplicate = service.complete_approved_external_action(outcome());
    let unchanged = service.agent_turn_contexts()[&turn.turn_id] == context
        && service.agent_turn_executions()[&turn.turn_id] == execution;
    let call_count = fs::read_to_string(&calls).unwrap().lines().count();
    let returned_lease = service
        .integration
        .mcp_transports_mut()
        .transports
        .contains_key("fixture");
    service.terminate_all_pane_processes().unwrap();
    drop(service);
    drop(transports);
    fs::remove_dir_all(root).unwrap();
    assert!(
        applied.unwrap(),
        "issued MCP result must remain under its original receipt"
    );
    assert_eq!(retained, Some(result));
    assert!(!duplicate.unwrap());
    assert!(unchanged);
    assert!(
        returned_lease,
        "completed transport must return only to its original lease"
    );
    assert_eq!(
        call_count, 1,
        "duplicate delivery cannot call the server again"
    );
}
