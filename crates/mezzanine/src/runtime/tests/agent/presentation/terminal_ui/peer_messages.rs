//! Peer-message transcript wrapping, marker, and raw-source copy parity.
//!
//! These tests keep literal peer payloads and Markdown presentation distinct
//! while preserving sender identity and exact source across narrow geometry.

use super::*;

/// Verifies a wrapped MMP transcript row keeps its sender indicator flush while
/// copy mode recovers the raw peer payload without the display-only continuation
/// inset or sender marker.
#[test]
fn runtime_peer_message_wraps_with_source_copy_payload() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nagent_wrap_column_cap = 24\n".to_string(),
        }])
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(24, 12).unwrap(), 100).unwrap(),
    );
    let payload = "alpha beta gamma delta epsilon";
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "text/plain; charset=utf-8",
            payload,
        )
        .unwrap();

    let copy_mode = ensure_agent_copy_mode_for_test(&mut service, "%1");
    let start = copy_mode
        .lines()
        .iter()
        .position(|line| line == "│ agent-%3> alpha beta")
        .expect("flush peer indicator row");
    let end = copy_mode
        .lines()
        .iter()
        .enumerate()
        .skip(start.saturating_add(1))
        .find(|(_index, line)| *line == "│      epsilon")
        .map(|(index, _line)| index)
        .expect("indented peer continuation row");
    assert!(copy_mode.lines()[start.saturating_add(1)].starts_with("│      "));
    let end_column = UnicodeWidthStr::width(copy_mode.lines()[end].as_str());
    copy_mode
        .select_range(
            CopyPosition {
                line: start,
                column: 0,
            },
            CopyPosition {
                line: end,
                column: end_column,
            },
        )
        .unwrap();
    assert_eq!(
        copy_mode
            .copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap(),
        payload
    );
}

/// Verifies authored newlines in canonical plaintext MMP payloads use the same
/// fixed five-space indent as width-generated continuation rows while source
/// copy preserves the original multiline payload without display-only spacing.
#[test]
fn runtime_peer_message_authored_newlines_match_wrap_indentation() {
    let mut service = test_runtime_service();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 100).unwrap(),
    );
    let payload = "first line\nsecond line";
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "text/plain; charset=utf-8",
            payload,
        )
        .unwrap();

    let copy_mode = ensure_agent_copy_mode_for_test(&mut service, "%1");
    let start = copy_mode
        .lines()
        .iter()
        .position(|line| line == "│ agent-%3> first line")
        .expect("first authored peer-message line");
    assert_eq!(
        copy_mode.lines()[start.saturating_add(1)],
        "│      second line"
    );
    let end_column = UnicodeWidthStr::width(copy_mode.lines()[start + 1].as_str());
    copy_mode
        .select_range(
            CopyPosition {
                line: start,
                column: 0,
            },
            CopyPosition {
                line: start + 1,
                column: end_column,
            },
        )
        .unwrap();
    assert_eq!(
        copy_mode
            .copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap(),
        payload
    );
}

/// Verifies CommonMark line breaks in MMP payloads receive the same fixed
/// five-space indent as assistant transcript messages.
#[test]
fn runtime_peer_message_markdown_newlines_match_wrap_indentation() {
    let mut service = test_runtime_service();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 100).unwrap(),
    );
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "text/markdown; charset=utf-8",
            "first line  \nsecond line",
        )
        .unwrap();

    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let start = rows
        .iter()
        .position(|line| line == "│ agent-%3> first line")
        .expect("first Markdown peer-message line");
    assert_eq!(rows[start.saturating_add(1)], "│      second line");
}

