//! Runtime agent trace and debug-log presentation helpers.
//!
//! This module owns JSON shaping and redaction for model requests, provider
//! responses, MAAP actions, action results, provider errors, and per-pane trace
//! logs. Runtime state transitions stay in the parent agent service module.

use super::outcome::{runtime_agent_terminal_preview, runtime_humanize_agent_diagnostic};
use super::{runtime_action_status_name, runtime_mezzanine_error_code};
use crate::agent::{
    AgentContext, AgentTurnRecord, ContextSourceKind, ModelMessageRole, ModelProfile, ModelRequest,
    ModelResponse, OpenAiPromptCacheDiagnostics, apply_default_action_gates,
    assemble_model_request_with_retained_tail_percent, openai_prompt_cache_diagnostics_for_request,
};
use crate::error::{MezError, Result};
use crate::runtime::{RuntimeSessionService, runtime_agent_turn_state_name};
use mez_agent::{ActionResult, AgentAction, AgentActionPayload, AgentTurnState, MaapBatch};

impl RuntimeSessionService {
    /// Records text in the bounded hidden per-pane trace log.
    fn record_agent_pane_trace_log_text(&mut self, pane_id: &str, text: &str) {
        let Some(log) = (!text.trim().is_empty()).then(|| {
            self.agent_pane_trace_logs
                .entry(pane_id.to_string())
                .or_default()
        }) else {
            return;
        };
        for line in text.trim_end_matches(['\r', '\n']).lines() {
            if !line.trim().is_empty() {
                log.push(runtime_bounded_trace_text(line));
            }
        }
        runtime_trim_agent_pane_trace_log(log);
    }

    /// Returns the retained trace log text for one pane.
    pub(in crate::runtime) fn agent_pane_trace_log_text(&self, pane_id: &str) -> Option<String> {
        let log = self.agent_pane_trace_logs.get(pane_id)?;
        (!log.is_empty()).then(|| log.join("\n"))
    }

