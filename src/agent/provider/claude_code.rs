//! Claude Code subprocess provider adapter.
//!
//! This module owns the experimental provider boundary for Claude Code
//! subscription-backed execution. The adapter invokes the local `claude` CLI in
//! noninteractive print mode for each request, captures bounded stdout/stderr,
//! and returns assistant text for the normal Mezzanine MAAP parsing path without
//! granting Claude Code direct tool execution or filesystem mutation authority.

use super::{
    AsyncModelProvider, MezError, ModelInteractionKind, ModelMessageRole, ModelRequest,
    ModelResponse, ModelTokenUsage, ProviderModelCatalog, Result,
    parse_fenced_maap_action_batch_for_turn, provider_maap_parse_error, validate_non_empty,
};
use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Executable name used for Claude Code subprocess requests.
const CLAUDE_CODE_PROGRAM: &str = "claude";
/// Corrective instruction used after Claude Code returns malformed MAAP output.
const CLAUDE_CODE_MAAP_RETRY_INSTRUCTION: &str = "Your previous response was invalid for Mezzanine because it did not contain one valid mezzanine-action-json action batch. Return only one validated Mezzanine MAAP action batch fenced in ```mezzanine-action-json``` with no surrounding prose.";
/// Corrective instruction used after Claude Code returns an empty response.
const CLAUDE_CODE_EMPTY_OUTPUT_RETRY_INSTRUCTION: &str = "Your previous response was empty. Return only one validated Mezzanine MAAP action batch fenced in ```mezzanine-action-json``` with no surrounding prose.";
/// Maximum stderr bytes retained in provider diagnostics.
const CLAUDE_CODE_STDERR_LIMIT: usize = 8192;

/// Captures one Claude Code subprocess completion for retry and validation.
struct ClaudeCodeSubprocessOutput {
    stdout: String,
    stderr: String,
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

/// Experimental Claude Code subprocess provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCodeProvider {
    provider_id: String,
    program: String,
    timeout_ms: u64,
}

impl ClaudeCodeProvider {
    /// Creates a Claude Code provider for one configured provider id.
    pub fn new(provider_id: impl Into<String>, timeout_ms: u64) -> Result<Self> {
        let provider_id = provider_id.into();
        validate_non_empty("Claude Code provider id", &provider_id)?;
        if timeout_ms == 0 {
            return Err(MezError::invalid_args(
                "Claude Code provider timeout must be greater than zero",
            ));
        }
        Ok(Self {
            provider_id,
            program: CLAUDE_CODE_PROGRAM.to_string(),
            timeout_ms,
        })
    }

    /// Overrides the Claude Code executable path for focused subprocess tests.
    #[cfg(test)]
    fn with_program(mut self, program: impl Into<String>) -> Result<Self> {
        let program = program.into();
        validate_non_empty("Claude Code provider program", &program)?;
        self.program = program;
        Ok(self)
    }

    /// Returns the configured provider id guarded by this provider instance.
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }
}

impl AsyncModelProvider for ClaudeCodeProvider {
    /// Returns the configured provider id.
    fn provider_id(&self) -> &str {
        self.provider_id()
    }

    /// Claude Code does not expose a stable model-catalog API to Mezzanine.
    fn list_models_async<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderModelCatalog>> + Send + 'a>> {
        Box::pin(async move {
            Err(MezError::invalid_state(format!(
                "Claude Code provider `{}` uses configured models",
                self.provider_id
            )))
        })
    }

    /// Sends one request through a bounded noninteractive Claude Code subprocess.
    fn send_request_async<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        Box::pin(async move {
            if request.provider != self.provider_id {
                return Err(MezError::invalid_args(
                    "Claude Code provider received a request for a different provider",
                ));
            }
            let raw_text = if request.interaction_kind == ModelInteractionKind::AutoSizing {
                let output = run_claude_code_subprocess(
                    &self.program,
                    &request.model,
                    &claude_code_prompt(request, None),
                    self.timeout_ms,
                )
                .await?;
                if output.stdout.is_empty() {
                    return Err(claude_code_empty_output_error(&output.stderr));
                }
                validate_claude_code_auto_sizing_output(&output.stdout)?;
                output.stdout
            } else {
                run_claude_code_request_with_corrective_retry(
                    &self.program,
                    request,
                    self.timeout_ms,
                )
                .await?
            };
            let action_batch = if request.interaction_kind == ModelInteractionKind::AutoSizing {
                None
            } else {
                let Some(batch) = parse_fenced_maap_action_batch_for_turn(
                    &raw_text,
                    &request.turn_id,
                    &request.agent_id,
                )
                .map_err(|error| provider_maap_parse_error(error, &raw_text))?
                else {
                    return Err(provider_maap_parse_error(
                        MezError::invalid_args(
                            "Claude Code response must contain a mezzanine-action-json block",
                        ),
                        &raw_text,
                    ));
                };
                Some(batch)
            };
            Ok(ModelResponse {
                provider: self.provider_id.clone(),
                model: request.model.clone(),
                raw_text,
                usage: ModelTokenUsage::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch,
                provider_transcript_events: Vec::new(),
            })
        })
    }
}

