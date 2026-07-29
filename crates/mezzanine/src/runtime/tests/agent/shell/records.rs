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

/// Verifies record-browser Ctrl+Up and Ctrl+Down move by five lines, while
/// PageUp and PageDown move by the active modal page.
///
/// Ctrl-arrow navigation provides bounded fine scrolling without changing the
/// selected record. Page navigation still uses the modal's content-row capacity
/// and returns to the initial viewport on PageUp.
#[test]
fn runtime_record_browser_ctrl_arrows_scroll_five_lines_and_paging_uses_modal_height() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 30).unwrap();
    let primary = service.attach_primary("primary", true, size, 120).unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let records = (0..100)
        .map(|index| mez_mux::record_browser::RecordBrowserRecord {
            id: format!("issue-{index}"),
            open_command: Some(format!("/show-issues issue-{index}")),
            title: format!("Issue {index}"),
            metadata: Vec::new(),
            markdown: String::new(),
        })
        .collect();
    let browser =
        mez_mux::record_browser::RecordBrowser::new("Issues", records, Vec::new()).unwrap();
    let page = browser.render_page();
    service.register_pending_record_browser_overlay(&pane_id, "show-issues", browser, None);
    let response = crate::runtime::runtime_agent_shell_command_response_json(
        &pane_id,
        "/show-issues",
        Some(&crate::runtime::AgentShellCommandOutcome::Display {
            command: "show-issues".to_string(),
            body: page.raw_markdown,
        }),
    );
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    apply_record_browser_input(&mut service, &primary, b"\x1b[1;5B");
    assert_eq!(service.primary_display_overlay().unwrap().scroll_offset, 5);

    apply_record_browser_input(&mut service, &primary, b"\x1b[1;5A");
    assert_eq!(service.primary_display_overlay().unwrap().scroll_offset, 0);

    apply_record_browser_input(&mut service, &primary, b"\x1b[6~");
    assert_eq!(
        service.primary_display_overlay().unwrap().scroll_offset,
        mez_mux::render::modal_overlay_page_rows(size)
    );

    apply_record_browser_input(&mut service, &primary, b"\x1b[5~");
    assert_eq!(service.primary_display_overlay().unwrap().scroll_offset, 0);
}

/// Verifies kind-selector navigation preserves the retained record cursor.
///
/// The selector and record links share an overlay presentation, but moving a
/// selector row must not replace the selected record that the browser retains
/// for later detail navigation.
#[test]
fn runtime_record_browser_kind_selector_navigation_preserves_record_cursor() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let mut browser = mez_mux::record_browser::RecordBrowser::new(
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
        vec![
            mez_mux::record_browser::RecordBrowserFilterChoice {
                label: "all kinds".to_string(),
                value: String::new(),
            },
            mez_mux::record_browser::RecordBrowserFilterChoice {
                label: "defect".to_string(),
                value: "defect".to_string(),
            },
        ],
    )
    .unwrap();
    browser.set_table_id_column("Issue");
    browser.set_table_columns_with_labels(vec![
        ("Summary".to_string(), "summary".to_string()),
        ("Kind".to_string(), "kind".to_string()),
    ]);
    browser.set_active_index(1);
    let page = browser.render_page();
    service.register_pending_record_browser_overlay(&pane_id, "show-issues", browser, None);
    let response = crate::runtime::runtime_agent_shell_command_response_json(
        &pane_id,
        "/show-issues",
        Some(&crate::runtime::AgentShellCommandOutcome::Display {
            command: "show-issues".to_string(),
            body: page.raw_markdown,
        }),
    );
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    apply_record_browser_input(&mut service, &primary, b"k");
    let overlay = service
        .primary_display_overlay()
        .expect("kind selector should retain the display overlay");
    assert!(
        overlay
            .selections
            .iter()
            .all(|selection| selection.command.is_empty()),
        "kind-selector focus must exclude record-link selections: {:?}",
        overlay.selections
    );
    let selector_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 12).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let all_kinds_row = selector_view
        .lines
        .iter()
        .position(|line| line.contains("all kinds"))
        .expect("kind selector should render the all-kinds choice");
    let defect_row = selector_view
        .lines
        .iter()
        .position(|line| line.contains("defect"))
        .expect("kind selector should render the defect choice");
    let all_kinds_column =
        display_column_for_fragment(&selector_view.lines[all_kinds_row], "all kinds");
    let defect_column = display_column_for_fragment(&selector_view.lines[defect_row], "defect");
    let active_rendition = styled_line_rendition_at(
        &TerminalStyledLine {
            text: selector_view.lines[all_kinds_row].clone(),
            style_spans: selector_view.line_style_spans[all_kinds_row].clone(),
            copy_text: None,
        },
        all_kinds_column,
    );
    let inactive_rendition = styled_line_rendition_at(
        &TerminalStyledLine {
            text: selector_view.lines[defect_row].clone(),
            style_spans: selector_view.line_style_spans[defect_row].clone(),
            copy_text: None,
        },
        defect_column,
    );
    assert_eq!(
        active_rendition.foreground,
        Some(service.ui_theme().colors.agent_model.foreground)
    );
    assert_eq!(
        active_rendition.background,
        Some(service.ui_theme().colors.agent_model.background)
    );
    assert_eq!(
        inactive_rendition.foreground,
        Some(service.ui_theme().colors.display_overlay.foreground)
    );
    assert_eq!(inactive_rendition.background, None);

    apply_record_browser_input(&mut service, &primary, b"\x1b[B");

    let moved_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 12).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let moved_all_kinds_rendition = styled_line_rendition_at(
        &TerminalStyledLine {
            text: moved_view.lines[all_kinds_row].clone(),
            style_spans: moved_view.line_style_spans[all_kinds_row].clone(),
            copy_text: None,
        },
        all_kinds_column,
    );
    let moved_defect_rendition = styled_line_rendition_at(
        &TerminalStyledLine {
            text: moved_view.lines[defect_row].clone(),
            style_spans: moved_view.line_style_spans[defect_row].clone(),
            copy_text: None,
        },
        defect_column,
    );
    assert_eq!(
        moved_all_kinds_rendition.foreground,
        Some(service.ui_theme().colors.display_overlay.foreground)
    );
    assert_eq!(moved_all_kinds_rendition.background, None);
    assert_eq!(
        moved_defect_rendition.foreground,
        Some(service.ui_theme().colors.agent_model.foreground)
    );
    assert_eq!(
        moved_defect_rendition.background,
        Some(service.ui_theme().colors.agent_model.background)
    );

    let browser = &service
        .primary_display_overlay()
        .unwrap()
        .record_browser
        .as_ref()
        .unwrap()
        .browser;
    assert_eq!(browser.active_index(), 1);
    assert_eq!(browser.active_record_id(), Some("issue-2"));
    assert_eq!(
        browser
            .prompt_selection()
            .map(|selection| selection.active_index),
        Some(1)
    );
}

