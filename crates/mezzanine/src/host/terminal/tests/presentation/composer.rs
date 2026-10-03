//! State-aware composer fixtures over the production attached-view renderer.
//!
//! Decoration is display-only; these tests inspect geometry and state cues,
//! not measured usability, font appearance or universal terminal accessibility.

use crate::host::terminal::{
    TerminalClientLoopConfig, TerminalFrameContext, TerminalPaneFrameContext,
    agent_prompt_reserved_line_count, render_attached_client_view,
};
use crate::ui::readline::{ReadlinePrompt, ReadlinePromptKind};
use mez_core::IdFactory;
use mez_mux::layout::{Size, Window};
use mez_mux::presentation::{AgentComposerContext, ClientViewRole};
use std::collections::BTreeMap;

/// Builds a standalone pane with explicit composer facts and no passive rails.
fn view(
    size: Size,
    prompt: &ReadlinePrompt,
    context: AgentComposerContext,
    role: ClientViewRole,
) -> mez_mux::presentation::RenderedClientView {
    view_at_tick(size, prompt, context, role, 0)
}

/// Renders deterministic wave phases through the production attached adapter.
fn view_at_tick(
    size: Size,
    prompt: &ReadlinePrompt,
    context: AgentComposerContext,
    role: ClientViewRole,
    tick: u64,
) -> mez_mux::presentation::RenderedClientView {
    let window = Window::new(&mut IdFactory::default(), 0, "composer", size).unwrap();
    let pane_id = window.panes()[0].id.to_string();
    let mut frame = TerminalFrameContext {
        animation_tick_ms: tick,
        agent_status_wave_active: true,
        ..Default::default()
    };
    frame.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".into()),
            agent_prompt: Some(prompt.clone()),
            agent_composer: Some(context),
            agent_display_lines: vec!["executing (12s • esc to interrupt)".into()],
            ..Default::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context: frame,
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..Default::default()
    };
    render_attached_client_view(role, &window, &BTreeMap::new(), &config, size)
        .unwrap()
        .unwrap()
}

