//! Runtime tests for actions config behavior.

use super::*;

/// Verifies runtime-owned config changes render with the same stylized
/// normal-mode action line as other non-shell actions.
///
/// Config mutations do not go through the pane shell, but users still need a
/// compact action row that makes the operation and setting path visible without
/// dumping result payloads into the pane.
#[test]
fn runtime_config_change_action_logs_styled_action_line_in_normal_mode() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "config-1".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::ConfigChange {
            setting_path: "theme.active".to_string(),
            operation: "set".to_string(),
            value: Some("kanagawa".to_string()),
        },
    };

    let emitted = service
        .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
        .unwrap();
    assert!(emitted);

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let pane_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(pane_text.contains("agent: config change: set theme.active"));
    assert!(!pane_text.contains("kanagawa"));
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: config change:"))
        .unwrap();
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let prefix_column = display_column_for_fragment(&action_line.text, "agent:");
    let action_column = display_column_for_fragment(&action_line.text, "config change");
    let argument_column = display_column_for_fragment(&action_line.text, "theme.active");
    let prefix_rendition = styled_line_rendition_at(action_line, prefix_column);
    let action_rendition = styled_line_rendition_at(action_line, action_column);
    let argument_rendition = styled_line_rendition_at(action_line, argument_column);
    assert_eq!(
        prefix_rendition.foreground,
        Some(theme.colors.agent_transcript_status.foreground)
    );
    assert!(prefix_rendition.dim);
    assert_eq!(
        action_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground)
    );
    assert!(action_rendition.bold);
    assert_ne!(
        argument_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground),
        "{action_line:?}"
    );
}

/// Verifies approved non-theme agent `config_change` actions persist through
/// the same user config mutation path that terminal control requests use.
///
/// The action is model-authored, but once `/approve` has accepted it the
/// resulting config file and live runtime setting should agree without a second
/// model-visible live-only override.
#[test]
fn runtime_config_change_persists_generic_setting_and_applies_live() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let config_root = temp_root("runtime-agent-config-change-generic");
    service.set_config_root(config_root.clone());
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "turn-config-generic".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 200,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        initial_capability: None,
        state: AgentTurnState::Running,
    };
    let action = mez_agent::AgentAction {
        id: "config-generic".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::ConfigChange {
            setting_path: "history.lines".to_string(),
            operation: "set".to_string(),
            value: Some("7".to_string()),
        },
    };

    let result = service
        .execute_config_change_action_for_turn(&turn, &action, &primary, "approved")
        .unwrap();

    assert_eq!(result.status, ActionStatus::Succeeded);
    assert_eq!(service.terminal_history_limit(), 7);
    let config_text = fs::read_to_string(config_root.join("config.toml")).unwrap();
    assert!(config_text.contains("lines = 7"), "{config_text}");
    assert!(
        result
            .structured_content_json
            .as_deref()
            .unwrap_or_default()
            .contains("persistent_control_response"),
        "{result:?}"
    );
    let _ = fs::remove_dir_all(config_root);
}

/// Verifies agent `config_change` reset removes the explicit override.
///
/// Reset is model-facing language for returning a field to its lower-precedence
/// or default value. Runtime execution should therefore share the `config/unset`
/// path while exposing the clearer operation name in MAAP.
#[test]
fn runtime_config_change_reset_removes_override_and_restores_default() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let config_root = temp_root("runtime-agent-config-change-reset");
    service.set_config_root(config_root.clone());
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "turn-config-reset".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 200,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        initial_capability: None,
        state: AgentTurnState::Running,
    };
    let set_action = mez_agent::AgentAction {
        id: "config-reset-set".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::ConfigChange {
            setting_path: "history.lines".to_string(),
            operation: "set".to_string(),
            value: Some("7".to_string()),
        },
    };
    let reset_action = mez_agent::AgentAction {
        id: "config-reset".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::ConfigChange {
            setting_path: "history.lines".to_string(),
            operation: "reset".to_string(),
            value: None,
        },
    };

    service
        .execute_config_change_action_for_turn(&turn, &set_action, &primary, "approved")
        .unwrap();
    assert_eq!(service.terminal_history_limit(), 7);
    let result = service
        .execute_config_change_action_for_turn(&turn, &reset_action, &primary, "approved")
        .unwrap();

    assert_eq!(result.status, ActionStatus::Succeeded);
    assert_eq!(service.terminal_history_limit(), 10_000);
    let config_text = fs::read_to_string(config_root.join("config.toml")).unwrap();
    assert!(!config_text.contains("lines = 7"), "{config_text}");
    let _ = fs::remove_dir_all(config_root);
}

