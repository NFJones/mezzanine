//! Attached terminal polling and auxiliary runtime event-stream handling.

use super::{
    ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH, ATTACH_EVENT_STREAM_READ_BUFFER_BYTES,
    AsyncAttachedTerminalIo, AuxiliarySocketKind, MezError, Result, UnixStream,
    attached_terminal_output_disconnected, auxiliary_socket_path_for_control_socket,
    decode_control_frame, encode_control_body,
};
use base64::Engine as _;
use std::io::Write;
use tokio::io::AsyncReadExt;

use crate::runtime::{IrohCompressionPolicy, RuntimeIrohCompressionCodec};

const IROH_CLIENT_CLIPBOARD_MAX_BYTES: usize = 8 * 1024 * 1024;
const IROH_CLIENT_CLIPBOARD_MAX_CHUNK_BYTES: usize = 256 * 1024;
const IROH_CLIENT_CLIPBOARD_MAX_CHUNKS: usize =
    IROH_CLIENT_CLIPBOARD_MAX_BYTES / IROH_CLIENT_CLIPBOARD_MAX_CHUNK_BYTES;

/// One bounded in-progress client clipboard transfer.
struct IrohClipboardTransfer {
    sequence: u64,
    total_bytes: usize,
    chunk_count: usize,
    next_index: usize,
    bytes: Vec<u8>,
    started_at: tokio::time::Instant,
}

/// Strict connection-local assembler for negotiated clipboard effect frames.
#[derive(Default)]
struct IrohClipboardAssembler {
    last_sequence: u64,
    transfer: Option<IrohClipboardTransfer>,
}

impl IrohClipboardAssembler {
    /// Applies one clipboard notification and returns completed UTF-8 content.
    fn apply(&mut self, body: &str) -> Result<Option<String>> {
        self.discard_expired();
        let value: serde_json::Value = serde_json::from_str(body)
            .map_err(|_| MezError::invalid_args("invalid Iroh clipboard effect JSON"))?;
        let method = value
            .get("method")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| MezError::invalid_args("Iroh clipboard effect omitted method"))?;
        let params = value
            .get("params")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| MezError::invalid_args("Iroh clipboard effect omitted params"))?;
        let sequence = params
            .get("sequence")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| MezError::invalid_args("Iroh clipboard effect omitted sequence"))?;

        match method {
            "client/clipboard.begin" => {
                self.transfer = None;
                let total_bytes = params
                    .get("total_bytes")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| {
                        MezError::invalid_args("Iroh clipboard begin omitted total byte count")
                    })?;
                let chunk_count = params
                    .get("chunks")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| {
                        MezError::invalid_args("Iroh clipboard begin omitted chunk count")
                    })?;
                if sequence <= self.last_sequence
                    || total_bytes > IROH_CLIENT_CLIPBOARD_MAX_BYTES
                    || chunk_count == 0
                    || chunk_count > IROH_CLIENT_CLIPBOARD_MAX_CHUNKS
                {
                    return Err(MezError::invalid_args(
                        "Iroh clipboard begin exceeds sequence or size bounds",
                    ));
                }
                self.transfer = Some(IrohClipboardTransfer {
                    sequence,
                    total_bytes,
                    chunk_count,
                    next_index: 0,
                    bytes: Vec::with_capacity(total_bytes),
                    started_at: tokio::time::Instant::now(),
                });
                Ok(None)
            }
            "client/clipboard.chunk" => {
                let index = params
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| MezError::invalid_args("Iroh clipboard chunk omitted index"))?;
                let encoded = params
                    .get("data_base64")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        MezError::invalid_args("Iroh clipboard chunk omitted encoded data")
                    })?;
                let transfer = self.transfer.as_mut().ok_or_else(|| {
                    MezError::invalid_args("Iroh clipboard chunk has no active transfer")
                })?;
                if sequence != transfer.sequence
                    || index != transfer.next_index
                    || index >= transfer.chunk_count
                {
                    self.transfer = None;
                    return Err(MezError::invalid_args(
                        "Iroh clipboard chunk ordering is invalid",
                    ));
                }
                let chunk = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| MezError::invalid_args("Iroh clipboard chunk is not base64"))?;
                if chunk.len() > IROH_CLIENT_CLIPBOARD_MAX_CHUNK_BYTES
                    || transfer.bytes.len().saturating_add(chunk.len()) > transfer.total_bytes
                {
                    self.transfer = None;
                    return Err(MezError::invalid_args(
                        "Iroh clipboard chunk exceeds declared bounds",
                    ));
                }
                transfer.bytes.extend_from_slice(&chunk);
                transfer.next_index += 1;
                Ok(None)
            }
            "client/clipboard.commit" => {
                let transfer = self.transfer.take().ok_or_else(|| {
                    MezError::invalid_args("Iroh clipboard commit has no active transfer")
                })?;
                if sequence != transfer.sequence
                    || transfer.next_index != transfer.chunk_count
                    || transfer.bytes.len() != transfer.total_bytes
                {
                    return Err(MezError::invalid_args(
                        "Iroh clipboard commit does not match the declared transfer",
                    ));
                }
                let content = String::from_utf8(transfer.bytes).map_err(|_| {
                    MezError::invalid_args("Iroh clipboard transfer is not valid UTF-8")
                })?;
                self.last_sequence = sequence;
                Ok(Some(content))
            }
            _ => Err(MezError::invalid_args(
                "unsupported Iroh clipboard effect method",
            )),
        }
    }

    /// Discards a partial transfer after the bounded completion deadline.
    fn discard_expired(&mut self) {
        const TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        if self
            .transfer
            .as_ref()
            .is_some_and(|transfer| transfer.started_at.elapsed() >= TRANSFER_TIMEOUT)
        {
            self.transfer = None;
        }
    }

    /// Returns the deadline for the current partial transfer, when present.
    fn expiration_deadline(&self) -> Option<tokio::time::Instant> {
        const TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        self.transfer
            .as_ref()
            .map(|transfer| transfer.started_at + TRANSFER_TIMEOUT)
    }
}

