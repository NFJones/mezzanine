//! Runtime Render implementation.
//!
//! This module owns the runtime render boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::types::{
    RunningShellTransactionKind, RuntimeDisplayOverlay, RuntimeDisplayOverlaySearchMatch,
    RuntimeDisplayOverlaySelection, RuntimeDisplayOverlaySelectionKind, RuntimeMouseClickState,
    RuntimePaneAgentStatusSelector, RuntimePrimaryPromptInput,
};
use super::{
    AgentShellVisibility, AgentTurnRecord, AgentTurnState, AttachedClientStepApplication,
    AttachedTerminalClientStepPlan, ClientViewRole, CopyMode, CopyModeKeyAction,
    DeferredCommandPromptHistoryWrite, DeferredPaneInput, EventKind, MIN_PANE_COLUMNS,
    MIN_PANE_ROWS, MezError, MouseAction, MouseBorderCell, MousePaneRegion, MouseResizeDragState,
    MouseSelectionDragState, MouseWindowActionFrameCell, MouseWindowFrameCell, MuxAction,
    ObserverDecisionState, PaneDescriptor, PaneGeometry, PaneInputDispatch,
    PaneNavigationDirection, PasteBufferTarget, ReadlineInputDecoder, ReadlineOutcome,
    ReadlinePrompt, ReadlinePromptKind, RenderedClientView, Result,
    RuntimeAgentModifiedFileSummary, RuntimeAgentPromptInput, RuntimeSessionService, Size,
    SplitDirection, TerminalClientLoopAction, TerminalClientLoopConfig, TerminalFrameContext,
    TerminalFramePosition, TerminalPaneFrameContext, TerminalScreen, TerminalWindowFrameContext,
    TerminalWindowStatusContext, WindowFocusTarget, WindowFrameAction,
    agent_prompt_reserved_line_count, current_unix_millis, current_unix_seconds, json_escape,
    key_chord_input_bytes, mouse_action_name, mux_action_command_prompt_prefill, mux_action_name,
    pane_border_cells_for_geometries, pane_content_size_for_geometry,
    pane_frame_merges_into_divider, pane_navigation_direction,
    pane_render_region_size_for_geometry, parse_command_sequence, render_attached_client_view,
    rendered_pane_geometries, rendered_window_body_size, runtime_agent_shell_command_response_json,
    runtime_agent_turn_duration_display, runtime_agent_turn_state_name,
    runtime_approval_policy_name, runtime_copy_position_for_view, runtime_fit_status_line,
    runtime_paste_bytes, window_frame_action_pillbox_cells, window_frame_pillbox_cells,
};
/// Maximum elapsed time between two pane-content clicks recognized as a double click.
const DOUBLE_CLICK_WORD_SELECTION_WINDOW_MS: u64 = 500;

use crate::agent::{
    AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE, ActionResult, AgentAction,
    agent_output_content_type_is_diff, agent_output_content_type_is_markdown,
};
use crate::command::baseline_commands;
use crate::mcp::McpServerStatus;
use crate::readline::DEFAULT_READLINE_HISTORY_LIMIT;
use crate::selector::{
    SelectorCandidate, SelectorCandidateKind, SelectorExtraCandidate, SelectorSurface,
};
use crate::terminal::{
    CopyPosition, GraphicRendition, GroupFocusTarget, MousePaneAgentSelectorCell,
    MousePaneAgentStatusCell, PaneAgentStatusField, TerminalStyleSpan, TerminalStyledLine,
    TerminalWindowGroupFrameContext, UiTheme, WindowFrameCommandKind,
    compose_modal_display_overlay_lines, compose_prompt_overlay_presentation_with_styles,
    modal_display_overlay_max_scroll, modal_display_overlay_page_rows,
    pane_frame_agent_status_pillbox_cells, terminal_grapheme_width, terminal_graphemes,
    terminal_text_width, window_group_frame_pillbox_cells,
};
use crate::transcript::AgentPresentationEntry;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod client_view;
mod geometry;
mod input;
mod mouse;
mod overlay;
mod paste;
mod presentation;
mod time;

use geometry::clipped_overlay_style_span;
use input::{
    RuntimeDisplayOverlayInputAction, RuntimeSelectorInputAction,
    runtime_display_overlay_input_action, runtime_selector_input_action,
    runtime_selector_step_index,
};
use mouse::{
    MouseResizeDragUpdate, horizontal_mouse_resize_state, mouse_resize_update_from_state,
    vertical_mouse_resize_state,
};
use overlay::*;
use paste::{RuntimePasteSource, runtime_readline_paste_bytes};
use presentation::*;
use time::{runtime_human_system_uptime, runtime_local_datetime_seconds_string};

// Attached terminal input application and client view rendering.

/// Root pane-agent display name shown in pane status surfaces.
const ROOT_AGENT_DISPLAY_NAME: &str = "manager";

/// Carries Mouse Pane Target state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MousePaneTarget {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pane_id: String,
    /// Stores the position value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    position: CopyPosition,
}

/// Carries Mouse Selection Edge state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseSelectionEdge {
    /// Represents the Above case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Above,
    /// Represents the Below case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Below,
}

/// Carries Mouse Selection Target state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MouseSelectionTarget {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pane_id: String,
    /// Stores the position value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    position: CopyPosition,
    /// Stores the edge value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    edge: Option<MouseSelectionEdge>,
}

impl MouseSelectionEdge {
    /// Runs the scroll delta operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn scroll_delta(self, origin: CopyPosition, current: CopyPosition) -> isize {
        let lines = origin.line.abs_diff(current.line).max(1);
        let lines = isize::try_from(lines).unwrap_or(isize::MAX);
        match self {
            MouseSelectionEdge::Above => -lines,
            MouseSelectionEdge::Below => lines,
        }
    }
}

impl RuntimeSessionService {
    /// Returns the compact approval label shown in the pane agent status area.
    fn runtime_frame_policy_mode_name(policy: crate::permissions::ApprovalPolicy) -> &'static str {
        match policy {
            crate::permissions::ApprovalPolicy::Ask => "ask",
            crate::permissions::ApprovalPolicy::AutoAllow => "auto-allow",
            crate::permissions::ApprovalPolicy::FullAccess => "full-access",
        }
    }

    /// Runs the active agent shell visible operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn active_agent_shell_visible(&self) -> Result<bool> {
        let pane_id = self.active_pane_id()?;
        Ok(self
            .agent_shell_store
            .get(&pane_id)
            .is_some_and(|session| session.visibility == AgentShellVisibility::Visible))
    }

    /// Reports whether the focused pane is waiting for an agent turn to stop before exit.
    fn active_agent_shell_exit_pending(&self) -> Result<bool> {
        let pane_id = self.active_pane_id()?;
        Ok(self.agent_shell_store.get(&pane_id).is_some_and(|session| {
            session.visibility == AgentShellVisibility::HidePendingTaskCompletion
        }))
    }

    /// Runs the write input to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn write_input_to_pane(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        target: Option<&str>,
        input: &[u8],
    ) -> Result<PaneInputDispatch> {
        self.require_live()?;
        if input.is_empty() {
            return Err(MezError::invalid_args("pane input must not be empty"));
        }
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let descriptor = match target {
            Some(target) => self.find_pane_descriptor(target).ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
            })?,
            None => self.active_window_pane_descriptor(None)?,
        };
        self.write_input_to_pane_descriptor(primary_client_id, &descriptor, input)
    }

    /// Runs the write input to pane descriptor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn write_input_to_pane_descriptor(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        descriptor: &PaneDescriptor,
        input: &[u8],
    ) -> Result<PaneInputDispatch> {
        self.require_live()?;
        if input.is_empty() {
            return Err(MezError::invalid_args("pane input must not be empty"));
        }
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let primary_pid = self
            .primary_pid_for_live_pane_process(descriptor.pane_id.as_str())
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pane process not found",
                )
            })?;
        self.clear_shell_output_filters_for_foreground_input(descriptor.pane_id.as_str());
        self.active_copy_modes.remove(descriptor.pane_id.as_str());
        self.scrollback_copy_mode_panes
            .remove(descriptor.pane_id.as_str());
        self.write_runtime_pane_input(descriptor.pane_id.as_str(), input)?;
        Ok(PaneInputDispatch {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: descriptor.pane_id.to_string(),
            primary_pid,
            bytes_written: input.len(),
        })
    }

    /// Runs the apply attached terminal step plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn apply_attached_terminal_step_plan(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        step: &AttachedTerminalClientStepPlan,
    ) -> Result<AttachedClientStepApplication> {
        self.apply_attached_terminal_step_plan_inner(primary_client_id, step, false)
            .map(|(application, _)| application)
    }

    /// Runs the apply attached terminal step plan deferred pane io operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn apply_attached_terminal_step_plan_deferred_pane_io(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        step: &AttachedTerminalClientStepPlan,
    ) -> Result<(AttachedClientStepApplication, Vec<DeferredPaneInput>)> {
        self.apply_attached_terminal_step_plan_inner(primary_client_id, step, true)
    }

    /// Shows or clears the primary-client command display overlay.
    ///
    /// Non-empty line sets are rendered as a modal full-window view on the next
    /// primary render pass. An empty line set clears any active overlay. This
    /// fails when the runtime is no longer live.
    pub fn show_primary_display_overlay(&mut self, lines: Vec<String>) -> Result<()> {
        let line_style_spans = vec![Vec::new(); lines.len()];
        self.show_primary_display_overlay_inner(lines, line_style_spans, Vec::new(), false)
    }

    /// Shows or clears the primary-client recoverable error status overlay.
    ///
    /// Error overlays render over the window status bar and are dismissed by
    /// the next user action without consuming that action. This keeps runtime
    /// errors visible without turning them into modal state.
    pub fn show_primary_error_overlay(&mut self, lines: Vec<String>) -> Result<()> {
        self.require_live()?;
        self.primary_error_status_overlay = lines
            .into_iter()
            .find(|line| !line.trim().is_empty())
            .map(|line| runtime_primary_error_status_text(&line));
        Ok(())
    }

    /// Runs the show primary display overlay inner operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn show_primary_display_overlay_inner(
        &mut self,
        lines: Vec<String>,
        mut line_style_spans: Vec<Vec<TerminalStyleSpan>>,
        selections: Vec<RuntimeDisplayOverlaySelection>,
        dismiss_on_any_input: bool,
    ) -> Result<()> {
        self.require_live()?;
        self.primary_display_overlay = if lines.is_empty() {
            None
        } else {
            line_style_spans.truncate(lines.len());
            line_style_spans.resize(lines.len(), Vec::new());
            let active_selection_index = (!selections.is_empty()).then_some(0);
            Some(RuntimeDisplayOverlay {
                lines,
                line_style_spans,
                scroll_offset: 0,
                search_input: None,
                search_query: None,
                search_match: None,
                search_status: None,
                mouse_selection: None,
                selections,
                active_selection_index,
                dismiss_on_any_input,
            })
        };
        Ok(())
    }

    /// Clears the primary-client command display overlay.
    ///
    /// Returns true when an overlay was active before the call.
    pub fn clear_primary_display_overlay(&mut self) -> bool {
        self.primary_display_overlay.take().is_some()
    }

    /// Appends terminal-command display output to the active pane buffer.
    ///
    /// Short acknowledgement-style command output should remain in the pane
    /// transcript instead of forcing a modal command-output overlay. The bytes
    /// are fed through the same pane-screen ingestion path as process output so
    /// rendering state, scrollback, and observers stay consistent.
    fn append_runtime_command_display_lines_to_active_pane(
        &mut self,
        lines: &[String],
    ) -> Result<()> {
        let visible_lines = lines
            .iter()
            .map(|line| sanitized_agent_terminal_line(line))
            .filter(|line| !line.trim().is_empty())
            .take(200)
            .collect::<Vec<_>>();
        if visible_lines.is_empty() {
            return Ok(());
        }
        let pane_id = self.active_pane_id()?.to_string();
        let mut bytes = Vec::new();
        for line in visible_lines {
            bytes.extend_from_slice(b"\r\nmez: ");
            bytes.extend_from_slice(line.as_bytes());
        }
        bytes.extend_from_slice(b"\r\n");
        self.apply_pane_output_bytes(pane_id, bytes)?;
        Ok(())
    }

    /// Opens an actor-owned command prompt on the primary client.
    ///
    /// The prompt is rendered as part of the next primary client view. Input is
    /// captured by runtime state until the prompt is submitted, cancelled, or
    /// closed by EOF.
    pub fn enter_primary_command_prompt(&mut self, prefill: &str) -> Result<()> {
        self.enter_primary_prompt(ReadlinePromptKind::Command, prefill)
    }

    /// Runs the enter primary prompt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn enter_primary_prompt(&mut self, kind: ReadlinePromptKind, prefill: &str) -> Result<()> {
        self.require_live()?;
        if kind == ReadlinePromptKind::Command && self.primary_command_prompt_history.is_empty() {
            self.reload_primary_command_prompt_history()?;
        }
        let mut prompt_input = runtime_primary_prompt_input(kind, prefill);
        if kind == ReadlinePromptKind::Command {
            prompt_input
                .prompt
                .buffer
                .set_history(self.primary_command_prompt_history.clone());
            prompt_input
                .prompt
                .set_selector_extra_candidates(self.runtime_command_selector_extra_candidates());
        }
        self.primary_prompt_input = Some(prompt_input);
        Ok(())
    }

    /// Runs the apply attached terminal step plan inner operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_attached_terminal_step_plan_inner(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        step: &AttachedTerminalClientStepPlan,
        defer_pane_io: bool,
    ) -> Result<(AttachedClientStepApplication, Vec<DeferredPaneInput>)> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let mut deferred_pane_inputs = Vec::new();
        let mut report = AttachedClientStepApplication {
            forwarded_bytes: 0,
            mux_actions_applied: 0,
            mouse_actions_reported: 0,
            unsupported_actions: Vec::new(),
            agent_prompt_inputs_applied: 0,
            view_refresh_required: false,
            full_redraw_required: false,
        };

        if !step.actions.is_empty() && self.primary_error_status_overlay.take().is_some() {
            report.view_refresh_required = true;
            report.full_redraw_required = true;
            return Ok((report, deferred_pane_inputs));
        }

        for action in &step.actions {
            if !matches!(action, TerminalClientLoopAction::EnterPrefixKeyMode) {
                self.primary_prefix_key_pending = false;
            }
            let primary_display_overlay_requires_full_redraw =
                self.primary_display_overlay_action_requires_full_redraw(action);
            if self.primary_display_overlay.is_some()
                && self.apply_primary_display_overlay_terminal_action(primary_client_id, action)?
            {
                report.view_refresh_required = true;
                if primary_display_overlay_requires_full_redraw {
                    report.full_redraw_required = true;
                }
                continue;
            }
            if self.pane_agent_status_selector.is_some()
                && self
                    .apply_pane_agent_status_selector_terminal_action(primary_client_id, action)?
            {
                report.view_refresh_required = true;
                continue;
            }
            if self.pane_agent_status_selector.is_some()
                && !matches!(
                    action,
                    TerminalClientLoopAction::HandleMouse(
                        MouseAction::OpenPaneAgentStatusSelector { .. }
                            | MouseAction::HoverPaneAgentStatusSelector { .. }
                            | MouseAction::SelectPaneAgentStatusSelector { .. }
                            | MouseAction::ScrollPaneAgentStatusSelector { .. }
                            | MouseAction::ClosePaneAgentStatusSelector
                    )
                )
            {
                self.pane_agent_status_selector = None;
                report.view_refresh_required = true;
            }
            if self.primary_prompt_input.is_some()
                && matches!(
                    action,
                    TerminalClientLoopAction::ForwardToPane(_)
                        | TerminalClientLoopAction::ForwardMouseToPane { .. }
                )
            {
                if self.apply_primary_prompt_terminal_action(primary_client_id, action)? {
                    report.view_refresh_required = true;
                }
                continue;
            }
            match action {
                TerminalClientLoopAction::ForwardToPane(input) => {
                    if self.active_agent_shell_visible()? {
                        if self.apply_attached_agent_prompt_input(primary_client_id, input)? {
                            self.sync_tracked_pty_sizes()?;
                            report.agent_prompt_inputs_applied =
                                report.agent_prompt_inputs_applied.saturating_add(1);
                            report.view_refresh_required = true;
                            if !self.active_agent_shell_visible()? {
                                report.full_redraw_required = true;
                            }
                        }
                    } else if self.active_agent_shell_exit_pending()? {
                        let pane_id = self.active_pane_id()?;
                        self.append_agent_status_text_to_terminal_buffer(
                            &pane_id,
                            "agent: input blocked while agent shell is stopping",
                        )?;
                        report.agent_prompt_inputs_applied =
                            report.agent_prompt_inputs_applied.saturating_add(1);
                        report.view_refresh_required = true;
                        report.full_redraw_required = true;
                    } else {
                        if defer_pane_io {
                            let descriptor = self.active_window_pane_descriptor(None)?;
                            self.clear_shell_output_filters_for_foreground_input(
                                descriptor.pane_id.as_str(),
                            );
                            self.active_copy_modes.remove(descriptor.pane_id.as_str());
                            self.scrollback_copy_mode_panes
                                .remove(descriptor.pane_id.as_str());
                            deferred_pane_inputs.push(DeferredPaneInput {
                                pane_id: descriptor.pane_id.to_string(),
                                bytes: input.clone(),
                                priority: false,
                            });
                            report.forwarded_bytes =
                                report.forwarded_bytes.saturating_add(input.len());
                        } else {
                            let dispatch =
                                self.write_input_to_pane(primary_client_id, None, input)?;
                            report.forwarded_bytes = report
                                .forwarded_bytes
                                .saturating_add(dispatch.bytes_written);
                        }
                    }
                }
                TerminalClientLoopAction::ForwardMouseToPane { pane_id, input } => {
                    let Some(descriptor) = self.find_pane_descriptor(pane_id) else {
                        continue;
                    };
                    if defer_pane_io {
                        self.clear_shell_output_filters_for_foreground_input(
                            descriptor.pane_id.as_str(),
                        );
                        self.active_copy_modes.remove(descriptor.pane_id.as_str());
                        self.scrollback_copy_mode_panes
                            .remove(descriptor.pane_id.as_str());
                        deferred_pane_inputs.push(DeferredPaneInput {
                            pane_id: descriptor.pane_id.to_string(),
                            bytes: input.clone(),
                            priority: false,
                        });
                        report.forwarded_bytes = report.forwarded_bytes.saturating_add(input.len());
                    } else {
                        let dispatch = self.write_input_to_pane_descriptor(
                            primary_client_id,
                            &descriptor,
                            input,
                        )?;
                        report.forwarded_bytes = report
                            .forwarded_bytes
                            .saturating_add(dispatch.bytes_written);
                    }
                }
                TerminalClientLoopAction::ExecuteMux(action) => {
                    if let Some(prefill) = mux_action_command_prompt_prefill(*action) {
                        match self.enter_primary_command_prompt(prefill) {
                            Ok(()) => {
                                report.view_refresh_required = true;
                            }
                            Err(error) => {
                                self.present_attached_action_error(&mut report, &error)?
                            }
                        }
                        continue;
                    }
                    let toggles_agent_shell = *action == MuxAction::ToggleAgentShell;
                    match self.apply_attached_mux_action(primary_client_id, *action) {
                        Ok(true) => {
                            report.mux_actions_applied =
                                report.mux_actions_applied.saturating_add(1);
                            report.view_refresh_required = true;
                            if toggles_agent_shell || Self::mux_action_requires_full_redraw(*action)
                            {
                                report.full_redraw_required = true;
                            }
                        }
                        Ok(false) => {
                            report
                                .unsupported_actions
                                .push(format!("mux:{}", mux_action_name(*action)));
                        }
                        Err(error) => self.present_attached_action_error(&mut report, &error)?,
                    }
                }
                TerminalClientLoopAction::ExecuteCommand(command) => {
                    match self.execute_terminal_command(primary_client_id, command) {
                        Ok(output) => {
                            self.append_lifecycle_event(
                                EventKind::Diagnostic,
                                format!(
                                    r#"{{"key_binding_command":"{}","output":"{}"}}"#,
                                    json_escape(command),
                                    json_escape(&output)
                                ),
                            )?;
                            report.mux_actions_applied =
                                report.mux_actions_applied.saturating_add(1);
                            report.view_refresh_required = true;
                            report.full_redraw_required = true;
                        }
                        Err(error) => self.present_attached_action_error(&mut report, &error)?,
                    }
                }
                TerminalClientLoopAction::HandleMouse(action) => {
                    let overlay_was_open = self.primary_display_overlay.is_some();
                    match self.apply_attached_mouse_action(primary_client_id, action.clone()) {
                        Ok(true) => {
                            report.mouse_actions_reported =
                                report.mouse_actions_reported.saturating_add(1);
                            report.view_refresh_required = true;
                            if Self::mouse_action_requires_full_redraw(action.clone())
                                || overlay_was_open != self.primary_display_overlay.is_some()
                            {
                                report.full_redraw_required = true;
                            }
                        }
                        Ok(false) => {
                            report.mouse_actions_reported =
                                report.mouse_actions_reported.saturating_add(1);
                            report
                                .unsupported_actions
                                .push(format!("mouse:{}", mouse_action_name(action.clone())));
                        }
                        Err(error) => self.present_attached_action_error(&mut report, &error)?,
                    }
                }
                TerminalClientLoopAction::HandleCopyMode(action) => {
                    match self.apply_attached_copy_mode_action(*action) {
                        Ok(true) => {
                            report.view_refresh_required = true;
                        }
                        Ok(false) => {
                            report
                                .unsupported_actions
                                .push(format!("copy-mode:{action:?}"));
                        }
                        Err(error) => self.present_attached_action_error(&mut report, &error)?,
                    }
                }
                TerminalClientLoopAction::EnterPrefixKeyMode => {
                    self.primary_prefix_key_pending = true;
                    report.view_refresh_required = true;
                }
                TerminalClientLoopAction::ReportUnboundPrefix(chord) => report
                    .unsupported_actions
                    .push(format!("prefix:unbound:{chord:?}")),
            }
        }

        self.persist_or_defer_registry_update()?;
        Ok((report, deferred_pane_inputs))
    }

    /// Returns true when a mux action can change pane/window geometry enough to
    /// require resetting the attached terminal frame before the next render.
    fn mux_action_requires_full_redraw(action: MuxAction) -> bool {
        matches!(
            action,
            MuxAction::NewWindow
                | MuxAction::NewGroup
                | MuxAction::SplitPaneVertical
                | MuxAction::SplitPaneHorizontal
                | MuxAction::TogglePaneZoom
                | MuxAction::CycleLayouts
                | MuxAction::KillPaneAfterConfirmation
                | MuxAction::BreakPaneToNewWindow
                | MuxAction::SwapPanePrevious
                | MuxAction::SwapPaneNext
        )
    }

    /// Records a recoverable foreground action error as a transient primary
    /// status notice instead of allowing it to abort the attached client.
    fn present_attached_action_error(
        &mut self,
        report: &mut AttachedClientStepApplication,
        error: &MezError,
    ) -> Result<()> {
        self.show_primary_error_overlay(vec![format!("mez error: {error}")])?;
        report.view_refresh_required = true;
        report.full_redraw_required = true;
        Ok(())
    }

    /// Returns true when a mouse action can change pane geometry and therefore
    /// needs a full attached-frame redraw after the action is applied.
    fn mouse_action_requires_full_redraw(action: MouseAction) -> bool {
        matches!(
            action,
            MouseAction::ResizePane { .. } | MouseAction::ReleaseWindowAction { .. }
        )
    }

    /// Runs the apply primary display overlay terminal action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_primary_display_overlay_terminal_action(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        action: &TerminalClientLoopAction,
    ) -> Result<bool> {
        match action {
            TerminalClientLoopAction::ForwardToPane(input)
            | TerminalClientLoopAction::ForwardMouseToPane { input, .. } => {
                self.apply_primary_display_overlay_input(primary_client_id, input)
            }
            TerminalClientLoopAction::HandleMouse(MouseAction::SelectDisplayOverlay {
                position,
            }) => self.apply_primary_display_overlay_selection(primary_client_id, *position),
            TerminalClientLoopAction::HandleMouse(MouseAction::BeginDisplayOverlaySelection {
                position,
            }) => self.begin_primary_display_overlay_mouse_selection(*position),
            TerminalClientLoopAction::HandleMouse(MouseAction::UpdateDisplayOverlaySelection {
                position,
            }) => self.update_primary_display_overlay_mouse_selection(*position),
            TerminalClientLoopAction::HandleMouse(MouseAction::FinishDisplayOverlaySelection {
                position,
            }) => self.finish_primary_display_overlay_mouse_selection(primary_client_id, *position),
            TerminalClientLoopAction::HandleMouse(MouseAction::ScrollDisplayOverlay { lines }) => {
                self.apply_primary_display_overlay_scroll(*lines)
            }
            TerminalClientLoopAction::ExecuteMux(_)
            | TerminalClientLoopAction::ExecuteCommand(_)
            | TerminalClientLoopAction::HandleMouse(_)
            | TerminalClientLoopAction::HandleCopyMode(_)
            | TerminalClientLoopAction::EnterPrefixKeyMode
            | TerminalClientLoopAction::ReportUnboundPrefix(_) => Ok(false),
        }
    }

    /// Reports whether one primary display overlay action should invalidate the
    /// attached client's retained output frame.
    ///
    /// Keyboard and mouse-wheel navigation only move the overlay viewport or
    /// active row, so the next rendered view can be applied through the normal
    /// diff renderer. Exiting the modal overlay or executing a selected row can
    /// expose a different underlying view or run a command, so those paths keep
    /// the stronger redraw signal.
    fn primary_display_overlay_action_requires_full_redraw(
        &self,
        action: &TerminalClientLoopAction,
    ) -> bool {
        match action {
            TerminalClientLoopAction::ForwardToPane(input)
            | TerminalClientLoopAction::ForwardMouseToPane { input, .. } => {
                if self
                    .primary_display_overlay
                    .as_ref()
                    .is_some_and(|overlay| overlay.dismiss_on_any_input && !input.is_empty())
                {
                    return true;
                }
                matches!(
                    runtime_display_overlay_input_action(input),
                    RuntimeDisplayOverlayInputAction::Exit
                        | RuntimeDisplayOverlayInputAction::SelectActive
                ) && self
                    .primary_display_overlay
                    .as_ref()
                    .is_none_or(|overlay| overlay.search_input.is_none())
            }
            TerminalClientLoopAction::HandleMouse(MouseAction::SelectDisplayOverlay { .. }) => true,
            TerminalClientLoopAction::HandleMouse(MouseAction::ScrollDisplayOverlay { .. }) => {
                false
            }
            TerminalClientLoopAction::ExecuteMux(_)
            | TerminalClientLoopAction::ExecuteCommand(_)
            | TerminalClientLoopAction::HandleMouse(_)
            | TerminalClientLoopAction::HandleCopyMode(_)
            | TerminalClientLoopAction::EnterPrefixKeyMode
            | TerminalClientLoopAction::ReportUnboundPrefix(_) => false,
        }
    }

    /// Executes the selectable command row under a primary display overlay
    /// mouse click.
    fn apply_primary_display_overlay_selection(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(overlay) = self.primary_display_overlay.as_ref() else {
            return Ok(false);
        };
        if position.line == 0 {
            return Ok(false);
        }
        let display_line_index = overlay
            .scroll_offset
            .saturating_add(position.line.saturating_sub(1));
        let selection_index = runtime_display_overlay_selection_index_at_position(
            overlay,
            display_line_index,
            position.column,
        );
        let Some(command) = selection_index
            .and_then(|index| overlay.selections.get(index))
            .map(|selection| selection.command.clone())
        else {
            return Ok(false);
        };
        if let Some(overlay) = self.primary_display_overlay.as_mut() {
            overlay.active_selection_index = selection_index;
        }
        self.execute_primary_display_overlay_selection_command(primary_client_id, &command)
    }

    /// Starts a mouse text selection in the primary command-output overlay.
    fn begin_primary_display_overlay_mouse_selection(
        &mut self,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(selection_position) = self.primary_display_overlay_position_for_mouse(position)
        else {
            return Ok(false);
        };
        if let Some(overlay) = self.primary_display_overlay.as_mut() {
            overlay.mouse_selection = Some((selection_position, selection_position));
        }
        Ok(true)
    }

    /// Extends a mouse text selection in the primary command-output overlay.
    fn update_primary_display_overlay_mouse_selection(
        &mut self,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(selection_position) = self.primary_display_overlay_position_for_mouse(position)
        else {
            return Ok(false);
        };
        if let Some(overlay) = self.primary_display_overlay.as_mut() {
            let start = overlay
                .mouse_selection
                .map(|(start, _)| start)
                .unwrap_or(selection_position);
            overlay.mouse_selection = Some((start, selection_position));
        }
        Ok(true)
    }

    /// Finishes a mouse text selection in the primary command-output overlay and copies it.
    fn finish_primary_display_overlay_mouse_selection(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(selection_position) = self.primary_display_overlay_position_for_mouse(position)
        else {
            return Ok(false);
        };
        let copied = if let Some(overlay) = self.primary_display_overlay.as_mut() {
            let start = overlay
                .mouse_selection
                .map(|(start, _)| start)
                .unwrap_or(selection_position);
            overlay.mouse_selection = Some((start, selection_position));
            primary_display_overlay_copy_selection(overlay)
        } else {
            None
        };
        if let Some(copied) = copied.filter(|text| !text.is_empty()) {
            self.copy_text_to_buffer_and_host_clipboard(
                "mouse",
                copied,
                "display-overlay:mouse".to_string(),
            )?;
            return Ok(true);
        }
        self.apply_primary_display_overlay_selection(primary_client_id, position)
    }

    /// Converts one terminal mouse cell to overlay-content coordinates.
    fn primary_display_overlay_position_for_mouse(
        &self,
        position: CopyPosition,
    ) -> Option<CopyPosition> {
        let overlay = self.primary_display_overlay.as_ref()?;
        let line = position.line.checked_sub(1)?;
        let line = overlay.scroll_offset.saturating_add(line);
        let text = overlay.lines.get(line)?;
        let prefix_columns = runtime_display_overlay_line_prefix_columns(overlay, line);
        let column = position.column.saturating_sub(prefix_columns);
        let column = column.min(terminal_text_width(text));
        Some(CopyPosition { line, column })
    }
}

