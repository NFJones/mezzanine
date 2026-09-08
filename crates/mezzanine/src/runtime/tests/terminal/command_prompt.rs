//! Runtime tests for terminal command prompt behavior.

use super::*;

/// Verifies that agent-scoped operations with slash-command equivalents are no
/// longer accepted through the live terminal command prompt. These workflows
/// belong in pane-local agent slash commands, while the terminal command
/// language remains focused on multiplexer/session control.
#[test]
fn runtime_terminal_command_rejects_agent_scoped_slash_duplicates() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let removed = [
        "auth-logout",
        "mcp-list",
        "list-project-trust",
        "trust-project /tmp/project",
        "reject-project /tmp/project",
        "revoke-project-trust /tmp/project",
        "permissions",
        "approval",
        "list-command-rules",
        "allow-command cargo test",
        "deny-command rm",
        "prompt-command git commit",
        "remove-command-rule rule1",
        "bypass-approvals status",
    ];

    for input in removed {
        let error = service
            .execute_terminal_command(&primary, input)
            .unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
        assert!(
            error.message().contains("unknown command"),
            "{input}: {error}"
        );
    }
}

/// Verifies that a repeated pane-content click copies the surrounding
/// readline-style word to the mouse paste buffer and host clipboard. This
/// protects double-click selection from using a separate whitespace-only token
/// model or leaving copy mode active after the word is copied.
#[test]
fn runtime_double_click_copies_readline_word_under_pointer() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    *service.host_clipboard_mut_for_tests() =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"alpha beta --flag");
    service.set_pane_screen("%1".to_string(), screen);

    for _ in 0..2 {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::HandleMouse(
                        MouseAction::FocusPane(CopyPosition { line: 0, column: 7 }),
                    )],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }

    assert_eq!(service.paste_buffers().get("mouse"), Some("beta"));
    assert_eq!(
        TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().as_slice(),
        ["beta"]
    );
    assert!(
        service
            .active_copy_mode_for_presented_surface("%1")
            .is_none()
    );
}

/// Verifies clicks on different retained pane surfaces cannot combine into a
/// double-click word selection even when pane id and cell coordinates match.
#[test]
fn runtime_double_click_state_is_scoped_to_presented_surface() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    *service.host_clipboard_mut_for_tests() =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let size = Size::new(20, 4).unwrap();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut agent_screen = TerminalScreen::new(size, 10).unwrap();
    agent_screen.feed(b"gamma delta --flag");
    service.set_agent_pane_screen("%1", &conversation_id, agent_screen);
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    let mut process_screen = TerminalScreen::new(size, 10).unwrap();
    process_screen.feed(b"alpha beta --flag");
    service.set_process_pane_screen("%1", process_screen);
    let click = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::FocusPane(CopyPosition { line: 0, column: 7 }),
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    service
        .apply_attached_terminal_step_plan(&primary, &click)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .apply_attached_terminal_step_plan(&primary, &click)
        .unwrap();
    assert!(service.paste_buffers().get("mouse").is_none());

    service
        .apply_attached_terminal_step_plan(&primary, &click)
        .unwrap();
    assert_eq!(service.paste_buffers().get("mouse"), Some("delta"));
    assert_eq!(
        TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().as_slice(),
        ["delta"]
    );
}

/// Verifies that runtime `terminal/command` accepts only the spec-defined
/// `input` field. The legacy `command` alias is rejected at the params schema
/// boundary so clients cannot depend on a non-normative request shape.
#[test]
fn runtime_terminal_command_rejects_legacy_command_alias() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let alias_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"terminal-command-alias","method":"terminal/command","params":{"idempotency_key":"terminal-command-alias","command":"list-windows"}}"#,
        &primary,
    );

    assert!(
        alias_response.contains(r#""mezzanine_code":"invalid_params""#),
        "{alias_response}"
    );
    assert!(
        alias_response.contains("terminal/command params contains unknown field `command`"),
        "{alias_response}"
    );
}

