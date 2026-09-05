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
