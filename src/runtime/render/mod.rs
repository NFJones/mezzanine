//! Runtime Render implementation.
//!
//! This module owns the runtime render boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use mez_mux::input::{
    GroupFocusTarget, MouseBorderCell, MousePaneRegion, MouseWindowFrameCell, MuxAction,
    PasteBufferTarget, WindowFocusTarget, key_chord_input_bytes,
};
#[cfg(test)]
use mez_mux::overlay::{
    OVERLAY_ACTIVE_SELECTOR as DISPLAY_OVERLAY_ACTIVE_SELECTOR,
    OVERLAY_INACTIVE_SELECTOR as DISPLAY_OVERLAY_INACTIVE_SELECTOR,
};
use mez_mux::overlay::{
    OverlaySelection, OverlaySelectionKind, apply_overlay_scroll_delta, clamp_overlay_scroll,
    overlay_copy_selection, overlay_footer, overlay_line_prefix_columns, overlay_link_rendition,
    overlay_next_search_match, overlay_render_lines, overlay_rendered_line_style_spans,
    overlay_rendered_selection_start, overlay_selection_index_at_position,
    overlay_selection_index_is_visible, overlay_selection_rendition, overlay_text_at,
    scroll_overlay_to_line,
};
use mez_mux::presentation::{
    pane_content_size_for_geometry, pane_frame_merges_into_divider,
    pane_render_region_size_for_geometry, rendered_window_body_size,
};
use mez_mux::render::{modal_overlay_max_scroll, modal_overlay_page_rows};

use super::service_state::{
    RunningShellTransactionKind, RuntimeDisplayOverlay, RuntimeMouseClickState,
    RuntimePaneAgentStatusSelector, RuntimePrimaryPromptInput, RuntimeRecordBrowserOverlayFrame,
    RuntimeRecordBrowserOverlaySource,
};
use super::{
    AgentShellVisibility, AgentTurnRecord, AgentTurnState, AttachedClientStepApplication,
    AttachedTerminalClientStepPlan, ClientViewRole, CopyMode, CopyModeKeyAction, EventKind,
    MezError, MouseAction, MouseResizeDragState, MouseSelectionDragState,
    MouseWindowActionFrameCell, ObserverDecisionState, PaneDescriptor, PaneGeometry,
    PaneInputDispatch, PaneNavigationDirection, ReadlineInputDecoder, ReadlineOutcome,
    ReadlinePrompt, ReadlinePromptKind, RenderedClientView, Result,
    RuntimeAgentModifiedFileSummary, RuntimeAgentPromptInput, RuntimeSessionService,
    RuntimeSideEffect, Size, SplitDirection, TerminalClientLoopAction, TerminalClientLoopConfig,
    TerminalFrameContext, TerminalScreen, WindowFrameAction, agent_prompt_reserved_line_count,
    current_unix_millis, current_unix_seconds, json_escape, mouse_action_name,
    mux_action_command_prompt_prefill, mux_action_name, pane_navigation_direction,
    parse_command_sequence, render_attached_client_view, rendered_pane_geometries,
    runtime_agent_shell_command_response_json, runtime_agent_turn_duration_display,
    runtime_agent_turn_state_name, runtime_approval_policy_name, runtime_copy_position_for_view,
    runtime_fit_status_line, runtime_paste_bytes, window_frame_action_pillbox_cells,
    window_frame_pillbox_cells,
};
/// Maximum elapsed time between two pane-content clicks recognized as a double click.
const DOUBLE_CLICK_WORD_SELECTION_WINDOW_MS: u64 = 500;
/// How long the copied-word highlight remains visible after a double click.
const DOUBLE_CLICK_WORD_SELECTION_HIGHLIGHT_MS: u64 = 500;