/// Verifies that an unknown command submitted through the supported
/// `terminal/command` JSON-RPC method is reported as invalid command input, not
/// as JSON-RPC method-not-found. The transport method is implemented; only the
/// command language token is unknown.
#[test]
fn runtime_terminal_command_unknown_input_is_invalid_params() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"terminal-command-unknown","method":"terminal/command","params":{"idempotency_key":"terminal-command-unknown","input":"does-not-exist"}}"#,
        &primary,
    );

    assert!(
        response.contains(r#""mezzanine_code":"invalid_params""#),
        "{response}"
    );
    assert!(
        response.contains("unknown command `does-not-exist`"),
        "{response}"
    );
    assert!(
        !response.contains(r#""mezzanine_code":"method_not_found""#),
        "{response}"
    );
}

/// Verifies zen commands mutate the ordinary session live override in sequence
/// and return structured mutation outcomes without display payloads. Reading
/// the effective value for every toggle keeps semicolon execution causal.
#[test]
fn runtime_zen_command_mutates_live_override_sequentially_and_silently() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let output = service
        .execute_terminal_command(&primary, "zen on; zen toggle; zen toggle; zen off")
        .unwrap();
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();

    assert_eq!(output["executed"], 4);
    assert_eq!(output["outcomes"].as_array().unwrap().len(), 4);
    assert!(
        output["outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|outcome| {
                outcome["command"] == "zen"
                    && outcome["kind"] == "mutated"
                    && outcome.get("body").is_none()
            })
    );
    assert!(!service.terminal_zen_mode());
    assert!(service.primary_display_overlay().is_none());
    assert!(service.primary_error_status_overlay().is_none());
}

/// Verifies explicit zen modes are idempotent against the effective setting,
/// including the default-off state before a live override layer exists. A
/// no-op remains structured but must not advance configuration generation.
#[test]
fn runtime_zen_command_reports_effective_noops_without_config_mutation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let config_event_count = |service: &RuntimeSessionService| {
        service
            .event_log()
            .unwrap()
            .replay_for(&EventAudience::AllPrimaries)
            .into_iter()
            .filter(|event| event.kind == EventKind::ConfigChanged)
            .count()
    };
    let initial_config_events = config_event_count(&service);

    let off = service
        .execute_terminal_command(&primary, "zen off")
        .unwrap();
    let off: serde_json::Value = serde_json::from_str(&off).unwrap();
    assert_eq!(off["outcomes"][0]["kind"], "noop");
    assert_eq!(config_event_count(&service), initial_config_events);

    let on = service
        .execute_terminal_command(&primary, "zen on; zen on")
        .unwrap();
    let on: serde_json::Value = serde_json::from_str(&on).unwrap();
    assert_eq!(on["outcomes"][0]["kind"], "mutated");
    assert_eq!(on["outcomes"][1]["kind"], "noop");
    assert_eq!(config_event_count(&service), initial_config_events + 1);
    assert!(service.terminal_zen_mode());
}

/// Verifies zen validates its raw command arguments exactly, including flags
/// that positional parsing would otherwise discard, and leaves effective state
/// unchanged after every rejected form.
#[test]
fn runtime_zen_command_rejects_noncanonical_raw_arguments_without_mutation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    for input in [
        "zen",
        "zen maybe",
        "zen ON",
        "zen --toggle",
        "zen -t on",
        "zen on extra",
    ] {
        let error = service
            .execute_terminal_command(&primary, input)
            .unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
        assert_eq!(error.message(), "usage: zen on|off|toggle", "{input}");
        assert!(!service.terminal_zen_mode(), "{input}");
    }
}

/// Verifies the terminal-command authorization boundary rejects observer
/// callers before zen can alter the shared session presentation setting.
#[test]
fn runtime_zen_command_requires_attached_primary_authority() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 121)
        .unwrap();

    let error = service
        .execute_terminal_command(&observer, "zen on")
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(!service.terminal_zen_mode());
}

