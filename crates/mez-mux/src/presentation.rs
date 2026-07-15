//! Dependency-neutral multiplexer presentation contracts.
//!
//! This module owns small value types used to compose terminal surfaces into
//! pane and window presentation. Product configuration and agent-specific
//! frame metadata remain in the Mezzanine composition crate.

use std::collections::BTreeMap;

use mez_terminal::{TerminalSize, TerminalStyleSpan};

use crate::copy::CopyPosition;
use crate::layout::{PaneGeometry, range_overlap_u16};
use crate::theme::UiTheme;

/// Cursor shape used when presenting an attached interactive surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalCursorStyle {
    /// A full-cell block cursor.
    #[default]
    Block,
    /// An underline cursor.
    Underline,
    /// A vertical bar cursor.
    Bar,
}

impl TerminalCursorStyle {
    /// Returns the DECSCUSR parameter for this shape and blink behavior.
    pub const fn decscusr_parameter(self, blink: bool) -> u8 {
        match (self, blink) {
            (Self::Block, true) => 1,
            (Self::Block, false) => 2,
            (Self::Underline, true) => 3,
            (Self::Underline, false) => 4,
            (Self::Bar, true) => 5,
            (Self::Bar, false) => 6,
        }
    }
}

/// Role of one rendered attached-client view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientViewRole {
    /// Primary interactive client.
    Primary,
    /// Observer awaiting promotion or initial synchronization.
    PendingObserver,
    /// Read-only observer client.
    Observer,
}

/// Absolute client-space region reserved for a readline prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadlinePromptRegion {
    /// Top row of the region.
    pub row: usize,
    /// Left column of the region.
    pub column: usize,
    /// Width of the region in terminal cells.
    pub columns: usize,
    /// Height of the region in terminal cells.
    pub rows: usize,
}

/// Transport-neutral rendered attached-client viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedClientView {
    /// Client role for which this view was rendered.
    pub role: ClientViewRole,
    /// Authoritative multiplexer size used to compose the view.
    pub authoritative_size: TerminalSize,
    /// Actual attached-client size.
    pub client_size: TerminalSize,
    /// Rendered text rows.
    pub lines: Vec<String>,
    /// Per-line non-default SGR style spans aligned with `lines`.
    pub line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Active copy-mode selection range.
    pub selection: Option<(CopyPosition, CopyPosition)>,
    /// Whether presenting the view requires host-client scrolling.
    pub requires_client_scroll: bool,
    /// First visible source row.
    pub viewport_row: usize,
    /// First visible source column.
    pub viewport_column: usize,
    /// Cursor row in client coordinates.
    pub cursor_row: usize,
    /// Cursor column in client coordinates.
    pub cursor_column: usize,
    /// Whether the cursor is visible.
    pub cursor_visible: bool,
    /// Cursor shape requested from the host terminal.
    pub cursor_style: TerminalCursorStyle,
    /// Whether the host cursor should blink.
    pub cursor_blink: bool,
    /// Cursor blink interval in milliseconds.
    pub cursor_blink_interval_ms: u64,
    /// Whether application-keypad mode is active.
    pub application_keypad: bool,
    /// Whether host bracketed paste should be enabled.
    pub bracketed_paste: bool,
    /// Whether host focus reporting should be enabled.
    pub focus_events: bool,
    /// Whether the active pane is using its alternate screen.
    pub alternate_screen: bool,
    /// Whether host mouse reporting should be enabled.
    pub host_mouse_reporting: bool,
    /// Requested animation refresh cadence in milliseconds, or zero.
    pub animation_refresh_interval_ms: u64,
    /// Resolved UI theme used for this view.
    pub ui_theme: UiTheme,
    /// Absolute client-space prompt region, when present.
    pub agent_prompt_region: Option<ReadlinePromptRegion>,
    /// Whether the primary prompt is active.
    pub primary_prompt_active: bool,
}

/// Host-terminal modes required to present one attached-client frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachedTerminalOutputModes {
    /// Whether application-keypad mode should be enabled.
    pub application_keypad: bool,
    /// Whether bracketed-paste mode should be enabled.
    pub bracketed_paste: bool,
    /// Whether focus-event reporting should be enabled.
    pub focus_events: bool,
    /// Whether the active pane is using its alternate screen.
    pub alternate_screen: bool,
    /// Whether host mouse reporting should be enabled.
    pub host_mouse_reporting: bool,
    /// Host cursor shape.
    pub cursor_style: TerminalCursorStyle,
    /// Whether the host cursor should blink.
    pub cursor_blink: bool,
    /// Cursor blink interval in milliseconds.
    pub cursor_blink_interval_ms: u64,
    /// Elapsed cursor blink phase in milliseconds.
    pub cursor_blink_elapsed_ms: u64,
    /// Requested animation refresh cadence in milliseconds, or zero.
    pub animation_refresh_interval_ms: u64,
    /// Whether the cursor should be visible in this phase.
    pub cursor_visible: bool,
    /// Cursor row in client coordinates.
    pub cursor_row: usize,
    /// Cursor column in client coordinates.
    pub cursor_column: usize,
}