/// Verifies retained record browsers reflow from raw Markdown when the primary
/// terminal becomes narrower and paginate the resulting physical rows.
///
/// Rewrapping previously rendered strings would compound indentation and lose
/// Markdown structure. The resize path must instead rerender the retained
/// browser, bound every selectable body row after its two-cell gutter, and make
/// the modal footer count the expanded physical-row collection.
#[test]
fn runtime_record_browser_resize_reflows_rows_and_footer_counts_physical_lines() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 6).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let browser = mez_mux::record_browser::RecordBrowser::new(
        "Issues",
        vec![mez_mux::record_browser::RecordBrowserRecord {
            id: "issue-1".to_string(),
            open_command: Some("/show-issues issue-1".to_string()),
            title: "A record title with enough words to occupy several physical rows when rendered as a capped detail but not when rendered in the wider list view".to_string(),
            metadata: vec![("kind".to_string(), "defect".to_string())],
            markdown: "A detail body with enough words to wrap.".to_string(),
        }],
        Vec::new(),
    )
    .unwrap();
    let page = browser.render_page();
    service.register_pending_record_browser_overlay(&pane_id, "show-issues", browser, None);
    let response = crate::runtime::runtime_agent_shell_command_response_json(
        &pane_id,
        "/show-issues",
        Some(&crate::runtime::AgentShellCommandOutcome::Display {
            command: "show-issues".to_string(),
            body: page.raw_markdown,
        }),
    );
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    let wide_line_count = service.primary_display_overlay().unwrap().lines.len();

    apply_record_browser_input(&mut service, &primary, b"\r");
    let detail_line_count = service.primary_display_overlay().unwrap().lines.len();
    assert!(detail_line_count > wide_line_count);
    apply_record_browser_input(&mut service, &primary, b"\x1b");
    assert_eq!(
        service.primary_display_overlay().unwrap().lines.len(),
        wide_line_count
    );

    service
        .resize_attached_primary_terminal(&primary, Size::new(20, 6).unwrap())
        .unwrap();

    let overlay = service.primary_display_overlay().unwrap();
    assert!(overlay.lines.len() > wide_line_count, "{overlay:?}");
    assert!(
        overlay
            .lines
            .iter()
            .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= 18),
        "{overlay:?}"
    );
    let physical_line_count = overlay.lines.len();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(20, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .last()
            .is_some_and(|footer| footer.contains(&format!("/{physical_line_count}"))),
        "{view:?}"
    );
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
    let older_issue = store
        .add_issue(
            project.clone(),
            mez_agent::issues::IssueKind::Defect,
            "Second issue".to_string(),
            Some("Second body".to_string()),
            None,
            1,
        )
        .unwrap();
    let recent_issue = store
        .add_issue(
            project.clone(),
            mez_agent::issues::IssueKind::Task,
            "First issue".to_string(),
            Some("First body".to_string()),
            None,
            2,
        )
        .unwrap();
    let cross_project_issue = store
        .add_issue(
            "/other/project".to_string(),
            mez_agent::issues::IssueKind::Task,
            "Cross-project issue".to_string(),
            Some("Cross-project body".to_string()),
            None,
            3,
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
    assert!(footer.contains("a: all"), "{footer}");
    assert!(footer.contains("k/p/x: filter"), "{footer}");
    assert!(footer.contains("s: save"), "{footer}");
    let overlay = service.primary_display_overlay().unwrap();
    let page = overlay
        .record_browser
        .as_ref()
        .unwrap()
        .browser
        .render_page();
    assert!(
        page.raw_markdown
            .contains("| Issue | Summary | Project | Kind | State | Updated |"),
        "{}",
        page.raw_markdown
    );
    assert!(
        page.raw_markdown.contains("First issue"),
        "{}",
        page.raw_markdown
    );
    assert_eq!(
        overlay
            .selections
            .iter()
            .map(|selection| selection.logical_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
    let recent_selection_index = overlay
        .selections
        .iter()
        .position(|selection| selection.logical_id == 0)
        .unwrap();
    let older_selection_index = overlay
        .selections
        .iter()
        .position(|selection| selection.logical_id == 1)
        .unwrap();
    assert_eq!(overlay.active_selection_index, Some(recent_selection_index));
    assert_eq!(
        overlay.selections[recent_selection_index].command,
        format!("/show-issues {}", recent_issue.id)
    );
    assert_eq!(
        overlay.selections[older_selection_index].command,
        format!("/show-issues {}", older_issue.id)
    );
    assert!(
        !overlay_view
            .lines
            .iter()
            .any(|line| line.contains(&cross_project_issue.id)),
        "{overlay_view:?}"
    );

    let toggle_all = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"a".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(toggle_all.forwarded_bytes, 0);
    assert!(toggle_all.view_refresh_required);
    let overlay = service.primary_display_overlay().unwrap();
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains("all projects"))
    );
    assert!(
        overlay
            .record_browser
            .as_ref()
            .unwrap()
            .browser
            .records()
            .iter()
            .any(|record| record.id == cross_project_issue.id)
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"a".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let overlay = service.primary_display_overlay().unwrap();
    assert!(
        !overlay
            .record_browser
            .as_ref()
            .unwrap()
            .browser
            .records()
            .iter()
            .any(|record| record.id == cross_project_issue.id)
    );

    apply_record_browser_input(&mut service, &primary, b"\x1b[B");
    let overlay = service.primary_display_overlay().unwrap();
    let older_selection_index = overlay
        .selections
        .iter()
        .position(|selection| selection.logical_id == 1)
        .unwrap();
    assert_eq!(overlay.active_selection_index, Some(older_selection_index));
    assert_eq!(
        overlay
            .record_browser
            .as_ref()
            .unwrap()
            .browser
            .active_record_id(),
        Some(older_issue.id.as_str())
    );

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
    assert_eq!(record_browser.browser.render_page().title, "Second issue");
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains("Second body"))
    );
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
    assert_eq!(record_browser.browser.active_record_id(), Some("issue-2"));
    assert_eq!(overlay.active_selection_index, Some(1));
    assert!(overlay.lines.iter().any(|line| line.contains("issue-2")));
}

