//! CLI terminal protocol tests.

use super::*;

/// Verifies that control-socket attach clients consume cursor metadata emitted
/// by terminal step/view responses. These clients render line batches locally,
/// so the response parser must carry cursor placement into attached-terminal
/// output modes rather than hiding the cursor by default.
#[test]
fn terminal_step_response_output_modes_parse_cursor_metadata() {
    let modes = terminal_step_response_output_modes(
        r#"{"jsonrpc":"2.0","id":1,"result":{"view":{"cursor":{"row":2,"column":7,"visible":true,"style":"bar","blink":true,"blink_interval_ms":250},"output_modes":{"application_keypad":true,"bracketed_paste":true,"host_mouse_reporting":false,"animation_refresh_interval_ms":180},"lines":["pane"]}}}"#,
    )
    .unwrap()
    .unwrap();

    assert_eq!(modes.cursor_row, 2);
    assert_eq!(modes.cursor_column, 7);
    assert!(modes.cursor_visible);
    assert_eq!(
        modes.cursor_style,
        mez_mux::presentation::TerminalCursorStyle::Bar
    );
    assert!(modes.cursor_blink);
    assert_eq!(modes.cursor_blink_interval_ms, 250);
    assert!(modes.application_keypad);
    assert!(modes.bracketed_paste);
    assert!(!modes.host_mouse_reporting);
    assert_eq!(modes.animation_refresh_interval_ms, 180);
}

/// Verifies that control-socket attach clients parse SGR style spans from the
/// runtime presentation payload. Without this, detachable attach renders the
/// same terminal text but silently drops color and text attributes.
#[test]
fn terminal_step_response_line_style_spans_parse_color_and_attributes() {
    let spans = terminal_step_response_line_style_spans(
        r#"{"jsonrpc":"2.0","id":1,"result":{"view":{"lines":["styled"],"line_style_spans":[[{"start":1,"length":3,"rendition":{"bold":true,"dim":false,"italic":true,"underline":true,"double_underline":false,"strikethrough":true,"inverse":false,"hidden":false,"foreground":{"kind":"rgb","red":1,"green":2,"blue":3},"background":{"kind":"indexed","index":4}}}]]}}}"#,
    )
    .unwrap();

    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0][0].start, 1);
    assert_eq!(spans[0][0].length, 3);
    assert!(spans[0][0].rendition.bold);
    assert!(spans[0][0].rendition.italic);
    assert!(spans[0][0].rendition.underline);
    assert!(spans[0][0].rendition.strikethrough);
    assert_eq!(
        spans[0][0].rendition.foreground,
        Some(mez_terminal::TerminalColor::Rgb(1, 2, 3))
    );
    assert_eq!(
        spans[0][0].rendition.background,
        Some(mez_terminal::TerminalColor::Indexed(4))
    );
}

/// Verifies that the detachable control-socket attach request preserves SGR
/// mouse packets as raw byte arrays for runtime-side hit testing, application
/// forwarding, legacy mouse translation, and pane resize handling.
#[test]
fn terminal_step_control_request_preserves_sgr_mouse_bytes() {
    let client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    let mouse = b"\x1b[<0;12;5M";
    let request =
        terminal_step_control_request(3, &client_id, Size::new(80, 24).unwrap(), mouse, true);
    let parsed: serde_json::Value = serde_json::from_str(&request).unwrap();
    let bytes = parsed
        .get("params")
        .and_then(|params| params.get("input_bytes"))
        .and_then(serde_json::Value::as_array)
        .unwrap()
        .iter()
        .map(|value| value.as_u64().unwrap() as u8)
        .collect::<Vec<_>>();

    assert_eq!(bytes, mouse);
}

