//! Persisted MCP server configuration command helpers.
//!
//! This module owns the shared, side-effect-bounded planning layer for MCP
//! server configuration commands. CLI and terminal-command callers use these
//! helpers so enablement, safe settings, tool filters, and approval policy are
//! persisted to the same `mcp_servers.<id>` configuration paths with identical
//! validation and display semantics.

use crate::config::{
    ConfigMutation, ConfigMutationOperation, ConfigMutationPlan, ConfigMutationValue, ConfigPaths,
    ConfigScope, persist_config_mutation,
};
use crate::error::{MezError, Result};

/// Transport-specific MCP server settings accepted by `mcp add`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpConfigTransport {
    /// Register a stdio MCP server command with argv fragments.
    Stdio { command: String, args: Vec<String> },
    /// Register a streamable HTTP MCP endpoint.
    StreamableHttp { url: String },
}

/// Safe scalar MCP server settings accepted by config commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpConfigSetting {
    /// Human-facing server display name.
    Name,
    /// Working directory for stdio server startup.
    Cwd,
    /// Startup timeout in milliseconds.
    StartupTimeoutMs,
    /// Tool-call timeout in milliseconds.
    ToolTimeoutMs,
    /// Environment-variable name containing a bearer token.
    BearerTokenEnv,
}

/// One shared MCP configuration command to plan or persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpConfigCommand {
    /// Add or replace one configured MCP server.
    Add {
        /// Stable server identifier.
        id: String,
        /// Transport details to persist.
        transport: McpConfigTransport,
        /// Whether the server is enabled after registration.
        enabled: bool,
    },
    /// Remove one configured MCP server table.
    Remove { id: String },
    /// Toggle one server's persisted enablement bit.
    Enable { id: String, enabled: bool },
    /// Set one safe scalar server setting.
    Set {
        /// Stable server identifier.
        id: String,
        /// Setting to update.
        setting: McpConfigSetting,
        /// User-supplied value.
        value: String,
    },
    /// Remove one safe scalar server setting.
    Unset {
        /// Stable server identifier.
        id: String,
        /// Setting to remove.
        setting: McpConfigSetting,
    },
    /// Replace the allow-list of enabled MCP tools.
    ToolsEnable { id: String, tools: Vec<String> },
    /// Replace the deny-list of disabled MCP tools.
    ToolsDisable { id: String, tools: Vec<String> },
    /// Clear both tool allow and deny lists.
    ToolsReset { id: String },
    /// Set server-level tool approval policy.
    ApprovalSet { id: String, approval: String },
    /// Remove server-level tool approval policy.
    ApprovalUnset { id: String },
}

/// Aggregate result for a persisted MCP config command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpConfigCommandReport {
    /// Whether any persisted config text changed.
    pub changed: bool,
    /// Whether runtime config reload is required to observe the change.
    pub reload_required: bool,
}

/// Validates a dynamic MCP config identifier used as a config path segment.
pub fn validate_mcp_config_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(MezError::invalid_args(format!(
            "{label} must contain only ASCII letters, digits, '_' or '-'"
        )));
    }
    Ok(())
}

