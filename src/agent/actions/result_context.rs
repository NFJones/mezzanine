//! Action-result transcript and model-context rendering.
//!
//! This module owns compact, bounded projections of action results for
//! durable transcript storage and follow-up model context. It keeps
//! skill-result summarization, JSON audit pruning, and truncation notices
//! separate from turn execution while preserving raw shell output bytes inside
//! ordinary model context.

use super::{ActionResult, ActionStatus, ShellReadObservation};

/// Maximum action-result content bytes included in one model-facing context
/// block before native truncation metadata is appended.
const MODEL_ACTION_RESULT_CONTENT_LIMIT_BYTES: u64 = 256 * 1024;

/// Executes the `action_result_transcript_content` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(in crate::agent) fn action_result_transcript_content(result: &ActionResult) -> String {
    if matches!(result.action_type, "request_skills" | "call_skill") {
        let mut content = format!(
            "action_id={} action_type={} status={:?}",
            result.action_id, result.action_type, result.status
        );
        if let Some(summary) = skill_action_result_transcript_summary(result) {
            content.push_str("\nskill_action_summary:\n");
            content.push_str(&summary);
        }
        if let Some(error) = &result.error {
            content.push_str("\nerror:");
            content.push_str(&error.code);
            content.push(' ');
            content.push_str(&error.message);
        }
        return content;
    }
    action_result_context_content(result)
}

/// Builds a compact durable summary for non-effecting skill actions.
///
/// Skill result bodies can contain complete `SKILL.md` documents or catalogs.
/// Durable transcript storage keeps only metadata that helps audit what
/// happened without allowing those workflow instructions to become future
/// model prompt context.
fn skill_action_result_transcript_summary(result: &ActionResult) -> Option<String> {
    match result.action_type {
        "request_skills" => skill_catalog_result_transcript_summary(result),
        "call_skill" => called_skill_result_transcript_summary(result),
        _ => None,
    }
}

/// Summarizes a skill-catalog action result without copying descriptions.
fn skill_catalog_result_transcript_summary(result: &ActionResult) -> Option<String> {
    let data = result.structured_content_json.as_deref()?;
    let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let skills = value
        .get("skills")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let diagnostics = value
        .get("diagnostics")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let names = skills
        .iter()
        .filter_map(|skill| skill.get("name").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    let mut lines = vec![format!(
        "skills={} diagnostics={}",
        names.len(),
        diagnostics
    )];
    if !names.is_empty() {
        lines.push(format!("names={}", names.join(",")));
    }
    Some(lines.join("\n"))
}

/// Summarizes a loaded skill result without copying the skill body.
fn called_skill_result_transcript_summary(result: &ActionResult) -> Option<String> {
    let data = result.structured_content_json.as_deref()?;
    let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let object = value.as_object()?;
    let mut fields = Vec::new();
    for key in [
        "name",
        "source",
        "path",
        "skill_bytes",
        "additional_context_bytes",
    ] {
        let Some(value) = object.get(key) else {
            continue;
        };
        if let Some(text) = json_scalar_context_text(value) {
            fields.push(format!("{key}={text}"));
        }
    }
    (!fields.is_empty()).then(|| fields.join("\n"))
}

/// Executes the `action_result_context_content` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn action_result_context_content(result: &ActionResult) -> String {
    let mut lines = vec![format!(
        "[action_result {} {} {}]",
        result.action_id,
        result.action_type,
        action_status_context_name(result.status)
    )];
    if let Some(error) = &result.error {
        lines.push(format!("error: {} {}", error.code, error.message));
        if let Some(data) = error
            .data_json
            .as_deref()
            .and_then(model_error_json_text_for_context)
        {
            lines.push(format!("error_data: {data}"));
        }
    }
    if action_result_has_shell_observation(result) {
        append_shell_action_result_context(result, &mut lines);
    } else {
        append_action_result_content_text(result, &mut lines);
        if let Some(data) = result
            .structured_content_json
            .as_deref()
            .and_then(model_structured_json_text_for_context)
        {
            lines.push(format!("data: {data}"));
        }
    }
    lines.join("\n")
}

/// Returns true when a result carries pane shell transaction observation data.
fn action_result_has_shell_observation(result: &ActionResult) -> bool {
    result
        .structured_content_json
        .as_deref()
        .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .and_then(|value| value.as_object().cloned())
        .is_some_and(|object| {
            object.contains_key("command") && object.contains_key("terminal_observation")
        })
}

