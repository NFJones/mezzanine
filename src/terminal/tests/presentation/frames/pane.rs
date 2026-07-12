//! Regression tests for terminal presentation frames pane behavior.

use crate::ids::IdFactory;
use crate::layout::{PaneGeometry, SplitDirection};
use crate::terminal::tests::fixtures::display_column_for_fragment;
use crate::terminal::{
    BTreeMap, ClientViewRole, DEFAULT_PANE_FRAME_TEMPLATE, PaneAgentStatusField, PaneRenderInput,
    Size, TerminalClientLoopConfig, TerminalFrameContext, TerminalFramePosition,
    TerminalFrameRenderOptions, TerminalFrameStyle, TerminalPaneFrameContext, Window,
    pane_frame_agent_status_pillbox_cells, render_attached_client_view,
    render_window_with_pane_frame_template, rendered_pane_geometries,
};
use mez_terminal::TerminalColor;

/// Verifies render pane frame uses named template fields.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn render_pane_frame_uses_named_template_fields() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(18, 2).unwrap());
    window.panes_mut()[0].title = "shell\u{1b}[31m".to_string();
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &TerminalFrameContext::default(),
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            "#{pane.index}|#{pane.title}|#{pane.id}|#{missing.field}",
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0].trim_end(), format!("0|shell[31m|{pane_id}|"));
    assert_eq!(rendered[1], "body              ");
}

/// Verifies render pane frame template fits narrow panes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn render_pane_frame_template_fits_narrow_panes() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(8, 2).unwrap());
    let inputs = vec![PaneRenderInput {
        pane_id: window.panes()[0].id.to_string(),
        lines: vec!["body".to_string()],
    }];

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &TerminalFrameContext::default(),
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            "#{pane.index}:#{pane.title}:#{pane.size}",
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0], "0:shell:");
}

/// Verifies that runtime-supplied frame context values are available through
/// the required named window and pane frame fields without leaking control
/// characters into the rendered terminal frame text.
#[test]
fn render_frame_templates_use_runtime_context_fields() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(120, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext {
        session_id: Some("$1".to_string()),
        policy_mode: Some("full-access".to_string()),
        pending_observer_count: 1,
        ..TerminalFrameContext::default()
    };
    frame_context
        .window_agent_active_counts
        .insert(window.id.to_string(), 2);
    frame_context
        .window_unread_message_counts
        .insert(window.id.to_string(), 3);
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            primary_pid: Some(4242),
            process_name: Some("bash\u{1b}[31m".to_string()),
            current_working_directory: Some("~/repo\u{1b}[31m".to_string()),
            mode: Some("copy".to_string()),
            agent_id: Some(format!("agent-{pane_id}")),
            agent_name: Some("manager".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("default".to_string()),
            history_position: Some("scroll:4".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(
            true,
            "#{session.id}|#{agent.active_count}|#{message.unread_count}",
            TerminalFramePosition::Top,
        ),
        TerminalFrameRenderOptions::plain(
            true,
            "#{session.id}|#{pane.primary_pid}|#{pane.process_name}|#{pane.pwd}|#{pane.mode}|#{agent.id}|#{agent.name}|#{agent.status}|#{agent.model}|#{policy.mode}|#{observer.pending_count}|#{history.position}",
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0].trim_end(), "$1|2|3");
    assert_eq!(
        rendered[1].trim_end(),
        format!(
            "$1|4242|bash[31m|~/repo[31m|copy|agent-{pane_id}|manager|running|default|full-access|1|scroll:4"
        )
    );
}

/// Verifies that the built-in default pane frame follows the spec guidance by
/// rendering pane identity without an idle or running agent marker. Agent
/// fields remain available only when users explicitly put them in a template.
#[test]
fn render_default_pane_frame_omits_agent_info() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(32, 2).unwrap());
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

    assert_eq!(rendered[0], format!("{}{}", " 0 shell ", " ".repeat(23)));
    assert!(!rendered[0].contains("running"), "{}", rendered[0]);
    assert!(!rendered[0].contains("default"), "{}", rendered[0]);
}

/// Verifies render explicit pane frame template can show agent info.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn render_explicit_pane_frame_template_can_show_agent_info() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(32, 2).unwrap());
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
            "#{pane.index}: #{pane.title} #{agent.status} #{agent.model}",
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0].trim_end(), "0: shell running default");
}

/// Verifies that the built-in pane frame leaves working-directory display to
/// the window status area outside agent mode.
#[test]
fn render_default_pane_frame_omits_pwd_in_normal_mode() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(40, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            current_working_directory: Some("~/repo".to_string()),
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

    assert_eq!(rendered[0], format!("{}{}", " 0 shell ", " ".repeat(31)));
}