/// Verifies `pane-settings` opens a keyboard-owned selector for the stable
/// requested pane without changing focus, includes both read-only and typed
/// action entries, and rejects observer callers at the shared command boundary.
#[test]
fn runtime_pane_settings_targets_stable_pane_without_retargeting_focus() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );
    service.session.select_pane(&primary, "%1").unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%2")
        .unwrap();

    let outcome = service
        .execute_terminal_command(&primary, "pane-settings -t %2")
        .unwrap();

    assert!(
        outcome.contains(r#""command":"pane-settings""#),
        "{outcome}"
    );
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .id
            .as_str(),
        "%1"
    );
    let selector = service
        .pane_agent_status_selector()
        .expect("pane-settings should open a keyboard selector");
    assert_eq!(selector.pane_id, "%2");
    assert_eq!(selector.field, PaneAgentStatusField::Settings);
    assert_eq!(selector.items.len(), selector.settings_entries.len());
    assert!(
        selector.settings_entries.iter().any(|entry| {
            entry.identity.action == crate::host::terminal::PaneStatusAction::None
        })
    );
    assert!(selector.settings_entries.iter().any(|entry| {
        matches!(
            entry.identity.action,
            crate::host::terminal::PaneStatusAction::Builtin(_)
        )
    }));

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.pane]\nright_status = \"\"\n".to_string(),
        }])
        .unwrap();
    assert!(
        service.pane_agent_status_selector().is_none(),
        "configuration replacement must invalidate stale occurrence generations"
    );

    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 121)
        .unwrap();
    let error = service
        .execute_terminal_command(&observer, "pane-settings -t %2")
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies blocked pane-provider inspection remains available in zen mode,
/// exposes only sanitized state, and does not reconcile or schedule work.
#[test]
fn runtime_pane_settings_provider_inspection_is_zen_safe_and_side_effect_free() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "blocked-provider-inspection".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.pane]\nright_status = \"#{pill.branch}\"\n[frames.pane.pills.branch]\ncommand = \"printf TOP_SECRET_PROVIDER_OUTPUT\"\ncwd = \"pane\"\nwhen = []\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.reconcile_pane_status_providers();
    assert!(service.prepare_pane_status_provider_refreshes(1).is_empty());
    service
        .execute_terminal_command(&primary, "zen on")
        .unwrap();

    let output = service
        .execute_terminal_command(&primary, "pane-settings --providers -t %1")
        .unwrap();
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();
    let body = output["outcomes"][0]["body"].as_str().unwrap();

    assert!(body.contains("provider=branch"), "{body}");
    assert!(body.contains("state=blocked"), "{body}");
    assert!(body.contains("reason="), "{body}");
    assert!(!body.contains("TOP_SECRET_PROVIDER_OUTPUT"), "{body}");
    assert!(service.terminal_zen_mode());
    assert!(service.prepare_pane_status_provider_refreshes(1).is_empty());
}

