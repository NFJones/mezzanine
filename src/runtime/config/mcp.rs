//! Runtime MCP server option readers.
//!
//! This module owns `mcp_servers` materialization from effective runtime
//! configuration, including server transport, approval, timeout, and external
//! capability settings. Keeping MCP parsing here separates integration policy
//! from provider, hook, permission, and general JSON helper domains.

use super::*;
use crate::mcp::{McpApprovalSetting, McpExternalCapability, McpServerConfig, McpServerKind};

/// Runs the runtime mcp registry from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_mcp_registry_from_config(root: &Value) -> Result<McpRegistry> {
    let mut registry = McpRegistry::default();
    let Some(servers) = runtime_json_object(root, "mcp_servers") else {
        return Ok(registry);
    };
    for (server_id, value) in servers {
        let Some(server) = value.as_object() else {
            return Err(MezError::config(format!(
                "mcp_servers.{server_id} must be an object"
            )));
        };
        let name = runtime_json_string(server.get("name")).unwrap_or(server_id);
        let args = runtime_json_string_array(server.get("args"))?.unwrap_or_default();
        let has_stdio_args = !args.is_empty();
        let mut config = if let Some(url) = runtime_json_string(server.get("url")) {
            McpServerConfig::streamable_http(server_id, name, url)
        } else {
            let command = runtime_json_string(server.get("command")).ok_or_else(|| {
                MezError::config(format!(
                    "mcp_servers.{server_id}.command is required for stdio transport"
                ))
            })?;
            McpServerConfig::stdio(server_id, name, command, args)
        };
        if let Some(enabled) = runtime_json_bool(server.get("enabled")) {
            config.enabled = enabled;
        }
        if config.kind == McpServerKind::Http && has_stdio_args {
            return Err(MezError::config(format!(
                "mcp_servers.{server_id}.args is only valid for stdio transport"
            )));
        }
        config.env = runtime_json_string_map(server.get("env"))?.unwrap_or_default();
        config.env_vars = runtime_json_string_array(server.get("env_vars"))?.unwrap_or_default();
        config.cwd = runtime_json_string(server.get("cwd")).map(ToOwned::to_owned);
        config.http_headers =
            runtime_json_string_map(server.get("http_headers"))?.unwrap_or_default();
        config.bearer_token_env =
            runtime_json_string(server.get("bearer_token_env")).map(ToOwned::to_owned);
        config.enabled_tools =
            runtime_json_string_array(server.get("enabled_tools"))?.unwrap_or_default();
        config.disabled_tools =
            runtime_json_string_array(server.get("disabled_tools"))?.unwrap_or_default();
        if let Some(timeout) = runtime_json_u64(server.get("startup_timeout_ms")) {
            config.startup_timeout_ms = timeout;
        } else if let Some(timeout) = runtime_json_u64(server.get("startup_timeout_sec")) {
            config.startup_timeout_ms = timeout.saturating_mul(1000);
        }
        if let Some(timeout) = runtime_json_u64(server.get("tool_timeout_ms")) {
            config.tool_timeout_ms = timeout;
        } else if let Some(timeout) = runtime_json_u64(server.get("tool_timeout_sec")) {
            config.tool_timeout_ms = timeout.saturating_mul(1000);
        }
        if let Some(approval) = runtime_json_string(server.get("approval")) {
            config.approval = runtime_mcp_approval_setting(approval)?;
        }
        if let Some(approvals) = server.get("tool_approvals") {
            config.tool_approvals = runtime_mcp_tool_approvals(approvals)?;
        }
        if let Some(external) = server.get("external_capability") {
            config.external_capability = runtime_mcp_external_capability(external)?;
        }
        registry.add_server(config)?;
    }
    Ok(registry)
}

/// Parses the MCP approval policy setting used by a server or individual tool.
fn runtime_mcp_approval_setting(value: &str) -> Result<McpApprovalSetting> {
    match value {
        "inherit" => Ok(McpApprovalSetting::Inherit),
        "prompt" => Ok(McpApprovalSetting::Prompt),
        "allow" => Ok(McpApprovalSetting::Allow),
        "deny" | "forbid" => Ok(McpApprovalSetting::Deny),
        _ => Err(MezError::config("unsupported MCP approval setting")),
    }
}

/// Parses per-tool MCP approval overrides from a server configuration object.
fn runtime_mcp_tool_approvals(value: &Value) -> Result<BTreeMap<String, McpApprovalSetting>> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config("mcp tool_approvals must be a map"))?;
    let mut approvals = BTreeMap::new();
    for (tool, approval) in object {
        let Some(value) = approval.as_str() else {
            return Err(MezError::config("mcp tool approvals must be strings"));
        };
        approvals.insert(tool.clone(), runtime_mcp_approval_setting(value)?);
    }
    Ok(approvals)
}

/// Parses the external-capability classification used for MCP capability routing.
fn runtime_mcp_external_capability(value: &Value) -> Result<McpExternalCapability> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config("mcp external_capability must be a map"))?;
    Ok(McpExternalCapability {
        mutates_filesystem_outside_shell: runtime_json_bool(
            object.get("mutates_filesystem_outside_shell"),
        )
        .unwrap_or(false),
        executes_processes_outside_shell: runtime_json_bool(
            object.get("executes_processes_outside_shell"),
        )
        .unwrap_or(false),
        accesses_credentials_outside_shell: runtime_json_bool(
            object.get("accesses_credentials_outside_shell"),
        )
        .unwrap_or(false),
        purpose: runtime_json_string(object.get("purpose"))
            .unwrap_or_default()
            .to_string(),
        usage_instructions: runtime_json_string(object.get("usage_instructions"))
            .unwrap_or_default()
            .to_string(),
    })
}
