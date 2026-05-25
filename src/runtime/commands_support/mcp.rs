//! Runtime command helpers for MCP server configuration and retry commands.
//!
//! This module owns the terminal-command surface that mutates MCP server
//! configuration, applies live override mutations, reports MCP retry outcomes,
//! and validates MCP command arguments. The facade re-exports these helpers so
//! existing runtime command dispatch paths keep their stable names.

use super::*;
use crate::runtime::RuntimeMcpRetryReport;

/// Adds an MCP server through the runtime live configuration layer and reloads MCP state.
pub(in crate::runtime) fn runtime_mcp_add_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let (server_id, transport, target, mutations) = runtime_mcp_add_mutations(invocation)?;

    let (changed, reload_required, report) =
        runtime_apply_live_override_mutations(service, mutations)?;
    service.append_lifecycle_event(
        EventKind::ConfigChanged,
        runtime_config_apply_event_payload("terminal/command:mcp-add", &report),
    )?;
    let (status, tool_count) =
        runtime_mcp_server_status_for_display(service, &server_id).unwrap_or(("missing", 0));
    Ok(format!(
        "server={server_id}:transport={transport}:target={}:changed={changed}:reload_required={reload_required}:status={status}:tools={tool_count}:source=runtime-config:layer={TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER}",
        json_escape(&target)
    ))
}

/// Refreshes provider information through the async runtime command path.
///
/// The command intentionally owns live provider discovery so ordinary pane
/// creation, selector opening, and `/model list` rendering can use cached or
/// configured information without making provider calls on the hot path.
pub(super) async fn runtime_refresh_provider_info_command_async(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let args = runtime_positional_args(invocation);
    if !args.is_empty() {
        return Err(MezError::invalid_args(
            "refresh-provider-info does not accept positional arguments",
        ));
    }
    service.refresh_provider_info_async().await
}

/// Runs the runtime mcp add command async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) async fn runtime_mcp_add_command_async(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let (server_id, transport, target, mutations) = runtime_mcp_add_mutations(invocation)?;

    let (changed, reload_required, report) =
        runtime_apply_live_override_mutations_async(service, mutations).await?;
    service.append_lifecycle_event(
        EventKind::ConfigChanged,
        runtime_config_apply_event_payload("terminal/command:mcp-add", &report),
    )?;
    let (status, tool_count) =
        runtime_mcp_server_status_for_display(service, &server_id).unwrap_or(("missing", 0));
    Ok(format!(
        "server={server_id}:transport={transport}:target={}:changed={changed}:reload_required={reload_required}:status={status}:tools={tool_count}:source=runtime-config:layer={TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER}",
        json_escape(&target)
    ))
}

/// Runs the runtime mcp add mutations operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_mcp_add_mutations(
    invocation: &CommandInvocation,
) -> Result<(String, String, String, Vec<ConfigMutation>)> {
    let server_id = runtime_mcp_server_id(invocation, "mcp-add requires a server id")?.to_string();
    let (transport, target) = runtime_mcp_transport_target(invocation)?;
    let transport = transport.to_string();
    let target = target.to_string();
    let arg_values = runtime_repeated_flag_values(&invocation.args, "--arg");
    let mut mutations = vec![ConfigMutation {
        path: format!("mcp_servers.{server_id}.enabled"),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::Boolean(true)),
    }];

    match transport.as_str() {
        "stdio" => {
            mutations.push(ConfigMutation {
                path: format!("mcp_servers.{server_id}.command"),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::String(
                    target.clone(),
                )),
            });
            mutations.push(ConfigMutation {
                path: format!("mcp_servers.{server_id}.args"),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(
                    arg_values,
                )),
            });
            mutations.push(ConfigMutation {
                path: format!("mcp_servers.{server_id}.url"),
                operation: ConfigMutationOperation::Unset,
            });
        }
        "streamable-http" => {
            mutations.push(ConfigMutation {
                path: format!("mcp_servers.{server_id}.url"),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::String(
                    target.clone(),
                )),
            });
            mutations.push(ConfigMutation {
                path: format!("mcp_servers.{server_id}.command"),
                operation: ConfigMutationOperation::Unset,
            });
            mutations.push(ConfigMutation {
                path: format!("mcp_servers.{server_id}.args"),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(
                    Vec::new(),
                )),
            });
        }
        _ => unreachable!("MCP transport parsing returns a known transport"),
    }

    Ok((server_id, transport, target, mutations))
}