/// Copies the currently selected primary display-overlay text.
fn primary_display_overlay_copy_selection(overlay: &RuntimeDisplayOverlay) -> Option<String> {
    let (start, end) = overlay.mouse_selection?;
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if start.line == end.line {
        return overlay
            .lines
            .get(start.line)
            .map(|line| primary_display_overlay_line_slice(line, start.column, end.column));
    }
    let mut copied = Vec::new();
    let first = overlay.lines.get(start.line)?;
    copied.push(primary_display_overlay_line_slice(
        first,
        start.column,
        terminal_text_width(first),
    ));
    for line_index in start.line.saturating_add(1)..end.line {
        copied.push(overlay.lines.get(line_index)?.clone());
    }
    let last = overlay.lines.get(end.line)?;
    copied.push(primary_display_overlay_line_slice(last, 0, end.column));
    Some(copied.join("\n"))
}

/// Applies a signed scroll delta to a display overlay and clamps the viewport.
fn apply_display_overlay_scroll_delta(
    overlay: &mut RuntimeDisplayOverlay,
    delta: isize,
    size: Size,
) -> bool {
    let previous = overlay.scroll_offset;
    if delta.is_negative() {
        overlay.scroll_offset = overlay.scroll_offset.saturating_sub(delta.unsigned_abs());
    } else {
        overlay.scroll_offset = overlay
            .scroll_offset
            .saturating_add(usize::try_from(delta).unwrap_or(usize::MAX));
    }
    runtime_clamp_display_overlay_scroll(overlay, size);
    runtime_display_overlay_update_active_selection_for_viewport(overlay, size);
    previous != overlay.scroll_offset
}

/// Returns whether one overlay selection is currently visible in the viewport.
fn runtime_display_overlay_selection_index_is_visible(
    overlay: &RuntimeDisplayOverlay,
    selection_index: usize,
    size: Size,
) -> bool {
    let Some(selection) = overlay.selections.get(selection_index) else {
        return false;
    };
    let page_rows = modal_display_overlay_page_rows(size).max(1);
    let visible_start = overlay.scroll_offset;
    let visible_end = visible_start.saturating_add(page_rows);
    selection.line_index >= visible_start && selection.line_index < visible_end
}

/// Keeps the active overlay selection executable only when it is visible.
fn runtime_display_overlay_update_active_selection_for_viewport(
    overlay: &mut RuntimeDisplayOverlay,
    size: Size,
) {
    if overlay.selections.is_empty() {
        overlay.active_selection_index = None;
        return;
    }
    if overlay
        .active_selection_index
        .is_some_and(|selection_index| {
            runtime_display_overlay_selection_index_is_visible(overlay, selection_index, size)
        })
    {
        return;
    }
    let page_rows = modal_display_overlay_page_rows(size).max(1);
    let visible_start = overlay.scroll_offset;
    let visible_end = visible_start.saturating_add(page_rows);
    overlay.active_selection_index = overlay.selections.iter().position(|selection| {
        selection.line_index >= visible_start && selection.line_index < visible_end
    });
}

/// Returns one display-column slice from a primary display-overlay line.
fn primary_display_overlay_line_slice(line: &str, start: usize, end: usize) -> String {
    let mut output = String::new();
    let mut column = 0usize;
    for grapheme in terminal_graphemes(line) {
        let width = terminal_grapheme_width(grapheme);
        let next = column.saturating_add(width);
        if next <= start {
            column = next;
            continue;
        }
        if column >= end || next > end {
            break;
        }
        output.push_str(grapheme);
        column = next;
    }
    output
}

/// Returns the overlay selection index under a mouse position.
fn runtime_display_overlay_selection_index_at_position(
    overlay: &RuntimeDisplayOverlay,
    line_index: usize,
    column: usize,
) -> Option<usize> {
    overlay
        .selections
        .iter()
        .enumerate()
        .filter(|(_, selection)| selection.line_index == line_index)
        .find(|(_, selection)| {
            let start = runtime_display_overlay_rendered_selection_start(overlay, selection);
            let end = start.saturating_add(selection.width);
            column >= start && column < end
        })
        .map(|(index, _)| index)
}

/// Returns the next forward pager-search match, wrapping once to the start.
fn runtime_display_overlay_next_search_match(
    overlay: &RuntimeDisplayOverlay,
    query: &str,
    current_line: usize,
) -> Option<RuntimeDisplayOverlaySearchMatch> {
    if query.is_empty() || overlay.lines.is_empty() {
        return None;
    }
    let start = current_line.saturating_add(1).min(overlay.lines.len());
    overlay.lines[start..]
        .iter()
        .enumerate()
        .find_map(|(index, line)| {
            runtime_display_overlay_search_match_on_line(line, query, start.saturating_add(index))
        })
        .or_else(|| {
            overlay.lines[..start]
                .iter()
                .enumerate()
                .find_map(|(index, line)| {
                    runtime_display_overlay_search_match_on_line(line, query, index)
                })
        })
}

/// Returns the render-cell range for a query match on one pager line.
fn runtime_display_overlay_search_match_on_line(
    line: &str,
    query: &str,
    line_index: usize,
) -> Option<RuntimeDisplayOverlaySearchMatch> {
    let byte_start = line.find(query)?;
    let byte_end = byte_start.saturating_add(query.len());
    Some(RuntimeDisplayOverlaySearchMatch {
        line_index,
        start_column: UnicodeWidthStr::width(&line[..byte_start]),
        width: UnicodeWidthStr::width(&line[byte_start..byte_end]),
    })
}

impl RuntimeSessionService {
    /// Executes one command selected from the primary display overlay.
    fn execute_primary_display_overlay_selection_command(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        command: &str,
    ) -> Result<bool> {
        self.primary_display_overlay = None;
        if command.trim_start().starts_with('/') {
            let pane_id = self.active_pane_id()?.to_string();
            let body = self.execute_agent_shell_command(primary_client_id, command)?;
            let display_output = runtime_agent_shell_display_output(&body, &self.ui_theme)?;
            self.set_agent_prompt_display_output(&pane_id, display_output)?;
            if runtime_agent_shell_visibility(&body).as_deref() == Some("hidden") {
                self.agent_prompt_inputs.remove(&pane_id);
            }
            return Ok(true);
        }
        let content = self
            .execute_terminal_command(primary_client_id, command)
            .and_then(|body| runtime_command_display_overlay_content(&body))?;
        if runtime_command_display_should_open_overlay(&content) {
            self.show_primary_display_overlay_inner(
                content.lines,
                content.line_style_spans,
                content.selections,
                false,
            )?;
        }
        Ok(true)
    }

    /// Applies mouse-wheel scrolling to the primary display overlay.
    fn apply_primary_display_overlay_scroll(&mut self, lines: isize) -> Result<bool> {
        let Some(overlay) = self.primary_display_overlay.as_mut() else {
            return Ok(false);
        };
        Ok(apply_display_overlay_scroll_delta(
            overlay,
            lines,
            self.session.authoritative_size,
        ))
    }

