//! Action-result transcript and model-context rendering.
//!
//! This module owns the single bounded projection of an action result used for
//! both active model context and durable transcript replay. Once exposed, the
//! projection remains byte-identical until an explicit compaction epoch.

use crate::{ActionResult, ActionStatus, ShellReadObservation};

#[cfg(test)]
mod tests;

/// Maximum action-result content bytes included in one model-facing context
/// block before native truncation metadata is appended.
const MODEL_ACTION_RESULT_CONTENT_LIMIT_BYTES: u64 = 256 * 1024;

/// Sanitized view of one action result used by model-context and transcript renderers.
///
/// The view centralizes status naming, structured payload parsing, and shell
/// observation detection so terminal presentation and model-context facts share
/// the same interpretation without reparsing the same payload at each branch.
struct ActionResultContextView<'a> {
    /// Result being rendered.
    result: &'a ActionResult,
    /// Compact lowercase status label for model-facing output.
    status_name: &'static str,
    /// Parsed structured payload, when the action returned one.
    structured: Option<serde_json::Value>,
    /// Whether this result carries shell transaction observation fields.
    is_shell_observation: bool,
}

impl<'a> ActionResultContextView<'a> {
    /// Builds one sanitized fact view from an action result.
    fn new(result: &'a ActionResult) -> Self {
        let structured = result
            .structured_content_json
            .as_deref()
            .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok());
        let is_shell_observation = structured
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .is_some_and(|object| {
                object.contains_key("command") && object.contains_key("terminal_observation")
            });
        Self {
            result,
            status_name: action_status_context_name(result.status),
            structured,
            is_shell_observation,
        }
    }

    /// Formats the stable model-facing action-result header line.
    fn header_line(&self) -> String {
        format!(
            "[action_result {} {} {}]",
            self.result.action_id, self.result.action_type, self.status_name
        )
    }

    /// Returns the structured payload as an object when available.
    fn structured_object(&self) -> Option<&serde_json::Map<String, serde_json::Value>> {
        self.structured
            .as_ref()
            .and_then(serde_json::Value::as_object)
    }

    /// Reports whether the result had structured payload data.
    fn has_structured_data(&self) -> bool {
        self.structured.is_some()
    }
}

/// Executes the `action_result_transcript_content` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn action_result_transcript_content(result: &ActionResult) -> String {
    action_result_context_content(result)
}

/// Fixed marker that replaces the omitted provider-visible body of one legacy
/// tool result whose header or scalar preamble validated.
const HISTORICAL_OUTPUT_OMITTED_MARKER: &str = "historical_output: omitted";

/// Maximum accepted byte length of one legacy action-result header line.
const HISTORICAL_HEADER_MAX_BYTES: usize = 512;

/// Maximum accepted byte length of one header action identity or type token.
const HISTORICAL_HEADER_TOKEN_MAX_BYTES: usize = 96;

/// Maximum accepted byte length of one retained legacy metadata scalar line.
const HISTORICAL_PREAMBLE_LINE_MAX_BYTES: usize = 128;

/// Maximum number of legacy metadata scalar lines consumed before reduction
/// stops, independent of the remaining body length.
const HISTORICAL_PREAMBLE_MAX_LINES: usize = 16;

/// Lowest accepted legacy shell termination signal number.
///
/// Zero means "no signal was delivered", so a historical `signal:` line must
/// name a real signal instead of the absence of one.
const HISTORICAL_SIGNAL_MIN: i32 = 1;

/// Highest accepted legacy shell termination signal number.
///
/// The historical producer wrote the raw `ExitStatusExt::signal()` value, an OS
/// `i32` that includes real-time signals outside the standard 1..=64 window, so
/// the accepted ceiling covers the full 8-bit signal number space rather than
/// the standard-signal subset. Zero, negative, and larger values stay rejected
/// by `historical_decimal_scalar_in_range`.
const HISTORICAL_SIGNAL_MAX: i32 = 255;

/// Legacy body markers that permanently end metadata-preamble reduction.
///
/// Historical producers wrote these markers immediately before a free-form
/// provider-visible body, so reduction must never resume after one of them.
const HISTORICAL_BODY_MARKERS: &[&str] = &["output:", "content:", "data:", "error:", "error_data:"];

