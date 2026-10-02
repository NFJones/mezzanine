//! Exact result identity and view-only retained activity disclosure regressions.

use super::*;

/// Result disclosure retains source beyond the bounded live preview; replay
/// reproduces that preview and opening/back navigation leaves history intact.
#[test]
fn activity_disclosure_retains_source_and_replays_only_bounded_preview() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let store = AgentTranscriptStore::new(temp_root("activity-result-source"));
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let action = mez_agent::AgentAction {
        id: "same-action".into(),
        payload: mez_agent::AgentActionPayload::McpCall {
            server: "fixture".into(),
            tool: "read".into(),
            arguments_json: "{}".into(),
        },
    };
    let result = mez_agent::ActionResult {
        protocol: "maap/1".into(),
        turn_id: "turn".into(),
        agent_id: "agent-%1".into(),
        action_id: action.id.clone(),
        action_type: "mcp_call",
        status: ActionStatus::Succeeded,
        content: Vec::new(),
        structured_content_json: None,
        permission_evaluation: None,
        is_error: false,
        error: None,
    };
    let text = format!("{}\nretained-tail-marker", "row\n".repeat(300));
    service
        .append_ordered_activity_result(
            "%1",
            ("response-one", 0, None),
            &action,
            &result,
            &text,
            Default::default(),
        )
        .unwrap();
    let entries = store.inspect_presentation(&conversation).unwrap();
    assert_eq!(entries.len(), 1);
    let source = crate::storage::transcript::activity::ActivitySource::decode(
        entries[0].source_text.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(source.response_id, "response-one");
    assert!(source.source.contains("retained-tail-marker"));
    assert!(
        !source
            .preview_source
            .as_deref()
            .unwrap()
            .contains("retained-tail-marker")
    );
    let original = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 20).unwrap(), 1000).unwrap(),
    );
    service
        .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
        .unwrap();
    let replay = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(original.iter().any(|line| line.contains("mcp call")));
    assert!(replay.iter().any(|line| line.contains("mcp call")));
    assert!(
        !replay
            .iter()
            .any(|line| line.contains("retained-tail-marker"))
    );
    let read = crate::runtime::commands::read_context_browser_for_command(
        &store,
        &conversation,
        "%1",
        "/show-context activity",
    )
    .unwrap();
    let mut browser = read.browser;
    assert_eq!(browser.records().len(), 1);
    assert!(
        browser.records()[0]
            .markdown
            .contains("retained-tail-marker")
    );
    browser
        .apply_action(mez_mux::record_browser::RecordBrowserAction::OpenActive)
        .unwrap();
    assert!(browser.is_detail_view());
    browser
        .apply_action(mez_mux::record_browser::RecordBrowserAction::BackToList)
        .unwrap();
    assert!(!browser.is_detail_view());
    assert_eq!(store.inspect_presentation(&conversation).unwrap(), entries);
    let page = browser.render_page();
    service.register_pending_record_browser_overlay("%1", "show-context", browser, None);
    let response = crate::runtime::runtime_agent_shell_command_response_json(
        "%1",
        "/show-context activity",
        Some(&crate::runtime::AgentShellCommandOutcome::Display {
            command: "show-context".into(),
            body: page.raw_markdown,
        }),
    );
    service
        .set_agent_prompt_response_display_output_for_tests("%1", &response)
        .unwrap();
    let mouse_action = service.primary_display_overlay().unwrap().selections[0].action_id;
    let screen_before = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    service
        .execute_primary_display_overlay_action(&primary, mouse_action)
        .unwrap();
    assert!(service.active_record_browser_is_detail());
    let keyboard_step = |input: &[u8]| AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(input.to_vec())],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &keyboard_step(b"\x1b"))
        .unwrap();
    assert!(!service.active_record_browser_is_detail());
    // Opening/back rebuilt the overlay generation. Reusing an old mouse id
    // must not reopen a component from a replaced projection.
    service
        .execute_primary_display_overlay_action(&primary, mouse_action)
        .unwrap();
    assert!(!service.active_record_browser_is_detail());
    service
        .apply_attached_terminal_step_plan(&primary, &keyboard_step(b"\r"))
        .unwrap();
    assert!(service.active_record_browser_is_detail());
    assert_eq!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines(),
        screen_before
    );
    assert_eq!(store.inspect_presentation(&conversation).unwrap(), entries);
}