    /// Returns the currently enabled agent diagnostic label for a pane.
    fn agent_diagnostic_label(&self, pane_id: &str) -> Option<&'static str> {
        self.agent_diagnostic_level_name(pane_id)
    }

    /// Appends a trace line for an agent turn state transition.
    pub(in crate::runtime) fn append_agent_trace_turn_transition(
        &mut self,
        turn: &AgentTurnRecord,
        from: AgentTurnState,
        to: AgentTurnState,
        reason: &str,
    ) -> Result<()> {
        let trace_line = format!(
            "agent trace: turn {} moved from {} to {} ({})",
            turn.turn_id,
            runtime_agent_turn_state_name(from),
            runtime_agent_turn_state_name(to),
            runtime_agent_terminal_preview(&runtime_humanize_agent_diagnostic(reason))
        );
        self.record_agent_pane_trace_log_text(&turn.pane_id, &trace_line);
        let Some(label) = self.agent_diagnostic_label(&turn.pane_id) else {
            return Ok(());
        };
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent {label}: turn {} moved from {} to {} ({})",
                turn.turn_id,
                runtime_agent_turn_state_name(from),
                runtime_agent_turn_state_name(to),
                runtime_agent_terminal_preview(&runtime_humanize_agent_diagnostic(reason))
            ),
        )
    }

    /// Appends one diagnostic trace event for an agent turn.
    pub(in crate::runtime) fn append_agent_trace_turn_event(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        message: &str,
    ) -> Result<()> {
        if self.find_pane_descriptor(pane_id).is_none() {
            return Ok(());
        }
        let trace_line = format!(
            "agent trace: turn {}: {}",
            turn_id,
            runtime_agent_terminal_preview(&runtime_humanize_agent_diagnostic(message))
        );
        self.record_agent_pane_trace_log_text(pane_id, &trace_line);
        let Some(label) = self.agent_diagnostic_label(pane_id) else {
            return Ok(());
        };
        let message = if self.agent_trace_enabled(pane_id) {
            message.to_string()
        } else {
            runtime_sanitize_agent_diagnostic_text(message)
        };
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!(
                "agent {label}: turn {}: {}",
                turn_id,
                runtime_agent_terminal_preview(&runtime_humanize_agent_diagnostic(&message))
            ),
        )
    }

    /// Appends a MAAP diagnostic while retaining a possibly fuller trace value.
    fn append_agent_trace_maap_value_with_retained(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        label: &str,
        display_value: serde_json::Value,
        retained_value: serde_json::Value,
    ) -> Result<()> {
        let retained_value = runtime_bounded_trace_value_strings(retained_value);
        let retained_body = serde_json::to_string_pretty(&retained_value).map_err(|error| {
            MezError::invalid_state(format!("MAAP trace JSON encoding failed: {error}"))
        })?;
        self.record_agent_pane_trace_log_text(
            pane_id,
            &format!("agent trace: turn {turn_id}: MAAP {label}\n{retained_body}"),
        );
        let Some(level_label) = self.agent_diagnostic_label(pane_id) else {
            return Ok(());
        };
        let value = runtime_bounded_trace_value_strings(display_value);
        let body = serde_json::to_string_pretty(&value).map_err(|error| {
            MezError::invalid_state(format!("MAAP trace JSON encoding failed: {error}"))
        })?;
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!("agent {level_label}: turn {turn_id}: MAAP {label}\n{body}"),
        )
    }

    /// Appends the model request submitted for one agent turn to trace output.
    pub(in crate::runtime) fn append_agent_trace_maap_request(
        &mut self,
        turn: &AgentTurnRecord,
        request: &ModelRequest,
    ) -> Result<()> {
        self.append_agent_trace_maap_value_with_retained(
            &turn.pane_id,
            &turn.turn_id,
            "request",
            runtime_model_request_trace_json(
                request,
                self.agent_trace_enabled(&turn.pane_id),
                true,
            ),
            runtime_model_request_trace_json(request, true, true),
        )
    }

    /// Records the provider request shape that the runtime is about to submit.
    pub(in crate::runtime) fn record_runtime_provider_request_shape_for_context(
        &mut self,
        model_profile: &ModelProfile,
        turn: &AgentTurnRecord,
        context: &AgentContext,
        available_mcp_tools: &[mez_agent::McpPromptTool],
        memory_actions_enabled: bool,
        issue_actions_enabled: bool,
    ) {
        let Ok(mut request) = assemble_model_request_with_retained_tail_percent(
            model_profile,
            turn,
            context,
            self.agent_compaction_raw_retention_percent,
        ) else {
            return;
        };
        apply_default_action_gates(
            &mut request,
            available_mcp_tools,
            memory_actions_enabled,
            issue_actions_enabled,
        );
        let (diagnostics, diagnostics_failed) = if request.provider == "openai" {
            match openai_prompt_cache_diagnostics_for_request(&request) {
                Ok(diagnostics) => (Some(diagnostics), false),
                Err(_) => (None, true),
            }
        } else {
            (None, false)
        };
        self.runtime_metrics.record_provider_request_shape(
            &request,
            diagnostics.as_ref(),
            diagnostics_failed,
        );
    }

    /// Appends the model response returned for one agent turn to trace output.
    pub(in crate::runtime) fn append_agent_trace_maap_response(
        &mut self,
        turn: &AgentTurnRecord,
        response: &ModelResponse,
    ) -> Result<()> {
        self.append_agent_trace_maap_value_with_retained(
            &turn.pane_id,
            &turn.turn_id,
            "response",
            runtime_model_response_trace_json(response, self.agent_trace_enabled(&turn.pane_id)),
            runtime_model_response_trace_json(response, true),
        )
    }

    /// Appends action results from one MAAP batch to trace output.
    pub(in crate::runtime) fn append_agent_trace_maap_action_results(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        label: &str,
        results: &[ActionResult],
    ) -> Result<()> {
        self.append_agent_trace_maap_value_with_retained(
            pane_id,
            turn_id,
            label,
            runtime_action_results_trace_json(results, self.agent_trace_enabled(pane_id)),
            runtime_action_results_trace_json(results, true),
        )
    }

    /// Appends one provider error to trace output.
    pub(in crate::runtime) fn append_agent_trace_provider_error(
        &mut self,
        turn: &AgentTurnRecord,
        provider_id: &str,
        model_profile: &ModelProfile,
        error: &MezError,
    ) -> Result<()> {
        let display_include_shell_view = self.agent_trace_enabled(&turn.pane_id);
        let display_value = runtime_agent_provider_error_trace_json(
            provider_id,
            model_profile,
            error,
            display_include_shell_view,
        );
        let retained_value =
            runtime_agent_provider_error_trace_json(provider_id, model_profile, error, true);
        self.append_agent_trace_maap_value_with_retained(
            &turn.pane_id,
            &turn.turn_id,
            "provider_error",
            display_value,
            retained_value,
        )
    }
}