/// Verifies that detachable control-socket primary attachment can run its
/// foreground terminal loop through the Tokio terminal IO boundary. This keeps
/// the legacy control protocol surface available while ensuring terminal
/// readiness, presentation entry, frame output, and clean hangup handling no
/// longer depend on the synchronous fd polling trait.
#[tokio::test(flavor = "current_thread")]
async fn control_socket_primary_attach_loop_uses_async_terminal_io() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/step")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("render"))
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("input_bytes"))
                .and_then(serde_json::Value::as_array)
                .and_then(|bytes| bytes.first())
                .and_then(serde_json::Value::as_u64),
            Some(u64::from(b'x'))
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-step-0","result":{"input_bytes":1,"application":{"forwarded_bytes":0,"mux_actions_applied":1,"mouse_actions_reported":0,"agent_prompt_inputs_applied":0,"view_refresh_required":true,"full_redraw_required":true,"unsupported_actions":[]},"view":null,"ui_theme":null}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["detached async"],"line_style_spans":[[]],"cursor":{"row":0,"column":14,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
    });

    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_input(b"x".to_vec());

    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    run_control_socket_attached_primary_client_loop_async(
        &mut client_stream,
        &mut io,
        primary_client_id,
        Size::new(80, 24).unwrap(),
    )
    .await
    .unwrap();
    server.join().unwrap();

    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 1);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, vec!["detached async"]);
    assert_eq!(io.written_frames[0].modes.cursor_column, 14);
    assert!(io.written_frames[0].modes.cursor_visible);
}

/// Verifies terminal-step response parsing keeps the full-redraw signal
/// separate from the basic view-refresh signal.
///
/// Full redraws must invalidate the attached client's retained output frame
/// before rendering. This regression protects the control-socket attach path
/// from collapsing the two runtime signals into a single boolean and then
/// redrawing against stale frame state.
#[test]
fn terminal_step_response_refresh_requirement_preserves_full_redraw() {
    let refresh = terminal_step_response_refresh_requirement(
        r#"{"jsonrpc":"2.0","id":"cli-terminal-step-0","result":{"application":{"view_refresh_required":false,"full_redraw_required":true}}}"#,
    )
    .unwrap();

    assert!(refresh.view_refresh_required);
    assert!(refresh.full_redraw_required);
}

/// Verifies a terminal step response stops the primary attach loop without a
/// follow-up view request because the daemon has already begun shutdown.
#[tokio::test(flavor = "current_thread")]
async fn control_socket_primary_attach_loop_stops_after_terminated_step() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    server_stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/step")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-step-0","result":{"application":{"view_refresh_required":true,"full_redraw_required":true},"session_terminated":true}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
        let mut unexpected = [0u8; 256];
        match server_stream.read(&mut unexpected) {
            Ok(0) => {}
            Ok(read) => panic!(
                "unexpected follow-up view request: {}",
                String::from_utf8_lossy(&unexpected[..read])
            ),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("unexpected server read error: {error}"),
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_input(b"exit\n".to_vec());
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    run_control_socket_attached_primary_client_loop_async(
        &mut client_stream,
        &mut io,
        primary_client_id,
        Size::new(80, 24).unwrap(),
    )
    .await
    .unwrap();
    server.join().unwrap();
    assert_eq!(io.written_frames.len(), 0);
    assert_eq!(io.invalidated_output_frames, 0);
}

/// Verifies a light terminal-step refresh requests a new view without
/// invalidating the retained output frame.
///
/// Focus changes need a fresh attached view for cursor and active-frame state,
/// but they should still use the differential renderer. This protects remote
/// terminal sessions from unnecessary full-screen clears during pane navigation.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_refreshes_without_invalidating_for_light_step() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-view-0")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["initial"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();

        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/step")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-step-1")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-step-1","result":{"input_bytes":1,"application":{"forwarded_bytes":0,"mux_actions_applied":1,"mouse_actions_reported":0,"agent_prompt_inputs_applied":0,"view_refresh_required":true,"full_redraw_required":false,"unsupported_actions":[]},"view":null,"ui_theme":null}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();

        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-view-1")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-1","result":{"view":{"lines":["focused"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
    });

    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_input(b"x".to_vec());
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 0);
    assert_eq!(io.written_frames.len(), 2);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
    assert_eq!(io.written_frames[1].lines, vec!["focused"]);
}

/// Verifies that once the initial attach redraw has already been satisfied,
/// a later primary-input step can stay input-only when the runtime reports no
/// explicit refresh requirement for that input.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_skips_view_after_input_without_refresh_request() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    server_stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["initial"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/step")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("render"))
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-step-0","result":{"input_bytes":1,"application":{"forwarded_bytes":1,"mux_actions_applied":0,"mouse_actions_reported":0,"agent_prompt_inputs_applied":0,"view_refresh_required":false,"full_redraw_required":false,"unsupported_actions":[]},"view":null,"ui_theme":null}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
        let mut unexpected = [0u8; 256];
        match server_stream.read(&mut unexpected) {
            Ok(0) => {}
            Ok(read) => panic!(
                "unexpected follow-up view request: {}",
                String::from_utf8_lossy(&unexpected[..read])
            ),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("unexpected server read error: {error}"),
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_input(b"x".to_vec());
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
}

