//! Provider-independent Claude Code CLI policy.
//!
//! This module owns deterministic Claude Code request/response policy,
//! including prompt and schema shaping, session identity, print-envelope and
//! MAAP parsing, usage accounting, diagnostic redaction, and retry
//! classification. The root adapter retains subprocess invocation, temporary
//! settings files, process-local locking, and product error projection.

use crate::{
    MaapBatch, ModelRequest, ModelTokenUsage, ProviderErrorKind, ProviderMalformedOutputError,
    ProviderRequestAssemblyError, ProviderRequestAssemblyResult, ProviderResponseError,
    maap_action_batch_schema, parse_maap_action_batch_json_for_turn,
    provider_malformed_output_error,
};

/// Corrective instruction used after Claude Code returns malformed MAAP output.
pub const CLAUDE_CODE_MAAP_RETRY_INSTRUCTION: &str = "Your previous response was invalid for Mezzanine because it did not satisfy the required structured output contract. Return only one validated Mezzanine MAAP action batch that matches the provided JSON schema, with no surrounding prose.";
/// Corrective instruction used after Claude Code returns an empty response.
pub const CLAUDE_CODE_EMPTY_OUTPUT_RETRY_INSTRUCTION: &str = "Your previous response was empty. Return only one validated Mezzanine MAAP action batch that matches the provided JSON schema, with no surrounding prose.";
/// Claude Code tool name required for schema-backed structured output.
pub const CLAUDE_CODE_STRUCTURED_OUTPUT_TOOL: &str = "StructuredOutput";
/// Maximum Claude Code diagnostic bytes retained at the provider boundary.
const CLAUDE_CODE_DIAGNOSTIC_LIMIT: usize = 8192;

/// Deterministic fields decoded from one Claude Code print-mode completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCodeOutput {
    /// Visible assistant text from the print-mode result envelope.
    pub assistant_text: String,
    /// Serialized structured-output object when Claude returned one.
    pub structured_output: Option<String>,
    /// Provider-reported token accounting.
    pub usage: ModelTokenUsage,
}

/// Failure returned while interpreting Claude Code output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeCodeResponseError {
    /// The provider envelope was invalid, incomplete, or internally inconsistent.
    Provider(ProviderResponseError),
    /// Provider-authored MAAP output did not satisfy the action contract.
    MalformedOutput(ProviderMalformedOutputError),
}

impl ClaudeCodeResponseError {
    /// Returns the stable provider error category.
    pub fn kind(&self) -> ProviderErrorKind {
        match self {
            Self::Provider(_) => ProviderErrorKind::InvalidState,
            Self::MalformedOutput(error) => error.kind(),
        }
    }

    /// Returns the human-readable parser diagnostic.
    pub fn message(&self) -> &str {
        match self {
            Self::Provider(error) => error.message(),
            Self::MalformedOutput(error) => error.message(),
        }
    }

    /// Returns retained raw provider output when available.
    pub fn provider_raw_text(&self) -> Option<&str> {
        match self {
            Self::Provider(error) => error.provider_raw_text(),
            Self::MalformedOutput(error) => Some(error.raw_text()),
        }
    }

    /// Returns the sanitized provider failure payload when available.
    pub fn provider_failure_json(&self) -> Option<&str> {
        match self {
            Self::Provider(error) => error.provider_failure_json(),
            Self::MalformedOutput(error) => Some(error.provider_failure_json()),
        }
    }
}

