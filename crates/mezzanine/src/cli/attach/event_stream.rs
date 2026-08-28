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
/// One consumer-visible wakeup plus one decoder-local latest wakeup bounds
/// presentation work without blocking ordered render-revision reconstruction.
const IROH_RENDER_WAKEUP_CHANNEL_CAPACITY: usize = 1;

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
    /// Latest authoritative v3 snapshot received with this input poll.
    pub(super) pushed_snapshot: Option<IrohPushedRenderSnapshot>,
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

/// One Iroh redraw wakeup paired with its ordered server event identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cli) struct IrohAttachRenderWakeup {
    /// Strongest redraw action implied by the decoded event burst.
    pub(super) action: AttachRenderAction,
    /// Latest ordered event represented by this wakeup, when supplied.
    event_id: Option<u64>,
    /// Latest authoritative pushed snapshot in this decoded burst.
    pub(super) pushed_snapshot: Option<IrohPushedRenderSnapshot>,
}

/// One validated authoritative render snapshot received on event-stream v3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IrohPushedRenderSnapshot {
    /// Stream-local monotonic revision assigned by the server.
    pub(super) revision: u64,
    /// Complete logical client frame ready for the shared terminal renderer.
    pub(super) frame: super::AttachClientFrame,
    /// Whether the retained physical-output diff base must be discarded first.
    pub(super) invalidate_output: bool,
}

/// Complete client render base retained for atomic v3 delta application.
#[derive(Debug, Clone)]
struct IrohRetainedRenderState {
    /// Last completely validated stream-local render revision.
    revision: u64,
    /// Complete rendered-view JSON represented by `revision`.
    view: Option<serde_json::Value>,
    /// Role this negotiated stream is authorized to render.
    expected_role: String,
}

impl IrohRetainedRenderState {
    /// Starts an empty retained base for one negotiated client role.
    fn new(expected_role: impl Into<String>) -> Self {
        Self {
            revision: 0,
            view: None,
            expected_role: expected_role.into(),
        }
    }
}

impl Default for IrohRetainedRenderState {
    fn default() -> Self {
        Self::new("primary")
    }
}

impl IrohAttachRenderWakeup {
    /// Builds a wakeup from one decoded event notification.
    const fn new(action: AttachRenderAction, event_id: Option<u64>) -> Self {
        Self {
            action,
            event_id,
            pushed_snapshot: None,
        }
    }

    /// Builds one immediately applicable authoritative v3 snapshot payload.
    pub(super) fn pushed_snapshot(snapshot: IrohPushedRenderSnapshot) -> Self {
        Self {
            action: AttachRenderAction::None,
            event_id: Some(snapshot.frame.event_cutoff.unwrap_or(0)),
            pushed_snapshot: Some(snapshot),
        }
    }