/// Verifies that an idle control-socket primary attach renders once for initial
/// presentation but does not keep sending render requests on repeated terminal
/// input timeouts. This protects the agent-inactive idle path from recreating
/// the previous fixed-cadence `terminal/step render=true` loop.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_does_not_repeat_idle_renders() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    server_stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["initial"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
        let mut unexpected = [0u8; 256];
        match server_stream.read(&mut unexpected) {
            Ok(0) => {}
            Ok(read) => panic!(
                "unexpected repeated idle render request: {}",
                String::from_utf8_lossy(&unexpected[..read])
            ),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("unexpected server read error: {error}"),
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_pending_input_read();
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
}

/// Verifies that runtime events wake an otherwise idle primary attach loop for
/// a fresh terminal view request without restoring fixed-cadence idle renders.
///
/// Pane output and lifecycle changes arrive through the daemon event socket, so
/// this regression protects prompt redraws after the idle-loop optimization
/// suppresses repeated timeout-driven renders.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_runtime_event_requests_view() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let (event_client_stream, mut event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let server = thread::spawn(move || {
        for (expected_id, response_lines) in [
            ("cli-terminal-view-0", "initial"),
            ("cli-terminal-view-1", "event redraw"),
        ] {
            let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
            let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                parsed.get("method").and_then(serde_json::Value::as_str),
                Some("terminal/view")
            );
            assert_eq!(
                parsed.get("id").and_then(serde_json::Value::as_str),
                Some(expected_id)
            );
            if expected_id == "cli-terminal-view-0" {
                event_server_stream
                    .write_all(&event_notification_frame("pane_changed"))
                    .unwrap();
                event_server_stream.flush().unwrap();
            }
            server_stream
                .write_all(&encode_control_body(&format!(
                    r#"{{"jsonrpc":"2.0","id":"{expected_id}","result":{{"view":{{"lines":["{response_lines}"],"line_style_spans":[[]],"cursor":{{"row":0,"column":12,"visible":true,"style":"bar","blink":false}},"output_modes":{{"application_keypad":false}}}}}}}}"#
                )))
                .unwrap();
            server_stream.flush().unwrap();
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_pending_input_read();
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async_with_runtime_events(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
            Some(event_client_stream),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 0);
    assert_eq!(io.written_frames.len(), 2);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
    assert_eq!(io.written_frames[1].lines, vec!["event redraw"]);
}