    /// Runs the apply primary display overlay input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_primary_display_overlay_input(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        input: &[u8],
    ) -> Result<bool> {
        let Some(overlay) = self.primary_display_overlay.as_ref() else {
            return Ok(false);
        };
        if overlay.dismiss_on_any_input && !input.is_empty() {
            self.primary_display_overlay = None;
            return Ok(true);
        }
        if overlay.search_input.is_some() {
            return self.apply_primary_display_overlay_search_input(input);
        }
        match runtime_display_overlay_input_action(input) {
            RuntimeDisplayOverlayInputAction::Exit => {
                self.primary_display_overlay = None;
                Ok(true)
            }
            RuntimeDisplayOverlayInputAction::StartSearch => {
                let Some(overlay) = self.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                overlay.search_input = Some(String::new());
                overlay.search_status = None;
                Ok(true)
            }
            RuntimeDisplayOverlayInputAction::EditSearchText
            | RuntimeDisplayOverlayInputAction::EditSearchBackspace => Ok(false),
            RuntimeDisplayOverlayInputAction::SelectActive => {
                let size = self.session.authoritative_size;
                let command = self
                    .primary_display_overlay
                    .as_ref()
                    .and_then(|overlay| {
                        let index = overlay.active_selection_index?;
                        runtime_display_overlay_selection_index_is_visible(overlay, index, size)
                            .then(|| overlay.selections.get(index))
                            .flatten()
                    })
                    .map(|selection| selection.command.clone());
                if let Some(command) = command {
                    self.execute_primary_display_overlay_selection_command(
                        primary_client_id,
                        &command,
                    )
                } else {
                    Ok(false)
                }
            }
            RuntimeDisplayOverlayInputAction::SelectPrevious => {
                self.move_primary_display_overlay_selection(-1)
            }
            RuntimeDisplayOverlayInputAction::SelectNext => {
                self.move_primary_display_overlay_selection(1)
            }
            RuntimeDisplayOverlayInputAction::SelectFirst => {
                self.set_primary_display_overlay_selection_index(0)
            }
            RuntimeDisplayOverlayInputAction::SelectLast => {
                let Some(overlay) = self.primary_display_overlay.as_ref() else {
                    return Ok(false);
                };
                self.set_primary_display_overlay_selection_index(
                    overlay.selections.len().saturating_sub(1),
                )
            }
            RuntimeDisplayOverlayInputAction::ScrollBy(delta) if delta < 0 => {
                let Some(overlay) = self.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                Ok(apply_display_overlay_scroll_delta(
                    overlay,
                    delta,
                    self.session.authoritative_size,
                ))
            }
            RuntimeDisplayOverlayInputAction::ScrollBy(delta) => {
                let Some(overlay) = self.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                Ok(apply_display_overlay_scroll_delta(
                    overlay,
                    delta,
                    self.session.authoritative_size,
                ))
            }
            RuntimeDisplayOverlayInputAction::Ignore => Ok(false),
        }
    }

    /// Applies one input chunk while the command-output pager search prompt is active.
    fn apply_primary_display_overlay_search_input(&mut self, input: &[u8]) -> Result<bool> {
        match runtime_display_overlay_input_action(input) {
            RuntimeDisplayOverlayInputAction::Exit => {
                let Some(overlay) = self.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                overlay.search_input = None;
                overlay.search_status = None;
                Ok(true)
            }
            RuntimeDisplayOverlayInputAction::SelectActive => {
                self.submit_primary_display_overlay_search()
            }
            RuntimeDisplayOverlayInputAction::EditSearchBackspace => {
                let Some(overlay) = self.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                let Some(search_input) = overlay.search_input.as_mut() else {
                    return Ok(false);
                };
                let changed = search_input.pop().is_some();
                Ok(changed)
            }
            RuntimeDisplayOverlayInputAction::EditSearchText => {
                let Ok(text) = std::str::from_utf8(input) else {
                    return Ok(false);
                };
                let Some(overlay) = self.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                let Some(search_input) = overlay.search_input.as_mut() else {
                    return Ok(false);
                };
                search_input.push_str(text);
                Ok(!text.is_empty())
            }
            RuntimeDisplayOverlayInputAction::StartSearch
            | RuntimeDisplayOverlayInputAction::SelectPrevious
            | RuntimeDisplayOverlayInputAction::SelectNext
            | RuntimeDisplayOverlayInputAction::SelectFirst
            | RuntimeDisplayOverlayInputAction::SelectLast
            | RuntimeDisplayOverlayInputAction::ScrollBy(_)
            | RuntimeDisplayOverlayInputAction::Ignore => Ok(false),
        }
    }

    /// Submits the active command-output pager search query.
    fn submit_primary_display_overlay_search(&mut self) -> Result<bool> {
        let Some(overlay) = self.primary_display_overlay.as_mut() else {
            return Ok(false);
        };
        let submitted = overlay.search_input.take().unwrap_or_default();
        let query = if submitted.is_empty() {
            let Some(query) = overlay.search_query.clone() else {
                overlay.search_status = Some("search: enter a query".to_string());
                return Ok(true);
            };
            query
        } else {
            overlay.search_query = Some(submitted.clone());
            submitted
        };
        let start_line = overlay
            .search_match
            .map(|search_match| search_match.line_index)
            .or_else(|| overlay.scroll_offset.checked_sub(1))
            .unwrap_or(overlay.scroll_offset);
        let Some(search_match) =
            runtime_display_overlay_next_search_match(overlay, &query, start_line)
        else {
            overlay.search_status = Some(format!("pattern not found: {query}"));
            return Ok(true);
        };
        overlay.search_match = Some(search_match);
        overlay.scroll_offset = search_match.line_index;
        runtime_clamp_display_overlay_scroll(overlay, self.session.authoritative_size);
        overlay.search_status = None;
        Ok(true)
    }

    /// Moves the active command overlay selection and keeps it visible.
    fn move_primary_display_overlay_selection(&mut self, delta: isize) -> Result<bool> {
        let Some(overlay) = self.primary_display_overlay.as_mut() else {
            return Ok(false);
        };
        if overlay.selections.is_empty() {
            return Ok(apply_display_overlay_scroll_delta(
                overlay,
                delta,
                self.session.authoritative_size,
            ));
        }
        let previous = overlay.active_selection_index.unwrap_or(0);
        let next = runtime_selector_step_index(previous, overlay.selections.len(), delta);
        overlay.active_selection_index = Some(next);
        if let Some(line_index) = overlay
            .selections
            .get(next)
            .map(|selection| selection.line_index)
        {
            runtime_scroll_display_overlay_to_line(
                overlay,
                line_index,
                self.session.authoritative_size,
            );
        }
        Ok(next != previous)
    }

    /// Sets the active command overlay selection and keeps it visible.
    fn set_primary_display_overlay_selection_index(&mut self, index: usize) -> Result<bool> {
        let Some(overlay) = self.primary_display_overlay.as_mut() else {
            return Ok(false);
        };
        if overlay.selections.is_empty() {
            let next = if index == 0 {
                0
            } else {
                modal_display_overlay_max_scroll(&overlay.lines, self.session.authoritative_size)
            };
            let changed = next != overlay.scroll_offset;
            overlay.scroll_offset = next;
            return Ok(changed);
        }
        let previous = overlay.active_selection_index.unwrap_or(0);
        let next = index.min(overlay.selections.len().saturating_sub(1));
        overlay.active_selection_index = Some(next);
        if let Some(line_index) = overlay
            .selections
            .get(next)
            .map(|selection| selection.line_index)
        {
            runtime_scroll_display_overlay_to_line(
                overlay,
                line_index,
                self.session.authoritative_size,
            );
        }
        Ok(next != previous)
    }

    /// Runs the apply primary prompt terminal action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_primary_prompt_terminal_action(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        action: &TerminalClientLoopAction,
    ) -> Result<bool> {
        match action {
            TerminalClientLoopAction::ForwardToPane(input) => {
                self.apply_primary_prompt_input(primary_client_id, input)
            }
            TerminalClientLoopAction::ForwardMouseToPane { .. }
            | TerminalClientLoopAction::ExecuteMux(_)
            | TerminalClientLoopAction::ExecuteCommand(_)
            | TerminalClientLoopAction::HandleMouse(_)
            | TerminalClientLoopAction::HandleCopyMode(_)
            | TerminalClientLoopAction::EnterPrefixKeyMode
            | TerminalClientLoopAction::ReportUnboundPrefix(_) => Ok(false),
        }
    }

    /// Runs the apply primary prompt input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_primary_prompt_input(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        input: &[u8],
    ) -> Result<bool> {
        if input == b"\x1b" {
            if self
                .primary_prompt_input
                .as_ref()
                .is_some_and(|prompt_input| prompt_input.prompt.reverse_search_active())
            {
                // Let the prompt consume Escape to cancel incremental search.
            } else {
                if self.primary_prompt_input.take().is_some() {
                    return Ok(true);
                }
                return Ok(false);
            }
        }
        if input == b"\x0c" {
            if self.primary_prompt_input.is_some() {
                let pane_id = self.active_pane_id()?;
                self.clear_agent_shell_terminal_view(&pane_id)?;
                return Ok(true);
            }
            return Ok(false);
        }
        let selector_extra_candidates = self.runtime_command_selector_extra_candidates();
        let Some(prompt_input) = self.primary_prompt_input.as_mut() else {
            return Ok(false);
        };
        if prompt_input.prompt.kind == ReadlinePromptKind::Command {
            prompt_input
                .prompt
                .set_selector_extra_candidates(selector_extra_candidates);
        }
        let outcomes = if input == b"\x1b" && prompt_input.prompt.reverse_search_active() {
            vec![prompt_input.prompt.apply_terminal_input(input)?]
        } else {
            prompt_input
                .decoder
                .apply_to_prompt(&mut prompt_input.prompt, input)?
        };
        let mut changed = false;
        for outcome in outcomes {
            match outcome {
                ReadlineOutcome::Submitted(command)
                | ReadlineOutcome::SubmittedWithDisplay { text: command, .. } => {
                    let prompt_kind = prompt_input.prompt.kind;
                    self.primary_prompt_input = None;
                    changed = true;
                    if !command.trim().is_empty() {
                        if prompt_kind == ReadlinePromptKind::Command {
                            self.remember_primary_command_prompt_submission(&command)?;
                        }
                        match self
                            .execute_terminal_command(primary_client_id, &command)
                            .and_then(|body| runtime_command_display_overlay_content(&body))
                        {
                            Ok(content)
                                if runtime_command_display_should_open_overlay(&content) =>
                            {
                                self.show_primary_display_overlay_inner(
                                    content.lines,
                                    content.line_style_spans,
                                    content.selections,
                                    false,
                                )?;
                            }
                            Ok(content) => {
                                self.append_runtime_command_display_lines_to_active_pane(
                                    &content.lines,
                                )?;
                            }
                            Err(error) => {
                                self.show_primary_display_overlay(vec![format!(
                                    "error: {error} - press Esc to return"
                                )])?;
                            }
                        }
                    }
                    return Ok(changed);
                }
                ReadlineOutcome::Cancelled | ReadlineOutcome::Eof => {
                    self.primary_prompt_input = None;
                    return Ok(true);
                }
                ReadlineOutcome::Edited => changed = true,
                ReadlineOutcome::Noop => {}
            }
        }
        Ok(changed)
    }

    /// Retains one submitted `Ctrl+A :` command for future readline history
    /// navigation and reverse search.
    fn remember_primary_command_prompt_submission(&mut self, command: &str) -> Result<()> {
        if command.trim().is_empty() {
            return Ok(());
        }
        self.primary_command_prompt_history
            .push(command.to_string());
        while self.primary_command_prompt_history.len() > DEFAULT_READLINE_HISTORY_LIMIT {
            self.primary_command_prompt_history.remove(0);
        }
        let Some(store) = self.agent_transcript_store.clone() else {
            return Ok(());
        };
        if self.defer_agent_transcript_writes {
            self.deferred_command_prompt_history_writes
                .push(DeferredCommandPromptHistoryWrite {
                    path: store.command_prompt_history_file(),
                    store,
                    command: command.to_string(),
                });
            return Ok(());
        }
        let _ = store.append_command_prompt_history(command)?;
        Ok(())
    }

    /// Reloads persisted primary command prompt history into the live prompt
    /// cache.
    fn reload_primary_command_prompt_history(&mut self) -> Result<()> {
        let Some(store) = self.agent_transcript_store.as_ref() else {
            return Ok(());
        };
        self.primary_command_prompt_history = store.command_prompt_history()?;
        Ok(())
    }

    /// Runs the apply attached agent prompt input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_attached_agent_prompt_input(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        input: &[u8],
    ) -> Result<bool> {
        if input.is_empty() {
            return Ok(false);
        }
        let pane_id = self.active_pane_id()?;
        self.apply_attached_agent_prompt_input_for_pane(primary_client_id, &pane_id, input)
    }

    /// Applies attached agent prompt input to an explicit pane.
    ///
    /// This is used by the ordinary focused-pane input path and by mouse
    /// paste routing, where the click can intentionally target a different
    /// pane-local prompt before bytes are decoded by readline.
    fn apply_attached_agent_prompt_input_for_pane(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        pane_id: &str,
        input: &[u8],
    ) -> Result<bool> {
        if input.is_empty() {
            return Ok(false);
        }
        if input == b"\x1b" {
            self.clear_agent_prompt_pending_ctrl_c_exit(pane_id);
        }
        if input == b"\x0c" {
            self.clear_agent_prompt_pending_ctrl_c_exit(pane_id);
            self.clear_agent_shell_terminal_view(pane_id)?;
            return Ok(true);
        }
        if input != b"\x03" {
            self.clear_agent_prompt_pending_ctrl_c_exit(pane_id);
        }
        let selector_extra_candidates = self.runtime_agent_selector_extra_candidates();
        let prompt_body_columns = self
            .agent_prompt_editable_body_width(pane_id)
            .unwrap_or(1)
            .max(1);

        let outcomes = {
            let state = self
                .agent_prompt_inputs
                .entry(pane_id.to_string())
                .or_insert_with(default_runtime_agent_prompt_input);
            state.prompt.set_prompt_body_columns(prompt_body_columns);
            state
                .prompt
                .set_selector_extra_candidates(selector_extra_candidates);
            if input == b"\x1b" {
                vec![state.prompt.apply_terminal_input(input)?]
            } else {
                state.decoder.apply_to_prompt(&mut state.prompt, input)?
            }
        };

        let mut changed = false;
        for outcome in outcomes {
            match outcome {
                ReadlineOutcome::Submitted(command) => {
                    changed = true;
                    if command.trim().is_empty() {
                        continue;
                    }
                    let body = match self.execute_agent_shell_command(primary_client_id, &command) {
                        Ok(body) => body,
                        Err(error) => {
                            self.set_agent_prompt_display_lines(
                                pane_id,
                                agent_prompt_error_display_lines(&error),
                            )?;
                            continue;
                        }
                    };
                    match runtime_agent_shell_display_output(&body, &self.ui_theme) {
                        Ok(display_output) => {
                            self.set_agent_prompt_display_output(pane_id, display_output)?;
                        }
                        Err(error) => {
                            self.set_agent_prompt_display_lines(
                                pane_id,
                                agent_prompt_error_display_lines(&error),
                            )?;
                        }
                    }
                    if runtime_agent_shell_visibility(&body).as_deref() == Some("hidden") {
                        self.agent_prompt_inputs.remove(pane_id);
                    }
                }
                ReadlineOutcome::SubmittedWithDisplay { text, display } => {
                    changed = true;
                    if text.trim().is_empty() {
                        continue;
                    }
                    let body = match self.execute_agent_shell_command_with_display(
                        primary_client_id,
                        &text,
                        &display,
                    ) {
                        Ok(body) => body,
                        Err(error) => {
                            self.set_agent_prompt_display_lines(
                                pane_id,
                                agent_prompt_error_display_lines(&error),
                            )?;
                            continue;
                        }
                    };
                    match runtime_agent_shell_display_output(&body, &self.ui_theme) {
                        Ok(display_output) => {
                            self.set_agent_prompt_display_output(pane_id, display_output)?;
                        }
                        Err(error) => {
                            self.set_agent_prompt_display_lines(
                                pane_id,
                                agent_prompt_error_display_lines(&error),
                            )?;
                        }
                    }
                    if runtime_agent_shell_visibility(&body).as_deref() == Some("hidden") {
                        self.agent_prompt_inputs.remove(pane_id);
                    }
                }
                ReadlineOutcome::Cancelled => {
                    changed = self.apply_agent_prompt_ctrl_c_interrupt_or_confirm_exit(
                        primary_client_id,
                        pane_id,
                    )?;
                }
                ReadlineOutcome::Eof => {
                    changed = true;
                    let _ = self.execute_agent_shell_command(primary_client_id, "/exit")?;
                    self.agent_prompt_inputs.remove(pane_id);
                }
                ReadlineOutcome::Edited => changed = true,
                ReadlineOutcome::Noop => {}
            }
        }
        Ok(changed)
    }

    /// Clears any pending idle Ctrl+C exit confirmation for one agent prompt.
    fn clear_agent_prompt_pending_ctrl_c_exit(&mut self, pane_id: &str) {
        if let Some(state) = self.agent_prompt_inputs.get_mut(pane_id) {
            state.pending_ctrl_c_exit_at_unix_ms = None;
        }
    }

    /// Applies the interrupt/exit contract for pane-local agent prompts.
    ///
    /// Ctrl+C confirmation and EOF exits share this helper so active work is
    /// stopped consistently before the pane-local prompt is hidden.
    fn apply_agent_prompt_interrupt_or_exit(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        pane_id: &str,
    ) -> Result<bool> {
        let command = if self.agent_shell_pane_has_active_turn(pane_id) {
            "/stop"
        } else {
            "/exit"
        };
        let body = self.execute_agent_shell_command(primary_client_id, command)?;
        match runtime_agent_shell_display_output(&body, &self.ui_theme) {
            Ok(display_output) => self.set_agent_prompt_display_output(pane_id, display_output)?,
            Err(error) => self.set_agent_prompt_display_lines(
                pane_id,
                agent_prompt_error_display_lines(&error),
            )?,
        }
        if runtime_agent_shell_visibility(&body).as_deref() == Some("hidden") {
            self.agent_prompt_inputs.remove(pane_id);
        }
        Ok(true)
    }

    /// Applies the Ctrl+C interrupt or double-confirm idle exit contract.
    fn apply_agent_prompt_ctrl_c_interrupt_or_confirm_exit(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        pane_id: &str,
    ) -> Result<bool> {
        const CTRL_C_EXIT_CONFIRM_WINDOW_MS: u64 = 3_000;
        if self.agent_shell_pane_has_active_turn(pane_id) {
            self.clear_agent_prompt_pending_ctrl_c_exit(pane_id);
            return self.apply_agent_prompt_interrupt_or_exit(primary_client_id, pane_id);
        }

        if let Some(state) = self.agent_prompt_inputs.get_mut(pane_id)
            && !state.prompt.buffer.line().is_empty()
        {
            state.prompt.buffer.set_line("");
            state.pending_ctrl_c_exit_at_unix_ms = None;
            state.display_lines.clear();
            return Ok(true);
        }

        let now = current_unix_millis();
        let confirmed = {
            let state = self
                .agent_prompt_inputs
                .entry(pane_id.to_string())
                .or_insert_with(default_runtime_agent_prompt_input);
            state
                .pending_ctrl_c_exit_at_unix_ms
                .is_some_and(|started| now.saturating_sub(started) <= CTRL_C_EXIT_CONFIRM_WINDOW_MS)
        };
        if confirmed {
            self.clear_agent_prompt_pending_ctrl_c_exit(pane_id);
            return self.apply_agent_prompt_interrupt_or_exit(primary_client_id, pane_id);
        }

        if let Some(state) = self.agent_prompt_inputs.get_mut(pane_id) {
            state.pending_ctrl_c_exit_at_unix_ms = Some(now);
        }
        self.set_agent_prompt_display_lines(
            pane_id,
            vec!["press ctrl-c again within 3s to exit agent mode".to_string()],
        )?;
        Ok(true)
    }

    /// Reports whether a pane-local agent shell currently owns interruptible work.
    fn agent_shell_pane_has_active_turn(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .is_some()
            || self.agent_turn_ledger.turns().iter().any(|turn| {
                turn.pane_id == pane_id
                    && matches!(
                        turn.state,
                        AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
                    )
            })
    }

    /// Builds dynamic primary command prompt selector candidates.
    fn runtime_command_selector_extra_candidates(&self) -> Vec<SelectorExtraCandidate> {
        self.mcp_registry
            .list_servers()
            .into_iter()
            .flat_map(|server| {
                let candidate = SelectorCandidate::new(
                    server.configured.id.clone(),
                    SelectorCandidateKind::Value,
                    true,
                )
                .with_detail(agent_shell_mcp_display_state_name(
                    server.configured.enabled,
                    server.status,
                ));
                [
                    SelectorExtraCandidate::new(
                        SelectorSurface::MezzanineCommand,
                        "mcp-remove",
                        candidate.clone(),
                    ),
                    SelectorExtraCandidate::new(
                        SelectorSurface::MezzanineCommand,
                        "mcp-retry",
                        candidate,
                    ),
                ]
            })
            .collect()
    }

    /// Builds dynamic agent prompt selector candidates from saved transcripts.
    fn runtime_agent_selector_extra_candidates(&self) -> Vec<SelectorExtraCandidate> {
        let mut candidates = self
            .agent_personality_profiles
            .iter()
            .map(|(profile_id, profile)| {
                SelectorExtraCandidate::new(
                    SelectorSurface::AgentCommand,
                    "personality",
                    SelectorCandidate::new(profile_id.clone(), SelectorCandidateKind::Value, true)
                        .with_detail(
                            profile
                                .name
                                .clone()
                                .unwrap_or_else(|| "personality profile".to_string()),
                        ),
                )
            })
            .collect::<Vec<_>>();
        candidates.extend(self.mcp_registry.list_servers().into_iter().map(|server| {
            SelectorExtraCandidate::new(
                SelectorSurface::AgentCommand,
                "list-mcp",
                SelectorCandidate::new(
                    server.configured.id.clone(),
                    SelectorCandidateKind::Value,
                    true,
                )
                .with_detail(agent_shell_mcp_display_state_name(
                    server.configured.enabled,
                    server.status,
                )),
            )
        }));
        if let Ok(pane_id) = self.active_pane_id() {
            let catalog = self.effective_skill_catalog_for_pane(&pane_id);
            candidates.extend(catalog.skills.into_iter().map(|skill| {
                SelectorExtraCandidate::new(
                    SelectorSurface::AgentCommand,
                    "$",
                    SelectorCandidate::new(
                        format!("${}", skill.name),
                        SelectorCandidateKind::Value,
                        true,
                    )
                    .with_detail(format!(
                        "{} ({})",
                        skill.description,
                        skill.source.as_str()
                    )),
                )
            }));
        }
        let Some(store) = self.agent_transcript_store.as_ref() else {
            return candidates;
        };
        candidates.extend(store.list().unwrap_or_default().into_iter().map(|summary| {
            SelectorExtraCandidate::new(
                SelectorSurface::AgentCommand,
                "resume",
                SelectorCandidate::new(
                    summary.conversation_id.clone(),
                    SelectorCandidateKind::Value,
                    true,
                )
                .with_detail(format!(
                    "{} entries, pane {}, agent {}",
                    summary.entries, summary.pane_id, summary.agent_id
                )),
            )
        }));
        candidates
    }

    /// Runs the reload agent prompt history for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn reload_agent_prompt_history_for_pane(&mut self, pane_id: &str) -> Result<()> {
        let Some(session_id) = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.session_id.clone())
        else {
            return Ok(());
        };
        let history = match self.agent_transcript_store.as_ref() {
            Some(store) => store.prompt_history(&session_id)?,
            None => Vec::new(),
        };
        self.agent_prompt_inputs
            .entry(pane_id.to_string())
            .or_insert_with(default_runtime_agent_prompt_input)
            .prompt
            .buffer
            .set_history(history);
        Ok(())
    }

    /// Runs the set agent prompt display lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_agent_prompt_display_lines(
        &mut self,
        pane_id: &str,
        display_lines: Vec<String>,
    ) -> Result<()> {
        let style = if agent_display_lines_are_error(&display_lines) {
            AgentTerminalPresentationStyle::Error
        } else {
            AgentTerminalPresentationStyle::Assistant
        };
        if style == AgentTerminalPresentationStyle::Error
            || self.agent_verbose_enabled(pane_id)
            || !agent_display_lines_are_low_level_status(&display_lines)
        {
            self.append_agent_terminal_lines_to_buffer(pane_id, &display_lines, style)?;
        }
        let state = self
            .agent_prompt_inputs
            .entry(pane_id.to_string())
            .or_insert_with(default_runtime_agent_prompt_input);
        state.display_lines.clear();
        Ok(())
    }

    /// Appends agent shell display output using the declared content renderer.
    fn set_agent_prompt_display_output(
        &mut self,
        pane_id: &str,
        display_output: RuntimeAgentShellDisplayOutput,
    ) -> Result<()> {
        match display_output {
            RuntimeAgentShellDisplayOutput::Lines(display_lines) => {
                self.set_agent_prompt_display_lines(pane_id, display_lines)?;
            }
            RuntimeAgentShellDisplayOutput::Overlay(content) => {
                if runtime_command_display_should_open_overlay(&content) {
                    self.show_primary_display_overlay_inner(
                        content.lines,
                        content.line_style_spans,
                        content.selections,
                        false,
                    )?;
                } else {
                    self.set_agent_prompt_display_lines(pane_id, content.lines)?;
                }
                let state = self
                    .agent_prompt_inputs
                    .entry(pane_id.to_string())
                    .or_insert_with(default_runtime_agent_prompt_input);
                state.display_lines.clear();
            }
        }
        Ok(())
    }

    /// Runs the append agent user prompt to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_user_prompt_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        prompt: &str,
    ) -> Result<()> {
        let lines = prefixed_agent_terminal_lines("user> ", prompt);
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &lines,
            AgentTerminalPresentationStyle::UserPrompt,
        )
    }

    /// Appends the parent-supplied prompt at the top of a spawned subagent pane.
    ///
    /// Subagent pane logs should expose the exact parent instruction that
    /// started the child turn so follow-up inspection does not require looking
    /// back through the parent pane.
    pub(super) fn append_agent_parent_prompt_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        prompt: &str,
    ) -> Result<()> {
        let lines = prefixed_agent_terminal_lines("parent> ", prompt);
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &lines,
            AgentTerminalPresentationStyle::UserPrompt,
        )
    }

    /// Runs the append agent assistant text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_assistant_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        self.append_agent_assistant_content_to_terminal_buffer(
            pane_id,
            text,
            AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
        )
    }

    /// Appends assistant output using its declared presentation media type.
    pub(super) fn append_agent_assistant_content_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
        content_type: &str,
    ) -> Result<()> {
        if agent_output_content_type_is_markdown(content_type)
            && !agent_say_text_is_displayed_patch_block(text)
        {
            return self.append_agent_assistant_markdown_to_terminal_buffer(pane_id, text);
        }
        if agent_output_content_type_is_diff(content_type) {
            return self.append_agent_diff_text_to_terminal_buffer(pane_id, text);
        }
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines = wrapped_prefixed_agent_terminal_lines("mez> ", text, display_width);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Assistant,
            rendered_lines.as_slice(),
            &[],
        )
    }

    /// Returns the display cells available after the agent transcript gutter.
    fn agent_terminal_markdown_frame_width(&self, pane_id: &str) -> Result<usize> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        Ok(bounded_agent_terminal_presentation_columns(columns)
            .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
            .max(1))
    }

    /// Returns display cells available after the agent transcript gutter.
    fn agent_terminal_markdown_terminal_width(&self, pane_id: &str) -> Result<usize> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        Ok(columns
            .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
            .max(1))
    }

    /// Returns display cells available for editable pane-local prompt text.
    ///
    /// This width mirrors the terminal renderer, which draws the editable text
    /// after both the agent transcript gutter and the `agent>` prompt marker.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current presentation width bounds the prompt.
    fn agent_prompt_editable_body_width(&self, pane_id: &str) -> Result<usize> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        let prompt_prefix_width = UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX)
            .saturating_add(UnicodeWidthStr::width(AGENT_PROMPT_TEXT_PREFIX));
        Ok(columns.saturating_sub(prompt_prefix_width).max(1))
    }

    /// Returns the current pane presentation width in terminal display cells.
    fn agent_terminal_presentation_columns(&self, pane_id: &str) -> Result<usize> {
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if let Some(columns) = self.agent_terminal_render_region_columns(pane_id) {
            return Ok(columns);
        }
        let columns = self
            .pane_screens
            .get(pane_id)
            .map(|screen| screen.size().columns)
            .unwrap_or(descriptor.size.columns);
        Ok(usize::from(columns))
    }

    /// Returns the pane-local render width used by the terminal compositor.
    fn agent_terminal_render_region_columns(&self, pane_id: &str) -> Option<usize> {
        let window = self.session.active_window()?;
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == pane_id)?;
        let body_size = rendered_window_body_size(window.size, self.window_frames_enabled).ok()?;
        let geometries = if window.zoomed_pane_id() == Some(&pane.id) {
            vec![PaneGeometry {
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
            .find(|geometry| geometry.index == pane.index)?;
        pane_content_size_for_geometry(
            geometry,
            &geometries,
            self.pane_frames_enabled,
            self.pane_frame_position,
        )
        .ok()
        .map(|size| usize::from(size.columns))
    }

    /// Returns the pane width to persist with one agent presentation entry.
    fn agent_presentation_terminal_width(&self, pane_id: &str) -> Option<u16> {
        self.pane_screens
            .get(pane_id)
            .map(|screen| screen.size().columns)
            .or_else(|| {
                self.find_pane_descriptor(pane_id)
                    .map(|descriptor| descriptor.size.columns)
            })
    }

    /// Persists one durable user-visible agent presentation entry.
    fn persist_agent_presentation_entry(
        &self,
        pane_id: &str,
        style_names: Vec<String>,
        display_lines: Vec<String>,
        copy_lines: Vec<String>,
        ansi_text: String,
    ) {
        if self.agent_presentation_replay_panes.contains(pane_id)
            || display_lines.is_empty()
            || style_names.len() != display_lines.len()
        {
            return;
        }
        let Some(store) = self.agent_transcript_store.as_ref() else {
            return;
        };
        let Some(session) = self.agent_shell_store.get(pane_id) else {
            return;
        };
        let Some(terminal_width) = self.agent_presentation_terminal_width(pane_id) else {
            return;
        };
        let Ok(sequence) = store.next_presentation_sequence(&session.session_id) else {
            return;
        };
        let entry = AgentPresentationEntry {
            conversation_id: session.session_id.clone(),
            sequence,
            created_at_unix_seconds: current_unix_seconds().max(1),
            pane_id: pane_id.to_string(),
            turn_id: session.running_turn_id.clone(),
            terminal_width,
            style_names,
            display_lines,
            copy_lines,
            ansi_text: (!ansi_text.is_empty()).then_some(ansi_text),
        };
        let _ = store.append_presentation(&entry);
    }

    /// Replays persisted presentation entries into the pane terminal buffer.
    pub(super) fn replay_agent_presentation_entries_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        entries: &[AgentPresentationEntry],
    ) -> Result<bool> {
        if entries.is_empty() {
            return Ok(false);
        }
        self.agent_presentation_replay_panes
            .insert(pane_id.to_string());
        let result = (|| -> Result<bool> {
            let mut sorted_entries = entries.iter().collect::<Vec<_>>();
            sorted_entries.sort_by_key(|entry| entry.sequence);
            for entry in sorted_entries {
                if let Some(ansi_text) = entry.ansi_text.as_deref() {
                    let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
                        MezError::new(
                            crate::error::MezErrorKind::NotFound,
                            "agent terminal presentation target pane not found",
                        )
                    })?;
                    if !self.pane_screens.contains_key(pane_id) {
                        let screen = TerminalScreen::new_with_history_config(
                            descriptor.size,
                            self.terminal_history_limit,
                            self.terminal_history_rotate_lines,
                        )?;
                        self.pane_screens.insert(pane_id.to_string(), screen);
                    }
                    self.clear_agent_shell_output_status_line(pane_id)?;
                    let screen = self.pane_screens.get_mut(pane_id).ok_or_else(|| {
                        MezError::invalid_state(
                            "agent terminal presentation screen was not initialized",
                        )
                    })?;
                    Self::feed_agent_terminal_screen(
                        screen,
                        ansi_text.as_bytes(),
                        "replaying persisted agent presentation",
                    )?;
                    if !entry.copy_lines.is_empty() {
                        screen.set_recent_normal_copy_texts(&entry.copy_lines);
                    }
                    continue;
                }
                let styled_lines = entry
                    .display_lines
                    .iter()
                    .enumerate()
                    .map(|(index, line)| {
                        let style = entry
                            .style_names
                            .get(index)
                            .and_then(|name| {
                                AgentTerminalPresentationStyle::from_persistence_name(name)
                            })
                            .unwrap_or(AgentTerminalPresentationStyle::Status);
                        (style, line.clone())
                    })
                    .collect::<Vec<_>>();
                self.append_agent_terminal_styled_lines_to_buffer(pane_id, &styled_lines)?;
                if !entry.copy_lines.is_empty()
                    && let Some(screen) = self.pane_screens.get_mut(pane_id)
                {
                    screen.set_recent_normal_copy_texts(&entry.copy_lines);
                }
            }
            let state = self
                .agent_prompt_inputs
                .entry(pane_id.to_string())
                .or_insert_with(default_runtime_agent_prompt_input);
            state.display_lines.clear();
            Ok(true)
        })();
        self.agent_presentation_replay_panes.remove(pane_id);
        result
    }

    /// Appends markdown assistant output as styled presentation lines.
    fn append_agent_assistant_markdown_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        markdown: &str,
    ) -> Result<()> {
        let frame_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let table_width = self.agent_terminal_markdown_terminal_width(pane_id)?;
        let body_rendered_lines = wrap_agent_rendered_lines_to_width(
            render_agent_markdown_body_lines(markdown, &self.ui_theme, table_width),
            frame_width,
            table_width,
        );
        let body_rendered_count = body_rendered_lines.len();
        let rendered_lines = frame_agent_markdown_lines(body_rendered_lines, frame_width);
        let raw_copy_lines = prefixed_agent_terminal_lines("mez> ", markdown)
            .into_iter()
            .map(|line| format!("{AGENT_TERMINAL_MESSAGE_PREFIX}{line}"))
            .collect::<Vec<_>>();
        let copy_lines = markdown_block_copy_lines(
            rendered_lines.as_slice(),
            body_rendered_count,
            raw_copy_lines,
        );
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Assistant,
            rendered_lines.as_slice(),
            &copy_lines,
        )
    }

    /// Runs the append agent status text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_status_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        let lines = text
            .trim_end_matches(['\r', '\n'])
            .lines()
            .map(sanitized_agent_terminal_line)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &lines,
            AgentTerminalPresentationStyle::Status,
        )
    }

    /// Runs the append agent verbose status text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_verbose_status_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        if self.agent_verbose_enabled(pane_id) {
            self.append_agent_status_text_to_terminal_buffer(pane_id, text)?;
        }
        Ok(())
    }

    /// Runs the append agent thinking text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_thinking_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        if self.agent_thinking_enabled(pane_id) {
            let columns = self.agent_terminal_presentation_columns(pane_id)?;
            self.append_agent_terminal_lines_to_buffer(
                pane_id,
                &agent_thinking_display_lines_for_width(text, columns),
                AgentTerminalPresentationStyle::Status,
            )?;
        }
        Ok(())
    }

    /// Runs the append agent error text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_error_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        let lines = text
            .trim_end_matches(['\r', '\n'])
            .lines()
            .map(sanitized_agent_terminal_line)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &lines,
            AgentTerminalPresentationStyle::Error,
        )
    }

    /// Runs the append agent command preview to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_command_preview_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        command: &str,
    ) -> Result<()> {
        /// Defines the MAX AGENT COMMAND PREVIEW LINES const used by this subsystem.
        ///
        /// Keeping this value documented makes the contract explicit at the module
        /// boundary and avoids relying on call-site inference.
        const MAX_AGENT_COMMAND_PREVIEW_LINES: usize = 10;
        let columns = self
            .pane_screens
            .get(pane_id)
            .map(|screen| usize::from(screen.size().columns))
            .or_else(|| {
                self.find_pane_descriptor(pane_id)
                    .map(|descriptor| usize::from(descriptor.size.columns))
            })
            .unwrap_or(80);
        let display_columns = bounded_agent_terminal_presentation_columns(columns);
        let prefix_width =
            UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX) + UnicodeWidthStr::width("$ ");
        let content_columns = display_columns.saturating_sub(prefix_width).max(1);
        let rendered_lines = command_preview_terminal_rendered_lines(
            command,
            content_columns,
            MAX_AGENT_COMMAND_PREVIEW_LINES,
            self.shell_classification_for_pane(pane_id),
            &self.ui_theme,
        );
        let copy_lines = rendered_lines
            .iter()
            .map(|line| line.display.clone())
            .collect::<Vec<_>>();
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Command,
            &rendered_lines,
            &copy_lines,
        )
    }

    /// Runs the append agent terminal lines to buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_terminal_lines_to_buffer(
        &mut self,
        pane_id: &str,
        lines: &[String],
        style: AgentTerminalPresentationStyle,
    ) -> Result<()> {
        let styled_lines = lines
            .iter()
            .map(|line| (style, line.clone()))
            .collect::<Vec<_>>();
        self.append_agent_terminal_styled_lines_to_buffer(pane_id, &styled_lines)
    }

    /// Feeds agent-owned presentation bytes into a terminal screen.
    ///
    /// Agent presentation content is model-authored, so terminal rendering must
    /// contain parser defects to the presentation batch instead of allowing a
    /// panic to cross the runtime state boundary.
    ///
    /// # Parameters
    /// - `screen`: The pane screen receiving rendered bytes.
    /// - `bytes`: The already-sanitized terminal bytes to feed.
    /// - `context`: A short description of the presentation operation.
    fn feed_agent_terminal_screen(
        screen: &mut TerminalScreen,
        bytes: &[u8],
        _context: &str,
    ) -> Result<()> {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| screen.feed(bytes))).is_err() {
            return Ok(());
        }
        Ok(())
    }

    /// Appends agent terminal lines with per-line presentation styles.
    ///
    /// Diff previews need additions, deletions, headers, and context to carry
    /// different colors while still flowing through the same pane-buffer gutter
    /// logic as normal agent transcript entries.
    pub(super) fn append_agent_terminal_styled_lines_to_buffer(
        &mut self,
        pane_id: &str,
        styled_lines: &[(AgentTerminalPresentationStyle, String)],
    ) -> Result<()> {
        if styled_lines.is_empty() {
            return Ok(());
        }
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if !self.pane_screens.contains_key(pane_id) {
            let screen = TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?;
            self.pane_screens.insert(pane_id.to_string(), screen);
        }
        self.clear_agent_shell_output_status_line(pane_id)?;
        let ansi_text = {
            let screen = self.pane_screens.get_mut(pane_id).ok_or_else(|| {
                MezError::invalid_state("agent terminal presentation screen was not initialized")
            })?;
            let mut bytes = String::new();
            let cursor = screen.cursor_state();
            let current_line_has_content = screen
                .visible_lines()
                .get(cursor.row)
                .is_some_and(|line| !line.trim().is_empty());
            if cursor.column == 0 && !current_line_has_content {
                bytes.push('\r');
            } else {
                bytes.push_str("\r\n");
            }
            for (style, line) in styled_lines {
                append_styled_agent_terminal_line(&mut bytes, *style, line, &self.ui_theme);
                bytes.push_str("\x1b[0m\r\n");
            }
            Self::feed_agent_terminal_screen(
                screen,
                bytes.as_bytes(),
                "appending styled agent lines",
            )?;
            bytes
        };
        self.persist_agent_presentation_entry(
            pane_id,
            styled_lines
                .iter()
                .map(|(style, _line)| style.persistence_name().to_string())
                .collect(),
            styled_lines
                .iter()
                .map(|(_style, line)| line.clone())
                .collect(),
            styled_lines
                .iter()
                .map(|(_style, line)| line.clone())
                .collect(),
            ansi_text,
        );
        Ok(())
    }

    /// Appends transformed assistant display lines while preserving raw copy text.
    fn append_agent_terminal_rendered_lines_to_buffer(
        &mut self,
        pane_id: &str,
        style: AgentTerminalPresentationStyle,
        rendered_lines: &[AgentRenderedLine],
        copy_lines: &[String],
    ) -> Result<()> {
        if rendered_lines.is_empty() {
            return Ok(());
        }
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if !self.pane_screens.contains_key(pane_id) {
            let screen = TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?;
            self.pane_screens.insert(pane_id.to_string(), screen);
        }
        self.clear_agent_shell_output_status_line(pane_id)?;
        let ansi_text = {
            let screen = self.pane_screens.get_mut(pane_id).ok_or_else(|| {
                MezError::invalid_state("agent terminal presentation screen was not initialized")
            })?;
            let mut bytes = String::new();
            let cursor = screen.cursor_state();
            let current_line_has_content = screen
                .visible_lines()
                .get(cursor.row)
                .is_some_and(|line| !line.trim().is_empty());
            if cursor.column == 0 && !current_line_has_content {
                bytes.push('\r');
            } else {
                bytes.push_str("\r\n");
            }
            for line in rendered_lines {
                append_styled_agent_terminal_rendered_line(&mut bytes, style, line, &self.ui_theme);
                bytes.push_str("\x1b[0m\r\n");
            }
            Self::feed_agent_terminal_screen(
                screen,
                bytes.as_bytes(),
                "appending rendered agent lines",
            )?;
            screen.set_recent_normal_copy_texts(copy_lines);
            bytes
        };
        self.persist_agent_presentation_entry(
            pane_id,
            vec![style.persistence_name().to_string(); rendered_lines.len()],
            rendered_lines
                .iter()
                .map(|line| line.display.clone())
                .collect(),
            copy_lines.to_vec(),
            ansi_text,
        );
        Ok(())
    }

    /// Updates the transient status row for a hidden running shell command.
    ///
    /// The row intentionally has no trailing newline. Later output replaces it
    /// in place, while the next durable agent transcript append clears it before
    /// writing normal log content.
    pub(super) fn append_agent_shell_output_status_line_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        line: &str,
    ) -> Result<()> {
        if self.agent_shell_view_enabled(pane_id) || line.trim().is_empty() {
            return Ok(());
        }
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if !self.pane_screens.contains_key(pane_id) {
            let screen = TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?;
            self.pane_screens.insert(pane_id.to_string(), screen);
        }
        let columns = self
            .pane_screens
            .get(pane_id)
            .map(|screen| usize::from(screen.size().columns))
            .unwrap_or_else(|| usize::from(descriptor.size.columns));
        let content_columns = columns
            .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
            .max(1);
        let line =
            fit_agent_terminal_text_width(&sanitized_agent_terminal_line(line), content_columns);
        let has_existing_status_line = self.agent_shell_output_status_lines.contains_key(pane_id);
        let screen = self.pane_screens.get_mut(pane_id).ok_or_else(|| {
            MezError::invalid_state("agent terminal presentation screen was not initialized")
        })?;
        let mut bytes = String::new();
        if has_existing_status_line {
            bytes.push_str("\r\x1b[2K");
        } else {
            let cursor = screen.cursor_state();
            let current_line_has_content = screen
                .visible_lines()
                .get(cursor.row)
                .is_some_and(|line| !line.trim().is_empty());
            if cursor.column == 0 && !current_line_has_content {
                bytes.push('\r');
            } else {
                bytes.push_str("\r\n");
            }
        }
        append_styled_agent_terminal_line(
            &mut bytes,
            AgentTerminalPresentationStyle::Status,
            &line,
            &self.ui_theme,
        );
        bytes.push_str("\x1b[0m");
        Self::feed_agent_terminal_screen(screen, bytes.as_bytes(), "updating shell output status")?;
        self.agent_shell_output_status_lines
            .insert(pane_id.to_string(), line);
        Ok(())
    }

    /// Clears a transient shell-output status row if one is active for a pane.
    fn clear_agent_shell_output_status_line(&mut self, pane_id: &str) -> Result<()> {
        if self
            .agent_shell_output_status_lines
            .remove(pane_id)
            .is_none()
        {
            return Ok(());
        }
        if let Some(screen) = self.pane_screens.get_mut(pane_id) {
            Self::feed_agent_terminal_screen(screen, b"\r\x1b[2K", "clearing shell output status")?;
        }
        Ok(())
    }

    /// Appends model-authored action summary text as normal-mode thinking logs.
    pub(super) fn append_agent_action_model_thinking_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
    ) -> Result<bool> {
        let thinking_lines = agent_action_model_thinking_lines(action);
        if thinking_lines.is_empty() {
            return Ok(false);
        }
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &agent_thinking_display_lines_for_width(&thinking_lines.join("\n"), columns),
            AgentTerminalPresentationStyle::Status,
        )?;
        Ok(true)
    }

    /// Appends a sanitized mutating-action diff preview to the pane buffer.
    ///
    /// The source text is the cleaned shell observation captured from the hidden
    /// transaction, so this path never exposes shell prompts or Mezzanine wrapper
    /// traffic while still giving users a copyable summary of filesystem changes.
    pub(super) fn append_agent_diff_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines =
            readable_agent_diff_display_lines_for_width(text, &self.ui_theme, display_width);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::DiffContext,
            &rendered_lines,
            &[],
        )
    }

    /// Records successful patch diffs for `/list-modified-files`.
    ///
    /// The source text is the same cleaned shell observation used for the
    /// normal diff preview, so counts are derived from the semantic patch diff
    /// rather than from shell echo or wrapper traffic.
    pub(super) fn record_agent_modified_files_from_diff(&mut self, pane_id: &str, text: &str) {
        let source_lines = cleaned_agent_diff_source_lines(text);
        for section in parse_agent_unified_diff_sections(&source_lines) {
            let path = agent_diff_section_path(&section).to_string();
            if path.is_empty() || path == "/dev/null" {
                continue;
            }
            let added = section
                .lines
                .iter()
                .filter(|line| line.marker == '+')
                .count();
            let removed = section
                .lines
                .iter()
                .filter(|line| line.marker == '-')
                .count();
            let entry = self
                .agent_modified_files
                .entry(pane_id.to_string())
                .or_default()
                .entry(path.clone())
                .or_insert_with(|| RuntimeAgentModifiedFileSummary {
                    path,
                    added: 0,
                    removed: 0,
                });
            entry.added = entry.added.saturating_add(added);
            entry.removed = entry.removed.saturating_add(removed);
        }
    }

    /// Appends a single human-readable action execution line to the pane.
    ///
    /// Semantic file/search and runtime URL actions should be legible in normal
    /// mode without dumping generated commands or result payloads. The line
    /// uses span-level styling so the action remains salient without forcing
    /// arguments to inherit the same visual weight.
    pub(super) fn append_agent_action_execution_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
    ) -> Result<bool> {
        let Some(header) = agent_action_execution_display_header(action) else {
            return Ok(false);
        };
        let thinking_lines = agent_action_model_thinking_lines(action);
        if !thinking_lines.is_empty() && self.agent_thinking_enabled(pane_id) {
            let columns = self.agent_terminal_presentation_columns(pane_id)?;
            self.append_agent_terminal_lines_to_buffer(
                pane_id,
                &agent_thinking_display_lines_for_width(&thinking_lines.join("\n"), columns),
                AgentTerminalPresentationStyle::Status,
            )?;
        }
        let rendered_line = agent_action_execution_rendered_line(&header, &self.ui_theme);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Status,
            &[rendered_line],
            &[],
        )?;
        Ok(true)
    }

    /// Appends a bounded, human-readable action result preview to the pane.
    ///
    /// Normal mode uses this renderer for mutating semantic action diffs. Other
    /// result previews remain reserved for elevated log levels.
    pub(super) fn append_agent_action_result_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
        result: &ActionResult,
        text: &str,
    ) -> Result<()> {
        if result.is_error {
            return Ok(());
        }
        if agent_action_result_uses_diff_preview(action) {
            return self.append_agent_diff_text_to_terminal_buffer(pane_id, text);
        }
        let Some(header) = agent_action_result_display_header(action) else {
            return Ok(());
        };
        let mut styled_lines = vec![(AgentTerminalPresentationStyle::Command, header)];
        styled_lines.extend(
            bounded_agent_action_result_display_lines(text)
                .into_iter()
                .map(|line| (AgentTerminalPresentationStyle::Status, line)),
        );
        self.append_agent_terminal_styled_lines_to_buffer(pane_id, &styled_lines)
    }

    /// Returns whether a cleaned action result preview should render in normal
    /// logging mode.
    pub(super) fn agent_action_result_renders_in_normal_mode(&self, action: &AgentAction) -> bool {
        agent_action_result_uses_diff_preview(action)
    }

    /// Runs the agent verbose enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn agent_verbose_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_verbose_status())
    }

    /// Runs the agent thinking enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn agent_thinking_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_thinking())
    }

    /// Runs the agent debug enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn agent_debug_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_debug())
    }

    /// Runs the agent trace enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn agent_trace_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_trace())
    }

    /// Runs the agent shell view enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn agent_shell_view_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_shell_view())
    }

    /// Runs the agent diagnostic level name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn agent_diagnostic_level_name(&self, pane_id: &str) -> Option<&'static str> {
        if self.agent_trace_enabled(pane_id) {
            Some("trace")
        } else if self.agent_debug_enabled(pane_id) {
            Some("debug")
        } else {
            None
        }
    }

    /// Runs the apply attached copy mode action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_attached_copy_mode_action(
        &mut self,
        action: CopyModeKeyAction,
    ) -> Result<bool> {
        let pane_id = self.active_pane_id()?;
        if self.scrollback_copy_mode_panes.remove(pane_id.as_str()) {
            self.active_copy_modes.remove(pane_id.as_str());
            return Ok(true);
        }
        let mut should_exit = false;
        let mut copied = None;
        {
            let copy_mode = self.ensure_active_copy_mode(pane_id.as_str())?;
            match action {
                CopyModeKeyAction::MoveUp => copy_mode.move_cursor_by(-1, 0),
                CopyModeKeyAction::MoveUpFast => copy_mode.move_cursor_by(-5, 0),
                CopyModeKeyAction::MoveDown => copy_mode.move_cursor_by(1, 0),
                CopyModeKeyAction::MoveDownFast => copy_mode.move_cursor_by(5, 0),
                CopyModeKeyAction::MoveLeft => copy_mode.move_cursor_by(0, -1),
                CopyModeKeyAction::MoveWordLeft => copy_mode.move_cursor_word_left(),
                CopyModeKeyAction::MoveRight => copy_mode.move_cursor_by(0, 1),
                CopyModeKeyAction::MoveWordRight => copy_mode.move_cursor_word_right(),
                CopyModeKeyAction::PageUp => copy_mode.page_up(),
                CopyModeKeyAction::PageDown => copy_mode.page_down(),
                CopyModeKeyAction::Top => copy_mode.scroll_to_top(),
                CopyModeKeyAction::LineStart => copy_mode.move_cursor_to_line_start(),
                CopyModeKeyAction::Bottom => copy_mode.scroll_to_bottom(),
                CopyModeKeyAction::LineEnd => copy_mode.move_cursor_to_line_end(),
                CopyModeKeyAction::BeginSelection => {
                    if copy_mode.selection().is_some() {
                        copied = Some(copy_mode.copy_selection()?);
                        copy_mode.clear_selection();
                    } else {
                        copy_mode.begin_keyboard_selection();
                    }
                }
                CopyModeKeyAction::Ignore => {}
                CopyModeKeyAction::Cancel => should_exit = true,
            }
        }
        if let Some(copied) = copied {
            let buffer_name = self
                .active_paste_buffer
                .clone()
                .unwrap_or_else(|| "clipboard".to_string());
            self.copy_text_to_buffer_and_host_clipboard(
                buffer_name.as_str(),
                copied,
                format!("pane:{pane_id}:copy-mode"),
            )?;
        }
        if should_exit {
            self.active_copy_modes.remove(pane_id.as_str());
            self.scrollback_copy_mode_panes.remove(pane_id.as_str());
        }
        Ok(true)
    }

    /// Runs the copy text to buffer and host clipboard operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn copy_text_to_buffer_and_host_clipboard(
        &mut self,
        name: &str,
        content: String,
        origin: String,
    ) -> Result<()> {
        self.paste_buffers
            .set_with_origin(name, content.as_str(), Some(origin))?;
        let _ = self.host_clipboard.copy(content.as_str());
        Ok(())
    }

    /// Runs the apply attached mouse action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_attached_mouse_action(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        action: MouseAction,
    ) -> Result<bool> {
        match action {
            MouseAction::Ignore => Ok(true),
            MouseAction::ForwardToPane => Ok(false),
            MouseAction::FocusWindow { index } => {
                self.session
                    .select_window(primary_client_id, &index.to_string())?;
                Ok(true)
            }
            MouseAction::FocusGroup { index } => {
                self.session
                    .select_group(primary_client_id, &index.to_string())?;
                self.sync_tracked_pty_sizes()?;
                Ok(true)
            }
            MouseAction::PressWindowAction { action } => {
                self.pressed_window_action = Some(action);
                Ok(true)
            }
            MouseAction::ReleaseWindowAction { action } => {
                let should_run = self.pressed_window_action.as_ref() == Some(&action);
                self.pressed_window_action = None;
                if should_run {
                    self.apply_window_frame_action(primary_client_id, action)?;
                }
                Ok(true)
            }
            MouseAction::CancelWindowAction => {
                self.pressed_window_action = None;
                Ok(true)
            }
            MouseAction::OpenPaneAgentStatusSelector { pane_index, field } => {
                self.open_pane_agent_status_selector(primary_client_id, pane_index, field)?;
                Ok(true)
            }
            MouseAction::HoverPaneAgentStatusSelector {
                pane_index,
                field,
                item_index,
            } => {
                self.hover_pane_agent_status_selector(pane_index, field, item_index);
                Ok(true)
            }
            MouseAction::SelectPaneAgentStatusSelector {
                pane_index,
                field,
                item_index,
            } => {
                self.select_pane_agent_status_selector(
                    primary_client_id,
                    pane_index,
                    field,
                    item_index,
                )?;
                Ok(true)
            }
            MouseAction::ScrollPaneAgentStatusSelector {
                pane_index,
                field,
                lines,
            } => {
                self.scroll_pane_agent_status_selector(pane_index, field, lines);
                Ok(true)
            }
            MouseAction::ClosePaneAgentStatusSelector => {
                self.pane_agent_status_selector = None;
                Ok(true)
            }
            MouseAction::BeginDisplayOverlaySelection { .. }
            | MouseAction::UpdateDisplayOverlaySelection { .. }
            | MouseAction::FinishDisplayOverlaySelection { .. } => Ok(false),
            MouseAction::SelectDisplayOverlay { .. } | MouseAction::ScrollDisplayOverlay { .. } => {
                Ok(false)
            }
            MouseAction::ShowWindowChooser { .. } => {
                self.execute_attached_display_command(primary_client_id, "choose-window")?;
                Ok(true)
            }
            MouseAction::FocusPane(position) => {
                let target = self
                    .mouse_pane_target_at(position)
                    .unwrap_or(MousePaneTarget {
                        pane_id: self.active_pane_id()?.to_string(),
                        position,
                    });
                let pane_id = target.pane_id.clone();
                self.session
                    .select_pane_global(primary_client_id, pane_id.as_str())?;
                if self.execute_agent_command_link_at_pane_position(
                    primary_client_id,
                    pane_id.as_str(),
                    target.position,
                )? {
                    self.mouse_selection_drag_state = None;
                    self.last_mouse_click_state = None;
                    return Ok(true);
                }
                let now = current_unix_millis();
                if self.last_mouse_click_state.as_ref().is_some_and(|click| {
                    click.pane_id == pane_id
                        && click.position == target.position
                        && now.saturating_sub(click.clicked_at_unix_ms)
                            <= DOUBLE_CLICK_WORD_SELECTION_WINDOW_MS
                }) {
                    self.mouse_selection_drag_state = None;
                    self.last_mouse_click_state = None;
                    self.copy_word_at_pane_position(
                        primary_client_id,
                        pane_id.as_str(),
                        target.position,
                    )?;
                    return Ok(true);
                }
                self.last_mouse_click_state = Some(RuntimeMouseClickState {
                    pane_id: pane_id.clone(),
                    position: target.position,
                    clicked_at_unix_ms: now,
                });
                self.mouse_selection_drag_state = Some(MouseSelectionDragState {
                    pane_id,
                    position: target.position,
                    origin_position: position,
                    autoscroll_position: None,
                });
                Ok(true)
            }
            MouseAction::CopyWord(position) => {
                let target = self
                    .mouse_pane_target_at(position)
                    .unwrap_or(MousePaneTarget {
                        pane_id: self.active_pane_id()?.to_string(),
                        position,
                    });
                self.copy_word_at_pane_position(
                    primary_client_id,
                    target.pane_id.as_str(),
                    target.position,
                )
            }
            MouseAction::FocusPaneOnly(position) => {
                let target = self
                    .mouse_pane_target_at(position)
                    .unwrap_or(MousePaneTarget {
                        pane_id: self.active_pane_id()?.to_string(),
                        position,
                    });
                self.session
                    .select_pane_global(primary_client_id, target.pane_id.as_str())?;
                self.mouse_selection_drag_state = None;
                Ok(true)
            }
            MouseAction::PasteClipboard(position) => {
                self.mouse_selection_drag_state = None;
                let target = self
                    .mouse_pane_target_at(position)
                    .unwrap_or(MousePaneTarget {
                        pane_id: self.active_pane_id()?.to_string(),
                        position,
                    });
                self.session
                    .select_pane_global(primary_client_id, target.pane_id.as_str())?;
                let Some(descriptor) = self.find_pane_descriptor(target.pane_id.as_str()) else {
                    return Ok(true);
                };
                match self.paste_clipboard_or_most_recent_buffer_to_text_entry_or_pane(
                    primary_client_id,
                    &descriptor,
                ) {
                    Ok(_) => Ok(true),
                    Err(err) if err.kind() == crate::error::MezErrorKind::NotFound => Ok(true),
                    Err(err) => Err(err),
                }
            }
            MouseAction::ResizePane { column, row } => {
                self.mouse_selection_drag_state = None;
                let Some(update) = self.mouse_resize_drag_update(column, row)? else {
                    let pane_id = self.active_pane_id()?;
                    let size = Size {
                        columns: column.saturating_add(1).max(MIN_PANE_COLUMNS),
                        rows: row.saturating_add(1).max(MIN_PANE_ROWS),
                    };
                    self.resize_pane_pty(primary_client_id, Some(pane_id.as_str()), size)?;
                    return Ok(true);
                };
                self.session
                    .replace_active_window_pane_geometries(primary_client_id, update.geometries)?;
                self.sync_tracked_pty_sizes()?;
                Ok(true)
            }
            MouseAction::FinishResizePane => {
                self.mouse_resize_drag_state = None;
                Ok(true)
            }
            MouseAction::ScrollHistory { lines, position } => {
                self.mouse_selection_drag_state = None;
                let target = self
                    .mouse_pane_target_at(position)
                    .unwrap_or(MousePaneTarget {
                        pane_id: self.active_pane_id()?.to_string(),
                        position,
                    });
                let should_exit = {
                    let copy_mode = self.ensure_active_copy_mode(target.pane_id.as_str())?;
                    copy_mode.scroll_by(lines);
                    lines > 0 && copy_mode.is_at_bottom() && copy_mode.selection().is_none()
                };
                if should_exit {
                    self.active_copy_modes.remove(target.pane_id.as_str());
                    self.scrollback_copy_mode_panes
                        .remove(target.pane_id.as_str());
                } else {
                    self.scrollback_copy_mode_panes
                        .insert(target.pane_id.clone());
                }
                Ok(true)
            }
            MouseAction::CopySelectionStart(position) => {
                let target = self.mouse_selection_target_at(position)?;
                self.session
                    .select_pane_global(primary_client_id, target.pane_id.as_str())?;
                let pane_id = target.pane_id;
                self.mouse_selection_drag_state = Some(MouseSelectionDragState {
                    pane_id: pane_id.clone(),
                    position: target.position,
                    origin_position: position,
                    autoscroll_position: None,
                });
                let copy_mode = self.ensure_mouse_selection_copy_mode(pane_id.as_str())?;
                let position = runtime_copy_position_for_view(copy_mode, target.position);
                copy_mode.select_range(position, position)?;
                Ok(true)
            }
            MouseAction::CopySelectionUpdate(position) => {
                self.apply_mouse_selection_update(primary_client_id, position, false)
            }
            MouseAction::CopySelectionFinish(position) => {
                self.apply_mouse_selection_update(primary_client_id, position, true)
            }
        }
    }

    /// Executes an agent command link embedded in visible pane output.
    ///
    /// # Parameters
    /// - `primary_client_id`: The primary client selecting the link.
    /// - `pane_id`: The pane whose visible output was clicked.
    /// - `position`: The pane-local cell position that was clicked.
    fn execute_agent_command_link_at_pane_position(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        pane_id: &str,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(command) = self.agent_command_link_at_pane_position(pane_id, position) else {
            return Ok(false);
        };
        let body = match self.execute_agent_shell_command(primary_client_id, &command) {
            Ok(body) => body,
            Err(error) => {
                self.set_agent_prompt_display_lines(
                    pane_id,
                    agent_prompt_error_display_lines(&error),
                )?;
                return Ok(true);
            }
        };
        match runtime_agent_shell_display_output(&body, &self.ui_theme) {
            Ok(display_output) => self.set_agent_prompt_display_output(pane_id, display_output)?,
            Err(error) => {
                self.set_agent_prompt_display_lines(
                    pane_id,
                    agent_prompt_error_display_lines(&error),
                )?;
            }
        }
        if runtime_agent_shell_visibility(&body).as_deref() == Some("hidden") {
            self.agent_prompt_inputs.remove(pane_id);
        }
        Ok(true)
    }

    /// Returns the agent command link at one visible pane position.
    ///
    /// # Parameters
    /// - `pane_id`: The pane whose visible line should be inspected.
    /// - `position`: The pane-local cell position to test.
    fn agent_command_link_at_pane_position(
        &self,
        pane_id: &str,
        position: CopyPosition,
    ) -> Option<String> {
        let screen = self.pane_screens.get(pane_id)?;
        let line = screen.visible_lines().get(position.line)?.to_string();
        agent_command_link_at_line_column(line.as_str(), position.column)
    }

    /// Runs a command-backed window status-bar action selected by mouse release.
    fn apply_window_frame_action(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        action: WindowFrameAction,
    ) -> Result<()> {
        let command = action.command().to_string();
        match action.command_kind() {
            WindowFrameCommandKind::Terminal => {
                self.execute_terminal_command(primary_client_id, &command)?;
            }
            WindowFrameCommandKind::Agent => {
                let pane_id = self.active_pane_id()?;
                self.enter_agent_mode_for_pane(&pane_id)?;
                self.execute_agent_shell_command(primary_client_id, &command)?;
            }
        }
        Ok(())
    }

    /// Applies keyboard navigation to the open pane-frame selector.
    fn apply_pane_agent_status_selector_terminal_action(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        action: &TerminalClientLoopAction,
    ) -> Result<bool> {
        let TerminalClientLoopAction::ForwardToPane(input) = action else {
            return Ok(false);
        };
        match runtime_selector_input_action(input) {
            RuntimeSelectorInputAction::Exit => {
                self.pane_agent_status_selector = None;
                Ok(true)
            }
            RuntimeSelectorInputAction::Previous => {
                self.move_pane_agent_status_selector(-1);
                Ok(true)
            }
            RuntimeSelectorInputAction::Next => {
                self.move_pane_agent_status_selector(1);
                Ok(true)
            }
            RuntimeSelectorInputAction::First => {
                self.set_pane_agent_status_selector_index(0);
                Ok(true)
            }
            RuntimeSelectorInputAction::Last => {
                if let Some(selector) = self.pane_agent_status_selector.as_ref() {
                    self.set_pane_agent_status_selector_index(
                        selector.items.len().saturating_sub(1),
                    );
                }
                Ok(true)
            }
            RuntimeSelectorInputAction::Select => {
                let Some(selector) = self.pane_agent_status_selector.as_ref() else {
                    return Ok(false);
                };
                self.select_pane_agent_status_selector(
                    primary_client_id,
                    selector.pane_index,
                    selector.field,
                    selector.active_index,
                )?;
                Ok(true)
            }
            RuntimeSelectorInputAction::Ignore => Ok(false),
        }
    }

    /// Moves the open pane-frame selector highlight by one row.
    fn move_pane_agent_status_selector(&mut self, delta: isize) {
        let visible_rows = self.pane_agent_status_selector_visible_rows();
        let Some(selector) = self.pane_agent_status_selector.as_mut() else {
            return;
        };
        selector.active_index =
            runtime_selector_step_index(selector.active_index, selector.items.len(), delta);
        runtime_pane_agent_status_selector_keep_active_visible(selector, visible_rows);
    }

    /// Sets the open pane-frame selector highlight to a bounded item index.
    fn set_pane_agent_status_selector_index(&mut self, item_index: usize) {
        let visible_rows = self.pane_agent_status_selector_visible_rows();
        let Some(selector) = self.pane_agent_status_selector.as_mut() else {
            return;
        };
        selector.active_index = item_index.min(selector.items.len().saturating_sub(1));
        runtime_pane_agent_status_selector_keep_active_visible(selector, visible_rows);
    }

    /// Scrolls the open pane-frame selector without changing pane scrollback.
    fn scroll_pane_agent_status_selector(
        &mut self,
        pane_index: usize,
        field: PaneAgentStatusField,
        lines: isize,
    ) {
        let visible_rows = self.pane_agent_status_selector_visible_rows();
        let Some(selector) = self.pane_agent_status_selector.as_mut() else {
            return;
        };
        if selector.pane_index != pane_index || selector.field != field {
            return;
        }
        let max_offset = selector.items.len().saturating_sub(visible_rows.max(1));
        if lines.is_negative() {
            selector.scroll_offset = selector.scroll_offset.saturating_sub(lines.unsigned_abs());
        } else {
            selector.scroll_offset = selector
                .scroll_offset
                .saturating_add(lines as usize)
                .min(max_offset);
        }
    }

    /// Returns the current selector's visible row count for the active window.
    fn pane_agent_status_selector_visible_rows(&self) -> usize {
        let Some(selector) = self.pane_agent_status_selector.as_ref() else {
            return 1;
        };
        let Some(size) = self.session.active_window().map(|window| window.size) else {
            return 1;
        };
        runtime_pane_agent_status_selector_layout(selector, size)
            .visible_items
            .len()
            .max(1)
    }

    /// Opens or applies the pane-frame selector for a pane.
    fn open_pane_agent_status_selector(
        &mut self,
        _primary_client_id: &crate::ids::ClientId,
        pane_index: usize,
        field: PaneAgentStatusField,
    ) -> Result<()> {
        let Some(window) = self.session.active_window() else {
            self.pane_agent_status_selector = None;
            return Ok(());
        };
        let Some(pane) = window.panes().iter().find(|pane| pane.index == pane_index) else {
            self.pane_agent_status_selector = None;
            return Ok(());
        };
        let pane_id = pane.id.to_string();
        if field == PaneAgentStatusField::Routing {
            self.pane_agent_status_selector = None;
            let outcome = self.execute_agent_shell_routing_command(&pane_id, "/routing toggle")?;
            let response =
                runtime_agent_shell_command_response_json(&pane_id, "/routing", Some(&outcome));
            if let Ok(display_output) =
                runtime_agent_shell_display_output(&response, &self.ui_theme)
            {
                self.set_agent_prompt_display_output(&pane_id, display_output)?;
            }
            return Ok(());
        }
        if field == PaneAgentStatusField::Thinking {
            self.pane_agent_status_selector = None;
            let outcome =
                self.execute_agent_shell_thinking_command(&pane_id, "/thinking toggle")?;
            let response =
                runtime_agent_shell_command_response_json(&pane_id, "/thinking", Some(&outcome));
            if let Ok(display_output) =
                runtime_agent_shell_display_output(&response, &self.ui_theme)
            {
                self.set_agent_prompt_display_output(&pane_id, display_output)?;
            }
            return Ok(());
        }
        let frame_context = self.terminal_frame_context();
        let cells = self.active_window_mouse_pane_agent_status_cells(&frame_context);
        let field_cells = cells
            .iter()
            .filter(|cell| cell.pane_index == pane_index && cell.field == field)
            .copied()
            .collect::<Vec<_>>();
        let Some(anchor_column) = field_cells.iter().map(|cell| cell.column).min() else {
            self.pane_agent_status_selector = None;
            return Ok(());
        };
        let anchor_row = field_cells.iter().map(|cell| cell.row).min().unwrap_or(0);
        let anchor_width = field_cells
            .iter()
            .map(|cell| cell.column)
            .max()
            .and_then(|max_column| max_column.checked_sub(anchor_column))
            .map(|width| width.saturating_add(1))
            .unwrap_or(1);
        let items = match field {
            PaneAgentStatusField::Model | PaneAgentStatusField::Preset => {
                self.configured_model_names_for_pane(&pane_id)?
            }
            PaneAgentStatusField::Reasoning => {
                let agent_id = format!("agent-{pane_id}");
                let (_active_name, active_profile) =
                    self.active_model_profile_for_pane(&pane_id, &agent_id, None)?;
                self.configured_reasoning_levels_for_pane_model(&pane_id, &active_profile.model)?
            }
            PaneAgentStatusField::Thinking => Vec::new(),
            PaneAgentStatusField::ApprovalPolicy => {
                vec![
                    "ask".to_string(),
                    "auto-allow".to_string(),
                    "full-access".to_string(),
                ]
            }
            PaneAgentStatusField::Latency => {
                let agent_id = format!("agent-{pane_id}");
                let (_active_name, active_profile) =
                    self.active_model_profile_for_pane(&pane_id, &agent_id, None)?;
                if self.model_profile_supports_latency_preference(&active_profile) {
                    vec![
                        "slow".to_string(),
                        "default".to_string(),
                        "fast".to_string(),
                    ]
                } else {
                    Vec::new()
                }
            }
            PaneAgentStatusField::Routing => Vec::new(),
        };
        if items.is_empty() {
            self.pane_agent_status_selector = None;
            return Ok(());
        }
        let active_value = self.active_pane_agent_status_selector_value(&pane_id, field);
        let active_index = active_value
            .as_deref()
            .and_then(|value| items.iter().position(|item| item == value))
            .unwrap_or(0);
        self.pane_agent_status_selector = Some(RuntimePaneAgentStatusSelector {
            pane_id,
            pane_index,
            field,
            items,
            active_index,
            scroll_offset: active_index,
            anchor_column,
            anchor_row,
            anchor_width,
        });
        let visible_rows = self.pane_agent_status_selector_visible_rows();
        if let Some(selector) = self.pane_agent_status_selector.as_mut() {
            runtime_pane_agent_status_selector_keep_active_visible(selector, visible_rows);
        }
        Ok(())
    }

    /// Updates the highlighted item for the open pane-frame selector.
    fn hover_pane_agent_status_selector(
        &mut self,
        pane_index: usize,
        field: PaneAgentStatusField,
        item_index: usize,
    ) {
        let Some(selector) = self.pane_agent_status_selector.as_mut() else {
            return;
        };
        if selector.pane_index == pane_index && selector.field == field {
            selector.active_index = item_index.min(selector.items.len().saturating_sub(1));
        }
    }

    /// Applies the selected pane-frame model or reasoning value.
    fn select_pane_agent_status_selector(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        pane_index: usize,
        field: PaneAgentStatusField,
        item_index: usize,
    ) -> Result<()> {
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let Some(selector) = self.pane_agent_status_selector.take() else {
            return Ok(());
        };
        if selector.pane_index != pane_index || selector.field != field {
            return Ok(());
        }
        let Some(value) = selector.items.get(item_index).cloned() else {
            return Ok(());
        };
        let outcome = match field {
            PaneAgentStatusField::Model | PaneAgentStatusField::Preset => {
                self.apply_pane_model_picker_selection(&selector.pane_id, &value)?
            }
            PaneAgentStatusField::Reasoning => {
                self.apply_pane_reasoning_picker_selection(&selector.pane_id, &value)?
            }
            PaneAgentStatusField::Thinking => return Ok(()),
            PaneAgentStatusField::ApprovalPolicy => {
                let outcome = self.execute_agent_shell_approval_command(
                    &selector.pane_id,
                    &format!("/approval {value}"),
                )?;
                let response = runtime_agent_shell_command_response_json(
                    &selector.pane_id,
                    "/approval",
                    Some(&outcome),
                );
                if let Ok(display_output) =
                    runtime_agent_shell_display_output(&response, &self.ui_theme)
                {
                    self.set_agent_prompt_display_output(&selector.pane_id, display_output)?;
                }
                return Ok(());
            }
            PaneAgentStatusField::Routing => return Ok(()),
            PaneAgentStatusField::Latency => {
                self.apply_pane_latency_picker_selection(&selector.pane_id, &value)?
            }
        };
        let response = runtime_agent_shell_command_response_json(
            &selector.pane_id,
            match field {
                PaneAgentStatusField::Model => "/model",
                PaneAgentStatusField::Reasoning => "/model reasoning",
                PaneAgentStatusField::Thinking => "/thinking",
                PaneAgentStatusField::Routing => "/routing",
                PaneAgentStatusField::ApprovalPolicy => "/approval",
                PaneAgentStatusField::Latency => "/latency",
                PaneAgentStatusField::Preset => "/model",
            },
            Some(&outcome),
        );
        if let Ok(display_output) = runtime_agent_shell_display_output(&response, &self.ui_theme) {
            self.set_agent_prompt_display_output(&selector.pane_id, display_output)?;
        }
        Ok(())
    }

    /// Returns the active pane-frame value represented by a selector field.
    fn active_pane_agent_status_selector_value(
        &self,
        pane_id: &str,
        field: PaneAgentStatusField,
    ) -> Option<String> {
        match field {
            PaneAgentStatusField::Model
            | PaneAgentStatusField::Reasoning
            | PaneAgentStatusField::Thinking => {
                let agent_id = format!("agent-{pane_id}");
                let (_active_name, profile) = self
                    .active_model_profile_for_pane(pane_id, &agent_id, None)
                    .ok()?;
                match field {
                    PaneAgentStatusField::Model => {
                        Some(format!("{}: {}", profile.provider, profile.model))
                    }
                    PaneAgentStatusField::Reasoning => profile.reasoning_profile,
                    PaneAgentStatusField::Thinking => self
                        .model_profile_thinking_enabled(&profile)
                        .map(|enabled| if enabled { "on" } else { "off" }.to_string()),
                    _ => None,
                }
            }
            PaneAgentStatusField::Routing => Some(
                if self
                    .agent_routing_overrides
                    .get(pane_id)
                    .copied()
                    .unwrap_or(self.agent_routing)
                {
                    "auto:on"
                } else {
                    "auto:off"
                }
                .to_string(),
            ),
            PaneAgentStatusField::ApprovalPolicy => Some(
                runtime_approval_policy_name(self.permission_policy.approval_policy).to_string(),
            ),
            PaneAgentStatusField::Latency => {
                let agent_id = format!("agent-{pane_id}");
                let (_active_name, profile) = self
                    .active_model_profile_for_pane(pane_id, &agent_id, None)
                    .ok()?;
                if !self.model_profile_supports_latency_preference(&profile) {
                    return None;
                }
                profile
                    .latency_preference
                    .or_else(|| Some("default".to_string()))
            }
            PaneAgentStatusField::Preset => self
                .active_model_preset_name_for_pane(pane_id)
                .map(|preset| format!("preset: {preset}")),
        }
    }

    /// Runs the apply mouse selection update operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_mouse_selection_update(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        position: CopyPosition,
        finish: bool,
    ) -> Result<bool> {
        let target = self.mouse_selection_target_at(position)?;
        self.session
            .select_pane_global(primary_client_id, target.pane_id.as_str())?;
        let pane_id = target.pane_id;
        let anchor = self
            .mouse_selection_drag_state
            .as_ref()
            .filter(|state| state.pane_id == pane_id)
            .map(|state| state.position)
            .unwrap_or(target.position);
        let origin = self
            .mouse_selection_drag_state
            .as_ref()
            .filter(|state| state.pane_id == pane_id)
            .map(|state| state.origin_position)
            .unwrap_or(position);
        if finish && !self.active_copy_modes.contains_key(pane_id.as_str()) {
            self.mouse_selection_drag_state = None;
            return Ok(true);
        }
        let copied = {
            let copy_mode = self.ensure_mouse_selection_copy_mode(pane_id.as_str())?;
            let start = copy_mode
                .selection()
                .map(|(start, _)| start)
                .unwrap_or_else(|| runtime_copy_position_for_view(copy_mode, anchor));
            if let Some(edge) = target.edge {
                copy_mode.scroll_by(edge.scroll_delta(origin, position));
            }
            let end = runtime_copy_position_for_view(copy_mode, target.position);
            copy_mode.select_range(start, end)?;
            finish.then(|| copy_mode.copy_selection()).transpose()?
        };
        if finish {
            self.mouse_selection_drag_state = None;
            self.active_copy_modes.remove(pane_id.as_str());
            self.scrollback_copy_mode_panes.remove(pane_id.as_str());
            if let Some(copied) = copied {
                self.copy_text_to_buffer_and_host_clipboard(
                    "mouse",
                    copied,
                    format!("pane:{pane_id}:mouse"),
                )?;
            }
        } else {
            self.mouse_selection_drag_state = Some(MouseSelectionDragState {
                pane_id,
                position: anchor,
                origin_position: origin,
                autoscroll_position: target.edge.map(|_| position),
            });
        }
        Ok(true)
    }

    /// Ensures mouse drag selection has a copy buffer for the selected pane.
    ///
    /// Alternate-screen applications are excluded from normal scrollback by
    /// design, but mouse drag selection is an explicit copy operation over the
    /// visible pane body. For that path, seed copy mode from visible rows so
    /// full-screen terminal apps can still be copied without changing history
    /// capture semantics.
    fn ensure_mouse_selection_copy_mode(&mut self, pane_id: &str) -> Result<&mut CopyMode> {
        if !self.active_copy_modes.contains_key(pane_id) {
            let viewport_rows = self.copy_mode_viewport_rows_for_pane(pane_id);
            let screen = self.pane_screens.get(pane_id).ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pane screen not found",
                )
            })?;
            let copy_mode = if screen.alternate_screen_active() {
                CopyMode::from_visible_screen(screen, viewport_rows)?
            } else {
                CopyMode::from_screen(screen, viewport_rows)?
            };
            self.active_copy_modes
                .insert(pane_id.to_string(), copy_mode);
        }
        self.active_copy_modes
            .get_mut(pane_id)
            .ok_or_else(|| MezError::invalid_state("active copy mode was not retained"))
    }

    /// Selects and copies the readline-style word under one pane-local position.
    ///
    /// # Parameters
    /// - `primary_client_id`: The primary client whose focus follows the click.
    /// - `pane_id`: Pane whose copy-mode buffer supplies the word text.
    /// - `position`: Pane-local terminal cell used as the word-selection anchor.
    fn copy_word_at_pane_position(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        pane_id: &str,
        position: CopyPosition,
    ) -> Result<bool> {
        self.session
            .select_pane_global(primary_client_id, pane_id)?;
        // Ensure a copy mode exists, then take ownership so the selection
        // highlight persists for one render frame before cleanup.
        self.ensure_active_copy_mode(pane_id)?;
        let mut copy_mode = self
            .active_copy_modes
            .remove(pane_id)
            .ok_or_else(|| MezError::invalid_state("active copy mode was not retained"))?;
        let copied = {
            let position = runtime_copy_position_for_view(&copy_mode, position);
            copy_mode.select_word_at(position)?;
            copy_mode.copy_selection()?
        };
        self.mouse_selection_drag_state = None;
        self.scrollback_copy_mode_panes.remove(pane_id);
        self.deferred_word_copy_cleanup
            .replace(Some((pane_id.to_string(), copy_mode)));
        self.copy_text_to_buffer_and_host_clipboard(
            "mouse",
            copied,
            format!("pane:{pane_id}:mouse-word"),
        )?;
        Ok(true)
    }

    /// Runs the mouse resize drag update operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mouse_resize_drag_update(
        &mut self,
        column: u16,
        row: u16,
    ) -> Result<Option<MouseResizeDragUpdate>> {
        if let Some(state) = self.mouse_resize_drag_state.clone() {
            return Ok(Some(mouse_resize_update_from_state(state, column, row)));
        }
        let Some(state) = self.mouse_resize_drag_state_at(column, row) else {
            return Ok(None);
        };
        let update = mouse_resize_update_from_state(state.clone(), column, row);
        self.mouse_resize_drag_state = Some(state);
        Ok(Some(update))
    }

    /// Runs the mouse resize drag state at operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mouse_resize_drag_state_at(&self, column: u16, row: u16) -> Option<MouseResizeDragState> {
        let window = self.session.active_window()?;
        let window_frame_visible = self.window_frames_enabled;
        let group_top_offset = u16::from(self.session.window_groups().len() > 1);
        if group_top_offset > 0 && row == 0 {
            return None;
        }
        let mut display_window = window.clone();
        display_window.size = Size::new(
            window.size.columns,
            window.size.rows.saturating_sub(group_top_offset).max(1),
        )
        .ok()?;
        let local_row = row.checked_sub(group_top_offset)?;
        if window_frame_visible {
            match self.window_frame_position {
                TerminalFramePosition::Top if local_row == 0 => return None,
                TerminalFramePosition::Bottom
                    if local_row == display_window.size.rows.saturating_sub(1) =>
                {
                    return None;
                }
                _ => {}
            }
        }
        let row_offset = group_top_offset.saturating_add(u16::from(
            window_frame_visible && self.window_frame_position == TerminalFramePosition::Top,
        ));
        let body_row = row.checked_sub(row_offset)?;
        let geometries = rendered_pane_geometries(&display_window, window_frame_visible).ok()?;

        vertical_mouse_resize_state(&geometries, column, body_row)
            .or_else(|| horizontal_mouse_resize_state(&geometries, body_row, column, row_offset))
    }

    /// Runs the mouse pane target at operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mouse_pane_target_at(&self, position: CopyPosition) -> Option<MousePaneTarget> {
        let window = self.session.active_window()?;
        let window_frame_visible = self.window_frames_enabled;
        let column = u16::try_from(position.column).ok()?;
        let row = u16::try_from(position.line).ok()?;
        let group_top_offset = u16::from(self.session.window_groups().len() > 1);
        if group_top_offset > 0 && row == 0 {
            return None;
        }
        let mut display_window = window.clone();
        display_window.size = Size::new(
            window.size.columns,
            window.size.rows.saturating_sub(group_top_offset).max(1),
        )
        .ok()?;
        let local_row = row.checked_sub(group_top_offset)?;
        let window_frame_top_offset = group_top_offset.saturating_add(u16::from(
            window_frame_visible && self.window_frame_position == TerminalFramePosition::Top,
        ));
        if window_frame_visible {
            match self.window_frame_position {
                TerminalFramePosition::Top if local_row == 0 => return None,
                TerminalFramePosition::Bottom
                    if local_row == display_window.size.rows.saturating_sub(1) =>
                {
                    return None;
                }
                _ => {}
            }
        }
        let body_row = row.checked_sub(window_frame_top_offset)?;
        let geometries = rendered_pane_geometries(&display_window, window_frame_visible).ok()?;
        for geometry in &geometries {
            let region_size = pane_render_region_size_for_geometry(geometry, &geometries).ok()?;
            let row_end = geometry.row.saturating_add(region_size.rows);
            let column_end = geometry.column.saturating_add(region_size.columns);
            if body_row < geometry.row
                || body_row >= row_end
                || column < geometry.column
                || column >= column_end
            {
                continue;
            }
            let pane = window
                .panes()
                .iter()
                .find(|pane| pane.index == geometry.index)?;
            let pane_frame_top_offset = u16::from(
                self.pane_frames_enabled
                    && self.pane_frame_position == TerminalFramePosition::Top
                    && !pane_frame_merges_into_divider(
                        geometry,
                        &geometries,
                        self.pane_frame_position,
                    ),
            );
            if pane_frame_top_offset > 0 && body_row == geometry.row {
                return None;
            }
            let local_row = body_row
                .saturating_sub(geometry.row)
                .saturating_sub(pane_frame_top_offset);
            let local_column = column.saturating_sub(geometry.column);
            return Some(MousePaneTarget {
                pane_id: pane.id.to_string(),
                position: CopyPosition {
                    line: usize::from(local_row),
                    column: usize::from(local_column),
                },
            });
        }
        None
    }

    /// Runs the mouse selection target at operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mouse_selection_target_at(&self, position: CopyPosition) -> Result<MouseSelectionTarget> {
        if let Some(state) = self.mouse_selection_drag_state.as_ref()
            && let Some(target) =
                self.mouse_pane_selection_target_at(state.pane_id.as_str(), position)
        {
            return Ok(target);
        }
        if let Some(target) = self.mouse_pane_target_at(position) {
            if let Some(selection_target) =
                self.mouse_pane_selection_target_at(target.pane_id.as_str(), position)
            {
                return Ok(selection_target);
            }
            return Ok(MouseSelectionTarget {
                pane_id: target.pane_id,
                position: target.position,
                edge: None,
            });
        }
        let active_pane_id = self.active_pane_id()?.to_string();
        if let Some(selection_target) =
            self.mouse_pane_selection_target_at(active_pane_id.as_str(), position)
        {
            return Ok(selection_target);
        }
        Ok(MouseSelectionTarget {
            pane_id: active_pane_id,
            position: CopyPosition { line: 0, column: 0 },
            edge: None,
        })
    }

    /// Runs the mouse pane selection target at operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mouse_pane_selection_target_at(
        &self,
        pane_id: &str,
        position: CopyPosition,
    ) -> Option<MouseSelectionTarget> {
        let window = self.session.active_window()?;
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == pane_id)?;
        let (region_row, region_column, content_size) =
            self.copy_mode_overlay_region(window, pane.index)?;
        let row = isize::try_from(position.line).ok()?;
        let column = isize::try_from(position.column).ok()?;
        let content_start_row = isize::try_from(region_row).ok()?;
        let content_rows = isize::try_from(content_size.rows).ok()?.max(1);
        let content_last_row = content_start_row.saturating_add(content_rows.saturating_sub(1));
        let edge = if row <= content_start_row {
            Some(MouseSelectionEdge::Above)
        } else if row >= content_last_row {
            Some(MouseSelectionEdge::Below)
        } else {
            None
        };
        let local_line = if row < content_start_row {
            0
        } else if row > content_last_row {
            usize::from(content_size.rows.saturating_sub(1))
        } else {
            usize::try_from(row.saturating_sub(content_start_row)).ok()?
        };
        let content_columns = usize::from(content_size.columns);
        let geometry_column = isize::try_from(region_column).ok()?;
        let content_end_column =
            geometry_column.saturating_add(isize::try_from(content_size.columns).ok()?);
        let local_column = if column < geometry_column {
            0
        } else if column >= content_end_column {
            content_columns
        } else {
            usize::try_from(column.saturating_sub(geometry_column)).ok()?
        };
        Some(MouseSelectionTarget {
            pane_id: pane_id.to_string(),
            position: CopyPosition {
                line: local_line,
                column: local_column,
            },
            edge,
        })
    }

    /// Runs the copy mode viewport rows for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn copy_mode_viewport_rows_for_pane(&self, pane_id: &str) -> usize {
        self.session
            .active_window()
            .and_then(|window| {
                window
                    .panes()
                    .iter()
                    .find(|pane| pane.id.as_str() == pane_id)
                    .and_then(|pane| self.copy_mode_overlay_region(window, pane.index))
            })
            .map(|(_, _, size)| usize::from(size.rows))
            .or_else(|| {
                self.find_pane_descriptor(pane_id)
                    .map(|descriptor| usize::from(descriptor.size.rows))
            })
            .unwrap_or_else(|| usize::from(self.session.authoritative_size.rows))
            .max(1)
    }

    /// Runs the ensure active copy mode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn ensure_active_copy_mode(&mut self, pane_id: &str) -> Result<&mut CopyMode> {
        if !self.active_copy_modes.contains_key(pane_id) {
            let viewport_rows = self.copy_mode_viewport_rows_for_pane(pane_id);
            let screen = self.pane_screens.get(pane_id).ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pane screen not found",
                )
            })?;
            let copy_mode = CopyMode::from_screen(screen, viewport_rows)?;
            self.active_copy_modes
                .insert(pane_id.to_string(), copy_mode);
        }
        self.active_copy_modes
            .get_mut(pane_id)
            .ok_or_else(|| MezError::invalid_state("active copy mode was not retained"))
    }

    /// Runs the apply attached mux action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_attached_mux_action(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        action: MuxAction,
    ) -> Result<bool> {
        match action {
            MuxAction::SendPrefixToPane => {
                let input = key_chord_input_bytes(self.key_bindings.escape).ok_or_else(|| {
                    MezError::invalid_state("configured prefix key cannot be sent to pane")
                })?;
                self.write_input_to_pane(primary_client_id, None, &input)?;
            }
            MuxAction::ListKeyBindings => {
                self.execute_attached_display_command(primary_client_id, "list-keys")?;
            }
            MuxAction::NewWindow => {
                self.create_window_with_pane_process(primary_client_id, "shell", true, None)?;
            }
            MuxAction::NewGroup => {
                self.create_group_with_pane_process(primary_client_id, "shell", true, None, None)?;
            }
            MuxAction::SplitPaneVertical => {
                self.split_pane_with_process(primary_client_id, SplitDirection::Vertical, None)?;
            }
            MuxAction::SplitPaneHorizontal => {
                self.split_pane_with_process(primary_client_id, SplitDirection::Horizontal, None)?;
            }
            MuxAction::FocusPane(direction) => {
                self.session.select_adjacent_pane(
                    primary_client_id,
                    pane_navigation_direction(direction),
                )?;
            }
            MuxAction::FocusLastPane => {
                self.session.select_last_pane(primary_client_id)?;
            }
            MuxAction::EnterCopyMode => {
                let pane_id = self.active_pane_id()?;
                self.ensure_active_copy_mode(pane_id.as_str())?;
            }
            MuxAction::EnterCopyModeAndPageUp => {
                let pane_id = self.active_pane_id()?;
                let copy_mode = self.ensure_active_copy_mode(pane_id.as_str())?;
                copy_mode.page_up();
            }
            MuxAction::FocusWindow(WindowFocusTarget::Next) => {
                self.session.next_window(primary_client_id)?;
            }
            MuxAction::FocusWindow(WindowFocusTarget::Previous) => {
                self.session.previous_window(primary_client_id)?;
            }
            MuxAction::FocusWindow(WindowFocusTarget::LastActive) => {
                self.session.last_window(primary_client_id)?;
            }
            MuxAction::FocusWindow(WindowFocusTarget::Index(index)) => {
                self.session
                    .select_window(primary_client_id, &index.to_string())?;
            }
            MuxAction::FocusWindow(WindowFocusTarget::ChooseInteractively) => {
                self.execute_attached_display_command(primary_client_id, "choose-window")?;
            }
            MuxAction::FocusGroup(GroupFocusTarget::Next) => {
                self.session.next_group(primary_client_id)?;
                self.sync_tracked_pty_sizes()?;
            }
            MuxAction::FocusGroup(GroupFocusTarget::Previous) => {
                self.session.previous_group(primary_client_id)?;
                self.sync_tracked_pty_sizes()?;
            }
            MuxAction::FocusGroup(GroupFocusTarget::LastActive) => {
                self.session.last_group(primary_client_id)?;
                self.sync_tracked_pty_sizes()?;
            }
            MuxAction::FocusGroup(GroupFocusTarget::ChooseInteractively) => {
                self.execute_attached_display_command(primary_client_id, "choose-group")?;
            }
            MuxAction::CyclePane => {
                self.session
                    .select_adjacent_pane(primary_client_id, PaneNavigationDirection::Right)?;
            }
            MuxAction::ShowPaneIndexes => {
                self.execute_attached_display_command(primary_client_id, "display-panes")?;
            }
            MuxAction::TogglePaneZoom => {
                self.session.toggle_active_pane_zoom(primary_client_id)?;
                self.sync_tracked_pty_sizes()?;
            }
            MuxAction::CycleLayouts => {
                self.session.cycle_layout(primary_client_id)?;
                self.sync_tracked_pty_sizes()?;
            }
            MuxAction::BreakPaneToNewWindow => {
                self.break_pane_and_sync_pty_sizes(
                    primary_client_id,
                    None,
                    Some("shell".to_string()),
                    true,
                )?;
            }
            MuxAction::SwapPanePrevious | MuxAction::SwapPaneNext => {
                if !self.swap_active_pane_with_neighbor(primary_client_id, action)? {
                    return Ok(false);
                }
            }
            MuxAction::DetachPrimaryClient => {
                self.detach_primary(primary_client_id, self.session.authoritative_size)?;
            }
            MuxAction::ChooseClientOrObserverToDetach => {
                self.execute_attached_display_command(primary_client_id, "choose-client")?;
            }
            MuxAction::PasteBuffer(PasteBufferTarget::MostRecent) => {
                if !self.paste_most_recent_buffer_to_active_pane(primary_client_id)? {
                    return Ok(false);
                }
            }
            MuxAction::PasteBuffer(PasteBufferTarget::ChooseInteractively) => {
                self.execute_attached_display_command(primary_client_id, "choose-buffer")?;
            }
            MuxAction::ListPasteBuffers => {
                self.execute_attached_display_command(primary_client_id, "list-buffers")?;
            }
            MuxAction::DeleteMostRecentPasteBuffer => {
                let Some(name) = self.paste_buffers.most_recent_name().map(ToOwned::to_owned)
                else {
                    return Ok(false);
                };
                self.execute_attached_display_command(
                    primary_client_id,
                    &format!("delete-buffer {name}"),
                )?;
            }
            MuxAction::ChoosePendingObservers => {
                self.execute_attached_display_command(primary_client_id, "choose-observer")?;
            }
            MuxAction::ToggleAgentShell => {
                self.toggle_active_agent_shell()?;
            }
            MuxAction::ShowMessages => {
                self.execute_attached_display_command(primary_client_id, "show-messages")?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Runs the execute attached display command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_attached_display_command(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        command: &str,
    ) -> Result<()> {
        let output = self.execute_terminal_command(primary_client_id, command)?;
        let output_excerpt = output.chars().take(384).collect::<String>();
        let truncated = output_excerpt.len() < output.len();
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"attached_display_command":"{}","output":"{}","truncated":{}}}"#,
                json_escape(command),
                json_escape(&output_excerpt),
                truncated
            ),
        )?;
        let content = runtime_command_display_overlay_content(&output)?;
        if runtime_command_display_should_open_overlay(&content) {
            self.show_primary_display_overlay_inner(
                content.lines,
                content.line_style_spans,
                content.selections,
                false,
            )
        } else {
            self.append_runtime_command_display_lines_to_active_pane(&content.lines)
        }
    }

    /// Runs the swap active pane with neighbor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn swap_active_pane_with_neighbor(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        action: MuxAction,
    ) -> Result<bool> {
        let window = self
            .session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        if window.panes().len() < 2 {
            return Ok(false);
        }
        let active = window.active_pane_index();
        let target = match action {
            MuxAction::SwapPanePrevious => {
                if active == 0 {
                    window.panes().len() - 1
                } else {
                    active - 1
                }
            }
            MuxAction::SwapPaneNext => (active + 1) % window.panes().len(),
            _ => return Ok(false),
        };
        self.swap_panes_and_sync_pty_sizes(primary_client_id, None, &target.to_string())?;
        Ok(true)
    }

    /// Runs the paste most recent buffer to active pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn paste_most_recent_buffer_to_active_pane(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
    ) -> Result<bool> {
        let Some(source) = self.most_recent_paste_buffer_source() else {
            return Ok(false);
        };
        let descriptor = self.active_window_pane_descriptor(None)?;
        self.paste_source_to_pane(primary_client_id, &descriptor, source)
    }

    /// Runs the paste clipboard or most recent buffer to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn paste_clipboard_or_most_recent_buffer_to_pane(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        descriptor: &PaneDescriptor,
    ) -> Result<bool> {
        let Some(source) = self.clipboard_or_most_recent_paste_source() else {
            return Ok(false);
        };
        self.paste_source_to_pane(primary_client_id, descriptor, source)
    }

    /// Pastes clipboard or paste-buffer content into active prompt text when
    /// one is visible, otherwise into the clicked pane.
    fn paste_clipboard_or_most_recent_buffer_to_text_entry_or_pane(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        descriptor: &PaneDescriptor,
    ) -> Result<bool> {
        let Some(source) = self.clipboard_or_most_recent_paste_source() else {
            return Ok(false);
        };
        self.paste_source_to_text_entry_or_pane(primary_client_id, descriptor, source)
    }

    /// Routes one paste source to a prompt text entry or a pane PTY.
    fn paste_source_to_text_entry_or_pane(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        descriptor: &PaneDescriptor,
        source: RuntimePasteSource,
    ) -> Result<bool> {
        if source.content.is_empty() {
            return Ok(false);
        }
        let paste_bytes = runtime_readline_paste_bytes(source.content.as_str());
        if self.primary_prompt_input.is_some() {
            return self.apply_primary_prompt_input(primary_client_id, &paste_bytes);
        }
        if self
            .agent_shell_store
            .get(descriptor.pane_id.as_str())
            .is_some_and(|session| session.visibility == AgentShellVisibility::Visible)
        {
            return self.apply_attached_agent_prompt_input_for_pane(
                primary_client_id,
                descriptor.pane_id.as_str(),
                &paste_bytes,
            );
        }
        self.paste_source_to_pane(primary_client_id, descriptor, source)
    }

    /// Runs the clipboard or most recent paste source operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn clipboard_or_most_recent_paste_source(&self) -> Option<RuntimePasteSource> {
        if let Some(content) = self
            .host_clipboard
            .read()
            .filter(|content| !content.is_empty())
        {
            return Some(RuntimePasteSource {
                label: "host-clipboard".to_string(),
                buffer_name: None,
                content,
            });
        }
        self.most_recent_paste_buffer_source()
    }

    /// Runs the most recent paste buffer source operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn most_recent_paste_buffer_source(&self) -> Option<RuntimePasteSource> {
        let buffer_name = self.paste_buffers.most_recent_name()?.to_string();
        let content = self.paste_buffers.get(&buffer_name)?.to_string();
        Some(RuntimePasteSource {
            label: "paste-buffer".to_string(),
            buffer_name: Some(buffer_name),
            content,
        })
    }

    /// Runs the paste source to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn paste_source_to_pane(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        descriptor: &PaneDescriptor,
        source: RuntimePasteSource,
    ) -> Result<bool> {
        if source.content.is_empty() {
            return Ok(false);
        }
        let paste_bytes = runtime_paste_bytes(
            self.pane_screens.get(descriptor.pane_id.as_str()),
            source.content.as_str(),
        );
        let dispatch = self.write_input_to_pane(
            primary_client_id,
            Some(descriptor.pane_id.as_str()),
            &paste_bytes,
        )?;
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","paste_source":"{}","paste_buffer":{},"input_bytes":{}}}"#,
                json_escape(&dispatch.pane_id),
                json_escape(&source.label),
                source
                    .buffer_name
                    .as_ref()
                    .map(|name| format!(r#""{}""#, json_escape(name)))
                    .unwrap_or_else(|| "null".to_string()),
                dispatch.bytes_written
            ),
        )?;
        Ok(true)
    }

    /// Runs the approve observer with runtime cutoff operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn approve_observer_with_runtime_cutoff(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        observer_id: &str,
    ) -> Result<()> {
        if let Some(visible_from_event_id) = self
            .event_log
            .as_ref()
            .map(|event_log| event_log.latest_event_id().saturating_add(1))
        {
            self.session
                .approve_observer_target_with_visible_from_event_id(
                    primary_client_id,
                    observer_id,
                    visible_from_event_id,
                )
        } else {
            self.session
                .approve_observer_target(primary_client_id, observer_id)
        }
    }

    /// Runs the active pane id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn active_pane_id(&self) -> Result<String> {
        self.session
            .active_window()
            .map(|window| window.active_pane().id.to_string())
            .ok_or_else(|| MezError::invalid_state("session has no active pane"))
    }
}

