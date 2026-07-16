//! Cli Attach implementation.
//!
//! This module owns the cli attach boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    Args, AsRawFd, AsyncAttachedTerminalIo, AsyncAttachedTerminalPresentationGuard,
    AttachedTerminalOutputModes, AuxiliarySocketKind, CliEnv, CliOutputFormat, ClientId,
    DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT, GraphicRendition, IsTerminal, MezError, Result,
    SessionRecord, SessionRegistry, Size, SocketSelection, TerminalColor, TerminalCursorStyle,
    TerminalStyleSpan, UnixStream, Write, attached_terminal_output_disconnected,
    auxiliary_socket_path_for_control_socket, decode_control_frame, encode_control_body,
    incomplete_control_response_error, io, json_escape, read_control_response_frames,
    records_to_json, registry_root, resolve_session_record_target, selected_socket_path,
    terminal_size_from_fd_or_environment, write_control_response, write_json_or_plain,
};
// Attach clients and interactive control-socket attachment helpers.

/// Maximum JSON-RPC event notification body accepted from the auxiliary event
/// stream.
const ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH: usize = 1024 * 1024;

/// Maximum bytes read from the auxiliary event stream in one socket read.
const ATTACH_EVENT_STREAM_READ_BUFFER_BYTES: usize = 8192;
/// Interval between idle terminal-size probes for attached control clients.
///
/// The attach loop should notice local terminal resizes even when the user is
/// not typing and the daemon has no new runtime events to report. Probing a
/// few times per second keeps resize-driven redraws responsive without
/// requiring a fixed-cadence render request.
const ATTACH_IDLE_TERMINAL_SIZE_REFRESH_INTERVAL: std::time::Duration =
    DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT;

/// Redraw requirements reported by one terminal step response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TerminalStepRefreshRequirement {
    /// Whether the attached client should request a fresh terminal view.
    pub view_refresh_required: bool,
    /// Whether the attached client must discard its retained output frame before
    /// rendering the fresh terminal view.
    pub full_redraw_required: bool,
}

/// Outcome from rendering one explicit primary terminal view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrimaryViewRenderOutcome {
    /// Whether the control connection and attached terminal are still usable.
    connected: bool,
    /// Milliseconds until the next animation-only view refresh.
    animation_refresh_interval_ms: u64,
}

impl PrimaryViewRenderOutcome {
    /// Builds an outcome for a disconnected control or terminal endpoint.
    const fn disconnected() -> Self {
        Self {
            connected: false,
            animation_refresh_interval_ms: 0,
        }
    }
}
/// Outcome from notifying the runtime about a primary terminal resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrimaryResizeRequestOutcome {
    /// Whether the control connection is still usable.
    connected: bool,
}
impl PrimaryResizeRequestOutcome {
    /// Builds an outcome for a disconnected control endpoint.
    const fn disconnected() -> Self {
        Self { connected: false }
    }
}

/// Tracks the local animation refresh deadline for a control-socket attach.
#[derive(Debug, Default)]
struct AttachAnimationRefresh {
    /// Current refresh interval advertised by the last rendered view.
    interval_ms: Option<u64>,
    /// Next local deadline for an animation-only `terminal/view`.
    deadline: Option<tokio::time::Instant>,
}

impl AttachAnimationRefresh {
    /// Returns the next animation refresh deadline, when animation is active.
    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.deadline
    }

    /// Updates the local refresh schedule from the latest rendered view.
    fn update_from_rendered_view(&mut self, refresh_interval_ms: u64) {
        if refresh_interval_ms == 0 {
            self.interval_ms = None;
            self.deadline = None;
            return;
        }
        self.interval_ms = Some(refresh_interval_ms);
        self.deadline = Some(
            tokio::time::Instant::now() + std::time::Duration::from_millis(refresh_interval_ms),
        );
    }
}
/// Tracks the next local wake deadline for idle terminal-size refresh probes.
#[derive(Debug)]
struct AttachTerminalSizeRefresh {
    /// Next local wake deadline for an idle terminal-size probe.
    deadline: tokio::time::Instant,
}
impl Default for AttachTerminalSizeRefresh {
    /// Builds the default size-refresh schedule for an attached client loop.
    fn default() -> Self {
        Self {
            deadline: tokio::time::Instant::now() + ATTACH_IDLE_TERMINAL_SIZE_REFRESH_INTERVAL,
        }
    }
}
impl AttachTerminalSizeRefresh {
    /// Returns the next idle terminal-size refresh deadline.
    fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }
    /// Reschedules the next idle terminal-size refresh from the current time.
    fn reschedule(&mut self) {
        self.deadline = tokio::time::Instant::now() + ATTACH_IDLE_TERMINAL_SIZE_REFRESH_INTERVAL;
    }
}

mod event_stream;
mod observer;
mod primary;
mod requests;
mod responses;
mod selection;

#[cfg(test)]
pub(super) use event_stream::{AttachRenderAction, AttachedRuntimeEventStream};
#[cfg(test)]
pub(super) use observer::run_control_socket_attached_observer_client_loop_async;
#[cfg(test)]
pub(super) use primary::{
    run_control_socket_attached_primary_client_loop_async,
    run_control_socket_attached_primary_client_loop_async_with_runtime_events,
};
#[cfg(test)]
pub(super) use requests::terminal_step_control_request;
#[cfg(test)]
pub(super) use responses::{
    terminal_step_response_line_style_spans, terminal_step_response_output_modes,
    terminal_step_response_refresh_requirement,
};
pub(super) use selection::{AttachCliArgs, run_attach, run_list};
#[cfg(test)]
pub(super) use selection::{attach_request_from_args, default_attach_socket_selection};
