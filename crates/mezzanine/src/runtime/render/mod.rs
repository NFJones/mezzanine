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
    overlay_render_lines, overlay_rendered_line_style_spans, overlay_rendered_selection_start,
    overlay_selection_index_at_position, overlay_selection_rendition, overlay_text_at,
};
use mez_mux::render::{modal_overlay_max_scroll, modal_overlay_page_rows};

use super::service_state::{
    RunningShellTransactionKind, RuntimeDisplayOverlay, RuntimeMouseClickState,
    RuntimePaneAgentStatusSelector, RuntimePrimaryPromptInput, RuntimeRecordBrowserOverlayFrame,
    RuntimeRecordBrowserOverlaySource,
};
use super::{
    AgentShellVisibility, AgentTurnRecord, AgentTurnState, AttachedClientStepApplication,
    AttachedTerminalClientStepPlan, ClientViewRole, ClipboardEffectIntent, ClipboardPasteSource,
    ClipboardPasteSourceKind, ClipboardPolicy, ClipboardWritePlan, CopyMode, CopyModeKeyAction,
    EffectiveConfig, EventKind, HostClipboard, KeyBindings, KeyChord, MezError, MouseAction,
    MouseResizeDragState, MouseSelectionDragState, MouseWindowActionFrameCell,
    ObserverDecisionState, PaneDescriptor, PaneInputDispatch, PaneNavigationDirection,
    PasteBuffers, ReadlineInputDecoder, ReadlineOutcome, ReadlinePrompt, ReadlinePromptKind,
    RenderedClientView, Result, RuntimeAgentPromptInput, RuntimeCommandBinding,
    RuntimeSessionService, RuntimeSideEffect, RuntimeStatusPillCache, RuntimeStatusPillDefinition,
    Size, SplitDirection, TerminalClientLoopAction, TerminalClientLoopConfig, TerminalFrameContext,
    TerminalScreen, WindowFrameAction, agent_prompt_reserved_line_count, current_unix_millis,
    current_unix_seconds, json_escape, mouse_action_name, mux_action_command_prompt_prefill,
    mux_action_name, pane_navigation_direction, parse_command_sequence,
    render_attached_client_view, runtime_agent_shell_command_response_json,
    runtime_agent_turn_duration_display, runtime_agent_turn_state_name,
    runtime_approval_policy_name, runtime_copy_position_for_view, runtime_fit_status_line,
    runtime_paste_bytes, select_clipboard_paste_source, window_frame_action_pillbox_cells,
    window_frame_pillbox_cells,
};
/// Maximum elapsed time between two pane-content clicks recognized as a double click.
const DOUBLE_CLICK_WORD_SELECTION_WINDOW_MS: u64 = 500;
/// How long the copied-word highlight remains visible after a double click.
const DOUBLE_CLICK_WORD_SELECTION_HIGHLIGHT_MS: u64 = 500;

