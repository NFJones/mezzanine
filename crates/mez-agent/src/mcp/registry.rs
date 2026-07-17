//! MCP server registry, startup planning, availability state, and prompt summaries.
//!
//! The registry owns configured server state, session blacklisting, environment
//! gating, tool exposure, and permission-aware tool-call planning.

use std::collections::BTreeMap;

use super::prompt::{
    AgentShellMcpServerSummary, AgentShellMcpSummary, AgentShellMcpToolSummary, McpPromptServer,
    McpPromptSummary, McpPromptTool, McpPromptUnavailableServer,
};
use super::types::{
    McpApprovalSetting, McpDiscoveredTool, McpEnvironmentPlan, McpServerConfig, McpServerKind,
    McpServerState, McpServerStatus, McpStartupPlan, McpStartupTransportPlan, McpToolCallPlan,
    McpToolCallRequest, McpToolEffects, McpToolState,
};
use super::{McpError as MezError, McpResult as Result, validate_mcp_tool_input_schema};

/// Normalizes model-visible MCP metadata without omitting call-relevant text.
fn normalized_mcp_prompt_text(value: &str) -> Option<String> {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed)
}

/// Carries Mcp Registry state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
pub struct McpRegistry {
    /// Stores the servers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    servers: BTreeMap<String, McpServerState>,
}

impl McpRegistry {
    /// Replaces registry contents with already-sanitized server states.
    ///
    /// Snapshot resume uses this to make saved MCP metadata visible without
    /// reintroducing raw transport credentials or treating restored metadata as
    /// a live transport authority.
    pub fn replace_with_states(&mut self, servers: Vec<McpServerState>) {
        self.servers = servers
            .into_iter()
            .map(|server| (server.configured.id.clone(), server))
            .collect();
    }