/// Verifies `show-pane-status` resolves an optional stable pane target, remains
/// usable in zen mode, reports preset provenance and shared layout decisions,
/// and does not activate provider work while inspecting retained state.
#[test]
fn runtime_show_pane_status_is_zen_safe_targeted_and_side_effect_free() {
    let mut service = test_runtime_service_with_size(Size::new(24, 6).unwrap());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "project-pane-status".to_string(),
            path: Some(PathBuf::from("/TOP/SECRET/project.toml")),
            format: ConfigFormat::Toml,
            scope: ConfigScope::ProjectOverlay,
            trusted: true,
            text: "version = 92\n[frames.pane]\nstatus_preset = \"minimal\"\nright_status = \"#{pill.model} #{pill.branch}\"\n[frames.pane.pills.model]\nfield = \"agent.model\"\nlabel = \"Selected model\"\n[frames.pane.pills.branch]\ncommand = \"printf TOP_SECRET_OUTPUT\"\ncwd = \"pane\"\nwhen = []\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 6).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_terminal_command(&primary, "zen on")
        .unwrap();

    let before = service.pending_pane_status_provider_refresh_count_for_tests();
    let output = service
        .execute_terminal_command(&primary, "show-pane-status -t %1")
        .unwrap();
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();
    let body = output["outcomes"][0]["body"].as_str().unwrap();

    assert!(body.contains("pane=%1"), "{body}");
    assert!(body.contains("preset=minimal"), "{body}");
    assert!(body.contains("preset_source=project-pane-status"), "{body}");
    assert!(
        body.contains("right_status_source=project-pane-status"),
        "{body}"
    );
    assert!(body.contains("source=pill.model"), "{body}");
    assert!(body.contains("owner=%1"), "{body}");
    assert!(body.contains("occurrence=right:0"), "{body}");
    assert!(body.contains("layout="), "{body}");
    assert!(body.contains("budget_cells="), "{body}");
    assert!(body.contains("provider=branch"), "{body}");
    assert!(!body.contains("TOP_SECRET"), "{body}");
    assert!(!body.contains("project.toml"), "{body}");
    assert_eq!(
        service.pending_pane_status_provider_refresh_count_for_tests(),
        before,
        "diagnostic inspection must not reconcile, admit, or schedule providers"
    );
    assert!(service.terminal_zen_mode());

    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 121)
        .unwrap();
    let forbidden = service
        .execute_terminal_command(&observer, "show-pane-status -t %1")
        .unwrap_err();
    assert_eq!(forbidden.kind(), crate::error::MezErrorKind::Forbidden);

    let stale = service
        .execute_terminal_command(&primary, "show-pane-status -t %999")
        .unwrap_err();
    assert_eq!(stale.kind(), crate::error::MezErrorKind::NotFound);
}

/// Verifies explicit retry is primary-authorized, clears only a matching
/// current block, and then encounters the unchanged admission decision again.
#[test]
fn runtime_pane_settings_retry_provider_preserves_authority_and_rejects_stale_state() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "blocked-provider-retry".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.pane]\nright_status = \"#{pill.branch}\"\n[frames.pane.pills.branch]\ncommand = \"printf ready\"\ncwd = \"pane\"\nwhen = []\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.reconcile_pane_status_providers();
    assert!(service.prepare_pane_status_provider_refreshes(1).is_empty());
    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 121)
        .unwrap();

    let forbidden = service
        .execute_terminal_command(&observer, "pane-settings --retry-provider branch -t %1")
        .unwrap_err();
    assert_eq!(forbidden.kind(), crate::error::MezErrorKind::Forbidden);

    let output = service
        .execute_terminal_command(&primary, "pane-settings --retry-provider branch -t %1")
        .unwrap();
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["outcomes"][0]["kind"], "mutated");

    let duplicate = service
        .execute_terminal_command(&primary, "pane-settings --retry-provider branch -t %1")
        .unwrap_err();
    assert_eq!(duplicate.kind(), crate::error::MezErrorKind::Conflict);
    assert!(service.prepare_pane_status_provider_refreshes(1).is_empty());
    let blocked_again = service
        .execute_terminal_command(&primary, "pane-settings --providers -t %1")
        .unwrap();
    assert!(blocked_again.contains("state=blocked"), "{blocked_again}");

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-removed".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.pane]\nright_status = \"\"\n".to_string(),
        }])
        .unwrap();
    let stale = service
        .execute_terminal_command(&primary, "pane-settings --retry-provider branch -t %1")
        .unwrap_err();
    assert_eq!(stale.kind(), crate::error::MezErrorKind::Conflict);
}