impl Default for AttachedTerminalOutputModes {
    fn default() -> Self {
        Self {
            application_keypad: false,
            bracketed_paste: false,
            focus_events: false,
            alternate_screen: false,
            host_mouse_reporting: true,
            cursor_style: TerminalCursorStyle::default(),
            cursor_blink: false,
            cursor_blink_interval_ms: 500,
            cursor_blink_elapsed_ms: 0,
            animation_refresh_interval_ms: 0,
            cursor_visible: false,
            cursor_row: 0,
            cursor_column: 0,
        }
    }
}

/// Transport-neutral result of one attached-client planning step.
///
/// The mux owns the output presentation and lifecycle envelope while callers
/// specialize the action and host-error role types at the product boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedClientStepPlan<Action, ErrorRole> {
    /// Input or mux actions selected during this planning step.
    pub actions: Vec<Action>,
    /// Rendered output lines ready for host presentation.
    pub output_lines: Vec<String>,
    /// Per-line non-default SGR style spans aligned with `output_lines`.
    pub output_line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Whether the attached input endpoint reported a hangup.
    pub input_hangup: bool,
    /// Whether the attached output endpoint reported a hangup.
    pub output_hangup: bool,
    /// Host endpoint roles that reported errors during this step.
    pub error_roles: Vec<ErrorRole>,
}

/// Transport-neutral readiness for one attached-client endpoint.
///
/// Product adapters identify which endpoints carry client input or output;
/// the mux then derives the lifecycle and I/O state needed by one headless
/// planning step without depending on file descriptors or host polling APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachedClientEndpointReadiness<Role> {
    /// Product-owned identity used when reporting endpoint errors.
    pub role: Role,
    /// Whether this endpoint carries attached-client input.
    pub input: bool,
    /// Whether this endpoint carries attached-client output.
    pub output: bool,
    /// Whether input is currently available.
    pub readable: bool,
    /// Whether output can currently be written.
    pub writable: bool,
    /// Whether the endpoint reported a hangup.
    pub hangup: bool,
    /// Whether the endpoint reported an error.
    pub error: bool,
}

/// Aggregated readiness and lifecycle state for one attached-client step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedClientReadiness<Role> {
    /// Whether an attached-client input endpoint is readable.
    pub input_readable: bool,
    /// Whether an attached-client output endpoint is writable.
    pub output_writable: bool,
    /// Whether an attached-client input endpoint reported a hangup.
    pub input_hangup: bool,
    /// Whether an attached-client output endpoint reported a hangup.
    pub output_hangup: bool,
    /// Product-owned endpoint roles that reported errors.
    pub error_roles: Vec<Role>,
}

/// Aggregates host endpoint readiness into the transport-neutral state used by
/// attached-client planning.
pub fn classify_attached_client_readiness<Role: Copy>(
    endpoints: impl IntoIterator<Item = AttachedClientEndpointReadiness<Role>>,
) -> AttachedClientReadiness<Role> {
    let mut state = AttachedClientReadiness {
        input_readable: false,
        output_writable: false,
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    for endpoint in endpoints {
        state.input_readable |= endpoint.input && endpoint.readable;
        state.output_writable |= endpoint.output && endpoint.writable;
        state.input_hangup |= endpoint.input && endpoint.hangup;
        state.output_hangup |= endpoint.output && endpoint.hangup;
        if endpoint.error {
            state.error_roles.push(endpoint.role);
        }
    }

    state
}

/// Completes one transport-neutral attached-client planning step.
///
/// Product adapters remain responsible for decoding host input and composing
/// styled viewport rows. The mux owns the final readiness gate and lifecycle
/// envelope so headless clients do not need to duplicate output suppression or
/// hangup/error propagation.
pub fn plan_attached_client_step<Action, ErrorRole>(
    readiness: AttachedClientReadiness<ErrorRole>,
    actions: Vec<Action>,
    output: Option<(Vec<String>, Vec<Vec<TerminalStyleSpan>>)>,
) -> AttachedClientStepPlan<Action, ErrorRole> {
    let (output_lines, output_line_style_spans) = if readiness.output_writable {
        output.unwrap_or_default()
    } else {
        (Vec::new(), Vec::new())
    };

    AttachedClientStepPlan {
        actions,
        output_lines,
        output_line_style_spans,
        input_hangup: readiness.input_hangup,
        output_hangup: readiness.output_hangup,
        error_roles: readiness.error_roles,
    }
}

/// Per-pane metadata consumed by mux frame and body presentation.
///
/// Scalar fields are presentation-only values. The prompt and supplemental
/// body-line types remain generic so the product can supply richer UI state
/// without making the mux depend on product-owned prompt or agent types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPaneFrameContext<Prompt = (), DisplayLines = Vec<String>> {
    /// Primary process id shown by `pane.primary_pid`.
    pub primary_pid: Option<u32>,
    /// Primary process name shown by `pane.process_name` when known.
    pub process_name: Option<String>,
    /// Primary process exit status shown by `pane.exit_status` when known.
    pub exit_status: Option<String>,
    /// Home-relative current working directory shown by `pane.pwd` when known.
    pub current_working_directory: Option<String>,
    /// Current pane interaction mode shown by `pane.mode`.
    pub mode: Option<String>,
    /// Opaque agent identity shown by `agent.id`.
    pub agent_id: Option<String>,
    /// Human-readable agent label shown by `agent.name`.
    pub agent_name: Option<String>,
    /// Opaque agent state shown by `agent.status`.
    pub agent_status: Option<String>,
    /// Active model label shown by `agent.model`.
    pub agent_model: Option<String>,
    /// Active reasoning label shown by `agent.reasoning`.
    pub agent_reasoning: Option<String>,
    /// Provider thinking-mode label shown by `agent.thinking`.
    pub agent_thinking: Option<String>,
    /// Pane-local routing label shown by `agent.routing`.
    pub agent_routing: Option<String>,
    /// Active latency label shown by `agent.latency`.
    pub agent_latency: Option<String>,
    /// Active model preset label shown by `agent.preset`.
    pub agent_preset: Option<String>,
    /// Last known input-context usage shown by `agent.context_usage`.
    pub agent_context_usage: Option<String>,
    /// Scrollback position shown by `history.position` when not at the live bottom.
    pub history_position: Option<String>,
    /// Product-owned prompt state rendered inside the pane body.
    pub agent_prompt: Option<Prompt>,
    /// Product-owned supplemental lines rendered above the prompt.
    pub agent_display_lines: DisplayLines,
}