/// Verifies `/show-context` renders only the active pane conversation in
/// transcript order and deletes the entry selected with pager arrow keys.
#[test]
fn runtime_agent_shell_show_context_deletes_the_selected_active_session_entry() {
    let root = temp_root("runtime-show-context-delete");
    let _ = fs::remove_dir_all(&root);
    let transcript_store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 14).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get(&pane_id)
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append_many(&[
            TranscriptEntry {
                conversation_id: conversation_id.clone(),
                sequence: 1,
                created_at_unix_seconds: 1,
                role: TranscriptRole::User,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                pane_id: pane_id.clone(),
                content: "first context entry".to_string(),
            },
            TranscriptEntry {
                conversation_id: conversation_id.clone(),
                sequence: 2,
                created_at_unix_seconds: 2,
                role: TranscriptRole::Assistant,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                pane_id: pane_id.clone(),
                content: "second context entry".to_string(),
            },
            TranscriptEntry {
                conversation_id: conversation_id.clone(),
                sequence: 3,
                created_at_unix_seconds: 3,
                role: TranscriptRole::User,
                turn_id: "turn-2".to_string(),
                agent_id: "agent-%1".to_string(),
                pane_id: pane_id.clone(),
                content: "third context entry".to_string(),
            },
        ])
        .unwrap();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: "other-conversation".to_string(),
            sequence: 1,
            created_at_unix_seconds: 4,
            role: TranscriptRole::User,
            turn_id: "other-turn".to_string(),
            agent_id: "agent-%2".to_string(),
            pane_id: "%2".to_string(),
            content: "other pane context".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries(&pane_id, 3)
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/show-context")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    let overlay = service.primary_display_overlay().unwrap();
    let page = overlay
        .record_browser
        .as_ref()
        .unwrap()
        .browser
        .render_page();
    assert!(
        page.raw_markdown
            .contains("| Sequence | Summary | Role | Turn | Agent | Created |"),
        "{}",
        page.raw_markdown
    );
    assert!(
        page.raw_markdown.contains("first context entry"),
        "{}",
        page.raw_markdown
    );
    assert_eq!(
        overlay
            .selections
            .iter()
            .map(|selection| selection.logical_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    for (logical_id, command) in [
        (0, "/show-context 1"),
        (1, "/show-context 2"),
        (2, "/show-context 3"),
    ] {
        assert!(
            overlay.selections.iter().any(|selection| {
                selection.logical_id == logical_id && selection.command == command
            }),
            "{overlay:?}"
        );
    }
    assert!(
        !overlay
            .lines
            .iter()
            .any(|line| line.contains("other pane context"))
    );

    apply_record_browser_input(&mut service, &primary, b"\x1b[B");

    let overlay = service.primary_display_overlay().unwrap();
    let record_browser = overlay.record_browser.as_ref().unwrap();
    let second_selection_index = overlay
        .selections
        .iter()
        .position(|selection| selection.logical_id == 1)
        .unwrap();
    assert_eq!(record_browser.browser.active_index(), 1);
    assert_eq!(overlay.active_selection_index, Some(second_selection_index));
    assert_eq!(record_browser.browser.active_record_id(), Some("2"));

    apply_record_browser_input(&mut service, &primary, b"d");

    let overlay = service.primary_display_overlay().unwrap();
    let record_browser = overlay.record_browser.as_ref().unwrap();
    let successor_selection_index = overlay
        .selections
        .iter()
        .position(|selection| selection.logical_id == 1)
        .unwrap();
    assert_eq!(record_browser.browser.active_index(), 1);
    assert_eq!(
        overlay.active_selection_index,
        Some(successor_selection_index)
    );
    assert_eq!(record_browser.browser.active_record_id(), Some("2"));
    assert_eq!(
        overlay
            .selections
            .iter()
            .map(|selection| selection.logical_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
    assert_eq!(
        overlay.selections[successor_selection_index].command,
        "/show-context 2"
    );

    apply_record_browser_input(&mut service, &primary, b"d");

    let entries = transcript_store.inspect(&conversation_id).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].content, "first context entry");
    assert_eq!(
        service
            .agent_shell_store()
            .get(&pane_id)
            .unwrap()
            .transcript_entries,
        1
    );
    let overlay = service.primary_display_overlay().unwrap();
    let record_browser = overlay.record_browser.as_ref().unwrap();
    assert_eq!(record_browser.browser.active_index(), 0);
    assert_eq!(overlay.active_selection_index, Some(0));
    assert_eq!(record_browser.browser.active_record_id(), Some("1"));
    assert!(overlay.lines.iter().any(|line| line.contains("Sequence")));
    assert!(
        !overlay
            .lines
            .iter()
            .any(|line| line.contains("other pane context"))
    );
    assert!(transcript_store.inspect("other-conversation").is_ok());
    let _ = fs::remove_dir_all(root);
}

/// Verifies the Save prompt completes relative paths against the owning pane
/// directory and accepts the selected literal path without shell escaping.
///
/// Record-browser exports must not resolve completion candidates against the
/// Mezzanine process directory because a pane can be operating in a different
/// project. The accepted completion must also be the path submitted to the
/// existing pane-relative save boundary.
#[test]
fn runtime_record_browser_save_prompt_completes_against_pane_directory() {
    let root = temp_root("runtime-record-browser-save-completion");
    let _ = fs::remove_dir_all(&root);
    let pane_root = root.join("pane");
    fs::create_dir_all(&pane_root).unwrap();
    fs::write(pane_root.join("report.md"), "existing").unwrap();
    fs::write(pane_root.join("report.txt"), "existing").unwrap();

    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service.set_pane_current_working_directory(pane_id.clone(), pane_root.clone());
    let browser = mez_mux::record_browser::RecordBrowser::new(
        "Issues",
        vec![mez_mux::record_browser::RecordBrowserRecord {
            id: "issue-1".to_string(),
            open_command: None,
            title: "First issue".to_string(),
            metadata: Vec::new(),
            markdown: "Body".to_string(),
        }],
        Vec::new(),
    )
    .unwrap();
    let page = browser.render_page();
    service.register_pending_record_browser_overlay(&pane_id, "show-issues", browser, None);
    let response = crate::runtime::runtime_agent_shell_command_response_json(
        &pane_id,
        "/show-issues",
        Some(&crate::runtime::AgentShellCommandOutcome::Display {
            command: "show-issues".to_string(),
            body: page.raw_markdown,
        }),
    );
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    apply_record_browser_input(&mut service, &primary, b"s");
    apply_record_browser_input(&mut service, &primary, b"rep");
    let overlay = service.primary_display_overlay().unwrap();
    let shadow_line = overlay
        .lines
        .iter()
        .position(|line| line == "Save to: report.md")
        .and_then(|index| overlay.line_style_spans.get(index))
        .expect("save completion should render a presentation-only shadow suffix");
    assert!(shadow_line.iter().any(|span| span.rendition.dim));
    assert!(matches!(
        overlay.record_browser.as_ref().and_then(|record_browser| record_browser.browser.prompt()),
        Some(mez_mux::record_browser::RecordBrowserPrompt::Save { input }) if input == "rep"
    ));
    apply_record_browser_input(&mut service, &primary, b"\t");

    assert_eq!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .and_then(|record_browser| record_browser.browser.prompt())
            .map(|prompt| match prompt {
                mez_mux::record_browser::RecordBrowserPrompt::Save { input } => input.clone(),
                _ => String::new(),
            }),
        Some("report.md".to_string())
    );

    apply_record_browser_input(&mut service, &primary, b"\t");
    assert_eq!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .and_then(|record_browser| record_browser.browser.prompt())
            .map(|prompt| match prompt {
                mez_mux::record_browser::RecordBrowserPrompt::Save { input } => input.clone(),
                _ => String::new(),
            }),
        Some("report.txt".to_string())
    );

    apply_record_browser_input(&mut service, &primary, b"\x1b[Z");
    assert_eq!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .and_then(|record_browser| record_browser.browser.prompt())
            .map(|prompt| match prompt {
                mez_mux::record_browser::RecordBrowserPrompt::Save { input } => input.clone(),
                _ => String::new(),
            }),
        Some("report.md".to_string())
    );

    apply_record_browser_input(&mut service, &primary, b"\r");
    assert!(pane_root.join("report.md").is_file());
    let _ = fs::remove_dir_all(root);
}

