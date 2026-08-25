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
    /// Whether the acknowledged terminal step detached this client.
    pub client_detached: bool,
    /// Whether the completed terminal step terminated the daemon session.
    pub session_terminated: bool,
}

/// One decoded terminal frame retained for client-local presentation overlays.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AttachClientFrame {
    /// Flattened server-owned base rows.
    lines: Vec<String>,
    /// Server-owned style spans aligned with `lines`.
    line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Host terminal modes associated with this frame.
    modes: AttachedTerminalOutputModes,
    /// Optional client-space Iroh status slot.
    iroh_status_slot: Option<crate::host::terminal::TerminalIrohStatusSlot>,
}

impl AttachClientFrame {
    /// Composes one local Iroh state pill without mutating the cached base frame.
    fn with_iroh_status(
        &self,
        connected: bool,
        quality: crate::host::terminal::TerminalIrohStatusQuality,
    ) -> (Vec<String>, Vec<Vec<TerminalStyleSpan>>) {
        let mut lines = self.lines.clone();
        let mut spans = self.line_style_spans.clone();
        let Some(slot) = self.iroh_status_slot else {
            return (lines, spans);
        };
        let Some(line) = lines.get_mut(slot.row) else {
            return (lines, spans);
        };
        let label = if connected { " ok " } else { " no " };
        let prefix = mez_mux::render::line_slice(line, 0, slot.column);
        let suffix =
            mez_mux::render::line_slice(line, slot.column.saturating_add(slot.width), usize::MAX);
        *line = format!(
            "{prefix}{}{suffix}",
            mez_mux::render::fit_width(label, slot.width)
        );
        spans.resize(lines.len(), Vec::new());
        let row_spans = &mut spans[slot.row];
        *row_spans = row_spans
            .iter()
            .flat_map(|span| {
                let slot_end = slot.column.saturating_add(slot.width);
                if mez_mux::render::style_span_overlaps_columns(*span, slot.column, slot_end) {
                    mez_mux::render::style_span_segments_outside_range(*span, slot.column, slot_end)
                } else {
                    vec![*span]
                }
            })
            .collect();
        let rendition = if connected {
            match quality {
                crate::host::terminal::TerminalIrohStatusQuality::Good => slot.good,
                crate::host::terminal::TerminalIrohStatusQuality::Degraded => slot.degraded,
                crate::host::terminal::TerminalIrohStatusQuality::Poor => slot.poor,
                crate::host::terminal::TerminalIrohStatusQuality::Unknown => slot.unknown,
            }
        } else {
            slot.unknown
        };
        row_spans.push(TerminalStyleSpan {
            start: slot.column,
            length: slot.width,
            rendition,
        });
        row_spans.sort_unstable_by_key(|span| span.start);
        (lines, spans)
    }
}

/// Previous selected-path counters retained for client-local health deltas.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AttachIrohPathSample {
    path_id: String,
    rtt_micros: u64,
    jitter_micros: u64,
    lost_packets: u64,
    congestion_events: u64,
}

/// Samples and classifies one attach client's retained Iroh connection.
#[derive(Debug)]
struct AttachIrohHealthTracker {
    previous: Option<AttachIrohPathSample>,
    quality: crate::host::terminal::TerminalIrohStatusQuality,
    deadline: tokio::time::Instant,
}

impl Default for AttachIrohHealthTracker {
    fn default() -> Self {
        Self {
            previous: None,
            quality: crate::host::terminal::TerminalIrohStatusQuality::Unknown,
            deadline: tokio::time::Instant::now(),
        }
    }
}

impl AttachIrohHealthTracker {
    const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

    fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }

    fn quality(&self) -> crate::host::terminal::TerminalIrohStatusQuality {
        self.quality
    }

    /// Samples the selected path and reports whether visible quality changed.
    fn sample(&mut self, connection: &iroh::endpoint::Connection) -> bool {
        let previous_quality = self.quality;
        let paths = connection.paths();
        let Some(path) = paths.iter().find(|path| path.is_selected()) else {
            self.quality = crate::host::terminal::TerminalIrohStatusQuality::Unknown;
            self.deadline = tokio::time::Instant::now() + Self::REFRESH_INTERVAL;
            return self.quality != previous_quality;
        };
        let stats = path.stats();
        let path_id = format!("{:?}", path.id());
        let rtt_micros = u64::try_from(stats.rtt.as_micros()).unwrap_or(u64::MAX);
        let same_path = self
            .previous
            .as_ref()
            .is_some_and(|previous| previous.path_id == path_id);
        let jitter_micros = self
            .previous
            .as_ref()
            .filter(|_| same_path)
            .map(|previous| {
                previous
                    .jitter_micros
                    .saturating_mul(3)
                    .saturating_add(rtt_micros.abs_diff(previous.rtt_micros))
                    / 4
            })
            .unwrap_or(0);
        let delta = |current: u64, previous: fn(&AttachIrohPathSample) -> u64| {
            self.previous
                .as_ref()
                .filter(|_| same_path)
                .map(|sample| current.saturating_sub(previous(sample)))
                .unwrap_or(0)
        };
        let lost_packets = delta(stats.lost_packets, |sample| sample.lost_packets);
        let congestion_events = delta(stats.congestion_events, |sample| sample.congestion_events);
        self.quality = crate::runtime::classify_runtime_iroh_connection_quality(
            rtt_micros,
            jitter_micros,
            lost_packets,
            congestion_events,
            std::time::Duration::ZERO,
        );
        self.previous = Some(AttachIrohPathSample {
            path_id,
            rtt_micros,
            jitter_micros,
            lost_packets: stats.lost_packets,
            congestion_events: stats.congestion_events,
        });
        self.deadline = tokio::time::Instant::now() + Self::REFRESH_INTERVAL;
        self.quality != previous_quality
    }
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
pub(super) use event_stream::AttachedRuntimeEventStream;
pub(in crate::cli) use event_stream::{AttachRenderAction, spawn_iroh_runtime_event_receiver};
#[cfg(test)]
pub(super) use observer::run_control_socket_attached_observer_client_loop_async;
#[cfg(test)]
pub(super) use primary::{
    run_control_socket_attached_primary_client_loop_async,
    run_control_socket_attached_primary_client_loop_async_with_runtime_events,
    run_iroh_attached_primary_client_loop_async,
};
pub(in crate::cli) use requests::read_async_control_response_frames;
#[cfg(test)]
pub(super) use requests::terminal_step_control_request;
#[cfg(test)]
pub(super) use responses::{
    terminal_step_response_line_style_spans, terminal_step_response_output_modes,
    terminal_step_response_refresh_requirement,
};
pub(in crate::cli) use selection::socket_selection_for_registry_session;
pub(super) use selection::{AttachCliArgs, run_attach, run_list};
#[cfg(test)]
pub(super) use selection::{attach_request_from_args, default_attach_socket_selection};