/// Runs the runtime model message role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_model_message_role_name(role: ModelMessageRole) -> &'static str {
    match role {
        ModelMessageRole::System => "system",
        ModelMessageRole::Developer => "developer",
        ModelMessageRole::User => "user",
        ModelMessageRole::Assistant => "assistant",
        ModelMessageRole::Tool => "tool",
    }
}

/// Runs the runtime context source kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_context_source_kind_name(source: ContextSourceKind) -> &'static str {
    match source {
        ContextSourceKind::System => "system",
        ContextSourceKind::UserInstruction => "user_instruction",
        ContextSourceKind::SkillInstruction => "skill_instruction",
        ContextSourceKind::DeveloperInstruction => "developer_instruction",
        ContextSourceKind::Policy => "policy",
        ContextSourceKind::Configuration => "configuration",
        ContextSourceKind::LocalMessage => "local_message",
        ContextSourceKind::RuntimeHint => "runtime_hint",
        ContextSourceKind::ProjectGuidance => "project_guidance",
        ContextSourceKind::Memory => "memory",
        ContextSourceKind::Transcript => "transcript",
        ContextSourceKind::TranscriptUser => "transcript_user",
        ContextSourceKind::TranscriptAssistant => "transcript_assistant",
        ContextSourceKind::TranscriptTool => "transcript_tool",
        ContextSourceKind::EvidenceLedger => "evidence_ledger",
        ContextSourceKind::CommittedEvidence => "committed_evidence",
        ContextSourceKind::ActionResult => "action_result",
    }
}

/// Runs the runtime json or string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_or_string(value: &str) -> serde_json::Value {
    serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.to_string()))
}

/// Runs the runtime redacted shell view text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_redacted_shell_view_text() -> &'static str {
    "hidden at debug log level; use /log-level trace"
}

/// Runs the runtime redacted shell view marker operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_redacted_shell_view_marker() -> serde_json::Value {
    serde_json::Value::String(runtime_redacted_shell_view_text().to_string())
}

pub(super) const RUNTIME_TRACE_TEXT_LIMIT_BYTES: usize = 8192;
/// Maximum retained trace lines per pane for deferred diagnostics.
pub(super) const RUNTIME_PANE_TRACE_LOG_MAX_LINES: usize = 2_000;
/// Maximum retained trace bytes per pane for deferred diagnostics.
pub(super) const RUNTIME_PANE_TRACE_LOG_MAX_BYTES: usize = 256 * 1024;

/// Returns bounded trace text so raw provider payloads cannot multiply large
/// inline file contents in terminal logs.
pub(super) fn runtime_bounded_trace_text(value: &str) -> String {
    if value.len() <= RUNTIME_TRACE_TEXT_LIMIT_BYTES {
        return value.to_string();
    }
    let mut end = RUNTIME_TRACE_TEXT_LIMIT_BYTES;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}...[truncated; original_bytes={}]",
        &value[..end],
        value.len()
    )
}

/// Returns a JSON string value with the runtime trace text cap applied.
pub(super) fn runtime_bounded_trace_string_value(value: &str) -> serde_json::Value {
    serde_json::Value::String(runtime_bounded_trace_text(value))
}

/// Recursively bounds JSON string leaves before trace rendering.
pub(super) fn runtime_bounded_trace_value_strings(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => runtime_bounded_trace_string_value(&text),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(runtime_bounded_trace_value_strings)
                .collect(),
        ),
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, runtime_bounded_trace_value_strings(value)))
                .collect(),
        ),
        other => other,
    }
}

/// Trims one pane trace log to the configured line and byte bounds.
pub(super) fn runtime_trim_agent_pane_trace_log(log: &mut Vec<String>) {
    while log.len() > RUNTIME_PANE_TRACE_LOG_MAX_LINES
        || log.iter().map(|line| line.len()).sum::<usize>() > RUNTIME_PANE_TRACE_LOG_MAX_BYTES
    {
        if log.is_empty() {
            return;
        }
        log.remove(0);
    }
}

