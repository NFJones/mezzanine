//! Runtime Render implementation.
//!
//! This module owns the runtime render boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use crate::runtime::RuntimeAgentPromptProviderInfoRefresh;
use crate::ui::selector::SelectorExtraCandidate;
use mez_mux::input::{
    GroupFocusTarget, MouseBorderCell, MousePaneRegion, MouseWindowFrameCell, MuxAction,
    PasteBufferTarget, WindowFocusTarget, key_chord_input_bytes,
};
#[cfg(test)]
use mez_mux::overlay::{
    OVERLAY_ACTIVE_SELECTOR as DISPLAY_OVERLAY_ACTIVE_SELECTOR,
    OVERLAY_INACTIVE_SELECTOR as DISPLAY_OVERLAY_INACTIVE_SELECTOR,
    overlay_rendered_selection_start,
};
use mez_mux::overlay::{
    OverlaySelection, OverlaySelectionKind, apply_overlay_scroll_delta, clamp_overlay_scroll,
    overlay_content_line_index_for_view_row, overlay_copy_selection, overlay_footer,
    overlay_line_prefix_columns, overlay_link_rendition, overlay_render_lines,
    overlay_rendered_line_style_spans, overlay_scroll_page_rows,
    overlay_selection_index_at_position, overlay_text_at, overlay_visible_line_indices,
};

use super::service_state::{
    RunningShellTransactionKind, RuntimeDisplayOverlay, RuntimeLiveOverlaySourceKind,
    RuntimeMouseClickState, RuntimePaneAgentStatusSelector, RuntimePrimaryPromptInput,
    RuntimeRecordBrowserOverlayFrame, RuntimeRecordBrowserOverlaySource,
};
use super::{
    AgentShellVisibility, AgentTurnRecord, AgentTurnState, AttachedClientStepApplication,
    AttachedTerminalClientStepPlan, ClientViewRole, ClipboardEffectIntent, ClipboardPasteSource,
    ClipboardPasteSourceKind, ClipboardPolicy, ClipboardWritePlan, CopyMode, CopyModeKeyAction,
    EffectiveConfig, EventKind, HostClipboard, KeyBindings, KeyChord, MezError, MouseAction,
    MouseResizeDragState, MouseSelectionDragState, MouseWindowActionFrameCell, PaneDescriptor,
    PaneInputDispatch, PaneNavigationDirection, PaneSurfaceKind, PasteBuffers,
    ReadlineInputDecoder, ReadlineOutcome, ReadlinePrompt, ReadlinePromptKind,
    RenderInvalidationReason, RenderedClientView, Result, RuntimeAgentPromptInput,
    RuntimeCommandBinding, RuntimeSessionService, RuntimeSideEffect, RuntimeStatusPillCache,
    RuntimeStatusPillDefinition, RuntimeTransition, Size, SplitDirection, TerminalClientLoopAction,
    TerminalClientLoopConfig, TerminalFrameContext, TerminalScreen, WindowFrameAction,
    agent_prompt_reserved_line_count, current_unix_millis, current_unix_seconds, json_escape,
    mouse_action_name, mux_action_command_prompt_prefill, mux_action_name,
    pane_navigation_direction, parse_command_sequence,
    render_attached_client_view_with_screen_and_row_resolvers,
    runtime_agent_shell_command_response_json, runtime_agent_turn_duration_display,
    runtime_agent_turn_state_name, runtime_approval_policy_name, runtime_copy_position_for_view,
    runtime_fit_status_line, runtime_paste_bytes, select_clipboard_paste_source,
    window_frame_action_pillbox_cells, window_frame_pillbox_cells,
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Whether provisional provider output is rendered while it arrives.
    terminal_streaming_output: bool,
    /// Whether Mez-owned readline prompts may request enhanced keyboard input.
    terminal_enhanced_keyboard_reporting: bool,
    /// Whether completion-attention title pills alternate their attention color.
    terminal_completion_attention_flashing: bool,
    /// Resolved color and rendition policy for product UI surfaces.
    ui_theme: UiTheme,
    /// Structured blocking-editor candidates used by explicit edit actions.
    external_editor: crate::runtime::RuntimeExternalEditorConfig,
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
            terminal_render_rate_limit_fps: 30,
            terminal_agent_wrap_column_cap: crate::host::terminal::DEFAULT_AGENT_WRAP_COLUMN_CAP,
            terminal_reduced_motion: false,
            terminal_streaming_output: true,
            terminal_enhanced_keyboard_reporting: false,
            terminal_completion_attention_flashing: true,
            ui_theme: UiTheme::default(),
            external_editor: crate::runtime::RuntimeExternalEditorConfig::default(),
            key_bindings: KeyBindings::default(),
            command_bindings: std::collections::BTreeMap::new(),
            terminal_clipboard: ClipboardPolicy::External,
        }
    }
}

impl RuntimePresentationSettings {
    /// Classifies the client repaint required by one resolved settings replacement.
    pub(crate) fn invalidation_reason(
        &self,
        replacement: &Self,
    ) -> Option<RenderInvalidationReason> {
        if self == replacement {
            None
        } else if self.requires_full_redraw(replacement) {
            Some(RenderInvalidationReason::FullRedraw)
        } else {
            Some(RenderInvalidationReason::Configuration)
        }
    }

    /// Reports whether changed settings affect retained cells or presentation geometry.
    fn requires_full_redraw(&self, replacement: &Self) -> bool {
        self.window_frames_enabled != replacement.window_frames_enabled
            || self.window_frame_template != replacement.window_frame_template
            || self.window_frame_right_status_template
                != replacement.window_frame_right_status_template
            || self.window_status_pill_definitions != replacement.window_status_pill_definitions
            || self.window_frame_position != replacement.window_frame_position
            || self.window_frame_style != replacement.window_frame_style
            || self.window_frame_visible_fields != replacement.window_frame_visible_fields
            || self.pane_frames_enabled != replacement.pane_frames_enabled
            || self.pane_frame_template != replacement.pane_frame_template
            || self.pane_frame_position != replacement.pane_frame_position
            || self.pane_frame_style != replacement.pane_frame_style
            || self.pane_frame_visible_fields != replacement.pane_frame_visible_fields
            || self.terminal_agent_wrap_column_cap != replacement.terminal_agent_wrap_column_cap
            || self.terminal_reduced_motion != replacement.terminal_reduced_motion
            || self.terminal_streaming_output != replacement.terminal_streaming_output
            || self.terminal_completion_attention_flashing
                != replacement.terminal_completion_attention_flashing
            || self.ui_theme != replacement.ui_theme
    }

    /// Reports whether live provider output is enabled after motion policy is applied.
    pub(crate) fn effective_agent_streaming_output(&self) -> bool {
        self.terminal_streaming_output && !self.terminal_reduced_motion
    }