/// Only the active state label changes rendition across wave ticks. Draft,
/// header text, timer, help and geometry remain unchanged, including when
/// interrupt help is filtered from a read-only projection.
#[test]
fn composer_header_animates_only_active_status_label() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("draft 雪");
    for (role, intercepted) in [
        (ClientViewRole::Primary, false),
        (ClientViewRole::Observer, false),
        (ClientViewRole::Primary, true),
    ] {
        let context = AgentComposerContext {
            keys: intercepted.then_some(mez_mux::presentation::AgentComposerKeys {
                escape: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        let first = view_at_tick(
            Size::new(80, 24).unwrap(),
            &prompt,
            context.clone(),
            role,
            0,
        );
        let later = view_at_tick(Size::new(80, 24).unwrap(), &prompt, context, role, 720);
        if role == ClientViewRole::Observer {
            assert!(
                first
                    .lines
                    .iter()
                    .any(|line| line.contains("Read-only view"))
            );
            assert!(!first.cursor_visible);
        }
        assert_eq!(
            first
                .lines
                .iter()
                .any(|line| line.contains("esc to interrupt")),
            role == ClientViewRole::Primary && !intercepted
        );
        assert_eq!(first.lines, later.lines);
        assert_eq!(
            (first.cursor_row, first.cursor_column),
            (later.cursor_row, later.cursor_column)
        );
        let row = first
            .lines
            .iter()
            .position(|line| line.contains("executing"))
            .unwrap();
        assert_ne!(first.line_style_spans[row], later.line_style_spans[row]);
        let start_byte = first.lines[row].find("executing").unwrap();
        let start = unicode_width::UnicodeWidthStr::width(&first.lines[row][..start_byte]);
        let end = start + "executing".len();
        let at = |spans: &[mez_terminal::TerminalStyleSpan], column: usize| {
            spans
                .iter()
                .rev()
                .find(|span| column >= span.start && column < span.start + span.length)
                .map(|span| span.rendition)
                .unwrap_or_default()
        };
        for column in 0..80 {
            if column < start || column >= end {
                assert_eq!(
                    at(&first.line_style_spans[row], column),
                    at(&later.line_style_spans[row], column)
                );
            }
        }
        assert!(
            later.line_style_spans[row]
                .iter()
                .all(|span| span.rendition.background.is_none())
        );
    }
}

/// Live status remains visible while drafting without stealing input/cursor;
/// idle/active transitions and duration changes do not change reserved rows.
#[test]
fn composer_keeps_live_status_and_exact_draft_in_comfortable_geometry() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("check 雪\nsecond line");
    let original = prompt.clone();
    for size in [Size::new(120, 40).unwrap(), Size::new(80, 24).unwrap()] {
        let active = view(
            size,
            &prompt,
            AgentComposerContext {
                guides_active_task: true,
                ..Default::default()
            },
            ClientViewRole::Primary,
        );
        let idle = view(
            size,
            &prompt,
            AgentComposerContext::default(),
            ClientViewRole::Primary,
        );
        assert!(
            active
                .lines
                .iter()
                .any(|line| line.contains("Guide this task") && line.contains("12s"))
        );
        assert!(active.lines.iter().any(|line| line.contains("Enter guide")));
        assert!(idle.lines.iter().any(|line| line.contains("Ask Mez")));
        assert_eq!(active.cursor_row, idle.cursor_row);
        assert!(active.lines[active.cursor_row].contains("second line"));
        assert_eq!(prompt, original);
        let observer = view(
            size,
            &prompt,
            AgentComposerContext::default(),
            ClientViewRole::Observer,
        );
        assert!(!observer.cursor_visible);
        assert!(
            observer
                .lines
                .iter()
                .any(|line| line.contains("Read-only view"))
        );
        assert!(
            !observer
                .lines
                .iter()
                .any(|line| line.contains("Enter send"))
        );
    }
}

/// Compact panes retain the existing editor footprint; large multiline input
/// stays bounded at half the pane, with one shared reservation/cursor layout.
#[test]
fn composer_reservation_is_responsive_and_bounded() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    let mut context = TerminalPaneFrameContext {
        agent_prompt: Some(prompt.clone()),
        agent_composer: Some(AgentComposerContext::default()),
        ..Default::default()
    };
    assert_eq!(agent_prompt_reserved_line_count(80, 24, Some(&context)), 3);
    assert_eq!(agent_prompt_reserved_line_count(40, 12, Some(&context)), 1);
    assert_eq!(agent_prompt_reserved_line_count(80, 6, Some(&context)), 1);
    prompt.buffer.set_line("line\n".repeat(40));
    context.agent_prompt = Some(prompt);
    assert_eq!(agent_prompt_reserved_line_count(80, 24, Some(&context)), 12);
}

/// Help follows readline precedence: reverse search accepts without sending,
/// slash drafts execute commands, approval review does not imply prose approval,
/// and rejected-paste ownership advertises only its real reset route.
#[test]
fn composer_help_tracks_search_commands_approval_and_paste_discard() {
    let size = Size::new(120, 40).unwrap();
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.buffer.set_line("/help");
    let command = view(
        size,
        &prompt,
        AgentComposerContext::default(),
        ClientViewRole::Primary,
    );
    assert!(
        command
            .lines
            .iter()
            .any(|line| line.contains("Enter command"))
    );
    prompt.apply_terminal_input(b"\x12").unwrap();
    let search = view(
        size,
        &prompt,
        AgentComposerContext::default(),
        ClientViewRole::Primary,
    );
    assert!(
        search
            .lines
            .iter()
            .any(|line| line.contains("Enter accept"))
    );
    assert!(!search.lines.iter().any(|line| line.contains("Enter send")));
    let approval = view(
        size,
        &ReadlinePrompt::new(ReadlinePromptKind::Agent),
        AgentComposerContext {
            approval_pending: true,
            ..Default::default()
        },
        ClientViewRole::Primary,
    );
    assert!(
        approval
            .lines
            .iter()
            .any(|line| line.contains("/show-approvals review"))
    );
    let discard = view(
        size,
        &prompt,
        AgentComposerContext {
            paste_discard_pending: true,
            ..Default::default()
        },
        ClientViewRole::Primary,
    );
    assert!(
        discard
            .lines
            .iter()
            .any(|line| line.contains("Esc reset input"))
    );
}