/// Verifies active animation metadata refreshes a socket-attached primary view
/// even when no runtime event arrives.
///
/// Agent status animation changes only presentation styling. It should not
/// require durable event-log traffic, but the attach loop still has to request
/// fresh views while the last rendered frame advertises an animation cadence.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_refreshes_active_animation_without_runtime_event() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    server_stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let (event_client_stream, event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let server = thread::spawn(move || {
        let _event_server_stream = event_server_stream;
        for (expected_id, response_lines, animation_refresh_interval_ms) in [
            ("cli-terminal-view-0", "thinking phase one", 180),
            ("cli-terminal-view-1", "thinking phase two", 0),
        ] {
            let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
            let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                parsed.get("method").and_then(serde_json::Value::as_str),
                Some("terminal/view")
            );
            assert_eq!(
                parsed.get("id").and_then(serde_json::Value::as_str),
                Some(expected_id)
            );
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": expected_id,
                "result": {
                    "view": {
                        "lines": [response_lines],
                        "line_style_spans": [[]],
                        "cursor": {
                            "row": 0,
                            "column": 18,
                            "visible": true,
                            "style": "bar",
                            "blink": false,
                        },
                        "output_modes": {
                            "application_keypad": false,
                            "animation_refresh_interval_ms": animation_refresh_interval_ms,
                        },
                    },
                },
            })
            .to_string();
            server_stream
                .write_all(&encode_control_body(&response))
                .unwrap();
            server_stream.flush().unwrap();
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_pending_input_read();
    io.push_pending_input_read();
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async_with_runtime_events(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
            Some(event_client_stream),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 0);
    assert_eq!(io.written_frames.len(), 2);
    assert_eq!(io.written_frames[0].lines, vec!["thinking phase one"]);
    assert_eq!(io.written_frames[1].lines, vec!["thinking phase two"]);
}
/// Verifies an idle primary control attach notices local terminal resizes and
/// requests a fresh view without waiting for user input or daemon events.
///
/// Terminal resizes are a local presentation concern, so the foreground attach
/// client should poll terminal size on its own and only invalidate/redraw when
/// the measured size actually changes.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_refreshes_idle_resize_without_input() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    server_stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let (event_client_stream, event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let server = thread::spawn(move || {
        let _event_server_stream = event_server_stream;
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-view-0")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("client_size"))
                .and_then(|size| size.get("columns"))
                .and_then(serde_json::Value::as_u64),
            Some(80)
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("client_size"))
                .and_then(|size| size.get("rows"))
                .and_then(serde_json::Value::as_u64),
            Some(24)
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["initial"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();

        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/step")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-resize-1")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("idempotency_key"))
                .and_then(serde_json::Value::as_str),
            Some("cli-c1-terminal-resize-1")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("render"))
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("input_bytes"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::is_empty),
            Some(true)
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("client_size"))
                .and_then(|size| size.get("columns"))
                .and_then(serde_json::Value::as_u64),
            Some(100)
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("client_size"))
                .and_then(|size| size.get("rows"))
                .and_then(serde_json::Value::as_u64),
            Some(30)
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-resize-1","result":{"input_bytes":0,"application":{"forwarded_bytes":0,"mux_actions_applied":0,"mouse_actions_reported":0,"agent_prompt_inputs_applied":0,"view_refresh_required":false,"full_redraw_required":false,"unsupported_actions":[]},"view":null,"ui_theme":null}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();

        {
            let (expected_id, expected_columns, expected_rows, response_lines) =
                ("cli-terminal-view-1", 100, 30, "resized");
            let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
            let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                parsed.get("method").and_then(serde_json::Value::as_str),
                Some("terminal/view")
            );
            assert_eq!(
                parsed.get("id").and_then(serde_json::Value::as_str),
                Some(expected_id)
            );
            assert_eq!(
                parsed
                    .get("params")
                    .and_then(|params| params.get("client_size"))
                    .and_then(|size| size.get("columns"))
                    .and_then(serde_json::Value::as_u64),
                Some(expected_columns)
            );
            assert_eq!(
                parsed
                    .get("params")
                    .and_then(|params| params.get("client_size"))
                    .and_then(|size| size.get("rows"))
                    .and_then(serde_json::Value::as_u64),
                Some(expected_rows)
            );
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": expected_id,
                "result": {
                    "view": {
                        "lines": [response_lines],
                        "line_style_spans": [[]],
                        "cursor": {
                            "row": 0,
                            "column": 7,
                            "visible": true,
                            "style": "bar",
                            "blink": false,
                        },
                        "output_modes": {
                            "application_keypad": false,
                        },
                    },
                },
            })
            .to_string();
            server_stream
                .write_all(&encode_control_body(&response))
                .unwrap();
            server_stream.flush().unwrap();
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_terminal_size(Some(Size::new(80, 24).unwrap()));
    io.push_terminal_size(Some(Size::new(100, 30).unwrap()));
    io.push_pending_input_read();
    io.push_pending_input_read();
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async_with_runtime_events(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
            Some(event_client_stream),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 1);
    assert_eq!(io.written_frames.len(), 2);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
    assert_eq!(io.written_frames[1].lines, vec!["resized"]);
}

/// Verifies that generic runtime events do not redraw the attached terminal.
///
/// Diagnostic notifications can be emitted as runtime bookkeeping, but they do
/// not alter the visible attached terminal frame. This protects the idle
/// efficiency refactor from turning event traffic into flicker.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_ignores_nonvisible_runtime_events() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let (event_client_stream, mut event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-view-0")
        );
        event_server_stream
            .write_all(&event_notification_frame("diagnostic"))
            .unwrap();
        event_server_stream.flush().unwrap();
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["initial"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
        drop(event_server_stream);
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_pending_input_read();
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async_with_runtime_events(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
            Some(event_client_stream),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 0);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
}