/// Builds the text prompt passed to the Claude Code CLI.
fn claude_code_prompt(request: &ModelRequest, retry_instruction: Option<&str>) -> String {
    let final_user_index = request
        .messages
        .iter()
        .rposition(|message| message.role == ModelMessageRole::User);
    let mut prompt = String::new();

    append_claude_code_instruction_framing(&mut prompt, request, retry_instruction);
    append_claude_code_prior_context(&mut prompt, request, final_user_index);
    append_claude_code_current_user_prompt(&mut prompt, request, final_user_index);

    prompt.push_str("Output contract:\n");
    prompt.push_str(
        "Respond with the validated Mezzanine MAAP action batch text only. Do not run tools or mutate files directly.\n",
    );
    prompt
}

/// Appends system, developer, and retry instructions as provider framing rather
/// than pretending Claude Code `--print` accepts a structured role transcript.
fn append_claude_code_instruction_framing(
    prompt: &mut String,
    request: &ModelRequest,
    retry_instruction: Option<&str>,
) {
    let has_instruction_framing = request.messages.iter().any(|message| {
        matches!(
            message.role,
            ModelMessageRole::System | ModelMessageRole::Developer
        )
    }) || retry_instruction.is_some();

    if !has_instruction_framing {
        return;
    }

    prompt.push_str("Instruction framing for Claude Code:\n");
    for message in &request.messages {
        let label = match message.role {
            ModelMessageRole::System => Some("System instruction"),
            ModelMessageRole::Developer => Some("Developer instruction"),
            ModelMessageRole::User | ModelMessageRole::Assistant | ModelMessageRole::Tool => None,
        };
        if let Some(label) = label {
            append_claude_code_section(prompt, label, &message.content);
        }
    }
    if let Some(retry_instruction) = retry_instruction {
        append_claude_code_section(prompt, "Developer retry instruction", retry_instruction);
    }
}

/// Appends non-instruction messages other than the final user turn as bounded
/// conversation context so they are not presented as the current user prompt.
fn append_claude_code_prior_context(
    prompt: &mut String,
    request: &ModelRequest,
    final_user_index: Option<usize>,
) {
    let mut wrote_heading = false;
    for (index, message) in request.messages.iter().enumerate() {
        if Some(index) == final_user_index
            || matches!(
                message.role,
                ModelMessageRole::System | ModelMessageRole::Developer
            )
        {
            continue;
        }

        if !wrote_heading {
            prompt.push_str("Prior conversation context (not the current user request):\n");
            wrote_heading = true;
        }

        let label = match message.role {
            ModelMessageRole::User => "Previous user message",
            ModelMessageRole::Assistant => "Previous assistant message",
            ModelMessageRole::Tool => "Previous tool result",
            ModelMessageRole::System | ModelMessageRole::Developer => unreachable!(),
        };
        append_claude_code_section(prompt, label, &message.content);
    }
}

/// Appends the last user message as Claude Code's current prompt, falling back
/// to instruction-only execution when callers provide no explicit user turn.
fn append_claude_code_current_user_prompt(
    prompt: &mut String,
    request: &ModelRequest,
    final_user_index: Option<usize>,
) {
    prompt.push_str("Current user request:\n");
    if let Some(index) = final_user_index {
        prompt.push_str(&request.messages[index].content);
    } else {
        prompt.push_str("Follow the instruction framing above.");
    }
    prompt.push_str("\n\n");
}