    /// Parses one complete presentation settings replacement.
    pub(crate) fn from_config(
        root: &serde_json::Value,
        effective: &EffectiveConfig,
    ) -> Result<Self> {
        let key_bindings = crate::runtime::runtime_key_bindings_from_config(root)?;
        let command_bindings =
            crate::runtime::runtime_command_bindings_from_effective(root, effective)?;
        crate::runtime::runtime_validate_key_binding_collisions(&key_bindings, &command_bindings)?;
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
            terminal_streaming_output:
                crate::runtime::runtime_terminal_streaming_output_from_config(root)?,
            terminal_enhanced_keyboard_reporting:
                crate::runtime::runtime_terminal_enhanced_keyboard_reporting_from_config(root)?,
            terminal_completion_attention_flashing:
                crate::runtime::runtime_terminal_completion_attention_flashing_from_config(root)?,
            ui_theme: crate::runtime::runtime_ui_theme_from_config(root)?,
            external_editor: crate::runtime::runtime_external_editor_config_from_config(root)?,
            key_bindings,
            command_bindings,
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
    /// Monotonic identity assigned to the newest host clipboard paste request.
    host_clipboard_paste_generation: u64,
    /// Newest destination awaiting a matching worker completion.
    pending_host_clipboard_paste: Option<(u64, crate::runtime::HostClipboardPasteTarget)>,
    /// Coalesced host clipboard work awaiting the external worker.
    pending_host_clipboard_reads: Vec<crate::runtime::RuntimeSideEffect>,
    /// Interactive copy modes keyed by pane and independently retained surface.
    active_copy_modes: std::collections::BTreeMap<(String, PaneSurfaceKind), CopyMode>,
    /// Pane surfaces using copy mode only as transient mouse scrollback.
    scrollback_copy_mode_panes: std::collections::BTreeSet<(String, PaneSurfaceKind)>,
}

impl Default for RuntimeCopyPresentationState {
    fn default() -> Self {
        Self {
            paste_buffers: PasteBuffers::default_limit(),
            active_paste_buffer: None,
            host_clipboard: HostClipboard::system(),
            host_clipboard_paste_generation: 0,
            pending_host_clipboard_paste: None,
            pending_host_clipboard_reads: Vec::new(),
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RuntimeAgentShellPreviewOwner {
    /// Turn that owns the running shell action.
    pub(crate) turn_id: String,
    /// Stable action identity within the turn.
    pub(crate) action_id: String,
    /// Exact shell transaction marker fencing stale output.
    pub(crate) marker: String,
}

/// Latest transient output retained for one running shell action.
#[derive(Debug, Clone)]
struct RuntimeAgentShellPreview {
    /// Pane-local order assigned when this owner first appears.
    first_seen_order: u64,
    /// Monotonic producer revision accepted for this owner.
    revision: u64,
    /// Sanitized terminal-width-fitted preview rows.
    lines: Vec<String>,
}

/// Pane-local projection of independently owned live shell previews.
///
/// `baseline_screen` contains durable presentation without these previews.
/// Runtime-owned lineage identifies the exact composite generation currently
/// installed without retaining another complete screen snapshot.
#[derive(Debug, Clone)]
struct RuntimeAgentShellPreviewPresentation {
    /// Conversation that owns both retained screen generations.
    conversation_id: String,
    /// Runtime-owned lineage of the exact composite screen currently installed.
    installed_lineage: u64,
    /// Durable pane generation onto which previews are projected.
    baseline_screen: std::sync::Arc<TerminalScreen>,
    /// Next pane-local first-seen order.
    next_order: u64,
    /// Independently mutable previews keyed by exact shell owner.
    previews: std::collections::BTreeMap<RuntimeAgentShellPreviewOwner, RuntimeAgentShellPreview>,
    /// Settled owners whose final tail remains until the next durable pane write.
    settled_owners: std::collections::BTreeSet<RuntimeAgentShellPreviewOwner>,
}

/// Transient terminal interaction state owned by one exact client.
///
/// The runtime still projects these values through the legacy presentation
/// fields while applying one client operation. The keyed copy is authoritative
/// between operations, which prevents another client from inheriting prompts,
/// overlays, prefix state, or incomplete mouse gestures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RuntimeClientPresentationState {
    primary_prompt_input: Option<RuntimePrimaryPromptInput>,
    primary_prefix_key_pending: bool,
    primary_display_overlay: Option<RuntimeDisplayOverlay>,
    agent_prompt_inputs: std::collections::BTreeMap<String, RuntimeAgentPromptInput>,
    active_copy_modes: std::collections::BTreeMap<(String, PaneSurfaceKind), CopyMode>,
    scrollback_copy_mode_panes: std::collections::BTreeSet<(String, PaneSurfaceKind)>,
    mouse_resize_drag_state: Option<MouseResizeDragState>,
    mouse_selection_drag_state: Option<MouseSelectionDragState>,
    last_mouse_click_state: Option<RuntimeMouseClickState>,
    deferred_word_copy_cleanup: Option<(String, PaneSurfaceKind, CopyMode, u64)>,
    pressed_window_action: Option<WindowFrameAction>,
    primary_error_status_overlay: Option<String>,
    pane_agent_status_selector: Option<RuntimePaneAgentStatusSelector>,
    presentation_revision: u64,
}

#[derive(Debug, Default)]
pub(crate) struct RuntimePresentationComponent {
    /// Current atomically replaceable presentation configuration.
    settings: RuntimePresentationSettings,
    /// Client renders deferred until the current runtime operation succeeds.
    deferred_render_effects: Vec<RuntimeSideEffect>,
    /// Generation-keyed immutable visible rows for pane composition.
    pane_styled_row_cache: std::cell::RefCell<RuntimePaneStyledRowCache>,
    /// Bounded geometry-plan cache for the current window snapshot.
    window_presentation_plan_cache: std::cell::RefCell<RuntimeWindowPresentationPlanCache>,
    /// Cached output for command-backed window status pills.
    window_status_pill_cache: std::cell::RefCell<RuntimeStatusPillCache>,
    /// Exact-client transient interaction state retained between operations.
    client_states:
        std::collections::HashMap<mez_core::ids::ClientId, RuntimeClientPresentationState>,
    /// Client whose state is currently projected through the compatibility fields.
    projected_client_id: Option<mez_core::ids::ClientId>,
    /// Copy, paste-buffer, and host-clipboard state.
    copy: RuntimeCopyPresentationState,
    /// Active agent prompt editor state keyed by pane id.
    agent_prompt_inputs: std::collections::BTreeMap<String, RuntimeAgentPromptInput>,
    /// Exact prompt snapshots retained while an external editor owns a pane.
    external_agent_prompt_edits:
        std::collections::BTreeMap<String, external_prompt::RuntimeAgentPromptEditSnapshot>,
    /// Provider refreshes submitted from agent prompts awaiting actor dispatch.
    pending_agent_prompt_provider_info_refreshes: Vec<RuntimeAgentPromptProviderInfoRefresh>,
    /// Background selector discoveries keyed by exact client and pane owner.
    agent_prompt_selector_refreshes: std::collections::HashMap<
        (mez_core::ids::ClientId, String),
        RuntimeAgentSelectorCandidateRefresh,
    >,
    /// Pane-local owner-aware transient shell-output projections.
    agent_shell_output_previews:
        std::collections::BTreeMap<String, RuntimeAgentShellPreviewPresentation>,
    /// Source-backed provider `say` output awaiting validated completion.
    agent_streaming_say_presentations:
        std::collections::BTreeMap<String, RuntimeStreamingSayPresentation>,
    /// Streamed action indices already installed as validated presentation.
    agent_promoted_streaming_say_actions:
        std::collections::BTreeMap<(String, String), std::collections::BTreeSet<usize>>,
    /// Panes replaying durable agent presentation entries.
    agent_presentation_replay_panes: std::collections::BTreeSet<String>,
    /// Newest pane size awaiting source-backed agent presentation replay.
    pending_agent_presentation_resize_sizes: std::collections::BTreeMap<String, Size>,
    /// Panes whose newest resize generation still needs one worker dispatch.
    pending_agent_presentation_resize_dispatches: std::collections::BTreeSet<String>,
    /// Installed source-backed presentation projections keyed by pane id.
    agent_presentation_projection_cache: std::collections::BTreeMap<String, (String, Size)>,
    /// Submitted command-prompt history retained across prompt openings.
    primary_command_prompt_history: Vec<mez_mux::readline::ReadlineHistoryEntry>,
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
    deferred_word_copy_cleanup:
        std::cell::RefCell<Option<(String, PaneSurfaceKind, CopyMode, u64)>>,
    /// Parent pane projections retained while ephemeral loop conversations run.
    agent_loop_parent_projections:
        std::collections::BTreeMap<String, RuntimeAgentLoopParentProjectionSnapshot>,
    /// Window-frame action pressed until a matching mouse release.
    pressed_window_action: Option<WindowFrameAction>,
    /// Transient primary-client error status.
    primary_error_status_overlay: Option<String>,
    /// Active pane-agent status selector.
    pane_agent_status_selector: Option<RuntimePaneAgentStatusSelector>,
    /// Source-isolated harness statuses keyed first by pane and then source.
    pane_harness_statuses: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, harness_status::RuntimePaneHarnessStatusEntry>,
    >,
    /// Monotonic sequence used to select the most recently updated source.
    next_pane_harness_status_sequence: u64,
    /// Unacknowledged background agent completions keyed by stable pane id.
    completion_attention_panes: std::collections::BTreeSet<String>,
}

/// Bounded pane-row projection cache owned by the active presentation window.
#[derive(Debug, Default)]
struct RuntimePaneStyledRowCache {
    rows: std::collections::BTreeMap<
        String,
        (u64, std::sync::Arc<[mez_terminal::TerminalStyledLine]>),
    >,
    hits: u64,
    misses: u64,
}

/// One-entry presentation-plan cache keyed by every geometry-affecting input.
#[derive(Debug, Default)]
struct RuntimeWindowPresentationPlanCache {
    entry: Option<(
        mez_mux::layout::Window,
        mez_mux::presentation::WindowPresentationOptions,
        std::sync::Arc<mez_mux::presentation::WindowPresentationPlan>,
    )>,
    hits: u64,
    misses: u64,
}

/// One in-flight pane-local selector candidate discovery.
#[derive(Debug)]
pub(super) struct RuntimeAgentSelectorCandidateRefresh {
    /// Prompt source generation captured when discovery started.
    generation: u64,
    /// Nonblocking completion receiver owned by serialized presentation state.
    receiver: std::sync::mpsc::Receiver<Vec<SelectorExtraCandidate>>,
}

/// One source-backed provider response currently projected into an agent pane.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeStreamingSayPresentation {
    /// Turn whose provider stream owns this presentation.
    turn_id: String,
    /// Provider interaction whose response-local ordinals own this source.
    response_index: usize,
    /// Conversation binding captured before the first streamed action.
    conversation_id: String,
    /// Runtime-owned lineage of the exact composite screen currently installed.
    installed_lineage: u64,
    /// Exact pane state restored before each rich-source reprojection.
    baseline_screen: std::sync::Arc<TerminalScreen>,
    /// Latest provider-only screen before shell previews are composited.
    provider_screen: std::sync::Arc<TerminalScreen>,
    /// Direct batch rationale accumulated from the provider stream.
    rationale: Option<RuntimeStreamingTextSource>,
    /// Established streamed actions keyed by their MAAP array index.
    actions: std::collections::BTreeMap<usize, RuntimeStreamingSayAction>,
    /// Established shell-command source keyed by MAAP action index.
    shell_commands: std::collections::BTreeMap<usize, RuntimeStreamingTextSource>,
    /// Monotonic render-input generation used to fence worker projections.
    revision: u64,
    /// Newest cumulative-source projection atomically installed in the pane.
    projected_revision: Option<u64>,
    /// Render inputs used by the newest atomically installed projection.
    projected_context: Option<RuntimeStreamingSayProjectionContext>,
    /// Worker-rendered action metadata retained for exact promotion.
    projected_actions: Option<Vec<RuntimeStreamingSayProjectedAction>>,
    /// Worker-rendered batch rationale retained with the installed projection.
    projected_rationale: Option<RuntimeStreamingSayProjectedRationale>,
    /// Installed screen lineage associated with retained projection metadata.
    projected_lineage: Option<u64>,
}

/// Non-source inputs that determine one streaming projection generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeStreamingSayProjectionContext {
    /// Whether thinking text was visible while the generation was rendered.
    thinking_enabled: bool,
    /// Shell dialect used by the command-preview highlighter.
    shell_classification: mez_agent::ShellClassification,
    /// Pane presentation width used by the static thinking renderer.
    presentation_columns: usize,
    /// Available width inside the agent presentation gutter.
    frame_width: usize,
    /// Full terminal width used by table and fenced renderers.
    table_width: usize,
    /// Immutable theme used to render the generation.
    ui_theme: mez_mux::theme::UiTheme,
    /// Exact terminal geometry used to render the generation.
    screen_size: Size,
}

/// Accumulated source and contract fields for one streamed `say` action.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeStreamingSayAction {
    /// Lifecycle status established before source text became visible.
    status: mez_agent::SayStatus,
    /// Normalized presentation media type.
    content_type: String,
    /// Complete decoded source received so far.
    text: String,
    /// Whether the JSON source string has closed.
    complete: bool,
}

