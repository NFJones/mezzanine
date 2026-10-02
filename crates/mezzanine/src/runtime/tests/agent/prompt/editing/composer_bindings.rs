//! Effective composer hints checked against production attached-input routing.

use super::*;

/// Completion help reflects mux interception after a selector was opened. Tab
/// and Enter keep their actual routes; pending prefix mode hides Escape reset.
#[test]
fn composer_selector_help_matches_intercepted_production_routes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    let prompt = &mut service
        .agent_prompt_inputs_mut_for_tests()
        .get_mut("%1")
        .unwrap()
        .prompt;
    prompt.buffer.set_line("$rev");
    prompt.set_selector_extra_candidates([crate::ui::selector::SelectorExtraCandidate::new(
        crate::ui::selector::SelectorSurface::AgentCommand,
        "$",
        mez_mux::selector::SelectorCandidate::new(
            "$review",
            mez_mux::selector::SelectorCandidateKind::Value,
            true,
        ),
    )]);
    prompt.apply_terminal_input(b"\t").unwrap();
    assert!(prompt.selector.is_some());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "selector-composer".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[keys]\nfocus_up = \"C-i\"\nfocus_down = \"C-m\"\n".into(),
        }])
        .unwrap();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    for bytes in [b"\r".as_slice(), b"\t".as_slice()] {
        assert!(matches!(
            crate::host::terminal::route_client_input(bytes, &config).unwrap(),
            TerminalClientLoopAction::ExecuteMux(_)
        ));
    }
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 40).unwrap(),
            &config,
        )
        .unwrap()
        .unwrap();
    assert!(
        !view
            .lines
            .iter()
            .any(|line| line.contains("Tab next") || line.contains("Enter send"))
    );
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("Shift+Tab previous"))
    );
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::EnterPrefixKeyMode],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let pending = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert!(!matches!(
        crate::host::terminal::route_client_input(b"\x1b", &pending).unwrap(),
        TerminalClientLoopAction::ForwardToPane(_)
    ));
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 40).unwrap(),
            &pending,
        )
        .unwrap()
        .unwrap();
    assert!(
        !view
            .lines
            .iter()
            .any(|line| line.contains("Esc stop") || line.contains("esc to interrupt"))
    );
}

/// Direct mux bindings intercept Tab, Enter and cancellation; the prefix takes
/// Ctrl+R. Search remains active across configuration changes, but its help must
/// no longer advertise those intercepted controls. Input precedence is unchanged.
#[test]
fn composer_search_help_matches_intercepted_production_routes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service
        .agent_prompt_inputs_mut_for_tests()
        .get_mut("%1")
        .unwrap()
        .prompt
        .apply_terminal_input(b"\x12")
        .unwrap();
    service.replace_config_layers(vec![ConfigLayer {
        name: "intercepted-composer".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[keys]\nescape = \"C-r\"\nfocus_up = \"C-i\"\nfocus_down = \"C-m\"\nfocus_left = \"C-c\"\n".into(),
    }]).unwrap();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let keys = config.frame_context.panes["%1"]
        .agent_composer
        .as_ref()
        .unwrap()
        .keys
        .as_ref()
        .unwrap();
    assert!(!keys.enter && !keys.tab && !keys.search && !keys.cancel_search);
    for bytes in [b"\r".as_slice(), b"\t".as_slice(), b"\x03".as_slice()] {
        assert!(matches!(
            crate::host::terminal::route_client_input(bytes, &config).unwrap(),
            TerminalClientLoopAction::ExecuteMux(_)
        ));
    }
    assert!(matches!(
        crate::host::terminal::route_client_input(b"\x12", &config).unwrap(),
        TerminalClientLoopAction::EnterPrefixKeyMode
    ));
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 40).unwrap(),
            &config,
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("Search history"))
    );
    for hint in ["Enter accept", "Ctrl+R search", "Ctrl+C cancel"] {
        assert!(!view.lines.iter().any(|line| line.contains(hint)));
    }
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::EnterPrefixKeyMode],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let pending = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert!(
        !pending.frame_context.panes["%1"]
            .agent_composer
            .as_ref()
            .unwrap()
            .keys
            .as_ref()
            .unwrap()
            .escape
    );
    assert!(!matches!(
        crate::host::terminal::route_client_input(b"\x1b", &pending).unwrap(),
        TerminalClientLoopAction::ForwardToPane(_)
    ));
}
