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
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let browser = mez_mux::record_browser::RecordBrowser::new(
        "Issues",
        vec![mez_mux::record_browser::RecordBrowserRecord {
            id: "issue-1".to_string(),
            open_command: Some("/show-issues issue-1".to_string()),
            title: "A record title with enough words to occupy several physical rows".to_string(),
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
    store
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
    assert!(
        !overlay_view
            .lines
            .iter()
            .any(|line| line.contains("Cross-project issue")),
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
            .lines
            .iter()
            .any(|line| line.contains("Cross-project issue"))
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
            .lines
            .iter()
            .any(|line| line.contains("Cross-project issue"))
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
        ])
        .unwrap();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: "other-conversation".to_string(),
            sequence: 1,
            created_at_unix_seconds: 3,
            role: TranscriptRole::User,
            turn_id: "other-turn".to_string(),
            agent_id: "agent-%2".to_string(),
            pane_id: "%2".to_string(),
            content: "other pane context".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries(&pane_id, 2)
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/show-context")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    let overlay = service.primary_display_overlay().unwrap();
    let first_line = overlay
        .lines
        .iter()
        .position(|line| line.contains("first context entry"))
        .unwrap();
    let second_line = overlay
        .lines
        .iter()
        .position(|line| line.contains("second context entry"))
        .unwrap();
    assert!(first_line < second_line);
    assert!(
        !overlay
            .lines
            .iter()
            .any(|line| line.contains("other pane context"))
    );

    for input in [b"\x1b[B".as_slice(), b"d".as_slice()] {
        service
            .apply_attached_terminal_step_plan(
                &primary,
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
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains("first context entry"))
    );
    assert!(
        !overlay
            .lines
            .iter()
            .any(|line| line.contains("second context entry"))
    );
    assert!(transcript_store.inspect("other-conversation").is_ok());
    let _ = fs::remove_dir_all(root);
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

    apply_record_browser_input(&mut service, &primary, b"\x1b[B");
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
            .any(|line| line.contains("No records found."))
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
