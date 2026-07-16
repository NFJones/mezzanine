//! Product terminal presentation and host-I/O adapters.
//!
//! `mez-terminal` owns terminal parsing and state while `mez-mux` owns neutral
//! client, copy, input, and render planning. This module binds those engines to
//! raw host file descriptors, clipboard commands, product prompt and agent
//! views, configured themes, terminal encoding, and mouse actions.

use std::collections::BTreeMap;
use std::os::fd::RawFd;
#[cfg(test)]
use std::time::Duration;

use rustix::fd::BorrowedFd;
use rustix::fs::fcntl_getfl;
use rustix::io::Errno;
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr};

use crate::error::{MezError, Result};
use mez_terminal::{
    GraphicRendition, MouseButton, MouseEvent, MouseEventKind, TerminalScreen, TerminalStyleSpan,
    TerminalStyledLine, parse_sgr_mouse,
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

#[cfg(test)]
pub use client_loop::AttachedTerminalClientLoopIo;
#[cfg(test)]
pub use client_loop::route_client_input;
pub(crate) use client_loop::route_client_input_actions;
pub use client_loop::{
    AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS, AttachedTerminalClientLoopConfig,
    AttachedTerminalClientLoopReport, AttachedTerminalClientStepPlan,
    ReadlinePromptClientPresentation, ReadlinePromptStatusRow, TerminalClientLoopAction,
    attached_terminal_output_disconnected, plan_attached_terminal_client_step,
};
pub(crate) use client_loop::{
    HostBracketedPasteBufferState, plan_attached_terminal_client_step_with_host_paste_buffer,
};
pub use copy::CopyMode;
#[cfg(test)]
pub use fd::AttachedTerminalFd;
#[cfg(test)]
pub use fd::poll_attached_terminal_fd_readiness;
pub use fd::{
    AttachedTerminalFdReadiness, AttachedTerminalFdRole, PaneRenderInput, TerminalClientLoopConfig,
    TerminalFdInterest, TerminalFrameContext, TerminalPaneFrameContext, TerminalRawModeGuard,
    read_attached_terminal_size,
};
pub use host_clipboard::{HostClipboard, HostClipboardCommand};
pub use mouse::{
    MouseAction, MousePaneAgentSelectorCell, MousePaneAgentStatusCell, MouseWindowActionFrameCell,
    PaneAgentStatusField, WindowFrameAction, WindowFrameCommandKind,
};
pub use render::{
    DEFAULT_PANE_FRAME_TEMPLATE, DEFAULT_PANE_FRAME_VISIBLE_FIELDS,
    DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE, DEFAULT_WINDOW_FRAME_TEMPLATE,
    DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS, agent_prompt_reserved_line_count,
    compose_modal_display_overlay_lines, compose_prompt_overlay_presentation_with_styles,
    pane_frame_agent_status_pillbox_cells, render_attached_client_view, rendered_pane_geometries,
    window_frame_action_pillbox_cells, window_frame_pillbox_cells,
    window_group_frame_pillbox_cells,
};
#[cfg(test)]
pub use render::{
    TerminalFrameRenderOptions, compose_display_overlay_line_style_spans,
    compose_display_region_overlay_line_style_spans, compose_display_region_overlay_lines,
    compose_modal_display_overlay_line_style_spans, compose_prompt_overlay_lines,
    compose_prompt_overlay_presentation, compose_prompt_region_presentation_with_styles,
    compose_readline_prompt_client_presentation, draw_window_from_screens,
    render_readline_prompt_status_row, render_window, render_window_with_pane_frame_template,
};
pub(crate) use screen::parse_mez_shell_transaction_osc;

use client_loop::borrow_raw_fd;
pub(crate) use render::{
    DEFAULT_AGENT_WRAP_COLUMN_CAP, agent_log_wrap_width, agent_wrap_column_cap,
    set_agent_wrap_column_cap, wrap_agent_log_lines,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
