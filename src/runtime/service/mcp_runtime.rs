//! MCP registry access, retry, discovery, and initialization event handling.

use super::mcp_helpers::*;
use super::*;

impl RuntimeSessionService {
    /// Runs the mcp registry operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mcp_registry(&self) -> &McpRegistry {
        self.integration.mcp_registry()
    }

    /// Runs the mcp registry mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mcp_registry_mut(&mut self) -> &mut McpRegistry {
        self.integration.mcp_registry_mut()
    }

    /// Clears all runtime-owned MCP transports and returns the number dropped.
    pub(crate) fn clear_runtime_mcp_transports(&mut self) -> usize {
        self.integration.mcp_transports_mut().clear_counted()
    }

    /// Runs the retry runtime mcp server operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn retry_runtime_mcp_server(
        &mut self,
        server_id: &str,
    ) -> Result<RuntimeMcpRetryReport> {
        let previous = self
            .integration
            .mcp_registry()
            .list_servers()
            .into_iter()
            .find(|server| server.configured.id == server_id)
            .cloned()
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "MCP server not found")
            })?;
        if !previous.configured.enabled {
            return Err(MezError::forbidden(
                "MCP server is disabled; enable it before retrying",
            ));
        }
        let retryable_before_retry = matches!(
            previous.status,
            McpServerStatus::Unavailable | McpServerStatus::Blacklisted | McpServerStatus::Failed
        );

        let mut registry = std::mem::take(self.integration.mcp_registry_mut());
        let result = self.retry_runtime_mcp_server_with_registry(
            &mut registry,
            server_id,
            previous.status,
            retryable_before_retry,
        );
        *self.integration.mcp_registry_mut() = registry;
        result
    }

    /// Runs the retry runtime mcp server with registry operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn retry_runtime_mcp_server_with_registry(
        &mut self,
        registry: &mut McpRegistry,
        server_id: &str,
        previous_status: McpServerStatus,
        retryable_before_retry: bool,
    ) -> Result<RuntimeMcpRetryReport> {
        registry.retry_server(server_id)?;
        self.integration.mcp_transports_mut().remove(server_id);
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let mut rediscovered = true;
        let mut reason = None;
        if let Err(error) = self.discover_runtime_mcp_transport(registry, server_id, &environment) {
            rediscovered = false;
            let message = error.message().to_string();
            let _ =
                registry.blacklist_for_session(server_id, message.clone(), current_unix_seconds());
            self.integration.mcp_transports_mut().remove(server_id);
            reason = Some(message);
        }

        let current = registry
            .list_servers()
            .into_iter()
            .find(|server| server.configured.id == server_id)
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "MCP server not found")
            })?;
        Ok(RuntimeMcpRetryReport {
            server_id: server_id.to_string(),
            previous_status,
            status: current.status,
            retryable_before_retry,
            rediscovered,
            tools: current.tools.len(),
            reason,
        })
    }

    /// Runs the discover runtime mcp transports async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) async fn discover_runtime_mcp_transports_async(
        &mut self,
        registry: &mut McpRegistry,
        environment: &BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        let server_ids = runtime_mcp_pending_discovery_server_ids(registry, |server| {
            runtime_mcp_server_has_live_auth_recovery(server, self.integration.auth_store())
        });
        let mut blacklisted = Vec::new();
        for server_id in server_ids {
            let should_reset = registry
                .list_servers()
                .into_iter()
                .find(|server| server.configured.id == server_id)
                .is_some_and(|server| {
                    runtime_mcp_server_needs_live_auth_rediscovery(
                        server,
                        runtime_mcp_server_has_live_auth_recovery(
                            server,
                            self.integration.auth_store(),
                        ),
                    )
                });
            if should_reset {
                registry.retry_server(&server_id)?;
            }
            match self
                .discover_runtime_mcp_transport_async(registry, &server_id, environment)
                .await
            {
                Ok(()) => {
                    if let Some(server) = registry
                        .list_servers()
                        .into_iter()
                        .find(|server| server.configured.id == server_id)
                    {
                        self.append_runtime_mcp_discovery_event(
                            &server_id,
                            server.status,
                            server.tools.len(),
                            None,
                            "runtime-mcp-discovery",
                        )?;
                    }
                }
                Err(error) => {
                    let reason = error.message().to_string();
                    let _ = registry.blacklist_for_session(
                        &server_id,
                        reason.clone(),
                        current_unix_seconds(),
                    );
                    self.integration.mcp_transports_mut().remove(&server_id);
                    self.append_runtime_mcp_discovery_event(
                        &server_id,
                        McpServerStatus::Blacklisted,
                        0,
                        Some(&reason),
                        "runtime-mcp-discovery",
                    )?;
                    blacklisted.push(server_id);
                }
            }
        }
        Ok(blacklisted)
    }

    /// Runs the discover runtime mcp transport operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn discover_runtime_mcp_transport(
        &mut self,
        registry: &mut McpRegistry,
        server_id: &str,
        environment: &BTreeMap<String, String>,
    ) -> Result<()> {
        let _ = (registry, environment);
        Err(MezError::invalid_state(format!(
            "MCP server `{server_id}` requires async runtime discovery"
        )))
    }

    /// Runs the discover runtime mcp transport async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) async fn discover_runtime_mcp_transport_async(
        &mut self,
        registry: &mut McpRegistry,
        server_id: &str,
        environment: &BTreeMap<String, String>,
    ) -> Result<()> {
        let checked_at = current_unix_seconds();
        let plan = registry.startup_plan(server_id, environment, checked_at)?;
        match &plan.transport {
            McpStartupTransportPlan::Stdio { .. } => {
                let mut connection = spawn_stdio_mcp_connection(&plan, environment).await?;
                let initialize = connection
                    .initialize("mezzanine", env!("CARGO_PKG_VERSION"), plan.timeout_ms)
                    .await?;
                connection.send_initialized_notification().await?;
                let mut tools = Vec::new();
                if initialize.supports_tools {
                    let mut cursor = None;
                    let mut pagination = mez_agent::mcp::McpToolListPagination::default();
                    loop {
                        let response = connection
                            .list_tools(cursor.as_deref(), plan.timeout_ms)
                            .await?;
                        tools.extend(response.tools);
                        let Some(next_cursor) =
                            pagination.advance(&plan.server_id, response.next_cursor)?
                        else {
                            break;
                        };
                        cursor = Some(next_cursor);
                    }
                }
                registry.mark_available_from_discovered_tools(server_id, tools, checked_at)?;
                self.integration
                    .mcp_transports_mut()
                    .insert_stdio(server_id.to_string(), connection);
            }
            McpStartupTransportPlan::StreamableHttp {
                bearer_token_env, ..
            } => {
                let oauth_token = if bearer_token_env.is_none() {
                    self.integration
                        .auth_store()
                        .and_then(|store| store.mcp_access_token(server_id).ok())
                } else {
                    None
                };
                let discovery = match discover_streamable_http_mcp_server_with_auth_token(
                    &plan,
                    environment,
                    "mezzanine",
                    env!("CARGO_PKG_VERSION"),
                    oauth_token
                        .as_ref()
                        .map(secrecy::ExposeSecret::expose_secret),
                )
                .await
                {
                    Ok(discovery) => discovery,
                    Err(error)
                        if error.kind() == crate::error::MezErrorKind::Forbidden
                            && bearer_token_env.is_none()
                            && self.integration.auth_store().is_some_and(|store| {
                                store.mcp_refresh_token(server_id).ok().flatten().is_some()
                            }) =>
                    {
                        let auth_store = self.integration.auth_store().ok_or_else(|| {
                            MezError::invalid_state("MCP OAuth refresh requires an auth store")
                        })?;
                        auth_store
                            .refresh_mcp_oauth_credential_for_server_async(server_id)
                            .await?;
                        let refreshed_token = auth_store.mcp_access_token(server_id)?;
                        discover_streamable_http_mcp_server_with_auth_token(
                            &plan,
                            environment,
                            "mezzanine",
                            env!("CARGO_PKG_VERSION"),
                            Some(secrecy::ExposeSecret::expose_secret(&refreshed_token)),
                        )
                        .await?
                    }
                    Err(error) => return Err(error),
                };
                registry.mark_available_from_discovered_tools(
                    server_id,
                    discovery.tools.clone(),
                    checked_at,
                )?;
                self.integration
                    .mcp_transports_mut()
                    .insert_streamable_http(
                        server_id.to_string(),
                        RuntimeHttpMcpTransportState {
                            startup_plan: plan,
                            session_id: discovery.session_id,
                            next_request_id: 1000,
                        },
                    );
            }
        }
        Ok(())
    }

    /// Appends a lifecycle event for one MCP server discovery result.
    pub(super) fn append_runtime_mcp_discovery_event(
        &mut self,
        server_id: &str,
        status: McpServerStatus,
        tools: usize,
        reason: Option<&str>,
        source: &str,
    ) -> Result<()> {
        let reason_json = reason
            .map(|reason| format!(r#""{}""#, json_escape(reason)))
            .unwrap_or_else(|| "null".to_string());
        self.append_lifecycle_event(
            EventKind::McpServerChanged,
            format!(
                r#"{{"server_id":"{}","status":"{}","tools":{},"reason":{},"source":"{}","message":"{}"}}"#,
                json_escape(server_id),
                runtime_mcp_service_status_name(status),
                tools,
                reason_json,
                json_escape(source),
                json_escape(&runtime_mcp_discovery_message(
                    server_id, status, tools, reason
                ))
            ),
        )
    }

    /// Appends readable server status events for pre-discovery unavailable MCP servers.
    pub(super) fn append_runtime_mcp_prechecked_status_events(
        &mut self,
        registry: &McpRegistry,
        source: &str,
    ) -> Result<()> {
        let statuses = registry
            .list_servers()
            .into_iter()
            .filter(|server| {
                server.configured.enabled
                    && matches!(
                        server.status,
                        McpServerStatus::Unavailable
                            | McpServerStatus::Blacklisted
                            | McpServerStatus::Failed
                    )
            })
            .map(|server| {
                (
                    server.configured.id.clone(),
                    server.status,
                    server.tools.len(),
                    server.blacklist_reason.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (server_id, status, tools, reason) in statuses {
            self.append_runtime_mcp_discovery_event(
                &server_id,
                status,
                tools,
                reason.as_deref(),
                source,
            )?;
        }
        Ok(())
    }

    /// Appends a readable lifecycle event before a batch MCP initialization run.
    pub(super) fn append_runtime_mcp_initialization_started_event(
        &mut self,
        source: &str,
        pending_servers: usize,
    ) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::McpServerChanged,
            format!(
                r#"{{"phase":"started","pending_servers":{},"source":"{}","message":"{}"}}"#,
                pending_servers,
                json_escape(source),
                json_escape(&format!(
                    "Starting MCP initialization for {} configured {}.",
                    pending_servers,
                    runtime_mcp_server_word(pending_servers)
                ))
            ),
        )
    }

    /// Appends a readable lifecycle event after a batch MCP initialization run.
    pub(super) fn append_runtime_mcp_initialization_completed_event(
        &mut self,
        registry: &McpRegistry,
        source: &str,
        attempted_servers: usize,
    ) -> Result<()> {
        let counts = RuntimeMcpInitializationCounts::from_registry(registry);
        self.append_lifecycle_event(
            EventKind::McpServerChanged,
            format!(
                r#"{{"phase":"completed","attempted_servers":{},"enabled_servers":{},"available_servers":{},"unavailable_servers":{},"pending_servers":{},"tools":{},"source":"{}","message":"{}"}}"#,
                attempted_servers,
                counts.enabled_servers,
                counts.available_servers,
                counts.unavailable_servers(),
                counts.pending_servers(),
                counts.available_tools,
                json_escape(source),
                json_escape(&format!(
                    "MCP initialization complete: {} enabled {} ready to field requests, {} unavailable, {} pending, {} available {}.",
                    counts.available_servers,
                    runtime_mcp_server_word(counts.available_servers),
                    counts.unavailable_servers(),
                    counts.pending_servers(),
                    counts.available_tools,
                    runtime_mcp_tool_word(counts.available_tools)
                ))
            ),
        )
    }
}
