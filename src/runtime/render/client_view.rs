//! Client view and terminal frame-context rendering helpers.
//!
//! This module owns rendering a session into a client-facing view, applying
//! primary overlays, copy-mode overlays, mouse hit regions, animation metadata,
//! and terminal frame context. Keeping this large frame-assembly path separate
//! from input mutation helpers makes the runtime render facade easier to scan.

use super::*;

/// Runs the apply copy mode selection spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn apply_copy_mode_selection_spans(
    copy_mode: &CopyMode,
    lines: &mut [TerminalStyledLine],
    ui_theme: &crate::terminal::UiTheme,
) {
    let Some((start, end)) = copy_mode.selection() else {
        return;
    };
    let (start, end) = ordered_copy_positions(start, end);
    let scroll_top = copy_mode.scroll_top();
    for (row_offset, line) in lines.iter_mut().enumerate() {
        let line_index = scroll_top.saturating_add(row_offset);
        if line_index < start.line || line_index > end.line {
            continue;
        }
        let selection_start = if line_index == start.line {
            start.column
        } else {
            0
        };
        let selection_end = if line_index == end.line {
            end.column
        } else {
            terminal_text_width(&line.text)
        };
        if selection_end <= selection_start {
            continue;
        }
        line.style_spans.push(TerminalStyleSpan {
            start: selection_start,
            length: selection_end.saturating_sub(selection_start),
            rendition: copy_selection_rendition(ui_theme),
        });
    }
}

/// Positions the attached terminal cursor at the active copy-mode cursor.
pub(super) fn apply_copy_mode_terminal_cursor(
    copy_mode: &CopyMode,
    view: &mut RenderedClientView,
    row: usize,
    column: usize,
    size: Size,
) {
    let cursor = copy_mode.cursor();
    let Some(row_offset) = cursor.line.checked_sub(copy_mode.scroll_top()) else {
        return;
    };
    if row_offset >= usize::from(size.rows) {
        return;
    }
    view.cursor_row = row.saturating_add(row_offset);
    view.cursor_column = column.saturating_add(
        cursor
            .column
            .min(usize::from(size.columns).saturating_sub(1)),
    );
    view.cursor_visible = view.cursor_row < usize::from(view.authoritative_size.rows)
        && view.cursor_column < usize::from(view.authoritative_size.columns);
}

/// Runs the ordered copy positions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn ordered_copy_positions(
    first: CopyPosition,
    second: CopyPosition,
) -> (CopyPosition, CopyPosition) {
    if (first.line, first.column) <= (second.line, second.column) {
        (first, second)
    } else {
        (second, first)
    }
}

/// Runs the copy selection rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn copy_selection_rendition(
    ui_theme: &crate::terminal::UiTheme,
) -> crate::terminal::GraphicRendition {
    let mut rendition = ui_theme.colors.copy_selection.rendition();
    rendition.inverse = true;
    rendition
}

impl RuntimeSessionService {
    pub fn render_client_view(
        &self,
        role: ClientViewRole,
        client_size: Size,
        config: &TerminalClientLoopConfig,
    ) -> Result<Option<RenderedClientView>> {
        let config = self.terminal_client_loop_config(config.clone())?;
        self.render_client_view_with_resolved_config(role, client_size, &config)
    }
    /// Renders a client view using a terminal configuration that has already
    /// been resolved from runtime state.
    ///
    /// Hot paths that need both the loop configuration and a frame use this
    /// helper to avoid rebuilding frame context and mouse hit regions twice
    /// for the same control request.
    pub fn render_client_view_with_resolved_config(
        &self,
        role: ClientViewRole,
        client_size: Size,
        config: &TerminalClientLoopConfig,
    ) -> Result<Option<RenderedClientView>> {
        let Some(window) = self.session.active_window() else {
            return if self.session.windows().is_empty() {
                Ok(None)
            } else {
                Err(MezError::invalid_state("session has no active window"))
            };
        };
        let mut view =
            render_attached_client_view(role, window, &self.pane_screens, config, client_size)?;
        if role == ClientViewRole::Primary
            && let Some(view) = view.as_mut()
        {
            self.overlay_copy_modes_on_view(window, view)?;
        }
        if role == ClientViewRole::Primary
            && let Some(view) = view.as_mut()
            && let Some(status) = self.pending_observer_status_line()
            && let Some(last_line) = view.lines.last_mut()
        {
            *last_line =
                runtime_fit_status_line(&status, usize::from(view.authoritative_size.columns));
            if let Some(last_spans) = view.line_style_spans.last_mut() {
                last_spans.clear();
            }
        }
        if role == ClientViewRole::Primary
            && let Some(view) = view.as_mut()
            && let Some(selector) = self.pane_agent_status_selector.as_ref()
        {
            self.overlay_pane_agent_status_selector(view, selector);
        }
        if role == ClientViewRole::Primary
            && let Some(view) = view.as_mut()
            && let Some(prompt_input) = self.primary_prompt_input.as_ref()
        {
            self.overlay_primary_prompt_input(view, prompt_input);
        }
        if role == ClientViewRole::Primary
            && let Some(view) = view.as_mut()
            && let Some(overlay) = self.primary_display_overlay.as_ref()
        {
            self.overlay_primary_display_overlay(view, overlay);
        }
        if role == ClientViewRole::Primary
            && let Some(view) = view.as_mut()
            && let Some(message) = self.primary_error_status_overlay.as_ref()
        {
            self.overlay_primary_error_status(view, message);
        }
        Ok(view)
    }