/// Builds the scalar config mutations for one MCP configuration command.
pub fn mcp_config_command_mutations(command: &McpConfigCommand) -> Result<Vec<ConfigMutation>> {
    match command {
        McpConfigCommand::Add {
            id,
            transport,
            enabled,
        } => {
            validate_mcp_config_identifier(id, "MCP server id")?;
            let mut mutations = vec![config_set_boolean(
                format!("mcp_servers.{id}.enabled"),
                *enabled,
            )];
            match transport {
                McpConfigTransport::Stdio { command, args } => {
                    if command.trim().is_empty() {
                        return Err(MezError::invalid_args(
                            "mcp add --command requires a non-empty command",
                        ));
                    }
                    mutations.push(config_set_string(
                        format!("mcp_servers.{id}.command"),
                        command,
                    ));
                    mutations.push(config_set_string_array(
                        format!("mcp_servers.{id}.args"),
                        args,
                    ));
                    mutations.push(config_unset(format!("mcp_servers.{id}.url")));
                }
                McpConfigTransport::StreamableHttp { url } => {
                    if !(url.starts_with("http://") || url.starts_with("https://")) {
                        return Err(MezError::invalid_args(
                            "mcp add --url requires an http:// or https:// URL",
                        ));
                    }
                    mutations.push(config_set_string(format!("mcp_servers.{id}.url"), url));
                    mutations.push(config_unset(format!("mcp_servers.{id}.command")));
                    mutations.push(config_set_string_array(
                        format!("mcp_servers.{id}.args"),
                        &[],
                    ));
                }
            }
            Ok(mutations)
        }
        McpConfigCommand::Remove { id } => {
            validate_mcp_config_identifier(id, "MCP server id")?;
            Ok(vec![config_unset(format!("mcp_servers.{id}"))])
        }
        McpConfigCommand::Enable { id, enabled } => {
            validate_mcp_config_identifier(id, "MCP server id")?;
            Ok(vec![config_set_boolean(
                format!("mcp_servers.{id}.enabled"),
                *enabled,
            )])
        }
        McpConfigCommand::Set { id, setting, value } => {
            validate_mcp_config_identifier(id, "MCP server id")?;
            Ok(vec![mcp_config_setting_mutation(id, *setting, value)?])
        }
        McpConfigCommand::Unset { id, setting } => {
            validate_mcp_config_identifier(id, "MCP server id")?;
            Ok(vec![config_unset(format!(
                "mcp_servers.{id}.{}",
                mcp_config_setting_key(*setting)
            ))])
        }
        McpConfigCommand::ToolsEnable { id, tools } => {
            validate_mcp_config_identifier(id, "MCP server id")?;
            validate_tool_names(tools)?;
            Ok(vec![config_set_string_array(
                format!("mcp_servers.{id}.enabled_tools"),
                tools,
            )])
        }
        McpConfigCommand::ToolsDisable { id, tools } => {
            validate_mcp_config_identifier(id, "MCP server id")?;
            validate_tool_names(tools)?;
            Ok(vec![config_set_string_array(
                format!("mcp_servers.{id}.disabled_tools"),
                tools,
            )])
        }
        McpConfigCommand::ToolsReset { id } => {
            validate_mcp_config_identifier(id, "MCP server id")?;
            Ok(vec![
                config_unset(format!("mcp_servers.{id}.enabled_tools")),
                config_unset(format!("mcp_servers.{id}.disabled_tools")),
            ])
        }
        McpConfigCommand::ApprovalSet { id, approval } => {
            validate_mcp_config_identifier(id, "MCP server id")?;
            validate_mcp_approval_value(approval)?;
            Ok(vec![config_set_string(
                format!("mcp_servers.{id}.approval"),
                approval,
            )])
        }
        McpConfigCommand::ApprovalUnset { id } => {
            validate_mcp_config_identifier(id, "MCP server id")?;
            Ok(vec![config_unset(format!("mcp_servers.{id}.approval"))])
        }
    }
}

/// Persists one MCP configuration command to the primary config file.
pub fn persist_mcp_config_command(
    paths: &ConfigPaths,
    command: &McpConfigCommand,
) -> Result<Vec<ConfigMutationPlan>> {
    let path = paths.ensure_default_config()?;
    mcp_config_command_mutations(command)?
        .into_iter()
        .map(|mutation| persist_config_mutation(&path, ConfigScope::Primary, mutation))
        .collect()
}

/// Summarizes a sequence of persisted MCP mutation plans.
pub fn mcp_config_command_report(plans: &[ConfigMutationPlan]) -> McpConfigCommandReport {
    McpConfigCommandReport {
        changed: plans.iter().any(|plan| plan.changed),
        reload_required: plans.iter().any(|plan| plan.reload_required),
    }
}

