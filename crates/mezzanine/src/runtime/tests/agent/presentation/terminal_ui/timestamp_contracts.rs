//! Settings-fenced workers, server-rendered client snapshots and entry families.
//!
//! Frozen synthetic occurrence instants remain presentation-only. These tests
//! exercise existing actor/worker and control paths without a provider request
//! or new wire fields, clocks, durable event stores or process-global settings.

use super::timestamp_settings::replace_timestamp_settings;
use super::*;

/// Every visible structured producer uses the same owned occurrence/frame
/// path. Equal clock values cannot collapse user/parent/peer/thinking/say,
/// status/error/macro, command-header or result-preview entries. Markdown tables,
/// fences, diffs, Unicode and unbroken tokens add rows, never extra clocks.
#[test]
fn runtime_log_timestamps_structured_families_share_owned_frame_projection() {
    let mut service = test_runtime_service_with_size(Size::new(100, 80).unwrap());
    replace_timestamp_settings(&mut service, true, 80, "");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(100, 70).unwrap(), 300).unwrap(),
    );
    service
        .append_agent_user_prompt_to_terminal_buffer("%1", "user entry\nsecond user row")
        .unwrap();
    service
        .append_agent_parent_prompt_to_terminal_buffer("%1", "parent entry\nsecond parent row")
        .unwrap();
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "text/plain",
            "peer entry\nsecond peer row",
        )
        .unwrap();
    service
        .append_agent_thinking_text_to_terminal_buffer("%1", "thinking entry\nsecond rationale row")
        .unwrap();
    service
        .append_agent_status_text_to_terminal_buffer("%1", "agent: status entry")
        .unwrap();
    service
        .append_agent_error_text_to_terminal_buffer("%1", "error entry")
        .unwrap();
    service
        .append_agent_macro_status_to_terminal_buffer("%1", "macro", Some(0), 2, "running")
        .unwrap();
    service
        .append_agent_macro_error_to_terminal_buffer("%1", "macro", 1, 2, "failed")
        .unwrap();
    service
        .append_agent_command_preview_to_terminal_buffer(
            "%1",
            "printf 'intent only, not executor start'",
        )
        .unwrap();
    service.append_agent_assistant_content_to_terminal_buffer("%1", "| col | value |\n| --- | --- |\n| 雪 | 01:02:03 authored |\n\n```rust\nlet value = 1;\n```", "text/markdown").unwrap();
    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new",
            "text/x-diff",
        )
        .unwrap();
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", &"longtoken".repeat(25))
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "family-action".into(),
        payload: mez_agent::AgentActionPayload::McpCall {
            server: "fixture".into(),
            tool: "inspect".into(),
            arguments_json: "{}".into(),
        },
    };
    service
        .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
        .unwrap();
    let result = mez_agent::ActionResult {
        protocol: "maap/1".into(),
        turn_id: "fixture-turn".into(),
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
    service
        .append_agent_action_result_text_to_terminal_buffer(
            "%1",
            &action,
            &result,
            "result entry\nsecond result row",
        )
        .unwrap();
    let screen = service.agent_pane_screen("%1").unwrap();
    let canonical = screen.normal_styled_content_lines();
    let mut identities = std::collections::BTreeMap::new();
    for entry in canonical.iter().filter_map(|row| row.presentation.as_ref()) {
        assert_eq!(entry.occurrence.unix_millis, Some(3_723_000));
        *identities.entry(entry.id.clone()).or_insert(0usize) += usize::from(entry.first_row);
    }
    assert!(
        identities.len() >= 12,
        "family inventory must be nonzero and diverse"
    );
    assert!(identities.values().all(|count| *count == 1));
    let copy = CopyMode::from_screen(screen, 70).unwrap();
    assert_eq!(
        copy.lines()
            .iter()
            .filter(|row| row.starts_with("01:02:03 │ "))
            .count(),
        identities.len()
    );
    assert!(
        copy.lines()
            .iter()
            .all(|row| mez_terminal::terminal_text_width(
                row,
                mez_terminal::terminal_emoji_width()
            ) <= 80)
    );
    assert!(
        canonical
            .iter()
            .all(|row| !row.text.starts_with("01:02:03 │ "))
    );
}

