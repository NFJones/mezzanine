//! Neutral action presentation and duplicate-output policy.
//!
//! This module owns bounded summaries, rationale suppression, runtime-visible
//! action classification, duplicate-mutation guards, and action outcome
//! selection over canonical agent records plus explicit product-supplied
//! execution plans. It performs no terminal I/O or runtime state mutation.

use super::{runtime_action_result_is_feedback_candidate, runtime_action_status_name};
use crate::*;

/// Product-supplied action plans and display policy used by neutral action
/// presentation decisions.
///
/// The plans are explicit inputs because the composition crate validates
/// model-authored shell commands and chooses concrete execution adapters. The
/// lower crate owns every decision that depends only on those plans and the
/// canonical action/result records.
#[derive(Debug, Clone, Copy, Default)]
pub struct ActionPresentationInput<'a> {
    /// Validated local execution plan, when the action is shell-backed.
    pub local_plan: Option<&'a LocalActionPlan>,
    /// Canonical network execution plan, when the action is network-backed.
    pub network_plan: Option<&'a NetworkActionPlan>,
    /// Whether a runtime-backed action target may be shown in status output.
    pub show_runtime_target: bool,
}

/// Neutral kind and target text for an action that did not reach its normal
/// execution presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionUserPhrase {
    /// Stable user-facing action kind.
    pub kind: &'static str,
    /// Bounded single-line action target.
    pub target: String,
}

/// One neutral status line selected from an action result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionOutcomeLine {
    /// Whether the product should present the line using error styling.
    pub is_error: bool,
    /// Bounded single-line status text.
    pub line: String,
}

/// Produces a bounded single-line preview for model-authored action text.
pub fn action_terminal_preview(value: &str) -> String {
    const MAX_ACTION_PREVIEW_CHARS: usize = 240;
    let trimmed = value.trim();
    let mut preview = String::new();
    let mut chars = trimmed.chars();
    for _ in 0..MAX_ACTION_PREVIEW_CHARS {
        let Some(ch) = chars.next() else {
            return preview;
        };
        preview.push(match ch {
            '\r' | '\n' => ' ',
            ch if ch.is_control() => ' ',
            ch => ch,
        });
    }
    if chars.next().is_some() {
        preview.push_str("...");
    }
    preview
}

/// Returns the concise neutral summary for one canonical action.
pub fn action_summary(action: &AgentAction, input: ActionPresentationInput<'_>) -> Option<String> {
    match &action.payload {
        AgentActionPayload::MemorySearch { query, .. } => {
            return Some(format!("memory search {}", action_terminal_preview(query)));
        }
        AgentActionPayload::MemoryStore { kind, .. } => {
            return Some(format!("memory store {}", action_terminal_preview(kind)));
        }
        _ => {}
    }
    let summary = input
        .local_plan
        .map(|plan| plan.summary.as_str())
        .or_else(|| input.network_plan.map(|plan| plan.summary.as_str()))?;
    let summary = action_terminal_preview(summary);
    (!summary.trim().is_empty()).then_some(summary)
}