/// Verifies bare and charset-qualified Markdown MMP payloads use the existing
/// Markdown presentation while plain peer payloads remain literal source text.
#[test]
fn runtime_peer_message_markdown_content_type_renders_markdown() {
    let mut service = test_runtime_service();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(60, 12).unwrap(), 100).unwrap(),
    );
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "text/markdown",
            "# Bare heading\n\n**important**",
        )
        .unwrap();
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%4",
            "text/markdown; charset=utf-8",
            "# Qualified heading",
        )
        .unwrap();
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%5",
            "text/plain; charset=utf-8",
            "# Literal heading",
        )
        .unwrap();

    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(
        rows.iter().any(|line| line == "│ agent-%3> Bare heading"),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|line| line == "│ agent-%4> Qualified heading"),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|line| line == "│ agent-%5> # Literal heading"),
        "{rows:#?}"
    );
}

/// Verifies Markdown MMP rows reserve a long sender label before rendering so
/// every physical row remains inside a narrow pane's transcript frame.
#[test]
fn runtime_peer_message_markdown_long_label_honors_narrow_frame_width() {
    let mut service = test_runtime_service();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(24, 12).unwrap(), 100).unwrap(),
    );
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%123456789",
            "text/markdown; charset=utf-8",
            "# Heading wraps safely",
        )
        .unwrap();

    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(
        rows.iter()
            .any(|line| line.starts_with("│ agent-%123456789>")),
        "{rows:#?}"
    );
    assert!(
        rows.iter().all(|line| line.chars().count() <= 24),
        "{rows:#?}"
    );
}

/// Verifies Markdown MMP rows hard-wrap after their directional prefix when a
/// configured agent cap is narrower than the pane, including long tokens that
/// Markdown layout alone cannot break to the transcript frame.
#[test]
fn runtime_peer_message_markdown_honors_configured_wrap_cap() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "peer-markdown-cap".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nagent_wrap_column_cap = 24\n".to_string(),
        }])
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap(),
    );
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "text/markdown; charset=utf-8",
            "**supercalifragilisticexpialidocious**",
        )
        .unwrap();

    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(
        rows.iter()
            .any(|line| line.starts_with("│ agent-%3> supercalifra")),
        "{rows:#?}"
    );
    assert!(
        rows.iter().any(|line| line.starts_with("│      gilistic")),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .filter(|line| !line.trim().is_empty())
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 24),
        "{rows:#?}"
    );
}

/// Verifies rendered and source copy retain each separately logged peer payload
/// when adjacent MMP messages share the same sender and one message wraps.
#[test]
fn runtime_peer_message_copy_keeps_adjacent_message_payloads() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nagent_wrap_column_cap = 24\n".to_string(),
        }])
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(24, 12).unwrap(), 100).unwrap(),
    );
    let first_payload = "first payload";
    let second_payload = "second payload wraps across rows";
    service
        .append_agent_received_peer_message_to_terminal_buffer_with_receive_identity(
            "%1",
            Some("peer-copy-first"),
            "agent-%3",
            "text/plain; charset=utf-8",
            first_payload,
        )
        .unwrap();
    service
        .append_agent_received_peer_message_to_terminal_buffer_with_receive_identity(
            "%1",
            Some("peer-copy-second"),
            "agent-%3",
            "text/plain; charset=utf-8",
            second_payload,
        )
        .unwrap();

    let copy_mode = ensure_agent_copy_mode_for_test(&mut service, "%1");
    let start = copy_mode
        .lines()
        .iter()
        .position(|line| line.starts_with("│ agent-%3> first"))
        .expect("first peer message row");
    let end = copy_mode
        .lines()
        .iter()
        .enumerate()
        .skip(start.saturating_add(1))
        .rfind(|(_index, line)| line.starts_with("│      "))
        .map(|(index, _line)| index)
        .expect("wrapped second peer message row");
    let end_column = UnicodeWidthStr::width(copy_mode.lines()[end].as_str());
    copy_mode
        .select_range(
            CopyPosition {
                line: start,
                column: 0,
            },
            CopyPosition {
                line: end,
                column: end_column,
            },
        )
        .unwrap();
    let expected = format!("{first_payload}\n{second_payload}");
    assert_eq!(
        copy_mode.copy_selection().unwrap(),
        "agent-%3> first\n     payload\nagent-%3> second\n     payload wraps\n     across rows"
    );
    assert_eq!(
        copy_mode
            .copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap(),
        expected
    );
}
