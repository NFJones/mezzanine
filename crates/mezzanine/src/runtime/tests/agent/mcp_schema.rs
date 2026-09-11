//! Runtime tests for live MCP tool-schema drift and approval schema binding.
//!
//! These scenarios protect the pre-transport revalidation clause: the currently
//! selected schema and the plan identity are checked atomically inside the actor,
//! a drifted approval settles only its own unexecuted call, and unrelated schema
//! changes cannot disturb an approval that still matches.

use super::*;

/// Builds one available `state` tool fixture with the supplied input schema.
fn state_tool_state(name: &str, schema: &str) -> mez_agent::mcp::McpToolState {
    mez_agent::mcp::McpToolState {
        server_id: String::new(),
        name: name.to_string(),
        available: false,
        blacklisted: false,
        permission_required: false,
        effects: mez_agent::mcp::McpToolEffects::none(),
        approval: mez_agent::mcp::McpApprovalSetting::Allow,
        description: format!("state tool {name}"),
        input_schema_json: schema.to_string(),
    }
}

/// Registers the `state` MCP server with an explicit tool approval setting.
///
/// Re-registering the same server is how a test models one MCP metadata refresh.
fn register_state_server_with_approval(
    service: &mut RuntimeSessionService,
    approval: mez_agent::mcp::McpApprovalSetting,
    tools: Vec<mez_agent::mcp::McpToolState>,
) {
    let mut config =
        mez_agent::mcp::McpServerConfig::stdio("state", "state", "mcp-state", Vec::new());
    config.approval = approval;
    service.mcp_registry_mut().add_server(config).unwrap();
    service
        .mcp_registry_mut()
        .mark_available("state", tools, 1)
        .unwrap();
}

/// Registers the approval-free `state` MCP server with the supplied tools.
fn register_state_server(
    service: &mut RuntimeSessionService,
    tools: Vec<mez_agent::mcp::McpToolState>,
) {
    register_state_server_with_approval(service, mez_agent::mcp::McpApprovalSetting::Allow, tools);
}