/// Verifies editable record-browser prompts treat printable pager hotkeys as
/// literal UTF-8 text so absolute paths and project globs can be entered one
/// terminal event at a time without closing the overlay or starting search.
#[test]
fn runtime_record_browser_editable_prompts_accept_pager_hotkey_characters() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let browser = mez_mux::record_browser::RecordBrowser::new(
        "Issues",
        vec![mez_mux::record_browser::RecordBrowserRecord {
            id: "issue-1".to_string(),
            open_command: None,
            title: "First issue".to_string(),
            metadata: Vec::new(),
            markdown: "Body".to_string(),
        }],
        Vec::new(),
    )
    .unwrap();
    let page = browser.render_page();
    service.register_pending_record_browser_overlay(&pane_id, "show-issues", browser, None);
    let response = crate::runtime::runtime_agent_shell_command_response_json(
        &pane_id,
        "/show-issues",
        Some(&crate::runtime::AgentShellCommandOutcome::Display {
            command: "show-issues".to_string(),
            body: page.raw_markdown,
        }),
    );
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    apply_record_browser_input(&mut service, &primary, b"s");
    for input in [b"/".as_slice(), b"q", b"/", b"a"] {
        apply_record_browser_input(&mut service, &primary, input);
    }
    assert!(matches!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .and_then(|record_browser| record_browser.browser.prompt()),
        Some(mez_mux::record_browser::RecordBrowserPrompt::Save { input }) if input == "/q/a"
    ));

    apply_record_browser_input(&mut service, &primary, b"\x1b");
    apply_record_browser_input(&mut service, &primary, b"p");
    for input in [b"/".as_slice(), b"q", b"/", b"*"] {
        apply_record_browser_input(&mut service, &primary, input);
    }
    assert!(matches!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .and_then(|record_browser| record_browser.browser.prompt()),
        Some(mez_mux::record_browser::RecordBrowserPrompt::Filter { field: mez_mux::record_browser::RecordBrowserFilterField::ProjectGlob, input }) if input == "/q/*"
    ));
}

/// Verifies `/list-personalities` renders only safe configured profile metadata
/// and applies the focused row through the pane-local personality command.
///
/// The record browser must preserve deterministic profile ordering, identify
/// the inherited default, and refresh in place after Enter without exposing
/// configured system instructions or opening a record detail page.
#[test]
fn runtime_agent_shell_list_personalities_selects_the_focused_profile() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 16).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_personality = \"alpha\"\n[personalities.alpha]\nname = \"Alpha\"\nsystem_prompt = \"alpha secret instructions\"\nresponse_style = \"terse\"\nplanning_enabled = true\n[personalities.bravo]\nname = \"Bravo\"\ninstructions = \"bravo secret instructions\"\nrouting_enabled = false\n"
                .to_string(),
        }])
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/list-personalities")
        .unwrap();
    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains(r#""command":"list-personalities""#),
        "{response}"
    );
    assert!(!response.contains("secret instructions"), "{response}");
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    let overlay = service.primary_display_overlay().unwrap();
    let browser = &overlay.record_browser.as_ref().unwrap().browser;
    let page = browser.render_page();
    assert!(
        page.raw_markdown.contains(
            "| Personality | Name | Selected | Selection source | Response style | Model profile | Planning | Routing |"
        ),
        "{}",
        page.raw_markdown
    );
    assert!(
        page.raw_markdown
            .contains("| [`alpha`](mez-agent:%2Fpersonality%20alpha) | Alpha | yes | default | terse | inherit | on | inherit |"),
        "{}",
        page.raw_markdown
    );
    assert!(
        page.raw_markdown
            .contains("| [`bravo`](mez-agent:%2Fpersonality%20bravo) | Bravo | no | — | default | inherit | inherit | off |"),
        "{}",
        page.raw_markdown
    );
    assert!(!page.raw_markdown.contains("secret instructions"));
    assert_eq!(browser.active_record_id(), Some("alpha"));

    apply_record_browser_input(&mut service, &primary, b"\x1b[B");
    apply_record_browser_input(&mut service, &primary, b"\r");

    assert_eq!(
        service
            .integration
            .agent_personality_selections()
            .get(&pane_id)
            .map(String::as_str),
        Some("bravo")
    );
    let overlay = service.primary_display_overlay().unwrap();
    let browser = &overlay.record_browser.as_ref().unwrap().browser;
    assert_eq!(browser.active_record_id(), Some("bravo"));
    assert!(!browser.is_detail_view());
    let refreshed = browser.render_page().raw_markdown;
    assert!(
        refreshed.contains("| [`alpha`](mez-agent:%2Fpersonality%20alpha) | Alpha | no | — | terse | inherit | on | inherit |"),
        "{refreshed}"
    );
    assert!(
        refreshed.contains("| [`bravo`](mez-agent:%2Fpersonality%20bravo) | Bravo | yes | pane | default | inherit | inherit | off |"),
        "{refreshed}"
    );
}