    /// Runs the add server operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn add_server(&mut self, config: McpServerConfig) -> Result<()> {
        config.validate()?;
        self.servers.insert(
            config.id.clone(),
            McpServerState {
                configured: config,
                status: McpServerStatus::Configured,
                last_checked_at_unix_seconds: None,
                blacklist_reason: None,
                discovered_instructions: None,
                tools: Vec::new(),
            },
        );
        Ok(())
    }

    /// Runs the mark available operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mark_available(
        &mut self,
        server_id: &str,
        tools: Vec<McpToolState>,
        checked_at_unix_seconds: u64,
    ) -> Result<()> {
        let server = self.server_mut(server_id)?;
        server.status = McpServerStatus::Available;
        server.last_checked_at_unix_seconds = Some(checked_at_unix_seconds);
        server.blacklist_reason = None;
        server.tools = tools
            .into_iter()
            .map(|mut tool| {
                tool.server_id = server_id.to_string();
                let schema_error = validate_mcp_tool_input_schema(&tool.input_schema_json).err();
                tool.available =
                    server.configured.tool_allowed_by_config(&tool.name) && schema_error.is_none();
                tool.blacklisted = false;
                tool.approval = server.configured.approval_for_tool(&tool.name);
                if let Some(reason) = schema_error {
                    tool.description = format!(
                        "{} Unavailable: invalid MCP input schema ({reason}).",
                        tool.description.trim()
                    )
                    .trim()
                    .to_string();
                }
                tool
            })
            .collect();
        Ok(())
    }

    /// Runs the mark available from discovered tools operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mark_available_from_discovered_tools(
        &mut self,
        server_id: &str,
        tools: Vec<McpDiscoveredTool>,
        checked_at_unix_seconds: u64,
    ) -> Result<()> {
        self.mark_available_from_discovery(server_id, tools, None, checked_at_unix_seconds)
    }

    /// Makes discovered tools callable and retains model-safe server guidance.
    pub fn mark_available_from_discovery(
        &mut self,
        server_id: &str,
        tools: Vec<McpDiscoveredTool>,
        instructions: Option<&str>,
        checked_at_unix_seconds: u64,
    ) -> Result<()> {
        let tool_states = tools
            .into_iter()
            .map(|tool| McpToolState {
                server_id: server_id.to_string(),
                name: tool.name,
                available: true,
                blacklisted: false,
                permission_required: true,
                effects: McpToolEffects {
                    has_side_effects: true,
                    ..McpToolEffects::none()
                },
                approval: McpApprovalSetting::Inherit,
                description: tool.description,
                input_schema_json: tool.input_schema_json,
            })
            .collect();
        self.mark_available(server_id, tool_states, checked_at_unix_seconds)?;
        self.server_mut(server_id)?.discovered_instructions =
            normalized_mcp_prompt_text(instructions.unwrap_or_default());
        Ok(())
    }

    /// Runs the mark starting operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mark_starting(&mut self, server_id: &str, checked_at_unix_seconds: u64) -> Result<()> {
        let server = self.server_mut(server_id)?;
        ensure_enabled(server)?;
        server.status = McpServerStatus::Starting;
        server.last_checked_at_unix_seconds = Some(checked_at_unix_seconds);
        Ok(())
    }

    /// Runs the mark unavailable operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mark_unavailable(
        &mut self,
        server_id: &str,
        reason: impl Into<String>,
        checked_at_unix_seconds: u64,
    ) -> Result<()> {
        let server = self.server_mut(server_id)?;
        server.status = McpServerStatus::Unavailable;
        server.last_checked_at_unix_seconds = Some(checked_at_unix_seconds);
        server.blacklist_reason = Some(reason.into());
        for tool in &mut server.tools {
            tool.available = false;
        }
        Ok(())
    }

    /// Runs the mark failed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mark_failed(
        &mut self,
        server_id: &str,
        reason: impl Into<String>,
        checked_at_unix_seconds: u64,
    ) -> Result<()> {
        let server = self.server_mut(server_id)?;
        server.status = McpServerStatus::Failed;
        server.last_checked_at_unix_seconds = Some(checked_at_unix_seconds);
        server.blacklist_reason = Some(reason.into());
        for tool in &mut server.tools {
            tool.available = false;
        }
        Ok(())
    }

    /// Runs the blacklist for session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn blacklist_for_session(
        &mut self,
        server_id: &str,
        reason: impl Into<String>,
        checked_at_unix_seconds: u64,
    ) -> Result<()> {
        let server = self.server_mut(server_id)?;
        server.status = McpServerStatus::Blacklisted;
        server.last_checked_at_unix_seconds = Some(checked_at_unix_seconds);
        server.blacklist_reason = Some(reason.into());
        for tool in &mut server.tools {
            tool.available = false;
            tool.blacklisted = true;
        }
        Ok(())
    }

    /// Runs the retry server operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn retry_server(&mut self, server_id: &str) -> Result<()> {
        let server = self.server_mut(server_id)?;
        ensure_enabled(server)?;
        server.status = McpServerStatus::Configured;
        server.blacklist_reason = None;
        for tool in &mut server.tools {
            tool.available = false;
            tool.blacklisted = false;
        }
        Ok(())
    }

    /// Runs the blacklist servers with missing environment operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn blacklist_servers_with_missing_environment(
        &mut self,
        environment: &BTreeMap<String, String>,
        checked_at_unix_seconds: u64,
    ) -> Result<Vec<String>> {
        let server_ids = self.servers.keys().cloned().collect::<Vec<_>>();
        let mut blacklisted = Vec::new();
        for server_id in server_ids {
            let missing = {
                let server = self.server(&server_id)?;
                if !server.configured.enabled {
                    Vec::new()
                } else {
                    missing_environment(server, environment)
                }
            };
            if missing.is_empty() {
                continue;
            }
            let reason = format!("missing required environment: {}", missing.join(", "));
            self.blacklist_for_session(&server_id, &reason, checked_at_unix_seconds)?;
            blacklisted.push(server_id);
        }
        Ok(blacklisted)
    }

    /// Runs the startup plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn startup_plan(
        &mut self,
        server_id: &str,
        environment: &BTreeMap<String, String>,
        checked_at_unix_seconds: u64,
    ) -> Result<McpStartupPlan> {
        let server = self.server(server_id)?;
        ensure_enabled(server)?;
        ensure_not_session_blacklisted(server)?;
        let missing = missing_environment(server, environment);
        if !missing.is_empty() {
            let reason = format!("missing required environment: {}", missing.join(", "));
            self.blacklist_for_session(server_id, &reason, checked_at_unix_seconds)?;
            return Err(MezError::forbidden(reason));
        }

        let server = self.server(server_id)?;
        let transport = match server.configured.kind {
            McpServerKind::Stdio => McpStartupTransportPlan::Stdio {
                command: server.configured.command.clone().ok_or_else(|| {
                    MezError::invalid_state("stdio MCP server has no command after validation")
                })?,
                args: server.configured.args.clone(),
                environment: McpEnvironmentPlan {
                    set: server.configured.env.clone(),
                    pass: server.configured.env_vars.clone(),
                },
                cwd: server.configured.cwd.clone(),
            },
            McpServerKind::Http => McpStartupTransportPlan::StreamableHttp {
                url: server.configured.url.clone().ok_or_else(|| {
                    MezError::invalid_state("HTTP MCP server has no url after validation")
                })?,
                headers: server.configured.http_headers.clone(),
                bearer_token_env: server.configured.bearer_token_env.clone(),
            },
        };
        let plan = McpStartupPlan {
            server_id: server.configured.id.clone(),
            server_name: server.configured.name.clone(),
            timeout_ms: server.configured.startup_timeout_ms,
            transport,
        };
        self.mark_starting(server_id, checked_at_unix_seconds)?;
        Ok(plan)
    }

    /// Runs the plan tool call operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn plan_tool_call(&self, request: &McpToolCallRequest) -> Result<McpToolCallPlan> {
        let server = self.server(&request.server_id)?;
        ensure_enabled(server)?;
        ensure_available(server)?;
        ensure_not_session_blacklisted(server)?;
        if !server.configured.tool_allowed_by_config(&request.tool_name) {
            return Err(MezError::forbidden("MCP tool is disabled by configuration"));
        }
        let tool = server
            .tools
            .iter()
            .find(|tool| tool.name == request.tool_name)
            .ok_or_else(|| MezError::not_found("MCP tool not found"))?;
        if tool.blacklisted || !tool.available {
            return Err(MezError::forbidden("MCP tool is not available"));
        }
        let approval = server.configured.approval_for_tool(&tool.name);
        if approval == McpApprovalSetting::Deny && !request.approval_bypass {
            return Err(MezError::forbidden("MCP tool is denied by policy"));
        }
        let approval_required = !request.approval_bypass
            && match approval {
                McpApprovalSetting::Prompt => true,
                McpApprovalSetting::Allow => false,
                McpApprovalSetting::Deny => false,
                McpApprovalSetting::Inherit => tool.permission_required || tool.effects.risky(),
            };
        Ok(McpToolCallPlan {
            server_id: request.server_id.clone(),
            tool_name: request.tool_name.clone(),
            arguments_json: request.arguments_json.clone(),
            timeout_ms: request
                .timeout_ms
                .unwrap_or(server.configured.tool_timeout_ms),
            approval_required,
            audit_event_class: "external_integration",
            effects: tool.effects,
        })
    }

    /// Runs the available tools operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn available_tools(&self) -> Vec<&McpToolState> {
        self.servers
            .values()
            .filter(|server| server.status == McpServerStatus::Available)
            .flat_map(|server| server.tools.iter())
            .filter(|tool| tool.available && !tool.blacklisted)
            .collect()
    }

    /// Runs the prompt summary operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn prompt_summary(&self) -> McpPromptSummary {
        let available_servers = self
            .servers
            .values()
            .filter(|server| {
                server.status == McpServerStatus::Available && server.configured.enabled
            })
            .map(|server| {
                let available_tools = server
                    .tools
                    .iter()
                    .filter(|tool| tool.available && !tool.blacklisted)
                    .collect::<Vec<_>>();
                let approval_required_tool_count = available_tools
                    .iter()
                    .filter(|tool| {
                        server.configured.approval_for_tool(&tool.name)
                            == McpApprovalSetting::Prompt
                            || (server.configured.approval_for_tool(&tool.name)
                                == McpApprovalSetting::Inherit
                                && (tool.permission_required || tool.effects.risky()))
                    })
                    .count();
                McpPromptServer {
                    server_id: server.configured.id.clone(),
                    display_name: server.configured.name.clone(),
                    purpose: mcp_prompt_server_purpose(server, &available_tools),
                    usage_instructions: mcp_prompt_server_usage_instructions(server),
                    tool_count: available_tools.len(),
                    approval_required_tool_count,
                }
            })
            .collect();
        let available_tools = self
            .servers
            .values()
            .filter(|server| {
                server.status == McpServerStatus::Available && server.configured.enabled
            })
            .flat_map(|server| {
                server
                    .tools
                    .iter()
                    .filter(|tool| tool.available && !tool.blacklisted)
                    .map(|tool| McpPromptTool {
                        server_id: server.configured.id.clone(),
                        tool_name: tool.name.clone(),
                        description: mcp_prompt_tool_description(server, tool),
                        approval_required: server.configured.approval_for_tool(&tool.name)
                            == McpApprovalSetting::Prompt
                            || (server.configured.approval_for_tool(&tool.name)
                                == McpApprovalSetting::Inherit
                                && (tool.permission_required || tool.effects.risky())),
                        input_schema_json: tool.input_schema_json.clone(),
                    })
            })
            .collect();
        let unavailable_servers = self
            .servers
            .values()
            .filter(|server| {
                !server.configured.enabled
                    || matches!(
                        server.status,
                        McpServerStatus::Configured
                            | McpServerStatus::Starting
                            | McpServerStatus::Unavailable
                            | McpServerStatus::Blacklisted
                            | McpServerStatus::Failed
                    )
            })
            .map(|server| McpPromptUnavailableServer {
                server_id: server.configured.id.clone(),
                purpose: server.configured.external_capability.purpose.clone(),
                usage_instructions: server
                    .configured
                    .external_capability
                    .usage_instructions
                    .clone(),
                reason: mcp_prompt_unavailable_reason(server),
                retryable: server.configured.enabled
                    && matches!(
                        server.status,
                        McpServerStatus::Configured
                            | McpServerStatus::Starting
                            | McpServerStatus::Unavailable
                            | McpServerStatus::Blacklisted
                            | McpServerStatus::Failed
                    ),
            })
            .collect();
        McpPromptSummary {
            available_servers,
            available_tools,
            unavailable_servers,
        }
    }

    /// Runs the list servers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn list_servers(&self) -> Vec<&McpServerState> {
        self.servers.values().collect()
    }

    /// Projects live registry state into bounded agent-shell display records.
    ///
    /// Discovery, transports, credentials, approval enforcement, and execution
    /// remain registry/runtime responsibilities and are not exposed here.
    pub fn agent_shell_summary(&self) -> AgentShellMcpSummary {
        let servers =
            self.list_servers()
                .into_iter()
                .map(|server| {
                    let session_blacklisted = server.status == McpServerStatus::Blacklisted;
                    let blacklisted = session_blacklisted || server.blacklist_reason.is_some();
                    let retryable = server.configured.enabled
                        && matches!(
                            server.status,
                            McpServerStatus::Unavailable
                                | McpServerStatus::Blacklisted
                                | McpServerStatus::Failed
                        );
                    let state = if !server.configured.enabled {
                        "disabled"
                    } else {
                        match server.status {
                            McpServerStatus::Configured => "enabled",
                            McpServerStatus::Starting => "starting",
                            McpServerStatus::Available => "available",
                            McpServerStatus::Unavailable => "unavailable",
                            McpServerStatus::Blacklisted => "blacklisted",
                            McpServerStatus::Failed => "failed",
                        }
                    };
                    let status = match server.status {
                        McpServerStatus::Configured => "configured",
                        McpServerStatus::Starting => "starting",
                        McpServerStatus::Available => "available",
                        McpServerStatus::Unavailable => "unavailable",
                        McpServerStatus::Blacklisted => "blacklisted",
                        McpServerStatus::Failed => "failed",
                    };
                    let transport = match server.configured.kind {
                        McpServerKind::Stdio => "stdio",
                        McpServerKind::Http => "streamable-http",
                    };
                    let tools = server
                        .tools
                        .iter()
                        .map(|tool| {
                            let state = if !server.configured.tool_allowed_by_config(&tool.name) {
                                "disabled"
                            } else if tool.blacklisted {
                                "blacklisted"
                            } else if tool.available {
                                "available"
                            } else {
                                "unavailable"
                            };
                            let approval = match tool.approval {
                                McpApprovalSetting::Inherit => "inherit",
                                McpApprovalSetting::Prompt => "prompt",
                                McpApprovalSetting::Allow => "allow",
                                McpApprovalSetting::Deny => "deny",
                            };
                            let mut effects = Vec::new();
                            if tool.effects.reads_filesystem {
                                effects.push("read-fs");
                            }
                            if tool.effects.mutates_filesystem {
                                effects.push("mutate-fs");
                            }
                            if tool.effects.executes_processes {
                                effects.push("execute-process");
                            }
                            if tool.effects.accesses_credentials {
                                effects.push("credential-access");
                            }
                            if tool.effects.uses_network {
                                effects.push("network");
                            }
                            if tool.effects.has_side_effects {
                                effects.push("side-effects");
                            }
                            AgentShellMcpToolSummary {
                                name: tool.name.clone(),
                                state: state.to_string(),
                                approval: approval.to_string(),
                                permission_required: tool.permission_required,
                                effects: if effects.is_empty() {
                                    "none".to_string()
                                } else {
                                    effects.join(",")
                                },
                                description: tool.description.clone(),
                            }
                        })
                        .collect();
                    AgentShellMcpServerSummary {
                        server_id: server.configured.id.clone(),
                        display_name: server.configured.name.clone(),
                        state: state.to_string(),
                        status: status.to_string(),
                        enabled: server.configured.enabled,
                        transport: transport.to_string(),
                        blacklisted,
                        session_blacklisted,
                        retryable,
                        reason: server.blacklist_reason.clone().or_else(|| {
                            (!server.configured.enabled).then(|| "disabled".to_string())
                        }),
                        tools,
                    }
                })
                .collect();
        AgentShellMcpSummary { servers }
    }

    /// Runs the server operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn server(&self, server_id: &str) -> Result<&McpServerState> {
        self.servers
            .get(server_id)
            .ok_or_else(|| MezError::not_found("MCP server not found"))
    }

    /// Runs the server mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn server_mut(&mut self, server_id: &str) -> Result<&mut McpServerState> {
        self.servers
            .get_mut(server_id)
            .ok_or_else(|| MezError::not_found("MCP server not found"))
    }
}

