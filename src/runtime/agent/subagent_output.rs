//! Subagent task-result output formatting.
//!
//! This module owns the final text delivered from child agents back to their
//! parent. It extracts useful say text and bounded action diagnostics from a
//! completed child turn without exposing raw provider envelopes or unbounded
//! terminal output.

use super::*;

/// Builds the message-delivered final output for a subagent task result.
///
/// Provider raw text often contains the MAAP JSON envelope rather than useful
/// user-facing text. Parent agents should receive conversational `say` text on
/// success, concrete action diagnostics on action failure, and provider error
/// text when the failure happened before any action result existed.
pub(super) fn subagent_task_output_for_execution(execution: &AgentTurnExecution) -> String {
    let mut lines = Vec::new();
    for result in &execution.action_results {
        if let Some(error) = &result.error {
            lines.push(format!(
                "{} {} {}: {}",
                result.action_type,
                result.action_id,
                runtime_action_status_name(result.status),
                error.message
            ));
            lines.extend(subagent_failed_action_diagnostic_lines(result));
            continue;
        }
        if result.action_type == "say" {
            lines.extend(
                result
                    .content_texts()
                    .into_iter()
                    .filter(|text| !text.trim().is_empty()),
            );
        }
    }

    if !lines.is_empty() {
        return lines.join("\n");
    }
    if execution.terminal_state == AgentTurnState::Completed {
        "completed without user-facing response".to_string()
    } else if !execution.response.raw_text.trim().is_empty() {
        execution.response.raw_text.trim().to_string()
    } else {
        "failed without action diagnostics".to_string()
    }
}

/// Returns bounded diagnostic lines for a failed subagent action result.
///
/// Parent agents rely on final `task_result` payloads to understand why a child
/// failed. Shell-backed semantic actions often store their useful stderr/stdout
/// preview in structured content rather than in plain content, so this extracts
/// both locations without exposing unbounded terminal output.
fn subagent_failed_action_diagnostic_lines(result: &ActionResult) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(command) = subagent_action_result_structured_string(result, &["command"]) {
        lines.push(format!(
            "{} {} command: {}",
            result.action_type,
            result.action_id,
            subagent_bounded_diagnostic_text(command.trim())
        ));
    }
    let output = result
        .content_texts()
        .into_iter()
        .find(|text| !text.trim().is_empty())
        .or_else(|| {
            subagent_action_result_structured_string(
                result,
                &["terminal_observation", "combined_output_preview"],
            )
            .filter(|text| !text.trim().is_empty())
        });
    if let Some(output) = output {
        lines.push(format!(
            "{} {} output:\n{}",
            result.action_type,
            result.action_id,
            subagent_bounded_diagnostic_text(output.trim())
        ));
    }
    lines
}

/// Extracts a string from nested action-result structured content.
///
/// # Parameters
/// - `result`: Action result containing optional structured JSON.
/// - `path`: Ordered object keys to traverse.
fn subagent_action_result_structured_string(
    result: &ActionResult,
    path: &[&str],
) -> Option<String> {
    let mut value: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref()?).ok()?;
    for key in path {
        value = value.get(*key)?.clone();
    }
    value.as_str().map(str::to_string)
}

/// Bounds diagnostic text included in subagent task results.
///
/// # Parameters
/// - `value`: Unbounded diagnostic text from action output or structured data.
fn subagent_bounded_diagnostic_text(value: &str) -> String {
    const MAX_SUBAGENT_DIAGNOSTIC_CHARS: usize = 4_000;
    let mut output = value
        .chars()
        .take(MAX_SUBAGENT_DIAGNOSTIC_CHARS)
        .collect::<String>();
    if value.chars().count() > MAX_SUBAGENT_DIAGNOSTIC_CHARS {
        output.push_str("\n[truncated]");
    }
    output
}
