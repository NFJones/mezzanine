//! Agent tests for mcp runtime behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies mcp action executor maps tool errors to failed results.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn mcp_action_executor_maps_tool_errors_to_failed_results() {
    let turn = turn();
    let action = mcp_action("mcp-1");
    let plan = mcp_plan();
    let mut executor = FakeMcpActionExecutor {
        plans: Vec::new(),
        response: McpToolCallResponse {
            content_json: r#"[{"type":"text","text":"denied"}]"#.to_string(),
            structured_content_json: None,
            is_error: true,
        },
    };

    let result = execute_mcp_action_through_runtime(&turn, &action, &plan, &mut executor).unwrap();

    assert_eq!(result.status, ActionStatus::Failed);
    assert!(result.is_error);
    assert_eq!(result.error.as_ref().unwrap().code, "mcp_tool_error");
    assert_eq!(result.content_texts(), vec!["denied"]);
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
        response: McpToolCallResponse {
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