    /// Combines a burst while retaining the strongest action and newest event.
    fn combine(self, other: Self) -> Self {
        let invalidate_output = self
            .pushed_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.invalidate_output)
            || other
                .pushed_snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.invalidate_output);
        let mut pushed_snapshot = match (self.pushed_snapshot, other.pushed_snapshot) {
            (Some(left), Some(right)) if left.revision >= right.revision => Some(left),
            (_, Some(right)) => Some(right),
            (Some(left), None) => Some(left),
            (None, None) => None,
        };
        if let Some(snapshot) = pushed_snapshot.as_mut() {
            snapshot.invalidate_output |= invalidate_output;
        }
        Self {
            action: self.action.combine(other.action),
            event_id: match (self.event_id, other.event_id) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (Some(_), None) | (None, Some(_)) | (None, None) => None,
            },
            pushed_snapshot,
        }
    }

    /// Reports whether an authoritative view already represents this ordinary redraw.
    fn is_covered_by(&self, event_cutoff: Option<u64>) -> bool {
        self.action == AttachRenderAction::View
            && matches!((self.event_id, event_cutoff), (Some(event_id), Some(cutoff)) if event_id <= cutoff)
    }
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
            pushed_snapshot: None,
        }),
        Ok(Ok(bytes)) => Ok(AttachedClientInputPoll {
            bytes,
            eof: false,
            render_action: AttachRenderAction::None,
            pushed_snapshot: None,
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
            pushed_snapshot: None,
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
                pushed_snapshot: None,
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
    event_receiver: &mut tokio::sync::mpsc::Receiver<Result<IrohAttachRenderWakeup>>,
    max_bytes: usize,
    animation_deadline: Option<tokio::time::Instant>,
    wake_deadline: tokio::time::Instant,
    event_cutoff: Option<u64>,
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
    let mut input = tokio::select! {
        biased;
        input = &mut input => input,
        event = event_receiver.recv() => match event {
            Some(Ok(wakeup)) => {
                let wakeup = coalesce_ready_iroh_render_actions(
                    event_receiver,
                    wakeup,
                    event_cutoff,
                )?;
                return Ok(AttachedClientInputPoll {
                    bytes: Vec::new(),
                    eof: false,
                    render_action: wakeup.action,
                    pushed_snapshot: wakeup.pushed_snapshot,
                });
            }
            Some(Err(error)) => return Err(error),
            None => return Ok(AttachedClientInputPoll {
                bytes: Vec::new(),
                eof: false,
                render_action: AttachRenderAction::Disconnect,
                pushed_snapshot: None,
            }),
        },
    }?;
    if !input.eof && !input.bytes.is_empty() {
        let wakeup = coalesce_ready_iroh_render_actions(
            event_receiver,
            IrohAttachRenderWakeup::new(AttachRenderAction::None, None),
            event_cutoff,
        )?;
        input.render_action = input.render_action.combine(wakeup.action);
        input.pushed_snapshot = wakeup.pushed_snapshot;
    }
    Ok(input)
}

/// Collapses already-ready Iroh redraw wakeups before the next view fetch.
pub(super) fn coalesce_ready_iroh_render_actions(
    event_receiver: &mut tokio::sync::mpsc::Receiver<Result<IrohAttachRenderWakeup>>,
    initial: IrohAttachRenderWakeup,
    event_cutoff: Option<u64>,
) -> Result<IrohAttachRenderWakeup> {
    let mut wakeup = if initial.is_covered_by(event_cutoff) {
        IrohAttachRenderWakeup::new(AttachRenderAction::None, initial.event_id)
    } else {
        initial
    };
    loop {
        match event_receiver.try_recv() {
            Ok(Ok(ready)) if ready.is_covered_by(event_cutoff) => {}
            Ok(Ok(ready)) => wakeup = wakeup.combine(ready),
            Ok(Err(error)) => return Err(error),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return Ok(wakeup),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                return Ok(wakeup.combine(IrohAttachRenderWakeup::new(
                    AttachRenderAction::Disconnect,
                    None,
                )));
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
        pushed_snapshot: None,
    }
}

/// Starts one bounded receiver for the negotiated Iroh event stream.
pub(in crate::cli) fn spawn_iroh_runtime_event_receiver(
    connection: iroh::endpoint::Connection,
    compression: IrohCompressionPolicy,
    setup_timeout: std::time::Duration,
    event_stream_version: u32,
    allow_pushed_render: bool,
    pushed_render_role: Option<String>,
    clipboard: Option<crate::host::terminal::HostClipboard>,
) -> (
    tokio::sync::mpsc::Receiver<Result<IrohAttachRenderWakeup>>,
    tokio::task::JoinHandle<()>,
) {
    let (sender, receiver) = tokio::sync::mpsc::channel(IROH_RENDER_WAKEUP_CHANNEL_CAPACITY);
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
            allow_pushed_render,
            pushed_render_role,
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

/// Retains only the latest complete decoded render wakeup awaiting delivery.
///
/// Every revision is still decoded into connection-local retained state before
/// this helper runs. Coalescing here only prevents presentation from replaying
/// superseded frames, and `combine` carries any skipped invalidation forward.
fn retain_latest_iroh_render_wakeup(
    pending: &mut Option<IrohAttachRenderWakeup>,
    wakeup: IrohAttachRenderWakeup,
) {
    *pending = Some(match pending.take() {
        Some(pending) => pending.combine(wakeup),
        None => wakeup,
    });
}

#[allow(
    clippy::too_many_arguments,
    reason = "connection setup, negotiated framing, render ownership, clipboard routing, and delivery are independent event receiver inputs"
)]
async fn receive_iroh_runtime_events(
    connection: iroh::endpoint::Connection,
    compression: IrohCompressionPolicy,
    setup_timeout: std::time::Duration,
    event_stream_version: u32,
    allow_pushed_render: bool,
    pushed_render_role: Option<String>,
    clipboard_sender: Option<&tokio::sync::watch::Sender<Option<String>>>,
    sender: &tokio::sync::mpsc::Sender<Result<IrohAttachRenderWakeup>>,
) -> Result<()> {
    if !matches!(event_stream_version, 1..=3) {
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
        let expected_preface = match event_stream_version {
            3 => crate::runtime::MEZZANINE_IROH_EVENT_STREAM_V3_PREFACE,
            2 => crate::runtime::MEZZANINE_IROH_EVENT_STREAM_V2_PREFACE,
            _ => crate::runtime::MEZZANINE_IROH_EVENT_STREAM_PREFACE,
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
    let mut clipboard_assembler = clipboard_sender
        .is_some()
        .then(IrohClipboardAssembler::default);
    let mut render_state =
        IrohRetainedRenderState::new(pushed_render_role.unwrap_or_else(|| "primary".to_string()));
    let mut buffer = [0u8; ATTACH_EVENT_STREAM_READ_BUFFER_BYTES];
    let mut pending_delivery: Option<IrohAttachRenderWakeup> = None;
    loop {
        let read = {
            let read = async {
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
                            return Ok::<Option<usize>, MezError>(None);
                        }
                    }
                } else {
                    tokio::select! {
                        read = stream.read(&mut buffer) => read.map_err(|_| {
                            MezError::invalid_state("Iroh event stream read failed")
                        })?,
                        _ = connection.closed() => None,
                    }
                };
                Ok::<Option<usize>, MezError>(read)
            };
            tokio::pin!(read);
            if pending_delivery.is_some() {
                tokio::select! {
                    biased;
                    permit = sender.reserve() => {
                        let Ok(permit) = permit else {
                            return Ok(());
                        };
                        if let Some(wakeup) = pending_delivery.take() {
                            permit.send(Ok(wakeup));
                        }
                        continue;
                    }
                    read = &mut read => read?,
                }
            } else {
                read.await?
            }
        };
        let Some(read) = read else {
            continue;
        };
        if read == 0 {
            if !pending.is_empty() {
                return Err(MezError::invalid_state(
                    "Iroh event stream closed with an incomplete frame",
                ));
            }
            retain_latest_iroh_render_wakeup(
                &mut pending_delivery,
                IrohAttachRenderWakeup::new(AttachRenderAction::Disconnect, None),
            );
            if let Some(wakeup) = pending_delivery.take() {
                let _ = sender.send(Ok(wakeup)).await;
            }
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
            allow_pushed_render,
            &mut render_state,
        )?;
        if action.action != AttachRenderAction::None || action.pushed_snapshot.is_some() {
            retain_latest_iroh_render_wakeup(&mut pending_delivery, action);
        }
    }
}

