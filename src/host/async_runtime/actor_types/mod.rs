//! Async Runtime Actor Types implementation.
//!
//! This module owns the async runtime actor types boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::AsyncRuntimeActorMetrics;
use super::{
    AgentId, AsRawFd, AsyncControlInputResult, AsyncMessageFanout, AsyncMessageInputResult,
    AsyncRuntimeControlConnectionConfig, AsyncRuntimeMessageConnectionConfig,
    AsyncRuntimeSessionHandle, AsyncWriteExt, AttachedClientStepApplication,
    AttachedTerminalClientStepPlan, AttachedTerminalFdReadiness, AttachedTerminalOutputModes,
    ClientEvent, ClientId, ClientStatusLine, ClientViewRole, ControlConnectionState,
    DeliveryCursor, FanoutBatch, Framed, JoinSet, MessageConnection, MezError, PaneProcess,
    ProtocolFrameCodec, RenderedClientView, Result, RuntimeAgentCompactionDispatch,
    RuntimeAgentProviderDispatch, RuntimeAgentProviderTask, RuntimeAgentRememberDispatch,
    RuntimeEvent, RuntimeEventBatch, RuntimeEventConnectionTable, RuntimeEventIngressReport,
    RuntimeEventWakeup, RuntimeLifecycleState, RuntimeSideEffect,
    RuntimeSnapshotControlAsyncOutcome, RuntimeSnapshotControlAsyncWork, Size, StreamExt,
    TerminalClientLoopConfig, TerminalStyleSpan, UnixListener, UnixStream,
    authorize_unix_peer_raw_fd, encode_frame, oneshot, plan_attached_terminal_client_step,
};
use crate::runtime::PaneResizeUpdate;
use crate::storage::snapshot::SnapshotRepository;

mod attached;
mod control;
mod message;
mod render;
mod request;

pub use attached::{
    AsyncAttachedTerminalStepRequest, plan_and_apply_async_attached_terminal_client_step,
    plan_async_attached_terminal_client_step,
};
pub use control::{
    serve_async_runtime_control_connection, serve_async_runtime_control_connection_loop,
    serve_async_runtime_control_connection_loop_with_snapshots,
    serve_async_runtime_control_connection_with_snapshots, serve_async_runtime_control_listener,
    serve_async_runtime_control_listener_with_snapshots,
};
pub use message::{
    serve_async_runtime_message_connection, serve_async_runtime_message_connection_loop,
    serve_async_runtime_message_listener, serve_async_runtime_message_listener_concurrent,
};
pub use render::{AsyncRenderedClientFlush, AsyncRenderedClientFrame};
pub(super) use request::AsyncRuntimeRequest;