impl std::fmt::Display for ClaudeCodeResponseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(formatter),
            Self::MalformedOutput(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ClaudeCodeResponseError {}

impl From<ProviderResponseError> for ClaudeCodeResponseError {
    fn from(error: ProviderResponseError) -> Self {
        Self::Provider(error)
    }
}

impl From<ProviderMalformedOutputError> for ClaudeCodeResponseError {
    fn from(error: ProviderMalformedOutputError) -> Self {
        Self::MalformedOutput(error)
    }
}

/// Result returned while interpreting Claude Code output.
pub type ClaudeCodeResponseResult<T> = Result<T, ClaudeCodeResponseError>;

/// Builds the Claude system prompt passed through the dedicated CLI channel.
pub fn claude_code_system_prompt(
    request: &ModelRequest,
    retry_instruction: Option<&str>,
) -> String {
    let mut prompt = String::new();
    append_claude_code_instruction_framing(&mut prompt, request, retry_instruction);
    if request.interaction_kind == crate::ModelInteractionKind::AutoSizing {
        prompt.push_str("Claude Code internal router boundary:\n");
        prompt.push_str("This turn is a hidden preflight classification step for Mezzanine's internal auto-sizing router, not a user-visible assistant response. Do not answer the user's task, continue the conversation, call native tools, or emit MAAP actions. When Mezzanine provides a JSON schema, use StructuredOutput only as a carrier for the router decision object.\n");
        prompt.push_str("Output contract:\n");
        prompt.push_str("Return exactly one JSON object matching the requested schema with version, size, reasoning_effort, confidence, and rationale. Do not include prose, markdown, code fences, or task-completion text before or after that JSON object.\n");
    } else {
        prompt.push_str("Claude Code direct-tool boundary:\n");
        prompt.push_str("Perform all requested operations through Mezzanine MAAP actions only. Do not call Claude Code native tools for local files, commands, web, MCP, subagents, config, memory, issue operations, or task delegation. Use only the response channel Mezzanine requested for this turn. When a MAAP schema is present, the only Claude Code tool Mezzanine may allow is StructuredOutput, and it is only a carrier for returning the MAAP action batch.\n");
        prompt.push_str("MAAP action mapping:\n");
        prompt.push_str("Translate Claude Code tool intents into Mezzanine actions: inspect files, search text, run commands, builds, tests, or git through shell_command; edit file contents through apply_patch; fetch explicit URLs through fetch_url when available; search the web through web_search when available; delegate work or message subagents through spawn_agent or send_message when available; request a missing capability with request_capability instead of calling a native Claude tool or asking the user for task-local facts you can safely discover.\n");
        prompt.push_str("Output contract:\n");
        prompt.push_str("Respond with the validated Mezzanine MAAP action batch text only. Do not run tools or mutate files directly. Native Claude Code tools must not be used except as needed to emit the requested MAAP action batch.\n");
    }
    prompt
}

/// Builds the text prompt passed to the Claude Code CLI stdin channel.
pub fn claude_code_prompt(request: &ModelRequest, _retry_instruction: Option<&str>) -> String {
    let mut prompt = String::new();
    append_claude_code_ordered_context(&mut prompt, request);
    prompt
}

/// Appends authoritative instructions to Claude's system-prompt channel.
fn append_claude_code_instruction_framing(
    prompt: &mut String,
    request: &ModelRequest,
    retry_instruction: Option<&str>,
) {
    let has_instruction_framing = request.messages.iter().any(|message| {
        matches!(
            message.role,
            crate::ModelMessageRole::System | crate::ModelMessageRole::Developer
        )
    }) || retry_instruction.is_some();
    if !has_instruction_framing {
        return;
    }
    prompt.push_str("Instruction framing for Claude Code:\n");
    for message in &request.messages {
        let label = match message.role {
            crate::ModelMessageRole::System => Some("System instruction"),
            crate::ModelMessageRole::Developer => Some("Developer instruction"),
            crate::ModelMessageRole::User
            | crate::ModelMessageRole::Assistant
            | crate::ModelMessageRole::Tool
            | crate::ModelMessageRole::Context => None,
        };
        if let Some(label) = label {
            append_claude_code_section(prompt, label, &message.content);
        }
    }
    if let Some(retry_instruction) = retry_instruction {
        append_claude_code_section(prompt, "Developer retry instruction", retry_instruction);
    }
}

/// Appends every non-instruction message in canonical order.
///
/// Claude Code accepts one stdin prompt rather than a native message array, so
/// role labels delimit events without selecting and relocating a "current"
/// user message. System and developer instructions travel through the CLI's
/// dedicated system-prompt channel.
fn append_claude_code_ordered_context(prompt: &mut String, request: &ModelRequest) {
    if request.interaction_kind == crate::ModelInteractionKind::AutoSizing {
        prompt.push_str("Ordered conversation context to classify (do not answer it):\n");
    } else {
        prompt.push_str("Ordered Mezzanine conversation context:\n");
    }
    let mut wrote_message = false;
    for message in &request.messages {
        if matches!(
            message.role,
            crate::ModelMessageRole::System | crate::ModelMessageRole::Developer
        ) {
            continue;
        }
        let label = match message.role {
            crate::ModelMessageRole::User => "User message",
            crate::ModelMessageRole::Assistant => "Assistant message",
            crate::ModelMessageRole::Tool => "Tool result",
            crate::ModelMessageRole::Context => "Mezzanine context (not user-authored)",
            crate::ModelMessageRole::System | crate::ModelMessageRole::Developer => unreachable!(),
        };
        append_claude_code_section(prompt, label, &message.content);
        wrote_message = true;
    }
    if !wrote_message {
        prompt.push_str("No conversation messages were provided; follow the system prompt.\n\n");
    }
}

/// Appends one labeled plaintext prompt section.
fn append_claude_code_section(prompt: &mut String, label: &str, content: &str) {
    prompt.push_str(label);
    prompt.push_str(":\n");
    prompt.push_str(content);
    prompt.push_str("\n\n");
}

/// Parses Claude Code MAAP output from schema-enforced responses.
pub fn parse_claude_code_maap_output(
    request: &ModelRequest,
    raw_text: &str,
    structured_output: Option<&str>,
) -> ClaudeCodeResponseResult<MaapBatch> {
    if let Some(structured_output) = structured_output {
        return parse_maap_action_batch_json_for_turn(
            structured_output,
            &request.turn_id,
            &request.agent_id,
        )
        .map_err(|error| {
            provider_malformed_output_error(
                ProviderErrorKind::InvalidArgs,
                error.message(),
                structured_output,
            )
            .into()
        });
    }
    let detail = raw_text.trim();
    let message = if claude_code_login_required_detail(detail) {
        "Claude Code response did not include structured_output for a schema-enforced MAAP turn because Claude Code is not logged in; run `claude /login` in a non-bare Claude CLI session or configure headless auth for provider-style invocations".to_string()
    } else if detail.is_empty() {
        "Claude Code response did not include structured_output for a schema-enforced MAAP turn"
            .to_string()
    } else {
        "Claude Code response did not include structured_output for a schema-enforced MAAP turn; check Claude Code login and StructuredOutput permissions"
            .to_string()
    };
    Err(ProviderResponseError::invalid_state(message)
        .with_provider_raw_text(raw_text.to_string())
        .into())
}

/// Builds the provider error used when Claude Code exits successfully but
/// produces no assistant output.
pub fn claude_code_empty_output_error(stderr: &str) -> ProviderResponseError {
    ProviderResponseError::invalid_state("Claude Code subprocess produced no assistant output")
        .with_provider_raw_text(stderr.to_string())
}

/// Parses assistant text, structured output, and token usage from Claude Code
/// JSON print output.
pub fn parse_claude_code_json_output(
    stdout: &str,
) -> Result<ClaudeCodeOutput, ProviderResponseError> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(ClaudeCodeOutput {
            assistant_text: String::new(),
            structured_output: None,
            usage: ModelTokenUsage::default(),
        });
    }
    let result_state = serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .map(|value| claude_code_json_result_state(&value))
        .unwrap_or("unknown");
    let envelope = match serde_json::from_str::<ClaudeCodeJsonEnvelope>(trimmed) {
        Ok(envelope) => envelope,
        Err(error) => {
            if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
                return Ok(ClaudeCodeOutput {
                    assistant_text: trimmed.to_string(),
                    structured_output: None,
                    usage: ModelTokenUsage::default(),
                });
            }
            return Err(ProviderResponseError::invalid_state(format!(
                "Claude Code JSON output could not be parsed: {error}"
            ))
            .with_provider_raw_text(trimmed.to_string()));
        }
    };
    let result = envelope.result.unwrap_or_default();
    if envelope.is_error {
        let subtype = envelope.subtype.as_deref().unwrap_or("unknown");
        let detail = result.trim();
        let base_message = match envelope.envelope_type.as_deref() {
            Some(envelope_type) if !envelope_type.is_empty() && !detail.is_empty() => {
                format!(
                    "Claude Code JSON output reported an error ({envelope_type}/{subtype}): {detail}"
                )
            }
            Some(envelope_type) if !envelope_type.is_empty() => {
                format!("Claude Code JSON output reported an error ({envelope_type}/{subtype})")
            }
            _ if !detail.is_empty() => {
                format!("Claude Code JSON output reported an error ({subtype}): {detail}")
            }
            _ => format!("Claude Code JSON output reported an error ({subtype})"),
        };
        let message = if claude_code_login_required_detail(detail) {
            format!(
                "{base_message}; run `claude /login` in a non-bare Claude CLI session or configure headless auth for provider-style invocations"
            )
        } else {
            base_message
        };
        return Err(ProviderResponseError::invalid_state(message)
            .with_provider_raw_text(trimmed.to_string()));
    }
    if envelope.structured_output.is_none()
        && envelope
            .permission_denials
            .iter()
            .any(|denial| denial.tool_name.as_deref() == Some(CLAUDE_CODE_STRUCTURED_OUTPUT_TOOL))
    {
        let detail = result.trim();
        let message = if detail.is_empty() {
            "Claude Code JSON output denied StructuredOutput permission required for schema-enforced responses".to_string()
        } else {
            format!(
                "Claude Code JSON output denied StructuredOutput permission required for schema-enforced responses: {detail}"
            )
        };
        return Err(ProviderResponseError::invalid_state(message)
            .with_provider_raw_text(trimmed.to_string()));
    }
    let structured_output = envelope
        .structured_output
        .map(|value| serde_json::to_string(&value))
        .transpose()
        .map_err(|error| {
            ProviderResponseError::invalid_state(format!(
                "Claude Code JSON structured output could not be serialized: {error}"
            ))
            .with_provider_raw_text(trimmed.to_string())
        })?;
    if result.trim().is_empty() && structured_output.is_none() {
        let message = match result_state {
            "missing" => {
                "Claude Code JSON output did not contain result text or structured output; result field was missing"
            }
            "null" => {
                "Claude Code JSON output did not contain result text or structured output; result field was null"
            }
            "empty" => {
                "Claude Code JSON output did not contain result text or structured output; result field was empty"
            }
            "blank" => {
                "Claude Code JSON output did not contain result text or structured output; result field was blank"
            }
            _ => "Claude Code JSON output did not contain result text or structured output",
        };
        return Err(ProviderResponseError::invalid_state(message)
            .with_provider_raw_text(trimmed.to_string()));
    }
    Ok(ClaudeCodeOutput {
        assistant_text: result.trim().to_string(),
        structured_output,
        usage: ModelTokenUsage {
            input_tokens: envelope.usage.input_tokens.unwrap_or(0),
            output_tokens: envelope.usage.output_tokens.unwrap_or(0),
            reasoning_tokens: 0,
            cached_input_tokens: envelope.usage.cache_read_input_tokens,
            cache_write_input_tokens: envelope.usage.cache_creation_input_tokens,
        },
    })
}