/// Owns transient client interaction state used by product presentation.
///
/// Fields are private to the render component and its descendants. Other
/// runtime components cross this boundary through narrow methods instead of
/// reaching into the session coordinator's former shared field bag.
#[derive(Debug, Default)]
pub(in crate::runtime) struct RuntimePresentationComponent {
    /// Submitted command-prompt history retained across prompt openings.
    primary_command_prompt_history: Vec<String>,
    /// Active primary-client readline prompt, when one is open.
    primary_prompt_input: Option<RuntimePrimaryPromptInput>,
    /// Whether the primary client's next key uses the prefix table.
    primary_prefix_key_pending: bool,
    /// Active primary-client modal display overlay.
    primary_display_overlay: Option<RuntimeDisplayOverlay>,
    /// Typed record browsers waiting for display-response presentation.
    pending_record_browser_overlays:
        std::collections::BTreeMap<(String, String), mez_mux::record_browser::RecordBrowser>,
    /// Query sources waiting to accompany pending record browsers.
    pending_record_browser_overlay_sources:
        std::collections::BTreeMap<(String, String), RuntimeRecordBrowserOverlaySource>,
    /// Parent browser views waiting to accompany pending child views.
    pending_record_browser_overlay_stacks:
        std::collections::BTreeMap<(String, String), Vec<RuntimeRecordBrowserOverlayFrame>>,
    mouse_resize_drag_state: Option<MouseResizeDragState>,
    mouse_selection_drag_state: Option<MouseSelectionDragState>,
    last_mouse_click_state: Option<RuntimeMouseClickState>,
    deferred_word_copy_cleanup: std::cell::RefCell<Option<(String, CopyMode, u64)>>,
    pressed_window_action: Option<WindowFrameAction>,
    primary_error_status_overlay: Option<String>,
    pane_agent_status_selector: Option<RuntimePaneAgentStatusSelector>,
}

impl RuntimePresentationComponent {
    /// Clears an in-progress pane-resize gesture after layout mutation.
    pub(in crate::runtime) fn clear_mouse_resize_drag_state(&mut self) {
        self.mouse_resize_drag_state = None;
    }
}

impl RuntimeSessionService {
    /// Registers typed browser state for a later agent-shell display response.
    pub(in crate::runtime) fn register_pending_record_browser_overlay(
        &mut self,
        pane_id: &str,
        command: &str,
        browser: mez_mux::record_browser::RecordBrowser,
        source: Option<RuntimeRecordBrowserOverlaySource>,
    ) {
        let key = (pane_id.to_string(), command.to_string());
        if let Some(source) = source {
            self.presentation
                .pending_record_browser_overlay_sources
                .insert(key.clone(), source);
        }
        self.presentation
            .pending_record_browser_overlays
            .insert(key, browser);
    }
}

#[cfg(test)]
impl RuntimeSessionService {
    /// Returns retained primary command-prompt history for integration tests.
    pub(in crate::runtime) fn primary_command_prompt_history(&self) -> &[String] {
        &self.presentation.primary_command_prompt_history
    }

    /// Replaces retained command-prompt history for an integration fixture.
    pub(in crate::runtime) fn set_primary_command_prompt_history_for_tests(
        &mut self,
        history: Vec<String>,
    ) {
        self.presentation.primary_command_prompt_history = history;
    }

    /// Adds one command-prompt history entry for an integration fixture.
    pub(in crate::runtime) fn push_primary_command_prompt_history_for_tests(
        &mut self,
        command: String,
    ) {
        self.presentation
            .primary_command_prompt_history
            .push(command);
    }

    /// Returns the active primary prompt for product integration tests.
    pub(in crate::runtime) fn primary_prompt_input(&self) -> Option<&RuntimePrimaryPromptInput> {
        self.presentation.primary_prompt_input.as_ref()
    }

    /// Reports whether the primary client is waiting for a prefix-table key.
    pub(in crate::runtime) fn primary_prefix_key_pending(&self) -> bool {
        self.presentation.primary_prefix_key_pending
    }

    /// Returns the active primary display overlay for product integration tests.
    pub(in crate::runtime) fn primary_display_overlay(&self) -> Option<&RuntimeDisplayOverlay> {
        self.presentation.primary_display_overlay.as_ref()
    }

    /// Replaces a pending record browser's parent stack for a test fixture.
    pub(in crate::runtime) fn set_pending_record_browser_overlay_stack_for_tests(
        &mut self,
        pane_id: &str,
        command: &str,
        stack: Vec<RuntimeRecordBrowserOverlayFrame>,
    ) {
        self.presentation
            .pending_record_browser_overlay_stacks
            .insert((pane_id.to_string(), command.to_string()), stack);
    }

    /// Reports whether any typed record browser still awaits presentation.
    pub(in crate::runtime) fn pending_record_browser_overlays_is_empty(&self) -> bool {
        self.presentation.pending_record_browser_overlays.is_empty()
    }