/// Runs the runtime sanitize shell view value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_sanitize_shell_view_value(
    value: serde_json::Value,
    preserve_command_fields: bool,
) -> serde_json::Value {
    match value {
        serde_json::Value::Object(mut object) => {
            if !preserve_command_fields && object.contains_key("command") {
                object.insert("command".to_string(), runtime_redacted_shell_view_marker());
            }
            if let Some(terminal_observation) = object.get_mut("terminal_observation") {
                let sanitized = runtime_sanitize_shell_view_value(
                    terminal_observation.take(),
                    preserve_command_fields,
                );
                *terminal_observation = sanitized;
            }
            if object.contains_key("combined_output_preview") {
                object.insert(
                    "combined_output_preview".to_string(),
                    runtime_redacted_shell_view_marker(),
                );
            }
            serde_json::Value::Object(
                object
                    .into_iter()
                    .map(|(key, value)| {
                        if key == "raw_text" {
                            (key, runtime_redacted_shell_view_marker())
                        } else {
                            (
                                key,
                                runtime_sanitize_shell_view_value(value, preserve_command_fields),
                            )
                        }
                    })
                    .collect(),
            )
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(|value| runtime_sanitize_shell_view_value(value, preserve_command_fields))
                .collect(),
        ),
        other => other,
    }
}

/// Runs the runtime sanitize action result context text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_sanitize_action_result_context_text(
    content: &str,
    preserve_command_fields: bool,
) -> String {
    let Some((prefix, structured)) = content.split_once("structured_content:\n") else {
        return content.to_string();
    };
    let structured = structured.trim_end();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(structured) else {
        return content.to_string();
    };
    let sanitized = runtime_sanitize_shell_view_value(value, preserve_command_fields);
    let Ok(sanitized_text) = serde_json::to_string(&sanitized) else {
        return content.to_string();
    };
    format!("{prefix}structured_content:\n{sanitized_text}")
}

/// Runs the runtime sanitize agent diagnostic text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_sanitize_agent_diagnostic_text(value: &str) -> String {
    if let Some((prefix, _)) = value.split_once(" command=") {
        return format!("{prefix} command={}", runtime_redacted_shell_view_text());
    }
    value.to_string()
}

/// Normalizes common MAAP `send_message` media aliases before MMP delivery.
///
/// The MMP transport remains strict about canonical media metadata. MAAP is
/// model-generated, so the runtime accepts the common `text/plain` shorthand
/// and delivers it as the canonical UTF-8 text media type instead of failing a
/// useful subagent coordination message.
pub(super) fn runtime_maap_message_content_type(content_type: &str) -> String {
    match content_type.trim().to_ascii_lowercase().as_str() {
        "text/plain" | "text/plain;charset=utf-8" | "text/plain; charset=utf-8" => {
            "text/plain; charset=utf-8".to_string()
        }
        "application/json" => "application/json".to_string(),
        _ => content_type.to_string(),
    }
}