/// Returns a model-facing reason for one non-callable MCP server state.
fn mcp_prompt_unavailable_reason(server: &McpServerState) -> String {
    if !server.configured.enabled {
        return "disabled".to_string();
    }
    if let Some(reason) = &server.blacklist_reason {
        return reason.clone();
    }
    match server.status {
        McpServerStatus::Configured => "runtime discovery pending".to_string(),
        McpServerStatus::Starting => "runtime discovery in progress".to_string(),
        McpServerStatus::Unavailable => "unavailable".to_string(),
        McpServerStatus::Blacklisted => "blacklisted for this session".to_string(),
        McpServerStatus::Failed => "startup or protocol failure".to_string(),
        McpServerStatus::Available => "available".to_string(),
    }
}

/// Builds model-facing tool metadata from tool and server capability text.
fn mcp_prompt_tool_description(server: &McpServerState, tool: &McpToolState) -> String {
    let mut parts = Vec::new();
    mcp_prompt_push_description_part(&mut parts, "", &tool.description);
    mcp_prompt_push_description_part(
        &mut parts,
        "User-configured non-authoritative server purpose",
        &server.configured.external_capability.purpose,
    );
    mcp_prompt_push_description_part(
        &mut parts,
        "User-configured non-authoritative usage guidance",
        &server.configured.external_capability.usage_instructions,
    );
    mcp_prompt_push_description_part(
        &mut parts,
        "MCP-server-provided non-authoritative instructions",
        server
            .discovered_instructions
            .as_deref()
            .unwrap_or_default(),
    );
    parts.join(" ")
}

