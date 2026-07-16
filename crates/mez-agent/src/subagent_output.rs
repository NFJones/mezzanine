//! Subagent task-result output formatting.
//!
//! This module owns the final text delivered from child agents back to their
//! parent. It extracts useful say text and bounded action diagnostics from a
//! completed child turn without exposing raw provider envelopes or unbounded
//! terminal output.

use crate::{ActionResult, ActionStatus, AgentTurnExecution, AgentTurnState};

/// Builds the message-delivered final output for a subagent task result.
///
/// Provider raw text often contains the MAAP JSON envelope rather than useful
/// user-facing text. Parent agents should receive conversational `say` text on
/// success, concrete action diagnostics on action failure, and provider error
/// text when the failure happened before any action result existed.
pub fn subagent_task_output_for_execution(execution: &AgentTurnExecution) -> String {
    let mut lines = Vec::new();
    for result in &execution.action_results {
        if let Some(error) = &result.error {
            lines.push(format!(
                "{} {} {}: {}",
                result.action_type,
                result.action_id,
                action_status_name(result.status),
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

/// Returns the stable status name included in child failure summaries.
fn action_status_name(status: ActionStatus) -> &'static str {
    match status {
        ActionStatus::Rejected => "rejected",
        ActionStatus::Blocked => "blocked",
        ActionStatus::Denied => "denied",
        ActionStatus::Running => "running",
        ActionStatus::Succeeded => "succeeded",
        ActionStatus::Failed => "failed",
        ActionStatus::Cancelled => "cancelled",
        ActionStatus::TimedOut => "timed_out",
        ActionStatus::Interrupted => "interrupted",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AllowedActionSet, ModelInteractionKind, ModelRequest, ModelResponse, ModelTokenUsage,
    };
    use std::collections::BTreeMap;

    /// Builds a result-free execution for testing terminal output fallbacks.
    fn execution(terminal_state: AgentTurnState, raw_text: &str) -> AgentTurnExecution {
        AgentTurnExecution {
            request: ModelRequest {
                provider: "test".to_string(),
                model: "test-model".to_string(),
                reasoning_effort: None,
                thinking_enabled: None,
                latency_preference: None,
                prompt_cache_retention: None,
                max_output_tokens: None,
                temperature: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: false,
                interaction_kind: ModelInteractionKind::ActionExecution,
                allowed_actions: AllowedActionSet::say_only(),
                stop: None,
                messages: Vec::new(),
            },
            response: ModelResponse {
                provider: "test".to_string(),
                model: "test-model".to_string(),
                raw_text: raw_text.to_string(),
                usage: ModelTokenUsage::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: ModelTokenUsage::default(),
            routing_token_usage_by_model: BTreeMap::new(),
            action_results: Vec::new(),
            final_turn: true,
            terminal_state,
        }
    }

    /// Verifies successful child turns never expose a raw provider envelope.
    #[test]
    fn completed_child_without_say_uses_stable_fallback() {
        let execution = execution(AgentTurnState::Completed, "raw MAAP envelope");
        assert_eq!(
            subagent_task_output_for_execution(&execution),
            "completed without user-facing response"
        );
    }

    /// Verifies pre-action provider failures preserve their bounded diagnostic.
    #[test]
    fn failed_child_without_action_results_uses_provider_diagnostic() {
        let execution = execution(AgentTurnState::Failed, "  provider unavailable  ");
        assert_eq!(
            subagent_task_output_for_execution(&execution),
            "provider unavailable"
        );
    }

    /// Verifies child diagnostics are bounded before delivery to a parent turn.
    #[test]
    fn child_diagnostic_text_is_bounded() {
        let output = subagent_bounded_diagnostic_text(&"x".repeat(4_001));
        assert_eq!(output.chars().filter(|ch| *ch == 'x').count(), 4_000);
        assert!(output.ends_with("\n[truncated]"));
    }
}
