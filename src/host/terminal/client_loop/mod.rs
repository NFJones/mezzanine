//! Terminal Client Loop implementation.
//!
//! This module owns the terminal client loop boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AttachedTerminalFdReadiness, AttachedTerminalFdRole, MouseAction, MouseButton, MouseEvent,
    MouseEventKind, Result, TerminalClientLoopConfig, parse_sgr_mouse,
};

mod input_adapter;
mod output_adapter;
mod runtime_step;
mod types;

pub use input_adapter::route_client_input;
pub(crate) use input_adapter::route_client_input_actions;
#[cfg(test)]
pub(crate) use input_adapter::{
    route_client_input_actions_with_host_paste_buffer,
    route_client_input_actions_with_host_paste_state,
};
pub use output_adapter::attached_terminal_output_disconnected;
pub(in crate::host::terminal) use output_adapter::borrow_raw_fd;
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
