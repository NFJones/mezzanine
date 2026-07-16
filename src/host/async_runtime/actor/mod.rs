//! Async Runtime Actor implementation.
//!
//! This module owns the async runtime actor boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.
use super::{
    AgentId, AgentProviderEvent, Arc, AsyncControlInputResult, AsyncHookEvent, AsyncMessageFanout,
    AsyncMessageInputResult, AsyncRenderedClientFlush, AsyncRenderedClientFrame,
    AsyncRuntimeActorConfig, AsyncRuntimeActorExit, AsyncRuntimeRequest, AsyncRuntimeSessionActor,
    AsyncRuntimeSessionHandle, AttachedClientStepApplication, AttachedTerminalClientStepPlan,
    AttachedTerminalOutputModes, ClientEvent, ClientId, ClientState, ClientStatusLine,
    ClientViewRole, ControlConnectionState, DEFAULT_ASYNC_IDLE_CLEANUP_INTERVAL, DeliveryCursor,
    FanoutBatch, MessageConnection, MezError, Notify, PaneEvent, PersistenceEvent,
    RenderInvalidationReason, Result, RuntimeAgentProviderDispatch, RuntimeEvent,
    RuntimeEventBatch, RuntimeEventConnectionTable, RuntimeEventIngressReport, RuntimeEventWakeup,
    RuntimeLifecycleState, RuntimeSessionService, RuntimeSideEffect,
    RuntimeSnapshotControlAsyncOutcome, RuntimeSnapshotControlAsyncWork,
    RuntimeSnapshotControlAsyncWorkKind, RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition,
    ShutdownEvent, Size, TerminalClientLoopConfig, TimerEvent, VecDeque,
    compose_client_presentation_with_styles, delivery_batch_json, encode_mmp_body, mpsc, oneshot,
    watch,
};
#[cfg(test)]
use super::{RenderedClientView, RuntimeAgentProviderTask};
use crate::control::{decode_control_frame, encode_control_body};
use crate::integrations::agent::provider::{
    provider_error_retry_class_from_parts, provider_event_error_from_parts,
    provider_event_error_kind,
};
use crate::runtime::PaneResizeUpdate;
#[cfg(test)]
use crate::runtime::coalesce_config_persistence_effects;
use mez_agent::{DEFAULT_PROVIDER_TIMEOUT_MS, ProviderErrorRetryClass};

// Serialized runtime actor and handle implementation.

/// Defines the DEFAULT SHELL RECOVERY INTERVAL MS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEFAULT_SHELL_RECOVERY_INTERVAL_MS: u64 = 250;
/// Grace added to provider worker claim leases beyond the provider timeout.
///
/// The async runtime watchdog must never expire a legitimate provider request
/// before the provider transport has had a chance to return its own timeout or
/// failure. A small grace window covers timer scheduling and event-ingress
/// latency without leaving abandoned worker claims unbounded.
const DEFAULT_PROVIDER_CLAIM_TIMEOUT_GRACE_MS: u64 = 30_000;
/// Provider worker claim lease before the runtime fails a still-running turn.
///
/// This lease follows the provider transport timeout instead of using an
/// independent short watchdog, preventing long-running model requests from
/// being failed by the actor while the HTTP provider call is still valid.
const DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS: u64 =
    DEFAULT_PROVIDER_TIMEOUT_MS + DEFAULT_PROVIDER_CLAIM_TIMEOUT_GRACE_MS;
/// Defines the DEFAULT PANE PIPE HEALTH DELAY MS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEFAULT_PANE_PIPE_HEALTH_DELAY_MS: u64 = 50;

mod coalesce;
mod construction;
mod drain;
mod events;
mod handle;
mod queue;
mod requests;

#[cfg(test)]
mod tests;