/// Known-safe action error codes emitted by durable action-result producers.
///
/// Legacy replay retains an `error_code` line only when it names one of these
/// codes: arbitrary text in that position could be body content or a secret
/// that merely resembles metadata.
///
/// The set is the union of every code a durable producer passes to
/// `ActionResult::failed` or to a shell-transaction failure carrier
/// (`RuntimeShellTransactionActionFailure::code` and
/// `RuntimeNativeShellFailure::kind`), enumerated from the construction sites in
/// this crate and in the `mezzanine` product crate rather than guessed. A single
/// producer-side authority is not reachable from this lower crate: the product
/// producers live in `mezzanine`, which depends on `mez-agent` and owns
/// `runtime_mezzanine_error_code`, `runtime_mcp_error_code`, and the runtime
/// failure carriers, so this literal list is the one place both crates can
/// share. The pinning regression test in `tests.rs` keeps this list equal to the
/// enumerated producer set so a future omission is visible in one place.
const HISTORICAL_SAFE_ERROR_CODES: &[&str] = &[
    "action_failed",
    "agent_aborted",
    "apply_patch_authority_changed",
    "apply_patch_execution_mode_changed",
    "apply_patch_hunk_context_mismatch",
    "apply_patch_hunk_mismatch",
    "apply_patch_payload_cap_exceeded",
    "apply_patch_read_transport_incomplete",
    "apply_patch_snapshot_byte_count_mismatch",
    "apply_patch_snapshot_checksum_mismatch",
    "apply_patch_transport_failed",
    "apply_patch_transport_incomplete",
    "apply_patch_unsafe_path",
    "apply_patch_validation_failed",
    "apply_patch_write_failed",
    "approval_denied",
    "approval_disapproved",
    "bubblewrap_path_resolution_failed",
    "bubblewrap_path_resolution_stale",
    "bubblewrap_pre_payload_failure",
    "bubblewrap_probe_identity_mismatch",
    "bubblewrap_probe_nonzero_exit",
    "bubblewrap_probe_output_mismatch",
    "bubblewrap_probe_output_truncated",
    "bubblewrap_probe_protocol_violation",
    "bubblewrap_probe_stale_identity",
    "bubblewrap_probe_timeout",
    "bubblewrap_probe_write_failed",
    "bubblewrap_status_invalid",
    "bubblewrap_status_mismatch",
    "cancelled",
    "config",
    "config_change_failed",
    "config_invalid",
    "conflict",
    "denied",
    "forbidden",
    "foreground_process_blocked_dispatch",
    "hook_blocked",
    "internal_error",
    "interrupted",
    "invalid_message_payload",
    "invalid_message_recipient",
    "invalid_params",
    "invalid_skill_name",
    "invalid_state",
    "invalidargs",
    "invalidstate",
    "io",
    "issue_dependency_validation_failed",
    "issue_store_unavailable",
    "issues_disabled",
    "macro_bridge_error",
    "macro_step_failed",
    "macro_step_ordering",
    "mcp_blacklisted",
    "mcp_invalid_args",
    "mcp_protocol_error",
    "mcp_schema_changed",
    "mcp_schema_unbound",
    "mcp_server_changed",
    "mcp_tool_error",
    "memory_disabled",
    "memory_store_unavailable",
    "message_recipient_forbidden",
    "method_not_found",
    "network_action_no_progress",
    "network_http_error",
    "network_request_failed",
    "not_found",
    "not_implemented",
    "notfound",
    "notimplemented",
    "pane_input_write_failed",
    "pane_not_ready",
    "permission_denied",
    "policy_forbidden",
    "rate_limited",
    "ratelimited",
    "readiness_probe_timeout",
    "sandbox_failure",
    "seatbelt_established_payload_incomplete",
    "seatbelt_pre_payload_failure",
    "seatbelt_probe_nonzero_exit",
    "seatbelt_probe_output_mismatch",
    "seatbelt_probe_output_truncated",
    "seatbelt_probe_protocol_violation",
    "seatbelt_probe_stale_identity",
    "seatbelt_probe_timeout",
    "seatbelt_probe_write_failed",
    "seatbelt_status_invalid",
    "seatbelt_status_mismatch",
    "shell_command_failed",
    "shell_dispatch_limit_exceeded",
    "shell_executable_not_os_verified",
    "shell_exit_nonzero",
    "shell_failed",
    "shell_identity_probe_failed",
    "shell_interrupted",
    "shell_protocol_violation",
    "shell_timeout",
    "shell_unavailable",
    "skill_catalog_already_requested",
    "skill_context_already_loaded",
    "skill_not_found",
    "timeout",
    "transport_error",
    "unauthorized",
    "unavailable",
    "unsupported",
    "unsupported_url_scheme",
    "user_cancelled",
    "user_only_host_access",
    "user_only_host_policy",
    "user_only_host_power_policy",
    "user_only_sandbox_policy",
    "user_only_transport_policy",
];