impl<Prompt, DisplayLines: Default> Default for TerminalPaneFrameContext<Prompt, DisplayLines> {
    fn default() -> Self {
        Self {
            primary_pid: None,
            process_name: None,
            exit_status: None,
            current_working_directory: None,
            mode: None,
            agent_id: None,
            agent_name: None,
            agent_status: None,
            agent_model: None,
            agent_reasoning: None,
            agent_thinking: None,
            agent_routing: None,
            agent_latency: None,
            agent_preset: None,
            agent_context_usage: None,
            history_position: None,
            agent_prompt: None,
            agent_display_lines: DisplayLines::default(),
        }
    }
}

/// Runtime fields available to the right side of a window status line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalWindowStatusContext {
    /// Named-field template for the right status line.
    pub template: String,
    /// Home-relative active pane working directory shown by `pane.pwd`.
    pub active_pane_working_directory: Option<String>,
    /// Cached command-backed pill values keyed by configured pill name.
    pub status_pills: BTreeMap<String, String>,
    /// Human-readable system uptime shown by `system.uptime`.
    pub system_uptime: String,
    /// Human-readable local datetime shown by `datetime.local`.
    pub datetime_local: String,
}

/// Runtime window metadata made available to default window-frame rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalWindowFrameContext {
    /// Stable window identity.
    pub id: String,
    /// Display index in the session window list.
    pub index: usize,
    /// User-facing title or name for the window.
    pub title: String,
    /// Whether this window is currently focused.
    pub active: bool,
    /// Whether this window is dedicated to spawned subagent panes.
    pub subagent: bool,
}

/// Runtime window-group metadata made available to group-frame rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalWindowGroupFrameContext {
    /// Stable group identity.
    pub id: String,
    /// Display index in the session group list.
    pub index: usize,
    /// User-facing title or name for the group.
    pub title: String,
    /// Whether this group is currently focused.
    pub active: bool,
}

/// Placement of a one-row terminal frame within its owning region.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalFramePosition {
    /// Render the frame before region body content.
    #[default]
    Top,
    /// Render the frame after region body content.
    Bottom,
}

/// Places a one-row frame within an authoritative terminal region.
///
/// Top frames are inserted before body content, while bottom frames replace
/// the final available row. In both cases the result is clipped to the
/// authoritative row count.
pub fn place_window_frame<T>(
    lines: &mut Vec<T>,
    frame: T,
    position: TerminalFramePosition,
    authoritative_rows: u16,
) {
    let rows = usize::from(authoritative_rows);
    match position {
        TerminalFramePosition::Top => {
            lines.insert(0, frame);
            lines.truncate(rows);
        }
        TerminalFramePosition::Bottom => {
            lines.truncate(rows.saturating_sub(1));
            lines.push(frame);
            lines.truncate(rows);
        }
    }
}