/// Verifies `/list-personalities` rejects arguments and renders a useful,
/// non-actionable empty browser when no personality profiles are configured.
#[test]
fn runtime_agent_shell_list_personalities_validates_arguments_and_empty_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();

    let invalid = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"personalities-invalid","method":"agent/shell/command","params":{"idempotency_key":"personalities-invalid","input":"/list-personalities extra"}}"#,
        &primary,
    );
    assert!(
        invalid.contains("list-personalities does not accept arguments"),
        "{invalid}"
    );

    let response = service
        .execute_agent_shell_command(&primary, "/list-personalities")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    let overlay = service.primary_display_overlay().unwrap();
    let browser = &overlay.record_browser.as_ref().unwrap().browser;
    assert!(browser.records().is_empty());
    assert!(
        browser
            .render_page()
            .raw_markdown
            .contains("No personalities are configured. Add profiles under `[personalities]`.")
    );

    apply_record_browser_input(&mut service, &primary, b"\r");
    assert!(
        service
            .integration
            .agent_personality_selections()
            .is_empty()
    );
}

/// Verifies an obsolete pane-local personality selection does not suppress a
/// still-valid configured default or mislabel the effective selection source.
///
/// Config reloads may remove a profile while retained pane metadata still
/// names it. The browser must recover by marking the configured default rather
/// than presenting every row as unselected.
#[test]
fn runtime_agent_shell_list_personalities_falls_back_from_stale_pane_selection() {
    let mut service = test_runtime_service();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_personality = \"alpha\"\n[personalities.alpha]\nname = \"Alpha\"\n"
                .to_string(),
        }])
        .unwrap();
    service
        .integration
        .agent_personality_selections_mut()
        .insert(pane_id.clone(), "removed-profile".to_string());

    let browser = service.personality_record_browser(&pane_id).unwrap();
    let markdown = browser.render_page().raw_markdown;

    assert_eq!(browser.active_record_id(), Some("alpha"));
    assert!(
        markdown.contains(
            "| [`alpha`](mez-agent:%2Fpersonality%20alpha) | Alpha | yes | default | default | inherit | inherit | inherit |"
        ),
        "{markdown}"
    );
}

