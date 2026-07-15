//! Terminal-state primitives.
//!
//! This module provides bounded history behavior, alternate-screen history
//! exclusion, TERM fallback selection, and Mezzanine's bounded
//! xterm-compatible screen core for common line-oriented output and terminal UI
//! control sequences. Unsupported terminal capabilities are kept explicit in
//! the profile layer instead of being advertised as full xterm emulation.

use std::collections::BTreeMap;
use std::os::fd::RawFd;
#[cfg(test)]
use std::time::Duration;

use rustix::fd::BorrowedFd;
use rustix::fs::fcntl_getfl;
use rustix::io::Errno;
#[cfg(test)]
use rustix::io::{read as rustix_read, write as rustix_write};
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr};

use crate::error::{MezError, Result};
use mez_terminal::{
    GraphicRendition, MouseButton, MouseEvent, MouseEventKind, TerminalColor, TerminalScreen,
    TerminalStyleSpan, TerminalStyledLine, parse_sgr_mouse,
};

/// Exposes the client loop module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod client_loop;
/// Exposes the copy module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod copy;
/// Exposes the fd module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod fd;
/// Exposes the host clipboard adapter boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod host_clipboard;
/// Exposes the mouse module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod mouse;
/// Exposes the render module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod render;
/// Exposes the screen module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod screen;

pub(crate) use client_loop::route_client_input_actions;
pub use client_loop::{
    AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS, AttachedTerminalClientLoopConfig,
    AttachedTerminalClientLoopReport, AttachedTerminalClientStepPlan,
    ReadlinePromptClientPresentation, ReadlinePromptStatusRow, TerminalClientLoopAction,
    attached_terminal_output_disconnected, plan_attached_terminal_client_step, route_client_input,
};
#[cfg(test)]
pub use client_loop::{
    AttachedTerminalClientLoopIo, AttachedTerminalFdLoopIo, run_attached_terminal_client_loop,
};
pub(crate) use client_loop::{
    AttachedTerminalOutputFrameState, attached_terminal_enter_presentation_frame,
    attached_terminal_restore_presentation_frame,
    encode_attached_terminal_output_update_frame_with_styles,
};
pub(crate) use client_loop::{
    HostBracketedPasteBufferState, plan_attached_terminal_client_step_with_host_paste_buffer,
};
pub use copy::CopyMode;
pub(crate) use copy::{
    AGENT_COPY_SKIP_LINE, AGENT_COPY_WRAP_CONTINUATION, encode_agent_copy_source_line,
};
#[cfg(test)]
pub use fd::poll_attached_terminal_fd_readiness;
pub use fd::{
    AttachedTerminalFd, AttachedTerminalFdReadiness, AttachedTerminalFdRole, PaneRenderInput,
    TerminalClientLoopConfig, TerminalFdInterest, TerminalFrameContext, TerminalPaneFrameContext,
    TerminalRawModeGuard, read_attached_terminal_size,
};
pub use host_clipboard::{HostClipboard, HostClipboardCommand};
pub use mouse::{
    MouseAction, MousePaneAgentSelectorCell, MousePaneAgentStatusCell, MouseWindowActionFrameCell,
    PaneAgentStatusField, WindowFrameAction, WindowFrameCommandKind, classify_mouse_event,
};
pub(crate) use render::overlay_fixed_column_style_spans;
#[cfg(test)]
pub(crate) use render::pane_divider_glyph_for_test;
pub use render::{
    DEFAULT_PANE_FRAME_TEMPLATE, DEFAULT_PANE_FRAME_VISIBLE_FIELDS,
    DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE, DEFAULT_WINDOW_FRAME_TEMPLATE,
    DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS, TerminalFrameRenderOptions,
    agent_prompt_reserved_line_count, compose_client_presentation,
    compose_client_presentation_with_styles, compose_display_overlay_line_style_spans,
    compose_display_overlay_lines, compose_display_region_overlay_line_style_spans,
    compose_display_region_overlay_lines, compose_modal_display_overlay_line_style_spans,
    compose_modal_display_overlay_lines, compose_prompt_overlay_lines,
    compose_prompt_overlay_presentation, compose_prompt_overlay_presentation_with_styles,
    compose_prompt_region_presentation_with_styles, compose_readline_prompt_client_presentation,
    draw_window_from_screens, modal_display_overlay_max_scroll, modal_display_overlay_page_rows,
    pane_border_cells_for_geometries, pane_frame_agent_status_pillbox_cells,
    render_attached_client_view, render_readline_prompt_status_row, render_window,
    render_window_with_pane_frame_template, rendered_pane_geometries,
    window_frame_action_pillbox_cells, window_frame_pillbox_cells,
    window_group_frame_pillbox_cells,
};
pub(crate) use screen::parse_mez_shell_transaction_osc;

use client_loop::borrow_raw_fd;
pub(crate) use render::{
    DEFAULT_AGENT_WRAP_COLUMN_CAP, TerminalEmojiWidth, agent_log_wrap_width, agent_wrap_column_cap,
    set_agent_wrap_column_cap, set_terminal_emoji_width, terminal_grapheme_width,
    terminal_graphemes, terminal_text_width, wrap_agent_log_lines,
};
use render::{char_count, line_slice};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
