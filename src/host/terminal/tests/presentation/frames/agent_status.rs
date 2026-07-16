//! Regression tests for terminal presentation frames agent status behavior.

use crate::host::terminal::tests::fixtures::display_column_for_fragment;
use crate::host::terminal::{
    BTreeMap, DEFAULT_PANE_FRAME_TEMPLATE, PaneRenderInput, TerminalClientLoopConfig,
    TerminalFrameContext, TerminalFrameRenderOptions, TerminalPaneFrameContext,
    render_attached_client_view, render_window_with_pane_frame_template,
};
use mez_core::ids::IdFactory;
use mez_mux::layout::{Size, SplitDirection, Window};
use mez_mux::presentation::ClientViewRole;
use mez_mux::presentation::TerminalFramePosition;
use mez_mux::theme::{BUILTIN_UI_THEME_NAMES, builtin_ui_theme_definition, resolve_ui_theme};
use mez_terminal::TerminalColor;

/// Verifies that the built-in pane frame shows agent model, reasoning, and
/// state status on the right side only while the pane is in agent mode.
#[test]
fn render_default_pane_frame_right_aligns_agent_status_in_agent_mode() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(56, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_name: Some("manager".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(
        rendered[0],
        " 0 shell                      gpt-5.5   high   running  "
    );
}

/// Verifies that overlong pane-frame agent status text cannot consume the
/// rightmost horizontal border cell. This protects split-pane divider rows
/// where the pane frame merges into the horizontal boundary between stacked
/// panes and the status pills need to sit one cell left of the visible border.
#[test]
fn render_default_pane_frame_keeps_right_border_for_overlong_agent_status() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(36, 6).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let bottom_pane_id = window.panes()[1].id.to_string();
    let inputs = window
        .panes()
        .iter()
        .map(|pane| PaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: vec![format!("pane{}", pane.index)],
        })
        .collect::<Vec<_>>();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        bottom_pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5-with-an-intentionally-long-name".to_string()),
            agent_reasoning: Some("extra-high".to_string()),
            agent_context_usage: Some("100%".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(
        rendered[2].chars().last(),
        Some('\u{2500}'),
        "merged pane frame should leave a right-edge border cell: {:?}",
        rendered[2]
    );
}

/// Verifies the default pane-frame agent status group includes a context usage
/// pill immediately before the live state pill.
///
/// Context pressure is what drives automatic compaction, so agent mode exposes
/// the percentage alongside model and reasoning metadata without making it a
/// selectable model/reasoning control.
#[test]
fn render_default_pane_frame_right_aligns_context_usage_before_agent_status() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(64, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            agent_routing: Some("auto:on".to_string()),
            agent_preset: Some("openai".to_string()),
            agent_context_usage: Some("87%".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert!(rendered[0].contains("gpt-5.5"), "{:?}", rendered[0]);
    assert!(rendered[0].contains(" high "), "{:?}", rendered[0]);
    assert!(rendered[0].contains(" route "), "{:?}", rendered[0]);
    assert!(rendered[0].contains(" 87% "), "{:?}", rendered[0]);
    assert!(rendered[0].contains(" running "), "{:?}", rendered[0]);
    assert!(
        !rendered[0].contains("openai"),
        "default pane frame should not render the preset pill: {:?}",
        rendered[0]
    );
}

/// Verifies that the built-in pane frame keeps agent status on the right side
/// without duplicating the working-directory pill now owned by the window
/// status area.
#[test]
fn render_default_pane_frame_right_aligns_agent_status_without_pwd() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(72, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            current_working_directory: Some("~/repos/mezzanine".to_string()),
            mode: Some("agent".to_string()),
            agent_name: Some("manager".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert!(rendered[0].contains("gpt-5.5   high   running"));
    assert!(!rendered[0].contains("~/repos/mezzanine"));
}

/// Verifies that the built-in pane frame styles each right-aligned agent status
/// field with a separate themed span and animates active work status. This keeps
/// model, reasoning, and state changes visually distinct while pane titles carry
/// subagent names.
#[test]
fn render_default_pane_frame_agent_status_uses_separate_themed_pills_without_name() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(84, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            current_working_directory: Some("~/repos/mezzanine".to_string()),
            mode: Some("agent".to_string()),
            agent_name: Some("Nova".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            agent_thinking: Some("on".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    frame_context.animation_tick_ms = 720;
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
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

    assert_eq!(
        view.lines[0],
        " 0 shell                                       gpt-5.5   high   thinking   running  "
    );
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.start == 0
            && span.length == " 0 shell  ".len()
            && span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
    }));
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
            && span.length == " gpt-5.5 ".len()
    }));
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.rendition.background == Some(TerminalColor::Rgb(0xe6, 0xc3, 0x84))
            && span.length == " high ".len()
    }));
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.rendition.background == Some(TerminalColor::Rgb(0xe6, 0xc3, 0x84))
            && span.length == " thinking ".len()
    }));
    assert!(!view.lines[0].contains("Nova"));
    assert!(!view.lines[0].contains("~/repos/mezzanine"));
    let status_start = display_column_for_fragment(&view.lines[0], "running");
    let status_end = status_start + "running".len();
    let status_backgrounds = view.line_style_spans[0]
        .iter()
        .filter(|span| {
            span.start < status_end && span.start.saturating_add(span.length) > status_start
        })
        .filter_map(|span| span.rendition.background)
        .collect::<Vec<_>>();
    assert!(
        status_backgrounds.len() > 1,
        "{:?}",
        view.line_style_spans[0]
    );
    assert!(
        status_backgrounds
            .iter()
            .any(|color| *color != TerminalColor::Rgb(0x7e, 0x9c, 0xd8)),
        "{status_backgrounds:?}"
    );
    assert!(
        !status_backgrounds.contains(&TerminalColor::Rgb(0xe6, 0xc3, 0x84)),
        "running scan should derive a harmonious range from the running color instead of reusing the reasoning accent: {status_backgrounds:?}"
    );
}