/// Sends one key sequence through the attached terminal into the active pager.
fn apply_record_browser_input(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
    input: &[u8],
) {
    service
        .apply_attached_terminal_step_plan(
            primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(input.to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
}

/// Builds one pending approval fixture with the fields rendered by the live
/// approval browser and consumed by the canonical decision boundary.
fn pending_approval_request(
    requesting_agent_id: &str,
    pane_id: &str,
    action_summary: &str,
) -> BlockedApprovalRequest {
    BlockedApprovalRequest {
        id: String::new(),
        requesting_agent_id: requesting_agent_id.to_string(),
        pane_id: pane_id.to_string(),
        parent_agent_chain: vec![requesting_agent_id.to_string()],
        action_kind: "shell_command".to_string(),
        action_summary: action_summary.to_string(),
        declared_effects: vec!["process_control".to_string()],
        matched_rules: vec!["default.prompt".to_string()],
        read_scopes: vec![".".to_string()],
        write_scopes: Vec::new(),
        cooperation_mode: None,
        created_at_unix_seconds: None,
        decided_at_unix_seconds: None,
        decided_by_client_id: None,
        state: mez_agent::permissions::BlockedApprovalState::Pending,
        decision: None,
        redirect_instruction: None,
    }
}

/// Verifies `/show-approvals` projects the live cross-agent queue and routes
/// approve-once and deny hotkeys through the canonical decision boundary.
///
/// The selected stable approval id must be decided, the neighboring request
/// must remain pending, and each decision must refresh the retained pager
/// without forwarding input to the pane.
#[test]
fn runtime_agent_shell_show_approvals_decides_selected_stable_ids() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 14).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let first_id = service
        .queue_blocked_approval(pending_approval_request(
            "agent-first",
            &pane_id,
            "cargo check",
        ))
        .unwrap();
    let second_id = service
        .queue_blocked_approval(pending_approval_request(
            "agent-second",
            &pane_id,
            "cargo test --all-targets",
        ))
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/show-approvals")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    let overlay = service.primary_display_overlay().unwrap();
    let page = overlay
        .record_browser
        .as_ref()
        .unwrap()
        .browser
        .render_page();
    assert!(
        page.raw_markdown
            .contains("| Approval | Summary | Pane | Agent | Action |"),
        "{}",
        page.raw_markdown
    );
    assert!(
        page.raw_markdown.contains("cargo check"),
        "{}",
        page.raw_markdown
    );
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains("agent-first"))
    );
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains("agent-second"))
    );
    assert_eq!(overlay.selections.len(), 2);
    assert_eq!(overlay.active_selection_index, Some(0));
    let first_selection = &overlay.selections[0];
    assert_eq!(
        first_selection.command,
        format!("/show-approvals {first_id}")
    );
    assert_eq!(first_selection.width, first_id.len());
    assert!(
        overlay.lines[first_selection.line_index].contains(&first_id),
        "{overlay:?}"
    );
    let second_selection = &overlay.selections[1];
    assert_eq!(
        second_selection.command,
        format!("/show-approvals {second_id}")
    );
    assert_eq!(second_selection.width, second_id.len());
    assert!(
        overlay.lines[second_selection.line_index].contains(&second_id),
        "{overlay:?}"
    );
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 14).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let footer = view.lines.last().cloned().unwrap_or_default();
    assert!(footer.contains("a: approve once"), "{footer}");
    assert!(footer.contains("d: deny"), "{footer}");
    let first_row = view
        .lines
        .iter()
        .position(|line| line.contains(&first_id))
        .expect("approval pager should render the first approval ID link");
    let second_row = view
        .lines
        .iter()
        .position(|line| line.contains(&second_id))
        .expect("approval pager should render the second approval ID link");
    let first_column = display_column_for_fragment(&view.lines[first_row], &first_id);
    let second_column = display_column_for_fragment(&view.lines[second_row], &second_id);
    let first_rendition = styled_line_rendition_at(
        &TerminalStyledLine {
            text: view.lines[first_row].clone(),
            style_spans: view.line_style_spans[first_row].clone(),
            copy_text: None,
        },
        first_column,
    );
    let second_rendition = styled_line_rendition_at(
        &TerminalStyledLine {
            text: view.lines[second_row].clone(),
            style_spans: view.line_style_spans[second_row].clone(),
            copy_text: None,
        },
        second_column,
    );
    assert!(first_rendition.underline, "{view:?}");
    assert_eq!(
        first_rendition.background,
        Some(service.ui_theme().colors.agent_model.background),
        "{view:?}"
    );
    assert_ne!(
        second_rendition.background,
        Some(service.ui_theme().colors.agent_model.background),
        "{view:?}"
    );

    apply_record_browser_input(&mut service, &primary, b"\x1b[B");
    let overlay = service.primary_display_overlay().unwrap();
    assert_eq!(overlay.active_selection_index, Some(1));
    assert_eq!(
        overlay.selections[1].command,
        format!("/show-approvals {second_id}")
    );
    let moved_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 14).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let moved_first_rendition = styled_line_rendition_at(
        &TerminalStyledLine {
            text: moved_view.lines[first_row].clone(),
            style_spans: moved_view.line_style_spans[first_row].clone(),
            copy_text: None,
        },
        first_column,
    );
    let moved_second_rendition = styled_line_rendition_at(
        &TerminalStyledLine {
            text: moved_view.lines[second_row].clone(),
            style_spans: moved_view.line_style_spans[second_row].clone(),
            copy_text: None,
        },
        second_column,
    );
    assert_ne!(
        moved_first_rendition.background,
        Some(service.ui_theme().colors.agent_model.background),
        "{moved_view:?}"
    );
    assert!(moved_second_rendition.underline, "{moved_view:?}");
    assert_eq!(
        moved_second_rendition.background,
        Some(service.ui_theme().colors.agent_model.background),
        "{moved_view:?}"
    );
    apply_record_browser_input(&mut service, &primary, b"a");

    assert_eq!(
        service.blocked_approvals().get(&second_id).unwrap().state,
        mez_agent::permissions::BlockedApprovalState::Approved
    );
    assert_eq!(
        service.blocked_approvals().get(&first_id).unwrap().state,
        mez_agent::permissions::BlockedApprovalState::Pending
    );
    let overlay = service.primary_display_overlay().unwrap();
    let browser = &overlay.record_browser.as_ref().unwrap().browser;
    assert_eq!(browser.active_record_id(), Some(first_id.as_str()));
    assert_eq!(overlay.selections.len(), 1);

    apply_record_browser_input(&mut service, &primary, b"d");

    assert_eq!(
        service.blocked_approvals().get(&first_id).unwrap().state,
        mez_agent::permissions::BlockedApprovalState::Disapproved
    );
    let overlay = service.primary_display_overlay().unwrap();
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains("No pending approvals."))
    );
    assert!(overlay.selections.is_empty());
}

/// Verifies a wrapped approval ID keeps physical link fragments associated
/// with one logical pager record.
///
/// The first link intentionally wraps at a narrow terminal width. Moving down
/// must focus and decide the second approval rather than the second fragment of
/// the first ID, which would otherwise leave the final approval link unfocused.
#[test]
fn runtime_agent_shell_show_approvals_maps_wrapped_links_to_logical_records() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(26, 14).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let first_id = service
        .queue_blocked_approval(pending_approval_request(
            "agent-first",
            &pane_id,
            "cargo check",
        ))
        .unwrap();
    let second_id = service
        .queue_blocked_approval(pending_approval_request(
            "agent-second",
            &pane_id,
            "cargo test",
        ))
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/show-approvals")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    let overlay = service.primary_display_overlay().unwrap();
    let first_fragment_count = overlay
        .selections
        .iter()
        .filter(|selection| selection.logical_id == 0)
        .count();
    assert!(first_fragment_count > 1, "{overlay:?}");
    let second_selection_index = overlay
        .selections
        .iter()
        .position(|selection| selection.logical_id == 1)
        .expect("second approval should retain an ID link");
    assert_eq!(
        overlay.selections[second_selection_index].command,
        format!("/show-approvals {second_id}")
    );

    apply_record_browser_input(&mut service, &primary, b"\x1b[B");

    let overlay = service.primary_display_overlay().unwrap();
    assert_eq!(overlay.active_selection_index, Some(second_selection_index));
    assert_eq!(
        overlay.selections[second_selection_index].logical_id, 1,
        "down should focus the second logical approval rather than a fragment of the first"
    );

    apply_record_browser_input(&mut service, &primary, b"a");

    assert_eq!(
        service.blocked_approvals().get(&second_id).unwrap().state,
        mez_agent::permissions::BlockedApprovalState::Approved
    );
    assert_eq!(
        service.blocked_approvals().get(&first_id).unwrap().state,
        mez_agent::permissions::BlockedApprovalState::Pending
    );
}