/// Accumulated source for one streamed plain-text presentation field.
#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeStreamingTextSource {
    /// Complete decoded source received so far.
    text: String,
    /// Whether the JSON source string has closed.
    complete: bool,
}

/// Persistable metadata for one atomically projected streamed action.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeStreamingSayProjectedAction {
    /// Original action position in the provider batch.
    pub(crate) action_index: usize,
    /// Visible action kind represented by this installed projection.
    pub(crate) kind: RuntimeStreamingSayProjectedActionKind,
    /// Stable presentation style name for every rendered line.
    pub(crate) style: String,
    /// Complete display lines published for this action.
    pub(crate) rendered_lines: Vec<String>,
    /// Complete raw-copy metadata associated with the rendered component.
    pub(crate) copy_lines: Vec<String>,
}

/// Persistable metadata for an atomically projected batch rationale.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeStreamingSayProjectedRationale {
    /// Stable presentation style name for every rendered line.
    pub(crate) style: String,
    /// Complete display lines published for the rationale.
    pub(crate) rendered_lines: Vec<String>,
    /// Complete raw-copy metadata associated with the rationale.
    pub(crate) copy_lines: Vec<String>,
}

/// Visible action kind retained with an installed streaming projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeStreamingSayProjectedActionKind {
    /// Provider `say` output rendered through the ordinary content renderer.
    Say,
    /// Shell-command source rendered through the ordinary command preview.
    ShellCommand {
        /// Whether the bounded preview omitted source text.
        truncated: bool,
    },
}

/// Result of reconciling provisional output with one validated completion.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RuntimeStreamingSayCompletionReconciliation {
    /// Action indices whose installed rows now own durable presentation.
    pub(crate) promoted_action_indices: std::collections::BTreeSet<usize>,
}

/// Immutable input for one cumulative streaming-say projection worker.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeStreamingSayProjectionWork {
    /// Pane that owns the source-backed presentation.
    pub(crate) pane_id: String,
    /// Provider turn that owns the streamed source.
    pub(crate) turn_id: String,
    /// Provider interaction that owns response-local action ordinals.
    pub(crate) response_index: usize,
    /// Conversation binding captured when streaming began.
    pub(crate) conversation_id: String,
    /// Exact source generation represented by this work item.
    pub(crate) revision: u64,
    /// Installed screen lineage captured before worker rendering began.
    pub(crate) installed_lineage: u64,
    /// Pre-stream screen from which the complete candidate is rebuilt.
    pub(crate) baseline_screen: std::sync::Arc<TerminalScreen>,
    /// Cumulative batch rationale state captured for this generation.
    pub(crate) rationale: Option<RuntimeStreamingTextSource>,
    /// Cumulative action state captured for this generation.
    pub(crate) actions: std::collections::BTreeMap<usize, RuntimeStreamingSayAction>,
    /// Cumulative shell-command state captured for this generation.
    pub(crate) shell_commands: std::collections::BTreeMap<usize, RuntimeStreamingTextSource>,
    /// Whether thinking text is visible for this pane.
    pub(crate) thinking_enabled: bool,
    /// Shell dialect used by the existing command-preview highlighter.
    pub(crate) shell_classification: mez_agent::ShellClassification,
    /// Current pane presentation width used by the static thinking renderer.
    pub(crate) presentation_columns: usize,
    /// Available width inside the agent presentation gutter.
    pub(crate) frame_width: usize,
    /// Full terminal width used by table and fenced renderers.
    pub(crate) table_width: usize,
    /// Immutable theme generation captured for this projection.
    pub(crate) ui_theme: mez_mux::theme::UiTheme,
    /// Exact terminal geometry captured for this projection.
    pub(crate) screen_size: Size,
}

/// Complete screen generation produced outside the serialized runtime actor.
#[derive(Debug)]
pub(crate) struct RuntimeStreamingSayProjectionResult {
    /// Pane that owns the candidate screen.
    pub(crate) pane_id: String,
    /// Provider turn that owns the candidate screen.
    pub(crate) turn_id: String,
    /// Provider interaction that owns response-local action ordinals.
    pub(crate) response_index: usize,
    /// Conversation binding captured by the worker input.
    pub(crate) conversation_id: String,
    /// Exact source generation represented by the candidate.
    pub(crate) revision: u64,
    /// Installed screen lineage captured by the worker input.
    pub(crate) installed_lineage: u64,
    /// Thinking visibility captured by the worker input.
    pub(crate) thinking_enabled: bool,
    /// Shell dialect captured by the worker input.
    pub(crate) shell_classification: mez_agent::ShellClassification,
    /// Pane presentation width captured by the worker input.
    pub(crate) presentation_columns: usize,
    /// Available width used to build the candidate.
    pub(crate) frame_width: usize,
    /// Full terminal width used to build tables and fenced content.
    pub(crate) table_width: usize,
    /// Immutable theme used to build the candidate.
    pub(crate) ui_theme: mez_mux::theme::UiTheme,
    /// Exact terminal geometry used to build the candidate.
    pub(crate) screen_size: Size,
    /// Complete per-action metadata built with the candidate screen.
    pub(crate) projected_actions: Vec<RuntimeStreamingSayProjectedAction>,
    /// Batch-rationale metadata built with the candidate screen.
    pub(crate) projected_rationale: Option<RuntimeStreamingSayProjectedRationale>,
    /// Fully rendered candidate published only as one state replacement.
    pub(crate) screen: TerminalScreen,
}

/// Immutable actor snapshot used to rebuild one resized agent presentation.
#[derive(Debug)]
pub(crate) struct RuntimeAgentPresentationResizeWork {
    /// Pane that owns the source-backed presentation.
    pub(crate) pane_id: String,
    /// Conversation binding captured before worker execution.
    conversation_id: String,
    /// Exact resized screen lineage that the candidate may replace.
    captured_lineage: u64,
    /// Exact terminal geometry represented by the candidate.
    size: Size,
    /// Durable presentation repository read only by the blocking worker.
    transcript_store: crate::storage::transcript::AgentTranscriptStore,
    /// Clone of the mux layout used by the canonical presentation renderer.
    session: mez_mux::session::Session,
    /// Runtime socket identity needed to construct an isolated renderer.
    socket_path: std::path::PathBuf,
    /// Runtime creation timestamp retained by the isolated renderer.
    created_at_unix_seconds: u64,
    /// Exact pane conversation state captured with the resize generation.
    agent_session: mez_agent::AgentShellSession,
    /// Immutable presentation settings captured with the resize generation.
    presentation_settings: RuntimePresentationSettings,
    /// Maximum history retained by the rebuilt terminal screen.
    history_limit: usize,
    /// History rotation batch retained by the rebuilt terminal screen.
    history_rotate_lines: usize,
    /// Provisionally resized screen used to preserve current terminal state.
    screen: TerminalScreen,
    /// Current transient shell previews to composite over durable replay.
    shell_output_previews: Option<RuntimeAgentShellPreviewPresentation>,
    /// Current streamed provider presentation to composite over durable replay.
    streaming_say_presentation: Option<RuntimeStreamingSayPresentation>,
}