    /// Runs the overlay primary prompt input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn overlay_primary_prompt_input(
        &self,
        view: &mut RenderedClientView,
        prompt_input: &RuntimePrimaryPromptInput,
    ) {
        let presentation = compose_prompt_overlay_presentation_with_styles(
            &view.lines,
            &view.line_style_spans,
            &prompt_input.prompt,
            view.authoritative_size,
            &self.ui_theme,
        );
        view.lines = presentation.lines;
        view.line_style_spans = presentation.line_style_spans;
        view.cursor_visible = presentation.cursor_visible;
        view.cursor_row = presentation.cursor_row;
        view.cursor_column = presentation.cursor_column;
        view.primary_prompt_active = true;
    }

    /// Clips existing style spans so an overlay fully owns one column range.
    pub(super) fn clip_line_style_spans_for_overlay(
        spans: &mut Vec<TerminalStyleSpan>,
        start: usize,
        length: usize,
    ) {
        if length == 0 {
            return;
        }
        let end = start.saturating_add(length);
        let mut clipped = Vec::with_capacity(spans.len().saturating_add(1));
        for span in spans.drain(..) {
            let span_end = span.start.saturating_add(span.length);
            if span_end <= start || span.start >= end {
                clipped.push(span);
                continue;
            }
            if span.start < start {
                clipped.push(TerminalStyleSpan {
                    start: span.start,
                    length: start.saturating_sub(span.start),
                    rendition: span.rendition,
                });
            }
            if span_end > end {
                clipped.push(TerminalStyleSpan {
                    start: end,
                    length: span_end.saturating_sub(end),
                    rendition: span.rendition,
                });
            }
        }
        *spans = clipped;
    }

    /// Draws a pane agent model/reasoning selector over the rendered view.
    fn overlay_pane_agent_status_selector(
        &self,
        view: &mut RenderedClientView,
        selector: &RuntimePaneAgentStatusSelector,
    ) {
        let layout = runtime_pane_agent_status_selector_layout(selector, view.authoritative_size);
        let column = usize::from(layout.column);
        let width = usize::from(layout.width);
        for item in layout.visible_items {
            let Some(value) = selector.items.get(item.item_index) else {
                continue;
            };
            let row = usize::from(item.row);
            if row >= view.lines.len() {
                continue;
            }
            let active = item.item_index == selector.active_index;
            let marker = if active { "›" } else { " " };
            let text = runtime_selector_line(marker, value, width);
            runtime_overlay_text_at(&mut view.lines[row], column, width, &text);
            if let Some(spans) = view.line_style_spans.get_mut(row) {
                Self::clip_line_style_spans_for_overlay(spans, column, width);
                spans.push(TerminalStyleSpan {
                    start: column,
                    length: width,
                    rendition: runtime_pane_agent_selector_rendition(
                        selector.field,
                        active,
                        &self.ui_theme,
                    ),
                });
            }
        }
    }

    /// Runs the overlay primary display overlay operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn overlay_primary_display_overlay(
        &self,
        view: &mut RenderedClientView,
        overlay: &RuntimeDisplayOverlay,
    ) {
        let render_lines = runtime_display_overlay_render_lines(overlay);
        view.lines = compose_modal_display_overlay_lines(
            &render_lines,
            view.authoritative_size,
            overlay.scroll_offset,
        );
        view.line_style_spans = vec![Vec::new(); view.lines.len()];
        if let Some(footer) = view.lines.last_mut() {
            *footer = runtime_fit_status_line(
                &runtime_display_overlay_footer(overlay),
                usize::from(view.authoritative_size.columns),
            );
        }
        let page_rows = modal_display_overlay_page_rows(view.authoritative_size);
        let max_columns = usize::from(view.authoritative_size.columns);
        for (selection_index, selection) in overlay.selections.iter().enumerate() {
            if selection.line_index < overlay.scroll_offset {
                continue;
            }
            let offset = selection.line_index.saturating_sub(overlay.scroll_offset);
            if offset >= page_rows {
                continue;
            }
            let row = offset.saturating_add(1);
            let active = overlay.active_selection_index == Some(selection_index);
            if let Some(spans) = view.line_style_spans.get_mut(row) {
                let start = runtime_display_overlay_rendered_selection_start(overlay, selection);
                if start < max_columns && selection.width > 0 {
                    spans.push(TerminalStyleSpan {
                        start,
                        length: selection.width.min(max_columns.saturating_sub(start)),
                        rendition: runtime_display_overlay_selection_rendition(
                            &self.ui_theme,
                            selection.kind,
                            active,
                        ),
                    });
                }
                if active {
                    spans.push(TerminalStyleSpan {
                        start: 0,
                        length: 1,
                        rendition: runtime_display_overlay_selection_rendition(
                            &self.ui_theme,
                            selection.kind,
                            true,
                        ),
                    });
                }
            }
        }
        for line_index in overlay.scroll_offset
            ..overlay
                .scroll_offset
                .saturating_add(page_rows)
                .min(overlay.lines.len())
        {
            let offset = line_index.saturating_sub(overlay.scroll_offset);
            let row = offset.saturating_add(1);
            let Some(spans) = view.line_style_spans.get_mut(row) else {
                continue;
            };
            *spans = runtime_display_overlay_rendered_line_style_spans(
                overlay,
                line_index,
                max_columns,
                &self.ui_theme,
            );
        }
        view.cursor_visible = false;
        view.cursor_row = 0;
        view.cursor_column = 0;
        view.primary_prompt_active = false;
    }

    /// Overlays a transient error notice on the window status bar row.
    fn overlay_primary_error_status(&self, view: &mut RenderedClientView, message: &str) {
        let Some(row) = self.primary_error_status_overlay_row(view) else {
            return;
        };
        let width = usize::from(view.authoritative_size.columns);
        if width == 0 {
            return;
        }
        let text = runtime_fit_status_line(message, width);
        if let Some(line) = view.lines.get_mut(row) {
            *line = text;
        }
        if let Some(spans) = view.line_style_spans.get_mut(row) {
            let rendition = if message.starts_with("mez error:") || message.starts_with("error:") {
                self.ui_theme.colors.agent_status_failed.rendition()
            } else {
                self.ui_theme.colors.agent_status_running.rendition()
            };
            spans.clear();
            spans.push(TerminalStyleSpan {
                start: 0,
                length: width,
                rendition,
            });
        }
        if view.cursor_row == row {
            view.cursor_visible = false;
        }
    }

    /// Returns the client row used for transient primary error notices.
    fn primary_error_status_overlay_row(&self, view: &RenderedClientView) -> Option<usize> {
        let rows = usize::from(view.authoritative_size.rows);
        if rows == 0 {
            return None;
        }
        if !self.window_frames_enabled {
            return Some(rows.saturating_sub(1));
        }
        let group_top_offset = usize::from(self.session.window_groups().len() > 1);
        Some(match self.window_frame_position {
            TerminalFramePosition::Top => group_top_offset.min(rows.saturating_sub(1)),
            TerminalFramePosition::Bottom => rows.saturating_sub(1),
        })
    }

    /// Runs the overlay copy modes on view operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn overlay_copy_modes_on_view(
        &self,
        window: &crate::layout::Window,
        view: &mut RenderedClientView,
    ) -> Result<()> {
        let mut deferred_cleanup = self.deferred_word_copy_cleanup.borrow_mut();
        let mut clear_deferred_cleanup = false;
        if let Some((pane_id, copy_mode, cleanup_at_unix_ms)) = deferred_cleanup.as_ref()
            && let Some(pane_index) = window
                .panes()
                .iter()
                .position(|p| p.id.as_str() == pane_id.as_str())
            && let Some((row, column, size)) = self.copy_mode_overlay_region(window, pane_index)
        {
            let mut lines = copy_mode.visible_styled_lines().to_vec();
            apply_copy_mode_selection_spans(copy_mode, &mut lines, &self.ui_theme);
            overlay_styled_lines(
                view,
                row,
                column,
                usize::from(size.columns),
                usize::from(size.rows),
                &lines,
            );
            clear_deferred_cleanup = current_unix_millis() >= *cleanup_at_unix_ms;
        }
        if clear_deferred_cleanup {
            deferred_cleanup.take();
        }
        drop(deferred_cleanup);
        for pane in window.panes() {
            let Some(copy_mode) = self.active_copy_modes.get(pane.id.as_str()) else {
                continue;
            };
            let Some((row, column, size)) = self.copy_mode_overlay_region(window, pane.index)
            else {
                continue;
            };
            let mut lines = copy_mode.visible_styled_lines().to_vec();
            apply_copy_mode_selection_spans(copy_mode, &mut lines, &self.ui_theme);
            overlay_styled_lines(
                view,
                row,
                column,
                usize::from(size.columns),
                usize::from(size.rows),
                &lines,
            );
            if pane.index == window.active_pane_index() {
                apply_copy_mode_terminal_cursor(copy_mode, view, row, column, size);
            }
        }
        Ok(())
    }

    /// Runs the copy mode overlay region operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_content_mouse_region(
        &self,
        window: &crate::layout::Window,
        pane_index: usize,
    ) -> Option<(usize, usize, Size)> {
        let window_frame_visible = self.window_frames_enabled;
        let group_top_offset = usize::from(self.session.window_groups().len() > 1);
        let display_size = Size::new(
            window.size.columns,
            window
                .size
                .rows
                .saturating_sub(u16::try_from(group_top_offset).ok()?)
                .max(1),
        )
        .ok()?;
        let body_size = rendered_window_body_size(display_size, window_frame_visible).ok()?;
        let geometries = if let Some(zoomed) = window.zoomed_pane_id() {
            let pane = window.panes().iter().find(|pane| &pane.id == zoomed)?;
            vec![crate::layout::PaneGeometry {
                index: pane.index,
                column: 0,
                row: 0,
                columns: body_size.columns,
                rows: body_size.rows,
            }]
        } else {
            window.pane_geometries_for_size(body_size)
        };
        let geometry = geometries
            .iter()
            .find(|geometry| geometry.index == pane_index)?;
        let render_region = pane_render_region_size_for_geometry(geometry, &geometries).ok()?;
        let full_content_size = pane_content_size_for_geometry(
            geometry,
            &geometries,
            self.pane_frames_enabled,
            self.pane_frame_position,
        )
        .ok()?;
        let window_top_offset = usize::from(
            window_frame_visible && self.window_frame_position == TerminalFramePosition::Top,
        );
        let pane_top_offset = usize::from(
            self.pane_frames_enabled
                && self.pane_frame_position == TerminalFramePosition::Top
                && full_content_size.rows < render_region.rows,
        );
        Some((
            group_top_offset
                .saturating_add(window_top_offset)
                .saturating_add(usize::from(geometry.row))
                .saturating_add(pane_top_offset),
            usize::from(geometry.column),
            full_content_size,
        ))
    }

    /// Runs the copy mode overlay region operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn copy_mode_overlay_region(
        &self,
        window: &crate::layout::Window,
        pane_index: usize,
    ) -> Option<(usize, usize, Size)> {
        let (row, column, full_content_size) =
            self.pane_content_mouse_region(window, pane_index)?;
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.index == pane_index)?;
        let reserved_rows = self.agent_prompt_reserved_rows_for_pane(
            pane.id.as_str(),
            usize::from(full_content_size.columns),
            usize::from(full_content_size.rows),
        );
        let reserved_rows = u16::try_from(reserved_rows)
            .unwrap_or(u16::MAX)
            .min(full_content_size.rows.saturating_sub(1));
        let content_size = Size {
            columns: full_content_size.columns,
            rows: full_content_size.rows.saturating_sub(reserved_rows).max(1),
        };
        Some((row, column, content_size))
    }

    /// Runs the agent prompt reserved rows for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn agent_prompt_reserved_rows_for_pane(
        &self,
        pane_id: &str,
        width: usize,
        body_rows: usize,
    ) -> usize {
        if width == 0 || body_rows == 0 {
            return 0;
        }
        let Some(agent_session) = self.agent_shell_store.get(pane_id) else {
            return 0;
        };
        if !matches!(agent_session.visibility, AgentShellVisibility::Visible) {
            return 0;
        }
        let pane_context = TerminalPaneFrameContext {
            agent_prompt: Some(
                self.agent_prompt_inputs
                    .get(pane_id)
                    .map(|input| input.prompt.clone())
                    .unwrap_or_else(|| ReadlinePrompt::new(ReadlinePromptKind::Agent)),
            ),
            agent_display_lines: self.runtime_agent_prompt_display_lines_for_pane(pane_id),
            ..TerminalPaneFrameContext::default()
        };
        agent_prompt_reserved_line_count(width, body_rows, Some(&pane_context))
    }

    /// Returns pane-local agent display lines plus the live turn timer footer.
    fn runtime_agent_prompt_display_lines_for_pane(&self, pane_id: &str) -> Vec<String> {
        let mut lines = self
            .agent_prompt_inputs
            .get(pane_id)
            .map(|input| input.display_lines.clone())
            .unwrap_or_default();
        if let Some(footer) = self.runtime_agent_working_footer_line(pane_id) {
            lines.push(footer);
        }
        lines
    }

    /// Builds the live working footer shown at the tail of an active agent pane.
    fn runtime_agent_working_footer_line(&self, pane_id: &str) -> Option<String> {
        if let Some(started_at) = self.agent_compacting_panes.get(pane_id) {
            let elapsed = current_unix_seconds().saturating_sub(*started_at);
            return Some(format!(
                "compacting ({} • esc to interrupt)",
                runtime_agent_turn_duration_display(elapsed)
            ));
        }
        if let Some(started_at) = self.agent_remembering_panes.get(pane_id) {
            let elapsed = current_unix_seconds().saturating_sub(*started_at);
            return Some(format!(
                "memorizing ({} • esc to interrupt)",
                runtime_agent_turn_duration_display(elapsed)
            ));
        }
        let running_turn_id = self
            .agent_shell_store
            .get(pane_id)?
            .running_turn_id
            .as_deref()?;
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == running_turn_id)?;
        let elapsed = current_unix_seconds().saturating_sub(turn.started_at_unix_seconds);
        Some(format!(
            "{} ({} • esc to interrupt)",
            self.runtime_agent_working_footer_state_label(turn),
            runtime_agent_turn_duration_display(elapsed)
        ))
    }

    /// Returns the human-readable active state label for the live agent footer.
    fn runtime_agent_working_footer_state_label(&self, turn: &AgentTurnRecord) -> &'static str {
        match self.runtime_agent_frame_status(turn) {
            "queued" => "queued",
            "remembering" => "remembering",
            "thinking" => "thinking",
            "executing" => "executing",
            "waiting" => "waiting",
            "compacting" => "compacting",
            "routing" => "routing",
            "running" => "running",
            "waiting_approval" => "waiting approval",
            "completed" => "completed",
            "failed" => "failed",
            "interrupted" => "interrupted",
            "stopped" => "stopped",
            _ => match turn.state {
                AgentTurnState::Queued => "queued",
                AgentTurnState::Running => {
                    if self.runtime_agent_turn_is_auto_sizing_routing(turn) {
                        "routing"
                    } else {
                        "running"
                    }
                }
                AgentTurnState::Blocked => "waiting approval",
                AgentTurnState::Completed => "completed",
                AgentTurnState::Failed => "failed",
                AgentTurnState::Interrupted => "interrupted",
            },
        }
    }

    /// Runs the terminal client loop config operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn terminal_client_loop_config(
        &self,
        mut config: TerminalClientLoopConfig,
    ) -> Result<TerminalClientLoopConfig> {
        config.bindings = self.key_bindings.clone();
        config.command_bindings = self
            .command_bindings
            .iter()
            .map(|(chord, binding)| (*chord, binding.command.clone()))
            .collect();
        config.prefix_key_pending = self.primary_prefix_key_pending;
        config.window_frames_enabled = self.window_frames_enabled;
        config.window_frame_template = self.window_frame_template.clone();
        config.window_frame_position = self.window_frame_position;
        config.window_frame_style = self.window_frame_style;
        config.window_frame_visible_fields = self.window_frame_visible_fields.clone();
        config.pane_frames_enabled = self.pane_frames_enabled;
        config.pane_frame_template = self.pane_frame_template.clone();
        config.pane_frame_position = self.pane_frame_position;
        config.pane_frame_style = self.pane_frame_style;
        config.pane_frame_visible_fields = self.pane_frame_visible_fields.clone();
        config.cursor_style = self.terminal_cursor_style;
        config.cursor_blink = self.terminal_cursor_blink;
        config.cursor_blink_interval_ms = self.terminal_cursor_blink_interval_ms;
        config.resize_debounce_ms = self.terminal_resize_debounce_ms;
        config.render_rate_limit_fps = self.terminal_render_rate_limit_fps;
        config.ui_theme = self.ui_theme.clone();
        config.primary_display_overlay_active = self.primary_display_overlay.is_some();
        let frame_context = self.terminal_frame_context();
        config.mouse_border_cells = self.active_window_mouse_border_cells();
        config.mouse_window_frame_cells = self.active_window_mouse_frame_cells(&frame_context);
        config.mouse_window_action_frame_cells =
            self.active_window_mouse_action_frame_cells(&frame_context);
        config.mouse_window_group_frame_cells =
            self.active_window_group_mouse_frame_cells(&frame_context);
        config.mouse_pane_agent_status_cells =
            self.active_window_mouse_pane_agent_status_cells(&frame_context);
        config.mouse_pane_agent_selector_cells = self.mouse_pane_agent_selector_cells();
        config.mouse_pane_regions = self.active_window_mouse_pane_regions();
        config.frame_context = frame_context;
        config.mouse_policy.pane_resize_active = self.mouse_resize_drag_state.is_some();
        let active_pane_id = self.active_pane_id().ok();
        let active_mouse_selection_state = active_pane_id.as_deref().and_then(|pane_id| {
            self.mouse_selection_drag_state
                .as_ref()
                .filter(|state| state.pane_id.as_str() == pane_id)
        });
        config.mouse_selection_active = active_mouse_selection_state.is_some();
        config.mouse_selection_autoscroll_position =
            active_mouse_selection_state.and_then(|state| state.autoscroll_position);
        if let Some(pane_id) = active_pane_id {
            config.mouse_policy.copy_mode_active =
                self.active_copy_modes.contains_key(pane_id.as_str())
                    || active_mouse_selection_state.is_some();
            config.mouse_policy.pane_application_mouse_mode = self
                .pane_screens
                .get(pane_id.as_str())
                .is_some_and(TerminalScreen::application_mouse_enabled);
            config.mouse_policy.pane_sgr_mouse_mode = self
                .pane_screens
                .get(pane_id.as_str())
                .is_some_and(TerminalScreen::application_sgr_mouse_enabled);
            config.mouse_policy.pane_application_cursor_mode = self
                .pane_screens
                .get(pane_id.as_str())
                .is_some_and(TerminalScreen::application_cursor_enabled);
            config.mouse_policy.pane_application_keypad_mode = self
                .pane_screens
                .get(pane_id.as_str())
                .is_some_and(TerminalScreen::application_keypad_enabled);
            config.pane_bracketed_paste_mode = self
                .pane_screens
                .get(pane_id.as_str())
                .is_some_and(TerminalScreen::bracketed_paste_enabled);
        }
        Ok(config)
    }

    /// Runs the active window mouse pane regions operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn active_window_mouse_pane_regions(&self) -> Vec<MousePaneRegion> {
        let Some(window) = self.session.active_window() else {
            return Vec::new();
        };
        let active_pane_id = window.active_pane().id.to_string();
        window
            .panes()
            .iter()
            .filter_map(|pane| {
                let (row, column, size) = self.pane_content_mouse_region(window, pane.index)?;
                let row = u16::try_from(row).ok()?;
                let column = u16::try_from(column).ok()?;
                let pane_id = pane.id.to_string();
                Some(MousePaneRegion {
                    pane_id: pane_id.clone(),
                    column,
                    row,
                    columns: size.columns,
                    rows: size.rows,
                    application_sgr_mouse_mode: self
                        .pane_screens
                        .get(pane_id.as_str())
                        .is_some_and(TerminalScreen::application_sgr_mouse_enabled),
                    application_mouse_mode: self
                        .pane_screens
                        .get(pane_id.as_str())
                        .is_some_and(TerminalScreen::application_mouse_enabled),
                    copy_mode_active: self.active_copy_modes.contains_key(pane_id.as_str()),
                    active: pane_id == active_pane_id,
                })
            })
            .collect()
    }

    /// Runs the active window mouse border cells operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn active_window_mouse_border_cells(&self) -> Vec<MouseBorderCell> {
        let Some(window) = self.session.active_window() else {
            return Vec::new();
        };
        let window_frame_visible = self.window_frames_enabled;
        let group_top_offset = u16::from(self.session.window_groups().len() > 1);
        let mut display_window = window.clone();
        if group_top_offset > 0
            && let Ok(size) = Size::new(
                window.size.columns,
                window.size.rows.saturating_sub(group_top_offset).max(1),
            )
        {
            display_window.size = size;
        }
        let geometries = rendered_pane_geometries(&display_window, window_frame_visible)
            .unwrap_or_else(|_| display_window.pane_geometries());
        let row_offset = group_top_offset.saturating_add(u16::from(
            window_frame_visible && self.window_frame_position == TerminalFramePosition::Top,
        ));
        pane_border_cells_for_geometries(&geometries, row_offset)
    }

    /// Runs the active window mouse frame cells operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn active_window_mouse_frame_cells(
        &self,
        frame_context: &TerminalFrameContext,
    ) -> Vec<MouseWindowFrameCell> {
        let Some(window) = self.session.active_window() else {
            return Vec::new();
        };
        if !self.window_frames_enabled {
            return Vec::new();
        }
        if self.window_frame_template != crate::terminal::DEFAULT_WINDOW_FRAME_TEMPLATE {
            return Vec::new();
        }
        let group_top_offset = u16::from(self.session.window_groups().len() > 1);
        let row = match self.window_frame_position {
            TerminalFramePosition::Top => group_top_offset,
            TerminalFramePosition::Bottom => window.size.rows.saturating_sub(1),
        };
        window_frame_pillbox_cells(frame_context, row, window.size.columns)
    }

    /// Returns mouse hit cells for built-in window status-bar action pills.
    fn active_window_mouse_action_frame_cells(
        &self,
        frame_context: &TerminalFrameContext,
    ) -> Vec<MouseWindowActionFrameCell> {
        let Some(window) = self.session.active_window() else {
            return Vec::new();
        };
        if !self.window_frames_enabled {
            return Vec::new();
        }
        if self.window_frame_right_status_template.trim().is_empty() {
            return Vec::new();
        }
        let group_top_offset = u16::from(self.session.window_groups().len() > 1);
        let row = match self.window_frame_position {
            TerminalFramePosition::Top => group_top_offset,
            TerminalFramePosition::Bottom => window.size.rows.saturating_sub(1),
        };
        window_frame_action_pillbox_cells(frame_context, row, window.size.columns)
    }

    /// Returns mouse hit cells for the conditional top window-group bar.
    fn active_window_group_mouse_frame_cells(
        &self,
        frame_context: &TerminalFrameContext,
    ) -> Vec<crate::terminal::MouseWindowGroupFrameCell> {
        let Some(window) = self.session.active_window() else {
            return Vec::new();
        };
        window_group_frame_pillbox_cells(frame_context, 0, window.size.columns)
    }

    /// Returns mouse hit cells for pane-frame agent model and reasoning pills.
    pub(super) fn active_window_mouse_pane_agent_status_cells(
        &self,
        frame_context: &TerminalFrameContext,
    ) -> Vec<MousePaneAgentStatusCell> {
        let Some(window) = self.session.active_window() else {
            return Vec::new();
        };
        if !self.pane_frames_enabled {
            return Vec::new();
        }
        let group_top_offset = u16::from(self.session.window_groups().len() > 1);
        let mut display_window = window.clone();
        if group_top_offset > 0
            && let Ok(size) = Size::new(
                window.size.columns,
                window.size.rows.saturating_sub(group_top_offset).max(1),
            )
        {
            display_window.size = size;
        }
        let Ok(geometries) = rendered_pane_geometries(&display_window, self.window_frames_enabled)
        else {
            return Vec::new();
        };
        let row_offset = group_top_offset.saturating_add(u16::from(
            self.window_frames_enabled && self.window_frame_position == TerminalFramePosition::Top,
        ));
        pane_frame_agent_status_pillbox_cells(
            &display_window,
            frame_context,
            &self.pane_frame_template,
            self.pane_frame_position,
            row_offset,
            &geometries,
        )
    }

    /// Returns mouse hit cells for the currently open pane agent selector.
    fn mouse_pane_agent_selector_cells(&self) -> Vec<MousePaneAgentSelectorCell> {
        let Some(selector) = self.pane_agent_status_selector.as_ref() else {
            return Vec::new();
        };
        let Some(window) = self.session.active_window() else {
            return Vec::new();
        };
        let layout = runtime_pane_agent_status_selector_layout(selector, window.size);
        layout
            .visible_items
            .into_iter()
            .flat_map(|item| {
                (0..layout.width).filter_map(move |offset| {
                    Some(MousePaneAgentSelectorCell {
                        column: layout.column.checked_add(offset)?,
                        row: item.row,
                        pane_index: selector.pane_index,
                        field: selector.field,
                        item_index: item.item_index,
                    })
                })
            })
            .collect()
    }
    /// Reports whether the active window currently needs agent animation.
    fn active_window_has_agent_animation(&self) -> bool {
        self.session
            .active_window()
            .into_iter()
            .flat_map(|window| window.panes().iter())
            .any(|pane| {
                let pane_id = pane.id.as_str();
                self.pane_has_live_agent_footer(pane_id)
                    || self.pane_has_active_agent_frame_status(pane_id)
            })
    }

    /// Reports whether the pane currently renders a live agent footer.
    fn pane_has_live_agent_footer(&self, pane_id: &str) -> bool {
        if self.agent_compacting_panes.contains_key(pane_id)
            || self.agent_remembering_panes.contains_key(pane_id)
        {
            return true;
        }
        let Some(running_turn_id) = self
            .agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
        else {
            return false;
        };
        self.agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == running_turn_id)
    }

    /// Reports whether a pane has an active-work status in its frame context.
    fn pane_has_active_agent_frame_status(&self, pane_id: &str) -> bool {
        if self.agent_compacting_panes.contains_key(pane_id)
            || self.agent_remembering_panes.contains_key(pane_id)
        {
            return true;
        }
        self.agent_turn_ledger
            .turns()
            .iter()
            .rev()
            .find(|turn| turn.pane_id == pane_id)
            .is_some_and(|turn| {
                matches!(
                    self.runtime_agent_frame_status(turn),
                    "queued"
                        | "running"
                        | "remembering"
                        | "thinking"
                        | "executing"
                        | "waiting"
                        | "compacting"
                )
            })
    }

    /// Builds the animation tick used by terminal frame rendering.
    fn runtime_frame_animation_tick_ms(&self) -> u64 {
        if self.terminal_reduced_motion || !self.active_window_has_agent_animation() {
            0
        } else {
            current_unix_millis()
        }
    }
    /// Builds right-status context only for fields the active template uses.
    fn runtime_window_status_context(&self) -> Option<TerminalWindowStatusContext> {
        if self.window_frame_right_status_template.trim().is_empty() {
            return None;
        }
        let template = self.window_frame_right_status_template.clone();
        let active_pane_working_directory = if template.contains("#{pane.pwd}") {
            self.active_pane_id()
                .ok()
                .and_then(|pane_id| self.pane_current_working_directory(&pane_id))
                .as_deref()
                .map(Self::runtime_pane_frame_working_directory_display)
        } else {
            None
        };
        let system_uptime = if template.contains("#{system.uptime}") {
            runtime_human_system_uptime()
        } else {
            String::new()
        };
        let datetime_local = if template.contains("#{datetime.local}") {
            runtime_local_datetime_seconds_string()
        } else {
            String::new()
        };
        let status_pills = self
            .window_status_pill_cache
            .borrow_mut()
            .refresh_active(&self.window_status_pill_definitions, &template);
        Some(TerminalWindowStatusContext {
            template,
            active_pane_working_directory,
            status_pills,
            system_uptime,
            datetime_local,
        })
    }

    /// Runs the terminal frame context operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn terminal_frame_context(&self) -> TerminalFrameContext {
        let pending_observer_count = self
            .session
            .observers()
            .iter()
            .filter(|observer| observer.state == ObserverDecisionState::Pending)
            .count();
        let policy_mode =
            Self::runtime_frame_policy_mode_name(self.permission_policy.approval_policy)
                .to_string();
        let shell_process_name = self
            .session
            .shell
            .path()
            .file_name()
            .map(|name| name.to_string_lossy().to_string());
        let mut context = TerminalFrameContext {
            session_id: Some(self.session.id.to_string()),
            policy_mode: Some(policy_mode),
            pending_observer_count,
            pressed_window_action: self.pressed_window_action.clone(),
            animation_tick_ms: self.runtime_frame_animation_tick_ms(),
            reduced_motion: self.terminal_reduced_motion,
            window_status: self.runtime_window_status_context(),
            ..TerminalFrameContext::default()
        };
        let active_window_id = self
            .session
            .active_window()
            .map(|window| window.id.to_string());
        let active_group_id = self
            .session
            .active_group()
            .map(|group| group.id.to_string());

        for group in self.session.window_groups() {
            context.groups.push(TerminalWindowGroupFrameContext {
                id: group.id.to_string(),
                index: group.index,
                title: if group.name.trim().is_empty() {
                    group.id.to_string()
                } else {
                    group.name.clone()
                },
                active: active_group_id.as_ref() == Some(&group.id.to_string()),
            });
        }

        for window in self.session.active_group_windows() {
            context.windows.push(TerminalWindowFrameContext {
                id: window.id.to_string(),
                index: self
                    .session
                    .active_group_window_display_index(&window.id)
                    .unwrap_or(window.index),
                title: window.title(),
                active: active_window_id.as_ref() == Some(&window.id.to_string()),
                subagent: self.subagent_window_ids.contains(window.id.as_str()),
            });
            let pane_ids = window
                .panes()
                .iter()
                .map(|pane| pane.id.to_string())
                .collect::<Vec<_>>();
            let active_count = self
                .agent_turn_ledger
                .turns()
                .iter()
                .filter(|turn| {
                    turn.state == AgentTurnState::Running
                        && pane_ids.iter().any(|pane_id| pane_id == &turn.pane_id)
                })
                .count()
                .saturating_add(
                    self.agent_compacting_panes
                        .iter()
                        .filter(|(pane_id, _)| {
                            pane_ids.iter().any(|window_pane| window_pane == *pane_id)
                        })
                        .count(),
                )
                .saturating_add(
                    self.agent_remembering_panes
                        .iter()
                        .filter(|(pane_id, _)| {
                            pane_ids.iter().any(|window_pane| window_pane == *pane_id)
                        })
                        .count(),
                );
            context
                .window_agent_active_counts
                .insert(window.id.to_string(), active_count);
            context.window_unread_message_counts.insert(
                window.id.to_string(),
                self.message_service.queued_window_message_count(&window.id),
            );

            for pane in window.panes() {
                let pane_id = pane.id.to_string();
                let latest_turn = self
                    .agent_turn_ledger
                    .turns()
                    .iter()
                    .rev()
                    .find(|turn| turn.pane_id == pane_id);
                let agent_session = self.agent_shell_store.get(&pane_id);
                let mode = if self.active_copy_modes.contains_key(pane_id.as_str()) {
                    "copy"
                } else if agent_session.is_some_and(|session| {
                    matches!(session.visibility, AgentShellVisibility::Visible)
                }) {
                    "agent"
                } else {
                    "normal"
                };
                let agent_id = latest_turn
                    .map(|turn| turn.agent_id.clone())
                    .or_else(|| agent_session.map(|_| format!("agent-{pane_id}")));
                let agent_name = agent_id
                    .as_deref()
                    .map(|agent_id| self.runtime_agent_display_name(agent_id));
                let active_agent_profile = agent_session
                    .is_some()
                    .then(|| {
                        self.active_model_profile_for_pane(
                            &pane_id,
                            &format!("agent-{pane_id}"),
                            None,
                        )
                        .ok()
                    })
                    .flatten();
                let agent_status = self
                    .agent_compacting_panes
                    .contains_key(&pane_id)
                    .then(|| "compacting".to_string())
                    .or_else(|| {
                        self.agent_remembering_panes
                            .contains_key(&pane_id)
                            .then(|| "memorizing".to_string())
                    })
                    .or_else(|| {
                        latest_turn.map(|turn| self.runtime_agent_frame_status(turn).to_string())
                    })
                    .or_else(|| agent_session.map(|_| "idle".to_string()));
                let agent_model = latest_turn
                    .and_then(|turn| {
                        self.agent_turn_model_profiles
                            .get(&turn.turn_id)
                            .map(|profile| profile.model.clone())
                            .or_else(|| {
                                self.provider_registry
                                    .resolve_profile(&turn.model_profile)
                                    .ok()
                                    .map(|profile| profile.model)
                            })
                    })
                    .or_else(|| {
                        active_agent_profile
                            .as_ref()
                            .map(|(_name, profile)| profile.model.clone())
                    });
                let agent_reasoning = latest_turn
                    .and_then(|turn| {
                        self.agent_turn_model_profiles
                            .get(&turn.turn_id)
                            .and_then(|profile| profile.reasoning_display_value())
                    })
                    .or_else(|| {
                        active_agent_profile
                            .as_ref()
                            .and_then(|(_name, profile)| profile.reasoning_display_value())
                    });
                let agent_thinking_profile = latest_turn
                    .and_then(|turn| self.agent_turn_model_profiles.get(&turn.turn_id).cloned())
                    .or_else(|| {
                        active_agent_profile
                            .as_ref()
                            .map(|(_name, profile)| profile.clone())
                    });
                let agent_thinking = agent_thinking_profile.as_ref().and_then(|profile| {
                    self.model_profile_thinking_enabled(profile)
                        .map(|enabled| if enabled { "on" } else { "off" }.to_string())
                });
                let agent_routing = agent_session.map(|_| {
                    if self
                        .agent_routing_overrides
                        .get(&pane_id)
                        .copied()
                        .unwrap_or(self.agent_routing)
                    {
                        "auto:on".to_string()
                    } else {
                        "auto:off".to_string()
                    }
                });
                let agent_latency_profile = latest_turn
                    .and_then(|turn| self.agent_turn_model_profiles.get(&turn.turn_id).cloned())
                    .or_else(|| {
                        active_agent_profile
                            .as_ref()
                            .map(|(_name, profile)| profile.clone())
                    });
                let agent_latency = agent_latency_profile.as_ref().and_then(|profile| {
                    self.model_profile_supports_latency_preference(profile)
                        .then(|| {
                            profile
                                .latency_preference
                                .clone()
                                .unwrap_or_else(|| "default".to_string())
                        })
                });
                let agent_context_usage = agent_session.and_then(|session| {
                    self.agent_context_usage_by_conversation
                        .get(&session.session_id)
                        .cloned()
                });
                let history_position = self
                    .active_copy_modes
                    .get(pane_id.as_str())
                    .filter(|copy_mode| !copy_mode.is_at_bottom())
                    .map(|copy_mode| {
                        format!(
                            "{}/{}",
                            copy_mode.visible_end_line(),
                            copy_mode.line_count()
                        )
                    });
                let current_working_directory = self
                    .pane_current_working_directory(pane_id.as_str())
                    .as_deref()
                    .map(Self::runtime_pane_frame_working_directory_display);
                context.panes.insert(
                    pane_id.clone(),
                    TerminalPaneFrameContext {
                        primary_pid: self.primary_pid_for_live_pane_process(pane_id.as_str()),
                        process_name: self.pane_processes.process_name(pane_id.as_str()).or_else(
                            || {
                                self.primary_pid_for_live_pane_process(pane_id.as_str())
                                    .and(shell_process_name.clone())
                            },
                        ),
                        exit_status: self
                            .pane_exit_records
                            .get(pane_id.as_str())
                            .map(|record| record.exit_status.frame_value()),
                        current_working_directory,
                        mode: Some(mode.to_string()),
                        agent_id,
                        agent_name,
                        agent_status,
                        agent_model,
                        agent_reasoning,
                        agent_thinking,
                        agent_routing,
                        agent_latency,
                        agent_preset: self.agent_preset_display_value_for_pane(pane_id.as_str()),
                        agent_context_usage,
                        history_position,
                        agent_prompt: agent_session
                            .is_some_and(|session| {
                                matches!(session.visibility, AgentShellVisibility::Visible)
                            })
                            .then(|| {
                                self.agent_prompt_inputs
                                    .get(&pane_id)
                                    .map(|input| input.prompt.clone())
                                    .unwrap_or_else(|| {
                                        ReadlinePrompt::new(ReadlinePromptKind::Agent)
                                    })
                            }),
                        agent_display_lines: self
                            .runtime_agent_prompt_display_lines_for_pane(&pane_id),
                    },
                );
            }
        }

        context
    }

    /// Returns the human-readable display name for a pane-associated agent.
    fn runtime_agent_display_name(&self, agent_id: &str) -> String {
        self.subagent_lineage
            .get(agent_id)
            .and_then(|lineage| {
                let display_name = lineage.display_name.trim();
                (!display_name.is_empty()).then(|| display_name.to_string())
            })
            .unwrap_or_else(|| ROOT_AGENT_DISPLAY_NAME.to_string())
    }

    /// Returns the pane-frame status for an agent turn.
    fn runtime_agent_frame_status(&self, turn: &AgentTurnRecord) -> &'static str {
        if turn.state == AgentTurnState::Blocked
            && self
                .joined_subagent_dependencies
                .values()
                .any(|dependency| dependency.parent_turn_id == turn.turn_id)
        {
            return "waiting";
        }
        if turn.state == AgentTurnState::Running {
            return self.runtime_running_agent_frame_status(turn);
        }
        runtime_agent_turn_state_name(turn.state)
    }

    /// Returns the active display substate for a running agent turn.
    fn runtime_running_agent_frame_status(&self, turn: &AgentTurnRecord) -> &'static str {
        if self.running_shell_transactions.values().any(|transaction| {
            transaction.turn_id == turn.turn_id
                && matches!(
                    transaction.kind,
                    RunningShellTransactionKind::AgentAction { .. }
                )
        }) {
            return "executing";
        }
        if self.running_shell_transactions.values().any(|transaction| {
            transaction.turn_id == turn.turn_id
                && transaction.kind == RunningShellTransactionKind::ReadinessProbe
        }) {
            return "waiting";
        }
        if self
            .agent_turn_executions
            .get(&turn.turn_id)
            .is_some_and(|execution| {
                self.execution_has_pending_shell_dispatch(&turn.turn_id, execution)
            })
        {
            return "waiting";
        }
        if self.runtime_agent_turn_is_auto_sizing_routing(turn) {
            return "routing";
        }
        if self.pending_agent_provider_tasks.contains(&turn.turn_id)
            || self
                .claimed_agent_provider_tasks
                .contains_key(&turn.turn_id)
        {
            return "thinking";
        }
        "running"
    }
    /// Returns whether a running turn is still in the auto-sizing router phase.
    fn runtime_agent_turn_is_auto_sizing_routing(&self, turn: &AgentTurnRecord) -> bool {
        if !self.agent_routing_enabled_for_pane(&turn.pane_id) {
            return false;
        }
        if self.agent_turn_executions.contains_key(&turn.turn_id) {
            return false;
        }
        if !(self.pending_agent_provider_tasks.contains(&turn.turn_id)
            || self
                .claimed_agent_provider_tasks
                .contains_key(&turn.turn_id))
        {
            return false;
        }
        true
    }

    /// Formats a pane working directory for compact pane-frame display.
    fn runtime_pane_frame_working_directory_display(path: &std::path::Path) -> String {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(std::path::PathBuf::from);
        if let Some(home) = home.as_deref() {
            if path == home {
                return "~".to_string();
            }
            if let Ok(relative) = path.strip_prefix(home)
                && !relative.as_os_str().is_empty()
            {
                let segments = relative
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                return Self::runtime_compact_working_directory_segments(Some("~"), &segments);
            }
        }

        let segments = path
            .components()
            .filter_map(|component| match component {
                std::path::Component::RootDir => None,
                std::path::Component::Normal(segment) => {
                    Some(segment.to_string_lossy().into_owned())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let prefix = path.has_root().then_some("/");
        Self::runtime_compact_working_directory_segments(prefix, &segments)
    }

    /// Compacts pane working-directory segments for frame rendering.
    fn runtime_compact_working_directory_segments(
        prefix: Option<&str>,
        segments: &[String],
    ) -> String {
        if segments.is_empty() {
            return prefix.unwrap_or_default().to_string();
        }
        if segments.len() <= 3 {
            return match prefix {
                Some("~") => format!("~/{segments}", segments = segments.join("/")),
                Some("/") => format!("/{segments}", segments = segments.join("/")),
                Some(prefix) => format!("{prefix}/{segments}", segments = segments.join("/")),
                None => segments.join("/"),
            };
        }
        format!(
            "…/{}",
            segments[segments.len().saturating_sub(3)..].join("/")
        )
    }

    /// Runs the pending observer status line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn pending_observer_status_line(&self) -> Option<String> {
        let pending = self
            .session
            .observers()
            .iter()
            .filter(|observer| observer.state == ObserverDecisionState::Pending)
            .count();
        (pending > 0).then(|| format!("observer: {pending} pending - Ctrl+A O choose-observer"))
    }
}