/// Verifies that structural runtime events redraw after invalidating the diff
/// base exactly once.
///
/// Layout-changing event notifications can make the previous output frame an
/// unsafe basis for incremental rendering, so the attach loop should invalidate
/// only for that stronger event class.
#[tokio::test(start_paused = true, flavor = "current_thread")]
/// Verifies ready terminal input wins over simultaneous runtime redraw events.
///
/// Frequent runtime notifications should not postpone already-ready keyboard
/// input, because the attached primary loop is responsible for forwarding user
/// bytes with low latency even while agent panes are producing redraw traffic.
async fn control_socket_primary_attach_loop_prefers_ready_input_over_runtime_event() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let (event_client_stream, mut event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-view-0")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["initial"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();

        event_server_stream
            .write_all(&event_notification_frame("pane_changed"))
            .unwrap();
        event_server_stream.flush().unwrap();

        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/step")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-step-1")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("input_bytes"))
                .and_then(serde_json::Value::as_array),
            Some(&vec![serde_json::Value::from(b'x')])
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-step-1","result":{"input_bytes":1,"application":{"forwarded_bytes":1,"mux_actions_applied":0,"mouse_actions_reported":0,"agent_prompt_inputs_applied":0,"view_refresh_required":false,"full_redraw_required":false,"unsupported_actions":[]},"view":null,"ui_theme":null}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();

        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-view-1")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-1","result":{"view":{"lines":["after input"],"line_style_spans":[[]],"cursor":{"row":0,"column":11,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
        drop(event_server_stream);
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_readiness(vec![readable_input_readiness()]);
    io.push_input(b"x".to_vec());
    io.push_pending_input_read();
    io.push_readiness(vec![readable_input_readiness()]);
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async_with_runtime_events(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
            Some(event_client_stream),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 0);
    assert_eq!(io.written_frames.len(), 2);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
    assert_eq!(io.written_frames[1].lines, vec!["after input"]);
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_structural_runtime_event_invalidates_once() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let (event_client_stream, mut event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let server = thread::spawn(move || {
        for (expected_id, response_lines) in [
            ("cli-terminal-view-0", "initial"),
            ("cli-terminal-view-1", "window changed"),
        ] {
            let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
            let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                parsed.get("method").and_then(serde_json::Value::as_str),
                Some("terminal/view")
            );
            assert_eq!(
                parsed.get("id").and_then(serde_json::Value::as_str),
                Some(expected_id)
            );
            server_stream
                .write_all(&encode_control_body(&format!(
                    r#"{{"jsonrpc":"2.0","id":"{expected_id}","result":{{"view":{{"lines":["{response_lines}"],"line_style_spans":[[]],"cursor":{{"row":0,"column":14,"visible":true,"style":"bar","blink":false}},"output_modes":{{"application_keypad":false}}}}}}}}"#
                )))
                .unwrap();
            server_stream.flush().unwrap();
            if expected_id == "cli-terminal-view-0" {
                event_server_stream
                    .write_all(&event_notification_frame("window_changed"))
                    .unwrap();
                event_server_stream.flush().unwrap();
            }
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_pending_input_read();
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async_with_runtime_events(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
            Some(event_client_stream),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 1);
    assert_eq!(io.written_frames.len(), 2);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
    assert_eq!(io.written_frames[1].lines, vec!["window changed"]);
}

/// Verifies that event stream decoding buffers split frames across socket reads.
///
/// Runtime event notifications use the same framed protocol as control
/// responses, so the attach client must not assume that one socket read contains
/// one complete event.
#[tokio::test(flavor = "current_thread")]
async fn attached_runtime_event_stream_buffers_split_frames() {
    let (event_client_stream, mut event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let mut event_stream = AttachedRuntimeEventStream::new(event_client_stream);
    let frame = event_notification_frame("pane_changed");
    let split_at = frame.len() / 2;
    event_server_stream.write_all(&frame[..split_at]).unwrap();
    event_server_stream.flush().unwrap();
    assert_eq!(
        event_stream.read_render_action().await.unwrap(),
        AttachRenderAction::None
    );
    event_server_stream.write_all(&frame[split_at..]).unwrap();
    event_server_stream.flush().unwrap();
    assert_eq!(
        event_stream.read_render_action().await.unwrap(),
        AttachRenderAction::View
    );
}

/// Verifies that a burst of runtime events is coalesced into the strongest
/// single render action.
///
/// A pane update followed by a structural event should not produce multiple
/// immediate redraw requests; the attach loop only needs the strongest action
/// from the burst.
#[tokio::test(flavor = "current_thread")]
async fn attached_runtime_event_stream_coalesces_event_burst() {
    let (event_client_stream, mut event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let mut event_stream = AttachedRuntimeEventStream::new(event_client_stream);
    let mut burst = event_notification_frame("pane_changed");
    burst.extend_from_slice(&event_notification_frame("window_changed"));
    event_server_stream.write_all(&burst).unwrap();
    event_server_stream.flush().unwrap();
    assert_eq!(
        event_stream.read_render_action().await.unwrap(),
        AttachRenderAction::InvalidateAndView
    );
}

/// Verifies interactive control-socket attachment reports an error when the
/// daemon closes the socket before sending a response frame, so callers do not
/// mistake an interrupted active attachment for a successful detach.
#[tokio::test(flavor = "current_thread")]
async fn control_socket_primary_attach_loop_exits_on_incomplete_response_eof() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        assert!(body.contains(r#""terminal/step""#), "{body}");
        drop(server_stream);
    });

    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_input(b"x".to_vec());

    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    let error = run_control_socket_attached_primary_client_loop_async(
        &mut client_stream,
        &mut io,
        primary_client_id,
        Size::new(80, 24).unwrap(),
    )
    .await
    .expect_err("unexpected daemon control-socket closure must fail attachment");
    server.join().unwrap();

    assert!(
        error
            .message()
            .contains("attached daemon control socket disconnected while awaiting a response"),
        "{error:?}"
    );

    assert_eq!(io.presentation_entries, 1);
    assert!(io.written_frames.is_empty());
}

/// Verifies the interactive observer attach loop polls observer-local status
/// before reading the terminal view.
///
/// Immediately attached observers issue rendered live view requests without
/// polling an approval endpoint.
#[tokio::test(flavor = "current_thread")]
async fn control_socket_observer_attach_loop_renders_immediately() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        let response = r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["observer live view"],"line_style_spans":[[]],"cursor":{"row":0,"column":18,"visible":true,"style":"block","blink":false},"output_modes":{"application_keypad":false}}}}"#;
        server_stream
            .write_all(&encode_control_body(response))
            .unwrap();
        server_stream.flush().unwrap();
    });

    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_input(b"x".to_vec());

    run_control_socket_attached_observer_client_loop_async(
        &mut client_stream,
        &mut io,
        Size::new(80, 24).unwrap(),
    )
    .await
    .unwrap();
    server.join().unwrap();

    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, vec!["observer live view"]);
}