/// Drains complete event frames under the immutable connection-local codec.
fn drain_negotiated_iroh_event_frames(
    pending: &mut Vec<u8>,
    compression: IrohCompressionPolicy,
    mut clipboard_assembler: Option<&mut IrohClipboardAssembler>,
    clipboard_sender: Option<&tokio::sync::watch::Sender<Option<String>>>,
    allow_pushed_render: bool,
    render_state: &mut IrohRetainedRenderState,
) -> Result<IrohAttachRenderWakeup> {
    let mut wakeup = IrohAttachRenderWakeup::new(AttachRenderAction::None, None);
    loop {
        let (decoded, consumed) = if compression.codec() == RuntimeIrohCompressionCodec::None {
            let Ok((body, consumed)) =
                decode_control_frame(pending, ATTACH_EVENT_STREAM_MAX_CONTENT_LENGTH)
            else {
                return Ok(wakeup);
            };
            wakeup = wakeup.combine(apply_negotiated_iroh_attach_frame(
                body.as_str(),
                clipboard_assembler.as_deref_mut(),
                clipboard_sender,
                allow_pushed_render,
                render_state,
            )?);
            pending.drain(..consumed);
            continue;
        } else {
            if pending.len() < IrohCompressionPolicy::envelope_header_length() {
                return Ok(wakeup);
            }
            let envelope_length = compression.declared_envelope_length(pending)?;
            if pending.len() < envelope_length {
                return Ok(wakeup);
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
        wakeup = wakeup.combine(apply_negotiated_iroh_attach_frame(
            body.as_str(),
            clipboard_assembler.as_deref_mut(),
            clipboard_sender,
            allow_pushed_render,
            render_state,
        )?);
        pending.drain(..consumed);
    }
}

/// Applies one decoded event or negotiated transient client-effect frame.
fn apply_negotiated_iroh_attach_frame(
    body: &str,
    clipboard_assembler: Option<&mut IrohClipboardAssembler>,
    clipboard_sender: Option<&tokio::sync::watch::Sender<Option<String>>>,
    allow_pushed_render: bool,
    render_state: &mut IrohRetainedRenderState,
) -> Result<IrohAttachRenderWakeup> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("Iroh event stream contained invalid JSON"))?;
    let method = value.get("method").and_then(serde_json::Value::as_str);
    if method == Some("render/snapshot") {
        if !allow_pushed_render {
            return Err(MezError::invalid_state(
                "legacy Iroh event stream contained a pushed render snapshot",
            ));
        }
        return parse_iroh_pushed_render_snapshot(&value, render_state);
    }
    if method == Some("render/delta") {
        if !allow_pushed_render {
            return Err(MezError::invalid_state(
                "legacy Iroh event stream contained a pushed render delta",
            ));
        }
        return parse_iroh_pushed_render_delta(&value, render_state);
    }
    if method.is_some_and(|method| method.starts_with("client/clipboard.")) {
        let Some(assembler) = clipboard_assembler else {
            return Ok(IrohAttachRenderWakeup::new(AttachRenderAction::None, None));
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
        return Ok(IrohAttachRenderWakeup::new(AttachRenderAction::None, None));
    }
    strict_iroh_attach_render_action(body)
}

/// Validates one complete authoritative v3 render snapshot atomically.
fn parse_iroh_pushed_render_snapshot(
    value: &serde_json::Value,
    render_state: &mut IrohRetainedRenderState,
) -> Result<IrohAttachRenderWakeup> {
    if value.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err(MezError::invalid_state(
            "Iroh render snapshot omitted JSON-RPC 2.0",
        ));
    }
    let params = value
        .get("params")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh render snapshot omitted params"))?;
    if params.get("kind").and_then(serde_json::Value::as_str) != Some("snapshot") {
        return Err(MezError::invalid_state(
            "Iroh render snapshot used an unsupported kind",
        ));
    }
    let revision = params
        .get("revision")
        .and_then(serde_json::Value::as_u64)
        .filter(|revision| *revision > render_state.revision)
        .ok_or_else(|| MezError::invalid_state("Iroh render snapshot revision is not monotonic"))?;
    let event_cutoff = params
        .get("event_cutoff")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_state("Iroh render snapshot omitted event cutoff"))?;
    let invalidate_output = params
        .get("invalidate_output")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            MezError::invalid_state("Iroh render snapshot omitted invalidation state")
        })?;
    let view = params
        .get("view")
        .filter(|view| view.is_object())
        .ok_or_else(|| MezError::invalid_state("Iroh render snapshot omitted its view"))?;
    if view.get("role").and_then(serde_json::Value::as_str)
        != Some(render_state.expected_role.as_str())
    {
        return Err(MezError::invalid_state(
            "Iroh render snapshot role did not match the negotiated client",
        ));
    }
    let line_count = view
        .get("lines")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| MezError::invalid_state("Iroh render snapshot lines are missing"))?;
    let style_row_count = view
        .get("line_style_spans")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| MezError::invalid_state("Iroh render snapshot styles are missing"))?;
    if line_count != style_row_count {
        return Err(MezError::invalid_state(
            "Iroh render snapshot lines and styles are misaligned",
        ));
    }
    let response = serde_json::json!({
        "result": {
            "view": view,
            "event_cutoff": event_cutoff,
        }
    })
    .to_string();
    let frame = super::responses::terminal_step_response_client_frame(&response)?
        .ok_or_else(|| MezError::invalid_state("Iroh render snapshot decoded no frame"))?;
    let retained_view = view.clone();
    render_state.revision = revision;
    render_state.view = Some(retained_view);
    Ok(IrohAttachRenderWakeup::pushed_snapshot(
        IrohPushedRenderSnapshot {
            revision,
            frame,
            invalidate_output,
        },
    ))
}