/// Content capture uses the canonical conversation-bound surface, not the
/// decorative client view or hidden process rows. Turning the setting on/off
/// preserves captured content and style coordinates byte-for-byte, including
/// authored timestamp-looking text; a real visual frame still shows the clock.
#[test]
fn runtime_log_timestamps_content_capture_and_visual_frames_are_distinct() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    let primary = service
        .attach_primary("capture", true, Size::new(80, 32).unwrap(), 120)
        .unwrap();
    let mut process = TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap();
    process.feed(b"hidden process fixture\r\n");
    service.set_process_pane_screen("%1", process);
    replace_timestamp_settings(&mut service, false, 80, "");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "01:02:03 authored capture")
        .unwrap();
    let request = r#"{"jsonrpc":"2.0","id":"timestamp-capture","method":"pane/capture","params":{"target":{"pane_id":"%1"},"range":{"origin":"visible","start":"start","end":"end"}}}"#;
    let original: serde_json::Value =
        serde_json::from_str(&service.dispatch_runtime_control_body(request, &primary)).unwrap();
    assert!(original.to_string().contains("authored capture"));
    assert!(!original.to_string().contains("hidden process fixture"));
    for enabled in [true, false, true] {
        replace_timestamp_settings(&mut service, enabled, 80, "");
        let capture: serde_json::Value =
            serde_json::from_str(&service.dispatch_runtime_control_body(request, &primary))
                .unwrap();
        assert_eq!(capture, original);
        let view = service
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 32).unwrap(),
                &TerminalClientLoopConfig::default(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            view.lines
                .iter()
                .filter(|row| row.contains("01:02:03 │ "))
                .count(),
            usize::from(enabled)
        );
    }
}

/// A resize result captured before an unchanged-size setting replacement must
/// be rejected atomically. Fresh work uses the current policy and retained
/// occurrence evidence; reconstructing source never samples the injected clock.
#[test]
fn runtime_log_timestamps_settings_fence_delayed_resize_worker() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    replace_timestamp_settings(&mut service, false, 80, "");
    let store = AgentTranscriptStore::new(temp_root("timestamp-resize-settings"));
    service.set_agent_transcript_store(store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            "**original source**",
            "text/markdown",
        )
        .unwrap();
    service
        .apply_pane_resize_completion_event("%1", Size::new(40, 20).unwrap())
        .unwrap();
    let captured = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .unwrap();
    replace_timestamp_settings(&mut service, true, 80, "");
    let before = service.agent_pane_screen("%1").unwrap().clone();
    let stale = RuntimeSessionService::build_agent_presentation_resize(captured)
        .unwrap()
        .unwrap();
    assert!(
        !service
            .apply_agent_presentation_resize_result(stale)
            .unwrap()
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &before);
    service.presentation.occurrence_clock_millis = Some(Some(99_999_000));
    let current = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .unwrap();
    let current = RuntimeSessionService::build_agent_presentation_resize(current)
        .unwrap()
        .unwrap();
    assert!(
        service
            .apply_agent_presentation_resize_result(current)
            .unwrap()
    );
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    assert!(
        rows.iter()
            .filter_map(|row| row.presentation.as_ref())
            .all(|entry| entry.occurrence.unix_millis == Some(3_723_000))
    );
}

/// A streamed component keeps its first-observation stamp while a captured
/// projection is rejected after same-size settings replacement. Fresh OFF and
/// ON work retains source identity/time and never imports a new actor clock.
#[test]
fn runtime_log_timestamps_settings_fence_streaming_and_keep_original_stamp() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    replace_timestamp_settings(&mut service, true, 80, "");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 200).unwrap(),
    );
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Final,
            content_type: "text/markdown".into(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "**first observation**".into(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "timestamp-stream", &event)
            .unwrap();
    }
    let captured = service
        .take_agent_streaming_say_projection_work("%1", "timestamp-stream")
        .unwrap()
        .unwrap();
    let stale = RuntimeSessionService::build_agent_streaming_say_projection(captured).unwrap();
    replace_timestamp_settings(&mut service, false, 80, "");
    assert!(
        !service
            .apply_agent_streaming_say_projection_result(stale)
            .unwrap()
    );
    for enabled in [false, true] {
        replace_timestamp_settings(&mut service, enabled, 80, "");
        service.presentation.occurrence_clock_millis = Some(Some(99_999_000));
        let work = service
            .take_agent_streaming_say_projection_work("%1", "timestamp-stream")
            .unwrap()
            .unwrap();
        let fresh = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(fresh)
                .unwrap()
        );
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        let entries = rows
            .iter()
            .filter_map(|row| row.presentation.as_ref())
            .collect::<Vec<_>>();
        assert!(!entries.is_empty());
        assert!(
            entries
                .iter()
                .all(|entry| entry.occurrence.unix_millis == Some(3_723_000))
        );
        assert_eq!(entries.iter().filter(|entry| entry.first_row).count(), 1);
    }
}