/// Immutable presentation configuration replaced atomically on config reload.
///
/// Parsing builds a complete value before the live component changes, so an
/// invalid option cannot leave cursor, frame-status, or render pacing policy
/// partially updated.
#[derive(Debug)]
pub(crate) struct RuntimePresentationSettings {
    /// Whether window frame rows are rendered.
    window_frames_enabled: bool,
    /// Window frame template rendered around each visible window.
    window_frame_template: String,
    /// Template rendered at the right side of a window frame.
    window_frame_right_status_template: String,
    /// Command-backed window status pill definitions keyed by pill name.
    window_status_pill_definitions: std::collections::BTreeMap<String, RuntimeStatusPillDefinition>,
    /// Placement of the window frame row.
    window_frame_position: TerminalFramePosition,
    /// Visual treatment of the window frame row.
    window_frame_style: TerminalFrameStyle,
    /// Window fields eligible for template expansion.
    window_frame_visible_fields: Vec<String>,
    /// Whether pane frame rows are rendered.
    pane_frames_enabled: bool,
    /// Pane frame template rendered around each visible pane.
    pane_frame_template: String,
    /// Placement of pane frame rows.
    pane_frame_position: TerminalFramePosition,
    /// Visual treatment of pane frame rows.
    pane_frame_style: TerminalFrameStyle,
    /// Pane fields eligible for template expansion.
    pane_frame_visible_fields: Vec<String>,
    /// Cursor shape used for the focused terminal client.
    terminal_cursor_style: mez_mux::presentation::TerminalCursorStyle,
    /// Whether the focused terminal cursor blinks.
    terminal_cursor_blink: bool,
    /// Cursor blink interval in milliseconds.
    terminal_cursor_blink_interval_ms: u64,
    /// Resize-event debounce interval in milliseconds.
    terminal_resize_debounce_ms: u64,
    /// Maximum attached-client render frequency.
    terminal_render_rate_limit_fps: u64,
    /// Maximum display width for product-owned agent rows.
    terminal_agent_wrap_column_cap: usize,
    /// Whether optional terminal animation is disabled.
    terminal_reduced_motion: bool,
    /// Resolved color and rendition policy for product UI surfaces.
    ui_theme: UiTheme,
    /// Configured mux key chords.
    key_bindings: KeyBindings,
    /// Configured prefix-table command bindings keyed by chord.
    command_bindings: std::collections::BTreeMap<KeyChord, RuntimeCommandBinding>,
    /// Clipboard policy used for OSC 52 terminal writes.
    terminal_clipboard: ClipboardPolicy,
}

impl Default for RuntimePresentationSettings {
    fn default() -> Self {
        Self {
            window_frames_enabled: true,
            window_frame_template: crate::host::terminal::DEFAULT_WINDOW_FRAME_TEMPLATE.to_string(),
            window_frame_right_status_template:
                crate::host::terminal::DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string(),
            window_status_pill_definitions: std::collections::BTreeMap::new(),
            window_frame_position: TerminalFramePosition::Bottom,
            window_frame_style: TerminalFrameStyle::Default,
            window_frame_visible_fields: crate::host::terminal::DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS
                .iter()
                .map(|field| (*field).to_string())
                .collect(),
            pane_frames_enabled: true,
            pane_frame_template: crate::host::terminal::DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
            pane_frame_position: TerminalFramePosition::Top,
            pane_frame_style: TerminalFrameStyle::Default,
            pane_frame_visible_fields: crate::host::terminal::DEFAULT_PANE_FRAME_VISIBLE_FIELDS
                .iter()
                .map(|field| (*field).to_string())
                .collect(),
            terminal_cursor_style: mez_mux::presentation::TerminalCursorStyle::Block,
            terminal_cursor_blink: false,
            terminal_cursor_blink_interval_ms: 500,
            terminal_resize_debounce_ms: 200,
            terminal_render_rate_limit_fps: 5,
            terminal_agent_wrap_column_cap: crate::host::terminal::DEFAULT_AGENT_WRAP_COLUMN_CAP,
            terminal_reduced_motion: false,
            ui_theme: UiTheme::default(),
            key_bindings: KeyBindings::default(),
            command_bindings: std::collections::BTreeMap::new(),
            terminal_clipboard: ClipboardPolicy::External,
        }
    }
}

