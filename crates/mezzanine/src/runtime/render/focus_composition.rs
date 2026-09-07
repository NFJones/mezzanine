//! Composite-only zen identity overlays, independent of navigation and timers.
//!
//! Each frame starts from current pane content. This adapter reads live records
//! without renewing them and paints bounded pills in authoritative coordinates;
//! normal viewport composition remains the sole clipping transform.

use super::{RuntimeSessionService, current_unix_millis};
use crate::host::terminal::{TerminalClientLoopConfig, render_focus_label};
use mez_mux::layout::Window;
use mez_mux::presentation::{
    FocusLabelScope, PresentationRegion, RenderedClientView, WindowPresentationPlan,
    focus_label_region,
};

/// Reports intersection of two absolute cell rectangles.
fn intersects(a: PresentationRegion, b: PresentationRegion) -> bool {
    !a.is_empty()
        && !b.is_empty()
        && a.row < b.row.saturating_add(b.rows)
        && b.row < a.row.saturating_add(a.rows)
        && a.column < b.column.saturating_add(b.columns)
        && b.column < a.column.saturating_add(a.columns)
}

impl RuntimeSessionService {
    /// Paints live identities without touching pane screens, geometry or hits.
    /// Required interactive surfaces suppress labels while deadlines continue.
    pub(super) fn overlay_zen_focus_labels(
        &self,
        window: &Window,
        plan: &WindowPresentationPlan,
        config: &TerminalClientLoopConfig,
        view: &mut RenderedClientView,
    ) {
        if !self.presentation.settings.terminal_zen_mode
            || self
                .presentation
                .settings
                .terminal_zen_focus_label_duration_ms
                == 0
            || self.presentation.primary_display_overlay.is_some()
        {
            return;
        }
        let Some(client) = self.presentation.projected_client_id.as_ref() else {
            return;
        };
        let Some(labels) = self.live_zen_focus_labels_for_client(client, current_unix_millis())
        else {
            return;
        };
        let mut painted = Vec::new();
        for (scope, label) in [
            (FocusLabelScope::Group, labels.group),
            (FocusLabelScope::Window, labels.window),
            (FocusLabelScope::Pane, labels.pane),
        ] {
            let Some(label) = label else {
                continue;
            };
            use super::focus_labels::RuntimeZenFocusLabelTarget;
            let group = self.session.active_group();
            let target_matches = match &label.target {
                RuntimeZenFocusLabelTarget::Group(id) => {
                    scope == FocusLabelScope::Group && group.is_some_and(|group| group.id == *id)
                }
                RuntimeZenFocusLabelTarget::Window(id) => {
                    scope == FocusLabelScope::Window
                        && window.id == *id
                        && group.is_some_and(|group| label.group_id.as_ref() == Some(&group.id))
                }
                RuntimeZenFocusLabelTarget::Pane(id) => {
                    scope == FocusLabelScope::Pane
                        && window.active_pane().id == *id
                        && label.window_id.as_ref() == Some(&window.id)
                        && group.is_some_and(|group| label.group_id.as_ref() == Some(&group.id))
                }
            };
            if !target_matches {
                continue;
            }
            let Some(mut region) = focus_label_region(plan, scope) else {
                continue;
            };
            let Some(pill) = render_focus_label(scope, window, config, usize::from(region.columns))
            else {
                continue;
            };
            region.columns =
                u16::try_from(mez_mux::render::char_count(&pill.text)).unwrap_or(u16::MAX);
            if painted.iter().any(|other| intersects(region, *other)) {
                continue;
            }
            if self.presentation.primary_prompt_input.is_some()
                && region.row == view.authoritative_size.rows.saturating_sub(1)
            {
                continue;
            }
            if let Some(selector) = self.presentation.pane_agent_status_selector.as_ref() {
                let layout = super::runtime_pane_agent_status_selector_layout(
                    selector,
                    view.authoritative_size,
                );
                if layout.visible_items.iter().any(|item| {
                    intersects(
                        region,
                        PresentationRegion {
                            row: item.row,
                            column: layout.column,
                            columns: layout.width,
                            rows: 1,
                        },
                    )
                }) {
                    continue;
                }
            }
            if let Some(prompt) = view.agent_prompt_region {
                let context = config
                    .frame_context
                    .panes
                    .get(window.active_pane().id.as_str());
                let rows = if context.is_some_and(|context| !context.agent_display_lines.is_empty())
                {
                    prompt.rows
                } else {
                    crate::host::terminal::agent_prompt_reserved_line_count(
                        prompt.columns,
                        prompt.rows,
                        context,
                    )
                };
                let protected = PresentationRegion {
                    row: u16::try_from(prompt.row.saturating_add(prompt.rows.saturating_sub(rows)))
                        .unwrap_or(u16::MAX),
                    column: u16::try_from(prompt.column).unwrap_or(u16::MAX),
                    rows: u16::try_from(rows).unwrap_or(u16::MAX),
                    columns: u16::try_from(prompt.columns).unwrap_or(u16::MAX),
                };
                if intersects(region, protected) {
                    continue;
                }
            }
            let Some((line, spans)) = view
                .lines
                .get_mut(usize::from(region.row))
                .zip(view.line_style_spans.get_mut(usize::from(region.row)))
            else {
                continue;
            };
            paint_pill(line, spans, usize::from(region.column), &pill);
            if region.contains(
                u16::try_from(view.cursor_row).unwrap_or(u16::MAX),
                u16::try_from(view.cursor_column).unwrap_or(u16::MAX),
            ) {
                view.cursor_visible = false;
            }
            painted.push(region);
        }
    }
}