/// Places a conditional top window-group frame above rendered window rows.
pub fn place_group_frame<T>(lines: &mut Vec<T>, frame: T, authoritative_rows: u16) {
    let rows = usize::from(authoritative_rows);
    lines.insert(0, frame);
    lines.truncate(rows);
}

/// Returns the drawable window body after reserving a mux-managed frame row.
pub fn rendered_window_body_size(size: TerminalSize, window_frames_enabled: bool) -> TerminalSize {
    let rows = if window_frames_enabled {
        size.rows.saturating_sub(1)
    } else {
        size.rows
    };
    TerminalSize {
        columns: size.columns,
        rows: rows.max(1),
    }
}

/// Returns whether a pane has a shared horizontal divider immediately below it.
pub fn geometry_has_bottom_divider(geometry: &PaneGeometry, geometries: &[PaneGeometry]) -> bool {
    let bottom = geometry.row.saturating_add(geometry.rows);
    geometries.iter().any(|candidate| {
        candidate.index != geometry.index
            && candidate.row == bottom
            && range_overlap_u16(
                geometry.column,
                geometry.column.saturating_add(geometry.columns),
                candidate.column,
                candidate.column.saturating_add(candidate.columns),
            ) > 0
    })
}

/// Returns whether a pane has a shared vertical divider immediately to its right.
pub fn geometry_has_right_divider(geometry: &PaneGeometry, geometries: &[PaneGeometry]) -> bool {
    let right = geometry.column.saturating_add(geometry.columns);
    geometries.iter().any(|candidate| {
        candidate.index != geometry.index
            && candidate.column == right
            && range_overlap_u16(
                geometry.row,
                geometry.row.saturating_add(geometry.rows),
                candidate.row,
                candidate.row.saturating_add(candidate.rows),
            ) > 0
    })
}

/// Returns the visible pane region after reserving shared divider cells.
pub fn pane_render_region_size_for_geometry(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
) -> TerminalSize {
    TerminalSize {
        columns: geometry
            .columns
            .saturating_sub(u16::from(geometry_has_right_divider(geometry, geometries)))
            .max(1),
        rows: geometry
            .rows
            .saturating_sub(u16::from(geometry_has_bottom_divider(geometry, geometries)))
            .max(1),
    }
}

/// Returns the pane body size available after divider and frame reservations.
pub fn pane_content_size_for_geometry(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
    pane_frames_enabled: bool,
    pane_frame_position: TerminalFramePosition,
) -> TerminalSize {
    let render_region = pane_render_region_size_for_geometry(geometry, geometries);
    let frame_rows = if pane_frames_enabled
        && !pane_frame_merges_into_divider(geometry, geometries, pane_frame_position)
    {
        1
    } else {
        0
    };
    TerminalSize {
        columns: render_region.columns,
        rows: render_region.rows.saturating_sub(frame_rows).max(1),
    }
}

/// Returns the rendered terminal row occupied by one pane frame.
///
/// Frames that merge into a shared horizontal divider use that divider row;
/// standalone frames use the pane render region's top or bottom edge.
pub fn pane_frame_row_for_geometry(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
    position: TerminalFramePosition,
    row_offset: u16,
) -> u16 {
    if pane_frame_merges_into_divider(geometry, geometries, position) {
        return row_offset.saturating_add(match position {
            TerminalFramePosition::Top => geometry.row.saturating_sub(1),
            TerminalFramePosition::Bottom => {
                geometry.row.saturating_add(geometry.rows).saturating_sub(1)
            }
        });
    }
    row_offset.saturating_add(match position {
        TerminalFramePosition::Top => geometry.row,
        TerminalFramePosition::Bottom => {
            let render_rows = pane_render_region_size_for_geometry(geometry, geometries).rows;
            geometry.row.saturating_add(render_rows).saturating_sub(1)
        }
    })
}

/// Bounded destination for one pane frame rendered on a shared divider row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergedPaneFramePlacement {
    /// Pane index in the owning window.
    pub pane_index: usize,
    /// Destination row in the window body canvas.
    pub row: usize,
    /// First destination column in the window body canvas.
    pub column_start: usize,
    /// Maximum number of terminal cells available to the frame.
    pub width: usize,
}

/// Computes pane-frame placements that occupy shared horizontal dividers.
pub fn merged_pane_frame_placements(
    geometries: &[PaneGeometry],
    position: TerminalFramePosition,
) -> Vec<MergedPaneFramePlacement> {
    geometries
        .iter()
        .filter(|geometry| pane_frame_merges_into_divider(geometry, geometries, position))
        .map(|geometry| MergedPaneFramePlacement {
            pane_index: geometry.index,
            row: usize::from(pane_frame_row_for_geometry(
                geometry, geometries, position, 0,
            )),
            column_start: usize::from(geometry.column),
            width: usize::from(pane_render_region_size_for_geometry(geometry, geometries).columns),
        })
        .collect()
}