/// Two primaries and a smaller observer receive server-formatted rows/styles.
/// Inline and immutable worker frames agree on the timestamp's cells/rendition;
/// changing the actor clock cannot change a retained entry. Observer clipping
/// retains the server's authoritative width rather than reformatting time.
#[test]
fn runtime_log_timestamps_two_primaries_observer_and_snapshot_styles_match() {
    let mut service = test_runtime_service_with_size(Size::new(80, 32).unwrap());
    let size = Size::new(80, 32).unwrap();
    let first = service.attach_primary("first", true, size, 120).unwrap();
    let second = service.attach_primary("second", true, size, 121).unwrap();
    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();
    replace_timestamp_settings(&mut service, true, 80, "");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 200).unwrap(),
    );
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "stable client frame")
        .unwrap();
    let mut expected = None;
    for (client, role, viewport) in [
        (first, ClientViewRole::Primary, size),
        (second, ClientViewRole::Primary, size),
        (
            observer,
            ClientViewRole::Observer,
            Size::new(40, 16).unwrap(),
        ),
    ] {
        replace_timestamp_settings(&mut service, true, 80, "");
        service.prepare_client_render(&client, role).unwrap();
        let config = service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap();
        let inline = service
            .render_client_view_for_client_with_resolved_config(&client, role, viewport, &config)
            .unwrap()
            .unwrap();
        let snapshot = service
            .capture_client_render_snapshot(&client, role, viewport, &config)
            .unwrap();
        let base = crate::runtime::compose_client_render_snapshot(snapshot).unwrap();
        let (worker, _) = service
            .complete_client_render_snapshot(role, &config, base)
            .unwrap();
        let worker = worker.unwrap();
        let row = inline
            .lines
            .iter()
            .position(|row| row.contains("01:02:03 │ "))
            .unwrap();
        assert_eq!(worker.lines[row], inline.lines[row]);
        assert_eq!(worker.line_style_spans[row], inline.line_style_spans[row]);
        let projection = (
            inline.lines[row].clone(),
            inline.line_style_spans[row].clone(),
        );
        if let Some(expected) = expected.as_ref() {
            assert_eq!(&projection, expected);
        } else {
            expected = Some(projection);
        }
        if role == ClientViewRole::Observer {
            assert!(inline.requires_client_scroll);
        }
        // The wire still transports server rows and styles. Replay production
        // full/delta bytes into a fresh modeled client terminal; the full draw
        // after OFF is the independent cell/rendition oracle for the delta.
        use mez_mux::attached_client::{
            AttachedTerminalModeTransitions, AttachedTerminalOutputFrameState,
            encode_attached_terminal_output_frame_with_styles,
            encode_attached_terminal_output_update_frame_with_verified_size,
        };
        use mez_mux::presentation::{
            AttachedTerminalOutputModes, compose_client_presentation_with_styles,
        };
        let (lines, spans) = compose_client_presentation_with_styles(&inline, None);
        let modes = AttachedTerminalOutputModes::default();
        let mut actual = TerminalScreen::new(viewport, 100).unwrap();
        actual.feed(&encode_attached_terminal_output_frame_with_styles(
            &lines, &spans, None, modes,
        ));
        let previous = AttachedTerminalOutputFrameState::new_with_modes(&lines, &spans, modes);
        replace_timestamp_settings(&mut service, false, 80, "");
        let off = service
            .render_client_view(role, viewport, &TerminalClientLoopConfig::default())
            .unwrap()
            .unwrap();
        let (off_lines, off_spans) = compose_client_presentation_with_styles(&off, None);
        let delta = encode_attached_terminal_output_update_frame_with_verified_size(
            &off_lines,
            &off_spans,
            None,
            modes,
            Some(&previous),
            AttachedTerminalModeTransitions::default(),
            Some(viewport),
        );
        actual.feed(&delta);
        let mut oracle = TerminalScreen::new(viewport, 100).unwrap();
        oracle.feed(&encode_attached_terminal_output_frame_with_styles(
            &off_lines, &off_spans, None, modes,
        ));
        assert_eq!(
            actual.visible_styled_lines(),
            oracle.visible_styled_lines(),
            "role={role:?}"
        );
        service.presentation.occurrence_clock_millis = Some(Some(99_999_000));
    }
}