/// Verifies that the default pane-frame agent pills expose mouse hit cells
/// across their padded pill surfaces. The picker and toggle paths rely on
/// these cells rather than text parsing, so this protects both visual spacing
/// and click targeting as one contract.
#[test]
fn render_default_pane_frame_agent_model_and_reasoning_pills_are_clickable() {
    fn cells_for_field(
        cells: &[crate::terminal::MousePaneAgentStatusCell],
        field: PaneAgentStatusField,
    ) -> Vec<u16> {
        cells
            .iter()
            .filter(|cell| cell.field == field)
            .map(|cell| cell.column)
            .collect::<Vec<_>>()
    }

    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(80, 3).unwrap());
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
            agent_thinking: Some("on".to_string()),
            agent_routing: Some("auto:on".to_string()),
            agent_context_usage: Some("42%".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    frame_context.policy_mode = Some("full-access".to_string());
    let geometries = rendered_pane_geometries(&window, false).unwrap();

    let cells = pane_frame_agent_status_pillbox_cells(
        &window,
        &frame_context,
        DEFAULT_PANE_FRAME_TEMPLATE,
        TerminalFramePosition::Top,
        0,
        &geometries,
    );

    for field in [
        PaneAgentStatusField::Model,
        PaneAgentStatusField::Reasoning,
        PaneAgentStatusField::Thinking,
        PaneAgentStatusField::Routing,
        PaneAgentStatusField::ApprovalPolicy,
    ] {
        assert!(
            !cells_for_field(&cells, field).is_empty(),
            "{field:?} should expose clickable pane-frame cells: {cells:?}"
        );
    }
    let approval_columns = cells_for_field(&cells, PaneAgentStatusField::ApprovalPolicy);
    let reasoning_columns = cells_for_field(&cells, PaneAgentStatusField::Reasoning);
    let thinking_columns = cells_for_field(&cells, PaneAgentStatusField::Thinking);
    let routing_columns = cells_for_field(&cells, PaneAgentStatusField::Routing);
    assert!(
        approval_columns.iter().max() > routing_columns.iter().min(),
        "approval and routing pills should occupy distinct cells: {cells:?}"
    );
    assert!(
        reasoning_columns.iter().max() < thinking_columns.iter().min()
            && thinking_columns.iter().max() < routing_columns.iter().min(),
        "thinking should sit between reasoning and routing pills: {cells:?}"
    );
}

/// Verifies that split-pane box drawing glyphs carry only a foreground color
/// and use the active-pane border color when the glyph encloses the active
/// pane. Background fill remains reserved for text spans on frame bars.
#[test]
fn render_active_pane_border_glyphs_are_foreground_only() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(24, 4).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let config = TerminalClientLoopConfig {
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
    let border_column = display_column_for_fragment(&view.lines[0], "\u{2502}");
    let border_span = view.line_style_spans[0]
        .iter()
        .find(|span| span.start == border_column)
        .unwrap();

    assert_eq!(
        border_span.rendition.foreground,
        Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8))
    );
    assert_eq!(border_span.rendition.background, None);
}

