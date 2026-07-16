//! Agent shell records tests.

use super::*;

/// Verifies agent-shell record browsers keep their typed browser state after
/// the Markdown display response opens the primary overlay.
///
/// `/show-issues` and `/show-memories` cross a JSON display-response boundary
/// before the terminal UI decides whether to open a modal pager. Retaining the
/// browser beside the rendered overlay is the prerequisite for later key-driven
/// filtering, detail navigation, and save prompts to act on structured browser
/// state instead of reparsing displayed Markdown.
#[test]
fn runtime_agent_shell_record_browser_display_retains_overlay_state() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let browser = mez_mux::record_browser::RecordBrowser::new(
        "Issues",
        vec![mez_mux::record_browser::RecordBrowserRecord {
            id: "issue-1".to_string(),
            open_command: Some("/show-issues issue-1".to_string()),
            title: "First issue".to_string(),
            metadata: vec![("kind".to_string(), "task".to_string())],
            markdown: "Body".to_string(),
        }],
        Vec::new(),
    )
    .unwrap();
    service.register_pending_record_browser_overlay(&pane_id, "show-issues", browser, None);
    let response = crate::runtime::runtime_agent_shell_command_response_json(
        &pane_id,
        "/show-issues",
        Some(&crate::runtime::AgentShellCommandOutcome::Display {
            command: "show-issues".to_string(),
            body: "# Issues\n\n- [`issue-1`](mez-agent:%2Fshow-issues%20issue-1)".to_string(),
        }),
    );
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    let overlay = service
        .primary_display_overlay()
        .expect("record-browser display should open an overlay");
    let record_browser = overlay
        .record_browser
        .as_ref()
        .expect("overlay should retain record-browser state");
    assert_eq!(record_browser.pane_id, pane_id);
    assert_eq!(record_browser.command, "show-issues");
    assert_eq!(record_browser.browser.render_page().title, "Issues");
    assert!(service.pending_record_browser_overlays_is_empty());
}

/// Verifies `/show-issues` overlays expose record-browser footer help and keep
/// Enter routed through the focused Markdown selection.
///
/// The browser intercepts filter and save keys directly, but Select should
/// still fall through to the shared overlay selection path so the focused
/// record opens as a child detail view.
#[test]
fn runtime_agent_shell_record_browser_footer_and_enter_open_detail() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 12).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-record-browser-footer-enter");
    let config_root = root.join("config");
    fs::create_dir_all(&config_root).unwrap();
    service.set_config_root(config_root.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[issues]
enabled = true
"
            .to_string(),
        }])
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let project = crate::storage::issues::project_key_for_working_directory(
        service
            .pane_current_working_directory(&pane_id)
            .unwrap_or_else(|| config_root.clone()),
    );
    let store = crate::storage::issues::IssueStore::under_config_root(config_root.clone());
    store
        .add_issue(
            project.clone(),
            mez_agent::issues::IssueKind::Defect,
            "Second issue".to_string(),
            Some("Second body".to_string()),
            None,
            1,
        )
        .unwrap();
    store
        .add_issue(
            project,
            mez_agent::issues::IssueKind::Task,
            "First issue".to_string(),
            Some("First body".to_string()),
            None,
            2,
        )
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/show-issues")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    let overlay_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 12).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let footer = overlay_view.lines.last().cloned().unwrap_or_default();
    assert!(footer.contains("esc: back"), "{footer}");
    assert!(footer.contains("enter: open"), "{footer}");
    assert!(footer.contains("k/p/x: filter"), "{footer}");
    assert!(footer.contains("s: save"), "{footer}");

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

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    let overlay = service
        .primary_display_overlay()
        .expect("Enter should keep the detail overlay open");
    let record_browser = overlay
        .record_browser
        .as_ref()
        .expect("detail overlay should retain record-browser state");
    assert_eq!(record_browser.command, "show-issues");
    assert_eq!(record_browser.browser.render_page().title, "First issue");
    assert!(overlay.lines.iter().any(|line| line.contains("First body")));
    let _ = fs::remove_dir_all(root);
}

/// Verifies Escape restores the parent record-browser list after a selected
/// record opens a child detail view.
///
/// The detail command crosses the agent-shell display response boundary, so
/// the parent browser and pager cursor must survive in the retained view stack
/// instead of being replaced permanently by the child overlay.
#[test]
fn runtime_agent_shell_record_browser_escape_restores_parent_view_stack() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let parent_browser = mez_mux::record_browser::RecordBrowser::new(
        "Issues",
        vec![
            mez_mux::record_browser::RecordBrowserRecord {
                id: "issue-1".to_string(),
                open_command: Some("/show-issues issue-1".to_string()),
                title: "First issue".to_string(),
                metadata: vec![("kind".to_string(), "task".to_string())],
                markdown: "First body".to_string(),
            },
            mez_mux::record_browser::RecordBrowserRecord {
                id: "issue-2".to_string(),
                open_command: Some("/show-issues issue-2".to_string()),
                title: "Second issue".to_string(),
                metadata: vec![("kind".to_string(), "defect".to_string())],
                markdown: "Second body".to_string(),
            },
        ],
        Vec::new(),
    )
    .unwrap();
    let mut child_browser = mez_mux::record_browser::RecordBrowser::new(
        "Issue detail",
        vec![mez_mux::record_browser::RecordBrowserRecord {
            id: "issue-1".to_string(),
            open_command: Some("/show-issues issue-1".to_string()),
            title: "First issue".to_string(),
            metadata: vec![("kind".to_string(), "task".to_string())],
            markdown: "First body".to_string(),
        }],
        Vec::new(),
    )
    .unwrap();
    child_browser.show_first_record_detail();
    let child_page = child_browser.render_page();
    service.register_pending_record_browser_overlay(&pane_id, "show-issues", child_browser, None);
    service.set_pending_record_browser_overlay_stack_for_tests(
        &pane_id,
        "show-issues",
        vec![
            crate::runtime::service_state::RuntimeRecordBrowserOverlayFrame {
                command: "show-issues".to_string(),
                source: None,
                browser: parent_browser,
                scroll_offset: 0,
                active_selection_index: Some(1),
            },
        ],
    );
    let response = crate::runtime::runtime_agent_shell_command_response_json(
        &pane_id,
        "/show-issues issue-1",
        Some(&crate::runtime::AgentShellCommandOutcome::Display {
            command: "show-issues".to_string(),
            body: child_page.raw_markdown,
        }),
    );
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    assert_eq!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .map(|record_browser| record_browser.browser.render_page().title),
        Some("First issue".to_string())
    );

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
    assert!(report.view_refresh_required);
    let overlay = service
        .primary_display_overlay()
        .expect("Escape should keep the restored parent overlay open");
    let record_browser = overlay
        .record_browser
        .as_ref()
        .expect("restored overlay should keep record-browser state");
    assert_eq!(record_browser.browser.render_page().title, "Issues");
    assert!(record_browser.stack.is_empty());
    assert_eq!(overlay.active_selection_index, Some(1));
    assert!(overlay.lines.iter().any(|line| line.contains("issue-2")));
}