impl RuntimePresentationSettings {
    /// Parses one complete presentation settings replacement.
    pub(crate) fn from_config(
        root: &serde_json::Value,
        effective: &EffectiveConfig,
    ) -> Result<Self> {
        Ok(Self {
            window_frames_enabled: crate::runtime::runtime_window_frames_enabled_from_config(root)?,
            window_frame_template: crate::runtime::runtime_window_frame_template_from_config(root)?,
            window_frame_right_status_template:
                crate::runtime::runtime_window_frame_right_status_template_from_config(root)?,
            window_status_pill_definitions:
                crate::runtime::runtime_status_pill_definitions_from_config(root)?,
            window_frame_position: crate::runtime::runtime_window_frame_position_from_config(root)?,
            window_frame_style: crate::runtime::runtime_window_frame_style_from_config(root)?,
            window_frame_visible_fields:
                crate::runtime::runtime_window_frame_visible_fields_from_config(root)?,
            pane_frames_enabled: crate::runtime::runtime_pane_frames_enabled_from_config(root)?,
            pane_frame_template: crate::runtime::runtime_pane_frame_template_from_config(root)?,
            pane_frame_position: crate::runtime::runtime_pane_frame_position_from_config(root)?,
            pane_frame_style: crate::runtime::runtime_pane_frame_style_from_config(root)?,
            pane_frame_visible_fields:
                crate::runtime::runtime_pane_frame_visible_fields_from_config(root)?,
            terminal_cursor_style: crate::runtime::runtime_terminal_cursor_style_from_config(root)?,
            terminal_cursor_blink: crate::runtime::runtime_terminal_cursor_blink_from_config(root)?,
            terminal_cursor_blink_interval_ms:
                crate::runtime::runtime_terminal_cursor_blink_interval_ms_from_config(root)?,
            terminal_resize_debounce_ms:
                crate::runtime::runtime_terminal_resize_debounce_ms_from_config(root)?,
            terminal_render_rate_limit_fps:
                crate::runtime::runtime_terminal_render_rate_limit_fps_from_config(root)?,
            terminal_agent_wrap_column_cap:
                crate::runtime::runtime_terminal_agent_wrap_column_cap_from_config(root)?,
            terminal_reduced_motion: crate::runtime::runtime_terminal_reduced_motion_from_config(
                root,
            )?,
            ui_theme: crate::runtime::runtime_ui_theme_from_config(root)?,
            key_bindings: crate::runtime::runtime_key_bindings_from_config(root)?,
            command_bindings: crate::runtime::runtime_command_bindings_from_effective(effective)?,
            terminal_clipboard: crate::runtime::runtime_terminal_clipboard_from_config(root)?,
        })
    }
}

/// Owns paste-buffer, host-clipboard, and copy-mode presentation state.
#[derive(Debug)]
struct RuntimeCopyPresentationState {
    /// Named internal paste buffers and their bounded contents.
    paste_buffers: PasteBuffers,
    /// Buffer selected as the implicit copy and paste target.
    active_paste_buffer: Option<String>,
    /// Configured desktop clipboard adapter.
    host_clipboard: HostClipboard,
    /// Interactive copy modes keyed by pane id.
    active_copy_modes: std::collections::BTreeMap<String, CopyMode>,
    /// Panes using copy mode only as transient mouse scrollback.
    scrollback_copy_mode_panes: std::collections::BTreeSet<String>,
}

impl Default for RuntimeCopyPresentationState {
    fn default() -> Self {
        Self {
            paste_buffers: PasteBuffers::default_limit(),
            active_paste_buffer: None,
            host_clipboard: HostClipboard::system(),
            active_copy_modes: std::collections::BTreeMap::new(),
            scrollback_copy_mode_panes: std::collections::BTreeSet::new(),
        }
    }
}