/// Complete canonical resize candidate built outside the serialized actor.
#[derive(Debug)]
pub(crate) struct RuntimeAgentPresentationResizeResult {
    /// Pane that owns the candidate screen.
    pane_id: String,
    /// Conversation binding captured by the worker input.
    conversation_id: String,
    /// Exact resized screen lineage that the candidate may replace.
    captured_lineage: u64,
    /// Exact terminal geometry represented by the candidate.
    size: Size,
    /// Presentation settings that determined semantic rendering.
    presentation_settings: RuntimePresentationSettings,
    /// History policy that determined retained canonical rows.
    history_limit: usize,
    /// History rotation policy that determined retained canonical rows.
    history_rotate_lines: usize,
    /// Fully rendered candidate published only as one state replacement.
    screen: TerminalScreen,
    /// Worker-reconciled shell preview state accompanying the candidate.
    shell_output_previews: Option<RuntimeAgentShellPreviewPresentation>,
    /// Worker-reconciled streaming state accompanying the candidate.
    streaming_say_presentation: Option<RuntimeStreamingSayPresentation>,
}

/// Pane-local presentation state restored when conversation resume fails.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeAgentResumePresentationSnapshot {
    prompt_input: Option<RuntimeAgentPromptInput>,
    shell_output_previews: Option<RuntimeAgentShellPreviewPresentation>,
    streaming_say_presentation: Option<RuntimeStreamingSayPresentation>,
    promoted_streaming_say_actions:
        std::collections::BTreeMap<(String, String), std::collections::BTreeSet<usize>>,
    projection: Option<(String, Size)>,
    pending_resize: Option<Size>,
    replay_active: bool,
    copy_modes: Vec<((String, PaneSurfaceKind), CopyMode)>,
    scrollback_surfaces: Vec<(String, PaneSurfaceKind)>,
    mouse_selection_drag_state: Option<MouseSelectionDragState>,
    last_mouse_click_state: Option<RuntimeMouseClickState>,
    deferred_word_copy_cleanup: Option<(String, PaneSurfaceKind, CopyMode, u64)>,
}

/// Exact parent projection retained while a logical loop uses ephemeral conversations.
#[derive(Debug, Clone)]
struct RuntimeAgentLoopParentProjectionSnapshot {
    pane_id: String,
    agent_screen: Option<(String, TerminalScreen)>,
    presentation: RuntimeAgentResumePresentationSnapshot,
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

/// Priority used to retain the strongest pending presentation invalidation.
fn presentation_render_invalidation_priority(reason: RenderInvalidationReason) -> u8 {
    match reason {
        RenderInvalidationReason::CursorBlink => 0,
        RenderInvalidationReason::StatusLine => 1,
        RenderInvalidationReason::PaneOutput => 2,
        RenderInvalidationReason::AgentPrompt => 3,
        RenderInvalidationReason::Overlay => 4,
        RenderInvalidationReason::Configuration => 5,
        RenderInvalidationReason::ResizeDrag => 6,
        RenderInvalidationReason::Resize => 7,
        RenderInvalidationReason::Layout => 8,
        RenderInvalidationReason::FullRedraw => 9,
    }
}

impl RuntimePresentationComponent {
    /// Returns the next live-overlay refresh deadline for one exact client.
    pub(crate) fn client_live_overlay_next_due_ms(
        &mut self,
        client_id: &mez_core::ids::ClientId,
    ) -> Option<u64> {
        self.capture_projected_client_state();
        self.client_states
            .get(client_id)
            .and_then(|state| state.primary_display_overlay.as_ref())
            .and_then(|overlay| overlay.live_source.as_ref())
            .map(|source| source.next_due_ms)
    }

    /// Returns the newest captured transient presentation revision for one client.
    pub(crate) fn client_presentation_revision(
        &mut self,
        client_id: &mez_core::ids::ClientId,
    ) -> u64 {
        self.capture_projected_client_state();
        self.client_states
            .get(client_id)
            .map(|state| state.presentation_revision)
            .unwrap_or_default()
    }

    /// Switches the compatibility fields to one exact client's transient state.
    pub(crate) fn activate_client_state(&mut self, client_id: &mez_core::ids::ClientId) {
        if self.projected_client_id.as_ref() == Some(client_id) {
            return;
        }
        self.capture_projected_client_state();
        let state = self
            .client_states
            .get(client_id)
            .cloned()
            .unwrap_or_default();
        self.primary_prompt_input = state.primary_prompt_input;
        self.primary_prefix_key_pending = state.primary_prefix_key_pending;
        self.primary_display_overlay = state.primary_display_overlay;
        self.agent_prompt_inputs = state.agent_prompt_inputs;
        self.copy.active_copy_modes = state.active_copy_modes;
        self.copy.scrollback_copy_mode_panes = state.scrollback_copy_mode_panes;
        self.mouse_resize_drag_state = state.mouse_resize_drag_state;
        self.mouse_selection_drag_state = state.mouse_selection_drag_state;
        self.last_mouse_click_state = state.last_mouse_click_state;
        self.deferred_word_copy_cleanup
            .replace(state.deferred_word_copy_cleanup);
        self.pressed_window_action = state.pressed_window_action;
        self.primary_error_status_overlay = state.primary_error_status_overlay;
        self.pane_agent_status_selector = state.pane_agent_status_selector;
        self.projected_client_id = Some(client_id.clone());
    }

    /// Persists the currently projected compatibility fields for their owner.
    pub(crate) fn capture_projected_client_state(&mut self) {
        let Some(client_id) = self.projected_client_id.clone() else {
            return;
        };
        let previous = self
            .client_states
            .get(&client_id)
            .cloned()
            .unwrap_or_default();
        let mut next = RuntimeClientPresentationState {
            primary_prompt_input: self.primary_prompt_input.clone(),
            primary_prefix_key_pending: self.primary_prefix_key_pending,
            primary_display_overlay: self.primary_display_overlay.clone(),
            agent_prompt_inputs: self.agent_prompt_inputs.clone(),
            active_copy_modes: self.copy.active_copy_modes.clone(),
            scrollback_copy_mode_panes: self.copy.scrollback_copy_mode_panes.clone(),
            mouse_resize_drag_state: self.mouse_resize_drag_state.clone(),
            mouse_selection_drag_state: self.mouse_selection_drag_state.clone(),
            last_mouse_click_state: self.last_mouse_click_state.clone(),
            deferred_word_copy_cleanup: self.deferred_word_copy_cleanup.borrow().clone(),
            pressed_window_action: self.pressed_window_action.clone(),
            primary_error_status_overlay: self.primary_error_status_overlay.clone(),
            pane_agent_status_selector: self.pane_agent_status_selector.clone(),
            presentation_revision: previous.presentation_revision,
        };
        let mut comparable_next = next.clone();
        if let (Some(next_source), Some(previous_source)) = (
            comparable_next
                .primary_display_overlay
                .as_mut()
                .and_then(|overlay| overlay.live_source.as_mut()),
            previous
                .primary_display_overlay
                .as_ref()
                .and_then(|overlay| overlay.live_source.as_ref()),
        ) && next_source.source == previous_source.source
            && next_source.refresh_interval_ms == previous_source.refresh_interval_ms
        {
            next_source.next_due_ms = previous_source.next_due_ms;
        }
        if comparable_next != previous {
            next.presentation_revision = previous.presentation_revision.saturating_add(1);
        }
        self.client_states.insert(client_id, next);
    }

    /// Removes all transient presentation owned by one detached client.
    pub(crate) fn remove_client_state(&mut self, client_id: &mez_core::ids::ClientId) {
        self.client_states.remove(client_id);
        self.agent_prompt_selector_refreshes
            .retain(|(candidate, _), _| candidate != client_id);
        if self.projected_client_id.as_ref() == Some(client_id) {
            self.primary_prompt_input = None;
            self.primary_prefix_key_pending = false;
            self.primary_display_overlay = None;
            self.agent_prompt_inputs.clear();
            self.copy.active_copy_modes.clear();
            self.copy.scrollback_copy_mode_panes.clear();
            self.mouse_resize_drag_state = None;
            self.mouse_selection_drag_state = None;
            self.last_mouse_click_state = None;
            self.deferred_word_copy_cleanup.replace(None);
            self.pressed_window_action = None;
            self.primary_error_status_overlay = None;
            self.pane_agent_status_selector = None;
            self.projected_client_id = None;
        }
    }