/// Bounded destination for one rendered pane inside a window-body canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneCanvasPlacement {
    /// Index of the pane's geometry in the source geometry slice.
    pub source_index: usize,
    /// First destination row in the window body canvas.
    pub row_start: usize,
    /// First destination column in the window body canvas.
    pub column_start: usize,
    /// Number of pane rows visible in the destination canvas.
    pub pane_rows: usize,
    /// Number of pane columns visible in the destination canvas.
    pub pane_columns: usize,
}

/// Computes pane-to-canvas placements with divider reservations and clipping.
pub fn pane_canvas_placements(
    size: TerminalSize,
    geometries: &[PaneGeometry],
) -> Vec<PaneCanvasPlacement> {
    let rows = usize::from(size.rows);
    let columns = usize::from(size.columns);
    let mut placements = Vec::with_capacity(geometries.len());
    for (source_index, geometry) in geometries.iter().enumerate() {
        let row_start = usize::from(geometry.row);
        let column_start = usize::from(geometry.column);
        if row_start >= rows || column_start >= columns {
            continue;
        }
        let region_size = pane_render_region_size_for_geometry(geometry, geometries);
        placements.push(PaneCanvasPlacement {
            source_index,
            row_start,
            column_start,
            pane_rows: usize::from(region_size.rows).min(rows.saturating_sub(row_start)),
            pane_columns: usize::from(region_size.columns)
                .min(columns.saturating_sub(column_start)),
        });
    }
    placements
}

/// Returns whether a pane frame should occupy an adjacent shared divider row.
///
/// Top frames merge into a horizontal divider immediately above the pane;
/// bottom frames merge into one immediately below it. Keeping this geometry
/// decision in the mux lets product renderers consume the result without
/// owning split-layout policy.
pub fn pane_frame_merges_into_divider(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
    frame_position: TerminalFramePosition,
) -> bool {
    geometries.iter().any(|candidate| {
        if candidate.index == geometry.index {
            return false;
        }
        let shares_boundary = match frame_position {
            TerminalFramePosition::Top => {
                candidate.row.saturating_add(candidate.rows) == geometry.row
            }
            TerminalFramePosition::Bottom => {
                geometry.row.saturating_add(geometry.rows) == candidate.row
            }
        };
        shares_boundary
            && range_overlap_u16(
                geometry.column,
                geometry.column.saturating_add(geometry.columns),
                candidate.column,
                candidate.column.saturating_add(candidate.columns),
            ) > 0
    })
}

/// One mux-managed pane-divider cell and its box-drawing glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneDividerCell {
    /// Zero-based terminal column occupied by the divider.
    pub column: u16,
    /// Zero-based terminal row occupied by the divider.
    pub row: u16,
    /// Thin box-drawing glyph selected from neighboring divider strokes.
    pub glyph: char,
}

/// Directional strokes that meet in one mux-managed pane-divider cell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PaneDividerConnections {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    vertical: bool,
    horizontal: bool,
}

impl PaneDividerConnections {
    fn add_vertical(&mut self, up: bool, down: bool) {
        self.vertical = true;
        self.up |= up;
        self.down |= down;
    }

    fn add_horizontal(&mut self, left: bool, right: bool) {
        self.horizontal = true;
        self.left |= left;
        self.right |= right;
    }

    fn has_vertical(&self) -> bool {
        self.vertical
    }

    fn has_horizontal(&self) -> bool {
        self.horizontal
    }

    fn glyph(self) -> char {
        pane_divider_glyph(self.up, self.down, self.left, self.right)
    }
}

/// Selects the thin box-drawing glyph for explicit divider connections.
pub fn pane_divider_glyph(mut up: bool, mut down: bool, mut left: bool, mut right: bool) -> char {
    if !up && !down && !left && !right {
        return ' ';
    }
    if !up && !down {
        left = true;
        right = true;
    }
    if !left && !right {
        up = true;
        down = true;
    }
    match (up, down, left, right) {
        (true, true, true, true) => '\u{253c}',
        (true, true, true, false) => '\u{2524}',
        (true, true, false, true) => '\u{251c}',
        (true, false, true, true) => '\u{2534}',
        (false, true, true, true) => '\u{252c}',
        (true, false, false, true) => '\u{2514}',
        (true, false, true, false) => '\u{2518}',
        (false, true, false, true) => '\u{250c}',
        (false, true, true, false) => '\u{2510}',
        (true, true, false, false) => '\u{2502}',
        (false, false, true, true) => '\u{2500}',
        (true, false, false, false) | (false, true, false, false) => '\u{2502}',
        (false, false, true, false) | (false, false, false, true) => '\u{2500}',
        (false, false, false, false) => ' ',
    }
}

