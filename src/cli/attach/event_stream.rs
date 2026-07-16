//! Attached terminal polling and auxiliary runtime event-stream handling.

use super::{
    ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH, ATTACH_EVENT_STREAM_READ_BUFFER_BYTES,
    AsyncAttachedTerminalIo, AuxiliarySocketKind, MezError, Result, UnixStream,
    attached_terminal_output_disconnected, auxiliary_socket_path_for_control_socket,
    decode_control_frame,
};
use tokio::io::AsyncReadExt;

/// Carries Attached Client Input Poll state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AttachedClientInputPoll {
    /// Stores the bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) bytes: Vec<u8>,
    /// Stores the eof value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) eof: bool,
    /// Render action requested by an auxiliary runtime event.
    pub(super) render_action: AttachRenderAction,
}

/// Render action requested by an attached runtime event stream notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum AttachRenderAction {
    /// No visible attached-terminal redraw is needed.
    None,
    /// Request a fresh `terminal/view` while preserving the diff-render base.
    View,
    /// Invalidate the diff-render base before requesting a fresh view.
    InvalidateAndView,
    /// The auxiliary event stream disconnected.
    Disconnect,
}

impl AttachRenderAction {
    /// Combines two actions, preserving the strongest action for an event burst.
    const fn combine(self, other: Self) -> Self {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }

    /// Returns the precedence rank for this action.
    const fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::View => 1,
            Self::InvalidateAndView => 2,
            Self::Disconnect => 3,
        }
    }
}

/// Runs the read attached client input or deadline wake operation.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn read_attached_client_input_or_deadline<I: AsyncAttachedTerminalIo>(
    terminal_io: &mut I,
    max_bytes: usize,
    animation_deadline: Option<tokio::time::Instant>,
    wake_deadline: tokio::time::Instant,
) -> Result<AttachedClientInputPoll> {
    let input = async {
        let _ = terminal_io.poll_input_readiness().await?;
        terminal_io.read_input(max_bytes).await
    };
    match tokio::time::timeout_at(wake_deadline, input).await {
        Ok(Ok(bytes)) if bytes.is_empty() => Ok(AttachedClientInputPoll {
            bytes,
            eof: true,
            render_action: AttachRenderAction::None,
        }),
        Ok(Ok(bytes)) => Ok(AttachedClientInputPoll {
            bytes,
            eof: false,
            render_action: AttachRenderAction::None,
        }),
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(idle_deadline_input_poll(animation_deadline)),
    }
}
/// Builds the synthetic input poll produced by an idle local deadline wakeup.
pub(super) fn idle_deadline_input_poll(
    animation_deadline: Option<tokio::time::Instant>,
) -> AttachedClientInputPoll {
    if animation_deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now()) {
        animation_refresh_input_poll()
    } else {
        AttachedClientInputPoll {
            bytes: Vec::new(),
            eof: false,
            render_action: AttachRenderAction::None,
        }
    }
}
/// Reads terminal input while also accepting runtime event redraw wakeups.
///
/// # Parameters
/// - `terminal_io`: The attached terminal input/output boundary.
/// - `event_stream`: Optional auxiliary runtime event stream.
/// - `max_bytes`: Maximum terminal input bytes to read.
pub(super) async fn read_attached_client_input_or_runtime_event<I: AsyncAttachedTerminalIo>(
    terminal_io: &mut I,
    event_stream: Option<&mut AttachedRuntimeEventStream>,
    max_bytes: usize,
    animation_deadline: Option<tokio::time::Instant>,
    size_refresh_deadline: tokio::time::Instant,
) -> Result<AttachedClientInputPoll> {
    let wake_deadline = animation_deadline
        .filter(|deadline| *deadline <= size_refresh_deadline)
        .unwrap_or(size_refresh_deadline);
    let input = read_attached_client_input_or_deadline(
        terminal_io,
        max_bytes,
        animation_deadline,
        wake_deadline,
    );
    tokio::pin!(input);
    let Some(event_stream) = event_stream else {
        return tokio::select! {
            result = &mut input => result,
        };
    };
    let mut input = tokio::select! {
        biased;
        input = &mut input => input,
        render_action = read_runtime_event_stream_action(event_stream) => {
            return Ok(AttachedClientInputPoll {
                bytes: Vec::new(),
                eof: false,
                render_action: render_action?,
            });
        }
    }?;
    if !input.eof && !input.bytes.is_empty() {
        input.render_action = input
            .render_action
            .combine(event_stream.try_read_ready_render_action()?);
    }
    Ok(input)
}

