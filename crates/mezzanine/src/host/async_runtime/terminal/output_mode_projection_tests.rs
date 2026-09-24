//! Field-complete local terminal output-mode projection regressions.

use super::*;

/// All config-only modes survive a missing view while view-owned modes
/// remain inactive; recovery suppresses only cursor blink.
#[test]
fn local_output_modes_preserve_absent_view_and_recovery_policy() {
    let mut config = TerminalClientLoopConfig::default();
    config.mouse_policy.pane_application_keypad_mode = true;
    config.mouse_policy.enabled = false;
    config.enhanced_keyboard_reporting = true;
    config.pane_bracketed_paste_mode = true;
    config.cursor_blink = true;
    config.cursor_blink_interval_ms = 700;
    let modes = local_output_modes(&config, None, 321, true);
    assert_eq!(
        modes,
        AttachedTerminalOutputModes {
            application_keypad: true,
            enhanced_keyboard_reporting: false,
            bracketed_paste: true,
            focus_events: false,
            alternate_screen: false,
            host_mouse_reporting: false,
            cursor_style: config.cursor_style,
            cursor_blink: true,
            cursor_blink_interval_ms: 700,
            cursor_blink_elapsed_ms: 321,
            animation_refresh_interval_ms: 0,
            cursor_visible: false,
            cursor_row: 0,
            cursor_column: 0,
        }
    );
    assert_eq!(
        local_output_modes(&config, None, 321, false),
        AttachedTerminalOutputModes {
            cursor_blink: false,
            ..modes
        }
    );
}

/// A primary readline view supplies cursor, focus and animation fields;
/// observer views cannot activate enhanced keyboard reporting.
#[test]
fn local_output_modes_project_view_fields_and_primary_keyboard_policy() {
    let config = TerminalClientLoopConfig {
        enhanced_keyboard_reporting: true,
        cursor_blink: true,
        ..TerminalClientLoopConfig::default()
    };
    let mut view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(8, 2).unwrap(),
        client_size: Size::new(8, 2).unwrap(),
        lines: vec!["pane".to_owned(), "prompt".to_owned()],
        line_style_spans: vec![Vec::new(), Vec::new()],
        selection: None,
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 1,
        cursor_column: 4,
        cursor_visible: true,
        cursor_style: config.cursor_style,
        cursor_blink: false,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        focus_events: true,
        alternate_screen: true,
        host_mouse_reporting: false,
        animation_refresh_interval_ms: 80,
        ui_theme: Default::default(),
        agent_prompt_region: None,
        primary_prompt_active: true,
        readline_input_active: true,
    };
    let modes = local_output_modes(&config, Some(&view), 19, true);
    assert_eq!(
        modes,
        AttachedTerminalOutputModes {
            application_keypad: false,
            enhanced_keyboard_reporting: true,
            bracketed_paste: false,
            focus_events: true,
            alternate_screen: true,
            host_mouse_reporting: true,
            cursor_style: config.cursor_style,
            cursor_blink: true,
            cursor_blink_interval_ms: 500,
            cursor_blink_elapsed_ms: 19,
            animation_refresh_interval_ms: 80,
            cursor_visible: true,
            cursor_row: 1,
            cursor_column: 4,
        }
    );
    assert_eq!(
        local_output_modes(&config, Some(&view), 19, false),
        AttachedTerminalOutputModes {
            cursor_blink: false,
            ..modes
        }
    );
    view.role = ClientViewRole::Observer;
    assert!(!local_output_modes(&config, Some(&view), 19, true).enhanced_keyboard_reporting);
}
