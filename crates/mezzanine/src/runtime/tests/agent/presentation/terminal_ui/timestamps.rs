//! Fixed occurrence-time decoration at the server's visual boundary.
//!
//! Canonical screen/source rows remain undecorated. These regressions exercise
//! the actual client view, not a timestamp-looking string in authored content.

use super::*;

/// A deterministic first observation decorates one semantic entry exactly once,
/// even when its authored newline and unbroken token create physical rows. The
/// server view must reserve blank continuation margins, while canonical rows
/// retain their original column-zero gutter and never gain timestamp bytes.
#[test]
fn runtime_log_timestamps_first_multiline_and_softwrap_are_visual_only() {
    let mut service = test_runtime_service();
    service.presentation.settings.terminal_agent_log_timestamps = true;
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    service.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 20).unwrap(), 120).unwrap(),
    );
    let source = format!("first authored row\n{}", "x".repeat(100));
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", &source)
        .unwrap();
    let canonical = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    assert!(canonical.iter().all(|row| !row.text.contains("01:02:03")));
    assert!(canonical.iter().any(|row| {
        row.presentation.as_ref().is_some_and(|entry| !entry.first_row)
    }));
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 32).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        view.lines.iter().filter(|row| row.contains("01:02:03 │ ")).count(),
        1,
        "one captured stamp must appear on the original first visual row"
    );
    assert!(view.lines.iter().any(|row| row.contains("         │ ")));
}