/// Validates Claude Code auto-sizing output and returns normalized router JSON.
pub fn validate_claude_code_auto_sizing_output(
    raw_text: &str,
    structured_output: Option<&str>,
) -> Result<String, ProviderResponseError> {
    let validation_input = structured_output.unwrap_or(raw_text);
    let candidate = structured_output
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| claude_code_extract_top_level_json_object(raw_text))
        .ok_or_else(|| {
            ProviderResponseError::invalid_state(
                "Claude Code auto-sizing response must be valid router JSON",
            )
            .with_provider_raw_text(raw_text.to_string())
        })?;
    let value =
        serde_json::from_str::<ClaudeCodeAutoSizingDecision>(&candidate).map_err(|error| {
            ProviderResponseError::invalid_state(format!(
                "Claude Code auto-sizing response must be valid router JSON: {error}"
            ))
            .with_provider_raw_text(validation_input.to_string())
        })?;
    if value.version != 1 {
        return Err(ProviderResponseError::invalid_state(
            "Claude Code auto-sizing response returned unsupported version",
        )
        .with_provider_raw_text(validation_input.to_string()));
    }
    if !matches!(value.size.as_str(), "small" | "medium" | "large") {
        return Err(ProviderResponseError::invalid_state(
            "Claude Code auto-sizing response returned unknown size bucket",
        )
        .with_provider_raw_text(validation_input.to_string()));
    }
    if !matches!(
        value.reasoning_effort.as_str(),
        "low" | "medium" | "high" | "xhigh"
    ) {
        return Err(ProviderResponseError::invalid_state(
            "Claude Code auto-sizing response returned unsupported reasoning effort",
        )
        .with_provider_raw_text(validation_input.to_string()));
    }
    if !(0.0..=1.0).contains(&value.confidence) {
        return Err(ProviderResponseError::invalid_state(
            "Claude Code auto-sizing response returned confidence outside 0..=1",
        )
        .with_provider_raw_text(validation_input.to_string()));
    }
    let rationale = value.rationale.trim();
    if rationale.is_empty() || rationale.chars().any(char::is_control) {
        return Err(ProviderResponseError::invalid_state(
            "Claude Code auto-sizing response returned invalid rationale",
        )
        .with_provider_raw_text(validation_input.to_string()));
    }
    Ok(candidate)
}

