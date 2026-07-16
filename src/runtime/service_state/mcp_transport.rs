//! Runtime-owned MCP transport state and retry reporting.

use super::{
    AuthStore, HookEvent, HookFailureKind, McpServerStatus, McpStartupPlan, McpStdioConnection,
    McpToolCallPlan, McpToolCallResponse, MezError, Result, execute_streamable_http_exchange,
    mcp_tools_call_operation,
};
use crate::error::MezErrorKind;
use secrecy::ExposeSecret;
use std::collections::BTreeMap;

/// Carries Runtime Mcp Transport Set state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Default)]
pub(in crate::runtime) struct RuntimeMcpTransportSet {
    /// Stores the transports value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) transports: BTreeMap<String, RuntimeMcpTransport>,
}

/// Carries Runtime Mcp Retry Report state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimeMcpRetryReport {
    /// Stores the server id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) server_id: String,
    /// Stores the previous status value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) previous_status: McpServerStatus,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) status: McpServerStatus,
    /// Stores the retryable before retry value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) retryable_before_retry: bool,
    /// Stores the rediscovered value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) rediscovered: bool,
    /// Stores the tools value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) tools: usize,
    /// Stores the reason value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) reason: Option<String>,
}

impl RuntimeMcpRetryReport {
    /// Runs the previous status name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn previous_status_name(&self) -> &'static str {
        runtime_mcp_status_name(self.previous_status)
    }

    /// Runs the status name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn status_name(&self) -> &'static str {
        runtime_mcp_status_name(self.status)
    }
}

/// Carries Runtime Mcp Transport state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(in crate::runtime) enum RuntimeMcpTransport {
    /// Represents the Stdio case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Stdio(McpStdioConnection),
    /// Represents the Streamable Http case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    StreamableHttp(RuntimeHttpMcpTransportState),
}

/// Carries Runtime Http Mcp Transport State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimeHttpMcpTransportState {
    /// Stores the startup plan value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) startup_plan: McpStartupPlan,
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) session_id: Option<String>,
    /// Stores the next request id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) next_request_id: u64,
}

/// Carries Runtime Hook Pipeline Block state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimeHookPipelineBlock {
    /// Stores the hook id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) hook_id: String,
    /// Stores the event value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) event: HookEvent,
    /// Stores the failure kind value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) failure_kind: HookFailureKind,
    /// Stores the message value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) message: String,
}

impl RuntimeMcpTransportSet {
    /// Runs the clear operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn clear(&mut self) {
        self.transports.clear();
    }

    /// Runs the clear counted operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn clear_counted(&mut self) -> usize {
        let count = self.transports.len();
        self.clear();
        count
    }

    /// Runs the insert stdio operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn insert_stdio(
        &mut self,
        server_id: String,
        connection: McpStdioConnection,
    ) {
        self.transports
            .insert(server_id, RuntimeMcpTransport::Stdio(connection));
    }

    /// Runs the insert streamable http operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn insert_streamable_http(
        &mut self,
        server_id: String,
        state: RuntimeHttpMcpTransportState,
    ) {
        self.transports
            .insert(server_id, RuntimeMcpTransport::StreamableHttp(state));
    }

    /// Runs the remove operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn remove(&mut self, server_id: &str) {
        self.transports.remove(server_id);
    }

    /// Runs the call tool operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn call_tool(
        &mut self,
        plan: &McpToolCallPlan,
        _environment: &BTreeMap<String, String>,
    ) -> Result<McpToolCallResponse> {
        Err(MezError::invalid_state(format!(
            "MCP server `{}` requires the async runtime tool execution path",
            plan.server_id
        )))
    }

    /// Runs the call tool async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) async fn call_tool_async(
        &mut self,
        plan: &McpToolCallPlan,
        environment: &BTreeMap<String, String>,
        auth_store: Option<&AuthStore>,
    ) -> Result<McpToolCallResponse> {
        let transport = self.transports.get_mut(&plan.server_id).ok_or_else(|| {
            MezError::invalid_state(format!(
                "MCP server `{}` has no owned runtime transport",
                plan.server_id
            ))
        })?;
        match transport {
            RuntimeMcpTransport::Stdio(connection) => connection.call_tool(plan).await,
            RuntimeMcpTransport::StreamableHttp(state) => {
                let request_id = state.next_request_id;
                state.next_request_id = state.next_request_id.saturating_add(1);
                let operation = mcp_tools_call_operation(request_id, plan)?;
                let oauth_token = match &state.startup_plan.transport {
                    mez_agent::mcp::McpStartupTransportPlan::StreamableHttp {
                        bearer_token_env,
                        ..
                    } if bearer_token_env.is_none() => {
                        auth_store.and_then(|store| store.mcp_access_token(&plan.server_id).ok())
                    }
                    _ => None,
                };
                let response = match execute_streamable_http_exchange(
                    &state.startup_plan,
                    environment,
                    operation.request_body(),
                    Some(operation.request_id()),
                    operation.timeout_ms(),
                    state.session_id.as_deref(),
                    oauth_token.as_ref().map(ExposeSecret::expose_secret),
                )
                .await
                {
                    Ok(response) => response,
                    Err(error)
                        if error.kind() == MezErrorKind::Forbidden
                            && oauth_token.is_some()
                            && auth_store.is_some_and(|store| {
                                store
                                    .mcp_refresh_token(&plan.server_id)
                                    .ok()
                                    .flatten()
                                    .is_some()
                            }) =>
                    {
                        let auth_store = auth_store.ok_or_else(|| {
                            MezError::invalid_state("MCP OAuth refresh requires an auth store")
                        })?;
                        auth_store
                            .refresh_mcp_oauth_credential_for_server_async(&plan.server_id)
                            .await?;
                        let refreshed_token = auth_store.mcp_access_token(&plan.server_id)?;
                        execute_streamable_http_exchange(
                            &state.startup_plan,
                            environment,
                            operation.request_body(),
                            Some(operation.request_id()),
                            operation.timeout_ms(),
                            state.session_id.as_deref(),
                            Some(refreshed_token.expose_secret()),
                        )
                        .await?
                    }
                    Err(error) => return Err(error),
                };
                if response.session_id.is_some() {
                    state.session_id = response.session_id.clone();
                }
                Ok(operation.parse_response(&response.protocol_body)?)
            }
        }
    }
}

/// Runs the runtime mcp status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_mcp_status_name(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Configured => "configured",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

impl std::fmt::Debug for RuntimeMcpTransportSet {
    /// Runs the fmt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeMcpTransportSet")
            .field("server_count", &self.transports.len())
            .finish()
    }
}