/// Verifies one attached-terminal read containing multiple arrow keys advances
/// a retained approval browser once per logical key.
///
/// Terminal reads are byte batches rather than key events. The overlay input
/// boundary must frame every CSI sequence before reduction or the whole batch
/// is ignored and the selector appears immovable in a live terminal.
#[test]
fn runtime_agent_shell_show_approvals_frames_batched_arrow_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 14).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    for agent_id in ["agent-first", "agent-second", "agent-third"] {
        service
            .queue_blocked_approval(pending_approval_request(agent_id, &pane_id, "cargo check"))
            .unwrap();
    }

    let response = service
        .execute_agent_shell_command(&primary, "/show-approvals")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    apply_record_browser_input(&mut service, &primary, b"\x1b[B\x1b[1;2B");

    let overlay = service.primary_display_overlay().unwrap();
    assert_eq!(
        overlay
            .active_selection_index
            .and_then(|index| overlay.selections.get(index))
            .map(|selection| selection.logical_id),
        Some(2)
    );
}

/// Verifies approving the final external action returns control to the pager
/// before the action's network transport begins.
///
/// Approval input runs inside the serialized runtime actor. It must only mark
/// the action running and queue worker-owned transport work; otherwise a slow
/// request prevents the empty approval pager from processing Escape and makes
/// the attached client appear frozen.
#[test]
fn runtime_agent_shell_show_approvals_closes_while_external_action_is_queued() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 14).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"approval-pager-external-action","input":"fetch the release notes"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "fetching release notes".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "fetch the requested release notes".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "fetch-approval".to_string(),
                    rationale: "read the release notes".to_string(),
                    payload: mez_agent::AgentActionPayload::FetchUrl {
                        url: "https://example.test/releases".to_string(),
                        format: None,
                        max_bytes: None,
                    },
                }],
                final_turn: true,
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
    let approval_id = service.blocked_approvals().pending()[0].id.clone();

    let response = service
        .execute_agent_shell_command(&primary, "/show-approvals")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests("%1", &response)
        .unwrap();
    apply_record_browser_input(&mut service, &primary, b"a");

    assert_eq!(
        service.blocked_approvals().get(&approval_id).unwrap().state,
        mez_agent::permissions::BlockedApprovalState::Approved
    );
    assert_eq!(
        service.pending_approved_external_actions(),
        vec![("turn-1".to_string(), "fetch-approval".to_string())]
    );
    assert!(
        service
            .primary_display_overlay()
            .unwrap()
            .lines
            .iter()
            .any(|line| line.contains("No pending approvals."))
    );

    apply_record_browser_input(&mut service, &primary, b"\x1b");

    assert!(service.primary_display_overlay().is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies approval decision keys remain ordinary search text while the
/// retained browser search editor is active.
///
/// Approval-specific shortcuts must not bypass the generic browser input
/// precedence or decide a request while the user is entering a query.
#[test]
fn runtime_agent_shell_show_approvals_preserves_search_input_precedence() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 14).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let approval_id = service
        .queue_blocked_approval(pending_approval_request(
            "agent-search",
            &pane_id,
            "cargo audit",
        ))
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/show-approvals")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    apply_record_browser_input(&mut service, &primary, b"/");
    apply_record_browser_input(&mut service, &primary, b"a");

    assert_eq!(
        service.blocked_approvals().get(&approval_id).unwrap().state,
        mez_agent::permissions::BlockedApprovalState::Pending
    );
    assert_eq!(
        service
            .primary_display_overlay()
            .unwrap()
            .search_input
            .as_deref(),
        Some("a")
    );
}

/// Verifies a record-browser refresh clears an unmatched search status instead
/// of letting it hide the controls for the refreshed approval list.
#[test]
fn runtime_agent_shell_record_browser_refresh_clears_stale_search_status() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 14).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let approval_id = service
        .queue_blocked_approval(pending_approval_request(
            "agent-search-refresh",
            &pane_id,
            "cargo audit",
        ))
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/show-approvals")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    apply_record_browser_input(&mut service, &primary, b"/");
    apply_record_browser_input(&mut service, &primary, b"absent");
    apply_record_browser_input(&mut service, &primary, b"\r");

    assert_eq!(
        service
            .primary_display_overlay()
            .unwrap()
            .search_status
            .as_deref(),
        Some("pattern not found: absent")
    );

    apply_record_browser_input(&mut service, &primary, b"a");

    assert_eq!(
        service.blocked_approvals().get(&approval_id).unwrap().state,
        mez_agent::permissions::BlockedApprovalState::Approved
    );
    let overlay = service.primary_display_overlay().unwrap();
    assert_eq!(overlay.search_input, None);
    assert_eq!(overlay.search_query, None);
    assert_eq!(overlay.search_match, None);
    assert_eq!(overlay.search_status, None);
}