/// Selects the corrective instruction for an empty or malformed MAAP result.
pub fn claude_code_corrective_retry_instruction(assistant_text: &str) -> &'static str {
    if assistant_text.is_empty() {
        CLAUDE_CODE_EMPTY_OUTPUT_RETRY_INSTRUCTION
    } else {
        CLAUDE_CODE_MAAP_RETRY_INSTRUCTION
    }
}

/// Redacts common secret-bearing fragments from subprocess diagnostics.
pub fn redact_claude_code_text(value: &str) -> String {
    let mut ranges = Vec::new();
    collect_bearer_secret_ranges(value, &mut ranges);
    collect_secret_key_value_ranges(value, &mut ranges);
    collect_openai_secret_key_ranges(value, &mut ranges);
    apply_secret_redactions(value, ranges)
}

/// Bounds diagnostic text retained from Claude Code stderr.
pub fn bound_claude_code_text(value: &str) -> String {
    if value.len() <= CLAUDE_CODE_DIAGNOSTIC_LIMIT {
        return value.to_string();
    }
    let mut end = CLAUDE_CODE_DIAGNOSTIC_LIMIT;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}... [truncated, {} bytes total]",
        &value[..end],
        value.len()
    )
}

/// Stores the Claude Code JSON usage counters relevant to provider accounting.
#[derive(Debug, Default, serde::Deserialize)]
struct ClaudeCodeJsonUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

/// Stores one Claude Code permission denial from a print-mode JSON envelope.
#[derive(Debug, Default, serde::Deserialize)]
struct ClaudeCodePermissionDenial {
    tool_name: Option<String>,
}

/// Stores the Claude Code JSON envelope shape used by print-mode output.
#[derive(Debug, serde::Deserialize)]
struct ClaudeCodeJsonEnvelope {
    #[serde(rename = "type")]
    envelope_type: Option<String>,
    subtype: Option<String>,
    #[serde(default)]
    is_error: bool,
    result: Option<String>,
    structured_output: Option<serde_json::Value>,
    #[serde(default)]
    permission_denials: Vec<ClaudeCodePermissionDenial>,
    #[serde(default)]
    usage: ClaudeCodeJsonUsage,
}

/// Stores the validated Claude Code auto-sizing router response shape.
#[derive(Debug, serde::Deserialize)]
struct ClaudeCodeAutoSizingDecision {
    version: u64,
    size: String,
    reasoning_effort: String,
    confidence: f64,
    rationale: String,
}

/// Classifies the raw JSON `result` field before typed envelope parsing erases
/// absent, null, empty, and blank distinctions.
fn claude_code_json_result_state(value: &serde_json::Value) -> &'static str {
    match value.get("result") {
        None => "missing",
        Some(serde_json::Value::Null) => "null",
        Some(serde_json::Value::String(text)) if text.is_empty() => "empty",
        Some(serde_json::Value::String(text)) if text.trim().is_empty() => "blank",
        Some(serde_json::Value::String(_)) => "present",
        Some(_) => "non_string",
    }
}

/// Reports whether a Claude Code detail requires interactive login or alternate
/// headless authentication.
fn claude_code_login_required_detail(detail: &str) -> bool {
    let lower = detail.trim().to_ascii_lowercase();
    lower.contains("not logged in") || lower.contains("please run /login")
}

