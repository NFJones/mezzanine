//! Safe metadata retrieval through the production runtime discovery executor.
//! Directory history and transport readiness are not tool-call permissions.

use super::*;

/// Passive lookup must report a useful bounded state/recovery category rather
/// than reflect arbitrary upstream errors into canonical action results. Raw
/// operator evidence stays private in registry state, and lookup starts nothing.
#[test]
fn runtime_mcp_directory_get_excludes_raw_server_errors() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .mcp_registry_mut()
        .add_server(mez_agent::mcp::McpServerConfig::stdio(
            "atlassian",
            "Atlassian",
            "never-executed-fixture",
            Vec::new(),
        ))
        .unwrap();
    let raw = format!(
        "upstream https://user:SYNTHETIC_MCP_SECRET@invalid.example/?token=SYNTHETIC_MCP_SECRET\nserver=forged\n\u{1b}[31m{}",
        "SYNTHETIC_MCP_ERROR_BODY ".repeat(8192),
    );
    service
        .mcp_registry_mut()
        .mark_failed("atlassian", raw.clone(), 1)
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "Inspect safe metadata")
        .unwrap();
    service.remove_pending_agent_provider_task(&started.turn_id);
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .cloned()
        .unwrap();
    let mut execution = metadata_execution(&turn, "atlassian");
    service
        .execute_running_mcp_discovery_actions_for_turn(&turn, &mut execution)
        .unwrap();
    let result = &execution.action_results[0];
    assert_eq!(result.status, ActionStatus::Succeeded);
    let encoded = result.structured_content_json.as_deref().unwrap();
    assert!(
        encoded.len() < 2048,
        "diagnostic lookup must not reflect an unbounded error"
    );
    assert!(!encoded.contains("SYNTHETIC_MCP_SECRET"));
    assert!(!encoded.contains("SYNTHETIC_MCP_ERROR_BODY"));
    assert!(!encoded.contains("server=forged"));
    let value: serde_json::Value = serde_json::from_str(encoded).unwrap();
    assert_eq!(value["server"]["state"], "failed");
    assert_eq!(value["server"]["reason"], "startup or protocol failure");
    assert_eq!(
        value["server"]["recovery"],
        "inspect_operator_diagnostics_then_explicit_retry"
    );
    assert!(mez_agent::mcp_retrieved_manifest_for_action_result(result).is_none());
    assert_eq!(
        service
            .mcp_registry()
            .list_servers()
            .into_iter()
            .find(|server| server.configured.id == "atlassian")
            .unwrap()
            .blacklist_reason
            .as_deref(),
        Some(raw.as_str())
    );
}

