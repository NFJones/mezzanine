//! Audit wrappers for MCP tool calls.
//!
//! Audit helpers record start and completion events around stdio and HTTP MCP
//! tool calls while preserving the underlying transport error/result behavior.

use std::collections::BTreeMap;

use crate::audit::{AuditActor, AuditLog, AuditRecord};
use crate::error::Result;

use super::stdio::McpStdioConnection;
use super::streamable_http::call_streamable_http_mcp_tool;
use mez_agent::mcp::{McpStartupPlan, McpToolCallPlan, McpToolCallResponse};

/// Runs the call stdio mcp tool with audit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn call_stdio_mcp_tool_with_audit(
    connection: &mut McpStdioConnection,
    plan: &McpToolCallPlan,
    audit_log: &mut AuditLog,
    session_id: &str,
    actor: AuditActor,
    call_id: &str,
) -> Result<McpToolCallResponse> {
    let started = AuditRecord::mcp_call(
        session_id,
        actor.clone(),
        &plan.server_id,
        &plan.tool_name,
        call_id,
        &plan.arguments_json,
        "started",
    );
    audit_log.append(started)?;

    let result = connection.call_tool(plan).await;
    let outcome = match &result {
        Ok(response) if response.is_error => "tool_error",
        Ok(_) => "succeeded",
        Err(_) => "failed",
    };
    let completed = AuditRecord::mcp_call(
        session_id,
        actor,
        &plan.server_id,
        &plan.tool_name,
        call_id,
        &plan.arguments_json,
        outcome,
    );
    audit_log.append(completed)?;
    result
}

/// Carries Mcp Tool Audit Call Context state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub struct McpToolAuditCallContext<'a> {
    /// Stores the audit log value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub audit_log: &'a mut AuditLog,
    /// Stores the mezzanine session id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mezzanine_session_id: &'a str,
    /// Stores the actor value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub actor: AuditActor,
    /// Stores the call id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub call_id: &'a str,
}

/// Runs the call streamable http mcp tool with audit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn call_streamable_http_mcp_tool_with_audit(
    plan: &McpStartupPlan,
    environment: &BTreeMap<String, String>,
    tool_call: &McpToolCallPlan,
    request_id: u64,
    session_id: Option<&str>,
    audit: McpToolAuditCallContext<'_>,
) -> Result<McpToolCallResponse> {
    let started = AuditRecord::mcp_call(
        audit.mezzanine_session_id,
        audit.actor.clone(),
        &tool_call.server_id,
        &tool_call.tool_name,
        audit.call_id,
        &tool_call.arguments_json,
        "started",
    );
    audit.audit_log.append(started)?;

    let result =
        call_streamable_http_mcp_tool(plan, environment, tool_call, request_id, session_id).await;
    let outcome = match &result {
        Ok(response) if response.is_error => "tool_error",
        Ok(_) => "succeeded",
        Err(_) => "failed",
    };
    let completed = AuditRecord::mcp_call(
        audit.mezzanine_session_id,
        audit.actor,
        &tool_call.server_id,
        &tool_call.tool_name,
        audit.call_id,
        &tool_call.arguments_json,
        outcome,
    );
    audit.audit_log.append(completed)?;
    result
}