/// Returns the first valid top-level JSON object embedded in assistant text.
fn claude_code_extract_top_level_json_object(text: &str) -> Option<String> {
    for (start, ch) in text.char_indices() {
        if ch != '{' {
            continue;
        }
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, ch) in text[start..].char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                } else {
                    match ch {
                        '\\' => escaped = true,
                        '"' => in_string = false,
                        _ => {}
                    }
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let end = start + offset + ch.len_utf8();
                        let candidate = text[start..end].trim();
                        if matches!(
                            serde_json::from_str::<serde_json::Value>(candidate),
                            Ok(serde_json::Value::Object(_))
                        ) {
                            return Some(candidate.to_string());
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Records ranges for bearer credentials, including compact URL separators.
fn collect_bearer_secret_ranges(value: &str, ranges: &mut Vec<(usize, usize)>) {
    let mut index = 0;
    while index < value.len() {
        if !ascii_case_insensitive_starts_with_at(value, index, "bearer")
            || !has_secret_left_boundary(value, index)
        {
            index = next_char_boundary(value, index);
            continue;
        }
        let mut credential_start = index + "bearer".len();
        let Some(separator) = value[credential_start..].chars().next() else {
            break;
        };
        if !separator.is_whitespace()
            && separator != '+'
            && !ascii_case_insensitive_starts_with_at(value, credential_start, "%20")
            && !ascii_case_insensitive_starts_with_at(value, credential_start, "%2b")
        {
            index = credential_start;
            continue;
        }
        credential_start = skip_bearer_separators(value, credential_start);
        let credential_end = secret_value_end(value, credential_start);
        if credential_end > credential_start {
            ranges.push((index, credential_end));
            index = credential_end;
        } else {
            index = credential_start;
        }
    }
}

/// Records ranges for explicit secret-bearing key/value fields.
fn collect_secret_key_value_ranges(value: &str, ranges: &mut Vec<(usize, usize)>) {
    const SECRET_KEYS: &[&str] = &[
        "access_token",
        "access-token",
        "authorization",
        "api_key",
        "api-key",
        "apikey",
        "secret",
        "token",
    ];
    let mut index = 0;
    while index < value.len() {
        let mut matched = false;
        for key in SECRET_KEYS {
            if !ascii_case_insensitive_starts_with_at(value, index, key)
                || !has_secret_left_boundary(value, index)
            {
                continue;
            }
            let Some(value_start) = secret_key_value_start(value, index + key.len()) else {
                continue;
            };
            let value_end = secret_value_end(value, value_start);
            if value_end > value_start {
                ranges.push((index, value_end));
                index = value_end;
                matched = true;
                break;
            }
        }
        if !matched {
            index = next_char_boundary(value, index);
        }
    }
}

/// Records ranges for OpenAI-style `sk-...` tokens.
fn collect_openai_secret_key_ranges(value: &str, ranges: &mut Vec<(usize, usize)>) {
    let mut index = 0;
    while index < value.len() {
        if !value[index..].starts_with("sk-") || !has_secret_left_boundary(value, index) {
            index = next_char_boundary(value, index);
            continue;
        }
        let credential_start = index + "sk-".len();
        if value[credential_start..]
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric())
        {
            index = credential_start;
            continue;
        }
        let credential_end = secret_value_end(value, credential_start);
        ranges.push((index, credential_end));
        index = credential_end;
    }
}

/// Returns the start of a matched key/value secret value.
fn secret_key_value_start(value: &str, mut cursor: usize) -> Option<usize> {
    if matches!(value[cursor..].chars().next(), Some('\'' | '"')) {
        cursor = next_char_boundary(value, cursor);
    }
    if !matches!(value[cursor..].chars().next(), Some(':' | '=')) {
        return None;
    }
    cursor = next_char_boundary(value, cursor);
    while matches!(value[cursor..].chars().next(), Some(ch) if ch.is_whitespace()) {
        cursor = next_char_boundary(value, cursor);
    }
    if matches!(value[cursor..].chars().next(), Some('\'' | '"')) {
        cursor = next_char_boundary(value, cursor);
    }
    Some(cursor)
}

/// Skips separators between `Bearer` and its credential.
fn skip_bearer_separators(value: &str, mut cursor: usize) -> usize {
    loop {
        if ascii_case_insensitive_starts_with_at(value, cursor, "%20")
            || ascii_case_insensitive_starts_with_at(value, cursor, "%2b")
        {
            cursor += 3;
            continue;
        }
        match value[cursor..].chars().next() {
            Some(ch) if ch.is_whitespace() || ch == '+' => cursor += ch.len_utf8(),
            _ => break cursor,
        }
    }
}

/// Returns the end of a compact secret value.
fn secret_value_end(value: &str, start: usize) -> usize {
    let mut end = start;
    for (offset, ch) in value[start..].char_indices() {
        if ch.is_whitespace() || matches!(ch, '\'' | '"' | ',' | ';' | ')' | ']' | '}' | '<' | '>')
        {
            return start + offset;
        }
        end = start + offset + ch.len_utf8();
    }
    end
}

/// Applies sorted and merged redaction ranges to diagnostic text.
fn apply_secret_redactions(value: &str, mut ranges: Vec<(usize, usize)>) -> String {
    ranges.retain(|(start, end)| start < end);
    if ranges.is_empty() {
        return value.to_string();
    }
    ranges.sort_unstable_by_key(|(start, _)| *start);
    let mut merged = Vec::<(usize, usize)>::new();
    for (start, end) in ranges {
        if let Some((_, last_end)) = merged.last_mut()
            && start <= *last_end
        {
            *last_end = (*last_end).max(end);
            continue;
        }
        merged.push((start, end));
    }
    let mut redacted = String::with_capacity(value.len());
    let mut cursor = 0;
    for (start, end) in merged {
        redacted.push_str(&value[cursor..start]);
        redacted.push_str("[redacted]");
        cursor = end;
    }
    redacted.push_str(&value[cursor..]);
    redacted
}

/// Reports whether an ASCII marker starts at one byte index.
fn ascii_case_insensitive_starts_with_at(value: &str, start: usize, needle: &str) -> bool {
    start.checked_add(needle.len()).is_some_and(|end| {
        end <= value.len() && value.as_bytes()[start..end].eq_ignore_ascii_case(needle.as_bytes())
    })
}

/// Reports whether a secret marker is outside an identifier-like word.
fn has_secret_left_boundary(value: &str, start: usize) -> bool {
    value[..start]
        .chars()
        .next_back()
        .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
}

/// Advances one UTF-8 scalar from a valid string boundary.
fn next_char_boundary(value: &str, index: usize) -> usize {
    value[index..]
        .chars()
        .next()
        .map_or(value.len(), |ch| index + ch.len_utf8())
}

/// Builds the Claude Code JSON schema argument for MAAP action-batch turns.
pub fn claude_code_maap_json_schema(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<String> {
    serde_json::to_string(&maap_action_batch_schema(
        &request.allowed_actions,
        &request.available_mcp_tools,
    ))
    .map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "Claude Code MAAP JSON schema could not be serialized: {error}"
        ))
    })
}