/// Returns the compact lowercase status name used in model-facing result
/// context.
fn action_status_context_name(status: ActionStatus) -> &'static str {
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

/// Appends compact shell-result context for the next provider turn.
fn append_shell_action_result_context(result: &ActionResult, lines: &mut Vec<String>) {
    let structured = result
        .structured_content_json
        .as_deref()
        .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok());
    let structured_object = structured.as_ref().and_then(serde_json::Value::as_object);
    if let Some(command) = structured_object
        .and_then(|object| object.get("command"))
        .and_then(serde_json::Value::as_str)
        .filter(|command| !command.trim().is_empty())
    {
        lines.push(format!("command: {command}"));
    }
    if let Some(observations) = structured_object
        .and_then(|object| object.get("read_observations"))
        .and_then(read_observations_for_context)
    {
        append_read_observation_lines(lines, &observations);
    }
    let terminal_observation = structured_object
        .and_then(|object| object.get("terminal_observation"))
        .and_then(serde_json::Value::as_object);
    if let Some(observation) = terminal_observation {
        append_json_scalar_line(lines, "exit_code", observation.get("exit_code"));
        append_json_scalar_line(lines, "signal", observation.get("signal"));
        append_true_bool_line(lines, "timed_out", observation.get("timed_out"));
        append_true_bool_line(lines, "interrupted", observation.get("interrupted"));
        append_true_bool_line(
            lines,
            "output_truncated",
            observation.get("output_truncated"),
        );
        append_true_bool_line(
            lines,
            "transport_incomplete",
            observation.get("transport_incomplete"),
        );
    }
    let output = shell_action_result_output_for_context(result, terminal_observation);
    if !output.trim().is_empty() {
        lines.push("output:".to_string());
        lines.push(output);
    }
    if structured.is_some() {
        return;
    }
    append_action_result_content_text(result, lines);
}

/// Parses structured read observations from one shell result payload.
fn read_observations_for_context(value: &serde_json::Value) -> Option<Vec<ShellReadObservation>> {
    let observations = serde_json::from_value::<Vec<ShellReadObservation>>(value.clone()).ok()?;
    (!observations.is_empty()).then_some(observations)
}

/// Appends structured read observations in a provider-visible single-line form.
fn append_read_observation_lines(lines: &mut Vec<String>, observations: &[ShellReadObservation]) {
    for observation in observations {
        lines.push(format!(
            "read_observation_json: {}",
            serde_json::to_string(observation)
                .expect("shell read observations should always serialize")
        ));
    }
}

/// Returns true when a line is known Mezzanine wrapper traffic rather than user
/// command output.
#[allow(dead_code)]
pub(super) fn shell_output_line_is_mezzanine_wrapper(trimmed: &str) -> bool {
    [
        "MEZ_MARKER_TOKEN",
        "MEZ_TURN",
        "MEZ_AGENT",
        "MEZ_PANE",
        "MEZ_STATUS",
        "MEZ_STTY_STATE",
        "MEZ_RESTORE_",
        "MEZ_HISTORY_",
        "HISTFILE=/dev/null",
        "MEZ_COMMAND_",
        "MEZ_OUTPUT_FILE",
        "__MEZ_SHELL_OUTPUT_BASE64_",
        "mez_marker=",
        "printf '\\033]133",
        "env -u MEZ_MARKER_TOKEN",
        "__mez_tx_",
        "unset -f __mez_tx_",
        "stty -",
        "unset MEZ_",
        "set +o history",
        "set -o history",
        "history -d",
    ]
    .iter()
    .any(|marker| trimmed.contains(marker))
}

/// Appends non-empty model-readable result text.
fn append_action_result_content_text(result: &ActionResult, lines: &mut Vec<String>) {
    let mut content = result.content_text();
    if !content.trim().is_empty() {
        if truncate_string_to_max_bytes(&mut content, MODEL_ACTION_RESULT_CONTENT_LIMIT_BYTES) {
            append_truncation_notice(&mut content, MODEL_ACTION_RESULT_CONTENT_LIMIT_BYTES);
        }
        lines.push("content:".to_string());
        lines.push(content);
    }
}