    /// Returns exact clients retained by provider refresh work awaiting dispatch.
    pub(crate) fn pending_client_references(
        &self,
    ) -> std::collections::HashSet<mez_core::ids::ClientId> {
        self.pending_agent_prompt_provider_info_refreshes
            .iter()
            .map(|refresh| refresh.primary_client_id.clone())
            .collect()
    }

    /// Removes pane-scoped interaction state from every retained client.
    pub(crate) fn remove_pane_state_for_all_clients(&mut self, pane_id: &str) {
        self.capture_projected_client_state();
        self.agent_prompt_selector_refreshes
            .retain(|(_, candidate), _| candidate != pane_id);
        for state in self.client_states.values_mut() {
            let before = state.clone();
            if state
                .primary_display_overlay
                .as_ref()
                .and_then(|overlay| overlay.live_source.as_ref())
                .is_some_and(|source| {
                    matches!(
                        &source.source,
                        RuntimeLiveOverlaySourceKind::AgentStatus {
                            pane_id: source_pane_id,
                            ..
                        } if source_pane_id == pane_id
                    )
                })
            {
                state.primary_display_overlay = None;
            }
            state.agent_prompt_inputs.remove(pane_id);
            state
                .active_copy_modes
                .retain(|(candidate, _), _| candidate != pane_id);
            state
                .scrollback_copy_mode_panes
                .retain(|(candidate, _)| candidate != pane_id);
            if state
                .mouse_selection_drag_state
                .as_ref()
                .is_some_and(|drag| drag.pane_id == pane_id)
            {
                state.mouse_selection_drag_state = None;
            }
            if state
                .last_mouse_click_state
                .as_ref()
                .is_some_and(|click| click.pane_id == pane_id)
            {
                state.last_mouse_click_state = None;
            }
            if state
                .deferred_word_copy_cleanup
                .as_ref()
                .is_some_and(|(candidate, _, _, _)| candidate == pane_id)
            {
                state.deferred_word_copy_cleanup = None;
            }
            if *state != before {
                state.presentation_revision = before.presentation_revision.saturating_add(1);
            }
        }
        if let Some(client_id) = self.projected_client_id.clone() {
            self.projected_client_id = None;
            self.activate_client_state(&client_id);
        }
    }

    /// Reports whether provisional provider output may be rendered live.
    pub(crate) fn effective_agent_streaming_output(&self) -> bool {
        self.settings.effective_agent_streaming_output()
    }

    /// Records a completion only when its pane is not currently focused.
    pub(crate) fn register_completion_attention(
        &mut self,
        pane_id: &str,
        focused_pane_id: Option<&str>,
    ) {
        if focused_pane_id != Some(pane_id) {
            self.completion_attention_panes.insert(pane_id.to_string());
        }
    }

    /// Acknowledges any completion currently owned by the focused pane.
    pub(crate) fn acknowledge_completion_attention(&mut self, pane_id: &str) {
        self.completion_attention_panes.remove(pane_id);
    }

    /// Removes completion state for a pane that no longer exists.
    pub(crate) fn remove_completion_attention(&mut self, pane_id: &str) {
        self.completion_attention_panes.remove(pane_id);
    }

    /// Removes every pane-keyed agent presentation artifact during teardown.
    pub(crate) fn remove_agent_presentation_state(&mut self, pane_id: &str) {
        self.agent_prompt_inputs.remove(pane_id);
        self.external_agent_prompt_edits.remove(pane_id);
        self.agent_prompt_selector_refreshes
            .retain(|(_, candidate), _| candidate != pane_id);
        self.agent_shell_output_previews.remove(pane_id);
        self.agent_streaming_say_presentations.remove(pane_id);
        self.agent_promoted_streaming_say_actions
            .retain(|(candidate_pane_id, _turn_id), _indices| candidate_pane_id != pane_id);
        self.agent_presentation_replay_panes.remove(pane_id);
        self.pending_agent_presentation_resize_sizes.remove(pane_id);
        self.pending_agent_presentation_resize_dispatches
            .remove(pane_id);
        self.agent_presentation_projection_cache.remove(pane_id);
        self.pane_harness_statuses.remove(pane_id);
    }

    /// Seeds every pane-keyed agent presentation map for teardown regressions.
    #[cfg(test)]
    pub(crate) fn seed_agent_presentation_state_for_tests(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        size: Size,
    ) {
        self.agent_prompt_inputs
            .insert(pane_id.to_string(), default_runtime_agent_prompt_input());
        let baseline_screen =
            TerminalScreen::new(size, 10).expect("test presentation screen should be valid");
        self.agent_shell_output_previews.insert(
            pane_id.to_string(),
            RuntimeAgentShellPreviewPresentation {
                conversation_id: conversation_id.to_string(),
                installed_lineage: 0,
                baseline_screen: std::sync::Arc::new(baseline_screen),
                next_order: 0,
                previews: std::collections::BTreeMap::new(),
                settled_owners: std::collections::BTreeSet::new(),
            },
        );
        self.agent_presentation_replay_panes
            .insert(pane_id.to_string());
        self.pending_agent_presentation_resize_sizes
            .insert(pane_id.to_string(), size);
        self.agent_presentation_projection_cache
            .insert(pane_id.to_string(), (conversation_id.to_string(), size));
    }

    /// Reports whether any pane-keyed agent presentation artifact remains.
    #[cfg(test)]
    pub(crate) fn has_agent_presentation_state_for_tests(&self, pane_id: &str) -> bool {
        self.agent_prompt_inputs.contains_key(pane_id)
            || self.agent_shell_output_previews.contains_key(pane_id)
            || self.agent_streaming_say_presentations.contains_key(pane_id)
            || self
                .agent_promoted_streaming_say_actions
                .keys()
                .any(|(candidate_pane_id, _turn_id)| candidate_pane_id == pane_id)
            || self.agent_presentation_replay_panes.contains(pane_id)
            || self
                .pending_agent_presentation_resize_sizes
                .contains_key(pane_id)
            || self
                .agent_presentation_projection_cache
                .contains_key(pane_id)
    }

    /// Replaces validated settings and returns their required client invalidation.
    pub(crate) fn apply_settings(
        &mut self,
        settings: RuntimePresentationSettings,
    ) -> Option<RenderInvalidationReason> {
        let invalidation_reason = self.settings.invalidation_reason(&settings);
        crate::host::terminal::set_agent_wrap_column_cap(settings.terminal_agent_wrap_column_cap);
        self.settings = settings;
        self.agent_presentation_projection_cache.clear();
        invalidation_reason
    }

    /// Installs immutable settings on an isolated projection worker.
    ///
    /// Worker-local renderers must not mutate process-global terminal policy;
    /// the live runtime already installed the captured policy before the work
    /// item was created.
    pub(crate) fn apply_projection_settings(&mut self, settings: RuntimePresentationSettings) {
        self.settings = settings;
        self.agent_presentation_projection_cache.clear();
    }

    /// Queues the strongest pending render effect per client after config succeeds.
    pub(crate) fn defer_render_effects(
        &mut self,
        effects: impl IntoIterator<Item = RuntimeSideEffect>,
    ) {
        for effect in effects {
            let RuntimeSideEffect::RenderClient {
                client_id,
                reason: incoming_reason,
            } = effect
            else {
                debug_assert!(
                    false,
                    "presentation render queue received a non-render effect"
                );
                continue;
            };
            let existing_reason =
                self.deferred_render_effects
                    .iter_mut()
                    .find_map(|effect| match effect {
                        RuntimeSideEffect::RenderClient {
                            client_id: existing_client_id,
                            reason,
                        } if existing_client_id == &client_id => Some(reason),
                        _ => None,
                    });
            if let Some(existing_reason) = existing_reason {
                if presentation_render_invalidation_priority(incoming_reason)
                    > presentation_render_invalidation_priority(*existing_reason)
                {
                    *existing_reason = incoming_reason;
                }
            } else {
                self.deferred_render_effects
                    .push(RuntimeSideEffect::RenderClient {
                        client_id,
                        reason: incoming_reason,
                    });
            }
        }
    }

