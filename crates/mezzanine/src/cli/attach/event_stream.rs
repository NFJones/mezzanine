//! Attached terminal polling and auxiliary runtime event-stream handling.

use super::{
    ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH, ATTACH_EVENT_STREAM_READ_BUFFER_BYTES,
    AsyncAttachedTerminalIo, AuxiliarySocketKind, MezError, Result, UnixStream,
    attached_terminal_output_disconnected, auxiliary_socket_path_for_control_socket,
    decode_control_frame, encode_control_body,
};
use std::io::Write;
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

/// Reads terminal input while accepting negotiated Iroh event wakeups.
pub(super) async fn read_attached_client_input_or_iroh_event<I: AsyncAttachedTerminalIo>(
    terminal_io: &mut I,
    event_receiver: &mut tokio::sync::mpsc::Receiver<Result<AttachRenderAction>>,
    max_bytes: usize,
    wake_deadline: tokio::time::Instant,
) -> Result<AttachedClientInputPoll> {
    let input = read_attached_client_input_or_deadline(terminal_io, max_bytes, None, wake_deadline);
    tokio::pin!(input);
    tokio::select! {
        biased;
        input = &mut input => input,
        event = event_receiver.recv() => match event {
            Some(Ok(render_action)) => Ok(AttachedClientInputPoll {
                bytes: Vec::new(),
                eof: false,
                render_action,
            }),
            Some(Err(error)) => Err(error),
            None => Ok(AttachedClientInputPoll {
                bytes: Vec::new(),
                eof: false,
                render_action: AttachRenderAction::Disconnect,
            }),
        },
    }
}

/// Builds the synthetic input poll produced by a local animation refresh tick.
pub(super) fn animation_refresh_input_poll() -> AttachedClientInputPoll {
    AttachedClientInputPoll {
        bytes: Vec::new(),
        eof: false,
        render_action: AttachRenderAction::View,
    }
}

/// Starts one bounded receiver for the negotiated Iroh event stream.
pub(in crate::cli) fn spawn_iroh_runtime_event_receiver(
    connection: iroh::endpoint::Connection,
    setup_timeout: std::time::Duration,
) -> (
    tokio::sync::mpsc::Receiver<Result<AttachRenderAction>>,
    tokio::task::JoinHandle<()>,
) {
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    let task = tokio::spawn(async move {
        let result = receive_iroh_runtime_events(connection, setup_timeout, &sender).await;
        if let Err(error) = result {
            let _ = sender.send(Err(error)).await;
        }
    });
    (receiver, task)
}

async fn receive_iroh_runtime_events(
    connection: iroh::endpoint::Connection,
    setup_timeout: std::time::Duration,
    sender: &tokio::sync::mpsc::Sender<Result<AttachRenderAction>>,
) -> Result<()> {
    let setup = async {
        let mut stream = tokio::select! {
            accepted = connection.accept_uni() => accepted.map_err(|_| {
                MezError::invalid_state("failed to accept negotiated Iroh event stream")
            })?,
            _ = connection.closed() => {
                return Err(MezError::invalid_state(
                    "Iroh connection closed before the negotiated event stream arrived",
                ));
            }
        };
        let mut preface = vec![0u8; crate::runtime::MEZZANINE_IROH_EVENT_STREAM_PREFACE.len()];
        stream
            .read_exact(&mut preface)
            .await
            .map_err(|_| MezError::invalid_state("Iroh event stream preface was truncated"))?;
        if preface != crate::runtime::MEZZANINE_IROH_EVENT_STREAM_PREFACE {
            return Err(MezError::invalid_state(
                "Iroh event stream used an unsupported preface or version",
            ));
        }
        Ok(stream)
    };
    let mut stream = match tokio::time::timeout(setup_timeout, setup).await {
        Ok(result) => result?,
        Err(_) => {
            connection.close(
                iroh::endpoint::VarInt::from_u32(1),
                b"event stream setup timed out",
            );
            return Err(MezError::invalid_state(
                "Iroh event stream setup timed out while awaiting the negotiated stream and preface",
            ));
        }
    };

    let mut pending = Vec::new();
    let mut buffer = [0u8; ATTACH_EVENT_STREAM_READ_BUFFER_BYTES];
    loop {
        let read = tokio::select! {
            read = stream.read(&mut buffer) => read.map_err(|_| {
                MezError::invalid_state("Iroh event stream read failed")
            })?,
            _ = connection.closed() => None,
        }
        .unwrap_or(0);
        if read == 0 {
            if !pending.is_empty() {
                return Err(MezError::invalid_state(
                    "Iroh event stream closed with an incomplete frame",
                ));
            }
            let _ = sender.send(Ok(AttachRenderAction::Disconnect)).await;
            return Ok(());
        }
        pending.extend_from_slice(&buffer[..read]);
        if pending.len() > ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH + 1024 {
            return Err(MezError::invalid_state(
                "Iroh event stream frame exceeds limit",
            ));
        }
        let mut action = AttachRenderAction::None;
        while let Ok((body, consumed)) =
            decode_control_frame(pending.as_slice(), ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH)
        {
            if consumed == 0 {
                break;
            }
            action = action.combine(strict_iroh_attach_render_action(body.as_str())?);
            pending.drain(..consumed);
        }
        if action != AttachRenderAction::None && sender.send(Ok(action)).await.is_err() {
            return Ok(());
        }
    }
}

