//! Control capture tests.

use super::*;

/// Verifies that pane/capture returns plain text content and any supplied
/// history and visible-line style spans over the same requested range.
#[test]
fn pane_capture_uses_supplied_visible_and_history_source() {
    let (mut session, primary) = test_session();
    let pane_id = session.windows()[0].panes()[0].id.to_string();
    let captures = vec![PaneCaptureSource {
        pane_id: pane_id.clone(),
        visible_lines: vec!["visible one".to_string(), "visible two".to_string()],
        visible_line_style_spans: vec![
            vec![TerminalStyleSpan {
                start: 0,
                length: 7,
                rendition: GraphicRendition {
                    bold: true,
                    dim: false,
                    italic: false,
                    strikethrough: false,
                    double_underline: false,
                    hidden: false,
                    underline: false,
                    inverse: false,
                    foreground: Some(TerminalColor::Indexed(2)),
                    background: None,
                },
            }],
            Vec::new(),
        ],
        history_lines: vec!["history".to_string()],
        history_line_style_spans: vec![vec![TerminalStyleSpan {
            start: 0,
            length: 7,
            rendition: GraphicRendition {
                bold: false,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: true,
                inverse: false,
                foreground: None,
                background: Some(TerminalColor::Indexed(4)),
            },
        }]],
        alternate_screen_active: false,
        truncated: false,
        primary_pid: Some(1234),
        process_state: Some("running".to_string()),
        readiness_state: Some("ready".to_string()),
        exit_status: None,
    }];
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"include_history":true,"range":{{"origin":"combined","start":"start","end":"end"}}}}}}"#,
        json_escape(&pane_id)
    );

    let response =
        dispatch_control_request_with_captures(&request, &mut session, &primary, &captures);

    assert!(response.contains("history\\nvisible one\\nvisible two"));
    assert!(response.contains(r#""range":{"origin":"combined","start":0,"end":3}"#));
    assert!(response.contains(r#""line_style_spans":[[{"start":0,"length":7"#));
    assert!(response.contains(r#""foreground":{"kind":"indexed","index":2}"#));
    assert!(response.contains(r#""background":{"kind":"indexed","index":4}"#));
    assert!(response.contains(r#""source_available":true"#));
    assert!(response.contains(r#""primary_pid":1234"#));
    assert!(response.contains(r#""readiness_state":"ready""#));
}

/// Verifies that pane/capture includes a supplied normalized exit-status
/// object in the embedded PaneState. Capture sources are used for restored or
/// otherwise non-live pane views, so this path must not collapse a known
/// process status back to `null`.
#[test]
fn pane_capture_embeds_supplied_exit_status() {
    let (mut session, primary) = test_session();
    let pane_id = session.windows()[0].panes()[0].id.to_string();
    session.set_pane_live_state(&pane_id, false).unwrap();
    let captures = vec![PaneCaptureSource {
        pane_id: pane_id.clone(),
        visible_lines: vec!["exited".to_string()],
        visible_line_style_spans: vec![Vec::new()],
        history_lines: Vec::new(),
        history_line_style_spans: Vec::new(),
        alternate_screen_active: false,
        truncated: false,
        primary_pid: None,
        process_state: Some("exited".to_string()),
        readiness_state: None,
        exit_status: Some(mez_mux::process::PaneExitStatus {
            code: Some(7),
            signal: None,
            success: false,
        }),
    }];
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"visible","start":"start","end":"end"}}}}}}"#,
        json_escape(&pane_id)
    );

    let response =
        dispatch_control_request_with_captures(&request, &mut session, &primary, &captures);

    assert!(
        response.contains(r#""process_state":"exited""#),
        "{response}"
    );
    assert!(
        response.contains(r#""exit_status":{"code":7,"signal":null,"success":false}"#),
        "{response}"
    );
}

/// This regression test verifies that pane/capture treats the CaptureRange
/// object as the source selector and slice window, rather than returning
/// the full captured buffer. It covers visible, history, and combined
/// origins because each origin builds its line vector differently before
/// applying start/end bounds.
#[test]
fn pane_capture_applies_visible_history_and_combined_ranges() {
    let (mut session, primary) = test_session();
    let pane_id = session.windows()[0].panes()[0].id.to_string();
    let captures = vec![PaneCaptureSource {
        pane_id: pane_id.clone(),
        visible_lines: vec![
            "visible zero".to_string(),
            "visible one".to_string(),
            "visible two".to_string(),
        ],
        visible_line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        history_lines: vec![
            "history zero".to_string(),
            "history one".to_string(),
            "history two".to_string(),
        ],
        history_line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        alternate_screen_active: false,
        truncated: false,
        primary_pid: None,
        process_state: None,
        readiness_state: None,
        exit_status: None,
    }];

    let visible_request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"visible","start":1,"end":3}}}}}}"#,
        json_escape(&pane_id)
    );
    let visible_response =
        dispatch_control_request_with_captures(&visible_request, &mut session, &primary, &captures);
    assert!(visible_response.contains("visible one\\nvisible two"));
    assert!(!visible_response.contains("visible zero"));
    assert!(visible_response.contains(r#""range":{"origin":"visible","start":1,"end":3}"#));

    let history_request = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"history","start":"start","end":2}}}}}}"#,
        json_escape(&pane_id)
    );
    let history_response =
        dispatch_control_request_with_captures(&history_request, &mut session, &primary, &captures);
    assert!(history_response.contains("history zero\\nhistory one"));
    assert!(!history_response.contains("history two"));
    assert!(history_response.contains(r#""range":{"origin":"history","start":0,"end":2}"#));

    let combined_request = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"combined","start":2,"end":5}}}}}}"#,
        json_escape(&pane_id)
    );
    let combined_response = dispatch_control_request_with_captures(
        &combined_request,
        &mut session,
        &primary,
        &captures,
    );
    assert!(combined_response.contains("history two\\nvisible zero\\nvisible one"));
    assert!(!combined_response.contains("history one"));
    assert!(!combined_response.contains("visible two"));
    assert!(combined_response.contains(r#""range":{"origin":"combined","start":2,"end":5}"#));
}

/// Verifies pane capture excludes alternate screen from history capture.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_capture_excludes_alternate_screen_from_history_capture() {
    let (mut session, primary) = test_session();
    let pane_id = session.windows()[0].panes()[0].id.to_string();
    let captures = vec![PaneCaptureSource {
        pane_id: pane_id.clone(),
        visible_lines: vec!["alternate".to_string()],
        visible_line_style_spans: vec![Vec::new()],
        history_lines: vec!["normal".to_string()],
        history_line_style_spans: vec![Vec::new()],
        alternate_screen_active: true,
        truncated: false,
        primary_pid: None,
        process_state: None,
        readiness_state: None,
        exit_status: None,
    }];
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"include_history":true,"range":{{"origin":"combined","start":"start","end":"end"}}}}}}"#,
        json_escape(&pane_id)
    );

    let response =
        dispatch_control_request_with_captures(&request, &mut session, &primary, &captures);

    assert!(response.contains(r#""content":"normal""#));
    assert!(!response.contains("alternate\\n"));
}

/// This regression test verifies that malformed CaptureRange values are
/// rejected before capture content is returned. The endpoint must fail
/// deterministically for missing ranges, reversed bounds, unsupported
/// origins, and endpoint values that are not valid offsets or symbolic
/// bounds.
#[test]
fn pane_capture_requires_valid_range() {
    let (mut session, primary) = test_session();
    let pane_id = session.windows()[0].panes()[0].id.to_string();

    let missing_range = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}}}}}}"#,
        json_escape(&pane_id)
    );
    let missing_response =
        dispatch_control_request_with_captures(&missing_range, &mut session, &primary, &[]);
    assert!(missing_response.contains("pane/capture requires range"));

    let invalid_range = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"history","start":2,"end":1}}}}}}"#,
        json_escape(&pane_id)
    );
    let invalid_response =
        dispatch_control_request_with_captures(&invalid_range, &mut session, &primary, &[]);
    assert!(invalid_response.contains("range start must not be greater than end"));

    let invalid_origin = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"scrollback","start":"start","end":"end"}}}}}}"#,
        json_escape(&pane_id)
    );
    let invalid_origin_response =
        dispatch_control_request_with_captures(&invalid_origin, &mut session, &primary, &[]);
    assert!(invalid_origin_response.contains("range origin must be visible"));

    let invalid_endpoint = format!(
        r#"{{"jsonrpc":"2.0","id":4,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"visible","start":-1,"end":"end"}}}}}}"#,
        json_escape(&pane_id)
    );
    let invalid_endpoint_response =
        dispatch_control_request_with_captures(&invalid_endpoint, &mut session, &primary, &[]);
    assert!(invalid_endpoint_response.contains("range start must be an integer"));
}
