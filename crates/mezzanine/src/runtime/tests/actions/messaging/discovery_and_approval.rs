//! Bounded peer discovery and exact-message approval regressions.
//!
//! Project discovery excludes foreign peers unless session scope is requested.
//! An approval must not authorize a changed recipient or changed payload.

use super::fixtures::{messaging_test_execution, messaging_test_turn};
use super::*;

/// Executes one read-only `list_agents` action and returns its structured result.
fn execute_list_agents_action(
    service: &mut crate::runtime::RuntimeSessionService,
    turn: &mez_agent::AgentTurnRecord,
    agent_type: Option<&str>,
    scope: Option<&str>,
) -> serde_json::Value {
    let action = mez_agent::AgentAction {
        id: "list-agents-1".to_string(),

        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: agent_type.map(str::to_string),
            scope: scope.map(str::to_string),
        },
    };
    let planned =
        mez_agent::plan_action_result(turn, &action, mez_agent::ActionPlanningInput::default())
            .expect("list_agents plan");
    let mut execution =
        messaging_test_execution(turn, &action, planned, mez_agent::AgentTurnState::Running);
    assert_eq!(
        service
            .execute_running_list_agents_actions_for_turn(turn, &mut execution)
            .unwrap(),
        1
    );
    serde_json::from_str(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .expect("agent discovery structured content"),
    )
    .expect("agent discovery result json")
}