/// Validates and atomically reconstructs one whole-row v3 render delta.
fn parse_iroh_pushed_render_delta(
    value: &serde_json::Value,
    render_state: &mut IrohRetainedRenderState,
) -> Result<IrohAttachRenderWakeup> {
    if value.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err(MezError::invalid_state(
            "Iroh render delta omitted JSON-RPC 2.0",
        ));
    }
    let params = value
        .get("params")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh render delta omitted params"))?;
    if params.get("kind").and_then(serde_json::Value::as_str) != Some("delta") {
        return Err(MezError::invalid_state(
            "Iroh render delta used an unsupported kind",
        ));
    }
    let base_revision = params
        .get("base_revision")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_state("Iroh render delta omitted its base revision"))?;
    if base_revision != render_state.revision {
        return Err(MezError::invalid_state(
            "Iroh render delta base revision does not match retained state",
        ));
    }
    let revision = params
        .get("revision")
        .and_then(serde_json::Value::as_u64)
        .filter(|revision| *revision > base_revision)
        .ok_or_else(|| MezError::invalid_state("Iroh render delta revision is not monotonic"))?;
    let event_cutoff = params
        .get("event_cutoff")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_state("Iroh render delta omitted event cutoff"))?;
    if params
        .get("invalidate_output")
        .and_then(serde_json::Value::as_bool)
        != Some(false)
    {
        return Err(MezError::invalid_state(
            "Iroh render delta cannot invalidate retained output",
        ));
    }
    let line_count = params
        .get("line_count")
        .and_then(serde_json::Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| MezError::invalid_state("Iroh render delta line count is invalid"))?;
    let base_view = render_state
        .view
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh render delta has no retained base view"))?;
    let base_lines = base_view
        .get("lines")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| MezError::invalid_state("Iroh retained render lines are missing"))?;
    let base_styles = base_view
        .get("line_style_spans")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| MezError::invalid_state("Iroh retained render styles are missing"))?;
    if base_lines.len() != line_count || base_styles.len() != line_count {
        return Err(MezError::invalid_state(
            "Iroh render delta line count does not match retained state",
        ));
    }
    let metadata = params
        .get("view")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh render delta omitted view metadata"))?;
    if metadata.contains_key("lines") || metadata.contains_key("line_style_spans") {
        return Err(MezError::invalid_state(
            "Iroh render delta metadata contains row state",
        ));
    }
    if metadata.get("role").and_then(serde_json::Value::as_str)
        != Some(render_state.expected_role.as_str())
    {
        return Err(MezError::invalid_state(
            "Iroh render delta role did not match the negotiated client",
        ));
    }

    let mut lines = base_lines.clone();
    let mut styles = base_styles.clone();
    let rows = params
        .get("rows")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| MezError::invalid_state("Iroh render delta omitted changed rows"))?;
    let mut seen = std::collections::BTreeSet::new();
    for row in rows {
        let row = row
            .as_object()
            .ok_or_else(|| MezError::invalid_state("Iroh render delta row is not an object"))?;
        let index = row
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .filter(|index| *index < line_count)
            .ok_or_else(|| {
                MezError::invalid_state("Iroh render delta row index is out of range")
            })?;
        if !seen.insert(index) {
            return Err(MezError::invalid_state(
                "Iroh render delta contains a duplicate row index",
            ));
        }
        let line = row
            .get("line")
            .filter(|line| line.is_string())
            .ok_or_else(|| MezError::invalid_state("Iroh render delta row text is invalid"))?;
        let style_spans = row
            .get("style_spans")
            .filter(|spans| spans.is_array())
            .ok_or_else(|| MezError::invalid_state("Iroh render delta row styles are invalid"))?;
        super::responses::parse_terminal_style_span_row(style_spans)?;
        lines[index] = line.clone();
        styles[index] = style_spans.clone();
    }

    let mut candidate = metadata.clone();
    candidate.insert("lines".to_string(), serde_json::Value::Array(lines));
    candidate.insert(
        "line_style_spans".to_string(),
        serde_json::Value::Array(styles),
    );
    let candidate = serde_json::Value::Object(candidate);
    let response = serde_json::json!({
        "result": {
            "view": &candidate,
            "event_cutoff": event_cutoff,
        }
    })
    .to_string();
    let frame = super::responses::terminal_step_response_client_frame(&response)?
        .ok_or_else(|| MezError::invalid_state("Iroh render delta decoded no frame"))?;

    render_state.revision = revision;
    render_state.view = Some(candidate);
    Ok(IrohAttachRenderWakeup::pushed_snapshot(
        IrohPushedRenderSnapshot {
            revision,
            frame,
            invalidate_output: false,
        },
    ))
}