/// Runs the apply copy mode selection spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn apply_copy_mode_selection_spans(
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
fn apply_copy_mode_terminal_cursor(
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
fn ordered_copy_positions(
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
fn copy_selection_rendition(
    ui_theme: &crate::terminal::UiTheme,
) -> crate::terminal::GraphicRendition {
    let mut rendition = ui_theme.colors.copy_selection.rendition();
    rendition.inverse = true;
    rendition
}

#[cfg(test)]
mod tests {
    use super::{
        AgentRenderedLine, AgentRenderedLineKind, agent_action_result_uses_diff_preview,
        agent_thinking_display_lines_for_width, command_preview_terminal_rendered_lines,
        readable_agent_diff_display_lines, readable_agent_diff_display_lines_for_width,
        render_command_markdown_body_lines, rendered_line_rendition_at,
        runtime_agent_shell_markdown_overlay_content, runtime_command_display_overlay_content,
        runtime_display_overlay_rendered_line_style_spans,
        runtime_display_overlay_rendered_selection_start,
        runtime_display_overlay_selection_prefix_columns, runtime_human_readable_display_lines,
        wrap_agent_rendered_line_to_width, wrap_agent_terminal_text,
        wrapped_prefixed_agent_terminal_lines,
    };
    use crate::agent::{AgentAction, AgentActionPayload};
    use crate::layout::Size;
    use crate::runtime::types::{
        RuntimeDisplayOverlay, RuntimeDisplayOverlaySearchMatch, RuntimeDisplayOverlaySelection,
        RuntimeDisplayOverlaySelectionKind,
    };
    use crate::terminal::{GraphicRendition, TerminalStyleSpan, default_ui_theme};

    /// Verifies normal-mode mutation result rendering treats patches as the
    /// only diff-producing file mutation operation.
    ///
    /// The semantic shell helper emits unified diffs for this action; this
    /// guard keeps the runtime display gate aligned so users see the readable
    /// change preview in normal logs.
    #[test]
    fn agent_action_result_diff_preview_includes_apply_patch_only() {
        let patch = AgentAction {
            id: "patch".to_string(),
            rationale: String::new(),
            payload: AgentActionPayload::ApplyPatch {
                patch: "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n*** End Patch"
                    .to_string(),
                strip: None,
            },
        };

        assert!(agent_action_result_uses_diff_preview(&patch));
    }

    /// Verifies semantic action diff output is parsed into compact display rows
    /// while removing Mezzanine-owned prompt and wrapper lines. This protects
    /// normal agent logs from showing raw PTY transaction mechanics around a
    /// filesystem change.
    #[test]
    fn readable_agent_diff_display_lines_parse_noisy_unified_diff() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let lines = readable_agent_diff_display_lines(
            "\n∙\nMEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\n\
             diff -- update file\n--- a/src/runtime/agent.rs\n+++ b/src/runtime/agent.rs\n\
             @@ -10,3 +10,3 @@\n context\n-old\n+new\n\
             @@ -20,2 +20,2 @@\n-again\n+done\n\n",
            &ui_theme,
        )
        .into_iter()
        .map(|line| line.display)
        .collect::<Vec<_>>();

        assert_eq!(
            lines,
            vec![
                "• Edited src/runtime/agent.rs (+2 -2)",
                "      10  context",
                "      11 -old",
                "      11 +new",
                "         ⋮",
                "      20 -again",
                "      20 +done",
            ]
        );
    }

    /// Verifies readable diff rows wrap to the supplied display width.
    ///
    /// Diff output should follow the same readability cap as other agent output:
    /// wrap at a prior space and indent continuation rows under the diff gutter,
    /// while leaving unbreakable long words for the pane to wrap naturally.
    #[test]
    fn readable_agent_diff_display_lines_wrap_at_spaces_only() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let lines = readable_agent_diff_display_lines_for_width(
            "diff -- update file\n--- a/src/main.rs\n+++ b/src/main.rs\n\
             @@ -1,1 +1,1 @@\n+alpha beta gamma delta epsilon zeta\n\
             +averyveryverylongunbreakabletoken\n",
            &ui_theme,
            32,
        )
        .into_iter()
        .map(|line| line.display)
        .collect::<Vec<_>>();

        assert!(
            lines
                .iter()
                .any(|line| line == "       1 +alpha beta gamma"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "          delta epsilon zeta"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("averyveryverylongunbre")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("akabletoken")),
            "{lines:?}"
        );
    }

    /// Verifies path-only mutation previews are rendered as concise summaries
    /// rather than raw `diff -- delete path` blocks. Directory and missing-path
    /// changes use this preview format instead of unified hunks.
    #[test]
    fn readable_agent_diff_display_lines_parse_path_delta() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let lines = readable_agent_diff_display_lines("diff -- delete path\n- a.txt\n", &ui_theme)
            .into_iter()
            .map(|line| line.display)
            .collect::<Vec<_>>();

        assert_eq!(lines, vec!["• Deleted a.txt (+0 -1)", "         - a.txt"]);
    }

    /// Verifies parsed unified diffs carry syntax token spans for known file
    /// types in addition to the structural diff gutter. This protects the
    /// rendered presentation from regressing to whole-line addition/deletion
    /// coloring once a path provides enough information to pick a syntax.
    #[test]
    fn readable_agent_diff_display_lines_highlight_known_file_type() {
        let mut definition = crate::terminal::builtin_ui_theme_definition("deepforest").unwrap();
        definition
            .colors
            .insert("syntax_type_fg".to_string(), "#010203".to_string());
        let ui_theme = crate::terminal::resolve_ui_theme("syntax-test", definition).unwrap();
        let lines = readable_agent_diff_display_lines(
            "diff -- update file\n--- a/src/main.rs\n+++ b/src/main.rs\n\
             @@ -1,1 +1,1 @@\n-fn old() {}\n+fn new() {}\n",
            &ui_theme,
        );
        let addition = lines
            .iter()
            .find(|line| line.display.contains("+fn new() {}"))
            .unwrap();

        assert!(
            addition
                .style_spans
                .iter()
                .any(|span| span.start >= 10 && span.rendition.foreground.is_some()),
            "{addition:?}"
        );
        assert!(
            addition.style_spans.iter().any(|span| {
                span.start >= 10
                    && matches!(
                        span.rendition.foreground,
                        Some(foreground)
                            if foreground == ui_theme.colors.syntax_keyword.foreground
                                || foreground == ui_theme.colors.syntax_type.foreground
                                || foreground == ui_theme.colors.syntax_function.foreground
                                || foreground == ui_theme.colors.syntax_plain.foreground
                    )
            }),
            "syntax keyword spans should follow the active theme palette: {addition:?}"
        );
    }

    /// Verifies shell command previews use the same theme-backed syntax
    /// highlighter as diff bodies while preserving the existing `$` prompt
    /// prefix. This protects normal command logs from losing syntax spans when
    /// commands are rendered without separate assistant summary lines.
    #[test]
    fn command_preview_terminal_rendered_lines_highlight_shell_syntax() {
        let mut definition = crate::terminal::builtin_ui_theme_definition("deepforest").unwrap();
        definition
            .colors
            .insert("syntax_keyword_fg".to_string(), "#010203".to_string());
        let ui_theme = crate::terminal::resolve_ui_theme("syntax-test", definition).unwrap();
        let lines = command_preview_terminal_rendered_lines(
            "if true; then echo \"ok\"; fi",
            80,
            10,
            crate::agent::ShellClassification::Bash,
            &ui_theme,
        );

        assert_eq!(
            lines
                .iter()
                .map(|line| line.display.as_str())
                .collect::<Vec<_>>(),
            vec!["$ if true; then echo \"ok\"; fi"]
        );
        assert!(lines[0].style_spans.iter().any(|span| {
            span.start >= 2
                && span.rendition.foreground == Some(crate::terminal::TerminalColor::Rgb(1, 2, 3))
        }));
    }

    /// Verifies command previews wrap at a whitespace boundary before the
    /// display limit instead of splitting a word at the exact column. This keeps
    /// shell command logs readable on narrow panes while preserving the existing
    /// prompt prefix and continuation indentation behavior.
    #[test]
    fn command_preview_wraps_at_space_before_boundary() {
        assert_eq!(
            wrap_agent_terminal_text("alpha beta gamma", 12),
            vec!["alpha beta".to_string(), "gamma".to_string()]
        );
    }

    /// Verifies command previews fall back to the exact display boundary when
    /// no whitespace boundary exists before the display limit.
    ///
    /// Word boundaries keep ordinary commands readable, but unbroken text still
    /// needs bounded rows so terminal rendering remains stable.
    #[test]
    fn command_preview_hard_wraps_unbroken_tokens_when_needed() {
        assert_eq!(
            wrap_agent_terminal_text("aaaaaaaaaaaaaaaa", 8),
            vec!["aaaaaaaa".to_string(), "aaaaaaaa".to_string()]
        );
    }

    /// Verifies agent thinking lines wrap to the bounded pane width and indent
    /// continuations after the `thinking:` label. This keeps rationale output
    /// readable without relying on terminal soft wrapping for normal text.
    #[test]
    fn agent_thinking_lines_wrap_with_label_indent() {
        assert_eq!(
            agent_thinking_display_lines_for_width("alpha beta gamma", 18),
            vec![
                "thinking: alpha".to_string(),
                "          beta".to_string(),
                "          gamma".to_string()
            ]
        );
    }

    /// Verifies compact routing records render as terse sentences in
    /// normal agent logs instead of exposing raw key/value fields.
    #[test]
    fn human_readable_display_lines_format_routing_sentence() {
        assert_eq!(
            runtime_human_readable_display_lines(
                "pane=%1 enabled=true default=false changed=true source=runtime-routing"
            ),
            vec!["routing is enabled for pane %1; default is disabled; changed.".to_string()]
        );
    }

    /// Verifies compact runtime-policy records render as direct status
    /// statements so approval changes are easier to scan in the pane log.
    #[test]
    fn human_readable_display_lines_format_policy_sentence() {
        assert_eq!(
            runtime_human_readable_display_lines(
                "field=approval_policy:current=ask:requested=full-access:authority_change=broadening:approval_required=true:approved_by=primary-command:changed=true:source=runtime-policy"
            ),
            vec![
                "approval policy changed from ask to full-access; authority broadening approved by primary-command.".to_string()
            ]
        );
    }

    /// Verifies agent-say copy rows render as a sentence rather than raw
    /// key/value fields with internal runtime source metadata.
    #[test]
    fn human_readable_display_lines_format_agent_say_copy_sentence() {
        assert_eq!(
            runtime_human_readable_display_lines(
                "target=%1:say=written:destination=buffer:buffer=agent-output:turn=turn-3:lines=1:bytes=38:source=runtime-agent-say"
            ),
            vec!["copied 38 bytes from turn-3 to buffer agent-output.".to_string()]
        );
        assert_eq!(
            runtime_human_readable_display_lines(
                "target=%1:say=not-written:reason=no-say-action:source=runtime-agent-say"
            ),
            vec!["agent say text was not copied: no-say-action.".to_string()]
        );
        assert_eq!(
            runtime_human_readable_display_lines(
                "target=%1:say=written:destination=clipboard:buffer=clipboard:turn=turn-3:lines=1:bytes=38:source=runtime-agent-say"
            ),
            vec!["copied 38 bytes from turn-3 to clipboard.".to_string()]
        );
    }

    /// Verifies transcript fork rows render in user terms while preserving the
    /// conversation and pane identifiers needed to reason about where content
    /// moved.
    #[test]
    fn human_readable_display_lines_format_agent_fork_sentence() {
        assert_eq!(
            runtime_human_readable_display_lines(
                "source=17aeaf99 conversation_id=ca770d entries=4 source_pane=%2 pane=%4 forked=true"
            ),
            vec!["forked 4 transcript entries from %2 into %4; conversation ca770d.".to_string()]
        );
    }

    /// Verifies markdown presentation wraps at a prior whitespace boundary and
    /// indents continuation rows after the agent prompt. This protects rendered
    /// markdown from drifting away from the element-aligned wrapping expected
    /// in agent transcript panes.
    #[test]
    fn markdown_presentation_wraps_at_space_with_continuation_indent() {
        let wrapped = wrap_agent_rendered_line_to_width(
            AgentRenderedLine {
                display: "mez> alpha beta gamma".to_string(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: AgentRenderedLineKind::Normal,
            },
            18,
        )
        .into_iter()
        .map(|line| line.display)
        .collect::<Vec<_>>();

        assert_eq!(
            wrapped,
            vec!["mez> alpha beta".to_string(), "     gamma".to_string()]
        );
    }

    /// Verifies markdown presentation preserves an overflowing unbroken token.
    ///
    /// The markdown contract asks non-table prose to avoid inserting hard
    /// splits when there is no usable whitespace boundary, leaving terminal
    /// soft wrapping to handle the long token.
    #[test]
    fn markdown_presentation_preserves_unbroken_token_after_prompt() {
        let wrapped = wrap_agent_rendered_line_to_width(
            AgentRenderedLine {
                display: "mez> aaaaaaaaaaaaaaaa".to_string(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: AgentRenderedLineKind::Normal,
            },
            12,
        )
        .into_iter()
        .map(|line| line.display)
        .collect::<Vec<_>>();

        assert_eq!(wrapped, vec!["mez> aaaaaaaaaaaaaaaa".to_string()]);
    }

    /// Verifies a leading grapheme wider than the segment is made representable.
    ///
    /// A leading two-cell grapheme cannot fit in a one-cell wrapping segment.
    /// The wrapper should consume it with a one-cell placeholder instead of
    /// emitting a row that exceeds the segment before any progress is possible.
    #[test]
    fn markdown_presentation_replaces_overwide_leading_grapheme() {
        let wrapped = wrap_agent_rendered_line_to_width(
            AgentRenderedLine {
                display: "漢abc".to_string(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: AgentRenderedLineKind::Normal,
            },
            1,
        )
        .into_iter()
        .map(|line| line.display)
        .collect::<Vec<_>>();

        assert_eq!(wrapped, vec!["…".to_string(), "abc".to_string()]);
    }

    /// Verifies command overlay markdown keeps internal `mez-agent:` links
    /// selectable without rendering their destination text.
    ///
    /// Saved-session rows use these links for clickable `/resume` commands, but
    /// the visible list should show the bold session UUID rather than a
    /// parenthesized implementation URI.
    #[test]
    fn agent_shell_markdown_overlay_hides_internal_agent_link_destinations() {
        let theme = default_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            "- [**saved-session**](mez-agent:/resume%20saved-session)",
            &theme,
        );

        assert_eq!(content.lines, vec!["• saved-session".to_string()]);
        assert_eq!(content.selections.len(), 1);
        assert_eq!(content.selections[0].command, "/resume saved-session");
        assert_eq!(content.selections[0].start_column, 2);
        assert_eq!(content.selections[0].width, "saved-session".len());
    }

    /// Verifies plain assistant text uses the same prompt-aligned continuation
    /// indentation as markdown output.
    #[test]
    fn plain_agent_output_wraps_under_agent_indicator() {
        let wrapped = wrapped_prefixed_agent_terminal_lines("mez> ", "alpha beta gamma delta", 18)
            .into_iter()
            .map(|line| line.display)
            .collect::<Vec<_>>();

        assert_eq!(
            wrapped,
            vec![
                "mez> alpha beta".to_string(),
                "     gamma delta".to_string()
            ]
        );
    }

    /// Verifies unknown file types still render readable diff rows.
    ///
    /// Syntax highlighting is an enhancement over the structural diff display.
    /// Unsupported extensions should keep the line-number gutter and diff
    /// marker coloring instead of dropping the changed line or panicking while
    /// resolving a syntax.
    #[test]
    fn readable_agent_diff_display_lines_falls_back_for_unknown_file_type() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let lines = readable_agent_diff_display_lines(
            "diff -- update file\n--- a/file.unknown-mez\n+++ b/file.unknown-mez\n\
             @@ -1,1 +1,1 @@\n-old value\n+new value\n",
            &ui_theme,
        );
        let addition = lines
            .iter()
            .find(|line| line.display.contains("+new value"))
            .unwrap();

        assert_eq!(addition.display, "       1 +new value");
        assert!(
            addition.style_spans.iter().all(|span| span.start < 10),
            "{addition:?}"
        );
    }

    /// Verifies command markdown can color compact diff counts.
    ///
    /// `/list-modified-files` emits compact markdown rows with renderer-owned
    /// span classes so additions and removals stay visually distinct without
    /// forcing that command into a bespoke renderer.
    #[test]
    fn command_markdown_renders_modified_file_count_spans() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let lines = render_command_markdown_body_lines(
            "- edited `src/lib.rs` (<span class=\"mez-diff-addition\">+12</span> <span class=\"mez-diff-deletion\">-3</span>)",
            &ui_theme,
        );
        let line = lines
            .iter()
            .find(|line| line.display.contains("+12") && line.display.contains("-3"))
            .unwrap();

        assert!(
            line.style_spans.iter().any(|span| {
                span.rendition.foreground == Some(ui_theme.colors.agent_transcript_user.foreground)
                    && span.rendition.bold
            }),
            "{line:?}"
        );
        assert!(
            line.style_spans.iter().any(|span| {
                span.rendition.foreground == Some(ui_theme.colors.agent_transcript_error.foreground)
                    && span.rendition.bold
            }),
            "{line:?}"
        );
    }

    /// Verifies apply-patch diff previews follow the active theme while keeping
    /// one render's resolved colors stable across the preview.
    ///
    /// This regression protects semantic diff output from borrowing pane-focus
    /// overlays while still requiring the renderer to use the active resolved
    /// transcript and syntax colors for diff gutters and file-aware syntax spans.
    #[test]
    fn readable_agent_diff_display_lines_follow_active_theme_palette() {
        let mut definition = crate::terminal::builtin_ui_theme_definition("deepforest").unwrap();
        definition.colors.insert(
            "agent_transcript_user_fg".to_string(),
            "#010203".to_string(),
        );
        definition.colors.insert(
            "agent_transcript_error_fg".to_string(),
            "#040506".to_string(),
        );
        definition.colors.insert(
            "agent_transcript_status_fg".to_string(),
            "#070809".to_string(),
        );
        definition
            .colors
            .insert("syntax_keyword_fg".to_string(), "#0a0b0c".to_string());
        definition
            .colors
            .insert("syntax_plain_fg".to_string(), "#0d0e0f".to_string());
        definition
            .colors
            .insert("syntax_type_fg".to_string(), "#101112".to_string());
        definition
            .colors
            .insert("syntax_function_fg".to_string(), "#131415".to_string());
        definition
            .colors
            .insert("syntax_operator_fg".to_string(), "#161718".to_string());
        let ui_theme = crate::terminal::resolve_ui_theme("constant-diff-test", definition).unwrap();
        let lines = readable_agent_diff_display_lines(
            "diff -- update file\n--- a/src/main.rs\n+++ b/src/main.rs\n\
             @@ -1,1 +1,1 @@\n-old_value()\n+fn new_value() {}\n",
            &ui_theme,
        );
        let addition = lines
            .iter()
            .find(|line| line.display.contains("+fn new_value() {}"))
            .unwrap();
        let deletion = lines
            .iter()
            .find(|line| line.display.contains("-old_value()"))
            .unwrap();

        assert!(
            addition.style_spans.iter().any(|span| {
                span.start == 0
                    && span.length == 10
                    && span.rendition.foreground
                        == Some(crate::terminal::TerminalColor::Rgb(1, 2, 3))
            }),
            "{addition:?}"
        );
        assert!(
            deletion.style_spans.iter().any(|span| {
                span.start == 0
                    && span.length == 10
                    && span.rendition.foreground
                        == Some(crate::terminal::TerminalColor::Rgb(4, 5, 6))
            }),
            "{deletion:?}"
        );
        assert!(
            addition.style_spans.iter().any(|span| {
                span.start >= 10
                    && matches!(
                        span.rendition.foreground,
                        Some(
                            crate::terminal::TerminalColor::Rgb(10, 11, 12)
                                | crate::terminal::TerminalColor::Rgb(13, 14, 15)
                                | crate::terminal::TerminalColor::Rgb(16, 17, 18)
                                | crate::terminal::TerminalColor::Rgb(19, 20, 21)
                                | crate::terminal::TerminalColor::Rgb(22, 23, 24)
                        )
                    )
            }),
            "{addition:?}"
        );
        assert!(
            addition.style_spans.iter().all(|span| {
                span.start < 10
                    || matches!(
                        span.rendition.foreground,
                        Some(
                            crate::terminal::TerminalColor::Rgb(10, 11, 12)
                                | crate::terminal::TerminalColor::Rgb(13, 14, 15)
                                | crate::terminal::TerminalColor::Rgb(16, 17, 18)
                                | crate::terminal::TerminalColor::Rgb(19, 20, 21)
                                | crate::terminal::TerminalColor::Rgb(22, 23, 24)
                        )
                    )
            }),
            "{addition:?}"
        );
    }

    /// Verifies agent slash markdown shown in the command overlay keeps
    /// `mez-agent:` links selectable after markdown rendering. This preserves
    /// `/list-sessions` resume links while moving informational slash output
    /// out of the pane transcript.
    #[test]
    fn agent_shell_markdown_overlay_preserves_agent_links() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            "- [`saved`](mez-agent:%2Fresume%20saved)",
            &ui_theme,
        );

        assert_eq!(content.command.as_deref(), Some("list-sessions"));
        assert!(
            content
                .lines
                .iter()
                .any(|line| line.contains("saved") && !line.contains("mez-agent:")),
            "{content:?}"
        );
        assert!(
            content
                .selections
                .iter()
                .any(|selection| selection.command == "/resume saved"),
            "{content:?}"
        );
        assert_eq!(
            content
                .selections
                .iter()
                .filter(|selection| selection.command == "/resume saved")
                .count(),
            1,
            "{content:?}"
        );
    }
    /// Verifies selectable pager links keep the markdown link styling emitted
    /// by the CommonMark renderer.
    ///
    /// `/list-sessions` and similar markdown-backed command overlays should
    /// keep links readable as ordinary text links while remaining keyboard and
    /// mouse selectable, so the overlay must retain the rendered line spans in
    /// addition to the selection metadata.
    #[test]
    fn agent_shell_markdown_overlay_preserves_selectable_link_style_spans() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            "- [`saved`](mez-agent:%2Fresume%20saved)",
            &ui_theme,
        );
        assert_eq!(content.selections.len(), 1, "{content:?}");
        let selection = &content.selections[0];
        let line = content.lines.get(selection.line_index).unwrap();
        let column = runtime_display_overlay_rendered_selection_start(
            &RuntimeDisplayOverlay {
                lines: content.lines.clone(),
                line_style_spans: content.line_style_spans.clone(),
                scroll_offset: 0,
                selections: content.selections.clone(),
                active_selection_index: Some(0),
                dismiss_on_any_input: false,
                search_input: None,
                search_query: None,
                search_match: None,
                search_status: None,
                mouse_selection: None,
            },
            selection,
        );
        assert_eq!(&line[column..column + selection.width], "saved");
        assert!(
            content.line_style_spans[selection.line_index]
                .iter()
                .any(|span| {
                    span.start == selection.start_column
                        && span.length == selection.width
                        && span.rendition.bold
                        && span.rendition.underline
                        && !span.rendition.inverse
                        && span.rendition.background.is_none()
                        && span.rendition.foreground
                            == Some(ui_theme.colors.agent_transcript_command.foreground)
                }),
            "{content:?}"
        );
    }
    /// Verifies an active pager link keeps link styling on every rendered cell.
    ///
    /// Selected command-overlay links layer selector and markdown spans on the
    /// same columns. The final rendered row must preserve the markdown link
    /// rendition through the last link character instead of letting the
    /// fallback selection span leak onto the tail cell.
    #[test]
    fn active_markdown_overlay_link_keeps_tail_cell_link_styling() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            "- [`saved`](mez-agent:%2Fresume%20saved)",
            &ui_theme,
        );
        let overlay = RuntimeDisplayOverlay {
            lines: content.lines.clone(),
            line_style_spans: content.line_style_spans.clone(),
            scroll_offset: 0,
            selections: content.selections.clone(),
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
        };
        let selection = &overlay.selections[0];
        let start = runtime_display_overlay_rendered_selection_start(&overlay, selection);
        let spans = runtime_display_overlay_rendered_line_style_spans(&overlay, 0, 80, &ui_theme);
        for column in start..start.saturating_add(selection.width) {
            let rendition = rendered_line_rendition_at(&spans, column);
            assert!(
                rendition.bold,
                "column {column} lost bold styling: {spans:?}"
            );
            assert!(
                rendition.underline,
                "column {column} lost underline styling: {spans:?}"
            );
            assert!(
                !rendition.inverse,
                "column {column} became inverse: {spans:?}"
            );
            assert_eq!(
                rendition.background,
                Some(ui_theme.colors.agent_model.background),
                "column {column} lost active selection background: {spans:?}"
            );
            assert_eq!(
                rendition.foreground,
                Some(ui_theme.colors.agent_transcript_command.foreground),
                "column {column} lost link foreground: {spans:?}"
            );
        }
    }
    /// Verifies an active saved-session UUID row keeps link styling on the
    /// final visible UUID character.
    ///
    /// `/list-sessions` rows are emitted as hidden `mez-agent:` resume links
    /// with bold UUID labels. The command overlay must preserve that link
    /// rendition across the full visible UUID when the row is selected,
    /// including the final character that previously fell back to plain text.
    #[test]
    fn active_saved_session_overlay_uuid_keeps_tail_cell_link_styling() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            &format!("- [**{session_id}**](mez-agent:%2Fresume%20{session_id})"),
            &ui_theme,
        );
        let overlay = RuntimeDisplayOverlay {
            lines: content.lines.clone(),
            line_style_spans: content.line_style_spans.clone(),
            scroll_offset: 0,
            selections: content.selections.clone(),
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
        };
        let selection = &overlay.selections[0];
        let start = runtime_display_overlay_rendered_selection_start(&overlay, selection);
        let spans = runtime_display_overlay_rendered_line_style_spans(&overlay, 0, 120, &ui_theme);
        for column in start..start.saturating_add(selection.width) {
            let rendition = rendered_line_rendition_at(&spans, column);
            assert!(
                rendition.bold,
                "column {column} lost bold styling: {spans:?}"
            );
            assert!(
                rendition.underline,
                "column {column} lost underline styling: {spans:?}"
            );
            assert!(
                !rendition.inverse,
                "column {column} became inverse: {spans:?}"
            );
            assert_eq!(
                rendition.background,
                Some(ui_theme.colors.agent_model.background),
                "column {column} lost active selection background: {spans:?}"
            );
            assert_eq!(
                rendition.foreground,
                Some(ui_theme.colors.agent_transcript_command.foreground),
                "column {column} lost link foreground: {spans:?}"
            );
        }
    }

    /// Verifies the active selector gutter stays isolated from a link that
    /// begins at the first visible body column.
    ///
    /// `/status` renders some selectable links without a list-prefix gap. When
    /// the active row's selector gutter abuts that first link cell, the gutter
    /// must remain a standalone styled cell so the link highlight does not
    /// visually shift left into the gutter column.
    #[test]
    fn active_markdown_overlay_front_of_line_link_keeps_gutter_separate() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("status".to_string()),
            "[`saved`](mez-agent:%2Fresume%20saved)",
            &ui_theme,
        );
        let overlay = RuntimeDisplayOverlay {
            lines: content.lines.clone(),
            line_style_spans: content.line_style_spans.clone(),
            scroll_offset: 0,
            selections: content.selections.clone(),
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
        };
        let selection = &overlay.selections[0];
        let start = runtime_display_overlay_rendered_selection_start(&overlay, selection);
        let spans = runtime_display_overlay_rendered_line_style_spans(&overlay, 0, 80, &ui_theme);
        assert_eq!(
            start,
            runtime_display_overlay_selection_prefix_columns(),
            "{spans:?}"
        );
        assert!(
            spans.iter().any(|span| {
                span.start == 0 && span.length == runtime_display_overlay_selection_prefix_columns()
            }),
            "missing isolated selector gutter span: {spans:?}"
        );
        let gutter_rendition = rendered_line_rendition_at(&spans, 0);
        let gutter_trailing_rendition = rendered_line_rendition_at(&spans, start - 1);
        let first_link_rendition = rendered_line_rendition_at(&spans, start);
        assert_eq!(
            gutter_rendition.foreground,
            Some(ui_theme.colors.agent_model.foreground),
            "gutter lost selection styling: {spans:?}"
        );
        assert_eq!(
            gutter_trailing_rendition.foreground,
            Some(ui_theme.colors.agent_model.foreground),
            "selector gutter trailing cell lost selection styling: {spans:?}"
        );
        assert_eq!(
            first_link_rendition.foreground,
            Some(ui_theme.colors.agent_transcript_command.foreground),
            "front-of-line link styling shifted into the gutter: {spans:?}"
        );
        assert!(
            gutter_rendition.underline,
            "gutter lost underline: {spans:?}"
        );
        assert!(
            gutter_trailing_rendition.underline,
            "selector gutter trailing cell lost underline: {spans:?}"
        );
        assert!(
            first_link_rendition.underline,
            "front-of-line link lost underline: {spans:?}"
        );
    }

    /// Verifies pager search highlighting is limited to the matched range.
    ///
    /// Search state stores a concrete body-column range instead of just the
    /// matching line, so rendering should style only the submitted match and
    /// leave surrounding text with its original body/link rendition.
    #[test]
    fn display_overlay_search_highlights_only_matching_columns() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let link_rendition = GraphicRendition {
            underline: true,
            foreground: Some(ui_theme.colors.agent_transcript_command.foreground),
            ..GraphicRendition::default()
        };
        let overlay = RuntimeDisplayOverlay {
            lines: vec!["prefix needle suffix".to_string()],
            line_style_spans: vec![vec![TerminalStyleSpan {
                start: 0,
                length: 20,
                rendition: link_rendition,
            }]],
            scroll_offset: 0,
            selections: Vec::new(),
            active_selection_index: None,
            dismiss_on_any_input: false,
            search_input: None,
            search_query: Some("needle".to_string()),
            search_match: Some(RuntimeDisplayOverlaySearchMatch {
                line_index: 0,
                start_column: 7,
                width: 6,
            }),
            search_status: None,
            mouse_selection: None,
        };

        let spans = runtime_display_overlay_rendered_line_style_spans(&overlay, 0, 80, &ui_theme);
        let before_match = rendered_line_rendition_at(&spans, 6);
        let first_match = rendered_line_rendition_at(&spans, 7);
        let final_match = rendered_line_rendition_at(&spans, 12);
        let after_match = rendered_line_rendition_at(&spans, 13);

        assert_eq!(
            before_match.foreground,
            Some(ui_theme.colors.agent_transcript_command.foreground),
            "style before match was overwritten: {spans:?}"
        );
        assert!(
            before_match.underline,
            "style before match lost underline: {spans:?}"
        );
        assert_eq!(
            first_match,
            ui_theme.colors.copy_selection.rendition(),
            "first match cell was not highlighted: {spans:?}"
        );
        assert_eq!(
            final_match,
            ui_theme.colors.copy_selection.rendition(),
            "final match cell was not highlighted: {spans:?}"
        );
        assert_eq!(
            after_match.foreground,
            Some(ui_theme.colors.agent_transcript_command.foreground),
            "style after match was overwritten: {spans:?}"
        );
        assert!(
            after_match.underline,
            "style after match lost underline: {spans:?}"
        );
    }

    /// Verifies pager search highlighting skips matches outside the visible row.
    ///
    /// A match range past the clipped viewport should not emit a fallback row
    /// highlight, otherwise the visible text appears to match a query that is
    /// actually off-screen.
    #[test]
    fn display_overlay_search_skips_offscreen_match_ranges() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let overlay = RuntimeDisplayOverlay {
            lines: vec!["visible text then hidden needle".to_string()],
            line_style_spans: vec![Vec::new()],
            scroll_offset: 0,
            selections: Vec::new(),
            active_selection_index: None,
            dismiss_on_any_input: false,
            search_input: None,
            search_query: Some("needle".to_string()),
            search_match: Some(RuntimeDisplayOverlaySearchMatch {
                line_index: 0,
                start_column: 25,
                width: 6,
            }),
            search_status: None,
            mouse_selection: None,
        };

        let spans = runtime_display_overlay_rendered_line_style_spans(&overlay, 0, 12, &ui_theme);

        assert!(
            spans
                .iter()
                .all(|span| span.rendition != ui_theme.colors.copy_selection.rendition()),
            "off-screen match produced a visible highlight: {spans:?}"
        );
    }

    /// Verifies `/list-sessions` only linkifies the first visible occurrence of
    /// a saved conversation id.
    ///
    /// The markdown source keeps a hidden `mez-agent:` resume link on the
    /// session row. If the same UUID-like id appears again in explanatory text,
    /// that later occurrence should remain plain text so keyboard and mouse
    /// navigation expose one selection per logical session.
    #[test]
    fn agent_shell_markdown_overlay_linkifies_each_session_id_once() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            "- [`018f6b3a-1b2c-7000-9000-cafebabefeed`](mez-agent:%2Fresume%20018f6b3a-1b2c-7000-9000-cafebabefeed)\n  - Resume: `/resume 018f6b3a-1b2c-7000-9000-cafebabefeed`",
            &ui_theme,
        );

        assert_eq!(
            content
                .selections
                .iter()
                .filter(|selection| {
                    selection.command == "/resume 018f6b3a-1b2c-7000-9000-cafebabefeed"
                })
                .count(),
            1,
            "{content:?}"
        );
        assert_eq!(content.selections[0].line_index, 0);
    }

    /// Verifies hidden markdown command links are mapped to their rendered
    /// occurrence instead of an earlier duplicate plain-text label.
    ///
    /// Command-overlay markdown hides `mez-agent:` destinations, so selectable
    /// metadata must be derived from the source/rendered row pair. A plain text
    /// occurrence before the actual markdown link should not receive link
    /// styling or become the mouse target for the hidden command.
    #[test]
    fn agent_shell_markdown_overlay_maps_hidden_links_to_exact_rendered_occurrence() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("status".to_string()),
            "saved before [`saved`](mez-agent:%2Fresume%20saved)",
            &ui_theme,
        );

        assert_eq!(content.lines, vec!["saved before saved".to_string()]);
        assert_eq!(content.selections.len(), 1, "{content:?}");
        let selection = &content.selections[0];
        assert_eq!(selection.command, "/resume saved");
        assert_eq!(selection.line_index, 0);
        assert_eq!(selection.start_column, "saved before ".len());
        assert_eq!(selection.width, "saved".len());
        assert!(
            content.line_style_spans[0]
                .iter()
                .all(|span| span.start != 0),
            "earlier duplicate text received link styling: {content:?}"
        );
    }

    /// Verifies single-link overlay mouse hit testing remains column bounded.
    ///
    /// Rows with one selectable command still contain inert gutter, whitespace,
    /// and descriptive text. Mouse selection should execute only clicks inside
    /// the advertised choice range, matching multi-chip rows.
    #[test]
    fn display_overlay_single_selection_hit_testing_requires_link_bounds() {
        let overlay = RuntimeDisplayOverlay {
            lines: vec!["text before [open] after".to_string()],
            line_style_spans: vec![Vec::new()],
            scroll_offset: 0,
            selections: vec![RuntimeDisplayOverlaySelection {
                line_index: 0,
                start_column: "text before ".len(),
                width: "[open]".len(),
                command: "/open".to_string(),
                kind: RuntimeDisplayOverlaySelectionKind::Primary,
            }],
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
        };
        let rendered_start =
            runtime_display_overlay_rendered_selection_start(&overlay, &overlay.selections[0]);

        assert_eq!(
            super::runtime_display_overlay_selection_index_at_position(&overlay, 0, 0),
            None
        );
        assert_eq!(
            super::runtime_display_overlay_selection_index_at_position(
                &overlay,
                0,
                rendered_start.saturating_add(1),
            ),
            Some(0)
        );
    }

    /// Verifies scrolling moves the active command selection to the visible
    /// viewport before Enter can execute it.
    ///
    /// Mouse-wheel and page-scroll paths should not leave keyboard execution
    /// armed on an off-screen action after the overlay viewport changes.
    #[test]
    fn display_overlay_scroll_keeps_active_selection_visible() {
        let mut overlay = RuntimeDisplayOverlay {
            lines: vec![
                "first".to_string(),
                "plain".to_string(),
                "also plain".to_string(),
                "second".to_string(),
                "tail".to_string(),
            ],
            line_style_spans: vec![Vec::new(); 5],
            scroll_offset: 0,
            selections: vec![
                RuntimeDisplayOverlaySelection {
                    line_index: 0,
                    start_column: 0,
                    width: 5,
                    command: "/first".to_string(),
                    kind: RuntimeDisplayOverlaySelectionKind::Primary,
                },
                RuntimeDisplayOverlaySelection {
                    line_index: 3,
                    start_column: 0,
                    width: 6,
                    command: "/second".to_string(),
                    kind: RuntimeDisplayOverlaySelectionKind::Primary,
                },
            ],
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
        };

        assert!(super::apply_display_overlay_scroll_delta(
            &mut overlay,
            3,
            Size::new(80, 4).unwrap(),
        ));
        assert_eq!(overlay.scroll_offset, 3);
        assert_eq!(overlay.active_selection_index, Some(1));
    }

    /// Verifies compact colon-delimited command display records render as
    /// readable one-line rows for terminal overlays while preserving the
    /// exact field values that users may need to copy into follow-up commands.
    #[test]
    fn human_readable_display_lines_format_colon_delimited_records() {
        let lines = runtime_human_readable_display_lines(
            "theme=kanagawa:source=builtin:active=true\nkey=C-a x:source=runtime-config:command=split-window -h",
        );

        assert_eq!(
            lines,
            vec![
                "theme: kanagawa | source: builtin | active: yes",
                "key: C-a x | source: runtime-config | command: split-window -h",
            ]
        );
    }

    /// Verifies compact display rows that include a non-key prefix keep the
    /// prefix as the first compact row segment. This covers
    /// selectors such as window, pane, and group lists whose first columns are
    /// positional identifiers rather than named fields.
    #[test]
    fn human_readable_display_lines_preserve_non_key_prefixes() {
        let lines = runtime_human_readable_display_lines(
            "0:g1:work:active=false:windows=2:action=select-group -t g1",
        );

        assert_eq!(
            lines,
            vec!["actions: [select] | 0 g1 work | active: no | windows: 2"]
        );
    }

    /// Verifies multi-action chooser records render as compact action chips.
    /// This is important for command rows such as `choose-buffer`, where a
    /// single item row may expose both a routine paste action and a destructive
    /// delete action.
    #[test]
    fn human_readable_display_lines_format_multiple_action_chips() {
        let lines = runtime_human_readable_display_lines(
            "buffer=main:bytes=5:origin=test:preview=hello:actions=paste-buffer -b main,delete-buffer main",
        );

        assert_eq!(
            lines,
            vec![
                "actions: [paste] [delete] | buffer: main | bytes: 5 | origin: test | preview: hello"
            ]
        );
    }

    /// Verifies descriptive action metadata is not promoted to an executable
    /// selector. Auth and status records often use `action=` to describe state,
    /// and those labels must remain readable text rather than interactive
    /// command choices.
    #[test]
    fn command_display_overlay_ignores_descriptive_action_metadata() {
        let body = serde_json::json!({
            "outcomes": [{
                "kind": "display",
                "body": "provider=openai method=browser action=interactive-required reason=run-auth source=auth-store"
            }]
        })
        .to_string();
        let content = runtime_command_display_overlay_content(&body).unwrap();

        assert!(content.selections.is_empty());
        assert_eq!(
            content.lines,
            vec![
                "provider: openai | method: browser | action: interactive-required | reason: run-auth | source: auth-store"
            ]
        );
    }

    /// Verifies non-field help and prose text pass through unchanged. The
    /// humanizer is intentionally narrow so command guides, errors, and shell
    /// output are not reformatted merely because they contain punctuation.
    #[test]
    fn human_readable_display_lines_leave_plain_text_unchanged() {
        let lines = runtime_human_readable_display_lines(
            "mezzanine command help\n  split-window          Split the active pane.",
        );

        assert_eq!(
            lines,
            vec![
                "mezzanine command help",
                "  split-window          Split the active pane.",
            ]
        );
    }

    /// Verifies space-delimited runtime status rows are also displayed as one
    /// readable row when every token is a compact key/value pair.
    #[test]
    fn human_readable_display_lines_format_space_delimited_records() {
        let lines = runtime_human_readable_display_lines(
            "approval_policy=ask source=runtime-policy bypass=false",
        );

        assert_eq!(
            lines,
            vec!["approval policy: ask | source: runtime-policy | bypass: no"]
        );
    }
}