/// Verifies read-only agent discovery always includes the requesting agent,
/// defaults to primary agents only, and widens to internal controllers.
#[test]
fn runtime_list_agents_defaults_to_primary_and_includes_self() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "discover session peers")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    service
        .ensure_runtime_message_identity("agent-peer", None, "reviewer", &[], now_ms)
        .unwrap();
    service
        .ensure_runtime_message_identity("agent-internal", None, "worker", &[], now_ms)
        .unwrap();
    service.register_macro_managed_subagent(
        "agent-internal",
        &turn.turn_id,
        &turn.agent_id,
        "review",
    );

    let primary = execute_list_agents_action(&mut service, &turn, None, Some("session"));
    assert_eq!(primary["agent_type"], "primary");
    assert_eq!(primary["truncated"], false);
    let rows = primary["agents"].as_array().unwrap();
    assert!(rows.iter().all(|row| row["kind"] == "primary"));
    let self_row = rows
        .iter()
        .find(|row| row["is_self"] == true)
        .expect("requesting agent row");
    assert_eq!(self_row["agent_id"], turn.agent_id);
    assert!(rows.iter().any(|row| row["agent_id"] == "agent-peer"));
    assert!(!rows.iter().any(|row| row["agent_id"] == "agent-internal"));

    let internal =
        execute_list_agents_action(&mut service, &turn, Some("internal"), Some("session"));
    let internal_ids = internal["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["agent_id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(internal_ids, vec!["agent-internal".to_string()]);
    assert!(
        internal["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["kind"] == "internal")
    );

    let all = execute_list_agents_action(&mut service, &turn, Some("all"), Some("session"));
    let all_ids = all["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["agent_id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    for agent_id in ["agent-peer", "agent-internal", turn.agent_id.as_str()] {
        assert!(all_ids.contains(&agent_id.to_string()), "{agent_id}");
    }

    let subagents =
        execute_list_agents_action(&mut service, &turn, Some("subagent"), Some("session"));
    assert_eq!(subagents["count"], 0);
    assert!(subagents["agents"].as_array().unwrap().is_empty());
    service.terminate_all_pane_processes().unwrap();
}

/// Project-default discovery returns the requester and same-project peers,
/// while an explicit session scope widens the same list without exposing roots.
#[test]
fn runtime_list_agents_defaults_to_requester_project_and_session_widens() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "discover project peers")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let requester = service.runtime_message_sender_identity(&turn).unwrap();
    let project_scope = mez_agent::messaging::ProjectScopeId::from_canonical_root_bytes(
        b"/workspace/requester-project",
    );
    service
        .message_service_mut()
        .rebind_agent_project_scope(&requester.agent_id, Some(project_scope.clone()))
        .unwrap();
    let same_project = mez_agent::messaging::SenderIdentity {
        agent_id: AgentId::opaque("agent-same-project").unwrap(),
        project_scope: Some(project_scope),
        pane_id: None,
        window_id: None,
        role: Some("agent".to_string()),
        capabilities: Vec::new(),
        objective: None,
    };
    let other_project = mez_agent::messaging::SenderIdentity {
        agent_id: AgentId::opaque("agent-other-project").unwrap(),
        project_scope: Some(
            mez_agent::messaging::ProjectScopeId::from_canonical_root_bytes(
                b"/workspace/other-project",
            ),
        ),
        pane_id: None,
        window_id: None,
        role: Some("agent".to_string()),
        capabilities: Vec::new(),
        objective: None,
    };
    service
        .message_service_mut()
        .ensure_agent_identity(same_project.clone(), 0)
        .unwrap();
    service
        .message_service_mut()
        .ensure_agent_identity(other_project.clone(), 0)
        .unwrap();

    let project = execute_list_agents_action(&mut service, &turn, Some("all"), None);
    assert_eq!(project["scope"], "project");
    let project_ids = project["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["agent_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(project_ids.contains(&turn.agent_id.as_str()));
    assert!(project_ids.contains(&same_project.agent_id.as_str()));
    assert!(!project_ids.contains(&other_project.agent_id.as_str()));

    let session = execute_list_agents_action(&mut service, &turn, Some("all"), Some("session"));
    assert_eq!(session["scope"], "session");
    assert!(
        session["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["agent_id"] == other_project.agent_id.as_str())
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies persistent children expose bounded ownership metadata without
/// changing the authority represented by MMP discovery rows.
#[test]
fn runtime_list_agents_reports_persistent_parent_ownership() {
    let mut service = test_runtime_service();
    let parent_conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "discover my persistent MMP worker")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    service
        .ensure_runtime_message_identity("agent-%2", None, "worker", &["subagent"], now_ms)
        .unwrap();
    service.publish_prepared_runtime_agent_objective(
        "agent-%2",
        Some("Triage persistent peer requests"),
    );
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: turn.agent_id.clone(),
            root_agent_id: turn.agent_id.clone(),
            depth: 1,
            display_name: "persistent worker".to_string(),
            terminal: false,
        },
    );
    service.set_persistent_subagent(
        "agent-%2",
        crate::runtime::RuntimePersistentSubagent {
            conversation_id: "persistent-child-conversation".to_string(),
            parent_agent_id: turn.agent_id.clone(),
            parent_conversation_id,
            objective: "Triage persistent peer requests".to_string(),
        },
    );

    let listed = execute_list_agents_action(&mut service, &turn, Some("subagent"), Some("session"));
    let row = listed["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["agent_id"] == "agent-%2")
        .expect("persistent child discovery row");
    assert_eq!(row["persistent"], true);
    assert_eq!(row["parent_agent_id"], turn.agent_id);
    assert_eq!(row["owned_by_self"], true);
    assert_eq!(row["objective"], "Triage persistent peer requests");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies discovery rows honor the documented row and string bounds and that
/// an unsupported agent-type filter is rejected instead of widened.
#[test]
fn runtime_list_agents_bounds_rows_and_rejects_unknown_agent_type() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "discover many peers")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    for index in 0..(mez_agent::AGENT_LIST_MAX_ROWS + 3) {
        service
            .ensure_runtime_message_identity(
                &format!("agent-peer-{index:03}"),
                None,
                "worker",
                &[],
                now_ms,
            )
            .unwrap();
    }

    let all = execute_list_agents_action(&mut service, &turn, Some("all"), Some("session"));
    let rows = all["agents"].as_array().unwrap();
    assert_eq!(rows.len(), mez_agent::AGENT_LIST_MAX_ROWS);
    assert_eq!(all["truncated"], true);
    for row in rows {
        for field in ["agent_id", "role", "pane_id", "window_id", "objective"] {
            if let Some(value) = row[field].as_str() {
                assert!(
                    value.len() <= mez_agent::AGENT_LIST_MAX_STRING_BYTES,
                    "{field}"
                );
            }
        }
        for capability in row["capabilities"].as_array().unwrap() {
            assert!(capability.as_str().unwrap().len() <= mez_agent::AGENT_LIST_MAX_STRING_BYTES);
        }
    }

    let action = mez_agent::AgentAction {
        id: "list-agents-1".to_string(),

        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("peers".to_string()),
            scope: None,
        },
    };
    let planned =
        mez_agent::plan_action_result(&turn, &action, mez_agent::ActionPlanningInput::default())
            .expect("list_agents plan");
    let mut execution =
        messaging_test_execution(&turn, &action, planned, mez_agent::AgentTurnState::Running);
    assert!(
        service
            .execute_running_list_agents_actions_for_turn(&turn, &mut execution)
            .is_err()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Plans one ask-mode message action and queues its resumable blocked approval.
fn block_runtime_send_message(
    service: &mut crate::runtime::RuntimeSessionService,
    turn: &mez_agent::AgentTurnRecord,
    recipient: &str,
    payload: &str,
) -> (mez_agent::AgentAction, String) {
    let action = mez_agent::AgentAction {
        id: "message-1".to_string(),

        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient: recipient.to_string(),
            scope: Some("session".to_string()),
            content_type: "text/plain; charset=utf-8".to_string(),
            payload: payload.to_string(),
            correlation_id: None,
        },
    };
    let blocked = mez_agent::plan_action_result(
        turn,
        &action,
        mez_agent::ActionPlanningInput {
            approval_policy: mez_agent::ApprovalPolicy::Ask,
            message_rule_decision: Some(mez_agent::permissions::RuleDecision::Prompt),
            ..mez_agent::ActionPlanningInput::default()
        },
    )
    .expect("message plan");
    assert_eq!(blocked.status, ActionStatus::Blocked);
    let execution =
        messaging_test_execution(turn, &action, blocked, mez_agent::AgentTurnState::Blocked);
    service
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution.clone());
    // Register the owning assistant execution so the settled message evidence
    // can be committed to the turn context on resumption.
    service
        .append_agent_execution_chronology(turn, &execution)
        .unwrap();
    let approval_ids = service
        .queue_blocked_approvals_for_execution(turn, &execution)
        .expect("queued message approval");
    assert_eq!(approval_ids.len(), 1);
    (action, approval_ids[0].clone())
}