/// Removes an MCP server through the runtime live configuration layer and reloads MCP state.
pub(in crate::runtime) fn runtime_mcp_remove_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let server_id = runtime_mcp_server_id(invocation, "mcp-remove requires a server id")?;
    let (changed, reload_required, report) = runtime_apply_live_override_mutations(
        service,
        vec![ConfigMutation {
            path: format!("mcp_servers.{server_id}"),
            operation: ConfigMutationOperation::Unset,
        }],
    )?;
    service.append_lifecycle_event(
        EventKind::ConfigChanged,
        runtime_config_apply_event_payload("terminal/command:mcp-remove", &report),
    )?;
    let removed = runtime_mcp_server_status_for_display(service, server_id).is_none();
    Ok(format!(
        "server={server_id}:removed={removed}:changed={changed}:reload_required={reload_required}:source=runtime-config:layer={TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER}"
    ))
}

/// Runs the runtime mcp remove command async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) async fn runtime_mcp_remove_command_async(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let server_id = runtime_mcp_server_id(invocation, "mcp-remove requires a server id")?;
    let (changed, reload_required, report) = runtime_apply_live_override_mutations_async(
        service,
        vec![ConfigMutation {
            path: format!("mcp_servers.{server_id}"),
            operation: ConfigMutationOperation::Unset,
        }],
    )
    .await?;
    service.append_lifecycle_event(
        EventKind::ConfigChanged,
        runtime_config_apply_event_payload("terminal/command:mcp-remove", &report),
    )?;
    let removed = runtime_mcp_server_status_for_display(service, server_id).is_none();
    Ok(format!(
        "server={server_id}:removed={removed}:changed={changed}:reload_required={reload_required}:source=runtime-config:layer={TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER}"
    ))
}

/// Retries an MCP server that was unavailable or session-blacklisted.
pub(in crate::runtime) fn runtime_mcp_retry_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let server_id = runtime_mcp_server_id(invocation, "mcp-retry requires a server id")?;
    let report = service.retry_runtime_mcp_server(server_id)?;
    service.append_lifecycle_event(
        EventKind::McpServerChanged,
        runtime_mcp_retry_event_payload("terminal/command:mcp-retry", &report),
    )?;
    Ok(runtime_mcp_retry_display(&report))
}

/// Runs the runtime mcp retry command async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) async fn runtime_mcp_retry_command_async(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let server_id = runtime_mcp_server_id(invocation, "mcp-retry requires a server id")?;
    let report = service.retry_runtime_mcp_server_async(server_id).await?;
    service.append_lifecycle_event(
        EventKind::McpServerChanged,
        runtime_mcp_retry_event_payload("terminal/command:mcp-retry", &report),
    )?;
    Ok(runtime_mcp_retry_display(&report))
}

/// Runs the runtime mcp retry display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_mcp_retry_display(report: &RuntimeMcpRetryReport) -> String {
    format!(
        "server={}:retried=true:previous_status={}:status={}:retryable_before_retry={}:rediscovered={}:tools={}:reason={}:source=runtime-mcp",
        json_escape(&report.server_id),
        report.previous_status_name(),
        report.status_name(),
        report.retryable_before_retry,
        report.rediscovered,
        report.tools,
        report
            .reason
            .as_deref()
            .map(json_escape)
            .unwrap_or_else(|| "none".to_string())
    )
}