/// Verifies config-change control idempotency keys are unique for distinct
/// payloads even if recovery or compatibility paths reuse an action id.
///
/// The JSON-RPC control layer treats idempotency keys as request identities, so
/// a batch of independent model-authored config changes must not collide merely
/// because the local action id is repeated.
#[test]
fn runtime_config_change_idempotency_uses_setting_payload() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let config_root = temp_root("runtime-agent-config-change-idempotency");
    service.set_config_root(config_root.clone());
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "turn-config-idempotency".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 200,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        initial_capability: None,
        state: AgentTurnState::Running,
    };
    let first = mez_agent::AgentAction {
        id: "config-reused".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::ConfigChange {
            setting_path: "history.lines".to_string(),
            operation: "set".to_string(),
            value: Some("7".to_string()),
        },
    };
    let second = mez_agent::AgentAction {
        id: "config-reused".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::ConfigChange {
            setting_path: "history.rotate_lines".to_string(),
            operation: "set".to_string(),
            value: Some("3".to_string()),
        },
    };

    let first_result = service
        .execute_config_change_action_for_turn(&turn, &first, &primary, "approved")
        .unwrap();
    let second_result = service
        .execute_config_change_action_for_turn(&turn, &second, &primary, "approved")
        .unwrap();

    assert_eq!(first_result.status, ActionStatus::Succeeded);
    assert_eq!(second_result.status, ActionStatus::Succeeded);
    assert_eq!(service.terminal_history_limit(), 7);
    assert_eq!(service.terminal_history_rotate_lines(), 3);
    let config_text = fs::read_to_string(config_root.join("config.toml")).unwrap();
    assert!(config_text.contains("lines = 7"), "{config_text}");
    assert!(config_text.contains("rotate_lines = 3"), "{config_text}");
    let _ = fs::remove_dir_all(config_root);
}