/// Extracts child agent, display name, and turn ids from a spawn response.
pub(super) fn runtime_spawn_json_agent_and_turn(
    spawn_json: &str,
) -> Result<(String, Option<String>, Option<String>)> {
    let value = serde_json::from_str::<serde_json::Value>(spawn_json).map_err(|error| {
        MezError::invalid_state(format!("subagent spawn response is invalid JSON: {error}"))
    })?;
    let child_agent_id = value
        .get("agent")
        .and_then(|agent| agent.get("id"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_state("subagent spawn response missing agent id"))?;
    let child_display_name = value
        .get("agent")
        .and_then(|agent| agent.get("display_name"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let child_turn_id = value
        .get("turn")
        .filter(|turn| !turn.is_null())
        .and_then(|turn| turn.get("id"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    Ok((
        child_agent_id.to_string(),
        child_display_name,
        child_turn_id,
    ))
}

/// Formats a human-readable subagent label while preserving the canonical id.
pub(super) fn runtime_subagent_display_label(agent_id: &str, display_name: Option<&str>) -> String {
    match display_name.map(str::trim).filter(|name| !name.is_empty()) {
        Some(display_name) => format!("{display_name} ({agent_id})"),
        None => agent_id.to_string(),
    }
}

/// Returns the concise lifecycle word used for parent-visible subagent results.
pub(super) fn runtime_subagent_result_status_label(success: bool, summary: &str) -> &'static str {
    if success {
        "completed"
    } else if summary.contains("cancelled") {
        "cancelled"
    } else if summary.contains("interrupted") {
        "interrupted"
    } else {
        "failed"
    }
}

/// Runs the runtime maap action payload trace json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_maap_action_payload_trace_json(
    payload: &AgentActionPayload,
    preserve_command_fields: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let mut data = serde_json::Map::new();
    match payload {
        AgentActionPayload::Say {
            status,
            text,
            content_type,
        } => {
            data.insert("status".to_string(), serde_json::json!(status.as_str()));
            data.insert("text".to_string(), runtime_bounded_trace_string_value(text));
            data.insert("content_type".to_string(), serde_json::json!(content_type));
        }
        AgentActionPayload::RequestCapability { capability, reason } => {
            data.insert(
                "capability".to_string(),
                serde_json::json!(capability.as_str()),
            );
            data.insert(
                "reason".to_string(),
                runtime_bounded_trace_string_value(reason),
            );
        }
        AgentActionPayload::RequestSkills => {}
        AgentActionPayload::CallSkill {
            name,
            additional_context,
        } => {
            data.insert("name".to_string(), serde_json::json!(name));
            data.insert(
                "additional_context".to_string(),
                additional_context
                    .as_deref()
                    .map(runtime_bounded_trace_string_value)
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        AgentActionPayload::ShellCommand {
            summary,
            command,
            interactive,
            stateful,
            timeout_ms,
        } => {
            data.insert(
                "summary".to_string(),
                runtime_bounded_trace_string_value(summary),
            );
            data.insert(
                "command".to_string(),
                if preserve_command_fields {
                    runtime_bounded_trace_string_value(command)
                } else {
                    runtime_redacted_shell_view_marker()
                },
            );
            data.insert("interactive".to_string(), serde_json::json!(interactive));
            data.insert("stateful".to_string(), serde_json::json!(stateful));
            data.insert("timeout_ms".to_string(), serde_json::json!(timeout_ms));
        }
        AgentActionPayload::ApplyPatch { patch, strip } => {
            data.insert("patch_bytes".to_string(), serde_json::json!(patch.len()));
            data.insert("strip".to_string(), serde_json::json!(strip));
        }
        AgentActionPayload::WebSearch {
            query,
            domains,
            recency_days,
            max_results,
        } => {
            data.insert(
                "query".to_string(),
                runtime_bounded_trace_string_value(query),
            );
            data.insert("domains".to_string(), serde_json::json!(domains));
            data.insert("recency_days".to_string(), serde_json::json!(recency_days));
            data.insert("max_results".to_string(), serde_json::json!(max_results));
        }
        AgentActionPayload::FetchUrl {
            url,
            format,
            max_bytes,
        } => {
            data.insert("url".to_string(), serde_json::json!(url));
            data.insert("format".to_string(), serde_json::json!(format));
            data.insert("max_bytes".to_string(), serde_json::json!(max_bytes));
        }
        AgentActionPayload::MemorySearch { query, limit } => {
            data.insert(
                "query".to_string(),
                runtime_bounded_trace_string_value(query),
            );
            data.insert("limit".to_string(), serde_json::json!(limit));
        }
        AgentActionPayload::MemoryStore {
            kind,
            priority,
            scope,
            keywords,
            content,
            expires_in_days,
        } => {
            data.insert("kind".to_string(), serde_json::json!(kind));
            data.insert("priority".to_string(), serde_json::json!(priority));
            data.insert("scope".to_string(), serde_json::json!(scope));
            data.insert("keywords".to_string(), serde_json::json!(keywords));
            data.insert(
                "content".to_string(),
                runtime_bounded_trace_string_value(content),
            );
            data.insert(
                "expires_in_days".to_string(),
                serde_json::json!(expires_in_days),
            );
        }
        AgentActionPayload::IssueAdd {
            kind,
            title,
            body,
            notes,
            ..
        } => {
            data.insert("kind".to_string(), serde_json::json!(kind));
            data.insert(
                "title".to_string(),
                runtime_bounded_trace_string_value(title),
            );
            data.insert(
                "body".to_string(),
                body.as_deref()
                    .map(runtime_bounded_trace_string_value)
                    .unwrap_or(serde_json::Value::Null),
            );
            data.insert(
                "notes".to_string(),
                notes
                    .as_deref()
                    .map(runtime_bounded_trace_string_value)
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        AgentActionPayload::IssueUpdate {
            id,
            kind,
            title,
            body,
            clear_body,
            notes,
            clear_notes,
            ..
        } => {
            data.insert("id".to_string(), runtime_bounded_trace_string_value(id));
            data.insert("kind".to_string(), serde_json::json!(kind));
            data.insert(
                "title".to_string(),
                title
                    .as_deref()
                    .map(runtime_bounded_trace_string_value)
                    .unwrap_or(serde_json::Value::Null),
            );
            data.insert(
                "body".to_string(),
                body.as_deref()
                    .map(runtime_bounded_trace_string_value)
                    .unwrap_or(serde_json::Value::Null),
            );
            data.insert("clear_body".to_string(), serde_json::json!(clear_body));
            data.insert(
                "notes".to_string(),
                notes
                    .as_deref()
                    .map(runtime_bounded_trace_string_value)
                    .unwrap_or(serde_json::Value::Null),
            );
            data.insert("clear_notes".to_string(), serde_json::json!(clear_notes));
        }
        AgentActionPayload::IssueQuery {
            kind,
            state,
            text,
            limit,
        } => {
            data.insert("kind".to_string(), serde_json::json!(kind));
            data.insert("state".to_string(), serde_json::json!(state));
            data.insert(
                "text".to_string(),
                text.as_deref()
                    .map(runtime_bounded_trace_string_value)
                    .unwrap_or(serde_json::Value::Null),
            );
            data.insert("limit".to_string(), serde_json::json!(limit));
        }
        AgentActionPayload::IssueDelete { id } => {
            data.insert("id".to_string(), runtime_bounded_trace_string_value(id));
        }
        AgentActionPayload::SendMessage {
            recipient,
            content_type,
            payload,
        } => {
            data.insert("recipient".to_string(), serde_json::json!(recipient));
            data.insert("content_type".to_string(), serde_json::json!(content_type));
            data.insert(
                "payload".to_string(),
                runtime_bounded_trace_string_value(payload),
            );
        }
        AgentActionPayload::SpawnAgent {
            role,
            placement,
            cooperation_mode,
            read_scopes,
            write_scopes,
            task_prompt,
        } => {
            data.insert("role".to_string(), serde_json::json!(role));
            data.insert("placement".to_string(), serde_json::json!(placement));
            data.insert(
                "cooperation_mode".to_string(),
                serde_json::json!(cooperation_mode),
            );
            data.insert("read_scopes".to_string(), serde_json::json!(read_scopes));
            data.insert("write_scopes".to_string(), serde_json::json!(write_scopes));
            data.insert(
                "task_prompt".to_string(),
                runtime_bounded_trace_string_value(task_prompt),
            );
        }
        AgentActionPayload::ConfigChange {
            setting_path,
            operation,
            value,
        } => {
            data.insert("setting_path".to_string(), serde_json::json!(setting_path));
            data.insert("operation".to_string(), serde_json::json!(operation));
            data.insert(
                "value".to_string(),
                value
                    .as_deref()
                    .map(runtime_bounded_trace_string_value)
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        AgentActionPayload::McpCall {
            server,
            tool,
            arguments_json,
        } => {
            data.insert("server".to_string(), serde_json::json!(server));
            data.insert("tool".to_string(), serde_json::json!(tool));
            data.insert(
                "arguments".to_string(),
                runtime_bounded_trace_value_strings(runtime_json_or_string(arguments_json)),
            );
        }
        AgentActionPayload::Complete => {}
        AgentActionPayload::Abort { reason } => {
            data.insert(
                "reason".to_string(),
                runtime_bounded_trace_string_value(reason),
            );
        }
    }
    data
}

/// Runs the runtime maap action trace json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_maap_action_trace_json(
    action: &AgentAction,
    preserve_command_fields: bool,
) -> serde_json::Value {
    let mut data = runtime_maap_action_payload_trace_json(&action.payload, preserve_command_fields);
    data.insert("id".to_string(), serde_json::json!(action.id));
    data.insert("type".to_string(), serde_json::json!(action.action_type()));
    data.insert("rationale".to_string(), serde_json::json!(action.rationale));
    serde_json::Value::Object(data)
}

/// Runs the runtime maap batch trace json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_maap_batch_trace_json(
    batch: &MaapBatch,
    preserve_command_fields: bool,
) -> serde_json::Value {
    serde_json::json!({
        "protocol": batch.protocol,
        "rationale": batch.rationale,
        "thought": batch.thought,
        "turn_id": batch.turn_id,
        "agent_id": batch.agent_id,
        "final": batch.final_turn,
        "actions": batch
            .actions
            .iter()
            .map(|action| runtime_maap_action_trace_json(action, preserve_command_fields))
            .collect::<Vec<_>>()
    })
}

/// Runs the runtime model request trace json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_model_request_trace_json(
    request: &ModelRequest,
    include_shell_view: bool,
    preserve_command_fields: bool,
) -> serde_json::Value {
    serde_json::json!({
        "provider": request.provider,
        "model": request.model,
        "max_output_tokens": request.max_output_tokens,
        "turn_id": request.turn_id,
        "agent_id": request.agent_id,
        "interaction_kind": request.interaction_kind.as_str(),
        "allowed_actions": request.allowed_actions.action_type_names(),
        "available_mcp_tools": request
            .available_mcp_tools
            .iter()
            .map(|tool| serde_json::json!({
                "server_id": tool.server_id,
                "tool_name": tool.tool_name,
                "description": tool.description,
                "approval_required": tool.approval_required,
                "input_schema": runtime_json_or_string(&tool.input_schema_json)
            }))
            .collect::<Vec<_>>(),
        "prompt_cache": openai_prompt_cache_diagnostics_for_request(request)
            .ok()
            .map(runtime_openai_prompt_cache_diagnostics_trace_json),
        "messages": request
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                let content = if include_shell_view {
                    message.content.clone()
                } else if message.source == ContextSourceKind::ActionResult {
                    runtime_sanitize_action_result_context_text(
                        &message.content,
                        preserve_command_fields
                    )
                } else {
                    message.content.clone()
                };
                serde_json::json!({
                    "index": index,
                    "role": runtime_model_message_role_name(message.role),
                    "source": runtime_context_source_kind_name(message.source),
                    "content": runtime_bounded_trace_text(&content),
                    "content_bytes": message.content.len(),
                    "content_truncated": content.len() > RUNTIME_TRACE_TEXT_LIMIT_BYTES
                })
            })
            .collect::<Vec<_>>()
    })
}

/// Builds trace JSON for provider prompt-cache diagnostics.
pub(super) fn runtime_openai_prompt_cache_diagnostics_trace_json(
    diagnostics: OpenAiPromptCacheDiagnostics,
) -> serde_json::Value {
    serde_json::json!({
        "prompt_cache_key": diagnostics.prompt_cache_key,
        "instructions_bytes": diagnostics.instructions_bytes,
        "instructions_sha256": diagnostics.instructions_sha256,
        "response_format_bytes": diagnostics.response_format_bytes,
        "response_format_sha256": diagnostics.response_format_sha256,
        "tools_bytes": diagnostics.tools_bytes,
        "tools_sha256": diagnostics.tools_sha256,
        "tool_choice_bytes": diagnostics.tool_choice_bytes,
        "tool_choice_sha256": diagnostics.tool_choice_sha256,
        "stable_input_bytes": diagnostics.stable_input_bytes,
        "stable_input_sha256": diagnostics.stable_input_sha256,
        "volatile_input_bytes": diagnostics.volatile_input_bytes,
        "volatile_input_sha256": diagnostics.volatile_input_sha256,
        "stable_prompt_prefix_bytes": diagnostics.stable_prompt_prefix_bytes,
        "stable_prompt_prefix_sha256": diagnostics.stable_prompt_prefix_sha256,
        "provider_request_shape_bytes": diagnostics.provider_request_shape_bytes,
        "provider_request_shape_sha256": diagnostics.provider_request_shape_sha256,
        "cacheable_prefix_bytes": diagnostics.cacheable_prefix_bytes,
        "cacheable_prefix_sha256": diagnostics.cacheable_prefix_sha256
    })
}

/// Runs the runtime model response trace json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_model_response_trace_json(
    response: &ModelResponse,
    include_shell_view: bool,
) -> serde_json::Value {
    serde_json::json!({
        "provider": response.provider,
        "model": response.model,
        "raw_text": if include_shell_view {
            serde_json::Value::String(runtime_bounded_trace_text(&response.raw_text))
        } else {
            runtime_redacted_shell_view_marker()
        },
        "raw_text_bytes": response.raw_text.len(),
        "raw_text_truncated": response.raw_text.len() > RUNTIME_TRACE_TEXT_LIMIT_BYTES,
        "usage": {
            "input_tokens": response.usage.input_tokens,
            "output_tokens": response.usage.output_tokens,
            "reasoning_tokens": response.usage.reasoning_tokens,
            "cached_input_tokens": response.usage.cached_input_tokens,
            "cached_input_tokens_reported": response.usage.cached_input_tokens.is_some(),
            "cached_input_hit_ratio": response.usage.cached_input_hit_ratio_display(),
            "total_tokens": response.usage.total_tokens()
        },
        "action_batch": response
            .action_batch
            .as_ref()
            .map(|batch| runtime_maap_batch_trace_json(batch, true))
    })
}

/// Builds provider-error diagnostic JSON with optional raw provider detail.
pub(super) fn runtime_agent_provider_error_trace_json(
    provider_id: &str,
    model_profile: &ModelProfile,
    error: &MezError,
    include_shell_view: bool,
) -> serde_json::Value {
    serde_json::json!({
        "provider": provider_id,
        "model": model_profile.model,
        "error": {
            "kind": runtime_mezzanine_error_code(error.kind()),
            "message": error.message()
        },
        "provider_raw_text": error.provider_raw_text().map(|raw_text| {
            if include_shell_view {
                serde_json::Value::String(runtime_bounded_trace_text(raw_text))
            } else {
                runtime_redacted_shell_view_marker()
            }
        }),
        "provider_failure_json": error
            .provider_failure_json()
            .map(runtime_json_or_string)
            .map(|value| if include_shell_view {
                value
            } else {
                runtime_sanitize_shell_view_value(value, true)
            })
    })
}

/// Runs the runtime action result trace json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_action_result_trace_json(
    result: &ActionResult,
    include_shell_view: bool,
) -> serde_json::Value {
    serde_json::json!({
        "protocol": result.protocol,
        "turn_id": result.turn_id,
        "agent_id": result.agent_id,
        "action_id": result.action_id,
        "action_type": result.action_type,
        "status": runtime_action_status_name(result.status),
        "content": result
            .content
            .iter()
            .map(|block| serde_json::json!({
                "type": block.block_type,
                "text": runtime_bounded_trace_text(&block.text),
                "text_bytes": block.text.len(),
                "text_truncated": block.text.len() > RUNTIME_TRACE_TEXT_LIMIT_BYTES
            }))
            .collect::<Vec<_>>(),
        "structured_content": result
            .structured_content_json
            .as_deref()
            .map(runtime_json_or_string)
            .map(|value| if include_shell_view {
                runtime_bounded_trace_value_strings(value)
            } else {
                runtime_bounded_trace_value_strings(runtime_sanitize_shell_view_value(value, true))
            }),
        "is_error": result.is_error,
        "error": result.error.as_ref().map(|error| serde_json::json!({
            "code": error.code,
            "message": runtime_bounded_trace_text(&error.message),
            "message_bytes": error.message.len(),
            "message_truncated": error.message.len() > RUNTIME_TRACE_TEXT_LIMIT_BYTES,
            "data": error
                .data_json
                .as_deref()
                .map(runtime_json_or_string)
                .map(runtime_bounded_trace_value_strings)
        }))
    })
}

/// Runs the runtime action results trace json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_action_results_trace_json(
    results: &[ActionResult],
    include_shell_view: bool,
) -> serde_json::Value {
    serde_json::Value::Array(
        results
            .iter()
            .map(|result| runtime_action_result_trace_json(result, include_shell_view))
            .collect(),
    )
}