    /// Drains render effects through the transport-neutral deferred-effect boundary.
    pub(crate) fn take_deferred_render_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.deferred_render_effects)
    }

    /// Clears an in-progress pane-resize gesture after layout mutation.
    pub(crate) fn clear_mouse_resize_drag_state(&mut self) {
        self.mouse_resize_drag_state = None;
    }

    /// Reports whether a pane-divider resize gesture is active.
    pub(crate) fn mouse_resize_drag_active(&self) -> bool {
        self.mouse_resize_drag_state.is_some()
    }

    /// Rebinds transient projections to one cheap provisional screen resize.
    ///
    /// Baseline screens are resized with the same bounded terminal operation
    /// as the live screen. Canonical semantic replay remains deferred, while
    /// streaming and shell-preview updates retain authority over the interim
    /// generation.
    pub(crate) fn rebase_agent_presentations_after_provisional_resize(
        &mut self,
        pane_id: &str,
        previous_lineage: u64,
        resized_lineage: u64,
        size: Size,
    ) {
        if let Some(preview) = self.agent_shell_output_previews.get_mut(pane_id)
            && preview.installed_lineage == previous_lineage
        {
            let mut baseline = preview.baseline_screen.as_ref().clone();
            baseline.resize(size);
            preview.baseline_screen = std::sync::Arc::new(baseline);
            preview.installed_lineage = resized_lineage;
        }
        if let Some(streaming) = self.agent_streaming_say_presentations.get_mut(pane_id)
            && streaming.installed_lineage == previous_lineage
        {
            let mut baseline = streaming.baseline_screen.as_ref().clone();
            baseline.resize(size);
            let mut provider = streaming.provider_screen.as_ref().clone();
            provider.resize(size);
            streaming.baseline_screen = std::sync::Arc::new(baseline);
            streaming.provider_screen = std::sync::Arc::new(provider);
            streaming.installed_lineage = resized_lineage;
            streaming.projected_revision = None;
            streaming.projected_context = None;
            streaming.projected_actions = None;
            streaming.projected_rationale = None;
            streaming.projected_lineage = None;
        }
    }

    /// Coalesces source-backed agent presentation replay to one final pane size.
    pub(crate) fn defer_agent_presentation_resize(&mut self, pane_id: &str, size: Size) {
        self.pending_agent_presentation_resize_sizes
            .insert(pane_id.to_string(), size);
        self.pending_agent_presentation_resize_dispatches
            .insert(pane_id.to_string());
    }

    /// Reports whether one pane has deferred agent presentation replay.
    #[cfg(test)]
    pub(crate) fn agent_presentation_resize_is_deferred(&self, pane_id: &str) -> bool {
        self.pending_agent_presentation_resize_sizes
            .contains_key(pane_id)
    }

    /// Drains one coalesced worker dispatch per pane while retaining its newest size.
    pub(crate) fn take_agent_presentation_resize_dispatch_effects(
        &mut self,
    ) -> Vec<RuntimeSideEffect> {
        let debounce_ms = self.settings.terminal_resize_debounce_ms.max(1);
        std::mem::take(&mut self.pending_agent_presentation_resize_dispatches)
            .into_iter()
            .map(
                |pane_id| RuntimeSideEffect::DispatchAgentPresentationResize {
                    pane_id,
                    debounce_ms,
                },
            )
            .collect()
    }

    /// Requests another worker dispatch for every resize retained during a drag.
    pub(crate) fn redispatch_pending_agent_presentation_resizes(&mut self) {
        self.pending_agent_presentation_resize_dispatches
            .extend(self.pending_agent_presentation_resize_sizes.keys().cloned());
    }
}

impl RuntimeSessionService {
    /// Captures presentation state owned only by one pane's resume transition.
    pub(crate) fn snapshot_agent_resume_presentation(
        &self,
        pane_id: &str,
    ) -> RuntimeAgentResumePresentationSnapshot {
        RuntimeAgentResumePresentationSnapshot {
            prompt_input: self.presentation.agent_prompt_inputs.get(pane_id).cloned(),
            shell_output_previews: self
                .presentation
                .agent_shell_output_previews
                .get(pane_id)
                .cloned(),
            streaming_say_presentation: self
                .presentation
                .agent_streaming_say_presentations
                .get(pane_id)
                .cloned(),
            promoted_streaming_say_actions: self
                .presentation
                .agent_promoted_streaming_say_actions
                .iter()
                .filter(|((candidate_pane_id, _turn_id), _indices)| candidate_pane_id == pane_id)
                .map(|(key, indices)| (key.clone(), indices.clone()))
                .collect(),
            projection: self
                .presentation
                .agent_presentation_projection_cache
                .get(pane_id)
                .cloned(),
            pending_resize: self
                .presentation
                .pending_agent_presentation_resize_sizes
                .get(pane_id)
                .copied(),
            replay_active: self
                .presentation
                .agent_presentation_replay_panes
                .contains(pane_id),
            copy_modes: self
                .presentation
                .copy
                .active_copy_modes
                .iter()
                .filter(|((candidate, _), _)| candidate == pane_id)
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            scrollback_surfaces: self
                .presentation
                .copy
                .scrollback_copy_mode_panes
                .iter()
                .filter(|(candidate, _)| candidate == pane_id)
                .cloned()
                .collect(),
            mouse_selection_drag_state: self
                .presentation
                .mouse_selection_drag_state
                .as_ref()
                .filter(|state| state.pane_id == pane_id)
                .cloned(),
            last_mouse_click_state: self
                .presentation
                .last_mouse_click_state
                .as_ref()
                .filter(|state| state.pane_id == pane_id)
                .cloned(),
            deferred_word_copy_cleanup: self
                .presentation
                .deferred_word_copy_cleanup
                .borrow()
                .as_ref()
                .filter(|(candidate, _, _, _)| candidate == pane_id)
                .cloned(),
        }
    }

    /// Restores one pane's exact presentation state after resume failure.
    pub(crate) fn restore_agent_resume_presentation(
        &mut self,
        pane_id: &str,
        mut snapshot: RuntimeAgentResumePresentationSnapshot,
    ) {
        let current_conversation = self
            .agent_pane_screen_state(pane_id)
            .map(|state| state.conversation_id().to_string());
        let current_lineage = current_conversation
            .as_deref()
            .and_then(|conversation_id| self.agent_pane_screen_lineage(pane_id, conversation_id));
        snapshot.shell_output_previews = snapshot.shell_output_previews.take().filter(|preview| {
            current_conversation.as_deref() == Some(preview.conversation_id.as_str())
                && current_lineage == Some(preview.installed_lineage)
        });
        snapshot.streaming_say_presentation =
            snapshot
                .streaming_say_presentation
                .take()
                .filter(|streaming| {
                    current_conversation.as_deref() == Some(streaming.conversation_id.as_str())
                        && current_lineage == Some(streaming.installed_lineage)
                });
        if snapshot.streaming_say_presentation.is_none() {
            snapshot.promoted_streaming_say_actions.clear();
        }
        self.presentation.agent_prompt_inputs.remove(pane_id);
        self.presentation
            .agent_prompt_selector_refreshes
            .retain(|(_, candidate), _| candidate != pane_id);
        if let Some(value) = snapshot.prompt_input {
            self.presentation
                .agent_prompt_inputs
                .insert(pane_id.to_string(), value);
        }
        self.presentation
            .agent_shell_output_previews
            .remove(pane_id);
        if let Some(value) = snapshot.shell_output_previews {
            self.presentation
                .agent_shell_output_previews
                .insert(pane_id.to_string(), value);
        }
        self.presentation
            .agent_streaming_say_presentations
            .remove(pane_id);
        if let Some(value) = snapshot.streaming_say_presentation {
            self.presentation
                .agent_streaming_say_presentations
                .insert(pane_id.to_string(), value);
        }
        self.presentation
            .agent_promoted_streaming_say_actions
            .retain(|(candidate_pane_id, _turn_id), _indices| candidate_pane_id != pane_id);
        self.presentation
            .agent_promoted_streaming_say_actions
            .extend(snapshot.promoted_streaming_say_actions);
        self.presentation
            .agent_presentation_projection_cache
            .remove(pane_id);
        if let Some(value) = snapshot.projection {
            self.presentation
                .agent_presentation_projection_cache
                .insert(pane_id.to_string(), value);
        }
        self.presentation
            .pending_agent_presentation_resize_sizes
            .remove(pane_id);
        if let Some(value) = snapshot.pending_resize {
            self.presentation
                .pending_agent_presentation_resize_sizes
                .insert(pane_id.to_string(), value);
        }
        self.presentation
            .agent_presentation_replay_panes
            .remove(pane_id);
        if snapshot.replay_active {
            self.presentation
                .agent_presentation_replay_panes
                .insert(pane_id.to_string());
        }
        self.clear_copy_state_for_pane(pane_id);
        self.presentation
            .copy
            .active_copy_modes
            .extend(snapshot.copy_modes);
        self.presentation
            .copy
            .scrollback_copy_mode_panes
            .extend(snapshot.scrollback_surfaces);
        if let Some(state) = snapshot.mouse_selection_drag_state {
            self.presentation.mouse_selection_drag_state = Some(state);
        }
        if let Some(state) = snapshot.last_mouse_click_state {
            self.presentation.last_mouse_click_state = Some(state);
        }
        if let Some(state) = snapshot.deferred_word_copy_cleanup {
            self.presentation
                .deferred_word_copy_cleanup
                .replace(Some(state));
        }
    }