/// Composes mux-managed divider cells from neighboring pane geometry.
pub fn pane_divider_cells(
    geometries: &[PaneGeometry],
    include_horizontal: bool,
) -> Vec<PaneDividerCell> {
    if geometries.len() < 2 {
        return Vec::new();
    }
    let mut cells = BTreeMap::new();
    for (index, first) in geometries.iter().enumerate() {
        for second in geometries.iter().skip(index.saturating_add(1)) {
            let first_right = first.column.saturating_add(first.columns);
            let second_right = second.column.saturating_add(second.columns);
            let first_bottom = first.row.saturating_add(first.rows);
            let second_bottom = second.row.saturating_add(second.rows);

            if first_right == second.column || second_right == first.column {
                let boundary = first_right.min(second_right).saturating_sub(1);
                let start = first.row.max(second.row);
                let end = first_bottom.min(second_bottom);
                for row in start..end {
                    cells
                        .entry((boundary, row))
                        .or_insert_with(PaneDividerConnections::default)
                        .add_vertical(row > start, row.saturating_add(1) < end);
                }
            }
            if include_horizontal && (first_bottom == second.row || second_bottom == first.row) {
                let boundary = first_bottom.min(second_bottom).saturating_sub(1);
                let start = first.column.max(second.column);
                let end = first_right.min(second_right);
                for column in start..end {
                    cells
                        .entry((column, boundary))
                        .or_insert_with(PaneDividerConnections::default)
                        .add_horizontal(column > start, column.saturating_add(1) < end);
                }
            }
        }
    }
    connect_touching_divider_cells(&mut cells);
    cells
        .into_iter()
        .map(|((column, row), connections)| PaneDividerCell {
            column,
            row,
            glyph: connections.glyph(),
        })
        .collect()
}

fn connect_touching_divider_cells(cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>) {
    let snapshot = cells.clone();
    for (&(column, row), connections) in &snapshot {
        if connections.has_vertical() {
            if let Some(below_row) = row.checked_add(1) {
                connect_vertical_neighbors(cells, &snapshot, (column, row), (column, below_row));
            }
            if column > 0 {
                connect_crossing_neighbors(
                    cells,
                    &snapshot,
                    (column, row),
                    (column.saturating_sub(1), row),
                    true,
                );
            }
            if let Some(right_column) = column.checked_add(1) {
                connect_crossing_neighbors(
                    cells,
                    &snapshot,
                    (column, row),
                    (right_column, row),
                    false,
                );
            }
        }
        if connections.has_horizontal() {
            if let Some(right_column) = column.checked_add(1) {
                connect_horizontal_neighbors(cells, &snapshot, (column, row), (right_column, row));
            }
            if row > 0 {
                connect_vertical_crossing(
                    cells,
                    &snapshot,
                    (column, row),
                    (column, row.saturating_sub(1)),
                    true,
                );
            }
            if let Some(below_row) = row.checked_add(1) {
                connect_vertical_crossing(
                    cells,
                    &snapshot,
                    (column, row),
                    (column, below_row),
                    false,
                );
            }
        }
    }
}

fn connect_vertical_neighbors(
    cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>,
    snapshot: &BTreeMap<(u16, u16), PaneDividerConnections>,
    current: (u16, u16),
    below: (u16, u16),
) {
    if snapshot
        .get(&below)
        .is_some_and(PaneDividerConnections::has_vertical)
    {
        cells.get_mut(&current).expect("current divider cell").down = true;
        cells.get_mut(&below).expect("neighbor divider cell").up = true;
    }
}

fn connect_horizontal_neighbors(
    cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>,
    snapshot: &BTreeMap<(u16, u16), PaneDividerConnections>,
    current: (u16, u16),
    right: (u16, u16),
) {
    if snapshot
        .get(&right)
        .is_some_and(PaneDividerConnections::has_horizontal)
    {
        cells.get_mut(&current).expect("current divider cell").right = true;
        cells.get_mut(&right).expect("neighbor divider cell").left = true;
    }
}

fn connect_crossing_neighbors(
    cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>,
    snapshot: &BTreeMap<(u16, u16), PaneDividerConnections>,
    current: (u16, u16),
    horizontal: (u16, u16),
    is_left: bool,
) {
    if snapshot
        .get(&horizontal)
        .is_some_and(PaneDividerConnections::has_horizontal)
    {
        if is_left {
            cells.get_mut(&current).expect("current divider cell").left = true;
            cells
                .get_mut(&horizontal)
                .expect("neighbor divider cell")
                .right = true;
        } else {
            cells.get_mut(&current).expect("current divider cell").right = true;
            cells
                .get_mut(&horizontal)
                .expect("neighbor divider cell")
                .left = true;
        }
    }
}

fn connect_vertical_crossing(
    cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>,
    snapshot: &BTreeMap<(u16, u16), PaneDividerConnections>,
    current: (u16, u16),
    vertical: (u16, u16),
    is_above: bool,
) {
    if snapshot
        .get(&vertical)
        .is_some_and(PaneDividerConnections::has_vertical)
    {
        if is_above {
            cells.get_mut(&current).expect("current divider cell").up = true;
            cells
                .get_mut(&vertical)
                .expect("neighbor divider cell")
                .down = true;
        } else {
            cells.get_mut(&current).expect("current divider cell").down = true;
            cells.get_mut(&vertical).expect("neighbor divider cell").up = true;
        }
    }
}