/// Parses a user-facing MCP setting name.
pub fn mcp_config_setting_from_user(value: &str) -> Result<McpConfigSetting> {
    match value {
        "name" => Ok(McpConfigSetting::Name),
        "cwd" => Ok(McpConfigSetting::Cwd),
        "startup-timeout-ms" | "startup_timeout_ms" => Ok(McpConfigSetting::StartupTimeoutMs),
        "tool-timeout-ms" | "tool_timeout_ms" => Ok(McpConfigSetting::ToolTimeoutMs),
        "bearer-token-env" | "bearer_token_env" => Ok(McpConfigSetting::BearerTokenEnv),
        _ => Err(MezError::invalid_args(
            "MCP setting must be name, cwd, startup-timeout-ms, tool-timeout-ms, or bearer-token-env",
        )),
    }
}

fn mcp_config_setting_mutation(
    id: &str,
    setting: McpConfigSetting,
    value: &str,
) -> Result<ConfigMutation> {
    let path = format!("mcp_servers.{id}.{}", mcp_config_setting_key(setting));
    match setting {
        McpConfigSetting::Name | McpConfigSetting::Cwd => Ok(config_set_string(path, value)),
        McpConfigSetting::BearerTokenEnv => {
            validate_mcp_config_identifier(value, "MCP bearer token environment variable")?;
            Ok(config_set_string(path, value))
        }
        McpConfigSetting::StartupTimeoutMs | McpConfigSetting::ToolTimeoutMs => value
            .parse::<i64>()
            .map_err(|_| MezError::invalid_args("MCP timeout value must be an integer"))
            .and_then(|timeout| {
                if timeout < 0 {
                    Err(MezError::invalid_args(
                        "MCP timeout value must not be negative",
                    ))
                } else {
                    Ok(config_set_integer(path, timeout))
                }
            }),
    }
}

fn mcp_config_setting_key(setting: McpConfigSetting) -> &'static str {
    match setting {
        McpConfigSetting::Name => "name",
        McpConfigSetting::Cwd => "cwd",
        McpConfigSetting::StartupTimeoutMs => "startup_timeout_ms",
        McpConfigSetting::ToolTimeoutMs => "tool_timeout_ms",
        McpConfigSetting::BearerTokenEnv => "bearer_token_env",
    }
}

fn validate_mcp_approval_value(value: &str) -> Result<()> {
    if matches!(value, "inherit" | "prompt" | "allow" | "deny") {
        Ok(())
    } else {
        Err(MezError::invalid_args(
            "MCP approval must be inherit, prompt, allow, or deny",
        ))
    }
}

fn validate_tool_names(tools: &[String]) -> Result<()> {
    if tools.is_empty() {
        return Err(MezError::invalid_args(
            "mcp tools command requires at least one tool",
        ));
    }
    for tool in tools {
        if tool.trim().is_empty() {
            return Err(MezError::invalid_args("MCP tool name must not be empty"));
        }
    }
    Ok(())
}

fn config_set_string(path: impl Into<String>, value: impl Into<String>) -> ConfigMutation {
    ConfigMutation {
        path: path.into(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::String(value.into())),
    }
}

fn config_set_integer(path: impl Into<String>, value: i64) -> ConfigMutation {
    ConfigMutation {
        path: path.into(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::Integer(value)),
    }
}

fn config_set_boolean(path: impl Into<String>, value: bool) -> ConfigMutation {
    ConfigMutation {
        path: path.into(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::Boolean(value)),
    }
}

fn config_set_string_array(path: impl Into<String>, values: &[String]) -> ConfigMutation {
    ConfigMutation {
        path: path.into(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(values.to_vec())),
    }
}

fn config_unset(path: impl Into<String>) -> ConfigMutation {
    ConfigMutation {
        path: path.into(),
        operation: ConfigMutationOperation::Unset,
    }
}