/// Normalizes one user-visible rationale or conversational action value for
/// duplicate-output comparison.
pub fn normalize_user_visible_text(value: &str) -> String {
    let trimmed = value.trim_start();
    trimmed
        .strip_prefix("agent thinking:")
        .or_else(|| trimmed.strip_prefix("thinking:"))
        .map(str::trim_start)
        .unwrap_or(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Returns whether an action rationale repeats text the same action already
/// exposes through its normal presentation.
pub fn action_rationale_repeats_visible_summary(
    action: &AgentAction,
    input: ActionPresentationInput<'_>,
) -> bool {
    let rationale = normalize_user_visible_text(&action.rationale);
    if rationale.is_empty() {
        return false;
    }
    if matches!(action.payload, AgentActionPayload::Say { .. }) {
        return true;
    }
    if !matches!(action.payload, AgentActionPayload::ShellCommand { .. })
        && let Some(summary) = action_summary(action, input)
        && rationale == normalize_user_visible_text(&summary)
    {
        return true;
    }
    match &action.payload {
        AgentActionPayload::ShellCommand { command, .. } => {
            rationale == normalize_user_visible_text(command)
        }
        AgentActionPayload::Say { text, .. }
        | AgentActionPayload::RequestCapability { reason: text, .. } => {
            rationale == normalize_user_visible_text(text)
        }
        AgentActionPayload::Abort { reason } => rationale == normalize_user_visible_text(reason),
        AgentActionPayload::McpCall { .. }
        | AgentActionPayload::SendMessage { .. }
        | AgentActionPayload::SpawnAgent { .. }
        | AgentActionPayload::ConfigChange { .. }
        | AgentActionPayload::MemorySearch { .. }
        | AgentActionPayload::MemoryStore { .. }
        | AgentActionPayload::IssueAdd { .. }
        | AgentActionPayload::IssueUpdate { .. }
        | AgentActionPayload::IssueQuery { .. }
        | AgentActionPayload::IssueDelete { .. }
        | AgentActionPayload::RequestSkills
        | AgentActionPayload::CallSkill { .. }
        | AgentActionPayload::ApplyPatch { .. }
        | AgentActionPayload::WebSearch { .. }
        | AgentActionPayload::FetchUrl { .. }
        | AgentActionPayload::Complete => false,
    }
}

/// Returns normalized conversational action text already visible for a batch.
pub fn batch_visible_action_texts(batch: &MaapBatch) -> Vec<String> {
    batch
        .actions
        .iter()
        .filter_map(|action| match &action.payload {
            AgentActionPayload::Say { text, .. } => Some(text),
            AgentActionPayload::Abort { reason } => Some(reason),
            _ => None,
        })
        .map(|text| normalize_user_visible_text(text))
        .filter(|text| !text.is_empty())
        .collect()
}

/// Returns whether a batch rationale repeats conversational text from the same
/// provider response.
pub fn batch_rationale_repeats_visible_text(batch: &MaapBatch, visible_texts: &[String]) -> bool {
    let rationale = normalize_user_visible_text(&batch.rationale);
    !rationale.is_empty() && visible_texts.iter().any(|text| text == &rationale)
}

/// Returns whether an action rationale repeats nearby conversational text.
pub fn action_rationale_repeats_visible_batch_text(
    action: &AgentAction,
    visible_texts: &[String],
) -> bool {
    let rationale = normalize_user_visible_text(&action.rationale);
    !rationale.is_empty() && visible_texts.iter().any(|text| text == &rationale)
}

/// Returns whether an action produces runtime-visible output after execution.
pub fn action_has_runtime_visible_effect(action: &AgentAction) -> bool {
    matches!(
        action.payload,
        AgentActionPayload::ShellCommand { .. }
            | AgentActionPayload::ApplyPatch { .. }
            | AgentActionPayload::WebSearch { .. }
            | AgentActionPayload::FetchUrl { .. }
            | AgentActionPayload::McpCall { .. }
            | AgentActionPayload::RequestSkills
            | AgentActionPayload::CallSkill { .. }
            | AgentActionPayload::SendMessage { .. }
            | AgentActionPayload::SpawnAgent { .. }
            | AgentActionPayload::ConfigChange { .. }
            | AgentActionPayload::MemorySearch { .. }
            | AgentActionPayload::MemoryStore { .. }
    )
}

/// Returns whether a successful duplicate action must be rejected to avoid
/// reapplying the same file mutation.
pub fn action_rejects_duplicate_success(action: &AgentAction) -> bool {
    matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
}

/// Returns whether a successful result records a suppressed duplicate file
/// mutation.
pub fn action_result_is_suppressed_duplicate_file_mutation(result: &ActionResult) -> bool {
    result.status == ActionStatus::Succeeded
        && result
            .structured_content_json
            .as_deref()
            .is_some_and(|content| content.contains("repeated_successful_file_mutation"))
}

/// Returns neutral kind and target text for a runtime-gated action.
pub fn action_user_phrase(
    action: &AgentAction,
    input: ActionPresentationInput<'_>,
) -> Option<ActionUserPhrase> {
    if let Some(plan) = input.local_plan {
        let kind = if matches!(action.payload, AgentActionPayload::ShellCommand { .. }) {
            "shell command"
        } else {
            "local action"
        };
        return Some(ActionUserPhrase {
            kind,
            target: action_terminal_preview(&plan.command),
        });
    }
    if let Some(plan) = input.network_plan {
        let kind = match action.payload {
            AgentActionPayload::WebSearch { .. } => "web search",
            AgentActionPayload::FetchUrl { .. } => "URL fetch",
            _ => "network action",
        };
        return Some(ActionUserPhrase {
            kind,
            target: action_terminal_preview(&plan.policy_command),
        });
    }
    let (kind, target) = match &action.payload {
        AgentActionPayload::McpCall { server, tool, .. } => (
            "MCP call",
            format!(
                "{}/{}",
                action_terminal_preview(server),
                action_terminal_preview(tool)
            ),
        ),
        AgentActionPayload::SendMessage { recipient, .. } => {
            ("message", action_terminal_preview(recipient))
        }
        AgentActionPayload::SpawnAgent { role, .. } => {
            ("subagent spawn", action_terminal_preview(role))
        }
        AgentActionPayload::ConfigChange {
            setting_path,
            operation,
            ..
        } => (
            "config change",
            format!(
                "{} {}",
                action_terminal_preview(operation),
                action_terminal_preview(setting_path)
            ),
        ),
        AgentActionPayload::MemorySearch { query, .. } => {
            ("memory search", action_terminal_preview(query))
        }
        AgentActionPayload::MemoryStore { kind, .. } => {
            ("memory store", action_terminal_preview(kind))
        }
        AgentActionPayload::IssueAdd { title, .. } => ("issue add", action_terminal_preview(title)),
        AgentActionPayload::IssueUpdate { id, .. } => ("issue update", action_terminal_preview(id)),
        AgentActionPayload::IssueQuery { text, .. } => (
            "issue query",
            text.as_deref()
                .map(action_terminal_preview)
                .unwrap_or_else(|| "current project".to_string()),
        ),
        AgentActionPayload::IssueDelete { id } => ("issue delete", action_terminal_preview(id)),
        AgentActionPayload::RequestSkills => ("skill lookup", "available skills".to_string()),
        AgentActionPayload::CallSkill { name, .. } => ("skill load", action_terminal_preview(name)),
        AgentActionPayload::Say { .. }
        | AgentActionPayload::RequestCapability { .. }
        | AgentActionPayload::Complete
        | AgentActionPayload::Abort { .. }
        | AgentActionPayload::ShellCommand { .. }
        | AgentActionPayload::ApplyPatch { .. }
        | AgentActionPayload::WebSearch { .. }
        | AgentActionPayload::FetchUrl { .. } => return None,
    };
    Some(ActionUserPhrase { kind, target })
}

/// Formats one bounded error suffix from an action result.
pub fn action_error_suffix(result: &ActionResult) -> String {
    let detail = result
        .error
        .as_ref()
        .map(|error| {
            if error.code.trim().is_empty() {
                error.message.clone()
            } else if error.message.trim().is_empty() {
                error.code.clone()
            } else {
                format!("{}: {}", error.code, error.message)
            }
        })
        .or_else(|| {
            let content = result.content_text();
            (!content.trim().is_empty()).then_some(content)
        })
        .map(|detail| action_terminal_preview(&detail))
        .unwrap_or_default();
    if detail.is_empty() {
        String::new()
    } else {
        format!(" ({detail})")
    }
}

/// Returns a compact HTTP status label from fetch structured content.
pub fn fetch_url_status_label(result: &ActionResult) -> Option<String> {
    let value: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref()?).ok()?;
    let status = value
        .get("response")
        .and_then(|response| response.get("status_code"))
        .and_then(serde_json::Value::as_u64)?;
    Some(format!("HTTP {status}"))
}

/// Builds a terse warning for a recoverable URL-fetch failure.
pub fn recoverable_network_warning_line(
    action: &AgentAction,
    result: &ActionResult,
) -> Option<String> {
    if !matches!(action.payload, AgentActionPayload::FetchUrl { .. })
        || !runtime_action_result_is_feedback_candidate(result)
    {
        return None;
    }
    let detail = fetch_url_status_label(result)
        .or_else(|| {
            result
                .error
                .as_ref()
                .map(|error| action_terminal_preview(&error.message))
        })
        .filter(|detail| !detail.trim().is_empty())
        .map(|detail| format!(" ({detail})"))
        .unwrap_or_default();
    Some(format!(
        "agent warning: URL fetch failed{detail}; model received the response details for recovery"
    ))
}

/// Selects one neutral status line for an action that could not reach its
/// normal visible execution path.
pub fn action_outcome_line(
    action: &AgentAction,
    result: &ActionResult,
    input: ActionPresentationInput<'_>,
) -> Option<ActionOutcomeLine> {
    let ActionUserPhrase { kind, target } = action_user_phrase(action, input)?;
    let runtime_owned_action = input.local_plan.is_some() || input.network_plan.is_some();
    let target = if runtime_owned_action && !input.show_runtime_target {
        None
    } else {
        Some(target)
    };
    match result.status {
        ActionStatus::Blocked => Some(ActionOutcomeLine {
            is_error: false,
            line: if let Some(summary) = action_summary(action, input).filter(|_| target.is_none())
            {
                format!("agent: {summary} (awaiting approval)")
            } else if let Some(target) = target {
                format!("agent: {kind} awaiting approval: {target}")
            } else {
                format!("agent: {kind} awaiting approval")
            },
        }),
        ActionStatus::Rejected
        | ActionStatus::Denied
        | ActionStatus::Failed
        | ActionStatus::Cancelled
        | ActionStatus::TimedOut
        | ActionStatus::Interrupted => {
            if let Some(line) = recoverable_network_warning_line(action, result) {
                return Some(ActionOutcomeLine {
                    is_error: false,
                    line,
                });
            }
            let detail = action_error_suffix(result);
            let failure_phase = if input.network_plan.is_some() && result.content.is_empty() {
                ""
            } else {
                " before execution"
            };
            let line =
                if let Some(summary) = action_summary(action, input).filter(|_| target.is_none()) {
                    format!(
                        "agent: {summary} ({kind} {}{failure_phase}{detail})",
                        runtime_action_status_name(result.status)
                    )
                } else if let Some(target) = target {
                    format!(
                        "agent: {kind} {}{failure_phase}: {target}{detail}",
                        runtime_action_status_name(result.status),
                    )
                } else {
                    format!(
                        "agent: {kind} {}{failure_phase}{detail}",
                        runtime_action_status_name(result.status),
                    )
                };
            Some(ActionOutcomeLine {
                is_error: true,
                line,
            })
        }
        ActionStatus::Running | ActionStatus::Succeeded => None,
    }
}