/// Verifies broad theme color changes from an agent turn are applied in one
/// runtime config batch.
///
/// The built-in `$mez-reference` skill can legitimately emit aliases plus
/// every `theme.colors.*` slot when the user asks for a complete palette.
/// Applying those changes as independent config-control requests reloads and
/// redraws the runtime dozens of times in one turn; batching preserves the
/// same final config while keeping live mutation to one validated reload.
#[test]
fn runtime_agent_config_change_batches_broad_theme_palette() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    let config_root = temp_root("runtime-agent-config-change-theme-batch");
    service.set_config_root(config_root.clone());
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-config-theme-batch","input":"$mez-reference make my terminal look like a mcdonalds. Don't leave any colors unset"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let before_config_events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .into_iter()
        .filter(|event| event.kind == EventKind::ConfigChanged)
        .count();
    let mut actions = Vec::new();
    for (name, value) in [
        ("primary", "#ffc72c"),
        ("secondary", "#da291c"),
        ("surface", "#fff8e1"),
        ("foreground", "#241400"),
        ("muted", "#6b5a32"),
        ("tertiary", "#009a44"),
        ("danger", "#b00020"),
        ("thinking", "#7a5c00"),
    ] {
        actions.push(mez_agent::AgentAction {
            id: format!("alias-{name}"),
            rationale: String::new(),
            payload: mez_agent::AgentActionPayload::ConfigChange {
                setting_path: format!("theme.aliases.{name}"),
                operation: "set".to_string(),
                value: Some(value.to_string()),
            },
        });
    }
    for slot in UI_COLOR_SLOT_NAMES {
        let value = if slot.ends_with("_bg") {
            if slot.contains("error") || slot.contains("danger") {
                "surface"
            } else {
                "primary"
            }
        } else if slot.contains("error") || slot.contains("danger") {
            "danger"
        } else if slot.contains("comment") || slot.contains("muted") {
            "muted"
        } else if slot.contains("string") || slot.contains("function") {
            "secondary"
        } else {
            "foreground"
        };
        actions.push(mez_agent::AgentAction {
            id: format!("color-{slot}"),
            rationale: String::new(),
            payload: mez_agent::AgentActionPayload::ConfigChange {
                setting_path: format!("theme.colors.{slot}"),
                operation: "set".to_string(),
                value: Some(value.to_string()),
            },
        });
    }
    let action_count = actions.len();
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap config response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "set every terminal theme color".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions,
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results.len(), action_count);
    assert!(
        execution
            .action_results
            .iter()
            .all(|result| result.status == ActionStatus::Succeeded),
        "{:?}",
        execution.action_results
    );
    assert_eq!(
        service.ui_theme.colors.prompt.background,
        TerminalColor::Rgb(0xff, 0xc7, 0x2c)
    );
    assert_eq!(
        service.ui_theme.colors.agent_transcript_error.foreground,
        TerminalColor::Rgb(0xb0, 0x00, 0x20)
    );
    let after_config_events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .into_iter()
        .filter(|event| event.kind == EventKind::ConfigChanged)
        .count();
    assert_eq!(after_config_events - before_config_events, 1);
    let config_text = fs::read_to_string(config_root.join("config.toml")).unwrap();
    assert!(
        config_text.contains(r##"primary = "#ffc72c""##),
        "{config_text}"
    );
    assert!(
        config_text.contains(r#"prompt_bg = "primary""#),
        "{config_text}"
    );
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap_or_default()
            .contains(r#""persistent_batch""#),
        "{:?}",
        execution.action_results[0]
    );
    service.pane_processes_mut().terminate_all().unwrap();
    let _ = fs::remove_dir_all(config_root);
}

/// Verifies pending config-change approvals are reconciled when the approval
/// policy changes to full access.
///
/// Configuration changes use the same approval mechanism as other privileged
/// model actions. A policy update that would satisfy the pending action should
/// resume it through the runtime config-control path without requiring a second
/// explicit `/approve`.
#[test]
fn runtime_config_change_resumes_after_full_access_change() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let config_root = temp_root("runtime-agent-config-change-persist");
    service.set_config_root(config_root.clone());
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-config-approval","input":"change my mez theme to catppuccin_latte"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap config response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "change the requested live configuration".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "config-1".to_string(),
                    rationale: String::new(),
                    payload: mez_agent::AgentActionPayload::ConfigChange {
                        setting_path: "theme.active".to_string(),
                        operation: "set".to_string(),
                        value: Some("catppuccin_latte".to_string()),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(service.blocked_approvals().pending().len(), 1);
    let approval_change = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-approval","method":"agent/shell/command","params":{"idempotency_key":"agent-approval-full-access","input":"/approval full-access"}}"#,
        &primary,
    );
    assert!(
        approval_change.contains("requested=full-access"),
        "{approval_change}"
    );
    assert_eq!(
        service.permission_policy().approval_policy,
        mez_agent::ApprovalPolicy::FullAccess
    );
    assert_eq!(service.blocked_approvals().pending().len(), 0);
    assert_eq!(service.ui_theme.name, "catppuccin_latte");
    assert_eq!(
        service.permission_policy().approval_policy,
        mez_agent::ApprovalPolicy::FullAccess
    );
    let config_text = fs::read_to_string(config_root.join("config.toml")).unwrap();
    assert!(
        config_text.contains(r#"active = "catppuccin_latte""#),
        "{config_text}"
    );
    assert!(config_text.contains("[theme.colors]"), "{config_text}");
    service.pane_processes_mut().terminate_all().unwrap();
    let _ = fs::remove_dir_all(config_root);
}