/// Verifies remote primary attach serializes resize, input, and view requests
/// behind one response each on the same persistent byte stream.
#[tokio::test(flavor = "current_thread")]
async fn iroh_primary_attach_loop_orders_resize_input_and_view() {
    let (mut client_stream, mut server_stream) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let mut methods = Vec::new();
        for expected in ["resize", "input", "view"] {
            let request = crate::cli::attach::read_async_control_response_frames(
                &mut server_stream,
                1024 * 1024,
                1,
            )
            .await
            .unwrap();
            let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            let method = parsed
                .get("method")
                .and_then(serde_json::Value::as_str)
                .unwrap();
            let expected_method = if expected == "view" {
                "terminal/view"
            } else {
                "terminal/step"
            };
            assert_eq!(method, expected_method);
            methods.push(expected.to_string());
            if expected == "resize" {
                assert!(
                    parsed
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|id| id.starts_with("cli-terminal-resize-"))
                );
                assert_eq!(
                    parsed
                        .get("params")
                        .and_then(|params| params.get("input_bytes"))
                        .and_then(serde_json::Value::as_array)
                        .map(Vec::is_empty),
                    Some(true)
                );
                assert_eq!(
                    parsed
                        .get("params")
                        .and_then(|params| params.get("client_size"))
                        .and_then(|size| size.get("columns"))
                        .and_then(serde_json::Value::as_u64),
                    Some(100)
                );
            }
            if expected == "input" {
                assert_eq!(
                    parsed
                        .get("params")
                        .and_then(|params| params.get("input_bytes"))
                        .and_then(serde_json::Value::as_array)
                        .and_then(|bytes| bytes.first())
                        .and_then(serde_json::Value::as_u64),
                    Some(u64::from(b'x'))
                );
            }
            let id = parsed.get("id").cloned().unwrap();
            let result = if expected == "view" {
                serde_json::json!({
                    "view": {
                        "lines": ["remote ordered"],
                        "line_style_spans": [[]],
                        "cursor": {
                            "row": 0,
                            "column": 14,
                            "visible": true,
                            "style": "bar",
                            "blink": false
                        },
                        "output_modes": {"application_keypad": false}
                    }
                })
            } else {
                serde_json::json!({
                    "input_bytes": 0,
                    "application": {
                        "forwarded_bytes": 0,
                        "mux_actions_applied": 0,
                        "mouse_actions_reported": 0,
                        "agent_prompt_inputs_applied": 0,
                        "view_refresh_required": expected == "input",
                        "full_redraw_required": false,
                        "unsupported_actions": []
                    },
                    "view": null,
                    "ui_theme": null,
                    "session_terminated": false
                })
            };
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            })
            .to_string();
            tokio::io::AsyncWriteExt::write_all(
                &mut server_stream,
                &encode_control_body(&response),
            )
            .await
            .unwrap();
            tokio::io::AsyncWriteExt::flush(&mut server_stream)
                .await
                .unwrap();
        }
        methods
    });

    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_terminal_size(Some(Size::new(100, 30).unwrap()));
    io.push_input(b"x".to_vec());
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    run_iroh_attached_primary_client_loop_async(
        &mut client_stream,
        &mut io,
        primary_client_id,
        Size::new(80, 24).unwrap(),
        Duration::from_secs(1),
    )
    .await
    .unwrap();

    assert_eq!(
        server.await.unwrap(),
        vec![
            "resize".to_string(),
            "input".to_string(),
            "view".to_string(),
        ]
    );
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 1);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, vec!["remote ordered"]);
}