/// Builds the Claude Code JSON schema argument for internal auto-sizing
/// router turns.
pub fn claude_code_auto_sizing_json_schema() -> ProviderRequestAssemblyResult<String> {
    serialize_claude_code_schema(
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["version", "size", "reasoning_effort", "confidence", "rationale"],
            "properties": {
                "version": { "type": "integer", "enum": [1] },
                "size": { "type": "string", "enum": ["small", "medium", "large"] },
                "reasoning_effort": { "type": "string", "enum": ["low", "medium", "high", "xhigh"] },
                "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                "rationale": { "type": "string", "minLength": 1 }
            }
        }),
        "auto-sizing",
    )
}

/// Builds the Claude Code JSON schema argument for internal macro-step judge
/// decisions.
pub fn claude_code_macro_judge_json_schema() -> ProviderRequestAssemblyResult<String> {
    serialize_claude_code_schema(
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": [
                "version", "outcome", "step_success", "rationale", "adapted_prompt",
                "user_message"
            ],
            "properties": {
                "version": { "type": "integer", "enum": [1] },
                "outcome": {
                    "type": "string",
                    "enum": [
                        "continue", "continue_with_adapted_prompt", "stop_failure",
                        "finish_success"
                    ]
                },
                "step_success": { "type": "boolean" },
                "rationale": { "type": "string", "minLength": 1 },
                "adapted_prompt": { "type": ["string", "null"] },
                "user_message": { "type": ["string", "null"] }
            }
        }),
        "macro judge",
    )
}

