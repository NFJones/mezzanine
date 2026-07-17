//! Runtime tests for config permissions behavior.

use super::*;

/// Verifies ensure private socket directory rejects group permissions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn ensure_private_socket_directory_rejects_group_permissions() {
    let root = std::env::temp_dir().join(format!("mez-runtime-test-mode-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();

    let error = ensure_private_socket_directory(&root, effective_uid()).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    let _ = fs::remove_dir_all(&root);
}

/// Verifies runtime control approval methods use runtime owned queue.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_approval_methods_use_runtime_owned_queue() {
    let mut service = test_runtime_service();
    let audit_root = temp_root("runtime-approval-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let approval_id = service
        .queue_blocked_approval(BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-%1".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "cargo test".to_string(),
            declared_effects: vec!["process_control".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: vec![".".to_string()],
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: mez_agent::permissions::BlockedApprovalState::Approved,
            decision: Some(mez_agent::permissions::ApprovalDecision::Disapprove),
            redirect_instruction: Some("ignored by create".to_string()),
        })
        .unwrap();

    let mut connection = ControlConnectionState::trusted_existing_client(primary.clone());
    let list = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"approval/list","params":{}}"#,
    );
    let (list_output, _) = service
        .handle_control_input_for_connection(&list, 4096, &mut connection)
        .unwrap();
    let (list_body, _) = decode_control_frame(&list_output, 4096).unwrap();
    assert!(list_body.contains(&format!(r#""approval_id":"{}""#, approval_id)));
    assert!(list_body.contains(r#""state":"pending""#), "{list_body}");
    assert!(list_body.contains(r#""created_at":""#), "{list_body}");
    assert!(list_body.contains(r#""decided_at":null"#), "{list_body}");

    let decide = encode_control_body(&format!(
        r#"{{"jsonrpc":"2.0","id":"decide","method":"approval/decide","params":{{"approval_id":"{}","decision":"approve","scope":{{"persistence":"session","command_prefix":["cargo","test"]}},"idempotency_key":"approval-decision"}}}}"#,
        approval_id
    ));
    let (decide_output, _) = service
        .handle_control_input_for_connection(&decide, 4096, &mut connection)
        .unwrap();
    let (decide_body, _) = decode_control_frame(&decide_output, 4096).unwrap();
    assert!(
        decide_body.contains(r#""state":"approved""#),
        "{decide_body}"
    );
    assert!(decide_body.contains(r#""decided_at":""#), "{decide_body}");
    assert!(
        decide_body.contains(&format!(r#""decided_by_client_id":"{}""#, primary)),
        "{decide_body}"
    );
    assert_eq!(
        service.blocked_approvals().get(&approval_id).unwrap().state,
        mez_agent::permissions::BlockedApprovalState::Approved
    );
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("cargo test"),
        RuleDecision::Allow
    );
    assert!(
        service.permission_policy().rules().iter().any(|rule| {
            rule.scope == CommandRuleScope::Session
                && matches!(rule.rule_match, RuleMatch::ExactSha256 { .. })
                && rule.decision == RuleDecision::Allow
        }),
        "approval/decide scope should control persisted exact command rules"
    );

    let approved_list = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"approved-list","method":"approval/list","params":{"state":"approved"}}"#,
    );
    let (approved_output, _) = service
        .handle_control_input_for_connection(&approved_list, 4096, &mut connection)
        .unwrap();
    let (approved_body, _) = decode_control_frame(&approved_output, 4096).unwrap();
    assert!(
        approved_body.contains(&format!(r#""approval_id":"{}""#, approval_id)),
        "{approved_body}"
    );

    let pending_list = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"pending-list","method":"approval/list","params":{"state":"pending"}}"#,
    );
    let (pending_output, _) = service
        .handle_control_input_for_connection(&pending_list, 4096, &mut connection)
        .unwrap();
    let (pending_body, _) = decode_control_frame(&pending_output, 4096).unwrap();
    assert!(pending_body.contains(r#""approvals":[]"#), "{pending_body}");

    let (repeated_output, _) = service
        .handle_control_input_for_connection(&decide, 4096, &mut connection)
        .unwrap();
    let (repeated_body, _) = decode_control_frame(&repeated_output, 4096).unwrap();
    assert_eq!(repeated_body, decide_body);

    let primary_events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        primary_events.iter().any(|event| {
            event.kind == EventKind::ApprovalChanged
                && event
                    .payload
                    .contains(&format!(r#""approval_id":"{}""#, approval_id))
        }),
        "{primary_events:?}"
    );
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"approval""#), "{audit}");
    assert!(audit.contains(r#""action":"prompt""#), "{audit}");
    assert!(audit.contains(r#""outcome":"prompted""#), "{audit}");
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"applied""#), "{audit}");
    assert!(
        audit.contains(&format!(r#""approval_id":"{approval_id}""#)),
        "{audit}"
    );
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies that project-persistent approval choices create and update the
/// project-local Mezzanine config with exact command rules for the command
/// arguments the user actually reviewed. This keeps the prompt workflow
/// config-driven: allow-forever writes an allow rule, deny writes a deny rule,
/// and future decisions are evaluated from the project overlay rather than a
/// hard-coded command blocklist.
#[test]
fn runtime_control_project_approval_decisions_persist_exact_command_rules() {
    let root = temp_root("runtime-project-approval-rules");
    fs::create_dir_all(root.join(".git")).unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let descriptor = service.initial_pane_descriptor().unwrap();
    service
        .start_pane_process_with_start_directory(descriptor, Some("sleep 30"), Some(&root))
        .unwrap();

    let allow_id = service
        .queue_blocked_approval(BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-%1".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "mez-test-command --flag".to_string(),
            declared_effects: vec!["unknown command effects".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: mez_agent::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();
    let deny_id = service
        .queue_blocked_approval(BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-%1".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "mez-test-command --delete".to_string(),
            declared_effects: vec!["unknown command effects".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: mez_agent::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();

    let allow = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"allow-project","method":"approval/decide","params":{{"approval_id":"{}","decision":"approve","scope":{{"persistence":"project"}},"idempotency_key":"allow-project"}}}}"#,
            allow_id
        ),
        &primary,
    );
    assert!(allow.contains(r#""state":"approved""#), "{allow}");

    let deny = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"deny-project","method":"approval/decide","params":{{"approval_id":"{}","decision":"disapprove","scope":{{"persistence":"project"}},"idempotency_key":"deny-project"}}}}"#,
            deny_id
        ),
        &primary,
    );
    assert!(deny.contains(r#""state":"disapproved""#), "{deny}");

    let project_config = root.join(".mezzanine/config.toml");
    let config_text = fs::read_to_string(&project_config).unwrap();
    assert!(
        config_text.contains(r#"approval_policy = "ask""#),
        "{config_text}"
    );
    assert!(
        config_text.contains(r#"match = "exact_sha256""#),
        "{config_text}"
    );
    assert!(
        config_text.contains(r#"decision = "allow""#),
        "{config_text}"
    );
    assert!(
        config_text.contains(r#"decision = "deny""#),
        "{config_text}"
    );
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --flag"),
        RuleDecision::Allow
    );
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --delete"),
        RuleDecision::Forbid
    );
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --flag extra"),
        RuleDecision::Prompt
    );
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --delete --dry-run"),
        RuleDecision::Prompt
    );
    assert!(
        service.permission_policy().rules().iter().any(|rule| {
            rule.scope == CommandRuleScope::Project
                && matches!(rule.rule_match, RuleMatch::ExactSha256 { .. })
                && rule.decision == RuleDecision::Allow
        }),
        "project approval should load an exact allow rule into the runtime policy"
    );
    assert!(
        service.permission_policy().rules().iter().any(|rule| {
            rule.scope == CommandRuleScope::Project
                && matches!(rule.rule_match, RuleMatch::ExactSha256 { .. })
                && rule.decision == RuleDecision::Forbid
        }),
        "project approval should load an exact deny rule into the runtime policy"
    );

    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime approval disapproval focuses blocked agent pane.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_approval_disapproval_focuses_blocked_agent_pane() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let blocked_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();
    service
        .agent_shell_store_mut()
        .ensure_session(blocked_pane.as_str())
        .unwrap();
    let approval_id = service
        .queue_blocked_approval(BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: format!("agent-{blocked_pane}"),
            pane_id: blocked_pane.to_string(),
            parent_agent_chain: vec![format!("agent-{blocked_pane}")],
            action_kind: "shell_command".to_string(),
            action_summary: "env".to_string(),
            declared_effects: vec!["approval required".to_string()],
            matched_rules: vec!["runtime.agent_action_blocked".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: mez_agent::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"deny","method":"approval/decide","params":{{"approval_id":"{}","decision":"disapprove","idempotency_key":"deny-blocked-agent"}}}}"#,
            approval_id
        ),
        &primary,
    );

    assert!(response.contains(r#""state":"disapproved""#), "{response}");
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .id
            .as_str(),
        blocked_pane.as_str()
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(blocked_pane.as_str())
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
}

/// Verifies that runtime config application fails closed when a layer attempts
/// to enter approval bypass directly. Bypass activation must stay tied to the
/// explicit primary-authorized command path rather than a passive config load
/// or live config reload.
#[test]
fn runtime_rejects_config_enabled_approval_bypass_mode() {
    let mut service = test_runtime_service();
    let error = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[permissions]\nbypass_mode = true\n".to_string(),
        }])
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Config);
    assert!(
        error
            .message()
            .contains("permissions.bypass_mode cannot be enabled from configuration"),
        "{}",
        error.message()
    );
    assert!(!service.permission_policy().approval_bypass());
}

/// Verifies that unknown project-trust method names do not enter the runtime's
/// project-trust dispatcher. Unsupported names must remain ordinary JSON-RPC
/// method-not-found errors rather than reporting a project-trust implementation
/// placeholder, because only the advertised project trust methods are valid.
#[test]
fn runtime_unknown_project_trust_method_uses_generic_method_not_found() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"unknown","method":"project/trust/archive","params":{}}"#,
        &primary,
    );

    assert!(
        response.contains(r#""mezzanine_code":"method_not_found""#),
        "{response}"
    );
    assert!(
        response.contains("unknown control method `project/trust/archive`"),
        "{response}"
    );
    assert!(!response.contains("project trust method"), "{response}");
}

/// Verifies runtime project trust decision applies and removes project overlays.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_project_trust_decision_applies_and_removes_project_overlays() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-project-trust");
    let audit_root = temp_root("runtime-project-trust-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    fs::create_dir_all(root.join(".git")).unwrap();
    let overlay_dir = root.join(".mezzanine");
    fs::create_dir_all(&overlay_dir).unwrap();
    let overlay_path = overlay_dir.join("config.toml");
    fs::write(
        &overlay_path,
        "version = 19\n[history]\nlines = 7\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    let trust_path = root.join("trust.tsv");
    service.set_project_trust_store(ProjectTrustStore::default(), Some(trust_path.clone()));
    let initial_report = service
        .replace_config_layers(vec![
            ConfigLayer {
                name: "primary".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[history]\nlines = 3\n".to_string(),
            },
            ConfigLayer {
                name: "project".to_string(),
                path: Some(overlay_path.clone()),
                format: ConfigFormat::Toml,
                scope: ConfigScope::ProjectOverlay,
                trusted: false,
                text: fs::read_to_string(&overlay_path).unwrap(),
            },
        ])
        .unwrap();
    assert_eq!(initial_report.project_trust_prompts_announced, 1);
    assert_eq!(service.terminal_history_limit(), 3);
    let primary_events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(primary_events.iter().any(|event| {
        event.kind == EventKind::ConfigChanged
            && event.payload.contains(r#""state":"pending""#)
            && event
                .payload
                .contains(r#""blocks_until_primary_decision":true"#)
            && event
                .payload
                .contains(&json_escape(&root.to_string_lossy()))
    }));
    assert_eq!(
        service
            .apply_runtime_config_layers()
            .unwrap()
            .project_trust_prompts_announced,
        0
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let blocked_prompt = service
        .execute_agent_shell_command(&primary, "summarize this project")
        .unwrap();
    assert!(
        blocked_prompt.contains(r#""kind":"display""#)
            && blocked_prompt.contains("agent command error: project trust decision pending")
            && blocked_prompt.contains("(conflict)"),
        "{blocked_prompt}"
    );
    assert!(
        blocked_prompt.contains("project trust decision pending"),
        "{blocked_prompt}"
    );
    assert!(service.agent_turn_ledger().turns().is_empty());

    let trust = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"trust","method":"project/trust/decide","params":{{"project_root":"{}","decision":"trust","idempotency_key":"trust-project"}}}}"#,
            json_escape(&root.to_string_lossy())
        ),
        &primary,
    );

    assert!(trust.contains(r#""state":"trusted""#), "{trust}");
    assert!(trust.contains(r#""trusted_at":""#), "{trust}");
    assert!(
        trust.contains(&format!(r#""decided_by_client_id":"{}""#, primary)),
        "{trust}"
    );
    assert!(!trust.contains(r#""trusted_at":"unix:"#), "{trust}");
    assert!(trust.contains(r#""changed_layers":["project"]"#), "{trust}");
    assert!(
        trust.contains(&json_escape(&overlay_path.to_string_lossy())),
        "{trust}"
    );
    assert!(
        trust.contains(&format!(
            r#""overlay_files":[{{"path":"{}","format":"toml","applied":true,"diagnostics":[]}}]"#,
            json_escape(&overlay_path.to_string_lossy())
        )),
        "{trust}"
    );
    assert!(
        trust.contains(r#""capability_expansion_summary":["permissions"]"#),
        "{trust}"
    );
    assert_eq!(service.terminal_history_limit(), 7);
    assert!(service.config_layers()[1].trusted);
    assert!(trust_path.exists());

    let trusted_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"trusted-list","method":"project/trust/list","params":{"state":"trusted"}}"#,
        &primary,
    );
    assert!(
        trusted_list.contains(&json_escape(&root.to_string_lossy())),
        "{trusted_list}"
    );

    let pending_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"pending-list","method":"project/trust/list","params":{"state":"pending"}}"#,
        &primary,
    );
    assert!(
        !pending_list.contains(&json_escape(&root.to_string_lossy())),
        "{pending_list}"
    );

    let invalid_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"invalid-list","method":"project/trust/list","params":{"state":"unknown"}}"#,
        &primary,
    );
    assert!(
        invalid_list.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_list}"
    );

    let revoke = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"revoke","method":"project/trust/revoke","params":{{"project_root":"{}","idempotency_key":"revoke-project"}}}}"#,
            json_escape(&root.to_string_lossy())
        ),
        &primary,
    );

    assert!(revoke.contains(r#""state":"revoked""#), "{revoke}");
    assert!(
        revoke.contains(&format!(r#""decided_by_client_id":"{}""#, primary)),
        "{revoke}"
    );

    let revoked_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"revoked-list","method":"project/trust/list","params":{"state":"revoked"}}"#,
        &primary,
    );
    assert!(
        revoked_list.contains(&json_escape(&root.to_string_lossy())),
        "{revoked_list}"
    );

    assert_eq!(service.terminal_history_limit(), 3);
    assert!(!service.config_layers()[1].trusted);

    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"configuration""#), "{audit}");
    assert!(audit.contains(r#""scope":"project_trust""#), "{audit}");
    assert!(audit.contains(r#""decision":"trusted""#), "{audit}");
    assert!(audit.contains(r#""decision":"revoked""#), "{audit}");
    assert!(audit.contains(r#""project_root""#), "{audit}");
    let _ = fs::remove_dir_all(audit_root);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime agent trust command logs and persists project trust request.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_agent_trust_command_logs_and_persists_project_trust_request() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-agent-trust-command");
    let config_root = root.join("config-root");
    let trust_path = config_root.join("project-trust.tsv");
    service.set_config_root(config_root.clone());
    fs::create_dir_all(root.join(".git")).unwrap();
    let overlay_dir = root.join(".mezzanine");
    fs::create_dir_all(&overlay_dir).unwrap();
    let overlay_path = overlay_dir.join("config.toml");
    fs::write(
        &overlay_path,
        "version = 19\n[history]\nlines = 11\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    service.set_project_trust_store(ProjectTrustStore::default(), None);
    let initial_report = service
        .replace_config_layers(vec![
            ConfigLayer {
                name: "primary".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[history]\nlines = 3\n".to_string(),
            },
            ConfigLayer {
                name: "project".to_string(),
                path: Some(overlay_path.clone()),
                format: ConfigFormat::Toml,
                scope: ConfigScope::ProjectOverlay,
                trusted: false,
                text: fs::read_to_string(&overlay_path).unwrap(),
            },
        ])
        .unwrap();
    assert_eq!(initial_report.project_trust_prompts_announced, 1);
    let primary_events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        primary_events.iter().any(|event| {
            event.kind == EventKind::ConfigChanged
                && event.payload.contains(r#""trust_command":"/trust "#)
                && event
                    .payload
                    .contains(&json_escape(&root.to_string_lossy()))
        }),
        "{primary_events:?}"
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let blocked_prompt = service
        .execute_agent_shell_command(&primary, "summarize this project")
        .unwrap();
    assert!(
        blocked_prompt.contains(r#""kind":"display""#)
            && blocked_prompt.contains("agent command error: project trust decision pending")
            && blocked_prompt.contains("(conflict)"),
        "{blocked_prompt}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("project trust pending:"), "{pane_text}");
    let collapsed_agent_wraps = pane_text.replace("\n▐ ", "");
    assert!(collapsed_agent_wraps.contains("/trust"), "{pane_text}");

    let trust = service
        .execute_agent_shell_command(&primary, "/trust")
        .unwrap();

    assert!(trust.contains(r#""kind":"mutated""#), "{trust}");
    assert!(trust.contains(r#""command":"trust""#), "{trust}");
    assert!(trust.contains("project trust granted"), "{trust}");
    assert!(trust.contains("persisted=true"), "{trust}");
    assert_eq!(service.terminal_history_limit(), 11);
    assert!(service.config_layers()[1].trusted);
    assert!(trust_path.exists());
    let persisted = ProjectTrustStore::load_from_file(&trust_path).unwrap();
    assert_eq!(persisted.get(&root).unwrap().state, TrustDecision::Trusted);
    let _ = fs::remove_dir_all(root);
}

/// Verifies that clickable pane-frame agent status pills cover live toggles
/// beyond model selection. Automatic reasoning should apply immediately like a
/// button, while approval policy should open the same selector flow used by
/// model and reasoning choices.
#[test]
fn runtime_pane_agent_status_selector_toggles_auto_and_selects_approval() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_default_routing(false);

    let open_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Routing,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(open_report.view_refresh_required);
    assert!(!open_report.full_redraw_required);
    assert!(service.pane_agent_status_selector().is_none());
    assert_eq!(service.agent_routing_override("%1"), Some(true));

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::ApprovalPolicy,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let full_access_index = service
        .pane_agent_status_selector()
        .and_then(|selector| {
            assert_eq!(selector.field, PaneAgentStatusField::ApprovalPolicy);
            selector.items.iter().position(|item| item == "full-access")
        })
        .expect("approval selector should include full-access");
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::ApprovalPolicy,
                        item_index: full_access_index,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(service.pane_agent_status_selector().is_none());
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(ClientViewRole::Primary, Size::new(80, 24).unwrap(), &config)
        .unwrap()
        .unwrap();
    let rendered = view.lines.join("\n");
    assert!(rendered.contains("full-access"), "{rendered}");
    assert!(rendered.contains("route"), "{rendered}");
    assert!(rendered.contains("gpt"), "{rendered}");
}

/// Verifies legacy agent-session metadata cannot narrow a configured elevated
/// approval default during restore.
///
/// Older checkpoints recorded the effective policy even when it only came from
/// the default configuration. Restoring those rows must not silently preempt a
/// user's newer `permissions.approval_policy = "full-access"` configuration.
#[test]
fn runtime_agent_session_restore_does_not_narrow_configured_approval_default() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-approval-default"));
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[permissions]\napproval_policy = \"full-access\"\n".to_string(),
        }])
        .unwrap();
    let mezzanine_session_id = service.session().id.as_str().to_string();
    transcript_store
        .save_agent_session_metadata(
            &mezzanine_session_id,
            &[mez_agent::transcript::AgentSessionMetadata {
                mezzanine_session_id: mezzanine_session_id.clone(),
                pane_id: "%1".to_string(),
                conversation_id: "legacy-ask".to_string(),
                prompt_cache_lineage_id: "lineage-legacy-ask".to_string(),
                visibility: "visible".to_string(),
                running_turn_id: None,
                running_turn_kind: None,
                transcript_entries: 1,
                log_level: "normal".to_string(),
                pane_model_profile: None,
                planning_enabled: false,
                response_style: None,
                directive: None,
                routing_enabled: None,
                approval_policy: Some("ask".to_string()),
                working_directory: None,
                project_root: None,
                context_usage: None,
                context_usage_snapshot: None,
                token_usage: Default::default(),
                token_usage_by_model: Default::default(),
            }],
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());

    let restored = service
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    assert_eq!(restored, 1);
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    let rewritten = transcript_store
        .load_agent_session_metadata(&mezzanine_session_id)
        .unwrap();
    assert_eq!(rewritten.len(), 1);
    assert_eq!(rewritten[0].approval_policy, None);
}
