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
    assert!(!service.active_copy_modes().contains_key("%1"));
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
