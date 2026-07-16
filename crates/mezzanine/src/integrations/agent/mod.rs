//! Product adapters for the provider-independent agent harness.
//!
//! `mez-agent` owns canonical contracts and deterministic harness behavior.
//! This module exposes explicit adapter modules for embedded prompt assets,
//! product shell transactions, permissions, concrete provider transports,
//! transcript persistence, network access, and runtime action execution. The
//! module root contains only private sibling wiring; product consumers import
//! the adapter that owns each concrete integration.

use std::collections::BTreeMap;
#[cfg(test)]
use std::path::Path;

use secrecy::{ExposeSecret, SecretString};

use crate::error::{MezError, Result};
use mez_agent::{McpPromptTool, ModelProfile, ModelResponse};

/// Exposes the actions module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod actions;
/// Exposes the context module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod context;
/// Exposes the network module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod network;
/// Exposes the prompt module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod prompt;
/// Exposes the provider module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod provider;
/// Exposes the slash module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod slash;
/// Exposes product-friendly subagent presentation names.
pub mod subagent;
use context::assemble_model_request;
#[cfg(test)]
use mez_agent::TranscriptEntry;
use mez_agent::local_action_plan;
use mez_agent::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentContext, AgentLogLevel,
    AgentShellStore, AgentShellVisibility, AgentTurnLedger, AgentTurnRecord, AgentTurnState,
    AllowedActionSet, ContextSourceKind, MaapBatch, McpExecutionRequest, ModelInteractionKind,
    ModelMessage, ModelMessageRole, ModelRequest, ProviderHttpRequest, ProviderHttpResponse,
    TranscriptPersistence, agent_shell_help_display, agent_shell_mcp_display,
    agent_shell_permissions_display, agent_shell_status_display,
};
#[cfg(test)]
use mez_agent::{
    DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS, EnvironmentSignature, MarkerToken, ToolDiscoveryCache,
    ToolInventory, tool_discovery_script,
};
#[cfg(test)]
use mez_agent::{ShellTransaction, ShellTransactionOutputTransport};
use provider::provider_error_retry_class;
use provider::{AsyncModelProvider, AsyncProviderHttpTransport};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