/// Returns the retained model-visible context text for one turn.
///
/// A settled unexecuted call leaves live turn executions once its failure feedback
/// is queued, so the retained context is the durable evidence carrying the settled
/// status and error code.
fn retained_turn_context_text(service: &RuntimeSessionService, turn_id: &str) -> String {
    service
        .agent_turn_contexts()
        .get(turn_id)
        .map(|context| {
            context
                .blocks()
                .iter()
                .map(|block| block.content.clone())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Builds one MCP call action fixture for the shared `state` server.
fn state_mcp_action(action_id: &str, path: &str) -> mez_agent::AgentAction {
    mez_agent::AgentAction {
        id: action_id.to_string(),
        payload: mez_agent::AgentActionPayload::McpCall {
            server: "state".to_string(),
            tool: "list".to_string(),
            arguments_json: format!(r#"{{"path":"{path}"}}"#),
        },
    }
}

/// Starts one runtime turn whose provider completion reaches MCP tool planning.
async fn start_state_mcp_turn(
    service: &mut RuntimeSessionService,
    idempotency_key: &str,
) -> AgentTurnRecord {
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{{"idempotency_key":"{idempotency_key}","input":"list one path"}}}}"#
        ),
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap()
}

/// Applies one provider completion holding a settled sibling and a running call.
async fn complete_state_mcp_turn(
    service: &mut RuntimeSessionService,
    turn: &AgentTurnRecord,
    sibling: &mez_agent::AgentAction,
    running: &[mez_agent::AgentAction],
) -> bool {
    let mut request = runtime_model_request_fixture(&turn.turn_id);
    request.agent_id = turn.agent_id.clone();
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::Mcp);
    let execution = mez_agent::AgentTurnExecution {
        request,
        response: mez_agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text: "list two paths".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "list one path".to_string(),
                actions: std::iter::once(sibling.clone())
                    .chain(running.iter().cloned())
                    .collect(),
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: std::iter::once(mez_agent::ActionResult::succeeded(
            turn,
            sibling,
            vec!["sibling done".to_string()],
            None,
        ))
        .chain(running.iter().map(|action| {
            mez_agent::ActionResult::running(
                turn,
                action,
                vec!["mcp call accepted for worker execution".to_string()],
                None,
            )
        }))
        .collect(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .apply_agent_provider_completed_event(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
            execution,
        )
        .await
        .unwrap()
}

/// Verifies a live schema refresh settles only the unexecuted call: the approved
/// call fails with bounded no-dispatch evidence, and an already successful
/// sibling is neither replayed nor re-run.
#[tokio::test]
async fn live_mcp_schema_drift_settles_only_the_unexecuted_call() {
    let list_schema =
        r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#;
    let mut service = test_runtime_service();
    register_state_server(&mut service, vec![state_tool_state("list", list_schema)]);
    let turn = start_state_mcp_turn(&mut service, "mcp-schema-drift").await;
    let sibling = state_mcp_action("list-sibling", "sibling");
    let running = state_mcp_action("list-drift", ".");

    assert!(
        complete_state_mcp_turn(
            &mut service,
            &turn,
            &sibling,
            std::slice::from_ref(&running)
        )
        .await,
        "provider completion was not applied"
    );
    assert_eq!(
        service.pending_approved_external_actions(),
        vec![(turn.turn_id.clone(), running.id.clone())]
    );

    // The server refreshes the selected schema without changing which arguments
    // are valid, so only the schema generation differs from the approved one.
    let refreshed_schema = r#"{"type":"object","description":"v2","properties":{"path":{"type":"string"}},"required":["path"]}"#;
    register_state_server(
        &mut service,
        vec![state_tool_state("list", refreshed_schema)],
    );

    // Park the parent turn the way a joined parent waits for external work: the
    // queued calls stay authorized and the settled call stays observable.
    service
        .agent_scheduler_mut()
        .wait_running(&turn.turn_id)
        .unwrap();
    service
        .agent_turn_ledger_mut()
        .finish_turn(&turn.turn_id, AgentTurnState::Blocked)
        .unwrap();
    let dispatch = service
        .claim_approved_external_action(&turn.turn_id, &running.id)
        .unwrap();
    assert!(
        dispatch.is_none(),
        "a drifted approval must not reach a live transport"
    );
    // The drifted call is settled as a bounded schema-drift failure rather than as
    // the transport fault this call would otherwise hit, so removing the drift
    // comparison fails this assertion instead of passing as an unwritten result.
    let context = retained_turn_context_text(&service, &turn.turn_id);
    assert!(
        context.contains("[action_result list-drift mcp_call failed]"),
        "{context}"
    );
    assert!(context.contains("error: mcp_schema_changed"), "{context}");
    // The already successful sibling is neither re-queued nor replayed: it stays
    // settled, and the settled call released its claim.
    assert!(
        context.contains("[action_result list-sibling mcp_call succeeded]"),
        "{context}"
    );
    assert!(
        service.pending_approved_external_actions().is_empty(),
        "{:?}",
        service.pending_approved_external_actions()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies an unrelated tool schema change leaves the approved call's binding
/// intact: revalidation passes and the call fails later at transport acquisition
/// instead of being reported as stale approval.
#[tokio::test]
async fn unrelated_mcp_schema_change_leaves_the_approval_binding_intact() {
    let list_schema =
        r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#;
    let mut service = test_runtime_service();
    register_state_server(&mut service, vec![state_tool_state("list", list_schema)]);
    let turn = start_state_mcp_turn(&mut service, "mcp-schema-unrelated").await;
    let sibling = state_mcp_action("list-sibling", "sibling");
    let running = state_mcp_action("list-drift", ".");
    assert!(
        complete_state_mcp_turn(
            &mut service,
            &turn,
            &sibling,
            std::slice::from_ref(&running)
        )
        .await
    );

    // Another tool of the same server changes; the approved tool's schema bytes,
    // and therefore its generation, are unchanged.
    register_state_server(
        &mut service,
        vec![
            state_tool_state("list", list_schema),
            state_tool_state(
                "other",
                r#"{"type":"object","properties":{"q":{"type":"string"}}}"#,
            ),
        ],
    );

    let dispatch = service
        .claim_approved_external_action(&turn.turn_id, &running.id)
        .unwrap();
    assert!(dispatch.is_none());
    let execution = service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .cloned()
        .unwrap();
    let revalidated = execution
        .action_results
        .iter()
        .find(|result| result.action_id == running.id)
        .unwrap();
    // The call passed schema revalidation and reached transport acquisition, so
    // it is neither reported as stale approval nor permanently failed: the claim
    // is released so the worker can retry once a transport exists.
    assert_eq!(revalidated.status, ActionStatus::Running);
    assert!(revalidated.error.is_none());
    assert_eq!(
        service.pending_approved_external_actions(),
        vec![(turn.turn_id.clone(), running.id.clone())]
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies an approved MCP call whose approval recorded no schema generation
/// fails closed: the unexecuted call is settled with a bounded reason before any
/// transport is acquired instead of dispatching with no schema comparison.
#[tokio::test]
async fn approved_mcp_call_without_bound_generation_is_not_dispatched() {
    let list_schema =
        r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#;
    let mut service = test_runtime_service();
    register_state_server_with_approval(
        &mut service,
        mez_agent::mcp::McpApprovalSetting::Prompt,
        vec![state_tool_state("list", list_schema)],
    );
    let turn = start_state_mcp_turn(&mut service, "mcp-schema-unbound").await;
    let sibling = state_mcp_action("list-sibling", "sibling");
    let running = state_mcp_action("list-unbound", ".");
    assert!(
        complete_state_mcp_turn(
            &mut service,
            &turn,
            &sibling,
            std::slice::from_ref(&running)
        )
        .await,
        "provider completion was not applied"
    );
    assert!(
        service
            .agent_turn_executions()
            .get(&turn.turn_id)
            .is_some_and(|execution| execution.action_results.iter().any(|result| {
                result.action_id == running.id && result.status == ActionStatus::Blocked
            })),
        "an approval-required MCP call must block before dispatch"
    );
    let execution = service.agent_turn_executions()[&turn.turn_id].clone();
    let approval_ids = service
        .queue_blocked_approvals_for_execution(&turn, &execution)
        .unwrap();
    let approval_id = approval_ids.first().cloned().expect("queued MCP approval");

    // The tool disappears between approval and re-plan, so the approval records no
    // schema generation at all. Approving then resumes the call as approved work.
    register_state_server_with_approval(
        &mut service,
        mez_agent::mcp::McpApprovalSetting::Prompt,
        vec![state_tool_state(
            "other",
            r#"{"type":"object","properties":{"q":{"type":"string"}}}"#,
        )],
    );
    service
        .integration
        .blocked_approvals_mut()
        .decide_with_client_at(
            &approval_id,
            mez_agent::permissions::ApprovalDecision::Approve,
            None,
            Some("client-1".to_string()),
            crate::runtime::current_unix_seconds(),
        )
        .unwrap();
    let decided = service
        .blocked_approvals()
        .get(&approval_id)
        .cloned()
        .expect("decided approval");
    let controller = mez_core::ids::ClientId::opaque("client-1".to_string()).unwrap();
    assert_eq!(
        service
            .resume_approved_blocked_agent_action(&approval_id, &decided, &controller)
            .unwrap(),
        Some(1)
    );

    // The tool returns with the same schema, so live planning succeeds and only the
    // missing approval binding keeps this call from being dispatched unchecked.
    register_state_server_with_approval(
        &mut service,
        mez_agent::mcp::McpApprovalSetting::Prompt,
        vec![state_tool_state("list", list_schema)],
    );
    let dispatch = service
        .claim_approved_external_action(&turn.turn_id, &running.id)
        .unwrap();
    assert!(
        dispatch.is_none(),
        "an unbound approval must not reach a live transport"
    );
    let context = retained_turn_context_text(&service, &turn.turn_id);
    assert!(
        context.contains("[action_result list-unbound mcp_call failed]"),
        "{context}"
    );
    assert!(context.contains("error: mcp_schema_unbound"), "{context}");
    service.terminate_all_pane_processes().unwrap();
}

/// Builds one runtime service whose configured pre-MCP hook records its own run.
///
/// The hook is a plain program hook, so the runtime executes it synchronously and a
/// recorded marker proves the hook actually ran for that call.
fn service_with_marker_hook(marker: &Path) -> RuntimeSessionService {
    let mut service = test_runtime_service();
    let report = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[hooks.mcpguard]\nevent = \"pre_mcp_tool_use\"\nprogram = \"/bin/sh\"\nargs = [\"-c\", \"cat > \\\"$1\\\"\", \"hook\", \"{}\"]\non_failure = \"warn\"\n",
                marker.display()
            ),
        }])
        .unwrap();
    assert_eq!(report.hooks_configured, 1);
    service
}

/// Verifies a call settled for schema drift never runs its configured pre-MCP
/// hook: the current-schema comparison happens before hooks, so a drifted call
/// has no hook side effects, while a live call in the same harness still does.
#[tokio::test]
async fn drifted_mcp_call_does_not_run_its_pre_mcp_hook() {
    let root = temp_root("mcp-schema-drift-hook");
    let marker = root.join("pre-mcp-hook-ran");
    let list_schema =
        r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#;

    // Control: the configured hook does run for a call that reaches dispatch, so a
    // missing marker below is evidence about ordering rather than about wiring.
    let mut control = service_with_marker_hook(&marker);
    register_state_server(&mut control, vec![state_tool_state("list", list_schema)]);
    let turn = start_state_mcp_turn(&mut control, "mcp-schema-hook-control").await;
    let sibling = state_mcp_action("list-sibling", "sibling");
    let running = state_mcp_action("list-hook-control", ".");
    assert!(
        complete_state_mcp_turn(
            &mut control,
            &turn,
            &sibling,
            std::slice::from_ref(&running)
        )
        .await
    );
    assert!(
        control
            .claim_approved_external_action(&turn.turn_id, &running.id)
            .unwrap()
            .is_none()
    );
    assert!(
        marker.exists(),
        "the configured pre-MCP hook did not run for a live call"
    );
    control.terminate_all_pane_processes().unwrap();
    fs::remove_file(&marker).unwrap();

    // Drift: the same configured hook must not run for a call settled for a changed
    // schema generation.
    let mut service = service_with_marker_hook(&marker);
    register_state_server(&mut service, vec![state_tool_state("list", list_schema)]);
    let turn = start_state_mcp_turn(&mut service, "mcp-schema-drift-hook").await;
    let sibling = state_mcp_action("list-sibling", "sibling");
    let running = state_mcp_action("list-hook-drift", ".");
    assert!(
        complete_state_mcp_turn(
            &mut service,
            &turn,
            &sibling,
            std::slice::from_ref(&running)
        )
        .await
    );
    register_state_server(
        &mut service,
        vec![state_tool_state(
            "list",
            r#"{"type":"object","description":"v2","properties":{"path":{"type":"string"}},"required":["path"]}"#,
        )],
    );
    service
        .agent_scheduler_mut()
        .wait_running(&turn.turn_id)
        .unwrap();
    service
        .agent_turn_ledger_mut()
        .finish_turn(&turn.turn_id, AgentTurnState::Blocked)
        .unwrap();
    assert!(
        service
            .claim_approved_external_action(&turn.turn_id, &running.id)
            .unwrap()
            .is_none()
    );
    let context = retained_turn_context_text(&service, &turn.turn_id);
    assert!(
        context.contains("[action_result list-hook-drift mcp_call failed]"),
        "{context}"
    );
    assert!(context.contains("error: mcp_schema_changed"), "{context}");
    assert!(
        !marker.exists(),
        "a call settled for schema drift must not run its pre-MCP hook"
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}