/// Appends one labeled plaintext section to the prompt with clear delimiters.
fn append_claude_code_section(prompt: &mut String, label: &str, content: &str) {
    prompt.push_str(label);
    prompt.push_str(":\n");
    prompt.push_str(content);
    prompt.push_str("\n\n");
}

/// Reports whether Claude Code output should receive one corrective retry.
fn claude_code_output_needs_corrective_retry(request: &ModelRequest, raw_text: &str) -> bool {
    if raw_text.is_empty() || request.interaction_kind == ModelInteractionKind::AutoSizing {
        return raw_text.is_empty();
    }
    match parse_fenced_maap_action_batch_for_turn(raw_text, &request.turn_id, &request.agent_id) {
        Ok(Some(_)) => false,
        Ok(None) | Err(_) => true,
    }
}

/// Builds the provider error used when Claude Code exits successfully but
/// produces no assistant output.
fn claude_code_empty_output_error(stderr: &str) -> MezError {
    MezError::invalid_state("Claude Code subprocess produced no assistant output")
        .with_provider_raw_text(stderr.to_string())
}

/// Validates that Claude Code auto-sizing output matches the router JSON
/// contract expected by the runtime parser.
fn validate_claude_code_auto_sizing_output(raw_text: &str) -> Result<()> {
    let value =
        serde_json::from_str::<ClaudeCodeAutoSizingDecision>(raw_text.trim()).map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code auto-sizing response must be valid router JSON: {error}"
            ))
            .with_provider_raw_text(raw_text.to_string())
        })?;
    if value.version != 1 {
        return Err(MezError::invalid_state(
            "Claude Code auto-sizing response returned unsupported version",
        )
        .with_provider_raw_text(raw_text.to_string()));
    }
    if !matches!(value.size.as_str(), "small" | "medium" | "large") {
        return Err(MezError::invalid_state(
            "Claude Code auto-sizing response returned unknown size bucket",
        )
        .with_provider_raw_text(raw_text.to_string()));
    }
    if !matches!(
        value.reasoning_effort.as_str(),
        "low" | "medium" | "high" | "xhigh"
    ) {
        return Err(MezError::invalid_state(
            "Claude Code auto-sizing response returned unsupported reasoning effort",
        )
        .with_provider_raw_text(raw_text.to_string()));
    }
    if !(0.0..=1.0).contains(&value.confidence) {
        return Err(MezError::invalid_state(
            "Claude Code auto-sizing response returned confidence outside 0..=1",
        )
        .with_provider_raw_text(raw_text.to_string()));
    }
    let rationale = value.rationale.trim();
    if rationale.is_empty() || rationale.chars().any(char::is_control) {
        return Err(MezError::invalid_state(
            "Claude Code auto-sizing response returned invalid rationale",
        )
        .with_provider_raw_text(raw_text.to_string()));
    }
    Ok(())
}

/// Runs one bounded Claude Code request and applies one corrective retry for
/// empty or malformed MAAP output.
async fn run_claude_code_request_with_corrective_retry(
    program: &str,
    request: &ModelRequest,
    timeout_ms: u64,
) -> Result<String> {
    let first_output = run_claude_code_subprocess(
        program,
        &request.model,
        &claude_code_prompt(request, None),
        timeout_ms,
    )
    .await?;
    if !claude_code_output_needs_corrective_retry(request, &first_output.stdout) {
        return Ok(first_output.stdout);
    }
    let retry_instruction = if first_output.stdout.is_empty() {
        CLAUDE_CODE_EMPTY_OUTPUT_RETRY_INSTRUCTION
    } else {
        CLAUDE_CODE_MAAP_RETRY_INSTRUCTION
    };
    let retry_output = run_claude_code_subprocess(
        program,
        &request.model,
        &claude_code_prompt(request, Some(retry_instruction)),
        timeout_ms,
    )
    .await?;
    if retry_output.stdout.is_empty() {
        return Err(claude_code_empty_output_error(&retry_output.stderr));
    }
    Ok(retry_output.stdout)
}