/// Returns configured purpose or a complete fallback derived from discovered tools.
fn mcp_prompt_server_purpose(server: &McpServerState, tools: &[&McpToolState]) -> String {
    if let Some(configured) =
        normalized_mcp_prompt_text(&server.configured.external_capability.purpose)
    {
        return configured;
    }
    let fallback = tools
        .iter()
        .map(|tool| {
            let description = tool
                .description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if description.is_empty() {
                tool.name.clone()
            } else {
                format!("{}: {}", tool.name, description)
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    normalized_mcp_prompt_text(&fallback).unwrap_or_default()
}

/// Combines higher-priority configured guidance with discovered instructions.
fn mcp_prompt_server_usage_instructions(server: &McpServerState) -> String {
    let configured =
        normalized_mcp_prompt_text(&server.configured.external_capability.usage_instructions);
    let discovered = server.discovered_instructions.as_deref();
    match (configured, discovered) {
        (Some(configured), Some(discovered)) => format!(
            "Operator guidance: {configured} MCP-server-provided non-authoritative instructions: {discovered}"
        ),
        (Some(configured), None) => configured,
        (None, Some(discovered)) => {
            format!("MCP-server-provided non-authoritative instructions: {discovered}")
        }
        (None, None) => String::new(),
    }
}

/// Appends one compact prompt-description clause when the source text exists.
fn mcp_prompt_push_description_part(parts: &mut Vec<String>, label: &str, value: &str) {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return;
    }
    if label.is_empty() {
        parts.push(collapsed);
    } else {
        parts.push(format!("{label}: {collapsed}."));
    }
}

/// Runs the ensure enabled operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn ensure_enabled(server: &McpServerState) -> Result<()> {
    if server.configured.enabled {
        Ok(())
    } else {
        Err(MezError::forbidden("MCP server is disabled"))
    }
}

/// Runs the ensure available operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn ensure_available(server: &McpServerState) -> Result<()> {
    if server.status == McpServerStatus::Available {
        Ok(())
    } else {
        Err(MezError::forbidden("MCP server is not available"))
    }
}

/// Runs the ensure not session blacklisted operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn ensure_not_session_blacklisted(server: &McpServerState) -> Result<()> {
    if server.status == McpServerStatus::Blacklisted {
        Err(MezError::forbidden(
            "MCP server is blacklisted for the session",
        ))
    } else {
        Ok(())
    }
}

/// Runs the missing environment operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn missing_environment(
    server: &McpServerState,
    environment: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut missing: Vec<String> = server
        .configured
        .env_vars
        .iter()
        .filter(|name| !environment.contains_key(*name))
        .cloned()
        .collect();
    if let Some(token_env) = &server.configured.bearer_token_env
        && !environment.contains_key(token_env)
    {
        missing.push(token_env.clone());
    }
    missing
}