fn strict_iroh_attach_render_action(body: &str) -> Result<IrohAttachRenderWakeup> {
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
    let params = value
        .get("params")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh event stream omitted params"))?;
    let event_type = params
        .get("event_type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_state("Iroh event stream omitted event_type"))?;
    if method != event_type {
        return Err(MezError::invalid_state(
            "Iroh event stream method and event_type did not match",
        ));
    }
    Ok(IrohAttachRenderWakeup::new(
        attach_render_action_for_event_type(event_type),
        params.get("event_id").and_then(serde_json::Value::as_u64),
    ))
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
    static IROH_LATEST_RENDER_DECODED: Mutex<bool> = Mutex::new(false);

    fn record_iroh_client_clipboard_write(content: &str) -> bool {
        IROH_CLIENT_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .push(content.to_string());
        true
    }

    fn record_iroh_latest_render_decoded(content: &str) -> bool {
        if content == "decoded" {
            *IROH_LATEST_RENDER_DECODED.lock().unwrap() = true;
        }
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
            false,
            None,
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
            false,
            None,
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
        assert_eq!(action.action, AttachRenderAction::View);

        client_connection.close(VarInt::from_u32(0), b"test complete");
        task.await.unwrap();
        client.close().await;
        server.close().await;
    }

    /// Verifies a negotiated v3 receiver requires the exact v3 preface and
    /// continues to deliver legacy redraw notifications until pushed render
    /// frames are introduced behind that negotiated boundary.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_v3_event_receiver_accepts_preface_and_delivers_event() {
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
            3,
            false,
            None,
            None,
        );
        let mut stream = server_connection.open_uni().await.unwrap();
        stream
            .write_all(crate::runtime::MEZZANINE_IROH_EVENT_STREAM_V3_PREFACE)
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
        assert_eq!(action.action, AttachRenderAction::View);

        client_connection.close(VarInt::from_u32(0), b"test complete");
        task.await.unwrap();
        client.close().await;
        server.close().await;
    }

    /// Verifies a blocked v3 presentation consumer does not make the decoder
    /// queue every intermediate viewport. Ordered revisions must continue to
    /// decode, skipped invalidation must remain sticky, and only the newest
    /// complete frame may follow the already-visible channel entry.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_v3_event_receiver_retains_latest_render_while_consumer_is_blocked() {
        *IROH_LATEST_RENDER_DECODED.lock().unwrap() = false;
        let (server, client, server_connection, client_connection) =
            connected_iroh_event_pair().await;
        let clipboard = crate::host::terminal::HostClipboard::new(
            record_iroh_latest_render_decoded,
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
            3,
            true,
            Some("primary".to_string()),
            Some(clipboard),
        );
        let mut stream = server_connection.open_uni().await.unwrap();
        stream
            .write_all(crate::runtime::MEZZANINE_IROH_EVENT_STREAM_V3_PREFACE)
            .await
            .unwrap();

        let snapshot = |revision: u64, invalidate_output: bool| {
            let viewport = format!(
                "viewport {revision} {}",
                "x".repeat(ATTACH_EVENT_STREAM_READ_BUFFER_BYTES)
            );
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "render/snapshot",
                "params": {
                    "kind": "snapshot",
                    "revision": revision,
                    "event_cutoff": revision,
                    "invalidate_output": invalidate_output,
                    "view": {
                        "role": "primary",
                        "lines": [viewport],
                        "line_style_spans": [[]],
                        "cursor": {"row": 0, "column": 0, "visible": false},
                        "output_modes": {}
                    }
                }
            })
            .to_string()
        };
        stream
            .write_all(&crate::control::encode_control_body(&snapshot(1, false)))
            .await
            .unwrap();
        stream.flush().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while receiver.len() != IROH_RENDER_WAKEUP_CHANNEL_CAPACITY {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the first render must occupy the presentation slot");

        for revision in 2..=12 {
            stream
                .write_all(&crate::control::encode_control_body(&snapshot(
                    revision,
                    revision == 5,
                )))
                .await
                .unwrap();
        }
        for body in [
            r#"{"jsonrpc":"2.0","method":"client/clipboard.begin","params":{"sequence":1,"total_bytes":7,"chunks":1}}"#,
            r#"{"jsonrpc":"2.0","method":"client/clipboard.chunk","params":{"sequence":1,"index":0,"data_base64":"ZGVjb2RlZA=="}}"#,
            r#"{"jsonrpc":"2.0","method":"client/clipboard.commit","params":{"sequence":1}}"#,
        ] {
            stream
                .write_all(&crate::control::encode_control_body(body))
                .await
                .unwrap();
        }
        stream.flush().await.unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if *IROH_LATEST_RENDER_DECODED.lock().unwrap() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ordered decoding must continue while presentation is blocked");
        let first = receiver.recv().await.unwrap().unwrap();
        assert_eq!(first.pushed_snapshot.unwrap().revision, 1);
        let latest = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .pushed_snapshot
            .expect("latest decoded render must be delivered");
        assert_eq!(latest.revision, 12);
        assert!(latest.frame.lines[0].starts_with("viewport 12 "));
        assert!(latest.frame.lines[0].len() > ATTACH_EVENT_STREAM_READ_BUFFER_BYTES);
        assert!(latest.invalidate_output);
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert!(*IROH_LATEST_RENDER_DECODED.lock().unwrap());

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
            false,
            None,
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
        assert_eq!(action.action, AttachRenderAction::View);
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

    /// Verifies v3 snapshots decode atomically, retain the authoritative
    /// cutoff and invalidation state, and reject non-monotonic revisions.
    #[test]
    fn pushed_render_snapshots_require_monotonic_complete_primary_frames() {
        let snapshot = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "render/snapshot",
            "params": {
                "kind": "snapshot",
                "revision": 1,
                "event_cutoff": 9,
                "invalidate_output": true,
                "view": {
                    "role": "primary",
                    "lines": ["pushed"],
                    "line_style_spans": [[]],
                    "cursor": {"row": 0, "column": 6, "visible": true},
                    "output_modes": {}
                }
            }
        });
        let mut render_state = IrohRetainedRenderState::default();
        let decoded = parse_iroh_pushed_render_snapshot(&snapshot, &mut render_state).unwrap();
        let pushed = decoded.pushed_snapshot.expect("snapshot should decode");

        assert_eq!(render_state.revision, 1);
        assert_eq!(pushed.revision, 1);
        assert_eq!(pushed.frame.lines, ["pushed"]);
        assert_eq!(pushed.frame.event_cutoff, Some(9));
        assert!(pushed.invalidate_output);

        let error = parse_iroh_pushed_render_snapshot(&snapshot, &mut render_state)
            .expect_err("duplicate revisions must be rejected atomically");
        assert!(error.message().contains("not monotonic"), "{error:?}");
        assert_eq!(render_state.revision, 1);
    }

    /// Verifies a whole-row delta reconstructs the authoritative complete
    /// frame and malformed rows leave the retained base entirely unchanged.
    #[test]
    fn pushed_render_row_delta_reconstructs_atomically() {
        let snapshot = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "render/snapshot",
            "params": {
                "kind": "snapshot",
                "revision": 1,
                "event_cutoff": 4,
                "invalidate_output": true,
                "view": {
                    "role": "primary",
                    "authoritative_size": {"columns": 80, "rows": 24},
                    "client_size": {"columns": 80, "rows": 24},
                    "lines": ["stable", "before", "tail"],
                    "line_style_spans": [[], [], []],
                    "cursor": {"row": 1, "column": 6, "visible": true},
                    "output_modes": {"application_keypad": false}
                }
            }
        });
        let mut render_state = IrohRetainedRenderState::default();
        parse_iroh_pushed_render_snapshot(&snapshot, &mut render_state).unwrap();

        let delta = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "render/delta",
            "params": {
                "kind": "delta",
                "base_revision": 1,
                "revision": 2,
                "event_cutoff": 7,
                "invalidate_output": false,
                "line_count": 3,
                "view": {
                    "role": "primary",
                    "authoritative_size": {"columns": 80, "rows": 24},
                    "client_size": {"columns": 80, "rows": 24},
                    "cursor": {"row": 1, "column": 5, "visible": true},
                    "output_modes": {"application_keypad": false}
                },
                "rows": [
                    {"index": 1, "line": "after", "style_spans": []}
                ]
            }
        });
        let retained_before_mismatch = render_state.clone();
        let mut mismatched = delta.clone();
        mismatched["params"]["base_revision"] = serde_json::Value::from(0);
        let mismatch = parse_iroh_pushed_render_delta(&mismatched, &mut render_state)
            .expect_err("a stale delta base must be rejected");
        assert!(mismatch.message().contains("base revision"), "{mismatch:?}");
        assert_eq!(render_state.revision, retained_before_mismatch.revision);
        assert_eq!(render_state.view, retained_before_mismatch.view);

        let decoded = parse_iroh_pushed_render_delta(&delta, &mut render_state).unwrap();
        let pushed = decoded.pushed_snapshot.expect("delta should reconstruct");

        assert_eq!(pushed.revision, 2);
        assert_eq!(pushed.frame.lines, ["stable", "after", "tail"]);
        assert_eq!(pushed.frame.event_cutoff, Some(7));
        assert_eq!(render_state.revision, 2);

        let retained_before_error = render_state.clone();
        let invalid = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "render/delta",
            "params": {
                "kind": "delta",
                "base_revision": 2,
                "revision": 3,
                "event_cutoff": 8,
                "invalidate_output": false,
                "line_count": 3,
                "view": {"role": "primary"},
                "rows": [
                    {"index": 0, "line": "first", "style_spans": []},
                    {"index": 0, "line": "duplicate", "style_spans": []}
                ]
            }
        });
        let error = parse_iroh_pushed_render_delta(&invalid, &mut render_state)
            .expect_err("duplicate row indexes must be rejected atomically");

        assert!(error.message().contains("duplicate row index"), "{error:?}");
        assert_eq!(render_state.revision, retained_before_error.revision);
        assert_eq!(render_state.view, retained_before_error.view);

        let mut out_of_range = invalid;
        out_of_range["params"]["rows"] = serde_json::json!([
            {"index": 3, "line": "outside", "style_spans": []}
        ]);
        let error = parse_iroh_pushed_render_delta(&out_of_range, &mut render_state)
            .expect_err("out-of-range rows must be rejected atomically");
        assert!(error.message().contains("out of range"), "{error:?}");
        assert_eq!(render_state.revision, retained_before_error.revision);
        assert_eq!(render_state.view, retained_before_error.view);
    }

    /// Verifies identity, Zstandard, and LZ4 envelopes each carry one
    /// independently decodable pushed snapshot, while legacy ownership rejects
    /// the same frame rather than silently treating it as a redraw wakeup.
    #[test]
    fn pushed_render_snapshots_decode_across_negotiated_compression_modes() {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "render/snapshot",
            "params": {
                "kind": "snapshot",
                "revision": 1,
                "event_cutoff": 4,
                "invalidate_output": false,
                "view": {
                    "role": "primary",
                    "lines": ["compressed pushed snapshot"],
                    "line_style_spans": [[]],
                    "cursor": {"row": 0, "column": 0, "visible": false},
                    "output_modes": {}
                }
            }
        })
        .to_string();
        let framed = encode_control_body(&body);

        for codec in [
            RuntimeIrohCompressionCodec::None,
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
            let mut pending = if codec == RuntimeIrohCompressionCodec::None {
                framed.clone()
            } else {
                compression
                    .encode_frame(&framed, crate::runtime::IrohFrameCompressionMode::Eligible)
                    .unwrap()
                    .as_bytes()
                    .to_vec()
            };
            let mut render_state = IrohRetainedRenderState::default();
            let decoded = drain_negotiated_iroh_event_frames(
                &mut pending,
                compression,
                None,
                None,
                true,
                &mut render_state,
            )
            .unwrap();
            let snapshot = decoded.pushed_snapshot.expect("snapshot should decode");
            assert_eq!(snapshot.frame.lines, ["compressed pushed snapshot"]);
            assert_eq!(render_state.revision, 1);
            assert!(pending.is_empty());

            let delta = encode_control_body(
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "render/delta",
                    "params": {
                        "kind": "delta",
                        "base_revision": 1,
                        "revision": 2,
                        "event_cutoff": 5,
                        "invalidate_output": false,
                        "line_count": 1,
                        "view": {
                            "role": "primary",
                            "cursor": {"row": 0, "column": 7, "visible": true},
                            "output_modes": {}
                        },
                        "rows": [
                            {"index": 0, "line": "delta through codec", "style_spans": []}
                        ]
                    }
                })
                .to_string(),
            );
            let mut pending = if codec == RuntimeIrohCompressionCodec::None {
                delta
            } else {
                compression
                    .encode_frame(&delta, crate::runtime::IrohFrameCompressionMode::Eligible)
                    .unwrap()
                    .as_bytes()
                    .to_vec()
            };
            let decoded = drain_negotiated_iroh_event_frames(
                &mut pending,
                compression,
                None,
                None,
                true,
                &mut render_state,
            )
            .unwrap();
            let delta = decoded.pushed_snapshot.expect("delta should decode");
            assert_eq!(delta.frame.lines, ["delta through codec"]);
            assert_eq!(render_state.revision, 2);
            assert!(pending.is_empty());
        }

        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        let mut render_state = IrohRetainedRenderState::default();
        let error = apply_negotiated_iroh_attach_frame(
            &value.to_string(),
            None,
            None,
            false,
            &mut render_state,
        )
        .expect_err("legacy render ownership must reject pushed snapshots");
        assert!(
            error.message().contains("legacy Iroh event stream"),
            "{error:?}"
        );
        assert_eq!(render_state.revision, 0);
    }

    /// Verifies ready repeated-key input retains a simultaneously queued Iroh
    /// redraw instead of starving visible updates until terminal input pauses.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_input_poll_preserves_ready_render_action_when_input_wins() {
        let mut terminal_io = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
        terminal_io.push_input(vec![0x7f]);
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        sender
            .try_send(Ok(IrohAttachRenderWakeup::new(
                AttachRenderAction::View,
                Some(8),
            )))
            .unwrap();

        let input = read_attached_client_input_or_iroh_event(
            &mut terminal_io,
            &mut receiver,
            4096,
            None,
            tokio::time::Instant::now() + std::time::Duration::from_secs(60),
            Some(7),
        )
        .await
        .unwrap();

        assert_eq!(input.bytes, vec![0x7f]);
        assert!(!input.eof);
        assert_eq!(input.render_action, AttachRenderAction::View);
    }

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
            None,
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
        sender
            .try_send(Ok(IrohAttachRenderWakeup::new(
                AttachRenderAction::View,
                Some(7),
            )))
            .unwrap();
        sender
            .try_send(Ok(IrohAttachRenderWakeup::new(
                AttachRenderAction::View,
                Some(9),
            )))
            .unwrap();

        assert_eq!(
            coalesce_ready_iroh_render_actions(
                &mut receiver,
                IrohAttachRenderWakeup::new(AttachRenderAction::View, Some(6)),
                Some(7),
            )
            .unwrap()
            .action,
            AttachRenderAction::View
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

        let error = coalesce_ready_iroh_render_actions(
            &mut receiver,
            IrohAttachRenderWakeup::new(AttachRenderAction::View, Some(1)),
            Some(1),
        )
        .expect_err("queued Iroh event failures must not be discarded");
        assert!(error.message().contains("queued Iroh event failure"));
    }

    /// Verifies redraw wakeups covered by the authoritative view cutoff do not
    /// trigger another RTT, while invalidating actions remain conservative.
    #[test]
    fn authoritative_view_cutoff_discards_only_covered_ordinary_redraws() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        sender
            .try_send(Ok(IrohAttachRenderWakeup::new(
                AttachRenderAction::View,
                Some(12),
            )))
            .unwrap();
        assert_eq!(
            coalesce_ready_iroh_render_actions(
                &mut receiver,
                IrohAttachRenderWakeup::new(AttachRenderAction::View, Some(11)),
                Some(12),
            )
            .unwrap()
            .action,
            AttachRenderAction::None
        );

        let (_sender, mut receiver) = tokio::sync::mpsc::channel(1);
        assert_eq!(
            coalesce_ready_iroh_render_actions(
                &mut receiver,
                IrohAttachRenderWakeup::new(AttachRenderAction::InvalidateAndView, Some(12)),
                Some(12),
            )
            .unwrap()
            .action,
            AttachRenderAction::InvalidateAndView
        );
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
            let mut render_state = IrohRetainedRenderState::default();
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
                drain_negotiated_iroh_event_frames(
                    &mut pending,
                    compression,
                    None,
                    None,
                    false,
                    &mut render_state,
                )
                .unwrap()
                .action,
                AttachRenderAction::None,
            );
            pending.extend_from_slice(&encoded.as_bytes()[split..]);
            assert_eq!(
                drain_negotiated_iroh_event_frames(
                    &mut pending,
                    compression,
                    None,
                    None,
                    false,
                    &mut render_state,
                )
                .unwrap()
                .action,
                AttachRenderAction::View,
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
        assert_eq!(valid.action, AttachRenderAction::View);

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