/// Style applied to a rendered frame row when styled terminal output is used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalFrameStyle {
    /// Leave frame text unstyled.
    #[default]
    Default,
    /// Render the frame with bold/intense text.
    Bold,
    /// Render the frame with underline text.
    Underline,
    /// Render the frame with inverse video.
    Inverse,
}

#[cfg(test)]
mod tests {
    use mez_terminal::{TerminalSize, TerminalStyleSpan};

    use crate::layout::PaneGeometry;

    use super::{
        AttachedClientEndpointReadiness, AttachedClientStepPlan, TerminalFramePosition,
        TerminalFrameStyle, classify_attached_client_readiness, pane_canvas_placements,
        pane_content_size_for_geometry, pane_divider_cells, pane_divider_glyph,
        pane_frame_merges_into_divider, pane_render_region_size_for_geometry, place_group_frame,
        place_window_frame, plan_attached_client_step, rendered_window_body_size,
    };

    /// Verifies neutral frame contracts retain the product's established
    /// top-positioned and unstyled defaults after ownership moves to the mux.
    #[test]
    fn frame_contract_defaults_remain_stable() {
        assert_eq!(TerminalFramePosition::default(), TerminalFramePosition::Top);
        assert_eq!(TerminalFrameStyle::default(), TerminalFrameStyle::Default);
    }

    /// Verifies the mux-owned attached-client result envelope remains generic
    /// over product actions and host endpoint roles while retaining styles.
    #[test]
    fn attached_client_step_plan_is_transport_neutral() {
        let plan = AttachedClientStepPlan {
            actions: vec!["redraw"],
            output_lines: vec!["pane".to_owned()],
            output_line_style_spans: vec![vec![TerminalStyleSpan {
                start: 0,
                length: 4,
                rendition: Default::default(),
            }]],
            input_hangup: false,
            output_hangup: true,
            error_roles: vec![7_u8],
        };

        assert_eq!(plan.actions, ["redraw"]);
        assert_eq!(plan.output_lines, ["pane"]);
        assert!(plan.output_hangup);
        assert_eq!(plan.error_roles, [7]);
    }

    /// Verifies host endpoint details are reduced to the input, output, and
    /// lifecycle state required by a headless mux client planning step.
    #[test]
    fn attached_client_readiness_is_transport_neutral() {
        let readiness = classify_attached_client_readiness([
            AttachedClientEndpointReadiness {
                role: "input",
                input: true,
                output: false,
                readable: true,
                writable: false,
                hangup: true,
                error: false,
            },
            AttachedClientEndpointReadiness {
                role: "output",
                input: false,
                output: true,
                readable: false,
                writable: true,
                hangup: false,
                error: true,
            },
        ]);

        assert!(readiness.input_readable);
        assert!(readiness.output_writable);
        assert!(readiness.input_hangup);
        assert!(!readiness.output_hangup);
        assert_eq!(readiness.error_roles, ["output"]);
    }

    /// Verifies headless step planning gates rendered output on host readiness
    /// while retaining actions and propagating lifecycle/error observations.
    #[test]
    fn attached_client_step_planning_is_transport_neutral() {
        let plan = plan_attached_client_step(
            super::AttachedClientReadiness {
                input_readable: true,
                output_writable: false,
                input_hangup: false,
                output_hangup: true,
                error_roles: vec!["output"],
            },
            vec!["forward-input"],
            Some((vec!["pane".to_owned()], vec![Vec::new()])),
        );

        assert_eq!(plan.actions, ["forward-input"]);
        assert!(plan.output_lines.is_empty());
        assert!(plan.output_line_style_spans.is_empty());
        assert!(plan.output_hangup);
        assert_eq!(plan.error_roles, ["output"]);
    }

    /// Verifies mux-owned frame placement preserves authoritative viewport
    /// height for top, bottom, and conditional group frame rows.
    #[test]
    fn frame_rows_are_placed_within_authoritative_height() {
        let mut top = vec!["body-1", "body-2", "body-3"];
        place_window_frame(&mut top, "frame", TerminalFramePosition::Top, 3);
        assert_eq!(top, ["frame", "body-1", "body-2"]);

        let mut bottom = vec!["body-1", "body-2", "body-3"];
        place_window_frame(&mut bottom, "frame", TerminalFramePosition::Bottom, 3);
        assert_eq!(bottom, ["body-1", "body-2", "frame"]);

        let mut group = vec!["body-1", "body-2", "body-3"];
        place_group_frame(&mut group, "group", 3);
        assert_eq!(group, ["group", "body-1", "body-2"]);
    }