/// Verifies keyboard selection from `pane-settings` executes the same typed
/// pane-owned action as a mouse hit without moving focus to the target pane.
/// The stored occurrence identity must survive only while its configuration
/// and pane context still match the rendered status item.
#[test]
fn runtime_pane_settings_keyboard_action_revalidates_stable_occurrence() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );
    service.session.select_pane(&primary, "%1").unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%2")
        .unwrap();
    service.set_agent_default_routing(false);
    service
        .execute_terminal_command(&primary, "pane-settings -t %2")
        .unwrap();
    let routing_index = service
        .pane_agent_status_selector()
        .and_then(|selector| {
            selector.settings_entries.iter().position(|entry| {
                entry.identity.action
                    == crate::host::terminal::PaneStatusAction::Builtin(
                        PaneAgentStatusField::Routing,
                    )
            })
        })
        .expect("pane-settings should contain the routing control");
    service
        .pane_agent_status_selector_mut_for_tests()
        .expect("pane-settings selector should remain open")
        .active_index = routing_index;

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(report.view_refresh_required);
    assert_eq!(service.agent_routing_override("%2"), Some(true));
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .id
            .as_str(),
        "%1"
    );

    service
        .execute_terminal_command(&primary, "pane-settings -t %2")
        .unwrap();
    let stale_index = service
        .pane_agent_status_selector()
        .and_then(|selector| {
            selector.settings_entries.iter().position(|entry| {
                entry.identity.action
                    == crate::host::terminal::PaneStatusAction::Builtin(
                        PaneAgentStatusField::Routing,
                    )
            })
        })
        .expect("reopened settings should contain routing");
    service
        .pane_agent_status_selector_mut_for_tests()
        .expect("reopened pane-settings selector should remain open")
        .active_index = stale_index;
    service.set_agent_routing_override("%2", Some(false));
    let error = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
    assert_eq!(service.agent_routing_override("%2"), Some(false));
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .id
            .as_str(),
        "%1"
    );
}

/// Verifies configured terminal and agent actions execute against the stable
/// pill owner rather than the pane that has focus when the action is applied.
#[test]
fn runtime_pane_settings_custom_actions_target_stable_owner_without_focus_change() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.pane]\nright_status = \"#{pill.rename} #{pill.plan}\"\n[frames.pane.pills.rename]\nfield = \"policy.mode\"\non_click = \"terminal:rename-pane -t {pane} provider-owner\"\n[frames.pane.pills.plan]\nfield = \"policy.mode\"\non_click = \"agent:/plan on\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );
    service.session.select_pane(&primary, "%1").unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%2")
        .unwrap();

    service
        .execute_terminal_command(&primary, "pane-settings -t %2")
        .unwrap();
    let terminal_index = service
        .pane_agent_status_selector()
        .and_then(|selector| {
            selector.settings_entries.iter().position(|entry| {
                matches!(
                    &entry.identity.action,
                    crate::host::terminal::PaneStatusAction::Terminal { actions, origin }
                        if actions == &[crate::host::terminal::PaneStatusTerminalAction {
                            command: crate::host::terminal::PaneStatusTerminalCommand::RenamePane,
                            arguments: vec!["provider-owner".to_string()],
                        }] && origin.as_ref().is_some_and(|origin| origin.trusted)
                )
            })
        })
        .expect("pane-settings should contain the terminal action");
    service
        .pane_agent_status_selector_mut_for_tests()
        .expect("pane-settings selector should remain open")
        .active_index = terminal_index;
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    let window = service.session().active_window().unwrap();
    assert_eq!(
        window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == "%2")
            .map(|pane| pane.title.as_str()),
        Some("provider-owner")
    );
    assert_ne!(
        window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == "%1")
            .map(|pane| pane.title.as_str()),
        Some("provider-owner")
    );
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .id
            .as_str(),
        "%1"
    );

    service
        .execute_terminal_command(&primary, "pane-settings -t %2")
        .unwrap();
    let agent_index = service
        .pane_agent_status_selector()
        .and_then(|selector| {
            selector.settings_entries.iter().position(|entry| {
                matches!(
                    &entry.identity.action,
                    crate::host::terminal::PaneStatusAction::Agent { command, origin }
                        if command == "/plan on"
                            && origin.as_ref().is_some_and(|origin| origin.trusted)
                )
            })
        })
        .expect("pane-settings should contain the agent action");
    service
        .pane_agent_status_selector_mut_for_tests()
        .expect("pane-settings selector should remain open")
        .active_index = agent_index;
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(service.agent_planning_enabled("%2"));
    assert!(!service.agent_planning_enabled("%1"));
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .id
            .as_str(),
        "%1"
    );
}