/// Plans one exact-ID metadata action without inventing prior search or
/// manifest evidence. The runtime executor remains responsible for lookup.
fn metadata_execution(
    turn: &mez_agent::AgentTurnRecord,
    server: &str,
) -> mez_agent::AgentTurnExecution {
    let action = mez_agent::AgentAction {
        id: "metadata-1".into(),
        payload: mez_agent::AgentActionPayload::McpServerGet {
            server: server.into(),
        },
    };
    mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "metadata".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "Inspect safe configured metadata".into(),
                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![mez_agent::ActionResult::running(
            turn,
            &action,
            Vec::new(),
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Running,
    }
}

/// Exact configured-ID retrieval must work with no reference/search and empty
/// optional metadata, across pending/starting/failed/blacklisted/disabled states.
/// Reading metadata neither starts a transport nor manufactures callable tools.
#[test]
fn runtime_mcp_directory_get_reports_configured_states_without_search() {
    for state in [
        "configured",
        "starting",
        "failed",
        "blacklisted",
        "disabled",
        "available",
    ] {
        let mut service = test_runtime_service();
        service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let mut config = mez_agent::mcp::McpServerConfig::stdio(
            "atlassian",
            "Atlassian",
            "must-not-start-secret-command",
            Vec::new(),
        );
        config.enabled = state != "disabled";
        config.env_vars.push("PRIVATE_MCP_TOKEN".into());
        service.mcp_registry_mut().add_server(config).unwrap();
        match state {
            "starting" => service
                .mcp_registry_mut()
                .mark_starting("atlassian", 1)
                .unwrap(),
            "failed" => service
                .mcp_registry_mut()
                .mark_failed("atlassian", "fixture handshake failed", 1)
                .unwrap(),
            "blacklisted" => service
                .mcp_registry_mut()
                .blacklist_for_session("atlassian", "fixture blacklist", 1)
                .unwrap(),
            "available" => service
                .mcp_registry_mut()
                .mark_available("atlassian", Vec::new(), 1)
                .unwrap(),
            _ => {}
        }
        let expected = service
            .mcp_registry()
            .agent_shell_server_summary("atlassian")
            .unwrap();
        let started = service
            .start_agent_prompt_turn("%1", "Inspect integration status")
            .unwrap();
        service.remove_pending_agent_provider_task(&started.turn_id);
        let turn = service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .cloned()
            .unwrap();
        let mut execution = metadata_execution(&turn, "atlassian");
        assert_eq!(
            service
                .execute_running_mcp_discovery_actions_for_turn(&turn, &mut execution)
                .unwrap(),
            1
        );
        let result = &execution.action_results[0];
        assert_eq!(
            result.status,
            ActionStatus::Succeeded,
            "{state}: {result:?}"
        );
        let value: serde_json::Value =
            serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
        assert_eq!(value["server"]["state"], expected.state);
        assert_eq!(value["server"]["status"], expected.status);
        assert_eq!(value["server"]["enabled"], expected.enabled);
        assert_eq!(
            service
                .mcp_registry()
                .agent_shell_server_summary("atlassian")
                .unwrap(),
            expected
        );
        assert!(
            !result
                .structured_content_json
                .as_deref()
                .unwrap()
                .contains("must-not-start-secret-command")
        );
        assert!(
            !result
                .structured_content_json
                .as_deref()
                .unwrap()
                .contains("PRIVATE_MCP_TOKEN")
        );
        assert!(
            mez_agent::invoked_mcp_tools_for_context(
                service.agent_turn_contexts().get(&turn.turn_id).unwrap(),
                &service.mcp_registry().prompt_summary(),
            )
            .is_empty()
        );
        if state != "available" {
            assert!(mez_agent::mcp_retrieved_manifest_for_action_result(result).is_none());
        }
        let mut missing = metadata_execution(&turn, "unknown");
        service
            .execute_running_mcp_discovery_actions_for_turn(&turn, &mut missing)
            .unwrap();
        assert_eq!(missing.action_results[0].status, ActionStatus::Failed);
        assert_eq!(
            missing.action_results[0].error.as_ref().unwrap().code,
            "not_found"
        );
    }
}

/// Exercises reference, automatic exposure, same-batch search and known-ID
/// lookup through ordinary asynchronous provider-result adoption. The complete
/// available schema becomes canonical evidence once, while directory context
/// alone exposes nothing and later live unavailability withdraws call planning.
#[tokio::test]
async fn runtime_mcp_directory_retrieval_adopts_manifest_for_each_discovery_path() {
    for mode in ["reference", "automatic", "search", "known-id"] {
        let mut service = test_runtime_service();
        service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let mut config = mez_agent::mcp::McpServerConfig::stdio(
            "atlassian",
            "Atlassian",
            "never-start-fixture",
            Vec::new(),
        );
        config.approval = mez_agent::mcp::McpApprovalSetting::Prompt;
        service.mcp_registry_mut().add_server(config).unwrap();
        service.mcp_registry_mut().mark_available("atlassian", vec![mez_agent::mcp::McpToolState {
            server_id: String::new(), name: "lookup".into(), available: true, blacklisted: false,
            permission_required: false, effects: mez_agent::mcp::McpToolEffects::none(),
            approval: mez_agent::mcp::McpApprovalSetting::Allow,
            description: "Lookup fixture data".into(),
            input_schema_json: r#"{"type":"object","properties":{"key":{"type":"string"}},"required":["key"],"additionalProperties":false}"#.into(),
        }], 1).unwrap();
        if mode == "automatic" {
            service
                .integration
                .replace_always_exposed_mcp_servers(vec!["atlassian".into()]);
        }
        let prompt = if mode == "reference" {
            "Use @atlassian"
        } else {
            "Inspect integration metadata"
        };
        let started = service.start_agent_prompt_turn("%1", prompt).unwrap();
        service.remove_pending_agent_provider_task(&started.turn_id);
        let turn = service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .cloned()
            .unwrap();
        let profile = service
            .agent_turn_model_profile(&turn.turn_id)
            .unwrap()
            .clone();
        assert!(
            mez_agent::invoked_mcp_tools_for_context(
                service.agent_turn_contexts().get(&turn.turn_id).unwrap(),
                &service.mcp_registry().prompt_summary(),
            )
            .is_empty()
        );
        let mut execution = metadata_execution(&turn, "atlassian");
        execution.final_turn = false;
        if mode == "search" {
            let action = mez_agent::AgentAction {
                id: "search-1".into(),
                payload: mez_agent::AgentActionPayload::McpServerSearch {
                    query: "atlassian".into(),
                    limit: Some(5),
                },
            };
            execution
                .response
                .action_batch
                .as_mut()
                .unwrap()
                .actions
                .insert(0, action.clone());
            execution.action_results.insert(
                0,
                mez_agent::ActionResult::running(&turn, &action, Vec::new(), None),
            );
        }
        let settled = service
            .apply_agent_provider_execution_async_for_tests(&turn, &profile, execution)
            .await
            .unwrap();
        assert!(
            settled
                .action_results
                .iter()
                .all(|result| result.status == ActionStatus::Succeeded),
            "{mode}: {:?}",
            settled.action_results
        );
        let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
        assert_eq!(
            context
                .blocks()
                .iter()
                .filter(|block| block.source == ContextSourceKind::McpRetrievedManifest)
                .count(),
            1
        );
        let tools = mez_agent::invoked_mcp_tools_for_context(
            context,
            &service.mcp_registry().prompt_summary(),
        );
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_name, "lookup");
        assert!(tools[0].input_schema_json.contains("additionalProperties"));
        let request = mez_agent::mcp::McpToolCallRequest {
            server_id: "atlassian".into(),
            tool_name: "lookup".into(),
            arguments_json: r#"{"key":"fixture"}"#.into(),
            timeout_ms: None,
            approval_bypass: false,
        };
        let plan = service.mcp_registry_mut().plan_tool_call(&request).unwrap();
        assert!(
            plan.approval_required,
            "retrieval must not satisfy approval"
        );
        let mut invalid = request.clone();
        invalid.arguments_json = "{}".into();
        assert!(service.mcp_registry_mut().plan_tool_call(&invalid).is_err());
        service
            .mcp_registry_mut()
            .mark_unavailable("atlassian", "fixture disconnected", 2)
            .unwrap();
        assert!(service.mcp_registry().available_tools().is_empty());
        assert!(service.mcp_registry_mut().plan_tool_call(&request).is_err());
    }
}