/// Invokes Claude Code in print mode with direct tool use disabled.
async fn run_claude_code_subprocess(
    program: &str,
    model: &str,
    prompt: &str,
    timeout_ms: u64,
) -> Result<ClaudeCodeSubprocessOutput> {
    let mut spawn_attempt = 0;
    let mut child = loop {
        match Command::new(program)
            .arg("--print")
            .arg("--model")
            .arg(model)
            .arg("--disallowedTools")
            .arg("*")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => break child,
            Err(error) if claude_code_spawn_error_is_transient(&error) && spawn_attempt == 0 => {
                spawn_attempt += 1;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => {
                let retry_hint = if claude_code_spawn_error_is_transient(&error) {
                    "; you can retry the request"
                } else {
                    ""
                };
                return Err(MezError::invalid_state(format!(
                    "Claude Code subprocess failed to start: {}{}",
                    redact_claude_code_text(&error.to_string()),
                    retry_hint
                )));
            }
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes()).await.map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code subprocess stdin write failed: {}; you can retry the request",
                redact_claude_code_text(&error.to_string())
            ))
        })?;
        stdin.shutdown().await.map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code subprocess stdin shutdown failed: {}; you can retry the request",
                redact_claude_code_text(&error.to_string())
            ))
        })?;
    }

    let output = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output())
        .await
        .map_err(|_| {
            MezError::invalid_state(format!(
                "Claude Code subprocess timed out after {timeout_ms}ms; you can retry the request"
            ))
        })?
        .map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code subprocess wait failed: {}; you can retry the request",
                redact_claude_code_text(&error.to_string())
            ))
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = redact_claude_code_text(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        return Err(MezError::invalid_state(format!(
            "Claude Code subprocess exited with status {}: {}",
            output.status,
            bound_claude_code_text(&stderr)
        ))
        .with_provider_raw_text(stderr));
    }
    Ok(ClaudeCodeSubprocessOutput { stdout, stderr })
}

/// Reports whether a subprocess spawn failure is likely transient and worth
/// one immediate retry before surfacing a provider setup failure.
fn claude_code_spawn_error_is_transient(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(26)
}

/// Redacts common secret-bearing fragments from subprocess diagnostics.
fn redact_claude_code_text(value: &str) -> String {
    let mut ranges = Vec::new();
    collect_bearer_secret_ranges(value, &mut ranges);
    collect_secret_key_value_ranges(value, &mut ranges);
    collect_openai_secret_key_ranges(value, &mut ranges);
    apply_secret_redactions(value, ranges)
}

/// Records ranges for bearer credentials, including compact URL-style
/// separators that whitespace tokenization would miss.
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

/// Records ranges for explicit secret-bearing key/value fields while avoiding
/// substring matches such as `tokenization` or `access_token_count`.
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

/// Records ranges for OpenAI-style `sk-...` secret tokens embedded in compact
/// diagnostics.
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

/// Returns the start index of a key/value secret value after a matched key.
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

/// Skips separators that can appear between `Bearer` and its credential.
fn skip_bearer_separators(value: &str, mut cursor: usize) -> usize {
    loop {
        if ascii_case_insensitive_starts_with_at(value, cursor, "%20")
            || ascii_case_insensitive_starts_with_at(value, cursor, "%2b")
        {
            cursor += 3;
            continue;
        }
        match value[cursor..].chars().next() {
            Some(ch) if ch.is_whitespace() || ch == '+' => {
                cursor += ch.len_utf8();
            }
            _ => break cursor,
        }
    }
}

/// Returns the end of a compact secret value without consuming surrounding
/// structured punctuation.
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

/// Reports whether an ASCII secret marker starts at a byte index.
fn ascii_case_insensitive_starts_with_at(value: &str, start: usize, needle: &str) -> bool {
    start.checked_add(needle.len()).is_some_and(|end| {
        end <= value.len() && value.as_bytes()[start..end].eq_ignore_ascii_case(needle.as_bytes())
    })
}

/// Reports whether a secret marker is not embedded inside an identifier-like
/// word.
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