/// Verifies a configured custom action cannot outlive its stable pane owner.
#[test]
fn runtime_pane_settings_custom_action_rejects_closed_owner() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.pane]\nright_status = \"#{pill.rename}\"\n[frames.pane.pills.rename]\nfield = \"policy.mode\"\nwhen = []\non_click = \"terminal:rename-pane -t {pane} provider-owner\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );
    service.session.select_pane(&primary, "%1").unwrap();
    service
        .execute_terminal_command(&primary, "pane-settings -t %2")
        .unwrap();
    let action_index = service
        .pane_agent_status_selector()
        .and_then(|selector| {
            selector.settings_entries.iter().position(|entry| {
                matches!(
                    entry.identity.action,
                    crate::host::terminal::PaneStatusAction::Terminal { .. }
                )
            })
        })
        .expect("pane-settings should contain the terminal action");
    service
        .pane_agent_status_selector_mut_for_tests()
        .expect("pane-settings selector should remain open")
        .active_index = action_index;
    service
        .execute_terminal_command(&primary, "kill-pane --force -t %2")
        .unwrap();

    let error = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
    assert!(
        error
            .message()
            .contains("pane status action owner is no longer available")
    );
    assert_ne!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "provider-owner"
    );
}

/// Verifies configured agent-action ingress rejects commands outside its
/// narrow pane-local control surface even when called directly at runtime.
/// Parser validation is not sufficient because restored or synthesized action
/// state must not gain access to generic security and global command dispatch.
#[test]
fn runtime_pane_status_agent_action_ingress_rejects_security_commands() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let error = service
        .execute_agent_shell_command_for_pane(&primary, "%1", "/sandbox disable --global")
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(error.message().contains("only /plan and /stop"));
}

/// Verifies the runtime custom-action trust gate rejects both missing and
/// explicitly untrusted exact-source provenance while admitting a trusted
/// effective source. Static document validation intentionally has no such
/// provenance object, so enforcement belongs at live action dispatch.
#[test]
fn runtime_pane_status_custom_action_origin_fails_closed() {
    let missing = crate::runtime::commands_support::require_runtime_pane_status_action_origin(None)
        .unwrap_err();
    assert_eq!(missing.kind(), crate::error::MezErrorKind::Forbidden);

    let untrusted = crate::host::terminal::PaneStatusProviderOrigin {
        layer_name: "project".to_string(),
        scope: crate::host::terminal::PaneStatusProviderScope::ProjectOverlay,
        path: Some("/repo/.mezzanine/config.toml".to_string()),
        trusted: false,
    };
    let rejected = crate::runtime::commands_support::require_runtime_pane_status_action_origin(
        Some(&untrusted),
    )
    .unwrap_err();
    assert_eq!(rejected.kind(), crate::error::MezErrorKind::Forbidden);

    let trusted = crate::host::terminal::PaneStatusProviderOrigin {
        trusted: true,
        ..untrusted
    };
    crate::runtime::commands_support::require_runtime_pane_status_action_origin(Some(&trusted))
        .unwrap();
}

/// Verifies live pane-status action dispatch uses the trust bit from the exact
/// effective layer that supplied `on_click`. Even an otherwise supported
/// owner-targeted rename must remain inert when that source layer is untrusted.
#[test]
fn runtime_pane_status_custom_action_rejects_untrusted_effective_source() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "untrusted-primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: false,
            text: "[frames.pane]\nright_status = \"#{pill.rename}\"\n[frames.pane.pills.rename]\nfield = \"policy.mode\"\nwhen = []\non_click = \"terminal:rename-pane -t {pane} untrusted-name\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .execute_terminal_command(&primary, "pane-settings -t %1")
        .unwrap();
    let action_index = service
        .pane_agent_status_selector()
        .and_then(|selector| {
            selector.settings_entries.iter().position(|entry| {
                matches!(
                    &entry.identity.action,
                    crate::host::terminal::PaneStatusAction::Terminal { .. }
                )
            })
        })
        .expect("pane-settings should retain the configured terminal action");
    let action_origin = service
        .pane_agent_status_selector()
        .and_then(|selector| selector.settings_entries.get(action_index))
        .and_then(|entry| match &entry.identity.action {
            crate::host::terminal::PaneStatusAction::Terminal { origin, .. } => origin.as_ref(),
            _ => None,
        })
        .expect("live terminal action should retain exact source provenance");
    assert_eq!(action_origin.layer_name, "untrusted-primary");
    assert!(!action_origin.trusted);
    service
        .pane_agent_status_selector_mut_for_tests()
        .expect("pane-settings selector should remain open")
        .active_index = action_index;

    let error = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(
        error
            .message()
            .contains("trusted effective source provenance")
    );
    assert_ne!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "untrusted-name"
    );
}