/// Builds the synthetic input poll produced by a local animation refresh tick.
pub(super) fn animation_refresh_input_poll() -> AttachedClientInputPoll {
    AttachedClientInputPoll {
        bytes: Vec::new(),
        eof: false,
        render_action: AttachRenderAction::View,
    }
}

/// Reads auxiliary runtime event notifications and returns the coalesced action.
pub(super) async fn read_runtime_event_stream_action(
    stream: &mut AttachedRuntimeEventStream,
) -> Result<AttachRenderAction> {
    stream.read_render_action().await
}

/// Stateful auxiliary runtime event stream decoder.
pub(in crate::cli) struct AttachedRuntimeEventStream {
    /// Auxiliary event stream socket.
    stream: tokio::net::UnixStream,
    /// Buffered bytes that have not yet formed a complete control frame.
    pending: Vec<u8>,
}

impl AttachedRuntimeEventStream {
    /// Creates a stateful decoder for one auxiliary event stream.
    pub(in crate::cli) fn new(stream: tokio::net::UnixStream) -> Self {
        Self {
            stream,
            pending: Vec::new(),
        }
    }

    /// Reads one event burst and returns the strongest render action it implies.
    pub(in crate::cli) async fn read_render_action(&mut self) -> Result<AttachRenderAction> {
        let mut action = AttachRenderAction::None;
        if !self.pending_contains_complete_frame() {
            match self.read_event_stream_chunk().await? {
                RuntimeEventStreamRead::Read => {}
                RuntimeEventStreamRead::Disconnected => return Ok(AttachRenderAction::Disconnect),
                RuntimeEventStreamRead::Pending => return Ok(AttachRenderAction::None),
            }
        }
        action = action.combine(self.drain_complete_event_frames()?);
        loop {
            match self.try_read_event_stream_chunk()? {
                RuntimeEventStreamRead::Read => {
                    action = action.combine(self.drain_complete_event_frames()?);
                }
                RuntimeEventStreamRead::Pending => return Ok(action),
                RuntimeEventStreamRead::Disconnected => {
                    return Ok(action.combine(AttachRenderAction::Disconnect));
                }
            }
        }
    }

    /// Drains any already-ready redraw events without waiting for new bytes.
    ///
    /// The foreground input loop uses this after local input wins the readiness
    /// race so a simultaneous runtime redraw wakeup can be satisfied by the same
    /// post-input render instead of lingering for a later redundant view request.
    pub(super) fn try_read_ready_render_action(&mut self) -> Result<AttachRenderAction> {
        let mut action = AttachRenderAction::None;
        if !self.pending_contains_complete_frame() {
            match self.try_read_event_stream_chunk()? {
                RuntimeEventStreamRead::Read => {}
                RuntimeEventStreamRead::Pending | RuntimeEventStreamRead::Disconnected => {
                    return Ok(AttachRenderAction::None);
                }
            }
        }
        action = action.combine(self.drain_complete_event_frames()?);
        loop {
            match self.try_read_event_stream_chunk()? {
                RuntimeEventStreamRead::Read => {
                    action = action.combine(self.drain_complete_event_frames()?);
                }
                RuntimeEventStreamRead::Pending | RuntimeEventStreamRead::Disconnected => {
                    return Ok(action);
                }
            }
        }
    }

    /// Reports whether the pending byte buffer begins with a complete frame.
    fn pending_contains_complete_frame(&self) -> bool {
        decode_control_frame(
            self.pending.as_slice(),
            ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH,
        )
        .is_ok()
    }

    /// Reads one awaited chunk from the event stream into the pending buffer.
    async fn read_event_stream_chunk(&mut self) -> Result<RuntimeEventStreamRead> {
        let mut buffer = [0u8; ATTACH_EVENT_STREAM_READ_BUFFER_BYTES];
        match self.stream.read(&mut buffer).await {
            Ok(0) => Ok(RuntimeEventStreamRead::Disconnected),
            Ok(read) => {
                self.push_pending_event_bytes(&buffer[..read])?;
                Ok(RuntimeEventStreamRead::Read)
            }
            Err(error) if runtime_event_stream_disconnected(error.kind()) => {
                Ok(RuntimeEventStreamRead::Disconnected)
            }
            Err(error) => Err(MezError::from(error)),
        }
    }