/// Starts one single-pending-value worker for client-local clipboard writes.
fn spawn_iroh_client_clipboard_worker(
    clipboard: crate::host::terminal::HostClipboard,
) -> (
    tokio::sync::watch::Sender<Option<String>>,
    tokio::task::JoinHandle<()>,
) {
    let (sender, mut receiver) = tokio::sync::watch::channel(None::<String>);
    let task = tokio::spawn(async move {
        while receiver.changed().await.is_ok() {
            let Some(content) = receiver.borrow_and_update().clone() else {
                continue;
            };
            let clipboard = clipboard.clone();
            let _ = tokio::task::spawn_blocking(move || clipboard.copy(content.as_str())).await;
        }
    });
    (sender, task)
}

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
    animation_deadline: Option<tokio::time::Instant>,
    wake_deadline: tokio::time::Instant,
) -> Result<AttachedClientInputPoll> {
    let wake_deadline = animation_deadline
        .filter(|deadline| *deadline <= wake_deadline)
        .unwrap_or(wake_deadline);
    let input = read_attached_client_input_or_deadline(
        terminal_io,
        max_bytes,
        animation_deadline,
        wake_deadline,
    );
    tokio::pin!(input);
    tokio::select! {
        biased;
        input = &mut input => input,
        event = event_receiver.recv() => match event {
            Some(Ok(render_action)) => Ok(AttachedClientInputPoll {
                bytes: Vec::new(),
                eof: false,
                render_action: coalesce_ready_iroh_render_actions(
                    event_receiver,
                    render_action,
                )?,
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

/// Collapses already-ready Iroh redraw wakeups before the next view fetch.
fn coalesce_ready_iroh_render_actions(
    event_receiver: &mut tokio::sync::mpsc::Receiver<Result<AttachRenderAction>>,
    mut render_action: AttachRenderAction,
) -> Result<AttachRenderAction> {
    loop {
        match event_receiver.try_recv() {
            Ok(Ok(next_action)) => render_action = render_action.combine(next_action),
            Ok(Err(error)) => return Err(error),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return Ok(render_action),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                return Ok(render_action.combine(AttachRenderAction::Disconnect));
            }
        }
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
    compression: IrohCompressionPolicy,
    setup_timeout: std::time::Duration,
    event_stream_version: u32,
    clipboard: Option<crate::host::terminal::HostClipboard>,
) -> (
    tokio::sync::mpsc::Receiver<Result<AttachRenderAction>>,
    tokio::task::JoinHandle<()>,
) {
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    let task = tokio::spawn(async move {
        let clipboard_worker = clipboard.map(spawn_iroh_client_clipboard_worker);
        let clipboard_sender = clipboard_worker
            .as_ref()
            .map(|(clipboard_sender, _)| clipboard_sender.clone());
        let result = receive_iroh_runtime_events(
            connection,
            compression,
            setup_timeout,
            event_stream_version,
            clipboard_sender.as_ref(),
            &sender,
        )
        .await;
        if let Some((clipboard_sender, clipboard_task)) = clipboard_worker {
            drop(clipboard_sender);
            clipboard_task.abort();
            let _ = clipboard_task.await;
        }
        if let Err(error) = result {
            let _ = sender.send(Err(error)).await;
        }
    });
    (receiver, task)
}

async fn receive_iroh_runtime_events(
    connection: iroh::endpoint::Connection,
    compression: IrohCompressionPolicy,
    setup_timeout: std::time::Duration,
    event_stream_version: u32,
    clipboard_sender: Option<&tokio::sync::watch::Sender<Option<String>>>,
    sender: &tokio::sync::mpsc::Sender<Result<AttachRenderAction>>,
) -> Result<()> {
    if !matches!(event_stream_version, 1 | 2) {
        return Err(MezError::invalid_args(
            "unsupported negotiated Iroh event stream version",
        ));
    }
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
        let expected_preface = if event_stream_version == 2 {
            crate::runtime::MEZZANINE_IROH_EVENT_STREAM_V2_PREFACE
        } else {
            crate::runtime::MEZZANINE_IROH_EVENT_STREAM_PREFACE
        };
        let mut preface = vec![0u8; expected_preface.len()];
        stream
            .read_exact(&mut preface)
            .await
            .map_err(|_| MezError::invalid_state("Iroh event stream preface was truncated"))?;
        if preface != expected_preface {
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
    let mut clipboard_assembler = (event_stream_version == 2).then(IrohClipboardAssembler::default);
    let mut buffer = [0u8; ATTACH_EVENT_STREAM_READ_BUFFER_BYTES];
    loop {
        let read = if let Some(deadline) = clipboard_assembler
            .as_ref()
            .and_then(IrohClipboardAssembler::expiration_deadline)
        {
            tokio::select! {
                read = stream.read(&mut buffer) => read.map_err(|_| {
                    MezError::invalid_state("Iroh event stream read failed")
                })?,
                _ = connection.closed() => None,
                _ = tokio::time::sleep_until(deadline) => {
                    if let Some(assembler) = clipboard_assembler.as_mut() {
                        assembler.discard_expired();
                    }
                    continue;
                }
            }
            .unwrap_or(0)
        } else {
            tokio::select! {
                read = stream.read(&mut buffer) => read.map_err(|_| {
                    MezError::invalid_state("Iroh event stream read failed")
                })?,
                _ = connection.closed() => None,
            }
            .unwrap_or(0)
        };
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
        let action = drain_negotiated_iroh_event_frames(
            &mut pending,
            compression,
            clipboard_assembler.as_mut(),
            clipboard_sender,
        )?;
        if action != AttachRenderAction::None && sender.send(Ok(action)).await.is_err() {
            return Ok(());
        }
    }
}

/// Drains complete event frames under the immutable connection-local codec.
fn drain_negotiated_iroh_event_frames(
    pending: &mut Vec<u8>,
    compression: IrohCompressionPolicy,
    mut clipboard_assembler: Option<&mut IrohClipboardAssembler>,
    clipboard_sender: Option<&tokio::sync::watch::Sender<Option<String>>>,
) -> Result<AttachRenderAction> {
    let mut action = AttachRenderAction::None;
    loop {
        let (decoded, consumed) = if compression.codec() == RuntimeIrohCompressionCodec::None {
            let Ok((body, consumed)) =
                decode_control_frame(pending, ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH)
            else {
                return Ok(action);
            };
            action = action.combine(apply_negotiated_iroh_attach_frame(
                body.as_str(),
                clipboard_assembler.as_deref_mut(),
                clipboard_sender,
            )?);
            pending.drain(..consumed);
            continue;
        } else {
            if pending.len() < IrohCompressionPolicy::envelope_header_length() {
                return Ok(action);
            }
            let envelope_length = compression.declared_envelope_length(pending)?;
            if pending.len() < envelope_length {
                return Ok(action);
            }
            (
                compression.decode_frame(&pending[..envelope_length])?,
                envelope_length,
            )
        };
        let (body, inner_consumed) =
            decode_control_frame(&decoded, ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH)?;
        if inner_consumed != decoded.len() {
            return Err(MezError::invalid_args(
                "negotiated Iroh event envelope must contain exactly one frame",
            ));
        }
        action = action.combine(apply_negotiated_iroh_attach_frame(
            body.as_str(),
            clipboard_assembler.as_deref_mut(),
            clipboard_sender,
        )?);
        pending.drain(..consumed);
    }
}

/// Applies one decoded event or negotiated transient client-effect frame.
fn apply_negotiated_iroh_attach_frame(
    body: &str,
    clipboard_assembler: Option<&mut IrohClipboardAssembler>,
    clipboard_sender: Option<&tokio::sync::watch::Sender<Option<String>>>,
) -> Result<AttachRenderAction> {
    let method = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("method")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    if method
        .as_deref()
        .is_some_and(|method| method.starts_with("client/clipboard."))
    {
        let Some(assembler) = clipboard_assembler else {
            return Ok(AttachRenderAction::None);
        };
        match assembler.apply(body) {
            Ok(Some(content)) => {
                if let Some(sender) = clipboard_sender {
                    sender.send_replace(Some(content));
                }
            }
            Ok(None) => {}
            Err(_) => assembler.transfer = None,
        }
        return Ok(AttachRenderAction::None);
    }
    strict_iroh_attach_render_action(body)
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
        "client_attached" | "client_detached" | "config_changed" | "window_changed" => {
            AttachRenderAction::InvalidateAndView
        }
        "agent_status" | "approval_changed" | "hook_failed" | "mcp_server_changed" | "message"
        | "pane_changed" => AttachRenderAction::View,
        _ => AttachRenderAction::View,
    }
}

#[cfg(test)]
mod iroh_setup_tests {
    use iroh::endpoint::{QuicTransportConfig, VarInt};
    use iroh::{Endpoint, RelayMode, SecretKey, endpoint::presets};
    use std::sync::Mutex;
    use tokio::io::AsyncWriteExt;

    use super::*;

    static IROH_CLIENT_CLIPBOARD_WRITES: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn record_iroh_client_clipboard_write(content: &str) -> bool {
        IROH_CLIENT_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .push(content.to_string());
        true
    }

    fn empty_iroh_client_clipboard_read() -> Option<String> {
        None
    }

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
            IrohCompressionPolicy::new(
                RuntimeIrohCompressionCodec::None,
                1,
                3,
                ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH + 1024,
            )
            .unwrap(),
            std::time::Duration::from_millis(50),
            1,
            None,
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
            IrohCompressionPolicy::new(
                RuntimeIrohCompressionCodec::None,
                1,
                3,
                ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH + 1024,
            )
            .unwrap(),
            std::time::Duration::from_secs(1),
            1,
            None,
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

    /// Verifies a negotiated v2 stream assembles one client-local clipboard
    /// write, executes it through the client-owned adapter, and continues to
    /// deliver later render events after a malformed transfer is discarded.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_v2_event_receiver_executes_clipboard_and_survives_malformed_effects() {
        IROH_CLIENT_CLIPBOARD_WRITES.lock().unwrap().clear();
        let (server, client, server_connection, client_connection) =
            connected_iroh_event_pair().await;
        let clipboard = crate::host::terminal::HostClipboard::new(
            record_iroh_client_clipboard_write,
            empty_iroh_client_clipboard_read,
        );
        let (mut receiver, task) = spawn_iroh_runtime_event_receiver(
            client_connection.clone(),
            IrohCompressionPolicy::new(
                RuntimeIrohCompressionCodec::None,
                1,
                3,
                ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH + 1024,
            )
            .unwrap(),
            std::time::Duration::from_secs(1),
            2,
            Some(clipboard),
        );
        let mut stream = server_connection.open_uni().await.unwrap();
        stream
            .write_all(crate::runtime::MEZZANINE_IROH_EVENT_STREAM_V2_PREFACE)
            .await
            .unwrap();
        for body in [
            r#"{"jsonrpc":"2.0","method":"client/clipboard.begin","params":{"sequence":1,"total_bytes":6,"chunks":1}}"#,
            r#"{"jsonrpc":"2.0","method":"client/clipboard.chunk","params":{"sequence":1,"index":1,"data_base64":"c2VjcmV0"}}"#,
            r#"{"jsonrpc":"2.0","method":"client/clipboard.begin","params":{"sequence":2,"total_bytes":5,"chunks":1}}"#,
            r#"{"jsonrpc":"2.0","method":"client/clipboard.chunk","params":{"sequence":2,"index":0,"data_base64":"aGVsbG8="}}"#,
            r#"{"jsonrpc":"2.0","method":"client/clipboard.commit","params":{"sequence":2}}"#,
            r#"{"jsonrpc":"2.0","method":"event/pane_changed","params":{"event_type":"pane_changed"}}"#,
        ] {
            stream
                .write_all(&crate::control::encode_control_body(body))
                .await
                .unwrap();
        }
        stream.flush().await.unwrap();

        let action = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(action, AttachRenderAction::View);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if !IROH_CLIENT_CLIPBOARD_WRITES.lock().unwrap().is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            IROH_CLIENT_CLIPBOARD_WRITES.lock().unwrap().as_slice(),
            ["hello"]
        );

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

    /// Verifies an Iroh input poll wakes at the rendered animation deadline
    /// even when neither terminal input nor a runtime event is available.
    #[tokio::test(start_paused = true, flavor = "current_thread")]
    async fn iroh_input_poll_honors_animation_deadline_without_event() {
        let mut terminal_io = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
        terminal_io.push_pending_input_read();
        let (_sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let animation_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(25);
        let wake_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);

        let input = read_attached_client_input_or_iroh_event(
            &mut terminal_io,
            &mut receiver,
            4096,
            Some(animation_deadline),
            wake_deadline,
        )
        .await
        .unwrap();

        assert!(input.bytes.is_empty());
        assert!(!input.eof);
        assert_eq!(input.render_action, AttachRenderAction::View);
    }

    /// Verifies a ready Iroh redraw burst becomes one strongest action instead
    /// of leaving stale redraw work queued behind the next authoritative view.
    #[test]
    fn ready_iroh_render_actions_are_coalesced_before_view_fetch() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        sender.try_send(Ok(AttachRenderAction::View)).unwrap();
        sender
            .try_send(Ok(AttachRenderAction::InvalidateAndView))
            .unwrap();

        assert_eq!(
            coalesce_ready_iroh_render_actions(&mut receiver, AttachRenderAction::View).unwrap(),
            AttachRenderAction::InvalidateAndView
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    /// Verifies coalescing does not hide a queued decode or transport failure,
    /// because malformed compressed events must still fail the attach visibly.
    #[test]
    fn ready_iroh_render_action_coalescing_preserves_errors() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        sender
            .try_send(Err(MezError::invalid_state("queued Iroh event failure")))
            .unwrap();

        let error = coalesce_ready_iroh_render_actions(&mut receiver, AttachRenderAction::View)
            .expect_err("queued Iroh event failures must not be discarded");
        assert!(error.message().contains("queued Iroh event failure"));
    }

    /// Verifies negotiated Zstandard and LZ4 event envelopes decode to the
    /// same strict render action while incomplete envelopes remain buffered.
    #[test]
    fn compressed_iroh_event_frames_decode_incrementally() {
        let frame = encode_control_body(
            r#"{"jsonrpc":"2.0","method":"event/pane_changed","params":{"event_type":"pane_changed","padding":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#,
        );
        for codec in [
            RuntimeIrohCompressionCodec::Zstd,
            RuntimeIrohCompressionCodec::Lz4,
        ] {
            let compression = IrohCompressionPolicy::new(
                codec,
                1,
                3,
                ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH + 1024,
            )
            .unwrap();
            let encoded = compression
                .encode_frame(&frame, crate::runtime::IrohFrameCompressionMode::Eligible)
                .unwrap();
            let split = encoded.as_bytes().len() - 1;
            let mut pending = encoded.as_bytes()[..split].to_vec();
            assert_eq!(
                drain_negotiated_iroh_event_frames(&mut pending, compression, None, None).unwrap(),
                AttachRenderAction::None
            );
            pending.extend_from_slice(&encoded.as_bytes()[split..]);
            assert_eq!(
                drain_negotiated_iroh_event_frames(&mut pending, compression, None, None).unwrap(),
                AttachRenderAction::View
            );
            assert!(pending.is_empty());
        }
    }

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

    /// Verifies a negotiated v2 assembler accepts contiguous single- and
    /// multi-chunk transfers while retaining the original UTF-8 bytes.
    #[test]
    fn iroh_clipboard_assembler_completes_ordered_bounded_transfers() {
        let mut assembler = IrohClipboardAssembler::default();
        assert!(assembler
            .apply(r#"{"jsonrpc":"2.0","method":"client/clipboard.begin","params":{"sequence":7,"total_bytes":5,"chunks":2}}"#)
            .unwrap()
            .is_none());
        assert!(assembler
            .apply(r#"{"jsonrpc":"2.0","method":"client/clipboard.chunk","params":{"sequence":7,"index":0,"data_base64":"aGU="}}"#)
            .unwrap()
            .is_none());
        assert!(assembler
            .apply(r#"{"jsonrpc":"2.0","method":"client/clipboard.chunk","params":{"sequence":7,"index":1,"data_base64":"bGxv"}}"#)
            .unwrap()
            .is_none());
        assert_eq!(
            assembler
                .apply(r#"{"jsonrpc":"2.0","method":"client/clipboard.commit","params":{"sequence":7}}"#)
                .unwrap()
                .as_deref(),
            Some("hello")
        );
    }

    /// Verifies malformed sequencing is discarded without exposing payload
    /// bytes through errors or producing a clipboard write.
    #[test]
    fn iroh_clipboard_assembler_discards_malformed_private_transfers() {
        let mut assembler = IrohClipboardAssembler::default();
        assembler
            .apply(r#"{"jsonrpc":"2.0","method":"client/clipboard.begin","params":{"sequence":9,"total_bytes":6,"chunks":1}}"#)
            .unwrap();
        let error = assembler
            .apply(r#"{"jsonrpc":"2.0","method":"client/clipboard.chunk","params":{"sequence":9,"index":1,"data_base64":"c2VjcmV0"}}"#)
            .unwrap_err();

        assert!(error.message().contains("ordering"), "{error:?}");
        assert!(!error.message().contains("secret"), "{error:?}");
        assert!(assembler
            .apply(r#"{"jsonrpc":"2.0","method":"client/clipboard.commit","params":{"sequence":9}}"#)
            .is_err());
    }

    /// Verifies incomplete sensitive payloads expire without requiring a new
    /// begin frame and cannot later be committed.
    #[tokio::test(start_paused = true, flavor = "current_thread")]
    async fn iroh_clipboard_assembler_expires_incomplete_transfer() {
        let mut assembler = IrohClipboardAssembler::default();
        assembler
            .apply(r#"{"jsonrpc":"2.0","method":"client/clipboard.begin","params":{"sequence":11,"total_bytes":6,"chunks":1}}"#)
            .unwrap();

        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        assembler.discard_expired();

        assert!(assembler.transfer.is_none());
        assert!(assembler
            .apply(r#"{"jsonrpc":"2.0","method":"client/clipboard.commit","params":{"sequence":11}}"#)
            .is_err());
    }
}