/// Truncates one UTF-8 string to the requested byte ceiling.
///
/// # Parameters
/// - `text`: The string to truncate in place.
/// - `max_bytes`: The maximum retained byte length.
fn truncate_string_to_max_bytes(text: &mut String, max_bytes: u64) -> bool {
    let Ok(limit) = usize::try_from(max_bytes) else {
        return false;
    };
    if text.len() <= limit {
        return false;
    }
    let mut boundary = limit;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    true
}

/// Appends a compact truncation notice to model-readable action content.
///
/// # Parameters
/// - `text`: The string receiving the notice.
/// - `max_bytes`: The byte ceiling that caused truncation.
fn append_truncation_notice(text: &mut String, max_bytes: u64) {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "[mez: action result content truncated after {max_bytes} bytes]"
    ));
}

/// Selects the shell output text worth returning to the model.
fn shell_action_result_output_for_context(
    result: &ActionResult,
    terminal_observation: Option<&serde_json::Map<String, serde_json::Value>>,
) -> String {
    let content = result.content_text();
    if !content.trim().is_empty() && !shell_result_content_is_generic_status(&content) {
        return content;
    }
    terminal_observation
        .and_then(|observation| observation.get("combined_output_preview"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Returns true when shell result content only restates status already carried
/// by the compact header and observation fields.
fn shell_result_content_is_generic_status(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed == "shell command accepted for pane execution"
        || trimmed.starts_with("shell command exited with status ")
        || trimmed == "shell command timed out"
        || trimmed == "shell command was interrupted"
}

/// Appends a scalar JSON field using a compact `key: value` representation.
fn append_json_scalar_line(
    lines: &mut Vec<String>,
    label: &str,
    value: Option<&serde_json::Value>,
) {
    let Some(value) = value else {
        return;
    };
    if value.is_null() {
        return;
    }
    if let Some(text) = json_scalar_context_text(value) {
        lines.push(format!("{label}: {text}"));
    }
}

/// Appends a Boolean field only when true.
fn append_true_bool_line(lines: &mut Vec<String>, label: &str, value: Option<&serde_json::Value>) {
    if value.and_then(serde_json::Value::as_bool) == Some(true) {
        lines.push(format!("{label}: true"));
    }
}

/// Formats scalar JSON values for compact context.
fn json_scalar_context_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

/// Produces model-facing error data after pruning shell/audit internals.
fn model_error_json_text_for_context(value: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(value).ok()?;
    let compact =
        compact_json_value_for_context_with_pruning(&parsed, model_error_json_audit_keys())?;
    serde_json::to_string(&compact).ok()
}

/// Produces model-facing structured result data after pruning audit fields.
fn model_structured_json_text_for_context(value: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(value).ok()?;
    let compact =
        compact_json_value_for_context_with_pruning(&parsed, model_structured_json_audit_keys())?;
    serde_json::to_string(&compact).ok()
}

/// Removes fields that do not add model-usable information and drops keys
/// reserved for audit/debug surfaces.
fn compact_json_value_for_context_with_pruning(
    value: &serde_json::Value,
    pruned_keys: &[&str],
) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(value) if value.is_empty() => None,
        serde_json::Value::Array(values) => {
            let values = values
                .iter()
                .filter_map(|value| compact_json_value_for_context_with_pruning(value, pruned_keys))
                .collect::<Vec<_>>();
            if values.is_empty() {
                None
            } else {
                Some(serde_json::Value::Array(values))
            }
        }
        serde_json::Value::Object(object) => {
            let object = object
                .iter()
                .filter(|(key, _)| !pruned_keys.contains(&key.as_str()))
                .filter_map(|(key, value)| {
                    compact_json_value_for_context_with_pruning(value, pruned_keys)
                        .map(|value| (key.clone(), value))
                })
                .collect::<serde_json::Map<_, _>>();
            if object.is_empty() {
                None
            } else {
                Some(serde_json::Value::Object(object))
            }
        }
        other => Some(other.clone()),
    }
}

/// Audit/debug fields that should never be replayed as model result context.
fn model_structured_json_audit_keys() -> &'static [&'static str] {
    &[
        "approval",
        "matched_rules",
        "sent_to_pane",
        "stateful",
        "policy_command",
        "summary",
        "terminal_observation",
        "generated_command_elided",
        "generated_command_bytes",
    ]
}

/// Error data fields that are useful for audit but encourage prompt bloat or
/// automatic command replay when included in model context.
fn model_error_json_audit_keys() -> &'static [&'static str] {
    &["command"]
}