/// One recognized field of the contiguous legacy metadata preamble.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HistoricalPreambleField {
    /// Non-negative process exit code produced by the shell executor.
    ExitCode,
    /// Native termination signal number produced by the shell executor.
    Signal,
    /// `true`-only timeout flag emitted by the durable summary producer.
    TimedOut,
    /// `true`-only output truncation flag emitted by the durable summary producer.
    OutputTruncated,
    /// Known-safe action error identity.
    ErrorCode,
}

impl HistoricalPreambleField {
    /// Returns this field's duplicate-detection bit.
    fn duplicate_bit(self) -> u8 {
        match self {
            Self::ExitCode => 1 << 0,
            Self::Signal => 1 << 1,
            Self::TimedOut => 1 << 2,
            Self::OutputTruncated => 1 << 3,
            Self::ErrorCode => 1 << 4,
        }
    }
}

/// Returns a provider-safe projection of one durable or legacy tool entry.
///
/// Legacy replay retains only a validated `[action_result ...]` header and the
/// contiguous scalar preamble that the historical durable producer emitted,
/// with every scalar individually validated. Reduction stops permanently at
/// the first body marker, separator, unknown line, duplicate field, or invalid
/// scalar, so a free-form body line that merely resembles metadata can never be
/// promoted into provider context.
///
/// Content without a validated header has no bounded safe projection and is
/// omitted; blank input is absent as well. Callers that owe a provider protocol
/// a tool-result envelope keep that envelope with safe empty or reduced output
/// instead of replaying legacy bytes.
pub fn historical_tool_result_context_content(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut lines = trimmed.lines();
    let header = historical_action_result_header(lines.next()?)?;
    let mut retained = vec![header];
    let mut seen = 0u8;
    for line in lines.take(HISTORICAL_PREAMBLE_MAX_LINES) {
        if line == HISTORICAL_OUTPUT_OMITTED_MARKER {
            break;
        }
        if historical_line_is_body_marker(line) {
            break;
        }
        let Some(field) = historical_preamble_field(line) else {
            break;
        };
        if seen & field.duplicate_bit() != 0 {
            break;
        }
        seen |= field.duplicate_bit();
        retained.push(line.to_string());
    }
    retained.push(HISTORICAL_OUTPUT_OMITTED_MARKER.to_string());
    Some(retained.join("\n"))
}

/// Validates one legacy `[action_result <id> <type> <status>]` header line.
///
/// The historical producer emitted exactly one space-separated identity, action
/// type, and compact status name inside the brackets. Ambiguous header text is
/// omitted rather than retained.
fn historical_action_result_header(line: &str) -> Option<String> {
    if line.len() > HISTORICAL_HEADER_MAX_BYTES {
        return None;
    }
    let inner = line.strip_prefix("[action_result ")?.strip_suffix(']')?;
    let mut tokens = inner.split(' ');
    let action_id = tokens.next()?;
    let action_type = tokens.next()?;
    let status = tokens.next()?;
    if tokens.next().is_some() {
        return None;
    }
    if !historical_header_token_is_valid(action_id)
        || !historical_header_token_is_valid(action_type)
    {
        return None;
    }
    if !matches!(
        status,
        "rejected"
            | "blocked"
            | "denied"
            | "running"
            | "succeeded"
            | "failed"
            | "cancelled"
            | "timed_out"
            | "interrupted"
    ) {
        return None;
    }
    Some(line.to_string())
}

/// Returns whether one header token matches the historical identifier grammar.
///
/// Historical action identities and types were short printable ASCII tokens.
/// The accepted class is every printable non-space byte except the header
/// delimiters (brackets, quote, and backslash), so punctuation in a
/// model-supplied action id cannot turn a legitimate header into an omission for
/// a character that cannot carry a secret. Control characters and delimiters
/// stay rejected because they could restructure the header line, and the byte
/// ceiling still bounds how much text one token may carry.
fn historical_header_token_is_valid(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= HISTORICAL_HEADER_TOKEN_MAX_BYTES
        && token
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'[' | b']' | b'"' | b'\\'))
}

/// Returns whether one line begins a free-form legacy body.
fn historical_line_is_body_marker(line: &str) -> bool {
    HISTORICAL_BODY_MARKERS.iter().any(|marker| {
        line.strip_prefix(marker)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })
}