    /// Retains one pane's parent projection before an ephemeral loop rebinds it.
    pub(crate) fn retain_agent_loop_parent_projection(&mut self, loop_id: &str, pane_id: &str) {
        if self
            .presentation
            .agent_loop_parent_projections
            .contains_key(loop_id)
        {
            return;
        }
        let agent_screen = self
            .agent_pane_screen_state(pane_id)
            .map(|state| (state.conversation_id().to_string(), state.screen().clone()));
        let presentation = self.snapshot_agent_resume_presentation(pane_id);
        self.presentation.agent_loop_parent_projections.insert(
            loop_id.to_string(),
            RuntimeAgentLoopParentProjectionSnapshot {
                pane_id: pane_id.to_string(),
                agent_screen,
                presentation,
            },
        );
    }

    /// Restores and consumes the exact parent projection retained for one loop.
    pub(crate) fn restore_agent_loop_parent_projection(&mut self, loop_id: &str, pane_id: &str) {
        let Some(snapshot) = self
            .presentation
            .agent_loop_parent_projections
            .remove(loop_id)
        else {
            return;
        };
        if snapshot.pane_id != pane_id {
            return;
        }
        if let Some((conversation_id, screen)) = snapshot.agent_screen {
            self.set_agent_pane_screen(pane_id, conversation_id, screen);
        } else {
            self.remove_agent_pane_screen(pane_id);
        }
        self.restore_agent_resume_presentation(pane_id, snapshot.presentation);
    }

    /// Discards retained loop projections owned by a pane that is being removed.
    pub(crate) fn discard_agent_loop_parent_projections_for_pane(&mut self, pane_id: &str) {
        self.presentation
            .agent_loop_parent_projections
            .retain(|_, snapshot| snapshot.pane_id != pane_id);
    }

    /// Acknowledges pending completion attention for the currently focused pane.
    pub(crate) fn acknowledge_focused_pane_completion(&mut self) {
        if let Ok(pane_id) = self.active_pane_id() {
            self.presentation
                .acknowledge_completion_attention(pane_id.as_str());
        }
    }

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