/// Serializes one deterministic Claude Code structured-output schema.
fn serialize_claude_code_schema(
    schema: serde_json::Value,
    interaction: &str,
) -> ProviderRequestAssemblyResult<String> {
    serde_json::to_string(&schema).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "Claude Code {interaction} JSON schema could not be serialized: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AllowedActionSet, ContextSourceKind, ModelInteractionKind, ModelMessage, ModelMessageRole,
    };

    /// Verifies Claude Code structured interactions expose closed schemas with
    /// the exact required router and macro-judge fields.
    #[test]
    fn claude_code_internal_json_schemas_are_strict() {
        let auto: serde_json::Value =
            serde_json::from_str(&claude_code_auto_sizing_json_schema().unwrap()).unwrap();
        let judge: serde_json::Value =
            serde_json::from_str(&claude_code_macro_judge_json_schema().unwrap()).unwrap();

        assert_eq!(auto["additionalProperties"], false);
        assert_eq!(
            auto["properties"]["version"]["enum"],
            serde_json::json!([1])
        );
        assert_eq!(judge["additionalProperties"], false);
        assert_eq!(
            judge["properties"]["outcome"]["enum"],
            serde_json::json!([
                "continue",
                "continue_with_adapted_prompt",
                "stop_failure",
                "finish_success"
            ])
        );
    }

    /// Verifies Claude Code MAAP schema construction follows the request's
    /// active action surface instead of exposing disallowed actions.
    #[test]
    fn claude_code_maap_json_schema_tracks_allowed_actions() {
        let request = claude_request();
        let schema = claude_code_maap_json_schema(&request).unwrap();

        assert!(schema.contains("say"), "{schema}");
        assert!(!schema.contains("shell_command"), "{schema}");
    }

    /// Verifies Claude Code prompt construction respects the CLI's single
    /// stdin prompt contract by framing authoritative instructions separately
    /// and preserving every conversation event in canonical order.
    #[test]
    fn claude_code_prompt_preserves_canonical_message_order() {
        let mut request = claude_request();
        request.messages = vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "System authority.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Developer,
                source: ContextSourceKind::DeveloperInstruction,
                placement: crate::ContextPlacement::StablePrefix,
                content: "Developer authority.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "Earlier user turn.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Assistant,
                source: ContextSourceKind::TranscriptAssistant,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "Earlier assistant turn.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Tool,
                source: ContextSourceKind::ActionResult,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "Prior tool result.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "Final user request.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Context,
                source: ContextSourceKind::RuntimeHint,
                placement: crate::ContextPlacement::EphemeralTail,
                content: "Current factual live state.".to_string(),
            },
        ];

        let system_prompt =
            claude_code_system_prompt(&request, Some("Retry with a valid MAAP batch."));
        let prompt = claude_code_prompt(&request, Some("Retry with a valid MAAP batch."));

        assert!(system_prompt.contains("Instruction framing for Claude Code:"));
        assert!(system_prompt.contains("System instruction:\nSystem authority."));
        assert!(system_prompt.contains("Developer instruction:\nDeveloper authority."));
        assert!(
            system_prompt.contains("Developer retry instruction:\nRetry with a valid MAAP batch.")
        );
        assert!(system_prompt.contains("Claude Code direct-tool boundary:"));
        assert!(system_prompt.contains("MAAP action mapping:"));
        assert!(system_prompt.contains("edit file contents through apply_patch"));
        assert!(prompt.contains("Ordered Mezzanine conversation context:"));
        assert!(prompt.contains("User message:\nEarlier user turn."));
        assert!(prompt.contains("Assistant message:\nEarlier assistant turn."));
        assert!(prompt.contains("Tool result:\nPrior tool result."));
        assert!(prompt.contains("User message:\nFinal user request."));
        let earlier_user = prompt.find("Earlier user turn.").unwrap();
        let assistant = prompt.find("Earlier assistant turn.").unwrap();
        let evidence = prompt.find("Prior tool result.").unwrap();
        let final_user = prompt.find("Final user request.").unwrap();
        let live_state = prompt.find("Current factual live state.").unwrap();
        assert!(earlier_user < assistant);
        assert!(assistant < evidence);
        assert!(evidence < final_user);
        assert!(final_user < live_state);
        assert!(!prompt.contains("System instruction:"));
        assert!(!prompt.contains("Developer instruction:"));
        assert!(!prompt.contains("Developer retry instruction:"));
    }

    /// Verifies Claude Code auto-sizing prompt construction frames the latest
    /// user text as router input rather than a task to execute.
    #[test]
    fn claude_code_auto_sizing_prompt_frames_hidden_router_preflight() {
        let mut request = claude_request();
        request.interaction_kind = ModelInteractionKind::AutoSizing;
        request.allowed_actions = AllowedActionSet::from_actions([]);
        request.messages = vec![
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "Earlier user turn.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Assistant,
                source: ContextSourceKind::TranscriptAssistant,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "Earlier assistant turn.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "Implement the runtime change.".to_string(),
            },
        ];

        let system_prompt = claude_code_system_prompt(&request, None);
        let prompt = claude_code_prompt(&request, None);

        assert!(system_prompt.contains("Claude Code internal router boundary:"));
        assert!(system_prompt.contains("hidden preflight classification step"));
        assert!(system_prompt.contains("Do not answer the user's task"));
        assert!(
            system_prompt.contains("Return exactly one JSON object matching the requested schema")
        );
        assert!(!system_prompt.contains("MAAP action mapping:"));
        assert!(prompt.contains("Ordered conversation context to classify (do not answer it):"));
        assert!(prompt.contains("User message:\nImplement the runtime change."));
    }

    /// Verifies corrective retry guidance stays in Claude Code's authoritative
    /// system channel instead of being duplicated after ordered chronology.
    #[test]
    fn claude_code_retry_instruction_stays_in_system_prompt() {
        let request = claude_request();
        let system_prompt =
            claude_code_system_prompt(&request, Some(CLAUDE_CODE_MAAP_RETRY_INSTRUCTION));
        let prompt = claude_code_prompt(&request, Some(CLAUDE_CODE_MAAP_RETRY_INSTRUCTION));

        assert!(
            system_prompt
                .contains("Developer retry instruction:\nYour previous response was invalid")
        );
        assert!(!prompt.contains("Developer retry instruction:"));
    }

    /// Verifies instruction-only Claude Code requests produce an explicit
    /// empty-conversation marker without recreating role-tagged transcript.
    #[test]
    fn claude_code_prompt_handles_instruction_only_requests() {
        let system_prompt = claude_code_system_prompt(&claude_request(), None);
        let prompt = claude_code_prompt(&claude_request(), None);

        assert!(system_prompt.contains("Developer instruction:\nReturn a final say action."));
        assert!(
            prompt.contains("No conversation messages were provided; follow the system prompt.")
        );
        assert!(!prompt.contains("Developer instruction:"));
        assert!(!prompt.contains("<developer>"));
    }

    /// Verifies Claude Code diagnostic redaction no longer uses broad
    /// substring matching that corrupts ordinary words and metric names.
    #[test]
    fn claude_code_redaction_preserves_non_secret_substrings() {
        let redacted = redact_claude_code_text(
            "tokenization access_token_count secretive authorization_count",
        );

        assert_eq!(
            redacted,
            "tokenization access_token_count secretive authorization_count"
        );
    }

    /// Verifies Claude Code diagnostic redaction catches structured and compact
    /// secret shapes without depending on whitespace-delimited tokens.
    #[test]
    fn claude_code_redaction_targets_secret_patterns() {
        let redacted = redact_claude_code_text(
            r#"login auth="Bearer+sk-abc123" api_key=sk-def456 token=secret-value"#,
        );

        assert!(redacted.contains("login"), "{redacted}");
        assert!(redacted.contains("[redacted]"), "{redacted}");
        assert!(!redacted.contains("Bearer"), "{redacted}");
        assert!(!redacted.contains("sk-abc123"), "{redacted}");
        assert!(!redacted.contains("sk-def456"), "{redacted}");
        assert!(!redacted.contains("secret-value"), "{redacted}");
    }

    /// Verifies Claude Code JSON usage parsing preserves omitted cache counters
    /// separately from explicit zero cache counters.
    ///
    /// Cache accounting displays provider-omitted fields as unknown while
    /// explicit provider zeros should remain known zero values. This protects
    /// the provider boundary from collapsing `None` and `Some(0)` as Claude
    /// Code print-mode usage envelopes evolve.
    #[test]
    fn claude_code_json_usage_distinguishes_missing_and_zero_cache_counters() {
        let omitted = r#"{"result":"hello","usage":{"input_tokens":2,"output_tokens":12}}"#;
        let explicit_zero = r#"{"result":"hello","usage":{"input_tokens":2,"output_tokens":12,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#;

        let omitted_usage = parse_claude_code_json_output(omitted).unwrap().usage;
        let explicit_zero_usage = parse_claude_code_json_output(explicit_zero).unwrap().usage;

        assert_eq!(omitted_usage.input_tokens, 2);
        assert_eq!(omitted_usage.cached_input_tokens, None);
        assert_eq!(omitted_usage.cached_input_tokens_display(), "unknown");
        assert_eq!(omitted_usage.cached_input_hit_ratio_display(), "unknown");
        assert_eq!(omitted_usage.cache_write_input_tokens, None);
        assert_eq!(explicit_zero_usage.input_tokens, 2);
        assert_eq!(explicit_zero_usage.cached_input_tokens, Some(0));
        assert_eq!(explicit_zero_usage.cached_input_tokens_display(), "0");
        assert_eq!(
            explicit_zero_usage.cached_input_hit_ratio_display(),
            "0.00%"
        );
        assert_eq!(explicit_zero_usage.cache_write_input_tokens, Some(0));
    }

    /// Verifies empty Claude Code JSON envelopes report which `result` state
    /// was observed so diagnostics can distinguish missing, null, empty, and
    /// blank provider output when no structured output is available.
    #[test]
    fn claude_code_json_output_distinguishes_absent_null_and_empty_result_states() {
        for (raw, expected) in [
            (
                r#"{"usage":{"input_tokens":2,"output_tokens":12}}"#,
                "result field was missing",
            ),
            (
                r#"{"result":null,"usage":{"input_tokens":2,"output_tokens":12}}"#,
                "result field was null",
            ),
            (
                r#"{"result":"","usage":{"input_tokens":2,"output_tokens":12}}"#,
                "result field was empty",
            ),
            (
                r#"{"result":"   ","usage":{"input_tokens":2,"output_tokens":12}}"#,
                "result field was blank",
            ),
        ] {
            let error = parse_claude_code_json_output(raw).unwrap_err();

            assert!(error.message().contains(expected), "{}", error.message());
            assert_eq!(error.provider_raw_text(), Some(raw));
        }
    }

    /// Verifies malformed Claude Code JSON output is reported at the JSON
    /// parser boundary instead of being downgraded to assistant text with
    /// default token usage.
    #[test]
    fn claude_code_json_output_rejects_malformed_json() {
        let raw = r#"{"type":"result","result":"unterminated""#;

        let error = parse_claude_code_json_output(raw).unwrap_err();

        assert!(
            error.message().contains("JSON output could not be parsed"),
            "{}",
            error.message()
        );
        assert_eq!(error.provider_raw_text(), Some(raw));
    }

    /// Verifies Claude Code JSON error envelopes surface as provider errors
    /// instead of falling through to MAAP parsing as assistant content.
    ///
    /// Claude Code can report structured-output failures in its JSON envelope.
    /// The provider must stop at that boundary so upstream retry or handoff
    /// logic receives the provider error instead of a misleading MAAP failure.
    #[test]
    fn claude_code_json_output_rejects_error_envelopes() {
        let raw = r#"{"type":"result","subtype":"error_max_structured_output_retries","is_error":true,"result":"schema failed","usage":{"input_tokens":2,"output_tokens":12}}"#;

        let error = parse_claude_code_json_output(raw).unwrap_err();

        assert!(
            error.message().contains(
                "reported an error (result/error_max_structured_output_retries): schema failed"
            ),
            "{}",
            error.message()
        );
        assert_eq!(error.provider_raw_text(), Some(raw));
    }

    /// Verifies login-required Claude Code JSON error envelopes tell the user
    /// how to satisfy authentication for interactive or provider-style runs.
    #[test]
    fn claude_code_json_output_reports_login_guidance() {
        let raw = r#"{"type":"result","subtype":"error_auth","is_error":true,"result":"Not logged in; Please run /login","usage":{"input_tokens":2,"output_tokens":12}}"#;

        let error = parse_claude_code_json_output(raw).unwrap_err();

        assert!(
            error.message().contains("run `claude /login`"),
            "{}",
            error.message()
        );
        assert!(
            error
                .message()
                .contains("configure headless auth for provider-style invocations"),
            "{}",
            error.message()
        );
        assert_eq!(error.provider_raw_text(), Some(raw));
    }

    /// Builds a minimal Claude Code request for deterministic policy tests.
    fn claude_request() -> ModelRequest {
        ModelRequest {
            provider: "claude-code".to_string(),
            model: "claude-sonnet-test".to_string(),
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
            messages: vec![ModelMessage {
                role: ModelMessageRole::Developer,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "Return a final say action.".to_string(),
            }],
        }
    }
}
