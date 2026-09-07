//! Regression tests for terminal presentation frames pane behavior.

use crate::host::terminal::render::pane_frame_status_diagnostic_projection;
use crate::host::terminal::tests::fixtures::display_column_for_fragment;
use crate::host::terminal::{
    BTreeMap, DEFAULT_PANE_FRAME_TEMPLATE, FramePillColorOverrides, PaneAgentStatusField,
    PaneRenderInput, PaneStatusAction, PaneStatusField, PaneStatusOverflowPolicy,
    PaneStatusPillDefinition, PaneStatusRail, PaneStatusStyle, TerminalClientLoopConfig,
    TerminalFrameContext, TerminalFrameRenderOptions, TerminalPaneFrameContext,
    pane_frame_agent_status_pillbox_cells, render_attached_client_view,
    render_window_with_pane_frame_template,
};
use mez_core::ids::IdFactory;
use mez_mux::layout::{PaneGeometry, Size, SplitDirection, Window};
use mez_mux::presentation::ClientViewRole;
use mez_mux::presentation::{
    TerminalFramePosition, TerminalFrameStyle, WindowPresentationOptions, plan_window_presentation,
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

    assert_eq!(rendered[0].trim_end(), format!(" 0|shell[31m|{pane_id}|"));
    assert_eq!(rendered[1], "body              ");
}

/// Verifies pane-status rails accept the same stable pane identity field as
/// pane frame templates. Repeated occurrences must remain visible so a live
/// rail replacement produces a distinct client presentation.
#[test]
fn render_pane_status_rail_uses_pane_identity() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(24, 2).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.pane_status.left_status.clear();
    frame_context.pane_status.right_status = "#{pane.id} #{pane.id}".to_string();

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

    assert_eq!(rendered[0].matches(&pane_id).count(), 2, "{}", rendered[0]);
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

    assert_eq!(rendered[0], " 0:she  ");
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
        ..TerminalFrameContext::default()
    };
    frame_context.pane_status.left_status.clear();
    frame_context.pane_status.right_status.clear();
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
            policy_mode: Some("full-access".to_string()),
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
            "#{session.id}|#{pane.primary_pid}|#{pane.process_name}|#{pane.pwd}|#{pane.mode}|#{agent.id}|#{agent.name}|#{agent.status}|#{agent.model}|#{policy.mode}|#{history.position}",
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0].trim_end(), "$1|2|3");
    assert_eq!(
        rendered[1].trim_end(),
        format!(
            " $1|4242|bash[31m|~/repo[31m|copy|agent-{pane_id}|manager|running|default|full-access|scroll:4"
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

/// Verifies active determinate terminal progress appears beside the default title pill.
///
/// The progress pill is pane-local presentation state and must disappear without
/// leaving blank residue when that state is absent.
#[test]
fn render_default_pane_frame_shows_active_terminal_progress() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(32, 2).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            terminal_progress_percent: Some(42),
            ..TerminalPaneFrameContext::default()
        },
    );

    let render = |context: &TerminalFrameContext| {
        render_window_with_pane_frame_template(
            &window,
            &inputs,
            context,
            TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
            TerminalFrameRenderOptions::plain(
                true,
                DEFAULT_PANE_FRAME_TEMPLATE,
                TerminalFramePosition::Top,
            ),
        )
        .unwrap()
    };
    assert!(render(&frame_context)[0].starts_with(" 0 shell   42% "));
    frame_context
        .panes
        .get_mut(&pane_id)
        .unwrap()
        .terminal_progress_percent = None;
    assert_eq!(
        render(&frame_context)[0],
        format!("{}{}", " 0 shell ", " ".repeat(23))
    );
}