/// Verifies that pane status rows merged into divider rows keep backgrounds
/// only on title/status pills. The horizontal divider itself and its boundary
/// junctions remain foreground-only connected box-drawing cells so split lines
/// do not become filled status bars or lose their interior tee glyphs.
#[test]
fn render_merged_pane_frame_fills_status_bar_and_preserves_vertical_separators() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(28, 6).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let config = TerminalClientLoopConfig {
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

    let merged_row = view
        .lines
        .iter()
        .position(|line| line.contains(" 2 she"))
        .expect("bottom-right pane frame should merge into divider row");
    let frame_text = " 2 she";
    assert!(view.lines[merged_row].contains(frame_text));
    let title_span = view.line_style_spans[merged_row]
        .iter()
        .find(|span| {
            span.length >= frame_text.len()
                && span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
        })
        .copied()
        .expect("merged status title should carry the title-pill background");
    let horizontal_column = view.lines[merged_row]
        .chars()
        .position(|ch| ch == '\u{2500}')
        .expect("merged divider row should retain horizontal box drawing fill");
    let horizontal_span = view.line_style_spans[merged_row]
        .iter()
        .rev()
        .find(|span| {
            horizontal_column >= span.start
                && horizontal_column < span.start.saturating_add(span.length)
        })
        .expect("horizontal divider fill should be styled");
    assert_eq!(horizontal_span.rendition.background, None);
    assert!(
        view.line_style_spans[merged_row].iter().any(|span| {
            span.start == title_span.start
                && span.length >= frame_text.len()
                && span.rendition.foreground == Some(TerminalColor::Rgb(0x00, 0x00, 0x00))
                && span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
        }),
        "{:?}",
        view.line_style_spans[merged_row]
    );

    let junction_column = title_span.start.saturating_sub(1);
    assert_eq!(
        view.lines[merged_row].chars().nth(junction_column),
        Some('\u{251c}')
    );
    let junction_span = view.line_style_spans[merged_row]
        .iter()
        .rev()
        .find(|span| {
            junction_column >= span.start
                && junction_column < span.start.saturating_add(span.length)
        })
        .expect("merged status junction should be styled");
    assert_eq!(junction_span.rendition.background, None);

    let vertical_row = view
        .lines
        .iter()
        .position(|line| line.contains(" 0 shell") && line.contains(" 1 shell"))
        .unwrap();
    let vertical_column = view.lines[vertical_row]
        .chars()
        .position(|ch| ch == '\u{2502}')
        .unwrap();
    let vertical_span = view.line_style_spans[vertical_row]
        .iter()
        .rev()
        .find(|span| {
            vertical_column >= span.start
                && vertical_column < span.start.saturating_add(span.length)
        })
        .expect("vertical separator should be styled");
    assert_eq!(vertical_span.rendition.background, None);
}

/// Verifies merged pane-frame rows preserve right-side tee intersections when
/// the pane status region ends at a full-height neighboring pane's divider.
#[test]
fn render_merged_pane_frame_preserves_right_side_tee_junction() {
    let window = super::super::layout::window_from_test_geometries(
        Size::new(28, 6).unwrap(),
        vec![
            PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: 14,
                rows: 3,
            },
            PaneGeometry {
                index: 1,
                column: 0,
                row: 3,
                columns: 14,
                rows: 3,
            },
            PaneGeometry {
                index: 2,
                column: 14,
                row: 0,
                columns: 14,
                rows: 6,
            },
        ],
    );
    let config = TerminalClientLoopConfig {
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

    let merged_row = 2;
    let junction_column = 13;
    assert_eq!(
        view.lines[merged_row].chars().nth(junction_column),
        Some('\u{2524}'),
        "{:?}",
        view.lines[merged_row]
    );
    let junction_span = view.line_style_spans[merged_row]
        .iter()
        .rev()
        .find(|span| {
            junction_column >= span.start
                && junction_column < span.start.saturating_add(span.length)
        })
        .expect("right-side tee junction should be styled");

    assert_eq!(junction_span.rendition.background, None);
}

/// Verifies that configured frame positions can place pane and window frame
/// rows after body content while preserving the authoritative window height.
#[test]
fn render_frame_positions_can_place_frames_at_bottom() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(12, 3).unwrap());
    let inputs = vec![PaneRenderInput {
        pane_id: window.panes()[0].id.to_string(),
        lines: vec!["body".to_string()],
    }];

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &TerminalFrameContext::default(),
        TerminalFrameRenderOptions::plain(true, "window", TerminalFramePosition::Bottom),
        TerminalFrameRenderOptions::plain(true, "pane", TerminalFramePosition::Bottom),
    )
    .unwrap();

    assert_eq!(
        rendered,
        vec!["body        ", "pane        ", "window      "]
    );
}

/// Verifies that configured frame styles are exposed as styled-line spans so
/// attached terminal output can replay them as SGR instead of plain text only.
/// Pane title rows include a subtle full-row theme fill and a stronger text
/// span for the configured title style.
#[test]
fn render_frame_styles_apply_to_styled_frame_lines() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(12, 3).unwrap());
    let mut config = TerminalClientLoopConfig {
        window_frames_enabled: true,
        window_frame_template: "window".to_string(),
        window_frame_style: TerminalFrameStyle::Inverse,
        pane_frames_enabled: true,
        pane_frame_template: "pane".to_string(),
        pane_frame_style: TerminalFrameStyle::Bold,
        ..TerminalClientLoopConfig::default()
    };
    config.window_frame_position = TerminalFramePosition::Top;
    config.pane_frame_position = TerminalFramePosition::Top;

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(view.line_style_spans[0][0].rendition.inverse);
    assert_eq!(view.line_style_spans[1][0].length, 12);
    assert!(
        view.line_style_spans[1]
            .iter()
            .any(|span| { span.length == 4 && span.rendition.bold })
    );
}