    /// Verifies mux-owned sizing reserves frame and divider cells while
    /// retaining a positive body for narrow pane regions.
    #[test]
    fn pane_presentation_sizes_reserve_mux_owned_rows_and_columns() {
        let size = TerminalSize::new(20, 10).unwrap();
        assert_eq!(
            rendered_window_body_size(size, true),
            TerminalSize::new(20, 9).unwrap()
        );

        let left = PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 10,
            rows: 5,
        };
        let right = PaneGeometry {
            index: 1,
            column: 10,
            row: 0,
            columns: 10,
            rows: 5,
        };
        let bottom = PaneGeometry {
            index: 2,
            column: 0,
            row: 5,
            columns: 20,
            rows: 5,
        };
        let geometries = [left, right, bottom];

        assert_eq!(
            pane_render_region_size_for_geometry(&left, &geometries),
            TerminalSize::new(9, 4).unwrap()
        );
        assert_eq!(
            pane_content_size_for_geometry(&left, &geometries, true, TerminalFramePosition::Top,),
            TerminalSize::new(9, 3).unwrap()
        );
        assert_eq!(
            pane_canvas_placements(TerminalSize::new(15, 4).unwrap(), &geometries),
            [
                super::PaneCanvasPlacement {
                    source_index: 0,
                    row_start: 0,
                    column_start: 0,
                    pane_rows: 4,
                    pane_columns: 9,
                },
                super::PaneCanvasPlacement {
                    source_index: 1,
                    row_start: 0,
                    column_start: 10,
                    pane_rows: 4,
                    pane_columns: 5,
                },
            ]
        );

        let clipped_then_visible = [
            PaneGeometry {
                index: 0,
                column: 20,
                row: 0,
                columns: 5,
                rows: 4,
            },
            PaneGeometry {
                index: 1,
                column: 0,
                row: 0,
                columns: 5,
                rows: 4,
            },
        ];
        assert_eq!(
            pane_canvas_placements(TerminalSize::new(15, 4).unwrap(), &clipped_then_visible,),
            [super::PaneCanvasPlacement {
                source_index: 1,
                row_start: 0,
                column_start: 0,
                pane_rows: 4,
                pane_columns: 5,
            }]
        );
    }

    /// Verifies pane-frame placement consumes shared split geometry without
    /// depending on product rendering, prompt state, or agent metadata.
    #[test]
    fn pane_frames_merge_only_with_adjacent_horizontal_dividers() {
        let top = PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 20,
            rows: 5,
        };
        let bottom = PaneGeometry {
            index: 1,
            column: 0,
            row: 5,
            columns: 20,
            rows: 5,
        };
        let side = PaneGeometry {
            index: 2,
            column: 20,
            row: 0,
            columns: 20,
            rows: 10,
        };
        let geometries = [top, bottom, side];

        assert!(pane_frame_merges_into_divider(
            &bottom,
            &geometries,
            TerminalFramePosition::Top,
        ));
        assert!(pane_frame_merges_into_divider(
            &top,
            &geometries,
            TerminalFramePosition::Bottom,
        ));
        assert!(!pane_frame_merges_into_divider(
            &top,
            &geometries,
            TerminalFramePosition::Top,
        ));
        assert_eq!(
            super::pane_frame_row_for_geometry(&bottom, &geometries, TerminalFramePosition::Top, 2,),
            6
        );
        assert_eq!(
            super::pane_frame_row_for_geometry(&top, &geometries, TerminalFramePosition::Bottom, 2,),
            6
        );
        assert_eq!(
            super::pane_frame_row_for_geometry(
                &side,
                &geometries,
                TerminalFramePosition::Bottom,
                2,
            ),
            11
        );
        assert_eq!(
            super::merged_pane_frame_placements(&geometries, TerminalFramePosition::Top),
            vec![super::MergedPaneFramePlacement {
                pane_index: 1,
                row: 4,
                column_start: 0,
                width: 19,
            }]
        );
    }

    /// Verifies mux divider composition selects stable line, corner, and
    /// junction glyphs without relying on product canvases or mouse types.
    #[test]
    fn pane_divider_glyphs_match_connection_shapes() {
        assert_eq!(pane_divider_glyph(true, true, false, false), '│');
        assert_eq!(pane_divider_glyph(false, false, true, true), '─');
        assert_eq!(pane_divider_glyph(false, true, false, true), '┌');
        assert_eq!(pane_divider_glyph(true, true, true, true), '┼');

        let geometries = [
            PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: 10,
                rows: 5,
            },
            PaneGeometry {
                index: 1,
                column: 10,
                row: 0,
                columns: 10,
                rows: 5,
            },
        ];
        let cells = pane_divider_cells(&geometries, true);
        assert_eq!(cells.len(), 5);
        assert!(cells.iter().all(|cell| cell.column == 9));
    }
}
