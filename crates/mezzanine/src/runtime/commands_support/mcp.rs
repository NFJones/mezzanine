//! Runtime command helpers for live runtime configuration commands.
//!
//! This module owns shared helpers for terminal-command paths that apply live
//! override mutations, persisted MCP configuration mutations, MCP retry, and
//! provider information refreshes.

use super::{
    ConfigMutation, ConfigMutationOperation, ConfigMutationValue, EventKind, Result,
    RuntimeSessionService, json_escape, runtime_config_apply_event_payload,
    runtime_parse_approval_policy, runtime_plan_live_override_mutation,
    runtime_store_live_override_plan,
};
use crate::runtime::RuntimeMcpRetryReport;

/// Runs the runtime mcp retry event payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_mcp_retry_event_payload(
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
pub(crate) fn runtime_apply_live_override_mutations(
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
