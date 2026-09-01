//! Async Runtime Actor Types implementation.
//!
//! This module owns the async runtime actor types boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::AsyncRuntimeActorMetrics;
#[cfg(test)]
use super::AttachedTerminalFdReadiness;
use super::{
    AgentId, AsRawFd, AsyncControlInputResult, AsyncMessageFanout, AsyncMessageInputResult,
    AsyncRuntimeControlConnectionConfig, AsyncRuntimeMessageConnectionConfig,
    AsyncRuntimeSessionHandle, AsyncWriteExt, AttachedClientStepApplication,
    AttachedTerminalClientStepPlan, AttachedTerminalOutputModes, AuthenticatedPeer, ClientEvent,
    ClientId, ClientStatusLine, ClientViewRole, ControlConnectionState, DeliveryCursor,
    FanoutBatch, Framed, JoinSet, MessageConnection, MezError, PaneProcess, ProtocolFrameCodec,
    RenderedClientView, Result, RuntimeAgentCompactionDispatch, RuntimeAgentProviderDispatch,
    RuntimeAgentProviderTask, RuntimeAgentRememberDispatch, RuntimeApprovedExternalActionDispatch,
    RuntimeApprovedExternalActionOutcome, RuntimeEvent, RuntimeEventBatch,
    RuntimeEventIngressReport, RuntimeEventWakeup, RuntimeLifecycleState,
    RuntimeProviderInfoRefreshOutcome, RuntimeSideEffect, RuntimeSnapshotControlAsyncOutcome,
    RuntimeSnapshotControlAsyncWork, Size, StreamExt, TerminalClientLoopConfig, TerminalStyleSpan,
    UnixListener, UnixStream, authenticated_unix_peer_uid, encode_frame, oneshot,
};
use crate::runtime::PaneResizeUpdate;
use crate::storage::snapshot::SnapshotRepository;

#[cfg(test)]
mod attached;
mod control;
mod message;
mod render;
mod request;

#[cfg(test)]
pub use attached::{
    AsyncAttachedTerminalStepRequest, plan_and_apply_async_attached_terminal_client_step,
    plan_async_attached_terminal_client_step,
};
#[cfg(test)]
pub use control::{
    serve_async_runtime_control_connection, serve_async_runtime_control_connection_loop,
    serve_async_runtime_control_listener,
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots,
};
pub use control::{
    serve_async_runtime_control_listener_with_snapshots,
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots_hooks_and_cancellation,
};
pub use message::serve_async_runtime_message_listener_concurrent;
#[cfg(test)]
pub use message::{
    serve_async_runtime_message_connection, serve_async_runtime_message_connection_loop,
    serve_async_runtime_message_listener,
};
pub(in crate::host::async_runtime) use render::{
    AsyncClientRenderToken, AsyncTerminalClientConfigInput,
};
pub use render::{
    AsyncIrohRenderSnapshot, AsyncRenderedClientFlush, AsyncRenderedClientFrame,
    AsyncTerminalClientConfigSnapshot,
};
pub(super) use request::{AsyncRuntimeRequest, AsyncRuntimeRequestEnvelope};