    /// Returns the transient primary error status for product integration tests.
    pub(in crate::runtime) fn primary_error_status_overlay(&self) -> Option<&str> {
        self.presentation.primary_error_status_overlay.as_deref()
    }

    /// Returns the active pane-agent selector for product integration tests.
    pub(in crate::runtime) fn pane_agent_status_selector(
        &self,
    ) -> Option<&RuntimePaneAgentStatusSelector> {
        self.presentation.pane_agent_status_selector.as_ref()
    }

    /// Returns deferred copied-word cleanup state for product integration tests.
    pub(in crate::runtime) fn deferred_word_copy_cleanup(
        &self,
    ) -> &std::cell::RefCell<Option<(String, CopyMode, u64)>> {
        &self.presentation.deferred_word_copy_cleanup
    }
}

use crate::command::baseline_commands;
use crate::selector::{SelectorExtraCandidate, SelectorSurface};
use crate::terminal::{
    MousePaneAgentSelectorCell, MousePaneAgentStatusCell, PaneAgentStatusField,
    WindowFrameCommandKind, compose_modal_display_overlay_lines,
    compose_prompt_overlay_presentation_with_styles, pane_frame_agent_status_pillbox_cells,
    window_group_frame_pillbox_cells,
};
use crate::transcript::AgentPresentationEntry;
use mez_agent::mcp::McpServerStatus;
use mez_agent::{
    AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE, ActionResult, agent_output_content_type_is_diff,
    agent_output_content_type_is_markdown,
};
use mez_mux::attached_client::mouse_border_cells_for_geometries;
use mez_mux::copy::CopyPosition;
use mez_mux::presentation::{
    TerminalFramePosition, TerminalPaneFrameContext, TerminalWindowFrameContext,
    TerminalWindowGroupFrameContext, TerminalWindowStatusContext,
};
use mez_mux::readline::DEFAULT_READLINE_HISTORY_LIMIT;
use mez_mux::selector::{SelectorCandidate, SelectorCandidateKind};
use mez_mux::theme::UiTheme;
use mez_terminal::{
    GraphicRendition, TerminalStyleSpan, TerminalStyledLine,
    active_terminal_text_width as terminal_text_width,
};

mod attached_step;
mod client_view;
mod copy_mode;
mod input;
mod mouse;
mod mux;
mod overlay;
mod paste;
mod presentation;
mod time;

use input::{
    RuntimeDisplayOverlayInputAction, RuntimeSelectorInputAction,
    runtime_display_overlay_input_action, runtime_selector_input_action,
    runtime_selector_step_index,
};
use mez_mux::render::{RichTextLine, push_or_extend_style_span, terminal_color_luminance};
#[cfg(test)]
use mez_mux::render::{RichTextLineKind, wrap_rich_text_line_to_width};
use overlay::{
    RuntimeAgentShellDisplayOutput, agent_command_link_at_line_column,
    agent_shell_mcp_display_state_name, default_runtime_agent_prompt_input,
    runtime_agent_shell_display_output, runtime_agent_shell_visibility,
    runtime_command_display_overlay_content, runtime_command_display_should_open_overlay,
    runtime_pane_agent_selector_rendition, runtime_pane_agent_status_selector_keep_active_visible,
    runtime_pane_agent_status_selector_layout, runtime_primary_prompt_input, runtime_selector_line,
};
#[cfg(test)]
use overlay::{runtime_agent_shell_markdown_overlay_content, runtime_human_readable_display_lines};
use presentation::{
    AgentTerminalPresentationStyle, agent_display_lines_are_error,
    agent_display_lines_are_low_level_status, agent_prompt_error_display_lines,
    overlay_styled_lines, render_command_markdown_body_lines, sanitized_agent_terminal_line,
    wrap_agent_terminal_text,
};
#[cfg(test)]
use presentation::{
    agent_action_execution_display_header, agent_action_result_uses_diff_preview,
    agent_thinking_display_lines_for_width, command_preview_terminal_rendered_lines,
    readable_agent_diff_display_lines, readable_agent_diff_display_lines_for_width,
    rendered_line_rendition_at, wrapped_prefixed_agent_terminal_lines,
};
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

#[cfg(test)]
mod tests;
