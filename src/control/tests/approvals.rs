//! Control approvals tests.

use super::*;

/// Verifies approval control methods list and decide blocked requests.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn approval_control_methods_list_and_decide_blocked_requests() {
    let (mut session, primary) = test_session();
    let mut queue = BlockedApprovalQueue::default();
    let approval_id = queue
        .create_at(
            BlockedApprovalRequest {
                id: String::new(),
                requesting_agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                parent_agent_chain: vec!["agent-0".to_string()],
                action_kind: "shell_command".to_string(),
                action_summary: "git diff".to_string(),
                declared_effects: vec!["read_filesystem".to_string()],
                matched_rules: vec!["git diff".to_string()],
                read_scopes: vec![".".to_string()],
                write_scopes: Vec::new(),
                cooperation_mode: None,
                created_at_unix_seconds: None,
                decided_at_unix_seconds: None,
                decided_by_client_id: None,
                state: BlockedApprovalState::Pending,
                decision: None,
                redirect_instruction: None,
            },
            10,
        )
        .unwrap();

    let list_response = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":1,"method":"approval/list","params":{}}"#,
        &mut session,
        &primary,
        &mut queue,
    );
    assert!(list_response.contains(&approval_id));
    assert!(list_response.contains(&format!(r#""id":"{}""#, approval_id)));
    assert!(list_response.contains(r#""version":1"#));
    assert!(list_response.contains(r#""state":"pending""#));
    assert!(list_response.contains(r#""requester":{"agent_id":"agent-1""#));
    assert!(list_response.contains(r#""parent_agent_chain":["agent-0"]"#));
    assert!(list_response.contains(r#""action_type":"shell_command""#));
    assert!(list_response.contains(r#""created_at":""#));
    assert!(list_response.contains(r#""decided_at":null"#));
    assert!(list_response.contains(r#""decided_by_client_id":null"#));
    assert!(list_response.contains(r#""summary":"git diff""#));
    assert!(list_response.contains(r#""effects":{"reads":["."]"#));
    assert!(list_response.contains(r#""scope":{"persistence":"project""#));
    assert!(list_response.contains(r#""instruction":null"#));
    assert!(list_response.contains(r#""matched_rules":["git diff"]"#));

    let pending_filter = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":11,"method":"approval/list","params":{"target":{"default":true},"state":"pending"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );
    assert!(pending_filter.contains(&approval_id), "{pending_filter}");

    let approved_filter = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":12,"method":"approval/list","params":{"state":"approved"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );
    assert!(
        approved_filter.contains(r#""approvals":[]"#),
        "{approved_filter}"
    );

    let decide_request = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"approval/decide","params":{{"approval_id":"{}","decision":"redirect","scope":{{"persistence":"once","command_prefix":["git","diff"]}},"instruction":"show diff summary only","idempotency_key":"decision-1"}}}}"#,
        approval_id
    );
    let decide_response = dispatch_control_request_with_approvals(
        &decide_request,
        &mut session,
        &primary,
        &mut queue,
    );

    assert!(decide_response.contains(r#""state":"redirected""#));
    assert!(decide_response.contains(r#""decision":"redirect""#));
    assert!(decide_response.contains(r#""decided_at":""#));
    assert!(decide_response.contains(&format!(r#""decided_by_client_id":"{}""#, primary)));
    assert!(decide_response.contains("show diff summary only"));

    let redirected_filter = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":13,"method":"approval/list","params":{"state":"redirected"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );
    assert!(
        redirected_filter.contains(&approval_id),
        "{redirected_filter}"
    );

    let invalid_filter = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":14,"method":"approval/list","params":{"state":"missing"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );
    assert!(
        invalid_filter.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_filter}"
    );
}

/// Verifies generic control dispatches empty approval state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn generic_control_dispatches_empty_approval_state() {
    let (mut session, primary) = test_session();

    let list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"approval/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(list.contains(r#""approvals":[]"#), "{list}");

    let filtered = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":11,"method":"approval/list","params":{"target":{"default":true},"state":"cancelled"}}"#,
        &mut session,
        &primary,
    );
    assert!(filtered.contains(r#""approvals":[]"#), "{filtered}");

    let missing = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"approval/decide","params":{"approval_id":"ba-missing","decision":"approve","idempotency_key":"missing-approval"}}"#,
        &mut session,
        &primary,
    );
    assert!(missing.contains(r#""error""#), "{missing}");
    assert!(missing.contains(r#""code":-32005"#), "{missing}");
    assert!(
        missing.contains(r#""mezzanine_code":"not_found""#),
        "{missing}"
    );
    assert!(missing.contains("approval request not found"), "{missing}");
}

/// Verifies approval decision control can emit required audit records.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn approval_decision_control_can_emit_required_audit_records() {
    let (mut session, primary) = test_session();
    let mut queue = BlockedApprovalQueue::default();
    let approval_id = queue
        .create_at(
            BlockedApprovalRequest {
                id: String::new(),
                requesting_agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                parent_agent_chain: Vec::new(),
                action_kind: "shell_command".to_string(),
                action_summary: "git status".to_string(),
                declared_effects: vec!["read_filesystem".to_string()],
                matched_rules: vec!["git status".to_string()],
                read_scopes: vec![".".to_string()],
                write_scopes: Vec::new(),
                cooperation_mode: None,
                created_at_unix_seconds: None,
                decided_at_unix_seconds: None,
                decided_by_client_id: None,
                state: BlockedApprovalState::Pending,
                decision: None,
                redirect_instruction: None,
            },
            10,
        )
        .unwrap();
    let root = temp_root("approval-audit");
    let path = root.join("audit.jsonl");
    let mut audit_log = AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: path.clone(),
        hash_chain: false,
        required: true,
    });
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"approval/decide","params":{{"approval_id":"{}","decision":"approve","idempotency_key":"audit-approval"}}}}"#,
        approval_id
    );

    let response = dispatch_control_request_with_approvals_and_audit(
        &request,
        &mut session,
        &primary,
        &mut queue,
        &mut audit_log,
    );

    assert!(response.contains(r#""state":"approved""#));
    let audit = fs::read_to_string(&path).unwrap();
    assert!(audit.contains(r#""event_type":"approval""#));
    assert!(audit.contains(r#""outcome":"started""#));
    assert!(audit.contains(r#""outcome":"applied""#));
    assert!(audit.contains(r#""approval_id""#));
    let _ = fs::remove_dir_all(root);
}
