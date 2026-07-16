//! Terminal Client Loop implementation.
//!
//! This module owns the terminal client loop boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use std::io::ErrorKind;
use std::time::Instant;

use super::mouse::mouse_copy_position;
use super::{
    AttachedTerminalFdReadiness, AttachedTerminalFdRole, BorrowedFd, MezError, MouseAction,
    MouseButton, MouseEvent, MouseEventKind, RawFd, Result, TerminalClientLoopConfig,
    TerminalStyleSpan, parse_sgr_mouse,
};
use mez_mux::attached_client::{
    application_cursor_forwarding_bytes, application_mouse_forwarding_bytes,
    classify_attached_mouse_event, earliest_sequence_start, input_sequence_start,
    malformed_sgr_mouse_prefix_len, prefix_sequence_len, sgr_mouse_sequence_len,
    sgr_mouse_sequence_start,
};
use mez_mux::copy::{CopyModeKeyAction, classify_copy_mode_key_action};
use mez_mux::input::{
    KeyChord, MuxAction, TerminalInputClassification, WindowFocusTarget, classify_prefix_binding,
    classify_terminal_input_with_command_bindings, key_chord_input_bytes, parse_key_chord_bytes,
};
#[cfg(test)]
use mez_mux::layout::Size;
#[cfg(test)]
use mez_mux::presentation::AttachedTerminalOutputModes;
use mez_mux::presentation::{
    ClientStatusLine, RenderedClientView, compose_client_presentation_with_styles,
};

mod input_adapter;
mod output_adapter;
mod runtime_step;
mod types;

pub use input_adapter::route_client_input;
pub(crate) use input_adapter::route_client_input_actions;
use input_adapter::route_client_input_actions_with_host_paste_buffer_state;
#[cfg(test)]
pub(crate) use input_adapter::{
    route_client_input_actions_with_host_paste_buffer,
    route_client_input_actions_with_host_paste_state,
};
pub use output_adapter::attached_terminal_output_disconnected;
pub(in crate::terminal) use output_adapter::borrow_raw_fd;
pub use runtime_step::plan_attached_terminal_client_step;
pub(crate) use runtime_step::plan_attached_terminal_client_step_with_host_paste_buffer;
#[cfg(test)]
pub use types::AttachedTerminalClientLoopIo;
pub(crate) use types::HostBracketedPasteBufferState;
pub use types::{
    AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS, AttachedTerminalClientLoopConfig,
    AttachedTerminalClientLoopReport, AttachedTerminalClientStepPlan,
    ReadlinePromptClientPresentation, ReadlinePromptStatusRow, TerminalClientLoopAction,
};