/// Parses one recognized legacy metadata scalar line.
///
/// Values are validated against the historical producer types and ranges, so an
/// out-of-range or malformed scalar stops reduction instead of retaining text
/// that may be body content.
fn historical_preamble_field(line: &str) -> Option<HistoricalPreambleField> {
    if line.len() > HISTORICAL_PREAMBLE_LINE_MAX_BYTES {
        return None;
    }
    if let Some(value) = line.strip_prefix("exit_code: ") {
        return historical_decimal_scalar_in_range(value, 0, 255)
            .then_some(HistoricalPreambleField::ExitCode);
    }
    if let Some(value) = line.strip_prefix("signal: ") {
        return historical_decimal_scalar_in_range(
            value,
            HISTORICAL_SIGNAL_MIN,
            HISTORICAL_SIGNAL_MAX,
        )
        .then_some(HistoricalPreambleField::Signal);
    }
    if line == "timed_out: true" {
        return Some(HistoricalPreambleField::TimedOut);
    }
    if line == "output_truncated: true" {
        return Some(HistoricalPreambleField::OutputTruncated);
    }
    if let Some(code) = line.strip_prefix("error_code: ") {
        return HISTORICAL_SAFE_ERROR_CODES
            .contains(&code)
            .then_some(HistoricalPreambleField::ErrorCode);
    }
    None
}

/// Returns whether one scalar is a short non-negative decimal in range.
///
/// Historical producers emitted plain JSON integers, so signs, whitespace, hex,
/// digit separators, and over-long digit runs are rejected.
fn historical_decimal_scalar_in_range(value: &str, min: i32, max: i32) -> bool {
    !value.is_empty()
        && value.len() <= 3
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value
            .parse::<i32>()
            .is_ok_and(|parsed| (min..=max).contains(&parsed))
}

/// Executes the `action_result_context_content` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn action_result_context_content(result: &ActionResult) -> String {
    let facts = ActionResultContextView::new(result);
    let mut lines = vec![facts.header_line()];
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
    if facts.is_shell_observation {
        append_shell_action_result_context(&facts, &mut lines);
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
fn append_shell_action_result_context(
    facts: &ActionResultContextView<'_>,
    lines: &mut Vec<String>,
) {
    let result = facts.result;
    let structured_object = facts.structured_object();
    if let Some(command) = structured_object
        .and_then(|object| object.get("command"))
        .and_then(serde_json::Value::as_str)
        .filter(|command| !command.trim().is_empty())
    {
        lines.push(format!("command: {command}"));
    }
    append_json_scalar_line(
        lines,
        "execution_transport",
        structured_object.and_then(|object| object.get("execution_transport")),
    );
    append_json_scalar_line(
        lines,
        "sent_to_pane",
        structured_object.and_then(|object| object.get("sent_to_pane")),
    );
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
        append_json_scalar_line(lines, "stream", observation.get("stream"));
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
        if let Some(assessment) = observation
            .get("sandbox_assessment")
            .and_then(serde_json::Value::as_object)
        {
            append_json_scalar_line(lines, "sandbox_backend", assessment.get("backend"));
            append_json_scalar_line(lines, "sandbox_status", assessment.get("sandbox_status"));
            append_json_scalar_line(lines, "sandbox_assessment_class", assessment.get("class"));
            append_json_scalar_line(
                lines,
                "sandbox_assessment_decision",
                assessment.get("decision"),
            );
            append_json_scalar_line(
                lines,
                "sandbox_assessment_confidence",
                assessment.get("confidence"),
            );
            append_json_scalar_line(
                lines,
                "sandbox_assessment_rationale",
                assessment.get("rationale"),
            );
            append_json_scalar_line(
                lines,
                "sandbox_restriction_id",
                assessment.get("restriction_id"),
            );
            append_true_bool_line(
                lines,
                "sandboxed_recovery_exhausted",
                assessment.get("sandboxed_recovery_exhausted"),
            );
            append_true_bool_line(
                lines,
                "partial_effect_warning",
                assessment.get("partial_effect_warning"),
            );
            append_json_scalar_line(
                lines,
                "automatic_replay",
                assessment.get("automatic_replay"),
            );
        }
    }
    let output = shell_action_result_output_for_context(result, terminal_observation);
    if !output.trim().is_empty() {
        lines.push("output:".to_string());
        lines.push(output);
    }
    if facts.has_structured_data() {
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
