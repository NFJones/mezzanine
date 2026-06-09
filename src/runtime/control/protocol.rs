//! Runtime control protocol formatting helpers.
//!
//! This module owns small JSON formatting, request-parameter validation, MMP
//! response classification, and client descriptor naming helpers used by the
//! runtime control adapter. Keeping these helpers separate prevents the main
//! control dispatcher from accumulating every protocol-adjacent utility.

use super::super::{
    ClientRole, ClientState, MezError, PaneId, Path, Result, json_escape,
    runtime_json_string_field, runtime_string_array_json, unix_seconds_to_rfc3339,
};

/// Returns whether one project-trust method is read-only for runtime dispatch.
pub(super) fn runtime_project_trust_read_method(method: &str) -> bool {
    matches!(method, "project/trust/list" | "project/trust/inspect")
}

/// Validates that state request params contain only the allowed fields.
pub(super) fn runtime_validate_state_request_params(
    params: Option<&str>,
    method: &str,
    allowed: &[&str],
) -> Result<()> {
    let Some(params) = params else {
        return Ok(());
    };
    let value = serde_json::from_str::<serde_json::Value>(params)
        .map_err(|_| MezError::invalid_args(format!("{method} params must be a JSON object")))?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{method} params must be a JSON object")))?;
    if let Some(key) = object
        .keys()
        .find(|key| !allowed.iter().any(|allowed| allowed == &key.as_str()))
    {
        return Err(MezError::invalid_args(format!(
            "{method} params contains unknown field `{key}`"
        )));
    }
    Ok(())
}

/// Formats an optional string as a JSON string literal or null.
pub(super) fn runtime_optional_string(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}

/// Returns the MMP message type from either canonical or legacy body fields.
pub(super) fn runtime_mmp_message_type(body: &str) -> Option<String> {
    runtime_json_string_field(body, "type")
        .or_else(|| runtime_json_string_field(body, "message_type"))
}

/// Returns whether an MMP response frame represents a successful response.
pub(super) fn runtime_mmp_response_succeeded(output: &[u8], max_content_length: usize) -> bool {
    crate::message::decode_mmp_frame(output, max_content_length)
        .map(|(body, _)| !body.contains(r#""type":"error""#))
        .unwrap_or(false)
}

/// Compares two paths after best-effort canonicalization.
pub(super) fn paths_equivalent(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

/// Derives the pane identity encoded by runtime-created agent ids.
pub(super) fn pane_id_from_runtime_agent_id(agent_id: &str) -> Option<PaneId> {
    agent_id
        .strip_prefix("agent-")
        .and_then(|pane_id| PaneId::parse('%', pane_id.to_string()))
}

/// Formats a snapshot resume plan as a runtime control JSON object.
pub(super) fn runtime_snapshot_resume_plan_json(plan: &crate::snapshot::LayoutLoadPlan) -> String {
    format!(
        r#"{{"session_id":"{}","window_count":{},"pane_count":{},"restart_required_panes":{},"limitations":{}}}"#,
        json_escape(&plan.session_id),
        plan.window_count,
        plan.pane_count,
        runtime_string_array_json(&plan.restart_required_panes),
        runtime_string_array_json(&plan.limitations)
    )
}

/// Extracts a snapshot id from a JSON-RPC request or returns an unknown marker.
pub(super) fn runtime_snapshot_id_from_request(request: &crate::control::JsonRpcRequest) -> String {
    request
        .params
        .as_deref()
        .and_then(|params| runtime_json_string_field(params, "snapshot_id"))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Formats a Unix timestamp as a JSON RFC3339 string.
pub(super) fn runtime_timestamp_json(value: u64) -> String {
    format!(r#""{}""#, unix_seconds_to_rfc3339(value))
}

/// Formats an optional Unix timestamp as a JSON RFC3339 string or null.
pub(super) fn runtime_optional_timestamp_json(value: Option<u64>) -> String {
    value
        .map(runtime_timestamp_json)
        .unwrap_or_else(|| "null".to_string())
}

/// Returns the control JSON name for an effective client role.
pub(super) fn runtime_client_role_name(role: ClientRole) -> &'static str {
    match role {
        ClientRole::Primary => "primary",
        ClientRole::PendingObserver => "pending_observer",
        ClientRole::Observer => "observer",
        ClientRole::Agent => "agent",
        ClientRole::Automation => "automation",
    }
}

/// Returns the control JSON name for a requested client role.
pub(super) fn runtime_client_requested_role_name(role: ClientRole) -> &'static str {
    match role {
        ClientRole::PendingObserver => "observer",
        ClientRole::Primary => "primary",
        ClientRole::Observer => "observer",
        ClientRole::Agent => "agent",
        ClientRole::Automation => "automation",
    }
}

/// Returns the control JSON name for one client lifecycle state.
pub(super) fn runtime_client_state_name(state: ClientState) -> &'static str {
    match state {
        ClientState::Attached => "attached",
        ClientState::Pending => "pending",
        ClientState::Detached => "detached",
        ClientState::Revoked => "revoked",
        ClientState::Failed => "failed",
    }
}

/// Formats an optional terminal size object for control JSON.
pub(super) fn runtime_size_object_json(size: Option<crate::layout::Size>) -> String {
    size.map(|size| format!(r#"{{"columns":{},"rows":{}}}"#, size.columns, size.rows))
        .unwrap_or_else(|| "null".to_string())
}

/// Formats an optional terminal descriptor object for control JSON.
pub(super) fn runtime_client_terminal_descriptor_json(
    size: Option<crate::layout::Size>,
    term: &str,
) -> String {
    size.map(|size| {
        format!(
            r#"{{"columns":{},"rows":{},"term":"{}"}}"#,
            size.columns,
            size.rows,
            json_escape(term)
        )
    })
    .unwrap_or_else(|| "null".to_string())
}