/// Approves one queued blocked approval and returns its decided record.
fn approve_blocked_runtime_action(
    service: &mut crate::runtime::RuntimeSessionService,
    approval_id: &str,
) -> mez_agent::permissions::BlockedApprovalRequest {
    service
        .integration
        .blocked_approvals_mut()
        .decide_with_client_at(
            approval_id,
            mez_agent::permissions::ApprovalDecision::Approve,
            None,
            Some("client-1".to_string()),
            current_unix_seconds(),
        )
        .expect("approve blocked action");
    service
        .blocked_approvals()
        .get(approval_id)
        .cloned()
        .expect("decided approval")
}

/// Verifies an ask-mode message blocks with a bounded approval payload and
/// delivers exactly once after `/approve` resumes it.
#[test]
fn runtime_send_message_approval_blocks_and_resumes_after_approve() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "message a peer agent")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let target = AgentId::opaque("agent-peer").unwrap();
    service
        .ensure_runtime_message_identity("agent-peer", None, "worker", &[], now_ms)
        .unwrap();

    let (_, approval_id) =
        block_runtime_send_message(&mut service, &turn, "agent:agent-peer", "hello peer");
    let approval = service
        .blocked_approvals()
        .get(&approval_id)
        .cloned()
        .expect("queued approval");
    assert_eq!(approval.action_kind, "send_message");
    assert_eq!(approval.action_summary, "send_message to agent:agent-peer");
    assert!(
        !approval
            .declared_effects
            .iter()
            .any(|effect| effect.contains("hello peer"))
    );

    let decided = approve_blocked_runtime_action(&mut service, &approval_id);
    let controller = mez_core::ids::ClientId::opaque("client-1".to_string()).unwrap();
    assert_eq!(
        service
            .resume_approved_blocked_agent_action(&approval_id, &decided, &controller)
            .unwrap(),
        Some(1)
    );

    let stored = service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .cloned()
        .expect("resumed execution");
    assert_eq!(stored.action_results[0].status, ActionStatus::Succeeded);
    let structured: serde_json::Value = serde_json::from_str(
        stored.action_results[0]
            .structured_content_json
            .as_deref()
            .expect("delivery structured content"),
    )
    .unwrap();
    assert_eq!(structured["scope"], "session");
    assert_eq!(structured["delivery_status"], "accepted");
    let messages = service.message_service().receive_for(&target, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].payload, "hello peer");
    // The approval record is retained for audit while its resumable reference
    // is consumed by the resumed delivery.
    assert_eq!(
        service
            .blocked_approvals()
            .get(&approval_id)
            .expect("retained approval record")
            .state,
        mez_agent::permissions::BlockedApprovalState::Approved
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies an approved send is re-validated against the recipient and payload
/// identity it was approved for, and delivers nothing when either changed.
#[test]
fn runtime_send_message_approval_rejects_changed_recipient_or_payload() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "message a peer agent")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let target = AgentId::opaque("agent-peer").unwrap();
    let other = AgentId::opaque("agent-other").unwrap();
    for agent_id in ["agent-peer", "agent-other"] {
        service
            .ensure_runtime_message_identity(agent_id, None, "worker", &[], now_ms)
            .unwrap();
    }

    let (_, approval_id) =
        block_runtime_send_message(&mut service, &turn, "agent:agent-peer", "hello peer");
    let decided = approve_blocked_runtime_action(&mut service, &approval_id);
    let controller = mez_core::ids::ClientId::opaque("client-1".to_string()).unwrap();

    let mut execution = service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .cloned()
        .expect("blocked execution");
    let mez_agent::AgentActionPayload::SendMessage { payload, .. } =
        &mut execution.response.action_batch.as_mut().unwrap().actions[0].payload
    else {
        panic!("send_message action");
    };
    *payload = "changed payload".to_string();
    service
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution.clone());
    let error = service
        .resume_approved_blocked_agent_action(&approval_id, &decided, &controller)
        .expect_err("changed payload must not resume");
    assert!(error.message().contains("no longer matches"), "{error:?}");

    let mez_agent::AgentActionPayload::SendMessage { payload, .. } =
        &mut execution.response.action_batch.as_mut().unwrap().actions[0].payload
    else {
        panic!("send_message action");
    };
    *payload = "hello peer".to_string();
    let mez_agent::AgentActionPayload::SendMessage { recipient, .. } =
        &mut execution.response.action_batch.as_mut().unwrap().actions[0].payload
    else {
        panic!("send_message action");
    };
    *recipient = format!("agent:{other}");
    service
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution.clone());
    let error = service
        .resume_approved_blocked_agent_action(&approval_id, &decided, &controller)
        .expect_err("changed recipient must not resume");
    assert!(error.message().contains("no longer matches"), "{error:?}");
    assert!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .is_empty()
    );

    let mez_agent::AgentActionPayload::SendMessage { recipient, .. } =
        &mut execution.response.action_batch.as_mut().unwrap().actions[0].payload
    else {
        panic!("send_message action");
    };
    *recipient = "agent:agent-peer".to_string();
    service
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution);
    assert_eq!(
        service
            .resume_approved_blocked_agent_action(&approval_id, &decided, &controller)
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        service
            .message_service()
            .receive_for(&target, u64::MAX)
            .len(),
        1
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies discovery rows enforce the documented capability bound and signal
/// shortened text instead of silently dropping it.
#[test]
fn runtime_list_agents_bounds_capabilities_and_signals_row_truncation() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "discover an oversized peer")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let oversized = "c".repeat(mez_agent::AGENT_LIST_MAX_STRING_BYTES + 64);
    let capabilities = (0..(mez_agent::AGENT_LIST_MAX_CAPABILITIES + 5))
        .map(|_| oversized.as_str())
        .collect::<Vec<_>>();
    service
        .ensure_runtime_message_identity("agent-oversized", None, "worker", &capabilities, now_ms)
        .unwrap();

    let all = execute_list_agents_action(&mut service, &turn, Some("all"), Some("session"));
    let rows = all["agents"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|row| row["agent_id"] == "agent-oversized")
        .expect("oversized peer row");
    let row_capabilities = row["capabilities"].as_array().unwrap();
    assert_eq!(
        row_capabilities.len(),
        mez_agent::AGENT_LIST_MAX_CAPABILITIES,
        "one row must carry at most the documented capability bound"
    );
    for capability in row_capabilities {
        assert!(capability.as_str().unwrap().len() <= mez_agent::AGENT_LIST_MAX_STRING_BYTES);
    }
    assert_eq!(
        row["truncated"], true,
        "a shortened row must signal its bounded text"
    );
    let self_row = rows
        .iter()
        .find(|row| row["is_self"] == true)
        .expect("self row");
    assert_eq!(self_row["truncated"], false);
    service.terminate_all_pane_processes().unwrap();
}
