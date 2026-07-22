//! Async Runtime Client implementation.
//!
//! This module owns the async runtime client boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AgentCompactionEvent, AgentProviderEvent, AgentRememberEvent, AgentTurnLedger, AgentTurnRunner,
    AsyncAgentProviderPollReport, AsyncAgentProviderServiceConfig, AsyncAttachedTerminalIo,
    AsyncAttachedTerminalLoopRequest, AsyncRuntimeSessionHandle, AsyncTerminalIoFuture,
    AsyncTerminalOutputWriteReport, AttachedTerminalClientLoopReport, AttachedTerminalFdReadiness,
    AttachedTerminalFdRole, AttachedTerminalOutputModes, ClientId, ClientStatusLine,
    ClientViewRole, DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT,
    DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES, MezError, MouseAction, Result,
    RuntimeAgentCompactionDispatch, RuntimeAgentProviderDispatch,
    RuntimeAgentProviderDispatchProvider, RuntimeAgentRememberDispatch, RuntimeEvent,
    RuntimeEventBatch, RuntimeLifecycleState, RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind,
    Size, TerminalClientLoopAction, empty_attached_terminal_loop_report,
    is_terminal_runtime_lifecycle_state, merge_attached_terminal_loop_report,
    run_async_attached_terminal_client_loop, sleep,
};
#[cfg(test)]
use super::{AsyncRuntimeService, AsyncRuntimeServiceExit};
use crate::error::MezErrorKind;
use crate::host::async_runtime::RenderInvalidationReason;
use crate::host::terminal::TerminalFdInterest;
use crate::integrations::agent::network::execute_network_action_with_transport_async;
use crate::integrations::agent::provider::{
    AsyncModelProvider, ReqwestProviderHttpTransport, provider_error_retry_class,
};
use crate::runtime::runtime_execute_auto_sizing_with_async_provider;
use mez_agent::AgentTurnRecord;
use mez_agent::{
    ActionStatus, AgentActionPayload, AgentTurnExecution, AgentTurnState, ContextSourceKind,
    ModelMessage, ModelMessageRole, ModelProfile, ModelRequest, ModelResponse,
    ProviderErrorRetryClass,
};
use mez_core::ids::AgentId;
use mez_terminal::TerminalStyleSpan;
use tokio::sync::watch;
use tokio::task::JoinSet;

mod provider_service;
mod terminal_service;

pub use provider_service::run_async_agent_provider_service;
#[cfg(test)]
pub(in crate::host::async_runtime) use provider_service::{
    classify_provider_monitor_liveness, execute_provider_worker_network_actions,
};
#[cfg(test)]
pub use terminal_service::build_async_attached_terminal_client_service;
pub use terminal_service::{
    AsyncAttachedTerminalClientServiceConfig, run_async_attached_terminal_client_service,
};
