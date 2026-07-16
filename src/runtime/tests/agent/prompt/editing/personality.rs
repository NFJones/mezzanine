//! Agent prompt editing personality tests.

use super::*;

/// Verifies `/personality` completion includes user-configured personality
/// profile ids.
///
/// Personality profiles have no built-in names, so completion must be sourced
/// from live runtime config rather than from a static candidate list.
#[test]
fn runtime_agent_prompt_personality_autocompletes_configured_profile() {
    let mut service = test_runtime_service();
    let root = temp_root("runtime-agent-personality-complete");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[personalities.careful]\nname = \"Careful\"\nresponse_style = \"terse\"\n",
    )
    .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/personality car".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/personality careful "
    );
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/personality` mutates live pane-scoped agent preferences and
/// that those preferences are appended to the next prompt context. This makes
/// the slash command affect provider input instead of only acknowledging a
/// runtime placeholder.
#[test]
fn runtime_agent_shell_personality_feeds_prompt_context() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let personality = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"personality","method":"agent/shell/command","params":{"idempotency_key":"personality","input":"/personality concise"}}"#,
        &primary,
    );
    assert!(personality.contains(r#""kind":"mutated""#), "{personality}");
    assert!(
        personality.contains(r#""command":"personality""#),
        "{personality}"
    );
    assert!(personality.contains("style=concise"), "{personality}");
    assert!(
        personality.contains("source=runtime-personality"),
        "{personality}"
    );
    assert!(!personality.contains("requires_runtime"), "{personality}");

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"preference-prompt","method":"agent/shell/command","params":{"idempotency_key":"preference-prompt","input":"prepare work"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(
        context
            .blocks
            .iter()
            .any(|block| block.label == "agent shell personality"
                && block.content.contains("concise"))
    );
}