/// Owns product presentation configuration and mutable client interaction state.
///
/// Fields are private to the render component and its descendants. Other
/// runtime components cross this boundary through narrow methods instead of
/// reaching into the session coordinator's former shared field bag.
#[derive(Debug, Default)]
pub(crate) struct RuntimePresentationComponent {
    /// Current atomically replaceable presentation configuration.
    settings: RuntimePresentationSettings,
    /// Cached output for command-backed window status pills.
    window_status_pill_cache: std::cell::RefCell<RuntimeStatusPillCache>,
    /// Copy, paste-buffer, and host-clipboard state.
    copy: RuntimeCopyPresentationState,
    /// Active agent prompt editor state keyed by pane id.
    agent_prompt_inputs: std::collections::BTreeMap<String, RuntimeAgentPromptInput>,
    /// Pane-local transient shell-output status rows.
    agent_shell_output_status_lines: std::collections::BTreeMap<String, Vec<String>>,
    /// Panes replaying durable agent presentation entries.
    agent_presentation_replay_panes: std::collections::BTreeSet<String>,
    /// Submitted command-prompt history retained across prompt openings.
    primary_command_prompt_history: Vec<String>,
    /// Active primary-client readline prompt, when one is open.
    primary_prompt_input: Option<RuntimePrimaryPromptInput>,
    /// Whether the primary client's next key uses the prefix table.
    primary_prefix_key_pending: bool,
    /// Active primary-client modal display overlay.
    primary_display_overlay: Option<RuntimeDisplayOverlay>,
    /// Transient candidate cycle for a record-browser Save path prompt.
    record_browser_save_completion: Option<RuntimeRecordBrowserSaveCompletion>,
    /// Typed record browsers waiting for display-response presentation.
    pending_record_browser_overlays:
        std::collections::BTreeMap<(String, String), mez_mux::record_browser::RecordBrowser>,
    /// Query sources waiting to accompany pending record browsers.
    pending_record_browser_overlay_sources:
        std::collections::BTreeMap<(String, String), RuntimeRecordBrowserOverlaySource>,
    /// Parent browser views waiting to accompany pending child views.
    pending_record_browser_overlay_stacks:
        std::collections::BTreeMap<(String, String), Vec<RuntimeRecordBrowserOverlayFrame>>,
    /// Active pane-divider resize gesture.
    mouse_resize_drag_state: Option<MouseResizeDragState>,
    /// Active mouse text-selection gesture.
    mouse_selection_drag_state: Option<MouseSelectionDragState>,
    /// Last pane-content click retained for double-click classification.
    last_mouse_click_state: Option<RuntimeMouseClickState>,
    /// Deferred copied-word highlight cleanup.
    deferred_word_copy_cleanup: std::cell::RefCell<Option<(String, CopyMode, u64)>>,
    /// Window-frame action pressed until a matching mouse release.
    pressed_window_action: Option<WindowFrameAction>,
    /// Transient primary-client error status.
    primary_error_status_overlay: Option<String>,
    /// Active pane-agent status selector.
    pane_agent_status_selector: Option<RuntimePaneAgentStatusSelector>,
}

/// Candidate cycle retained while one record-browser Save prompt is active.
///
/// The backend-neutral browser continues to own the editable path. Runtime
/// presentation retains only candidate ordering and the active candidate so
/// completion remains scoped to the pane that opened the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeRecordBrowserSaveCompletion {
    /// Input used to construct this candidate set.
    base_input: String,
    /// Literal candidate paths in stable filesystem order.
    candidates: Vec<String>,
    /// Candidate currently selected by Tab cycling.
    selected_index: usize,
}

impl RuntimePresentationComponent {
    /// Replaces validated presentation settings and synchronizes global width policy.
    pub(crate) fn apply_settings(&mut self, settings: RuntimePresentationSettings) {
        crate::host::terminal::set_agent_wrap_column_cap(settings.terminal_agent_wrap_column_cap);
        self.settings = settings;
    }

    /// Clears an in-progress pane-resize gesture after layout mutation.
    pub(crate) fn clear_mouse_resize_drag_state(&mut self) {
        self.mouse_resize_drag_state = None;
    }
}

impl RuntimeSessionService {
    /// Returns host clipboard state for presentation integration tests.
    #[cfg(test)]
    pub(crate) fn host_clipboard_for_tests(&self) -> &HostClipboard {
        &self.presentation.copy.host_clipboard
    }

    /// Returns mutable host clipboard state for presentation integration fixtures.
    #[cfg(test)]
    pub(crate) fn host_clipboard_mut_for_tests(&mut self) -> &mut HostClipboard {
        &mut self.presentation.copy.host_clipboard
    }

    /// Returns panes using copy mode as transient mouse scrollback.
    #[cfg(test)]
    pub(crate) fn scrollback_copy_mode_panes_for_tests(
        &self,
    ) -> &std::collections::BTreeSet<String> {
        &self.presentation.copy.scrollback_copy_mode_panes
    }

    /// Returns active agent prompt editors for integration tests.
    #[cfg(test)]
    pub(crate) fn agent_prompt_inputs_for_tests(
        &self,
    ) -> &std::collections::BTreeMap<String, RuntimeAgentPromptInput> {
        &self.presentation.agent_prompt_inputs
    }