/// Verifies a concurrently settled approval cannot transfer a browser
/// decision to the row that moves into its former list position.
///
/// The stale stable id must be sent through `approval/decide`, its error must
/// remain visible after refresh, and the neighboring request must stay pending.
#[test]
fn runtime_agent_shell_show_approvals_rejects_stale_selected_id() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 14).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let stale_id = service
        .queue_blocked_approval(pending_approval_request(
            "agent-stale",
            &pane_id,
            "cargo check",
        ))
        .unwrap();
    let neighboring_id = service
        .queue_blocked_approval(pending_approval_request(
            "agent-neighbor",
            &pane_id,
            "cargo test",
        ))
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/show-approvals")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    let settle = format!(
        r#"{{"jsonrpc":"2.0","id":"concurrent","method":"approval/decide","params":{{"approval_id":"{stale_id}","decision":"disapprove","idempotency_key":"concurrent-settlement"}}}}"#
    );
    let settle_response = service.dispatch_runtime_control_body(&settle, &primary);
    assert!(settle_response.contains(r#""result""#), "{settle_response}");

    apply_record_browser_input(&mut service, &primary, b"a");

    assert_eq!(
        service
            .blocked_approvals()
            .get(&neighboring_id)
            .unwrap()
            .state,
        mez_agent::permissions::BlockedApprovalState::Pending
    );
    let overlay = service.primary_display_overlay().unwrap();
    let browser = &overlay.record_browser.as_ref().unwrap().browser;
    assert_eq!(browser.active_record_id(), Some(neighboring_id.as_str()));
    assert!(
        overlay.lines.iter().any(|line| line.contains("Error:")),
        "{overlay:?}"
    );
}

/// Verifies `/show-memories` renders a selectable stable-ID table and opens the
/// memory selected with pager arrow keys.
///
/// The list order is backend-defined, so this test reads the rendered stable
/// IDs before moving. One Down-arrow must select the other logical record, and
/// Enter must open that selected record's detail rather than a table fragment.
#[test]
fn runtime_agent_shell_show_memories_opens_arrow_selected_table_record() {
    let root = temp_root("runtime-show-memories-table");
    let _ = fs::remove_dir_all(&root);
    let config_root = root.join("config");
    fs::create_dir_all(&config_root).unwrap();
    let mut service = test_runtime_service();
    service.set_config_root(config_root.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(120, 14).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let store = crate::storage::memory::PersistentMemoryStore::under_config_root(&config_root);
    for (id, updated_at, content) in [
        ("memory-table-first", 10, "first memory detail"),
        ("memory-table-second", 20, "second memory detail"),
    ] {
        store
            .upsert(MemoryRecord::new_with_defaults(
                id,
                mez_agent::memory::MemoryScope::Global,
                updated_at,
                updated_at,
                mez_agent::memory::MemorySource::Agent,
                50,
                content,
            ))
            .unwrap();
    }

    let response = service
        .execute_agent_shell_command(&primary, "/show-memories --scope global")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    let overlay = service.primary_display_overlay().unwrap();
    let page = overlay
        .record_browser
        .as_ref()
        .unwrap()
        .browser
        .render_page();
    assert!(
        page.raw_markdown
            .contains("| UUID | Summary | Scope | Kind | State | Priority | Updated |"),
        "{}",
        page.raw_markdown
    );
    assert!(
        page.raw_markdown.contains("first memory detail"),
        "{}",
        page.raw_markdown
    );
    assert_eq!(
        overlay
            .selections
            .iter()
            .map(|selection| selection.logical_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
    let second_selection_index = overlay
        .selections
        .iter()
        .position(|selection| selection.logical_id == 1)
        .unwrap();
    assert_eq!(overlay.active_selection_index, Some(0));
    let second_id = overlay.record_browser.as_ref().unwrap().browser.records()[1]
        .id
        .clone();
    assert_eq!(
        overlay.selections[second_selection_index].command,
        format!("/show-memories {second_id}")
    );
    let expected_detail = store.inspect(&second_id).unwrap().content;

    apply_record_browser_input(&mut service, &primary, b"\x1b[B");
    let overlay = service.primary_display_overlay().unwrap();
    assert_eq!(overlay.active_selection_index, Some(second_selection_index));
    assert_eq!(
        overlay
            .record_browser
            .as_ref()
            .unwrap()
            .browser
            .active_record_id(),
        Some(second_id.as_str())
    );

    apply_record_browser_input(&mut service, &primary, b"\r");
    let overlay = service.primary_display_overlay().unwrap();
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains(&expected_detail)),
        "{overlay:?}"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies the memory record browser deletes its selected durable record and
/// refreshes the same pager to an empty, valid selection state.
#[test]
fn runtime_agent_shell_show_memories_deletes_the_selected_record() {
    let root = temp_root("runtime-show-memories-delete");
    let _ = fs::remove_dir_all(&root);
    let config_root = root.join("config");
    fs::create_dir_all(&config_root).unwrap();
    let mut service = test_runtime_service();
    service.set_config_root(config_root.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 14).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let store = crate::storage::memory::PersistentMemoryStore::under_config_root(&config_root);
    store
        .upsert(MemoryRecord::new_with_defaults(
            "memory-delete",
            mez_agent::memory::MemoryScope::Global,
            10,
            10,
            mez_agent::memory::MemorySource::Agent,
            50,
            "delete this memory from the pager",
        ))
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/show-memories memory-delete")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    apply_record_browser_input(&mut service, &primary, b"d");

    assert!(store.inspect("memory-delete").is_err());
    let overlay = service.primary_display_overlay().unwrap();
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains("No memories found."))
    );
    assert_eq!(overlay.active_selection_index, None);
    let _ = fs::remove_dir_all(root);
}

/// Verifies issue pager deletion reports an open dependent in-place, then
/// succeeds after that dependent is resolved without closing the pager.
#[test]
fn runtime_agent_shell_show_issues_blocks_open_dependents_then_deletes() {
    let root = temp_root("runtime-show-issues-delete");
    let _ = fs::remove_dir_all(&root);
    let config_root = root.join("config");
    fs::create_dir_all(&config_root).unwrap();
    let mut service = test_runtime_service();
    service.set_config_root(config_root.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[issues]\nenabled = true\n".to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 14).unwrap(), 120)
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
    let prerequisite = store
        .add_issue(
            project.clone(),
            mez_agent::issues::IssueKind::Task,
            "Pager prerequisite".to_string(),
            None,
            None,
            10,
        )
        .unwrap();
    let dependent = store
        .add_issue_with_dependencies(
            mez_agent::issues::NewIssueRecord {
                project: project.clone(),
                kind: mez_agent::issues::IssueKind::Task,
                title: "Open dependent".to_string(),
                body: None,
                notes: None,
                depends_on: vec![prerequisite.id.clone()],
            },
            20,
        )
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, &format!("/show-issues {}", prerequisite.id))
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    apply_record_browser_input(&mut service, &primary, b"d");

    assert!(
        store
            .get_issue(project.clone(), prerequisite.id.clone())
            .unwrap()
            .is_some()
    );
    let overlay = service.primary_display_overlay().unwrap();
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains(&dependent.id))
    );

    store
        .update_issue(
            project.clone(),
            dependent.id,
            mez_agent::issues::IssueUpdate {
                state: Some(mez_agent::issues::IssueState::Resolved),
                ..mez_agent::issues::IssueUpdate::default()
            },
            30,
        )
        .unwrap();
    apply_record_browser_input(&mut service, &primary, b"d");

    assert!(store.get_issue(project, prerequisite.id).unwrap().is_none());
    let overlay = service.primary_display_overlay().unwrap();
    assert!(
        !overlay
            .lines
            .iter()
            .any(|line| line.contains("Pager prerequisite"))
    );
    let _ = fs::remove_dir_all(root);
}