    /// Reads one immediately available chunk from the event stream.
    fn try_read_event_stream_chunk(&mut self) -> Result<RuntimeEventStreamRead> {
        let mut buffer = [0u8; ATTACH_EVENT_STREAM_READ_BUFFER_BYTES];
        match self.stream.try_read(&mut buffer) {
            Ok(0) => Ok(RuntimeEventStreamRead::Disconnected),
            Ok(read) => {
                self.push_pending_event_bytes(&buffer[..read])?;
                Ok(RuntimeEventStreamRead::Read)
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                Ok(RuntimeEventStreamRead::Pending)
            }
            Err(error) if runtime_event_stream_disconnected(error.kind()) => {
                Ok(RuntimeEventStreamRead::Disconnected)
            }
            Err(error) => Err(MezError::from(error)),
        }
    }

    /// Appends bytes to the pending buffer while enforcing a bounded frame size.
    fn push_pending_event_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH + 1024 {
            return Err(MezError::invalid_state(
                "runtime event stream frame exceeds limit",
            ));
        }
        Ok(())
    }

    /// Drains all complete frames from the pending buffer into one render action.
    fn drain_complete_event_frames(&mut self) -> Result<AttachRenderAction> {
        let mut action = AttachRenderAction::None;
        loop {
            let Ok((body, consumed)) = decode_control_frame(
                self.pending.as_slice(),
                ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH,
            ) else {
                return Ok(action);
            };
            if consumed == 0 {
                return Ok(action);
            }
            action = action.combine(attach_render_action_for_event_body(body.as_str()));
            self.pending.drain(..consumed);
        }
    }
}

/// Result of one auxiliary event stream socket read attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeEventStreamRead {
    /// Bytes were read and appended to the pending buffer.
    Read,
    /// No bytes are currently available without awaiting the socket.
    Pending,
    /// The auxiliary event stream disconnected.
    Disconnected,
}

/// Reports whether an event stream I/O error should be treated as disconnect.
pub(super) fn runtime_event_stream_disconnected(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
    )
}

/// Classifies one event notification body into an attach render action.
pub(super) fn attach_render_action_for_event_body(body: &str) -> AttachRenderAction {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return AttachRenderAction::None;
    };
    let Some(event_type) = event_type_from_notification(&value) else {
        return AttachRenderAction::None;
    };
    attach_render_action_for_event_type(event_type)
}

/// Extracts an event type from a JSON-RPC event notification.
pub(super) fn event_type_from_notification(value: &serde_json::Value) -> Option<&str> {
    if let Some(event_type) = value
        .get("params")
        .and_then(|params| params.get("event_type"))
        .and_then(serde_json::Value::as_str)
    {
        return Some(event_type);
    }
    value
        .get("method")
        .and_then(serde_json::Value::as_str)
        .and_then(|method| method.strip_prefix("event/"))
}

/// Maps a runtime event type onto the attached client's render needs.
pub(super) fn attach_render_action_for_event_type(event_type: &str) -> AttachRenderAction {
    match event_type {
        "diagnostic" | "snapshot_changed" => AttachRenderAction::None,
        "client_attached" | "client_detached" | "config_changed" | "observer_decided"
        | "window_changed" => AttachRenderAction::InvalidateAndView,
        "agent_status" | "approval_changed" | "hook_failed" | "mcp_server_changed" | "message"
        | "observer_requested" | "pane_changed" => AttachRenderAction::View,
        _ => AttachRenderAction::View,
    }
}

/// Connects to the auxiliary event socket for event-driven attach redraws.
pub(super) fn optional_control_socket_event_stream(
    control_socket_path: &std::path::Path,
) -> Result<Option<tokio::net::UnixStream>> {
    let event_socket_path =
        auxiliary_socket_path_for_control_socket(control_socket_path, AuxiliarySocketKind::Event)?;
    let stream = match UnixStream::connect(event_socket_path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(MezError::from(error)),
    };
    stream.set_nonblocking(true)?;
    Ok(Some(tokio::net::UnixStream::from_std(stream)?))
}
/// Checks whether the control socket has closed while no response is pending.
///
/// Idle control-socket attach loops avoid sending render requests after input
/// timeouts, but they still need to notice daemon teardown promptly. The socket
/// should not deliver unsolicited bytes in this state, so readable EOF means the
/// attached client can exit cleanly without reintroducing periodic renders.
pub(super) fn control_socket_disconnected_without_pending_response(
    stream: &tokio::net::UnixStream,
) -> Result<bool> {
    let mut byte = [0u8; 1];
    match stream.try_read(&mut byte) {
        Ok(0) => Ok(true),
        Ok(_) => Err(MezError::invalid_state(
            "control socket delivered an unexpected response while idle",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
        Err(error) => {
            let error = MezError::from(error);
            if attached_terminal_output_disconnected(&error) {
                Ok(true)
            } else {
                Err(error)
            }
        }
    }
}