fn strict_iroh_attach_render_action(body: &str) -> Result<AttachRenderAction> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("Iroh event stream contained invalid JSON"))?;
    if value.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err(MezError::invalid_state(
            "Iroh event stream notification omitted JSON-RPC 2.0",
        ));
    }
    let method = value
        .get("method")
        .and_then(serde_json::Value::as_str)
        .and_then(|method| method.strip_prefix("event/"))
        .ok_or_else(|| MezError::invalid_state("Iroh event stream contained a non-event frame"))?;
    let event_type = value
        .get("params")
        .and_then(serde_json::Value::as_object)
        .and_then(|params| params.get("event_type"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_state("Iroh event stream omitted event_type"))?;
    if method != event_type {
        return Err(MezError::invalid_state(
            "Iroh event stream method and event_type did not match",
        ));
    }
    Ok(attach_render_action_for_event_type(event_type))
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

#[cfg(test)]
mod iroh_setup_tests {
    use iroh::endpoint::{QuicTransportConfig, VarInt};
    use iroh::{Endpoint, RelayMode, SecretKey, endpoint::presets};
    use tokio::io::AsyncWriteExt;

    use super::*;

    /// Creates one connected local Iroh pair with server-opened unidirectional
    /// streams enabled on the attach-side client endpoint.
    async fn connected_iroh_event_pair() -> (
        Endpoint,
        Endpoint,
        iroh::endpoint::Connection,
        iroh::endpoint::Connection,
    ) {
        let server = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .alpns(vec![crate::runtime::MEZZANINE_IROH_ALPN.to_vec()])
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .bind()
            .await
            .unwrap();
        let client = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .transport_config(
                QuicTransportConfig::builder()
                    .max_concurrent_uni_streams(VarInt::from_u32(1))
                    .build(),
            )
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .bind()
            .await
            .unwrap();
        let server_addr = server.addr();
        let server_accept = tokio::spawn({
            let server = server.clone();
            async move {
                server
                    .accept()
                    .await
                    .unwrap()
                    .accept()
                    .unwrap()
                    .await
                    .unwrap()
            }
        });
        let client_connection = client
            .connect(server_addr, crate::runtime::MEZZANINE_IROH_ALPN)
            .await
            .unwrap();
        let server_connection = server_accept.await.unwrap();
        (server, client, server_connection, client_connection)
    }

    /// Verifies a peer that never opens the negotiated event stream is bounded
    /// by setup timeout and the timed-out attach connection is closed.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_event_receiver_times_out_acceptance_and_closes_connection() {
        let (server, client, server_connection, client_connection) =
            connected_iroh_event_pair().await;
        let (mut receiver, task) = spawn_iroh_runtime_event_receiver(
            client_connection,
            std::time::Duration::from_millis(50),
        );

        let error = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .expect("event receiver must settle within setup timeout")
            .expect("event receiver must report setup failure")
            .unwrap_err();
        assert!(error.message().contains("setup timed out"), "{error}");
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            server_connection.closed(),
        )
        .await
        .expect("timed-out event setup must close the peer connection");

        task.await.unwrap();
        client.close().await;
        server.close().await;
    }

    /// Verifies an event stream opened within setup timeout accepts the version
    /// preface and continues delivering framed runtime notifications.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_event_receiver_accepts_preface_and_delivers_event() {
        let (server, client, server_connection, client_connection) =
            connected_iroh_event_pair().await;
        let (mut receiver, task) = spawn_iroh_runtime_event_receiver(
            client_connection.clone(),
            std::time::Duration::from_secs(1),
        );
        let mut stream = server_connection.open_uni().await.unwrap();
        stream
            .write_all(crate::runtime::MEZZANINE_IROH_EVENT_STREAM_PREFACE)
            .await
            .unwrap();
        stream
            .write_all(&crate::control::encode_control_body(
                r#"{"jsonrpc":"2.0","method":"event/pane_changed","params":{"event_type":"pane_changed"}}"#,
            ))
            .await
            .unwrap();
        stream.flush().await.unwrap();

        let action = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(action, AttachRenderAction::View);

        client_connection.close(VarInt::from_u32(0), b"test complete");
        task.await.unwrap();
        client.close().await;
        server.close().await;
    }
}

/// Connects to the auxiliary event socket for event-driven attach redraws.
pub(super) fn optional_control_socket_event_stream(
    control_socket_path: &std::path::Path,
    binding_token: &str,
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
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "event-init",
        "method": "event/initialize",
        "params": {
            "binding_token": binding_token,
            "after_event_id": 0
        }
    })
    .to_string();
    (&stream).write_all(&encode_control_body(&initialize))?;
    (&stream).flush()?;
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

#[cfg(test)]
mod iroh_tests {
    use super::*;

    #[test]
    fn strict_iroh_event_frames_require_matching_event_notifications() {
        let valid = strict_iroh_attach_render_action(
            r#"{"jsonrpc":"2.0","method":"event/pane_changed","params":{"event_type":"pane_changed"}}"#,
        )
        .unwrap();
        assert_eq!(valid, AttachRenderAction::View);

        let mismatch = strict_iroh_attach_render_action(
            r#"{"jsonrpc":"2.0","method":"event/pane_changed","params":{"event_type":"window_changed"}}"#,
        )
        .expect_err("mismatched method and event_type must be rejected");
        assert!(mismatch.message().contains("did not match"));

        let response = strict_iroh_attach_render_action(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#)
            .expect_err("control responses must not be accepted on the event stream");
        assert!(response.message().contains("non-event frame"));
    }
}