/// Verifies an acknowledged remote detach exits cleanly without issuing a
/// follow-up terminal view request against the now-detached client.
#[tokio::test(flavor = "current_thread")]
async fn iroh_primary_attach_loop_stops_after_acknowledged_detach() {
    let (mut client_stream, mut server_stream) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let request = crate::cli::attach::read_async_control_response_frames(
            &mut server_stream,
            1024 * 1024,
            1,
        )
        .await
        .unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/step")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("input_bytes"))
                .and_then(serde_json::Value::as_array)
                .map(|bytes| bytes
                    .iter()
                    .filter_map(serde_json::Value::as_u64)
                    .collect::<Vec<_>>()),
            Some(vec![1, u64::from(b'd')])
        );
        let id = parsed.get("id").cloned().unwrap();
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "application": {
                    "view_refresh_required": true,
                    "full_redraw_required": false
                },
                "client_detached": true,
                "session_terminated": false
            }
        })
        .to_string();
        tokio::io::AsyncWriteExt::write_all(&mut server_stream, &encode_control_body(&response))
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::flush(&mut server_stream)
            .await
            .unwrap();
        let mut unexpected = [0u8; 256];
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                tokio::io::AsyncReadExt::read(&mut server_stream, &mut unexpected),
            )
            .await
            .is_err(),
            "remote detach issued an unexpected follow-up request"
        );
    });

    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_input(b"\x01d".to_vec());
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    run_iroh_attached_primary_client_loop_async(
        &mut client_stream,
        &mut io,
        primary_client_id,
        Size::new(80, 24).unwrap(),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    server.await.unwrap();

    assert_eq!(io.presentation_entries, 1);
    assert!(io.written_frames.is_empty());
}

/// Verifies a lost acknowledgement after remote terminal input is visible and
/// never causes the client to reconnect or replay the buffered input.
#[tokio::test(flavor = "current_thread")]
async fn iroh_primary_attach_loop_reports_ambiguous_input_without_replay() {
    let (mut client_stream, mut server_stream) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let request = crate::cli::attach::read_async_control_response_frames(
            &mut server_stream,
            1024 * 1024,
            1,
        )
        .await
        .unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/step")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("input_bytes"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
    });

    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_input(b"x".to_vec());
    let primary_client_id = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    let error = run_iroh_attached_primary_client_loop_async(
        &mut client_stream,
        &mut io,
        primary_client_id,
        Size::new(80, 24).unwrap(),
        Duration::from_secs(1),
    )
    .await
    .expect_err("lost remote input acknowledgement must require reattach");
    server.await.unwrap();

    assert!(error.message().contains("outcome is unknown"), "{error:?}");
    assert!(error.message().contains("reattach required"), "{error:?}");
    assert!(
        error.message().contains("input was not replayed"),
        "{error:?}"
    );
    assert_eq!(io.presentation_entries, 1);
    assert!(io.written_frames.is_empty());
}
