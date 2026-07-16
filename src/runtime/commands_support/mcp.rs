//! Runtime command helpers for live runtime configuration commands.
//!
//! This module owns shared helpers for terminal-command paths that apply live
//! override mutations, persisted MCP configuration mutations, MCP retry, and
//! provider information refreshes.

use std::path::PathBuf;

use super::{
    CommandInvocation, ConfigMutation, ConfigMutationOperation, ConfigMutationValue, EventKind,
    MezError, Result, RuntimePersistedConfigMutationBatchReport, RuntimeSessionService,
    json_escape, runtime_apply_persisted_config_mutation_batch, runtime_config_apply_event_payload,
    runtime_parse_approval_policy, runtime_plan_live_override_mutation, runtime_positional_args,
    runtime_store_live_override_plan,
};
use crate::mcp::{
    McpConfigCommandReport, mcp_config_command_display, mcp_config_command_from_words,
    mcp_config_command_mutations,
};
use crate::runtime::{ConfigPaths, ConfigScope, RuntimeMcpRetryReport};
use mez_agent::mcp::{McpApprovalSetting, McpServerKind, McpServerStatus};

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

/// Executes the live `mcp` terminal command family.
pub(super) fn runtime_mcp_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let args = invocation.args.clone();
    match args.first().map(String::as_str) {
        Some("list") | None => Ok(runtime_mcp_list_display(service)),
        Some("inspect") => runtime_mcp_inspect_display(service, args.get(1).map(String::as_str)),
        Some("retry") => runtime_mcp_retry_command(service, args.get(1).map(String::as_str)),
        _ => runtime_mcp_config_command(service, &args),
    }
}

/// Builds a compact line-oriented MCP server display for live terminal output.
fn runtime_mcp_list_display(service: &RuntimeSessionService) -> String {
    let servers = service.mcp_registry().list_servers();
    if servers.is_empty() {
        return "servers=0 source=runtime-mcp".to_string();
    }
    let mut lines = vec![format!("servers={} source=runtime-mcp", servers.len())];
    for server in servers {
        lines.push(format!(
            "server={} name={} enabled={} status={} tools={}",
            json_escape(&server.configured.id),
            json_escape(&server.configured.name),
            server.configured.enabled,
            runtime_mcp_server_status_name(server.status),
            server.tools.len()
        ));
    }
    lines.join("\n")
}

/// Builds one live MCP server inspection display.
fn runtime_mcp_inspect_display(
    service: &RuntimeSessionService,
    server_id: Option<&str>,
) -> Result<String> {
    let server_id =
        server_id.ok_or_else(|| MezError::invalid_args("mcp inspect requires a server id"))?;
    let server = service
        .mcp_registry()
        .list_servers()
        .into_iter()
        .find(|server| server.configured.id == server_id)
        .ok_or_else(|| {
            MezError::new(crate::error::MezErrorKind::NotFound, "MCP server not found")
        })?;
    let transport = match server.configured.kind {
        McpServerKind::Stdio => "stdio",
        McpServerKind::Http => "http",
    };
    Ok(format!(
        "server={} name={} transport={} enabled={} status={} tools={} enabled_tools={} disabled_tools={} approval={}",
        json_escape(&server.configured.id),
        json_escape(&server.configured.name),
        transport,
        server.configured.enabled,
        runtime_mcp_server_status_name(server.status),
        server.tools.len(),
        server.configured.enabled_tools.len(),
        server.configured.disabled_tools.len(),
        runtime_mcp_approval_setting_name(server.configured.approval)
    ))
}

/// Retries one live MCP server and records the runtime lifecycle event.
fn runtime_mcp_retry_command(
    service: &mut RuntimeSessionService,
    server_id: Option<&str>,
) -> Result<String> {
    let server_id =
        server_id.ok_or_else(|| MezError::invalid_args("mcp retry requires a server id"))?;
    let report = service.retry_runtime_mcp_server(server_id)?;
    let payload = runtime_mcp_retry_event_payload("terminal/command:mcp", &report);
    service.append_lifecycle_event(EventKind::McpServerChanged, payload)?;
    Ok(runtime_mcp_retry_display(&report))
}

/// Persists one MCP config mutation command through the live runtime config path.
fn runtime_mcp_config_command(
    service: &mut RuntimeSessionService,
    args: &[String],
) -> Result<String> {
    let command = mcp_config_command_from_words(args)?;
    let mutations = mcp_config_command_mutations(&command)?;
    let path = runtime_mcp_primary_config_path(service)?;
    let batch = runtime_apply_persisted_config_mutation_batch(
        service,
        path,
        &mutations,
        "terminal/command:mcp",
    )?;
    Ok(mcp_config_command_display(
        &command,
        runtime_mcp_config_report(batch),
    ))
}

/// Finds or creates the primary config file for live MCP config commands.
fn runtime_mcp_primary_config_path(service: &RuntimeSessionService) -> Result<PathBuf> {
    if let Some(path) = service
        .config_layers()
        .iter()
        .find(|layer| layer.scope == ConfigScope::Primary && layer.path.is_some())
        .and_then(|layer| layer.path.clone())
    {
        return Ok(path);
    }
    let root = service
        .integration
        .config_root()
        .ok_or_else(|| MezError::invalid_state("mcp config command requires a config root"))?;
    ConfigPaths::from_root(root.to_path_buf()).ensure_default_config()
}

/// Returns the stable display name for an MCP server status.
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

/// Returns the stable display name for an MCP approval setting.
fn runtime_mcp_approval_setting_name(setting: McpApprovalSetting) -> &'static str {
    match setting {
        McpApprovalSetting::Inherit => "inherit",
        McpApprovalSetting::Prompt => "prompt",
        McpApprovalSetting::Allow => "allow",
        McpApprovalSetting::Deny => "deny",
    }
}

/// Converts runtime config batch metadata into the shared MCP display report.
fn runtime_mcp_config_report(
    batch: RuntimePersistedConfigMutationBatchReport,
) -> McpConfigCommandReport {
    McpConfigCommandReport {
        changed: batch.changed,
        reload_required: batch.reload_required,
    }
}

/// Renders retry metadata for terminal-command display.
fn runtime_mcp_retry_display(report: &RuntimeMcpRetryReport) -> String {
    format!(
        "server={}:action=retry:previous_status={}:status={}:retryable_before_retry={}:rediscovered={}:tools={}:reason={}",
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
    caller_client_id: Option<&mez_core::ids::ClientId>,
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