/// Verifies active agent status animation uses a wider theme-relative color
/// range across all built-in palettes.
///
/// The scan is derived from the running-status background with neighboring
/// hues, so each theme should produce multiple related true-color backgrounds
/// with visible separation from the base color without borrowing an unrelated
/// pill accent.
#[test]
fn render_active_agent_status_gradient_uses_theme_relative_harmony() {
    fn rgb_distance(left: TerminalColor, right: TerminalColor) -> i32 {
        let TerminalColor::Rgb(left_red, left_green, left_blue) = left else {
            panic!("expected true-color left background: {left:?}");
        };
        let TerminalColor::Rgb(right_red, right_green, right_blue) = right else {
            panic!("expected true-color right background: {right:?}");
        };
        (i32::from(left_red) - i32::from(right_red)).abs()
            + (i32::from(left_green) - i32::from(right_green)).abs()
            + (i32::from(left_blue) - i32::from(right_blue)).abs()
    }

    for name in BUILTIN_UI_THEME_NAMES {
        let definition =
            builtin_ui_theme_definition(name).unwrap_or_else(|| panic!("missing theme {name}"));
        let theme = resolve_ui_theme(name, definition).expect("built-in theme must resolve");
        let mut ids = IdFactory::default();
        let window = Window::new(&mut ids, 0, "main", Size::new(62, 3).unwrap());
        let pane_id = window.panes()[0].id.to_string();
        let mut frame_context = TerminalFrameContext::default();
        frame_context.panes.insert(
            pane_id,
            TerminalPaneFrameContext {
                mode: Some("agent".to_string()),
                agent_name: Some("manager".to_string()),
                agent_status: Some("running".to_string()),
                agent_model: Some("gpt-5.5".to_string()),
                agent_reasoning: Some("high".to_string()),
                ..TerminalPaneFrameContext::default()
            },
        );
        frame_context.animation_tick_ms = 1440;
        let config = TerminalClientLoopConfig {
            frame_context,
            window_frames_enabled: false,
            pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
            ui_theme: theme.clone(),
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
        let status_start = display_column_for_fragment(&view.lines[0], "running");
        let status_end = status_start + "running".len();
        let mut unique_backgrounds = Vec::<TerminalColor>::new();
        for background in view.line_style_spans[0]
            .iter()
            .filter(|span| {
                span.start < status_end && span.start.saturating_add(span.length) > status_start
            })
            .filter_map(|span| span.rendition.background)
        {
            if !unique_backgrounds.contains(&background) {
                unique_backgrounds.push(background);
            }
        }

        assert!(
            unique_backgrounds.len() >= 3,
            "{name} should animate with a multi-stop gradient: {unique_backgrounds:?}"
        );
        assert!(
            unique_backgrounds.iter().any(|color| rgb_distance(
                *color,
                theme.colors.agent_status_running.background
            ) >= 30),
            "{name} should visibly widen the running-status range from its base color: {unique_backgrounds:?}"
        );
        assert!(
            !unique_backgrounds.contains(&theme.colors.agent_reasoning.background),
            "{name} should not reuse the reasoning pill accent as the running scan highlight"
        );
    }
}

/// Verifies reduced-motion mode keeps active agent statuses static while
/// preserving the ordinary running-status color category.
///
/// Users on slow terminals or who prefer no animation should still see the
/// active status pill, but its style should not vary per cell or per frame
/// tick.
#[test]
fn render_reduced_motion_agent_status_uses_static_running_style() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(62, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext {
        reduced_motion: true,
        animation_tick_ms: 1440,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
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
    let status_start = display_column_for_fragment(&view.lines[0], "running");
    let status_end = status_start + "running".len();
    let unique_backgrounds = view.line_style_spans[0]
        .iter()
        .filter(|span| {
            span.start < status_end && span.start.saturating_add(span.length) > status_start
        })
        .filter(|span| span.length < usize::from(window.size.columns))
        .filter_map(|span| span.rendition.background)
        .fold(Vec::<TerminalColor>::new(), |mut colors, background| {
            if !colors.contains(&background) {
                colors.push(background);
            }
            colors
        });

    assert_eq!(
        unique_backgrounds,
        vec![config.ui_theme.colors.agent_status_running.background]
    );
}

/// Verifies that a parent agent waiting on joined child agents renders an
/// explicit `waiting` status with the same animated running-status treatment.
///
/// The status text should distinguish subagent joins from approval blocks, and
/// the animation should continue to communicate that work is still active.
#[test]
fn render_default_pane_frame_agent_status_waiting_uses_running_scan() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(56, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_name: Some("manager".to_string()),
            agent_status: Some("waiting".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    frame_context.animation_tick_ms = 720;
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
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

    assert_eq!(
        view.lines[0],
        " 0 shell                      gpt-5.5   high   waiting  "
    );
    let status_start = display_column_for_fragment(&view.lines[0], "waiting");
    let status_end = status_start + "waiting".len();
    let status_backgrounds = view.line_style_spans[0]
        .iter()
        .filter(|span| {
            span.start < status_end && span.start.saturating_add(span.length) > status_start
        })
        .filter_map(|span| span.rendition.background)
        .collect::<Vec<_>>();
    assert!(
        status_backgrounds.len() > 1,
        "{:?}",
        view.line_style_spans[0]
    );
    assert!(
        status_backgrounds
            .iter()
            .any(|color| *color != TerminalColor::Rgb(0x7e, 0x9c, 0xd8)),
        "{status_backgrounds:?}"
    );
}

/// Verifies that the routing substate reuses the animated running treatment so
/// auto-sizing stays visibly active while the router chooses a model.
#[test]
fn render_default_pane_frame_agent_status_routing_uses_running_scan() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(56, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_name: Some("manager".to_string()),
            agent_status: Some("routing".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    frame_context.animation_tick_ms = 720;
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
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
    assert_eq!(
        view.lines[0],
        " 0 shell                      gpt-5.5   high   routing  "
    );
    let status_start = display_column_for_fragment(&view.lines[0], "routing");
    let status_end = status_start + "routing".len();
    let status_backgrounds = view.line_style_spans[0]
        .iter()
        .filter(|span| {
            span.start < status_end && span.start.saturating_add(span.length) > status_start
        })
        .filter_map(|span| span.rendition.background)
        .collect::<Vec<_>>();
    assert!(
        status_backgrounds.len() > 1,
        "{:?}",
        view.line_style_spans[0]
    );
    assert!(
        status_backgrounds
            .iter()
            .any(|color| *color != TerminalColor::Rgb(0x7e, 0x9c, 0xd8)),
        "{status_backgrounds:?}"
    );
}

/// Verifies stopped agent turns use a muted status treatment instead of the
/// failed/error colors.
///
/// Stopping a turn is often user-directed control flow, so it should remain
/// distinguishable from a failed action without competing visually with real
/// errors in the pane frame.
#[test]
fn render_default_pane_frame_agent_status_stopped_is_muted() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(48, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_name: Some("manager".to_string()),
            agent_status: Some("stopped".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
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
    let status_start = display_column_for_fragment(&view.lines[0], "stopped");
    let status_background = view.line_style_spans[0]
        .iter()
        .rev()
        .find(|span| {
            span.start <= status_start && span.start.saturating_add(span.length) > status_start
        })
        .and_then(|span| span.rendition.background)
        .unwrap();

    assert_eq!(
        status_background,
        config.ui_theme.colors.agent_status_idle.background
    );
    assert_ne!(
        status_background,
        config.ui_theme.colors.agent_status_failed.background
    );
}

/// Verifies that scrollback position owns the right side of the default pane
/// header while copy-mode is away from the live bottom.
#[test]
fn render_default_pane_frame_scroll_position_replaces_agent_info() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(32, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            agent_status: Some("running".to_string()),
            agent_model: Some("default".to_string()),
            history_position: Some("4/20".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0], " 0 shell                   4/20 ");
    assert!(!rendered[0].contains('─'), "{}", rendered[0]);
    assert_eq!(rendered[1], "body                            ");
    assert!(!rendered[0].contains("running"), "{}", rendered[0]);
    assert!(!rendered[0].contains("default"), "{}", rendered[0]);
}

/// Verifies that the top pane status row uses the theme background instead of
/// box-drawing fill and carries the dedicated scroll-indicator background while
/// the scrollback position is visible.
#[test]
fn render_default_pane_frame_scroll_position_has_background_without_box_drawing_fill() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(32, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            history_position: Some("4/20".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
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

    assert_eq!(view.lines[0], " 0 shell                   4/20 ");
    assert!(!view.lines[0].contains('─'), "{}", view.lines[0]);
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.start == 27
            && span.length == 4
            && span.rendition.background == Some(TerminalColor::Rgb(0xe6, 0xc3, 0x84))
    }));
}