    /// Returns the structured blocking-editor command candidates.
    pub(crate) fn external_editor_config(&self) -> &crate::runtime::RuntimeExternalEditorConfig {
        &self.presentation.settings.external_editor
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

    /// Returns active copy modes keyed by pane and retained surface.
    pub(crate) fn active_copy_modes(
        &self,
    ) -> &std::collections::BTreeMap<(String, PaneSurfaceKind), CopyMode> {
        &self.presentation.copy.active_copy_modes
    }

    /// Returns mutable surface-qualified copy modes to copy and process adapters.
    pub(crate) fn active_copy_modes_mut(
        &mut self,
    ) -> &mut std::collections::BTreeMap<(String, PaneSurfaceKind), CopyMode> {
        &mut self.presentation.copy.active_copy_modes
    }

    /// Returns the interaction key for a pane's currently presented surface.
    pub(crate) fn presented_copy_mode_key(&self, pane_id: &str) -> (String, PaneSurfaceKind) {
        self.copy_mode_key(pane_id, self.presented_pane_surface(pane_id))
    }

    /// Returns the interaction key for one explicitly owned pane surface.
    pub(crate) fn copy_mode_key(
        &self,
        pane_id: &str,
        surface: PaneSurfaceKind,
    ) -> (String, PaneSurfaceKind) {
        (pane_id.to_string(), surface)
    }

    /// Returns the retained copy mode for a pane's currently presented surface.
    pub(crate) fn active_copy_mode_for_presented_surface(
        &self,
        pane_id: &str,
    ) -> Option<&CopyMode> {
        let key = self.presented_copy_mode_key(pane_id);
        self.presentation.copy.active_copy_modes.get(&key)
    }

    /// Returns mutable copy state for a pane's currently presented surface.
    pub(crate) fn active_copy_mode_for_presented_surface_mut(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut CopyMode> {
        let key = self.presented_copy_mode_key(pane_id);
        self.presentation.copy.active_copy_modes.get_mut(&key)
    }

    /// Installs copy state for a pane's currently presented surface.
    pub(crate) fn insert_active_copy_mode_for_presented_surface(
        &mut self,
        pane_id: &str,
        copy_mode: CopyMode,
    ) {
        let key = self.presented_copy_mode_key(pane_id);
        self.presentation
            .copy
            .active_copy_modes
            .insert(key, copy_mode);
    }

    /// Removes copy state for a pane's currently presented surface.
    pub(crate) fn remove_active_copy_mode_for_presented_surface(
        &mut self,
        pane_id: &str,
    ) -> Option<CopyMode> {
        let key = self.presented_copy_mode_key(pane_id);
        self.presentation.copy.active_copy_modes.remove(&key)
    }

    /// Reports whether transient scrollback copy mode owns the presented surface.
    pub(crate) fn presented_surface_uses_scrollback_copy_mode(&self, pane_id: &str) -> bool {
        let key = self.presented_copy_mode_key(pane_id);
        self.presentation
            .copy
            .scrollback_copy_mode_panes
            .contains(&key)
    }

    /// Marks the pane's currently presented surface as transient scrollback copy mode.
    pub(crate) fn mark_presented_surface_scrollback_copy_mode(&mut self, pane_id: &str) {
        let key = self.presented_copy_mode_key(pane_id);
        self.presentation
            .copy
            .scrollback_copy_mode_panes
            .insert(key);
    }

    /// Removes transient scrollback state from the pane's presented surface.
    pub(crate) fn remove_presented_surface_scrollback_copy_mode(&mut self, pane_id: &str) -> bool {
        let key = self.presented_copy_mode_key(pane_id);
        self.presentation
            .copy
            .scrollback_copy_mode_panes
            .remove(&key)
    }

    /// Clears copy and transient scrollback state for the presented surface only.
    pub(crate) fn clear_copy_state_for_presented_surface(&mut self, pane_id: &str) {
        let surface = self.presented_pane_surface(pane_id);
        self.clear_copy_state_for_surface(pane_id, surface);
    }

    /// Clears copy and transient scrollback state for one explicit surface.
    pub(crate) fn clear_copy_state_for_surface(&mut self, pane_id: &str, surface: PaneSurfaceKind) {
        let key = self.copy_mode_key(pane_id, surface);
        self.presentation.copy.active_copy_modes.remove(&key);
        self.presentation
            .copy
            .scrollback_copy_mode_panes
            .remove(&key);
    }

    /// Removes copy and transient scrollback state for every surface of a pane.
    pub(crate) fn clear_copy_state_for_pane(&mut self, pane_id: &str) {
        self.presentation
            .copy
            .active_copy_modes
            .retain(|(candidate, _), _| candidate != pane_id);
        self.presentation
            .copy
            .scrollback_copy_mode_panes
            .retain(|(candidate, _)| candidate != pane_id);
        if self
            .presentation
            .mouse_selection_drag_state
            .as_ref()
            .is_some_and(|state| state.pane_id == pane_id)
        {
            self.presentation.mouse_selection_drag_state = None;
        }
        if self
            .presentation
            .last_mouse_click_state
            .as_ref()
            .is_some_and(|state| state.pane_id == pane_id)
        {
            self.presentation.last_mouse_click_state = None;
        }
        if self
            .presentation
            .deferred_word_copy_cleanup
            .borrow()
            .as_ref()
            .is_some_and(|(candidate, _, _, _)| candidate == pane_id)
        {
            self.presentation.deferred_word_copy_cleanup.replace(None);
        }
    }

    /// Removes pane-scoped interaction state from every exact client.
    pub(crate) fn clear_client_interaction_state_for_removed_pane(&mut self, pane_id: &str) {
        self.presentation.remove_pane_state_for_all_clients(pane_id);
    }

    /// Clears copy and transient mouse state owned by one retained pane surface.
    pub(crate) fn clear_interaction_state_for_surface(
        &mut self,
        pane_id: &str,
        surface: PaneSurfaceKind,
    ) {
        self.clear_copy_state_for_surface(pane_id, surface);
        if self
            .presentation
            .mouse_selection_drag_state
            .as_ref()
            .is_some_and(|state| state.pane_id == pane_id && state.surface == surface)
        {
            self.presentation.mouse_selection_drag_state = None;
        }
        if self
            .presentation
            .last_mouse_click_state
            .as_ref()
            .is_some_and(|state| state.pane_id == pane_id && state.surface == surface)
        {
            self.presentation.last_mouse_click_state = None;
        }
        if self
            .presentation
            .deferred_word_copy_cleanup
            .borrow()
            .as_ref()
            .is_some_and(|(candidate, candidate_surface, _, _)| {
                candidate == pane_id && *candidate_surface == surface
            })
        {
            self.presentation.deferred_word_copy_cleanup.replace(None);
        }
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
        self.presentation
            .agent_prompt_selector_refreshes
            .retain(|(_, candidate), _| candidate != pane_id);
        self.presentation.agent_prompt_inputs.remove(pane_id)
    }

    /// Returns mutable agent prompt editor state for one pane.
    pub(crate) fn agent_prompt_input_mut(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut RuntimeAgentPromptInput> {
        self.presentation.agent_prompt_inputs.get_mut(pane_id)
    }

    /// Clones one pane prompt from the exact client's transient presentation.
    pub(crate) fn agent_prompt_input_for_client(
        &mut self,
        client_id: &mez_core::ids::ClientId,
        pane_id: &str,
    ) -> Option<RuntimeAgentPromptInput> {
        self.presentation.capture_projected_client_state();
        self.presentation
            .client_states
            .get(client_id)
            .and_then(|state| state.agent_prompt_inputs.get(pane_id))
            .cloned()
    }

    /// Replaces one pane prompt in the exact client's transient presentation.
    pub(crate) fn set_agent_prompt_input_for_client(
        &mut self,
        client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        prompt_input: RuntimeAgentPromptInput,
    ) {
        self.presentation.capture_projected_client_state();
        self.presentation
            .agent_prompt_selector_refreshes
            .remove(&(client_id.clone(), pane_id.to_string()));
        if self.presentation.projected_client_id.as_ref() == Some(client_id) {
            self.presentation
                .agent_prompt_inputs
                .insert(pane_id.to_string(), prompt_input);
            self.presentation.capture_projected_client_state();
            return;
        }
        let state = self
            .presentation
            .client_states
            .entry(client_id.clone())
            .or_default();
        state
            .agent_prompt_inputs
            .insert(pane_id.to_string(), prompt_input);
        state.presentation_revision = state.presentation_revision.saturating_add(1);
    }

    /// Marks runtime-provided agent selector candidates stale for every prompt.
    ///
    /// The next input batch starts a nonblocking refresh after the durable
    /// source backing completions has changed. Existing candidates remain
    /// available until the replacement snapshot completes.
    pub(crate) fn invalidate_agent_prompt_selector_extra_candidates(&mut self) {
        for prompt in self.presentation.agent_prompt_inputs.values_mut() {
            prompt.selector_extra_candidates_loaded = false;
            prompt.selector_extra_candidates_generation = prompt
                .selector_extra_candidates_generation
                .saturating_add(1);
        }
        self.presentation.agent_prompt_selector_refreshes.clear();
        let pane_ids = self
            .presentation
            .agent_prompt_inputs
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for pane_id in pane_ids {
            self.request_agent_prompt_selector_extra_candidates_refresh(&pane_id);
        }
    }

    /// Clears every active agent prompt editor during lifecycle teardown.
    pub(crate) fn clear_agent_prompt_inputs(&mut self) {
        self.presentation.agent_prompt_inputs.clear();
        self.presentation.agent_prompt_selector_refreshes.clear();
        self.presentation.external_agent_prompt_edits.clear();
    }

    /// Drains command-backed status pill refreshes scheduled during rendering.
    pub(crate) fn drain_status_pill_refresh_transition(&self) -> RuntimeTransition {
        let plans = self
            .presentation
            .window_status_pill_cache
            .borrow_mut()
            .drain_refresh_plans();
        RuntimeTransition {
            applied: false,
            side_effects: plans
                .into_iter()
                .map(|plan| RuntimeSideEffect::RefreshStatusPill { plan })
                .collect(),
        }
    }

    /// Applies a current status pill completion and reports visible change.
    pub(crate) fn apply_status_pill_event(
        &self,
        event: crate::runtime::RuntimeStatusPillEvent,
    ) -> Option<bool> {
        self.presentation
            .window_status_pill_cache
            .borrow_mut()
            .apply_event(
                &self.presentation.settings.window_status_pill_definitions,
                &self
                    .presentation
                    .settings
                    .window_frame_right_status_template,
                event,
            )
    }
}

impl RuntimeSessionService {
    /// Returns the exact render identity currently owned by an attached primary.
    pub(crate) fn client_render_identity(
        &mut self,
        client_id: &mez_core::ids::ClientId,
    ) -> Result<(String, u64, u64, u64)> {
        self.prepare_client_render(client_id, ClientViewRole::Primary)?;
        let window_id = self.session.active_window_for(client_id)?.id.to_string();
        let navigation_revision = self.session.navigation(client_id)?.revision;
        let layout_revision = self.session.mutation_revision();
        let presentation_revision = self.presentation.client_presentation_revision(client_id);
        Ok((
            window_id,
            navigation_revision,
            layout_revision,
            presentation_revision,
        ))
    }

    /// Cancels coordinate input derived from a stale frame and notifies its owner.
    pub(crate) fn cancel_stale_client_coordinate_input(
        &mut self,
        client_id: &mez_core::ids::ClientId,
    ) -> Result<AttachedClientStepApplication> {
        self.prepare_client_render(client_id, ClientViewRole::Primary)?;
        self.show_primary_notice_overlay(vec![
            "view changed; pointer input was cancelled".to_string(),
        ])?;
        self.presentation.capture_projected_client_state();
        Ok(AttachedClientStepApplication {
            view_refresh_required: true,
            full_redraw_required: true,
            ..AttachedClientStepApplication::default()
        })
    }
}

#[cfg(test)]
impl RuntimeSessionService {
    /// Replaces the active UI theme for a presentation integration fixture.
    pub(crate) fn set_ui_theme_for_tests(&mut self, ui_theme: UiTheme) {
        self.presentation.settings.ui_theme = ui_theme;
    }

    /// Returns retained primary command-prompt history for integration tests.
    pub(crate) fn primary_command_prompt_history(&self) -> Vec<String> {
        self.presentation
            .primary_command_prompt_history
            .iter()
            .map(|entry| entry.text.clone())
            .collect()
    }

    /// Replaces retained command-prompt history for an integration fixture.
    pub(crate) fn set_primary_command_prompt_history_for_tests(&mut self, history: Vec<String>) {
        self.presentation.primary_command_prompt_history = history
            .into_iter()
            .map(mez_mux::readline::ReadlineHistoryEntry::literal)
            .collect();
    }

    /// Adds one command-prompt history entry for an integration fixture.
    pub(crate) fn push_primary_command_prompt_history_for_tests(&mut self, command: String) {
        self.presentation
            .primary_command_prompt_history
            .push(mez_mux::readline::ReadlineHistoryEntry::literal(command));
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

    /// Returns mutable overlay state for focused product integration tests.
    #[cfg(test)]
    pub(crate) fn primary_display_overlay_mut(&mut self) -> Option<&mut RuntimeDisplayOverlay> {
        self.presentation.primary_display_overlay.as_mut()
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

    /// Returns pane styled-row cache hit, miss, and entry counts for tests.
    #[cfg(test)]
    pub(crate) fn pane_styled_row_cache_stats_for_tests(&self) -> (u64, u64, usize) {
        let cache = self.presentation.pane_styled_row_cache.borrow();
        (cache.hits, cache.misses, cache.rows.len())
    }

    /// Returns window presentation-plan cache hit, miss, and entry counts.
    #[cfg(test)]
    pub(crate) fn window_presentation_plan_cache_stats_for_tests(&self) -> (u64, u64, usize) {
        let cache = self.presentation.window_presentation_plan_cache.borrow();
        (cache.hits, cache.misses, usize::from(cache.entry.is_some()))
    }

    /// Returns the cached presentation plan for one test window snapshot.
    #[cfg(test)]
    pub(crate) fn window_presentation_plan_for_tests(
        &self,
        window: &mez_mux::layout::Window,
    ) -> Option<std::sync::Arc<mez_mux::presentation::WindowPresentationPlan>> {
        self.window_presentation_plan(window)
    }

    /// Returns deferred copied-word cleanup state for product integration tests.
    pub(crate) fn deferred_word_copy_cleanup(
        &self,
    ) -> &std::cell::RefCell<Option<(String, PaneSurfaceKind, CopyMode, u64)>> {
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
use crate::ui::selector::SelectorSurface;
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
mod external_prompt;
pub(in crate::runtime) use external_prompt::{
    ExternalPromptEditSettlement, normalize_external_agent_prompt,
};
mod harness_status;
pub(crate) use harness_status::RuntimePaneHarnessStatus;
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
#[cfg(test)]
pub(crate) use overlay::RuntimeCommandDisplayOverlayContent;
pub(in crate::runtime) use overlay::default_runtime_agent_prompt_input;
use overlay::{
    RuntimeAgentShellDisplayOutput, agent_command_link_at_line_column,
    agent_shell_mcp_display_state_name, runtime_agent_shell_display_output,
    runtime_agent_shell_visibility, runtime_command_display_overlay_content,
    runtime_command_display_should_open_overlay, runtime_pane_agent_selector_rendition,
    runtime_pane_agent_status_selector_layout, runtime_primary_prompt_input, runtime_selector_line,
};
#[cfg(test)]
use overlay::{runtime_agent_shell_markdown_overlay_content, runtime_human_readable_display_lines};
use presentation::{
    AgentTerminalPresentationStyle, agent_display_lines_are_error,
    agent_display_lines_are_low_level_status, agent_prompt_error_display_lines,
    overlay_styled_lines, render_command_markdown_body_lines_for_width,
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