    /// Returns mutable agent prompt editors for integration fixtures.
    #[cfg(test)]
    pub(crate) fn agent_prompt_inputs_mut_for_tests(
        &mut self,
    ) -> &mut std::collections::BTreeMap<String, RuntimeAgentPromptInput> {
        &mut self.presentation.agent_prompt_inputs
    }

    /// Replaces frame visibility for a presentation integration fixture.
    #[cfg(test)]
    pub(crate) fn set_frame_visibility_for_tests(
        &mut self,
        window_frames_enabled: bool,
        pane_frames_enabled: bool,
    ) {
        self.presentation.settings.window_frames_enabled = window_frames_enabled;
        self.presentation.settings.pane_frames_enabled = pane_frames_enabled;
    }

    /// Replaces pane frame placement for a presentation integration fixture.
    #[cfg(test)]
    pub(crate) fn set_pane_frame_position_for_tests(&mut self, position: TerminalFramePosition) {
        self.presentation.settings.pane_frame_position = position;
    }

    /// Registers typed browser state for a later agent-shell display response.
    pub(crate) fn register_pending_record_browser_overlay(
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

    /// Reports whether product window frames are enabled.
    pub(crate) fn window_frames_enabled(&self) -> bool {
        self.presentation.settings.window_frames_enabled
    }

    /// Returns the configured window frame template.
    pub(crate) fn window_frame_template(&self) -> &str {
        &self.presentation.settings.window_frame_template
    }

    /// Returns the configured window frame placement.
    pub(crate) fn window_frame_position(&self) -> TerminalFramePosition {
        self.presentation.settings.window_frame_position
    }

    /// Returns the configured window frame style.
    pub(crate) fn window_frame_style(&self) -> TerminalFrameStyle {
        self.presentation.settings.window_frame_style
    }

    /// Returns window fields eligible for frame template expansion.
    pub(crate) fn window_frame_visible_fields(&self) -> &[String] {
        &self.presentation.settings.window_frame_visible_fields
    }

    /// Reports whether product pane frames are enabled.
    pub(crate) fn pane_frames_enabled(&self) -> bool {
        self.presentation.settings.pane_frames_enabled
    }

    /// Returns the configured pane frame template.
    pub(crate) fn pane_frame_template(&self) -> &str {
        &self.presentation.settings.pane_frame_template
    }

    /// Returns the configured pane frame placement.
    pub(crate) fn pane_frame_position(&self) -> TerminalFramePosition {
        self.presentation.settings.pane_frame_position
    }

    /// Returns the configured pane frame style.
    pub(crate) fn pane_frame_style(&self) -> TerminalFrameStyle {
        self.presentation.settings.pane_frame_style
    }

    /// Returns pane fields eligible for frame template expansion.
    pub(crate) fn pane_frame_visible_fields(&self) -> &[String] {
        &self.presentation.settings.pane_frame_visible_fields
    }

    /// Returns the active product UI theme.
    pub(crate) fn ui_theme(&self) -> &UiTheme {
        &self.presentation.settings.ui_theme
    }

    /// Returns the configured mux key bindings.
    pub(crate) fn key_bindings(&self) -> &KeyBindings {
        &self.presentation.settings.key_bindings
    }

    /// Returns configured prefix-table command bindings.
    pub(crate) fn command_bindings(
        &self,
    ) -> &std::collections::BTreeMap<KeyChord, RuntimeCommandBinding> {
        &self.presentation.settings.command_bindings
    }

    /// Returns the runtime's bounded internal paste-buffer store.
    pub fn paste_buffers(&self) -> &PasteBuffers {
        &self.presentation.copy.paste_buffers
    }

    /// Returns mutable paste-buffer storage to product command adapters.
    pub(crate) fn paste_buffers_mut(&mut self) -> &mut PasteBuffers {
        &mut self.presentation.copy.paste_buffers
    }

    /// Returns the selected implicit copy and paste buffer.
    pub(crate) fn active_paste_buffer(&self) -> Option<&str> {
        self.presentation.copy.active_paste_buffer.as_deref()
    }

    /// Replaces the selected implicit copy and paste buffer.
    pub(crate) fn set_active_paste_buffer(&mut self, name: Option<String>) {
        self.presentation.copy.active_paste_buffer = name;
    }

    /// Returns active per-pane copy modes.
    pub(crate) fn active_copy_modes(&self) -> &std::collections::BTreeMap<String, CopyMode> {
        &self.presentation.copy.active_copy_modes
    }

    /// Returns mutable per-pane copy modes to copy and process adapters.
    pub(crate) fn active_copy_modes_mut(
        &mut self,
    ) -> &mut std::collections::BTreeMap<String, CopyMode> {
        &mut self.presentation.copy.active_copy_modes
    }

    /// Replaces the desktop clipboard adapter after configuration changes.
    pub(crate) fn set_host_clipboard(&mut self, host_clipboard: HostClipboard) {
        self.presentation.copy.host_clipboard = host_clipboard;
    }

    /// Returns the configured OSC 52 terminal clipboard policy.
    pub(crate) fn terminal_clipboard(&self) -> ClipboardPolicy {
        self.presentation.settings.terminal_clipboard
    }

    /// Removes one active agent prompt editor and returns its state.
    pub(crate) fn remove_agent_prompt_input(
        &mut self,
        pane_id: &str,
    ) -> Option<RuntimeAgentPromptInput> {
        self.presentation.agent_prompt_inputs.remove(pane_id)
    }

    /// Returns mutable agent prompt editor state for one pane.
    pub(crate) fn agent_prompt_input_mut(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut RuntimeAgentPromptInput> {
        self.presentation.agent_prompt_inputs.get_mut(pane_id)
    }

    /// Clears every active agent prompt editor during lifecycle teardown.
    pub(crate) fn clear_agent_prompt_inputs(&mut self) {
        self.presentation.agent_prompt_inputs.clear();
    }
}

#[cfg(test)]
impl RuntimeSessionService {
    /// Replaces the active UI theme for a presentation integration fixture.
    pub(crate) fn set_ui_theme_for_tests(&mut self, ui_theme: UiTheme) {
        self.presentation.settings.ui_theme = ui_theme;
    }

    /// Returns retained primary command-prompt history for integration tests.
    pub(crate) fn primary_command_prompt_history(&self) -> &[String] {
        &self.presentation.primary_command_prompt_history
    }

    /// Replaces retained command-prompt history for an integration fixture.
    pub(crate) fn set_primary_command_prompt_history_for_tests(&mut self, history: Vec<String>) {
        self.presentation.primary_command_prompt_history = history;
    }

    /// Adds one command-prompt history entry for an integration fixture.
    pub(crate) fn push_primary_command_prompt_history_for_tests(&mut self, command: String) {
        self.presentation
            .primary_command_prompt_history
            .push(command);
    }

    /// Returns the active primary prompt for product integration tests.
    pub(crate) fn primary_prompt_input(&self) -> Option<&RuntimePrimaryPromptInput> {
        self.presentation.primary_prompt_input.as_ref()
    }

    /// Reports whether the primary client is waiting for a prefix-table key.
    pub(crate) fn primary_prefix_key_pending(&self) -> bool {
        self.presentation.primary_prefix_key_pending
    }

    /// Returns the active primary display overlay for product integration tests.
    pub(crate) fn primary_display_overlay(&self) -> Option<&RuntimeDisplayOverlay> {
        self.presentation.primary_display_overlay.as_ref()
    }

    /// Returns the right-side frame status template for integration tests.
    pub(crate) fn window_frame_right_status_template(&self) -> &str {
        &self
            .presentation
            .settings
            .window_frame_right_status_template
    }

    /// Replaces a pending record browser's parent stack for a test fixture.
    pub(crate) fn set_pending_record_browser_overlay_stack_for_tests(
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
    pub(crate) fn pending_record_browser_overlays_is_empty(&self) -> bool {
        self.presentation.pending_record_browser_overlays.is_empty()
    }

    /// Returns the transient primary error status for product integration tests.
    pub(crate) fn primary_error_status_overlay(&self) -> Option<&str> {
        self.presentation.primary_error_status_overlay.as_deref()
    }

    /// Returns the active pane-agent selector for product integration tests.
    pub(crate) fn pane_agent_status_selector(&self) -> Option<&RuntimePaneAgentStatusSelector> {
        self.presentation.pane_agent_status_selector.as_ref()
    }

    /// Returns deferred copied-word cleanup state for product integration tests.
    pub(crate) fn deferred_word_copy_cleanup(
        &self,
    ) -> &std::cell::RefCell<Option<(String, CopyMode, u64)>> {
        &self.presentation.deferred_word_copy_cleanup
    }
}

use crate::host::terminal::{
    MousePaneAgentSelectorCell, MousePaneAgentStatusCell, PaneAgentStatusField,
    WindowFrameCommandKind, compose_modal_display_overlay_lines,
    compose_prompt_overlay_presentation_with_styles, pane_frame_agent_status_pillbox_cells,
    window_group_frame_pillbox_cells,
};
use crate::storage::transcript::AgentPresentationEntry;
use crate::ui::command::baseline_commands;
use crate::ui::selector::{SelectorExtraCandidate, SelectorSurface};
use mez_agent::mcp::McpServerStatus;
use mez_agent::{ActionResult, agent_output_content_type_is_markdown};
use mez_mux::attached_client::mouse_border_cells_for_geometries;
use mez_mux::copy::CopyPosition;
use mez_mux::presentation::{
    TerminalFramePosition, TerminalFrameStyle, TerminalPaneFrameContext,
    TerminalWindowFrameContext, TerminalWindowGroupFrameContext, TerminalWindowStatusContext,
    WindowPresentationOptions, WindowPresentationPlan, plan_window_presentation,
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

use mez_mux::overlay::{
    OverlayInputAction, OverlayInputOutcome, SelectorInputAction, SelectorInputOutcome,
    apply_overlay_input, apply_selector_input, overlay_input_action,
    scroll_selector as runtime_scroll_selector, selector_input_action,
    set_selector_index as runtime_set_selector_index,
};
#[cfg(test)]
use mez_mux::render::wrap_rich_text_line_to_width;
use mez_mux::render::{RichTextLine, push_or_extend_style_span};
use mez_mux::render::{
    RichTextLineKind, markdown_rendered_line_is_table_row,
    wrap_rich_text_line_to_width_with_source_ranges_hard,
};
use overlay::{
    RuntimeAgentShellDisplayOutput, agent_command_link_at_line_column,
    agent_shell_mcp_display_state_name, default_runtime_agent_prompt_input,
    runtime_agent_shell_display_output, runtime_agent_shell_visibility,
    runtime_command_display_overlay_content, runtime_command_display_should_open_overlay,
    runtime_pane_agent_selector_rendition, runtime_pane_agent_status_selector_layout,
    runtime_primary_prompt_input, runtime_selector_line,
};
#[cfg(test)]
use overlay::{runtime_agent_shell_markdown_overlay_content, runtime_human_readable_display_lines};
use presentation::{
    AgentTerminalPresentationStyle, agent_display_lines_are_error,
    agent_display_lines_are_low_level_status, agent_prompt_error_display_lines,
    overlay_styled_lines, render_command_markdown_body_lines_for_width,
    sanitized_agent_terminal_line,
};
#[cfg(test)]
use presentation::{
    agent_action_execution_display_header, agent_action_result_uses_diff_preview,
    agent_thinking_display_lines_for_width, command_preview_terminal_rendered_lines,
    readable_agent_diff_display_lines, readable_agent_diff_display_lines_for_width,
    render_agent_markdown_body_lines, render_command_markdown_body_lines,
    rendered_line_rendition_at, wrap_agent_terminal_text, wrapped_prefixed_agent_terminal_lines,
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