/// Repeated action ids are separated by exact response identity rather than
/// timestamps. Forking changes only conversation ownership of retained source.
#[test]
fn activity_fork_preserves_response_identity_and_exact_source() {
    use crate::storage::transcript::activity::{
        ACTIVITY_CONTENT_TYPE, ActivityComponentKind, ActivityIntent, ActivitySource,
    };
    let store = AgentTranscriptStore::new(temp_root("activity-fork-identity"));
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "source".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn".into(),
            agent_id: "agent".into(),
            pane_id: "%1".into(),
            content: "request".into(),
        })
        .unwrap();
    for (index, response) in ["response-one", "response-one", "response-two"]
        .into_iter()
        .enumerate()
    {
        let source = ActivitySource {
            version: 1,
            conversation_id: "source".into(),
            turn_id: "turn".into(),
            response_id: response.into(),
            action_id: Some("same".into()),
            action_ordinal: Some(0),
            transaction: None,
            kind: ActivityComponentKind::Result,
            status: "succeeded".into(),
            content_type: "text/plain".into(),
            source: "retained 雪\r\n".into(),
            preview_source: None,
            intent: ActivityIntent::default(),
        };
        store
            .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
                conversation_id: "source".into(),
                sequence: index as u64 + 1,
                created_at_unix_seconds: 1,
                pane_id: "%1".into(),
                turn_id: Some("turn".into()),
                terminal_width: 80,
                style_names: vec!["status".into()],
                display_lines: vec!["retained".into()],
                copy_lines: Vec::new(),
                ansi_text: None,
                source_text: Some(source.encode().unwrap()),
                source_content_type: Some(ACTIVITY_CONTENT_TYPE.into()),
            })
            .unwrap();
    }
    store.fork("source", "fork", 2).unwrap();
    let original = store.inspect_presentation("source").unwrap();
    let forked = store.inspect_presentation("fork").unwrap();
    assert_eq!(forked.len(), 3);
    for (original, forked) in original.iter().zip(&forked) {
        let mut source = ActivitySource::decode(original.source_text.as_deref().unwrap()).unwrap();
        source.conversation_id = "fork".into();
        assert_eq!(
            ActivitySource::decode(forked.source_text.as_deref().unwrap()).unwrap(),
            source
        );
    }
    let read = crate::runtime::commands::read_context_browser_for_command(
        &store,
        "fork",
        "%1",
        "/show-context activity",
    )
    .unwrap();
    assert_eq!(read.browser.records().len(), 2);
    assert!(read.browser.records()[0].markdown.contains("Component 2"));
    assert_ne!(
        read.browser.records()[0].metadata,
        read.browser.records()[1].metadata
    );
    // Historical producer %1 must not hide a resumed or forked conversation
    // inspected from its new attachment %2. No unrelated conversation is read.
    for conversation in ["source", "fork"] {
        let list = crate::runtime::commands::read_context_browser_for_command(
            &store,
            conversation,
            "%2",
            "/show-context activity",
        )
        .unwrap();
        assert_eq!(list.browser.records().len(), 2);
        let detail = crate::runtime::commands::read_context_browser_for_command(
            &store,
            conversation,
            "%2",
            "/show-context activity 2",
        )
        .unwrap();
        assert!(detail.browser.is_detail_view());
        assert_eq!(detail.browser.active_record_id(), Some("1"));
        assert!(detail.markdown.contains("retained 雪"));
    }
    let unrelated = crate::runtime::commands::read_context_browser_for_command(
        &store,
        "unrelated",
        "%2",
        "/show-context activity",
    )
    .unwrap();
    assert!(unrelated.browser.records().is_empty());
    assert!(
        crate::runtime::commands::read_context_browser_for_command(
            &store,
            "unrelated",
            "%2",
            "/show-context activity 2",
        )
        .is_err()
    );
}