/// Verifies an overflowed dropdown control remains keyboard-accessible through
/// `pane-settings` even though the pane frame exposes no direct hit cells for
/// that omitted occurrence. Opening the value selector must retain the same
/// stable pane owner and must not change pane focus.
#[test]
fn runtime_pane_settings_opens_overflowed_dropdown_without_visible_hit_cells() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.pane]\nright_status = \"#{policy.mode}\"\noverflow = \"menu\"\ntitle_min_width = 79\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_terminal_command(&primary, "pane-settings -t %1")
        .unwrap();
    let approval_index = service
        .pane_agent_status_selector()
        .and_then(|selector| {
            selector.settings_entries.iter().position(|entry| {
                entry.identity.action
                    == crate::host::terminal::PaneStatusAction::Builtin(
                        PaneAgentStatusField::ApprovalPolicy,
                    )
            })
        })
        .expect("overflow menu should retain the approval control");
    assert!(
        service
            .pane_agent_status_selector()
            .and_then(|selector| selector.items.get(approval_index))
            .is_some_and(|item| item.contains("[overflow; control]"))
    );
    service
        .pane_agent_status_selector_mut_for_tests()
        .expect("pane-settings selector should remain open")
        .active_index = approval_index;

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    let selector = service
        .pane_agent_status_selector()
        .expect("overflowed approval control should open its value selector");
    assert_eq!(selector.pane_id, "%1");
    assert_eq!(selector.field, PaneAgentStatusField::ApprovalPolicy);
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .id
            .as_str(),
        "%1"
    );
}

/// Verifies `pane-settings` accepts only its documented selector, provider
/// inspection, and provider retry forms so malformed options stay inert.
#[test]
fn runtime_pane_settings_rejects_noncanonical_arguments() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    for input in [
        "pane-settings extra",
        "pane-settings --target %1",
        "pane-settings -t",
        "pane-settings -t %1 extra",
        "pane-settings --providers extra",
        "pane-settings --providers -t",
        "pane-settings --retry-provider",
        "pane-settings --retry-provider branch extra",
        "pane-settings --retry-provider branch -t",
    ] {
        let error = service
            .execute_terminal_command(&primary, input)
            .unwrap_err();
        assert_eq!(
            error.kind(),
            crate::error::MezErrorKind::InvalidArgs,
            "{input}"
        );
        assert_eq!(
            error.message(),
            "usage: pane-settings [--providers | --retry-provider NAME] [-t pane]",
            "{input}"
        );
        assert!(service.pane_agent_status_selector().is_none(), "{input}");
    }
}

/// Verifies the JSON-RPC terminal command route invokes the same zen handler
/// and returns the same structured silent outcome as direct runtime callers.
#[test]
fn runtime_control_terminal_command_uses_zen_live_override_handler() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"zen-on","method":"terminal/command","params":{"idempotency_key":"zen-on","input":"zen on"}}"#,
        &primary,
    );
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();

    assert_eq!(response["result"]["executed"], 1);
    assert_eq!(response["result"]["outcomes"][0]["command"], "zen");
    assert_eq!(response["result"]["outcomes"][0]["kind"], "mutated");
    assert!(service.terminal_zen_mode());
}