/// Bounds diagnostic text retained from Claude Code stderr.
fn bound_claude_code_text(value: &str) -> String {
    if value.len() <= CLAUDE_CODE_STDERR_LIMIT {
        return value.to_string();
    }
    let mut end = CLAUDE_CODE_STDERR_LIMIT;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}... [truncated, {} bytes total]",
        &value[..end],
        value.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AllowedActionSet, ContextSourceKind, ModelMessage, ModelRequest, ProviderErrorRetryClass,
        provider_error_retry_class,
    };
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

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

    /// Verifies executable-busy subprocess spawn failures are treated as
    /// transient so parallel test fixtures and real CLI upgrades can recover
    /// with one bounded retry.
    #[test]
    fn claude_code_spawn_error_classifies_executable_busy_as_transient() {
        let error = std::io::Error::from_raw_os_error(26);

        assert!(claude_code_spawn_error_is_transient(&error));
    }

    /// Verifies Claude Code prompt construction respects the CLI's single
    /// stdin prompt contract by framing authoritative instructions separately,
    /// preserving prior turns only as context, and isolating the final user
    /// message as the current request.
    #[test]
    fn claude_code_prompt_isolates_final_user_request() {
        let mut request = claude_request();
        request.messages = vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::UserInstruction,
                content: "System authority.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Developer,
                source: ContextSourceKind::UserInstruction,
                content: "Developer authority.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                content: "Earlier user turn.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Assistant,
                source: ContextSourceKind::RuntimeHint,
                content: "Earlier assistant turn.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Tool,
                source: ContextSourceKind::ActionResult,
                content: "Prior tool result.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                content: "Final user request.".to_string(),
            },
        ];

        let prompt = claude_code_prompt(&request, Some("Retry with a valid MAAP batch."));

        assert!(
            prompt.contains("Instruction framing for Claude Code:"),
            "{prompt}"
        );
        assert!(
            prompt.contains("System instruction:\nSystem authority."),
            "{prompt}"
        );
        assert!(
            prompt.contains("Developer instruction:\nDeveloper authority."),
            "{prompt}"
        );
        assert!(
            prompt.contains("Developer retry instruction:\nRetry with a valid MAAP batch."),
            "{prompt}"
        );
        assert!(
            prompt.contains("Prior conversation context (not the current user request):"),
            "{prompt}"
        );
        assert!(
            prompt.contains("Previous user message:\nEarlier user turn."),
            "{prompt}"
        );
        assert!(
            prompt.contains("Previous assistant message:\nEarlier assistant turn."),
            "{prompt}"
        );
        assert!(
            prompt.contains("Previous tool result:\nPrior tool result."),
            "{prompt}"
        );
        assert!(
            prompt.contains("Current user request:\nFinal user request."),
            "{prompt}"
        );
        assert!(!prompt.contains("<system>"), "{prompt}");
        assert!(!prompt.contains("<developer>"), "{prompt}");
        assert!(!prompt.contains("<assistant>"), "{prompt}");
        assert!(!prompt.contains("<tool>"), "{prompt}");
    }

    /// Verifies instruction-only Claude Code requests still produce a current
    /// request section instead of recreating role-tagged transcript blocks.
    #[test]
    fn claude_code_prompt_handles_instruction_only_requests() {
        let prompt = claude_code_prompt(&claude_request(), None);

        assert!(
            prompt.contains("Developer instruction:\nReturn a final say action."),
            "{prompt}"
        );
        assert!(
            prompt.contains("Current user request:\nFollow the instruction framing above."),
            "{prompt}"
        );
        assert!(!prompt.contains("<developer>"), "{prompt}");
    }

    /// Verifies that Claude Code subprocess output is parsed as MAAP and that
    /// the adapter invokes a model-only print request with direct tools denied.
    #[tokio::test]
    async fn claude_code_provider_parses_print_output_and_denies_direct_tools() {
        let fixture = ClaudeCodeFixture::new("success");
        fixture.write_claude_script(
            r#"#!/bin/sh
printf '%s\n' "$@" > "$0.args"
cat > "$0.stdin"
cat <<'EOF'
```mezzanine-action-json
{
  "protocol": "maap/1",
  "turn_id": "turn-1",
  "agent_id": "agent-1",
  "rationale": "return final text",
  "actions": [
    {
      "id": "say-1",
      "type": "say",
      "status": "final",
      "rationale": "Reply",
      "text": "hello"
    }
  ],
  "final": true
}
```
EOF
"#,
        );
        let provider = fixture.provider(1_000);

        let response = provider
            .send_request_async(&claude_request())
            .await
            .unwrap();

        assert_eq!(response.provider, "claude-code");
        assert_eq!(response.model, "claude-sonnet-test");
        assert!(response.action_batch.is_some());
        let args = fs::read_to_string(fixture.program.with_extension("args")).unwrap();
        assert!(args.contains("--print"), "{args}");
        assert!(args.contains("--model"), "{args}");
        assert!(args.contains("claude-sonnet-test"), "{args}");
        assert!(args.contains("--disallowedTools"), "{args}");
        assert!(args.contains('*'), "{args}");
        let stdin = fs::read_to_string(fixture.program.with_extension("stdin")).unwrap();
        assert!(stdin.contains("Developer instruction:"), "{stdin}");
        assert!(
            stdin.contains("Do not run tools or mutate files directly"),
            "{stdin}"
        );
    }

    /// Verifies Claude Code subprocess prompts are fully delivered and closed
    /// before waiting, so subprocesses that read stdin to EOF do not observe a
    /// truncated prompt or hang behind buffered writer state.
    #[tokio::test]
    async fn claude_code_provider_closes_stdin_after_prompt_write() {
        let fixture = ClaudeCodeFixture::new("stdin-eof");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat > "$0.stdin"
wc -c < "$0.stdin" > "$0.stdin-bytes"
cat <<'EOF'
```mezzanine-action-json
{
  "protocol": "maap/1",
  "turn_id": "turn-1",
  "agent_id": "agent-1",
  "rationale": "return final text",
  "actions": [
    {
      "id": "say-1",
      "type": "say",
      "status": "final",
      "rationale": "Reply",
      "text": "hello"
    }
  ],
  "final": true
}
```
EOF
"#,
        );
        let provider = fixture.provider(1_000);

        let response = provider
            .send_request_async(&claude_request())
            .await
            .unwrap();

        assert!(response.action_batch.is_some());
        let stdin = fs::read_to_string(fixture.program.with_extension("stdin")).unwrap();
        let recorded_len = fs::read_to_string(fixture.program.with_extension("stdin-bytes"))
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap();
        assert_eq!(recorded_len, stdin.len());
        assert!(stdin.ends_with("directly.\n"), "{stdin}");
    }

    /// Verifies missing Claude Code executables are classified as provider
    /// setup failures instead of panicking or falling through to MAAP parsing.
    #[tokio::test]
    async fn claude_code_provider_reports_missing_binary() {
        let provider = ClaudeCodeProvider::new("claude-code", 1_000)
            .unwrap()
            .with_program("/tmp/mez-definitely-missing-claude-code")
            .unwrap();

        let error = provider
            .send_request_async(&claude_request())
            .await
            .unwrap_err();

        assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
        assert!(
            error.message().contains("failed to start"),
            "{}",
            error.message()
        );
    }

    /// Verifies nonzero Claude Code exits keep bounded, redacted stderr as raw
    /// provider text so auth/login failures can be diagnosed safely.
    #[tokio::test]
    async fn claude_code_provider_redacts_nonzero_stderr() {
        let fixture = ClaudeCodeFixture::new("nonzero");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
printf '%s\n' 'login failed token=secret-value authorization=Bearer abc' >&2
exit 7
"#,
        );
        let provider = fixture.provider(1_000);

        let error = provider
            .send_request_async(&claude_request())
            .await
            .unwrap_err();

        assert!(
            error.message().contains("exited with status"),
            "{}",
            error.message()
        );
        let raw = error.provider_raw_text().unwrap();
        assert!(raw.contains("login failed"), "{raw}");
        assert!(raw.contains("[redacted]"), "{raw}");
        assert!(!raw.contains("secret-value"), "{raw}");
        assert!(!raw.contains("Bearer"), "{raw}");
    }

    /// Verifies timeouts are surfaced as provider failures when the Claude Code
    /// subprocess does not complete within the configured request deadline.
    #[tokio::test]
    async fn claude_code_provider_reports_timeout() {
        let fixture = ClaudeCodeFixture::new("timeout");
        fixture.write_claude_script(
            r#"#!/bin/sh
sleep 1
"#,
        );
        let provider = fixture.provider(10);

        let error = provider
            .send_request_async(&claude_request())
            .await
            .unwrap_err();

        assert!(error.message().contains("timed out"), "{}", error.message());
        assert_eq!(
            provider_error_retry_class(&error),
            ProviderErrorRetryClass::RetryableTransport
        );
    }

    /// Verifies malformed Claude Code MAAP output gets one corrective retry
    /// before the provider returns a terminal parse failure.
    #[tokio::test]
    async fn claude_code_provider_retries_malformed_output_once() {
        let fixture = ClaudeCodeFixture::new("malformed-retry");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
count_file="$0.count"
count=0
if [ -f "$count_file" ]; then
    count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
if [ "$count" -eq 1 ]; then
    printf '%s\n' 'plain assistant text without a MAAP block'
    exit 0
fi
cat <<'EOF'
```mezzanine-action-json
{
  "protocol": "maap/1",
  "turn_id": "turn-1",
  "agent_id": "agent-1",
  "rationale": "return final text",
  "actions": [
    {
      "id": "say-1",
      "type": "say",
      "status": "final",
      "rationale": "Reply",
      "text": "hello"
    }
  ],
  "final": true
}
```
EOF
"#,
        );
        let provider = fixture.provider(1_000);

        let response = provider
            .send_request_async(&claude_request())
            .await
            .unwrap();

        assert!(response.action_batch.is_some());
        assert_eq!(
            fs::read_to_string(fixture.program.with_extension("count")).unwrap(),
            "2"
        );
    }

    /// Verifies malformed Claude Code output is preserved as provider raw text
    /// through the existing MAAP repair path.
    #[tokio::test]
    async fn claude_code_provider_preserves_malformed_output() {
        let fixture = ClaudeCodeFixture::new("malformed");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
printf '%s\n' 'plain assistant text without a MAAP block'
"#,
        );
        let provider = fixture.provider(1_000);

        let error = provider
            .send_request_async(&claude_request())
            .await
            .unwrap_err();

        assert_eq!(
            error.provider_raw_text(),
            Some("plain assistant text without a MAAP block")
        );
    }

    /// Verifies empty Claude Code stdout gets one corrective retry before the
    /// provider returns the successful MAAP response from the retry.
    #[tokio::test]
    async fn claude_code_provider_retries_empty_output_once() {
        let fixture = ClaudeCodeFixture::new("empty-output-retry");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
count_file="$0.count"
count=0
if [ -f "$count_file" ]; then
    count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
if [ "$count" -eq 1 ]; then
    exit 0
fi
cat <<'EOF'
```mezzanine-action-json
{
  "protocol": "maap/1",
  "turn_id": "turn-1",
  "agent_id": "agent-1",
  "rationale": "return final text",
  "actions": [
    {
      "id": "say-1",
      "type": "say",
      "status": "final",
      "rationale": "Reply",
      "text": "hello"
    }
  ],
  "final": true
}
```
EOF
"#,
        );
        let provider = fixture.provider(1_000);

        let response = provider
            .send_request_async(&claude_request())
            .await
            .unwrap();

        assert!(response.action_batch.is_some());
        assert_eq!(
            fs::read_to_string(fixture.program.with_extension("count")).unwrap(),
            "2"
        );
    }

    /// Verifies empty Claude Code stdout is classified as a provider failure
    /// and preserves redacted stderr so missing-login diagnostics remain
    /// available without leaking credentials.
    #[tokio::test]
    async fn claude_code_provider_reports_empty_output_with_redacted_stderr() {
        let fixture = ClaudeCodeFixture::new("empty-output");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
printf '%s\n' 'missing login authorization=Bearer abc token=secret-value' >&2
exit 0
"#,
        );
        let provider = fixture.provider(1_000);

        let error = provider
            .send_request_async(&claude_request())
            .await
            .unwrap_err();

        assert!(
            error.message().contains("produced no assistant output"),
            "{}",
            error.message()
        );
        let raw = error.provider_raw_text().unwrap();
        assert!(raw.contains("missing login"), "{raw}");
        assert!(raw.contains("[redacted]"), "{raw}");
        assert!(!raw.contains("secret-value"), "{raw}");
        assert!(!raw.contains("Bearer"), "{raw}");
    }

    /// Verifies Claude Code auto-sizing responses preserve valid router JSON
    /// without entering MAAP parsing.
    #[tokio::test]
    async fn claude_code_provider_preserves_valid_auto_sizing_json() {
        let fixture = ClaudeCodeFixture::new("auto-sizing-valid");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
cat <<'EOF'
{"version":1,"size":"medium","reasoning_effort":"high","confidence":0.82,"rationale":"coding task needs a medium model"}
EOF
"#,
        );
        let provider = fixture.provider(1_000);
        let mut request = claude_request();
        request.interaction_kind = ModelInteractionKind::AutoSizing;
        request.allowed_actions = AllowedActionSet::from_actions([]);

        let response = provider.send_request_async(&request).await.unwrap();

        assert_eq!(response.action_batch, None);
        assert_eq!(
            response.raw_text.trim(),
            "{\"version\":1,\"size\":\"medium\",\"reasoning_effort\":\"high\",\"confidence\":0.82,\"rationale\":\"coding task needs a medium model\"}"
        );
    }

    /// Verifies malformed Claude Code auto-sizing responses fail validation so
    /// the runtime does not accept garbage sizing output as success.
    #[tokio::test]
    async fn claude_code_provider_rejects_malformed_auto_sizing_output() {
        let fixture = ClaudeCodeFixture::new("auto-sizing-malformed");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
printf '%s\n' 'plain assistant text without router json'
"#,
        );
        let provider = fixture.provider(1_000);
        let mut request = claude_request();
        request.interaction_kind = ModelInteractionKind::AutoSizing;
        request.allowed_actions = AllowedActionSet::from_actions([]);

        let error = provider.send_request_async(&request).await.unwrap_err();

        assert!(
            error
                .message()
                .contains("auto-sizing response must be valid router JSON"),
            "{}",
            error.message()
        );
        assert_eq!(
            error.provider_raw_text(),
            Some("plain assistant text without router json")
        );
    }

    /// Verifies structurally invalid Claude Code auto-sizing JSON is rejected
    /// before the runtime consumes it as a routing decision.
    #[tokio::test]
    async fn claude_code_provider_rejects_invalid_auto_sizing_shape() {
        let fixture = ClaudeCodeFixture::new("auto-sizing-invalid-shape");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
cat <<'EOF'
{"version":1,"size":"giant","reasoning_effort":"high","confidence":1.5,"rationale":""}
EOF
"#,
        );
        let provider = fixture.provider(1_000);
        let mut request = claude_request();
        request.interaction_kind = ModelInteractionKind::AutoSizing;
        request.allowed_actions = AllowedActionSet::from_actions([]);

        let error = provider.send_request_async(&request).await.unwrap_err();

        assert!(
            error.message().contains("unknown size bucket"),
            "{}",
            error.message()
        );
        assert_eq!(
            error.provider_raw_text(),
            Some(
                "{\"version\":1,\"size\":\"giant\",\"reasoning_effort\":\"high\",\"confidence\":1.5,\"rationale\":\"\"}"
            )
        );
    }

    struct ClaudeCodeFixture {
        root: std::path::PathBuf,
        program: std::path::PathBuf,
    }

    impl ClaudeCodeFixture {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "mez-claude-code-{label}-{}-{}",
                std::process::id(),
                current_test_nonce()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self {
                program: root.join("claude"),
                root,
            }
        }

        fn write_claude_script(&self, script: &str) {
            let staged = self.program.with_extension("staged");
            fs::write(&staged, script).unwrap();
            let mut permissions = fs::metadata(&staged).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&staged, permissions).unwrap();
            fs::rename(&staged, &self.program).unwrap();
        }

        fn provider(&self, timeout_ms: u64) -> ClaudeCodeProvider {
            ClaudeCodeProvider::new("claude-code", timeout_ms)
                .unwrap()
                .with_program(self.program.to_string_lossy())
                .unwrap()
        }
    }

    impl Drop for ClaudeCodeFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

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
                content: "Return a final say action.".to_string(),
            }],
        }
    }

    fn current_test_nonce() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }
}
