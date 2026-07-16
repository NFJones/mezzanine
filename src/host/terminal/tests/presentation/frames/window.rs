//! Regression tests for terminal presentation frames window behavior.

use crate::host::terminal::{
    BTreeMap, DEFAULT_PANE_FRAME_TEMPLATE, DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE,
    DEFAULT_WINDOW_FRAME_TEMPLATE, PaneRenderInput, TerminalClientLoopConfig, TerminalFrameContext,
    TerminalFrameRenderOptions, WindowFrameAction, render_attached_client_view,
    render_window_with_pane_frame_template, window_frame_action_pillbox_cells,
};
use mez_core::ids::IdFactory;
use mez_mux::layout::{Size, SplitDirection, Window};
use mez_mux::presentation::ClientViewRole;
use mez_mux::presentation::{
    TerminalFramePosition, TerminalWindowFrameContext, TerminalWindowStatusContext,
};
use mez_terminal::TerminalColor;
use unicode_width::UnicodeWidthStr;

/// Verifies that window frame templates render named fields, sanitize control
/// characters, and reserve one row from the rendered window body.
#[test]
fn render_window_frame_uses_named_template_fields() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 7, "main\u{1b}[31m", Size::new(18, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let inputs = window
        .panes()
        .iter()
        .map(|pane| PaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: vec![format!("pane{}", pane.index)],
        })
        .collect::<Vec<_>>();

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &TerminalFrameContext::default(),
        TerminalFrameRenderOptions::plain(
            true,
            "#{window.index}|#{window.name}|#{window.pane_count}|#{layout.name}",
            TerminalFramePosition::Top,
        ),
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
    )
    .unwrap();

    assert_eq!(rendered.len(), 3);
    assert_eq!(rendered[0], "7|main[31m|2|tiled");
    assert_eq!(rendered[1], "pane0   \u{2502}pane1    ");
}

/// Verifies that the built-in default window frame renders ordered window
/// pillboxes from runtime frame context rather than only the active window. This
/// keeps the foreground footer useful as a multi-window navigation surface,
/// gives the styled renderer concrete spans for highlighting the focused window
/// pill, and verifies unfocused subagent windows receive their distinct pill
/// color.
#[test]
fn render_default_window_frame_uses_window_pillbox_context() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 1, "work", Size::new(40, 3).unwrap());
    let inputs = vec![PaneRenderInput {
        pane_id: window.panes()[0].id.to_string(),
        lines: vec!["body".to_string()],
    }];
    let frame_context = TerminalFrameContext {
        windows: vec![
            TerminalWindowFrameContext {
                id: "@1".to_string(),
                index: 0,
                title: "shell".to_string(),
                active: false,
                subagent: true,
            },
            TerminalWindowFrameContext {
                id: "@2".to_string(),
                index: 1,
                title: "work".to_string(),
                active: true,
                subagent: false,
            },
        ],
        ..TerminalFrameContext::default()
    };

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_WINDOW_FRAME_TEMPLATE,
            TerminalFramePosition::Bottom,
        ),
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
    )
    .unwrap();

    assert_eq!(rendered[2].trim_end(), " 0 shell   1 work");
    let mut config = TerminalClientLoopConfig {
        frame_context,
        window_frame_template: DEFAULT_WINDOW_FRAME_TEMPLATE.to_string(),
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };
    config.window_frames_enabled = true;
    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    assert!(view.line_style_spans[2].iter().any(|span| {
        span.start >= 10 && span.rendition.background == Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8))
    }));
    assert!(view.line_style_spans[2].iter().any(|span| {
        span.start == 0
            && span.rendition.background.is_some()
            && span.rendition.background != Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8))
    }));
}

/// Verifies that the window status bar renders single-cell action pills
/// with mouse-addressable geometry, a distinct pressed style, and a trailing
/// safety column. This protects the templated controls as clickable terminal
/// UI rather than passive text while avoiding host-terminal edge-cell clipping.
#[test]
fn render_default_window_frame_action_pills_are_clickable_and_pressed() {
    let mut ids = IdFactory::default();
    let window = Window::new(
        &mut ids,
        0,
        "abcdefghijklmnopqrstuvwxZ",
        Size::new(80, 3).unwrap(),
    );
    let horizontal_split_action = WindowFrameAction::terminal_button("-", "split-window -h");
    let new_window_action = WindowFrameAction::terminal_button("□", "new-window");
    let frame_context = TerminalFrameContext {
        pressed_window_action: Some(new_window_action.clone()),
        window_status: Some(TerminalWindowStatusContext {
            template: DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string(),
            active_pane_working_directory: Some("~/repo".to_string()),
            status_pills: BTreeMap::new(),
            system_uptime: "1h".to_string(),
            datetime_local: "2026-05-09 12:00:00".to_string(),
        }),
        windows: vec![TerminalWindowFrameContext {
            id: "@1".to_string(),
            index: 0,
            title: "abcdefghijklmnopqrstuvwxZ".to_string(),
            active: true,
            subagent: false,
        }],
        ..TerminalFrameContext::default()
    };
    let config = TerminalClientLoopConfig {
        frame_context: frame_context.clone(),
        window_frame_template: DEFAULT_WINDOW_FRAME_TEMPLATE.to_string(),
        window_frames_enabled: true,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(
        view.lines[2].contains("-   +   □   ⊕   λ"),
        "{}",
        view.lines[2]
    );
    assert!(!view.lines[2].contains(" Δ"), "{}", view.lines[2]);
    assert_eq!(view.lines[2].chars().last(), Some(' '), "{}", view.lines[2]);
    let status_start = view.lines[2]
        .find(" ~/repo")
        .expect("default window status should render the active pane cwd");
    assert!(
        status_start < usize::from(window.size.columns).saturating_sub(1),
        "window status should leave the final column as frame fill: {}",
        view.lines[2]
    );
    let cells = window_frame_action_pillbox_cells(&frame_context, 2, window.size.columns);
    assert!(
        cells
            .iter()
            .any(|cell| cell.row == 2 && cell.action == horizontal_split_action),
        "horizontal split action pill should expose clickable cells"
    );
    let new_window_start = cells
        .iter()
        .filter(|cell| cell.row == 2 && cell.action == new_window_action)
        .map(|cell| cell.column)
        .min()
        .expect("new-window action pill should expose clickable cells");
    assert!(view.line_style_spans[2].iter().any(|span| {
        span.start == usize::from(new_window_start)
            && span.length == 3
            && span.rendition.background == Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8))
    }));
}