/// Replaces only pill cells while blanking cut wide-glyph halves in place.
/// The existing slice helpers also remove styles from synthesized boundary
/// blanks; following content never shifts left when a glyph is cut.
fn paint_pill(
    line: &mut String,
    spans: &mut Vec<mez_terminal::TerminalStyleSpan>,
    column: usize,
    pill: &mez_terminal::TerminalStyledLine,
) {
    use mez_mux::render::{char_count, fit_width, line_slice, line_slice_style_spans};
    let end = column.saturating_add(char_count(&pill.text));
    let width = char_count(line).max(end);
    let mut output_spans = line_slice_style_spans(line, spans, 0, column);
    output_spans.extend(
        pill.style_spans
            .iter()
            .map(|span| mez_terminal::TerminalStyleSpan {
                start: column.saturating_add(span.start),
                ..*span
            }),
    );
    output_spans.extend(
        line_slice_style_spans(line, spans, end, width.saturating_sub(end))
            .into_iter()
            .map(|span| mez_terminal::TerminalStyleSpan {
                start: end.saturating_add(span.start),
                ..span
            }),
    );
    *line = format!(
        "{}{}{}",
        fit_width(&line_slice(line, 0, column), column),
        pill.text,
        line_slice(line, end, width)
    );
    *spans = output_spans;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::runtime::RuntimeServiceFixture;
    use mez_core::ids::ClientId;
    use mez_mux::layout::Size;
    use mez_mux::presentation::ClientViewRole;

    /// Creates a primary with long-lived records so wall-clock scheduling cannot
    /// make ordinary rendering assertions expire under a loaded test runner.
    fn fixture() -> (RuntimeSessionService, ClientId) {
        let mut service = RuntimeServiceFixture::new().build();
        let primary = service
            .attach_primary("primary", true, Size::new(40, 10).unwrap(), 1)
            .unwrap();
        service.presentation.settings.terminal_zen_mode = true;
        service
            .presentation
            .settings
            .terminal_zen_focus_label_duration_ms = 60_000;
        service
            .prepare_client_render(&primary, ClientViewRole::Primary)
            .unwrap();
        (service, primary)
    }

    /// Resolves current display metadata on every frame, matching production
    /// rather than retaining stale title text across a rename or selection.
    fn view(service: &mut RuntimeSessionService, client: &ClientId) -> RenderedClientView {
        service
            .prepare_client_render(client, ClientViewRole::Primary)
            .unwrap();
        let config = service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap();
        service
            .render_client_view_for_client_with_resolved_config(
                client,
                ClientViewRole::Primary,
                Size::new(40, 10).unwrap(),
                &config,
            )
            .unwrap()
            .unwrap()
    }

    /// A real committed window change must paint one bottom-left identity;
    /// expiry recomposes current underlying output without changing pane size.
    #[test]
    fn zen_focus_composition_window_appears_and_expiry_restores_current_content() {
        let (mut service, primary) = fixture();
        service
            .execute_terminal_command(&primary, "new-window visible-name")
            .unwrap();
        let pane = service.session.active_pane_for(&primary).unwrap().clone();
        let mut screen = mez_terminal::TerminalScreen::new(pane.size, 100).unwrap();
        screen.feed(b"\x1b[10;1Hunderlying");
        service.set_pane_screen(pane.id.to_string(), screen);
        let shown = view(&mut service, &primary);
        assert!(
            shown.lines[9].starts_with(" 1 visible-name "),
            "{:?}",
            shown.lines
        );
        assert!(shown.line_style_spans[9].iter().any(|span| span.start == 0));
        let mut changed = mez_terminal::TerminalScreen::new(pane.size, 100).unwrap();
        changed.feed(b"\x1b[10;1Hnew content");
        service.set_pane_screen(pane.id.to_string(), changed);
        assert!(service.expire_zen_focus_labels_for_client(&primary, u64::MAX));
        let expired = view(&mut service, &primary);
        assert!(expired.lines[9].starts_with("new content"));
        assert_eq!(
            service.session.active_pane_for(&primary).unwrap().size,
            pane.size
        );
        assert_eq!(shown.authoritative_size, expired.authoritative_size);
    }

    /// Cutting a wide glyph must blank its uncovered half without shifting the
    /// following text or retaining the cut glyph's rendition.
    #[test]
    fn zen_focus_composition_wide_boundary_preserves_cells_and_styles() {
        use mez_terminal::{GraphicRendition, TerminalStyleSpan, TerminalStyledLine};
        let bold = GraphicRendition {
            bold: true,
            ..GraphicRendition::default()
        };
        for (column, expected) in [(0, "x Z"), (1, " xZ")] {
            let mut line = "界Z".to_string();
            let mut spans = vec![TerminalStyleSpan {
                start: 0,
                length: 3,
                rendition: bold,
            }];
            let pill = TerminalStyledLine {
                text: "x".to_string(),
                copy_text: None,
                style_spans: Vec::new(),
            };
            paint_pill(&mut line, &mut spans, column, &pill);
            assert_eq!(line, expected);
            assert_eq!(mez_mux::render::char_count(&line), 3);
            assert_eq!(
                spans,
                vec![TerminalStyleSpan {
                    start: 2,
                    length: 1,
                    rendition: bold
                }]
            );
        }
    }

    /// Group transitions paint the top-left identity, while pane transitions
    /// use the current split anchor. Renames are resolved afresh with the
    /// original deadline and cannot leave longer prior label text behind.
    #[test]
    fn zen_focus_composition_group_pane_and_rename_use_current_identity() {
        let (mut service, primary) = fixture();
        let before = service.capture_zen_focus_snapshots();
        service
            .session
            .new_group(&primary, "group-name", true)
            .unwrap();
        service.reconcile_zen_focus_snapshots(before);
        let shown = view(&mut service, &primary);
        assert!(shown.lines[0].contains("group-name"));
        service.presentation.clear_all_zen_focus_labels();
        service.presentation.settings.pane_frame_template = "#{pane.title}".to_string();
        let before = service.capture_zen_focus_snapshots();
        let pane_id = service
            .session
            .split_active_pane(&primary, mez_mux::layout::SplitDirection::Horizontal)
            .unwrap();
        service.reconcile_zen_focus_snapshots(before);
        service
            .session
            .rename_pane(&primary, Some(pane_id.as_str()), "界🙂e\u{301}-long-title")
            .unwrap();
        let expected = service
            .live_zen_focus_labels_for_client(&primary, current_unix_millis())
            .unwrap();
        let shown = view(&mut service, &primary);
        assert!(
            shown
                .lines
                .iter()
                .any(|line| line.contains("界🙂e\u{301}-long-title")),
            "{:?}",
            shown.lines
        );
        service
            .session
            .rename_pane(&primary, Some(pane_id.as_str()), "x")
            .unwrap();
        let renamed = view(&mut service, &primary);
        assert!(renamed.lines.iter().any(|line| line.contains(" x ")));
        assert!(!renamed.lines.iter().any(|line| line.contains("long-title")));
        assert_eq!(
            service.live_zen_focus_labels_for_client(&primary, current_unix_millis()),
            Some(expected)
        );
    }

    /// Command input owns the bottom row but leaves a nonintersecting group
    /// identity visible. Closing the prompt does not renew any label deadline.
    #[test]
    fn zen_focus_composition_command_prompt_wins_only_at_intersection() {
        let (mut service, primary) = fixture();
        let before = service.capture_zen_focus_snapshots();
        service
            .session
            .new_group(&primary, "group-control", true)
            .unwrap();
        service.reconcile_zen_focus_snapshots(before);
        service
            .execute_terminal_command(&primary, "new-window window-control")
            .unwrap();
        service.presentation.primary_prompt_input = Some(super::super::RuntimePrimaryPromptInput {
            prompt: crate::ui::readline::ReadlinePrompt::new(
                crate::ui::readline::ReadlinePromptKind::Command,
            ),
            decoder: Default::default(),
        });
        let shown = view(&mut service, &primary);
        assert!(shown.lines[0].contains("group-control"));
        assert!(!shown.lines[9].contains("window-control"));
        assert!(shown.primary_prompt_active);
        service.presentation.primary_prompt_input = None;
        assert!(view(&mut service, &primary).lines[9].contains("window-control"));
    }

    /// Observers retain authoritative anchors before one ordinary viewport
    /// transform: a bottom label outside their viewport is not relocated.
    #[test]
    fn zen_focus_composition_observer_clips_once_without_relocation() {
        let (mut service, primary) = fixture();
        service
            .execute_terminal_command(&primary, "new-window observer-label")
            .unwrap();
        let observer = service
            .session
            .attach_observer_with_terminal("observer", None, 2)
            .unwrap();
        service
            .prepare_client_render(&observer, ClientViewRole::Observer)
            .unwrap();
        let config = service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap();
        let mut shown = service
            .render_client_view_for_client_with_resolved_config(
                &observer,
                ClientViewRole::Observer,
                Size::new(20, 5).unwrap(),
                &config,
            )
            .unwrap()
            .unwrap();
        assert!(shown.lines[9].contains("observer-label"));
        let (clipped, _) = mez_mux::presentation::compose_client_viewport(&shown);
        assert!(!clipped.iter().any(|line| line.contains("observer-label")));
        shown.viewport_row = 5;
        let (clipped, _) = mez_mux::presentation::compose_client_viewport(&shown);
        assert!(clipped[4].contains("observer-label"));
    }

    /// A label may hide an overlapping application cursor but never a cursor
    /// elsewhere on the same row, and expiry restores ordinary visibility.
    #[test]
    fn zen_focus_composition_cursor_coverage_is_cell_local() {
        for (column, visible) in [(1, false), (35, true)] {
            let (mut service, primary) = fixture();
            service
                .execute_terminal_command(&primary, "new-window cursor")
                .unwrap();
            let pane = service.session.active_pane_for(&primary).unwrap().clone();
            let mut screen = mez_terminal::TerminalScreen::new(pane.size, 100).unwrap();
            screen.feed(format!("\x1b[10;{column}H\x1b[?25h").as_bytes());
            service.set_pane_screen(pane.id.to_string(), screen);
            assert_eq!(view(&mut service, &primary).cursor_visible, visible);
            service.expire_zen_focus_labels_for_client(&primary, u64::MAX);
            assert!(view(&mut service, &primary).cursor_visible);
        }
    }

    /// Missed reconciliation must never relabel a different current window
    /// using an old record; tiny budgets must not paint padding-only chrome.
    #[test]
    fn zen_focus_composition_rejects_stale_targets_and_empty_pills() {
        let (mut service, primary) = fixture();
        let first = service
            .session
            .active_window_for(&primary)
            .unwrap()
            .id
            .clone();
        service
            .execute_terminal_command(&primary, "new-window stale-label")
            .unwrap();
        service
            .session
            .select_window(&primary, first.as_str())
            .unwrap();
        let shown = view(&mut service, &primary);
        assert!(!shown.lines[9].contains("stale-label"));
        assert!(!shown.lines[9].starts_with(" 0 "));
        let config = service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap();
        for width in 0..=2 {
            assert!(
                render_focus_label(
                    FocusLabelScope::Window,
                    service.session.active_window().unwrap(),
                    &config,
                    width
                )
                .is_none()
            );
        }
    }

    /// Labels preserve pane title expansion but never expand action lists;
    /// fallback identities remain available for empty or control-only titles.
    #[test]
    fn zen_focus_composition_pane_template_excludes_action_lists() {
        let (mut service, primary) = fixture();
        service.presentation.settings.pane_frame_template =
            "#{window.actions}#{window.buttons}#{pane.title}".to_string();
        service
            .session
            .rename_pane(&primary, None, "identity")
            .unwrap();
        let config = service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap();
        let pill = render_focus_label(
            FocusLabelScope::Pane,
            service.session.active_window().unwrap(),
            &config,
            40,
        )
        .unwrap();
        assert_eq!(pill.text, " identity ");
    }

    /// All three live scopes compete at fixed anchors on a one-row canvas.
    /// On a taller canvas the window can coexist with the group, but the pane
    /// cannot relocate away from the group's occupied top-left cells.
    #[test]
    fn zen_focus_composition_scope_collisions_never_relocate() {
        use super::super::focus_labels::{RuntimeZenFocusLabel, RuntimeZenFocusLabelTarget};
        use mez_mux::presentation::{WindowPresentationOptions, plan_window_presentation};

        for rows in [1, 10] {
            let (mut service, primary) = fixture();
            let mut baseline = view(&mut service, &primary);
            let mut window = service.session.active_window().unwrap().clone();
            window.size = Size::new(40, rows).unwrap();
            let plan =
                plan_window_presentation(&window, WindowPresentationOptions::default()).unwrap();
            let original_plan = plan.clone();
            let group = service.session.active_group().unwrap().id.clone();
            let label = |target| RuntimeZenFocusLabel {
                target,
                group_id: Some(group.clone()),
                window_id: Some(window.id.clone()),
                expires_at_unix_ms: u64::MAX,
            };
            let state = &mut service
                .presentation
                .client_states
                .entry(primary.clone())
                .or_default()
                .zen_focus_labels;
            state.group = Some(label(RuntimeZenFocusLabelTarget::Group(group.clone())));
            state.window = Some(label(RuntimeZenFocusLabelTarget::Window(window.id.clone())));
            state.pane = Some(label(RuntimeZenFocusLabelTarget::Pane(
                window.active_pane().id.clone(),
            )));
            let mut config = service
                .terminal_client_loop_config(TerminalClientLoopConfig::default())
                .unwrap();
            config
                .frame_context
                .groups
                .iter_mut()
                .find(|item| item.active)
                .unwrap()
                .title = "GROUP".to_string();
            config
                .frame_context
                .windows
                .iter_mut()
                .find(|item| item.id == window.id.as_str())
                .unwrap()
                .title = "WINDOW".to_string();
            config.pane_frame_template = "PANE".to_string();
            baseline.authoritative_size = window.size;
            baseline.client_size = window.size;
            baseline.lines = vec![".".repeat(40); usize::from(rows)];
            baseline.line_style_spans = vec![Vec::new(); usize::from(rows)];
            baseline.agent_prompt_region = None;
            let mut shown = baseline.clone();
            service.overlay_zen_focus_labels(&window, &plan, &config, &mut shown);
            assert!(shown.lines[0].contains("GROUP"));
            assert_eq!(
                shown.lines.iter().any(|line| line.contains("WINDOW")),
                rows > 1
            );
            assert!(!shown.lines.iter().any(|line| line.contains("PANE")));
            service
                .presentation
                .client_states
                .get_mut(&primary)
                .unwrap()
                .zen_focus_labels
                .group = None;
            let mut shown = baseline;
            service.overlay_zen_focus_labels(&window, &plan, &config, &mut shown);
            assert!(shown.lines[usize::from(rows - 1)].contains("WINDOW"));
            assert_eq!(
                shown.lines.iter().any(|line| line.contains("PANE")),
                rows > 1
            );
            assert_eq!(plan, original_plan);
            assert_eq!(shown.authoritative_size, window.size);
        }
    }

    /// A resize baseline captured with a live label must blank that label
    /// during the gesture. Expiry while hidden must survive release without
    /// replaying the baseline's stale identity into the current content.
    #[test]
    fn zen_focus_composition_resize_drag_expires_without_replay() {
        use crate::runtime::{
            AttachedTerminalClientStepPlan, MouseAction, TerminalClientLoopAction,
        };
        let (mut service, primary) = fixture();
        service
            .execute_terminal_command(&primary, "new-window drag-focus")
            .unwrap();
        service
            .session
            .split_active_pane(&primary, mez_mux::layout::SplitDirection::Horizontal)
            .unwrap();
        assert!(view(&mut service, &primary).lines[9].contains("drag-focus"));
        let config = service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap();
        let border = config.mouse_border_cells.first().unwrap();
        let step = |action| AttachedTerminalClientStepPlan {
            actions: vec![TerminalClientLoopAction::HandleMouse(action)],
            output_lines: Vec::new(),
            output_line_style_spans: Vec::new(),
            input_hangup: false,
            output_hangup: false,
            error_roles: Vec::new(),
        };
        service
            .apply_attached_terminal_step_transition(
                &primary,
                &step(MouseAction::ResizePane {
                    column: border.column,
                    row: border.row,
                }),
            )
            .unwrap();
        assert!(
            service
                .presentation
                .mouse_resize_drag_baseline_view
                .is_some()
        );
        let hidden = view(&mut service, &primary);
        assert!(!hidden.lines.iter().any(|line| line.contains("drag-focus")));
        assert!(service.expire_zen_focus_labels_for_client(&primary, u64::MAX));
        service
            .apply_attached_terminal_step_transition(&primary, &step(MouseAction::FinishResizePane))
            .unwrap();
        assert!(
            service
                .presentation
                .take_pending_divider_layout_commit()
                .is_some()
        );
        let restored = view(&mut service, &primary);
        assert!(
            !restored
                .lines
                .iter()
                .any(|line| line.contains("drag-focus"))
        );
        assert_eq!(restored.authoritative_size, hidden.authoritative_size);
    }

    /// Selector rectangles suppress only intersecting labels, without exposing
    /// a clipped fragment beside the control. Modal ownership suppresses all
    /// labels, and expiry while hidden cannot replay after modal dismissal.
    #[test]
    fn zen_focus_composition_selector_and_modal_precedence() {
        let (mut service, primary) = fixture();
        service
            .execute_terminal_command(&primary, "new-window focus")
            .unwrap();
        let pane = service.session.active_pane_for(&primary).unwrap().clone();
        service.presentation.pane_agent_status_selector =
            Some(super::super::RuntimePaneAgentStatusSelector {
                navigation: mez_mux::overlay::AnchoredSelector {
                    pane_id: pane.id.to_string(),
                    pane_index: pane.index,
                    field: crate::host::terminal::PaneAgentStatusField::Settings,
                    items: vec!["control".to_string()],
                    active_index: 0,
                    scroll_offset: 0,
                    anchor_column: 0,
                    anchor_row: 8,
                    anchor_width: 8,
                },
                source_identity: None,
                settings_entries: Vec::new(),
            });
        let hidden = view(&mut service, &primary);
        assert!(hidden.lines[9].contains("control"));
        assert!(!hidden.lines[9].contains("focus"));
        service
            .presentation
            .pane_agent_status_selector
            .as_mut()
            .unwrap()
            .anchor_column = 30;
        let beside = view(&mut service, &primary);
        assert!(beside.lines[9].contains("focus"));
        assert!(beside.lines[9].contains("control"));
        service.presentation.pane_agent_status_selector = None;
        service
            .show_primary_display_overlay(vec!["required modal".to_string()])
            .unwrap();
        let modal = view(&mut service, &primary);
        assert!(
            modal
                .lines
                .iter()
                .any(|line| line.contains("required modal"))
        );
        assert!(!modal.lines.iter().any(|line| line.contains("focus")));
        assert!(service.expire_zen_focus_labels_for_client(&primary, u64::MAX));
        service.presentation.primary_display_overlay = None;
        assert!(
            !view(&mut service, &primary)
                .lines
                .iter()
                .any(|line| line.contains("focus"))
        );
    }

    /// Rectangle boundary contact alone is not intersection.
    #[test]
    fn zen_focus_composition_rectangle_boundaries() {
        let region = PresentationRegion {
            row: 0,
            column: 0,
            columns: 5,
            rows: 1,
        };
        assert!(intersects(region, region));
        assert!(!intersects(
            region,
            PresentationRegion {
                column: 5,
                ..region
            }
        ));
        assert!(!intersects(region, PresentationRegion { row: 1, ..region }));
    }
}