/// Runs the runtime mcp retry event payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_mcp_retry_event_payload(
    source: &str,
    report: &RuntimeMcpRetryReport,
) -> String {
    format!(
        r#"{{"source":"{}","server_id":"{}","previous_status":"{}","status":"{}","retryable_before_retry":{},"rediscovered":{},"tools":{},"reason":{}}}"#,
        json_escape(source),
        json_escape(&report.server_id),
        report.previous_status_name(),
        report.status_name(),
        report.retryable_before_retry,
        report.rediscovered,
        report.tools,
        report
            .reason
            .as_deref()
            .map(|reason| format!(r#""{}""#, json_escape(reason)))
            .unwrap_or_else(|| "null".to_string())
    )
}

/// Applies a sequence of live config mutations before reloading runtime state once.
pub(in crate::runtime) fn runtime_apply_live_override_mutations(
    service: &mut RuntimeSessionService,
    mutations: Vec<ConfigMutation>,
) -> Result<(bool, bool, crate::runtime::RuntimeConfigApplyReport)> {
    let mut changed = false;
    let mut reload_required = false;
    for mutation in mutations {
        let plan = runtime_plan_live_override_mutation(service, mutation)?;
        changed |= plan.changed;
        reload_required |= plan.reload_required;
        runtime_store_live_override_plan(service, &plan.text);
    }
    let report = service.apply_runtime_config_layers()?;
    Ok((changed, reload_required, report))
}

/// Applies one permission-policy live override from a runtime command.
///
/// # Parameters
/// - `service`: Runtime service whose live config layer receives the change.
/// - `path`: Dotted configuration path under the permission policy surface.
/// - `value`: String value to store for the permission policy field.
/// - `event_source`: Lifecycle event source describing the command path.
pub(super) fn runtime_apply_permission_live_override(
    service: &mut RuntimeSessionService,
    caller_client_id: Option<&crate::ids::ClientId>,
    path: &str,
    value: &str,
    event_source: &str,
) -> Result<()> {
    let previous_permission_policy = service.permission_policy().clone();
    let (changed, _, report) = runtime_apply_live_override_mutations(
        service,
        vec![ConfigMutation {
            path: path.to_string(),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::String(value.to_string())),
        }],
    )?;
    if path == "permissions.approval_policy"
        && let Ok(policy) = runtime_parse_approval_policy(value)
    {
        service.set_live_approval_policy_override(policy);
    }
    if changed {
        service.append_lifecycle_event(
            EventKind::ConfigChanged,
            runtime_config_apply_event_payload(event_source, &report),
        )?;
    }
    service.reconcile_pending_agent_approvals_after_permission_change(
        caller_client_id,
        &previous_permission_policy,
        event_source,
    )?;
    Ok(())
}

/// Runs the runtime apply live override mutations async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) async fn runtime_apply_live_override_mutations_async(
    service: &mut RuntimeSessionService,
    mutations: Vec<ConfigMutation>,
) -> Result<(bool, bool, crate::runtime::RuntimeConfigApplyReport)> {
    let mut changed = false;
    let mut reload_required = false;
    for mutation in mutations {
        let plan = runtime_plan_live_override_mutation(service, mutation)?;
        changed |= plan.changed;
        reload_required |= plan.reload_required;
        runtime_store_live_override_plan(service, &plan.text);
    }
    let report = service.apply_runtime_config_layers_async().await?;
    Ok((changed, reload_required, report))
}

/// Returns a configured MCP server's display status and tool count.
pub(in crate::runtime) fn runtime_mcp_server_status_for_display(
    service: &RuntimeSessionService,
    server_id: &str,
) -> Option<(&'static str, usize)> {
    service
        .mcp_registry()
        .list_servers()
        .into_iter()
        .find(|server| server.configured.id == server_id)
        .map(|server| {
            (
                runtime_mcp_server_status_name(server.status),
                server.tools.len(),
            )
        })
}

/// Runs the runtime mcp server status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_mcp_server_status_name(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Configured => "configured",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

/// Parses and validates the server id argument for live MCP command mutations.
pub(in crate::runtime) fn runtime_mcp_server_id<'a>(
    invocation: &'a CommandInvocation,
    missing: &str,
) -> Result<&'a str> {
    let mut index = 0;
    while index < invocation.args.len() {
        let arg = invocation.args[index].as_str();
        if matches!(arg, "--command" | "--url" | "--arg") {
            index = index.saturating_add(2);
            continue;
        }
        if arg.starts_with('-') {
            index = index.saturating_add(1);
            continue;
        }
        runtime_validate_config_identifier(arg, "MCP server id")?;
        return Ok(arg);
    }
    Err(MezError::invalid_args(missing))
}

/// Parses the requested MCP transport from mutually exclusive command flags.
pub(in crate::runtime) fn runtime_mcp_transport_target(
    invocation: &CommandInvocation,
) -> Result<(&'static str, &str)> {
    let command = runtime_flag_value(&invocation.args, "--command");
    let url = runtime_flag_value(&invocation.args, "--url");
    if command.is_some() == url.is_some() {
        return Err(MezError::invalid_args(
            "mcp-add requires exactly one of --command or --url",
        ));
    }
    Ok(match (command, url) {
        (Some(command), None) => ("stdio", command),
        (None, Some(url)) => ("streamable-http", url),
        _ => unreachable!("validated exactly one transport target"),
    })
}

/// Collects repeated flag values such as `--arg VALUE`.
pub(in crate::runtime) fn runtime_repeated_flag_values(args: &[String], flag: &str) -> Vec<String> {
    args.windows(2)
        .filter(|window| window.first().is_some_and(|arg| arg == flag))
        .filter_map(|window| window.get(1))
        .map(ToString::to_string)
        .collect()
}

/// Validates dotted-config identifiers used inside live config mutation paths.
pub(in crate::runtime) fn runtime_validate_config_identifier(
    value: &str,
    label: &str,
) -> Result<()> {
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