/// Verifies that the window bar can reserve a configurable right-aligned
/// status line and style action buttons, uptime, and local datetime separately.
/// This keeps the window list usable on the left while making dynamic status
/// items visually distinct and removable through the status template.
#[test]
fn render_window_status_uses_right_aligned_themed_segments() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 1, "work", Size::new(96, 3).unwrap());
    let frame_context = TerminalFrameContext {
        windows: vec![TerminalWindowFrameContext {
            id: "@2".to_string(),
            index: 1,
            title: "work".to_string(),
            active: true,
            subagent: false,
        }],
        window_status: Some(TerminalWindowStatusContext {
            template: DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string(),
            active_pane_working_directory: Some("~/repo".to_string()),
            status_pills: BTreeMap::new(),
            system_uptime: "2d 03h 04m".to_string(),
            datetime_local: "2026-05-05 10:11:12".to_string(),
        }),
        ..TerminalFrameContext::default()
    };
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frame_template: DEFAULT_WINDOW_FRAME_TEMPLATE.to_string(),
        window_frames_enabled: true,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(view.lines[2].contains("1 work"));
    assert!(view.lines[2].contains("-   +   □   ⊕   λ"));
    assert!(!view.lines[2].contains(" Δ"));
    assert!(view.lines[2].contains(" ~/repo "));
    assert!(view.lines[2].find(" ~/repo ").unwrap() < view.lines[2].find(" + ").unwrap());
    assert!(view.lines[2].contains(" 2d 03h 04m "));
    assert!(view.lines[2].contains(" 2026-05-05 10:11:12 "));
    assert!(view.lines[2].ends_with(" 2026-05-05 10:11:12  "));
    assert_eq!(view.lines[2].chars().last(), Some(' '), "{}", view.lines[2]);
    let uptime_start_bytes = view.lines[2].find(" 2d 03h 04m ").unwrap();
    let uptime_start = UnicodeWidthStr::width(&view.lines[2][..uptime_start_bytes]);
    assert!(view.line_style_spans[2].iter().any(|span| {
        span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
            && span.start == uptime_start
            && span.length == " 2d 03h 04m ".len()
    }));
    let datetime_start_bytes = view.lines[2].find(" 2026-05-05 10:11:12 ").unwrap();
    let datetime_start = UnicodeWidthStr::width(&view.lines[2][..datetime_start_bytes]);
    assert!(view.line_style_spans[2].iter().any(|span| {
        span.rendition.background == Some(TerminalColor::Rgb(0xe6, 0xc3, 0x84))
            && span.start == datetime_start
            && span.length == " 2026-05-05 10:11:12 ".len()
    }));
}

/// Verifies that cached command-backed window status pills render through the
/// normal status template pill path. This keeps shell command execution out of
/// terminal rendering while preserving themed, padded status segments for
/// `#{pill.<name>}` fields.
#[test]
fn render_window_status_uses_cached_command_status_pills() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 1, "work", Size::new(50, 3).unwrap());
    let frame_context = TerminalFrameContext {
        windows: vec![TerminalWindowFrameContext {
            id: "@2".to_string(),
            index: 1,
            title: "work".to_string(),
            active: true,
            subagent: false,
        }],
        window_status: Some(TerminalWindowStatusContext {
            template: "#{pill.cpu} #{datetime.local}".to_string(),
            active_pane_working_directory: None,
            status_pills: BTreeMap::from([("cpu".to_string(), "CPU 42%".to_string())]),
            system_uptime: String::new(),
            datetime_local: "2026-05-05 10:11:12".to_string(),
        }),
        ..TerminalFrameContext::default()
    };
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frame_template: DEFAULT_WINDOW_FRAME_TEMPLATE.to_string(),
        window_frames_enabled: true,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(view.lines[2].contains(" CPU 42% "), "{}", view.lines[2]);
    let pill_start_bytes = view.lines[2].find(" CPU 42% ").unwrap();
    let pill_start = UnicodeWidthStr::width(&view.lines[2][..pill_start_bytes]);
    assert!(view.line_style_spans[2].iter().any(|span| {
        span.start == pill_start
            && span.length == " CPU 42% ".len()
            && span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
    }));
}

/// Verifies that a framed window never grows beyond the authoritative window
/// height when there is only enough vertical space for the window frame row.
#[test]
fn render_window_frame_fits_single_row_window() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(12, 1).unwrap());
    let inputs = vec![PaneRenderInput {
        pane_id: window.panes()[0].id.to_string(),
        lines: vec!["body".to_string()],
    }];

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &TerminalFrameContext::default(),
        TerminalFrameRenderOptions::plain(
            true,
            "#{window.index}:#{window.name}",
            TerminalFramePosition::Top,
        ),
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
    )
    .unwrap();

    assert_eq!(rendered, vec!["0:main      "]);
}
