//! Agent prompt editing input tests.

use super::*;

/// Verifies that ordinary pane input is redirected into the pane-local agent
/// prompt while agent mode is active, without entering the older modal prompt
/// loop. Mux actions remain available because only forward-to-pane text is
/// intercepted by the runtime.
#[test]
fn runtime_attached_input_submits_visible_agent_prompt_non_modally() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(
            b"summarize\nmore\r".to_vec(),
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .map(|task| task.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-1"]
    );
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "");
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[String::from("summarize\nmore")]
    );
    assert!(prompt_state.display_lines.is_empty());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> summarize"), "{pane_text}");
    assert!(pane_text.contains("more"), "{pane_text}");
    assert!(
        !pane_text.contains("agent: turn turn-1 running"),
        "{pane_text}"
    );
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .unwrap();
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert_eq!(turn.state, AgentTurnState::Running);
    assert!(
        context
            .blocks
            .iter()
            .any(|block| block.content.contains("summarize\nmore"))
    );
}

/// Verifies large prompt paste blocks can exceed the visible pane area.
///
/// Bracketed paste payloads may arrive split across terminal reads and contain
/// far more text than can be rendered in the prompt area. The prompt renderer
/// should show one compact block while the submitted turn receives the exact
/// payload.
#[test]
fn runtime_agent_prompt_preserves_large_split_paste_beyond_visible_area() {
    let mut service = test_runtime_service_with_size(Size::new(50, 8).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(50, 8).unwrap(), 10).unwrap(),
    );

    let payload = (0..80)
        .map(|index| format!("line-{index:02}-{}", "x".repeat(36)))
        .collect::<Vec<_>>()
        .join("\n");
    let mut first = Vec::new();
    first.extend_from_slice(b"prefix ");
    first.extend_from_slice(b"\x1b[200~");
    first.extend_from_slice(&payload.as_bytes()[..payload.len() / 2]);
    let mut second = Vec::new();
    second.extend_from_slice(&payload.as_bytes()[payload.len() / 2..]);
    second.extend_from_slice(b"\x1b[201~ suffix\r");

    for input in [first, second] {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(input)],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }

    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[format!("prefix {payload} suffix")]
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> prefix [Pasted"), "{pane_text}");
    assert!(!pane_text.contains("line-79"), "{pane_text}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(
        context
            .blocks
            .iter()
            .any(|block| { block.content.contains(&format!("prefix {payload} suffix")) })
    );
}

/// Verifies that the pane-local agent prompt accepts encoded Ctrl+R from
/// terminals that use xterm modifyOtherKeys for modified printable keys.
///
/// Agent mode intercepts ordinary pane input before it reaches the PTY. This
/// protects that interception path so encoded reverse-search keys still edit
/// the prompt from its history instead of becoming a no-op escape sequence.
#[test]
fn runtime_agent_prompt_accepts_encoded_ctrl_r_history_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state
            .prompt
            .buffer
            .set_history(vec!["/status".to_string(), "/help".to_string()]);
        prompt_state.prompt.buffer.set_line("/s");
    }

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"\x1b[27;5;114~".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "/status");
}

/// Verifies standalone Escape clears pane-local agent prompt text without
/// hiding the agent shell.
///
/// Agent-shell exit is reserved for Ctrl+C confirmation or empty Ctrl+D. A
/// normal Escape press should only clear the current draft and keep the pane
/// prompt session active.
#[test]
fn runtime_agent_prompt_escape_clears_input_without_hiding_shell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state.prompt.buffer.set_line("draft text");
    }
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    let followup = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"next".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(followup.forwarded_bytes, 0);
    assert_eq!(followup.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "next");
}

/// Verifies standalone Escape cancels pane-local agent reverse search without
/// exiting the agent shell.
///
/// Agent prompts share readline behavior with the primary command prompt, but
/// Escape also has agent-mode exit semantics. This keeps the reverse-search
/// case routed to the prompt before the broader exit handling runs.
#[test]
fn runtime_agent_prompt_escape_cancels_reverse_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state
            .prompt
            .buffer
            .set_history(vec!["/status".to_string(), "/help".to_string()]);
        prompt_state.prompt.buffer.set_line("/s");
    }

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x12".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .reverse_search_active()
    );

    let escape = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(escape.forwarded_bytes, 0);
    assert_eq!(escape.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert!(!prompt_state.prompt.reverse_search_active());
    assert_eq!(prompt_state.prompt.buffer.line(), "/s");
    assert!(service.agent_shell_store().get("%1").is_some());
}
