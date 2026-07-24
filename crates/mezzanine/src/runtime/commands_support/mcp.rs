//! Runtime command helpers for live runtime configuration commands.
//!
//! This module owns shared helpers for terminal-command paths that apply live
//! override mutations, persisted MCP configuration mutations, MCP retry, and
//! provider information refreshes.

use super::json_escape;
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