/// Verifies custom pane templates can opt into the progress scalar explicitly.
///
/// Custom templates remain stable by default while `pane.progress` exposes the
/// active percentage without coupling template expansion to OSC protocol types.
#[test]
fn render_custom_pane_frame_can_show_terminal_progress() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(24, 2).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.pane_status.left_status.clear();
    frame_context.pane_status.right_status.clear();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            terminal_progress_percent: Some(7),
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
            "#{pane.title} #{pane.progress}",
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();
    assert_eq!(rendered[0].trim_end(), " shell 7%");
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

    assert_eq!(rendered[0].trim_end(), " 0: shell running default");
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
        cells: &[crate::host::terminal::MousePaneAgentStatusCell],
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
            agent_planning: Some("on".to_string()),
            agent_routing: Some("auto:on".to_string()),
            agent_context_usage: Some("42%".to_string()),
            policy_mode: Some("full-access".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let plan = plan_window_presentation(
        &window,
        WindowPresentationOptions {
            pane_frames_visible: true,
            ..WindowPresentationOptions::default()
        },
    )
    .unwrap();

    let cells = pane_frame_agent_status_pillbox_cells(
        &window,
        &frame_context,
        DEFAULT_PANE_FRAME_TEMPLATE,
        &plan,
    );

    for field in [
        PaneAgentStatusField::Model,
        PaneAgentStatusField::Reasoning,
        PaneAgentStatusField::Thinking,
        PaneAgentStatusField::Planning,
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
    let planning_columns = cells_for_field(&cells, PaneAgentStatusField::Planning);
    let routing_columns = cells_for_field(&cells, PaneAgentStatusField::Routing);
    assert!(
        approval_columns.iter().max() > routing_columns.iter().min(),
        "approval and routing pills should occupy distinct cells: {cells:?}"
    );
    assert!(
        reasoning_columns.iter().max() < thinking_columns.iter().min()
            && thinking_columns.iter().max() < planning_columns.iter().min()
            && planning_columns.iter().max() < routing_columns.iter().min(),
        "planning should sit between thinking and routing pills: {cells:?}"
    );
}

/// Verifies duplicate named built-in occurrences retain distinct semantic
/// identities while sharing their stable pane owner, configured style, and
/// built-in action. A read-only occurrence renders but exposes no hit cells.
#[test]
fn render_configured_pane_status_occurrences_keep_semantic_identity() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(120, 3).unwrap());
    let pane_id = window.panes()[0].id.clone();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.pane_status.left_status.clear();
    frame_context.pane_status.right_status =
        "#{pill.model} #{pill.readonly} #{pill.model}".to_string();
    let mut model = PaneStatusPillDefinition::builtin(PaneStatusField::AgentModel);
    model.label = Some("Model".to_string());
    model.style = PaneStatusStyle::AgentStatusFailed;
    model.color_overrides = FramePillColorOverrides {
        foreground: Some("primary_text".to_string()),
        background: Some("primary".to_string()),
    };
    model.priority = 80;
    let mut readonly = model.clone();
    readonly.label = Some("Read".to_string());
    readonly.action = PaneStatusAction::None;
    frame_context
        .pane_status
        .pills
        .insert("model".to_string(), model);
    frame_context
        .pane_status
        .pills
        .insert("readonly".to_string(), readonly);
    frame_context.panes.insert(
        pane_id.to_string(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_model: Some("gpt-5.6".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let plan = plan_window_presentation(
        &window,
        WindowPresentationOptions {
            pane_frames_visible: true,
            ..WindowPresentationOptions::default()
        },
    )
    .unwrap();
    let cells = pane_frame_agent_status_pillbox_cells(
        &window,
        &frame_context,
        DEFAULT_PANE_FRAME_TEMPLATE,
        &plan,
    );
    let ordinals = cells
        .iter()
        .map(|cell| cell.identity.occurrence.ordinal)
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(ordinals, [0, 2].into_iter().collect());
    assert!(cells.iter().all(|cell| {
        cell.identity.owner_pane_id == pane_id
            && cell.identity.occurrence.rail == PaneStatusRail::Right
            && cell.identity.style == PaneStatusStyle::AgentStatusFailed
            && cell.identity.color_overrides
                == FramePillColorOverrides {
                    foreground: Some("primary_text".to_string()),
                    background: Some("primary".to_string()),
                }
            && cell.identity.priority == 80
            && cell.identity.action == PaneStatusAction::Builtin(PaneAgentStatusField::Model)
    }));
}

/// Named pane pills apply independent palette channels after semantic style
/// selection, while bare fields retain their established theme rendition.
#[test]
fn render_named_pane_status_pills_apply_palette_channel_overrides() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(72, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.pane_status.left_status.clear();
    frame_context.pane_status.right_status =
        "#{pill.model} #{pill.reasoning} #{agent.reasoning}".to_string();
    let mut model = PaneStatusPillDefinition::builtin(PaneStatusField::AgentModel);
    model.color_overrides = FramePillColorOverrides {
        foreground: Some("test_foreground".to_string()),
        background: Some("test_background".to_string()),
    };
    let mut reasoning = PaneStatusPillDefinition::builtin(PaneStatusField::AgentReasoning);
    reasoning.style = PaneStatusStyle::AgentStatusFailed;
    reasoning.color_overrides.foreground = Some("test_foreground".to_string());
    frame_context.pane_status.pills.extend([
        ("model".to_string(), model),
        ("reasoning".to_string(), reasoning),
    ]);
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_model: Some("model-value".to_string()),
            agent_reasoning: Some("reason-value".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let foreground = TerminalColor::Rgb(0x12, 0x34, 0x56);
    let background = TerminalColor::Rgb(0x65, 0x43, 0x21);
    let mut config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        ..TerminalClientLoopConfig::default()
    };
    config
        .ui_theme
        .aliases
        .insert("test_foreground".to_string(), foreground);
    config
        .ui_theme
        .aliases
        .insert("test_background".to_string(), background);

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    let row = &view.lines[0];
    let spans = &view.line_style_spans[0];
    let model_column = display_column_for_fragment(row, "model-value");
    let named_reasoning_column = display_column_for_fragment(row, "reason-value");
    let bare_reasoning_column = row
        .rfind("reason-value")
        .expect("bare reasoning occurrence should render");

    assert!(spans.iter().any(|span| {
        model_column >= span.start
            && model_column < span.start.saturating_add(span.length)
            && span.rendition.foreground == Some(foreground)
            && span.rendition.background == Some(background)
    }));
    assert!(spans.iter().any(|span| {
        named_reasoning_column >= span.start
            && named_reasoning_column < span.start.saturating_add(span.length)
            && span.rendition.foreground == Some(foreground)
            && span.rendition.background
                == Some(config.ui_theme.colors.agent_status_failed.background)
    }));
    assert!(spans.iter().any(|span| {
        bare_reasoning_column >= span.start
            && bare_reasoning_column < span.start.saturating_add(span.length)
            && span.rendition == config.ui_theme.colors.agent_reasoning.rendition()
    }));
}

/// A foreground-only override preserves each animated running background, but
/// an explicit background makes the named occurrence static at that color.
#[test]
fn render_named_running_pane_status_override_controls_scan_precedence() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(48, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let foreground = TerminalColor::Rgb(0x12, 0x34, 0x56);
    let background = TerminalColor::Rgb(0x65, 0x43, 0x21);
    let render = |background_name: Option<&str>| {
        let mut frame_context = TerminalFrameContext::default();
        frame_context.pane_status.left_status.clear();
        frame_context.pane_status.right_status = "#{pill.status}".to_string();
        let mut status = PaneStatusPillDefinition::builtin(PaneStatusField::AgentStatus);
        status.color_overrides = FramePillColorOverrides {
            foreground: Some("test_foreground".to_string()),
            background: background_name.map(ToOwned::to_owned),
        };
        frame_context
            .pane_status
            .pills
            .insert("status".to_string(), status);
        frame_context.panes.insert(
            pane_id.clone(),
            TerminalPaneFrameContext {
                mode: Some("agent".to_string()),
                agent_status: Some("running".to_string()),
                ..TerminalPaneFrameContext::default()
            },
        );
        frame_context.animation_tick_ms = 720;
        let mut config = TerminalClientLoopConfig {
            frame_context,
            window_frames_enabled: false,
            pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
            ..TerminalClientLoopConfig::default()
        };
        config
            .ui_theme
            .aliases
            .insert("test_foreground".to_string(), foreground);
        config
            .ui_theme
            .aliases
            .insert("test_background".to_string(), background);
        render_attached_client_view(
            ClientViewRole::Primary,
            &window,
            &BTreeMap::new(),
            &config,
            window.size,
        )
        .unwrap()
        .unwrap()
    };

    let animated = render(None);
    let start = display_column_for_fragment(&animated.lines[0], "running");
    let end = start + "running".len();
    let animated_renditions = (start..end)
        .map(|column| {
            animated.line_style_spans[0]
                .iter()
                .rev()
                .find(|span| {
                    column >= span.start && column < span.start.saturating_add(span.length)
                })
                .expect("running status column should be styled")
                .rendition
        })
        .collect::<Vec<_>>();
    assert!(
        animated_renditions
            .iter()
            .all(|rendition| rendition.foreground == Some(foreground))
    );
    assert!(
        animated_renditions
            .windows(2)
            .any(|pair| pair[0].background != pair[1].background),
        "{animated_renditions:?}"
    );

    let static_view = render(Some("test_background"));
    let start = display_column_for_fragment(&static_view.lines[0], "running");
    let end = start + "running".len();
    let static_renditions = (start..end)
        .map(|column| {
            static_view.line_style_spans[0]
                .iter()
                .rev()
                .find(|span| {
                    column >= span.start && column < span.start.saturating_add(span.length)
                })
                .expect("static running status column should be styled")
                .rendition
        })
        .collect::<Vec<_>>();
    assert!(static_renditions.iter().all(|rendition| {
        rendition.foreground == Some(foreground) && rendition.background == Some(background)
    }));
}

/// Verifies explicit empty rails suppress every implicit progress, agent, and
/// history item while leaving the pane title row itself enabled.
#[test]
fn render_explicit_empty_pane_status_rails_have_no_implicit_items() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(48, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.pane_status.left_status.clear();
    frame_context.pane_status.right_status.clear();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            terminal_progress_percent: Some(40),
            agent_model: Some("gpt-5.6".to_string()),
            history_position: Some("scroll:4".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let plan = plan_window_presentation(
        &window,
        WindowPresentationOptions {
            pane_frames_visible: true,
            ..WindowPresentationOptions::default()
        },
    )
    .unwrap();

    assert!(
        pane_frame_agent_status_pillbox_cells(
            &window,
            &frame_context,
            DEFAULT_PANE_FRAME_TEMPLATE,
            &plan,
        )
        .is_empty()
    );
}

/// Verifies a narrow pane selects status controls as complete semantic pills
/// from the shared priority pool. The lower-priority control must expose no
/// hit cells, while every cell of the retained control remains actionable.
#[test]
fn render_narrow_pane_status_has_only_complete_priority_selected_hit_targets() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(20, 3).unwrap());
    let pane_id = window.panes()[0].id.clone();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.pane_status.left_status = "#{pill.low}".to_string();
    frame_context.pane_status.right_status = "#{pill.high}".to_string();
    frame_context.pane_status.overflow = PaneStatusOverflowPolicy::Hide;
    frame_context.pane_status.title_min_width = 8;
    let mut low = PaneStatusPillDefinition::builtin(PaneStatusField::AgentReasoning);
    low.priority = 10;
    let mut high = PaneStatusPillDefinition::builtin(PaneStatusField::AgentModel);
    high.priority = 90;
    frame_context
        .pane_status
        .pills
        .insert("low".to_string(), low);
    frame_context
        .pane_status
        .pills
        .insert("high".to_string(), high);
    frame_context.panes.insert(
        pane_id.to_string(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_model: Some("model".to_string()),
            agent_reasoning: Some("medium".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let plan = plan_window_presentation(
        &window,
        WindowPresentationOptions {
            pane_frames_visible: true,
            ..WindowPresentationOptions::default()
        },
    )
    .unwrap();

    let cells = pane_frame_agent_status_pillbox_cells(
        &window,
        &frame_context,
        DEFAULT_PANE_FRAME_TEMPLATE,
        &plan,
    );

    assert!(
        cells
            .iter()
            .all(|cell| cell.field == PaneAgentStatusField::Model),
        "lower-priority reasoning pill must not expose clipped cells: {cells:?}"
    );
    assert_eq!(cells.len(), " model ".chars().count());
    assert_eq!(
        cells.iter().map(|cell| cell.column).collect::<Vec<_>>(),
        (12..19).collect::<Vec<_>>()
    );
}

/// Verifies diagnostics reuse the authoritative whole-pill layout while also
/// retaining configured occurrences that rendering omits before fitting.
#[test]
fn pane_status_diagnostic_projects_unavailable_conditioned_and_overflowed_occurrences() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(21, 3).unwrap());
    let pane = &window.panes()[0];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.pane_status.left_status = "#{pill.missing} #{pill.shell_only}".to_string();
    frame_context.pane_status.right_status = "#{pill.low} #{pill.high}".to_string();
    frame_context.pane_status.overflow = PaneStatusOverflowPolicy::Menu;
    frame_context.pane_status.title_min_width = 8;
    let missing = PaneStatusPillDefinition::builtin(PaneStatusField::AgentPlanning);
    let mut shell_only = PaneStatusPillDefinition::builtin(PaneStatusField::AgentModel);
    shell_only.when = vec![crate::host::terminal::PaneStatusCondition::ShellView];
    let mut low = PaneStatusPillDefinition::builtin(PaneStatusField::AgentReasoning);
    low.priority = 10;
    let mut high = PaneStatusPillDefinition::builtin(PaneStatusField::AgentModel);
    high.priority = 90;
    frame_context.pane_status.pills.extend([
        ("missing".to_string(), missing),
        ("shell_only".to_string(), shell_only),
        ("low".to_string(), low),
        ("high".to_string(), high),
    ]);
    frame_context.panes.insert(
        pane.id.to_string(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_model: Some("model".to_string()),
            agent_reasoning: Some("medium".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let diagnostic = pane_frame_status_diagnostic_projection(
        &window,
        pane,
        &frame_context,
        DEFAULT_PANE_FRAME_TEMPLATE,
        21,
        '─',
    );

    assert_eq!(diagnostic.owner_pane_id, pane.id);
    assert_eq!(diagnostic.pane_width_cells, 21);
    assert_eq!(diagnostic.title_min_width_cells, 8);
    assert_eq!(diagnostic.occurrences.len(), 4);
    assert_eq!(diagnostic.occurrences[0].source, "pill.missing");
    assert_eq!(diagnostic.occurrences[0].availability, "unavailable");
    assert_eq!(diagnostic.occurrences[1].availability, "condition-hidden");
    assert_eq!(diagnostic.occurrences[2].layout_state, Some("overflow"));
    assert_eq!(diagnostic.occurrences[3].layout_state, Some("full"));
    assert_eq!(diagnostic.occurrences[3].identity.owner_pane_id, pane.id);
    assert_eq!(diagnostic.occurrences[3].identity.occurrence.ordinal, 1);
    assert!(diagnostic.occurrences[3].full_cells >= diagnostic.occurrences[3].selected_cells);
    assert!(diagnostic.status_budget_cells >= diagnostic.status_used_cells);
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
        Some(TerminalColor::Rgb(0xbf, 0xff, 0x00))
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
                && span.rendition.background
                    == Some(config.ui_theme.colors.pane_frame_active.background)
        })
        .copied()
        .expect("active merged pane title should carry the title-pill background");
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
                && span.rendition.foreground
                    == Some(config.ui_theme.colors.pane_frame_active.foreground)
                && span.rendition.background
                    == Some(config.ui_theme.colors.pane_frame_active.background)
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
        vec!["body        ", " pane       ", "window      "]
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
            .any(|span| { span.length == 6 && span.rendition.bold })
    );
}
