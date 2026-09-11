//! Agent tests for mcp runtime behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;
use crate::integrations::agent::actions::execute_mcp_action_through_runtime_async;

/// Records asynchronous MCP execution requests so tests can assert dispatch counts.
struct RecordingAsyncMcpActionExecutor {
    plans: Vec<McpExecutionRequest>,
    response: McpExecutionResponse,
}

impl mez_agent::AsyncMcpActionExecutor for RecordingAsyncMcpActionExecutor {
    type Error = crate::error::MezError;

    async fn execute_mcp_call_async(
        &mut self,
        request: &McpExecutionRequest,
    ) -> Result<McpExecutionResponse> {
        self.plans.push(request.clone());
        Ok(self.response.clone())
    }
}

/// Builds one available state-server tool fixture that requires `path`.
fn state_tool_state() -> mez_agent::mcp::McpToolState {
    mez_agent::mcp::McpToolState {
        server_id: String::new(),
        name: "list".to_string(),
        available: false,
        blacklisted: false,
        permission_required: false,
        effects: mez_agent::mcp::McpToolEffects::none(),
        approval: mez_agent::mcp::McpApprovalSetting::Allow,
        description: "List one path".to_string(),
        input_schema_json:
            r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#
                .to_string(),
    }
}

/// Builds one registered state MCP server fixture for planning tests.
fn state_registry() -> McpRegistry {
    let mut registry = McpRegistry::default();
    let mut config =
        mez_agent::mcp::McpServerConfig::stdio("state", "state", "mcp-state", Vec::new());
    config.approval = mez_agent::mcp::McpApprovalSetting::Allow;
    registry.add_server(config).unwrap();
    registry
        .mark_available("state", vec![state_tool_state()], 1)
        .unwrap();
    registry
}

/// Builds one MCP tool-call request against the shared state fixture.
fn state_request(arguments_json: &str) -> mez_agent::mcp::McpToolCallRequest {
    mez_agent::mcp::McpToolCallRequest {
        server_id: "state".to_string(),
        tool_name: "list".to_string(),
        arguments_json: arguments_json.to_string(),
        timeout_ms: None,
        approval_bypass: false,
    }
}

#[test]
/// Verifies a rejected MCP call reaches the registry as a bounded invalid-args
/// failure with zero transport dispatches, and that the model's corrected call is
/// then planned against the same selected schema and dispatches exactly once.
///
/// This regression scenario documents the behavior being protected so a failure
/// points at a concrete contract change rather than an incidental implementation
/// detail.
fn rejected_arguments_dispatch_no_transport_and_the_repaired_call_dispatches_once() {
    let turn = turn();
    let action = mcp_action("mcp-1");
    let mut registry = state_registry();
    let mut executor = FakeMcpActionExecutor {
        plans: Vec::new(),
        response: McpExecutionResponse {
            content_json: r#"[{"type":"text","text":"ok"}]"#.to_string(),
            structured_content_json: None,
            is_error: false,
        },
    };

    let rejected = registry
        .plan_tool_call(&state_request("{}"))
        .expect_err("missing required path was accepted");
    assert_eq!(rejected.kind(), mez_agent::mcp::McpErrorKind::InvalidArgs);
    assert!(
        rejected
            .message()
            .contains("category=instance_violates_schema")
    );
    assert!(executor.plans.is_empty());

    let plan = registry
        .plan_tool_call(&state_request(r#"{"path":"."}"#))
        .unwrap();
    let execution_request = McpExecutionRequest::from(&plan);
    let result =
        execute_mcp_action_through_runtime(&turn, &action, &execution_request, &mut executor)
            .unwrap();

    assert_eq!(result.status, ActionStatus::Succeeded);
    assert_eq!(executor.plans.len(), 1);
    assert_eq!(executor.plans[0].schema_generation, plan.schema_generation);
}

#[tokio::test]
/// Verifies the synchronous and asynchronous MCP dispatch adapters both reject an
/// unstamped plan before invoking a transport, and both dispatch exactly once when
/// the plan carries the approved schema generation.
///
/// This regression scenario documents the behavior being protected so a failure
/// points at a concrete contract change rather than an incidental implementation
/// detail.
async fn sync_and_async_dispatch_paths_require_a_bounded_schema_generation() {
    let turn = turn();
    let action = mcp_action("mcp-1");
    let plan = mcp_plan();
    let response = McpExecutionResponse {
        content_json: r#"[{"type":"text","text":"ok"}]"#.to_string(),
        structured_content_json: None,
        is_error: false,
    };
    let mut sync_executor = FakeMcpActionExecutor {
        plans: Vec::new(),
        response: response.clone(),
    };
    let mut async_executor = RecordingAsyncMcpActionExecutor {
        plans: Vec::new(),
        response: response.clone(),
    };

    let mut unstamped = plan.clone();
    unstamped.schema_generation.clear();
    assert!(
        execute_mcp_action_through_runtime(&turn, &action, &unstamped, &mut sync_executor).is_err()
    );
    assert!(
        execute_mcp_action_through_runtime_async(&turn, &action, &unstamped, &mut async_executor)
            .await
            .is_err()
    );
    assert!(sync_executor.plans.is_empty());
    assert!(async_executor.plans.is_empty());

    execute_mcp_action_through_runtime(&turn, &action, &plan, &mut sync_executor).unwrap();
    execute_mcp_action_through_runtime_async(&turn, &action, &plan, &mut async_executor)
        .await
        .unwrap();
    assert_eq!(sync_executor.plans.len(), 1);
    assert_eq!(async_executor.plans.len(), 1);
    assert_eq!(
        sync_executor.plans[0].schema_generation,
        plan.schema_generation
    );
    assert_eq!(
        async_executor.plans[0].schema_generation,
        plan.schema_generation
    );
}

#[test]
/// Verifies mcp action executor preserves tool errors as successful action evidence.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn mcp_action_executor_preserves_tool_errors_as_successful_evidence() {
    let turn = turn();
    let action = mcp_action("mcp-1");
    let plan = mcp_plan();
    let mut executor = FakeMcpActionExecutor {
        plans: Vec::new(),
        response: McpExecutionResponse {
            content_json: r#"[{"type":"text","text":"denied"}]"#.to_string(),
            structured_content_json: None,
            is_error: true,
        },
    };

    let result = execute_mcp_action_through_runtime(&turn, &action, &plan, &mut executor).unwrap();

    assert_eq!(result.status, ActionStatus::Succeeded);
    assert!(!result.is_error);
    assert!(result.error.is_none());
    assert_eq!(result.content_texts(), vec!["denied"]);
    assert!(
        result
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("\"is_error\":true")
    );
}

#[test]
/// Verifies mcp action executor maps tool response to action result.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn mcp_action_executor_maps_tool_response_to_action_result() {
    let turn = turn();
    let action = mcp_action("mcp-1");
    let plan = mcp_plan();
    let mut executor = FakeMcpActionExecutor {
        plans: Vec::new(),
        response: McpExecutionResponse {
            content_json: r#"[{"type":"text","text":"ok"}]"#.to_string(),
            structured_content_json: Some(r#"{"items":1}"#.to_string()),
            is_error: false,
        },
    };

    let result = execute_mcp_action_through_runtime(&turn, &action, &plan, &mut executor).unwrap();

    assert_eq!(result.status, ActionStatus::Succeeded);
    assert_eq!(result.content_texts(), vec!["ok"]);
    assert_eq!(executor.plans, vec![plan]);
    assert!(
        result
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("\"server\":\"state\"")
    );
}
