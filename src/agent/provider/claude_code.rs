//! Claude Code subprocess provider adapter.
//!
//! This module owns the experimental provider boundary for Claude Code
//! subscription-backed execution. The adapter invokes the local `claude` CLI in
//! noninteractive print mode for each request, captures bounded stdout/stderr,
//! and returns assistant text for the normal Mezzanine MAAP parsing path without
//! granting Claude Code direct tool execution or filesystem mutation authority.

use super::schema::maap_action_batch_schema;
use super::{
    AsyncModelProvider, MaapBatch, MezError, ModelInteractionKind, ModelMessageRole, ModelRequest,
    ModelResponse, ModelTokenUsage, ProviderModelCatalog, Result,
    parse_maap_action_batch_json_for_turn, provider_maap_parse_error, validate_non_empty,
};
use sha2::Digest;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Executable name used for Claude Code subprocess requests.
const CLAUDE_CODE_PROGRAM: &str = "claude";
/// Corrective instruction used after Claude Code returns malformed MAAP output.
const CLAUDE_CODE_MAAP_RETRY_INSTRUCTION: &str = "Your previous response was invalid for Mezzanine because it did not satisfy the required structured output contract. Return only one validated Mezzanine MAAP action batch that matches the provided JSON schema, with no surrounding prose.";
/// Corrective instruction used after Claude Code returns an empty response.
const CLAUDE_CODE_EMPTY_OUTPUT_RETRY_INSTRUCTION: &str = "Your previous response was empty. Return only one validated Mezzanine MAAP action batch that matches the provided JSON schema, with no surrounding prose.";
/// Claude Code tool name required for schema-backed structured output.
const CLAUDE_CODE_STRUCTURED_OUTPUT_TOOL: &str = "StructuredOutput";
/// Maximum stderr bytes retained in provider diagnostics.
const CLAUDE_CODE_STDERR_LIMIT: usize = 8192;
/// Number of short retries after Claude reports a session lock is still active.
const CLAUDE_CODE_SESSION_LOCK_RETRY_ATTEMPTS: usize = 4;
/// Delay between Claude Code session-lock retries.
const CLAUDE_CODE_SESSION_LOCK_RETRY_DELAY_MS: u64 = 50;

/// Process-local registry for serializing Claude Code print invocations by
/// stable Claude session id.
static CLAUDE_CODE_SESSION_STATES: OnceLock<Mutex<BTreeMap<String, Arc<ClaudeCodeSessionState>>>> =
    OnceLock::new();

/// Captures one Claude Code subprocess completion for retry and validation.
struct ClaudeCodeSubprocessOutput {
    assistant_text: String,
    structured_output: Option<String>,
    stderr: String,
    usage: ModelTokenUsage,
}

/// Stores the Claude Code request result after optional corrective retry.
struct ClaudeCodeRequestOutput {
    raw_text: String,
    structured_output: Option<String>,
    usage: ModelTokenUsage,
    latest_request_usage: Option<ModelTokenUsage>,
}

/// Carries the shared subprocess inputs for one Claude Code print invocation.
struct ClaudeCodeSubprocessRequest<'a> {
    program: &'a str,
    model: &'a str,
    session: Option<ClaudeCodeSessionRef<'a>>,
    system_prompt: &'a str,
    prompt: &'a str,
    resume_prompt: Option<&'a str>,
    reasoning_effort: Option<&'a str>,
    timeout_ms: u64,
    json_output: bool,
    json_schema: Option<&'a str>,
}

/// Carries the resume-specific prompt variants for one Claude Code invocation.
struct ClaudeCodeResumeSubprocessRequest<'a> {
    program: &'a str,
    model: &'a str,
    session: ClaudeCodeSessionRef<'a>,
    system_prompt: &'a str,
    resume_prompt: &'a str,
    create_prompt: &'a str,
    reasoning_effort: Option<&'a str>,
    timeout_ms: u64,
    json_output: bool,
    json_schema: Option<&'a str>,
}

/// Carries the final Claude Code CLI invocation arguments for one subprocess.
struct ClaudeCodeSessionInvocationRequest<'a> {
    program: &'a str,
    model: &'a str,
    session: Option<ClaudeCodeSessionInvocation<'a>>,
    system_prompt: &'a str,
    prompt: &'a str,
    reasoning_effort: Option<&'a str>,
    timeout_ms: u64,
    json_output: bool,
    json_schema: Option<&'a str>,
}

/// Tracks one Claude Code conversation id across short-lived print subprocesses.
struct ClaudeCodeSessionState {
    lock: tokio::sync::Mutex<()>,
}

impl ClaudeCodeSessionState {
    /// Creates unlocked Claude session state.
    fn new() -> Self {
        Self {
            lock: tokio::sync::Mutex::new(()),
        }
    }
}

/// Borrowed state for one locked Claude Code conversation.
#[derive(Clone, Copy)]
struct ClaudeCodeSessionRef<'a> {
    session_id: &'a str,
}

/// Selects the Claude Code CLI session flag for one subprocess.
#[derive(Clone, Copy)]
enum ClaudeCodeSessionInvocation<'a> {
    /// Create a subprocess conversation with a caller-provided id.
    Create { session_id: &'a str },
    /// Resume an existing subprocess conversation.
    Resume { session_id: &'a str },
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
            let (raw_text, structured_output, usage, latest_request_usage) =
                if request.interaction_kind == ModelInteractionKind::AutoSizing {
                    let prompt = claude_code_prompt(request, None);
                    let system_prompt = claude_code_system_prompt(request, None);
                    let output = run_claude_code_subprocess(ClaudeCodeSubprocessRequest {
                        program: &self.program,
                        model: &request.model,
                        session: None,
                        system_prompt: &system_prompt,
                        prompt: &prompt,
                        resume_prompt: None,
                        reasoning_effort: request.reasoning_effort.as_deref(),
                        timeout_ms: self.timeout_ms,
                        json_output: true,
                        json_schema: None,
                    })
                    .await?;
                    if output.assistant_text.is_empty() {
                        return Err(claude_code_empty_output_error(&output.stderr));
                    }
                    validate_claude_code_auto_sizing_output(&output.assistant_text)?;
                    (output.assistant_text, None, output.usage, None)
                } else {
                    let output = run_claude_code_request_with_corrective_retry(
                        &self.program,
                        request,
                        self.timeout_ms,
                    )
                    .await?;
                    (
                        output.raw_text,
                        output.structured_output,
                        output.usage,
                        output.latest_request_usage,
                    )
                };
            let action_batch = if request.interaction_kind == ModelInteractionKind::AutoSizing {
                None
            } else {
                let batch = parse_claude_code_maap_output(
                    request,
                    &raw_text,
                    structured_output.as_deref(),
                )?;
                Some(batch)
            };
            Ok(ModelResponse {
                provider: self.provider_id.clone(),
                model: request.model.clone(),
                raw_text,
                usage,
                latest_request_usage,
                quota_usage: Vec::new(),
                action_batch,
                provider_transcript_events: Vec::new(),
            })
        })
    }
}

/// Builds the Claude system prompt passed via the dedicated CLI flag.
fn claude_code_system_prompt(request: &ModelRequest, retry_instruction: Option<&str>) -> String {
    let mut prompt = String::new();

    append_claude_code_instruction_framing(&mut prompt, request, retry_instruction);
    prompt.push_str("Claude Code direct-tool boundary:\n");
    prompt.push_str(
        "Do not call Claude Code native tools for local files, commands, web, MCP, subagents, config, memory, issue operations, or task delegation. Use only the response channel Mezzanine requested for this turn. When a MAAP schema is present, the only Claude Code tool Mezzanine may allow is StructuredOutput, and it is only a carrier for returning the MAAP action batch.\n",
    );
    if request.interaction_kind != ModelInteractionKind::AutoSizing {
        prompt.push_str("Output contract:\n");
        prompt.push_str(
            "Respond with the validated Mezzanine MAAP action batch text only. Do not run tools or mutate files directly.\n",
        );
    }
    prompt
}

/// Builds the text prompt passed to the Claude Code CLI stdin channel.
fn claude_code_prompt(request: &ModelRequest, _retry_instruction: Option<&str>) -> String {
    let final_user_index = request
        .messages
        .iter()
        .rposition(|message| message.role == ModelMessageRole::User);
    let mut prompt = String::new();

    append_claude_code_prior_context(&mut prompt, request, final_user_index);
    append_claude_code_current_user_prompt(&mut prompt, request, final_user_index);
    prompt
}

/// Builds the current-turn-only stdin prompt used when resuming an existing
/// Claude Code conversation.
fn claude_code_current_turn_prompt(request: &ModelRequest) -> String {
    let final_user_index = request
        .messages
        .iter()
        .rposition(|message| message.role == ModelMessageRole::User);
    let mut prompt = String::new();

    append_claude_code_current_user_prompt(&mut prompt, request, final_user_index);
    prompt
}

/// Appends system, developer, and retry instructions to Claude's dedicated
/// system-prompt channel instead of flattening them into the stdin prompt.
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
        prompt.push_str("Follow the system prompt.");
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
fn claude_code_output_needs_corrective_retry(
    request: &ModelRequest,
    raw_text: &str,
    structured_output: Option<&str>,
) -> bool {
    if request.interaction_kind == ModelInteractionKind::AutoSizing {
        return raw_text.is_empty();
    }
    parse_claude_code_maap_output(request, raw_text, structured_output).is_err()
}

/// Builds the Claude Code JSON schema argument for MAAP action-batch turns.
fn claude_code_maap_json_schema(request: &ModelRequest) -> Result<String> {
    serde_json::to_string(&maap_action_batch_schema(
        &request.allowed_actions,
        &request.available_mcp_tools,
    ))
    .map_err(|error| {
        MezError::invalid_state(format!(
            "Claude Code MAAP JSON schema could not be serialized: {error}"
        ))
    })
}

/// Parses Claude Code MAAP output from schema-enforced Claude Code responses.
fn parse_claude_code_maap_output(
    request: &ModelRequest,
    raw_text: &str,
    structured_output: Option<&str>,
) -> Result<MaapBatch> {
    if let Some(structured_output) = structured_output {
        return parse_maap_action_batch_json_for_turn(
            structured_output,
            &request.turn_id,
            &request.agent_id,
        )
        .map_err(|error| provider_maap_parse_error(error, structured_output));
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
    Err(MezError::invalid_state(message).with_provider_raw_text(raw_text.to_string()))
}

/// Builds the provider error used when Claude Code exits successfully but
/// produces no assistant output.
fn claude_code_empty_output_error(stderr: &str) -> MezError {
    MezError::invalid_state("Claude Code subprocess produced no assistant output")
        .with_provider_raw_text(stderr.to_string())
}

/// Parses assistant text, structured output, and token usage from Claude Code
/// JSON print output.
fn parse_claude_code_json_output(
    stdout: &str,
) -> Result<(String, Option<String>, ModelTokenUsage)> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok((String::new(), None, ModelTokenUsage::default()));
    }
    let envelope = match serde_json::from_str::<ClaudeCodeJsonEnvelope>(trimmed) {
        Ok(envelope) => envelope,
        Err(_) => return Ok((trimmed.to_string(), None, ModelTokenUsage::default())),
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
        return Err(MezError::invalid_state(message).with_provider_raw_text(trimmed.to_string()));
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
        return Err(MezError::invalid_state(message).with_provider_raw_text(trimmed.to_string()));
    }
    let structured_output = envelope
        .structured_output
        .map(|value| serde_json::to_string(&value))
        .transpose()
        .map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code JSON structured output could not be serialized: {error}"
            ))
            .with_provider_raw_text(trimmed.to_string())
        })?;
    if result.is_empty() && structured_output.is_none() {
        return Err(MezError::invalid_state(
            "Claude Code JSON output did not contain result text or structured output",
        )
        .with_provider_raw_text(trimmed.to_string()));
    };
    let input_tokens = envelope.usage.input_tokens.unwrap_or(0);
    let cached_input_tokens = envelope.usage.cache_read_input_tokens;
    Ok((
        result.trim().to_string(),
        structured_output,
        ModelTokenUsage {
            input_tokens: input_tokens.saturating_add(cached_input_tokens.unwrap_or(0)),
            output_tokens: envelope.usage.output_tokens.unwrap_or(0),
            reasoning_tokens: 0,
            cached_input_tokens,
            cache_write_input_tokens: envelope.usage.cache_creation_input_tokens,
        },
    ))
}

/// Reports whether a Claude Code detail string indicates that the CLI needs
/// interactive login or alternate headless authentication.
fn claude_code_login_required_detail(detail: &str) -> bool {
    let lower = detail.trim().to_ascii_lowercase();
    lower.contains("not logged in") || lower.contains("please run /login")
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

/// Returns the Claude Code session id used to resume one Mez conversation.
fn claude_code_session_id(request: &ModelRequest) -> Option<String> {
    if let Some(session_id) = request
        .prompt_cache_session_id
        .as_deref()
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
    {
        if claude_code_uuid_is_valid(session_id) {
            return Some(session_id.to_ascii_lowercase());
        }
        return Some(claude_code_uuid_from_stable_key(&format!(
            "session:{session_id}"
        )));
    }
    if let Some(lineage_id) = request
        .prompt_cache_lineage_id
        .as_deref()
        .map(str::trim)
        .filter(|lineage_id| !lineage_id.is_empty())
    {
        return Some(claude_code_uuid_from_stable_key(&format!(
            "lineage:{lineage_id}"
        )));
    }
    None
}

/// Returns shared process-local state for one Claude Code session id.
fn claude_code_session_state(session_id: &str) -> Arc<ClaudeCodeSessionState> {
    let registry = CLAUDE_CODE_SESSION_STATES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut sessions = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    sessions
        .entry(session_id.to_string())
        .or_insert_with(|| Arc::new(ClaudeCodeSessionState::new()))
        .clone()
}

/// Reports whether a string already has Claude's UUID-shaped session id form.
fn claude_code_uuid_is_valid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23].iter().all(|index| bytes[*index] == b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
}

/// Derives a deterministic UUID-shaped Claude session id from stable Mez data.
fn claude_code_uuid_from_stable_key(key: &str) -> String {
    let digest = sha2::Sha256::digest(format!("mezzanine-claude-code-session-v1\n{key}"));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// Runs one bounded Claude Code request and applies one corrective retry for
/// empty or malformed MAAP output.
async fn run_claude_code_request_with_corrective_retry(
    program: &str,
    request: &ModelRequest,
    timeout_ms: u64,
) -> Result<ClaudeCodeRequestOutput> {
    let session_id = claude_code_session_id(request);
    let session_state = session_id.as_deref().map(claude_code_session_state);
    let _session_guard = match session_state.as_ref() {
        Some(state) => Some(state.lock.lock().await),
        None => None,
    };
    let session = session_id
        .as_deref()
        .map(|session_id| ClaudeCodeSessionRef { session_id });
    let maap_json_schema = claude_code_maap_json_schema(request)?;
    let first_prompt = claude_code_prompt(request, None);
    let first_resume_prompt = claude_code_current_turn_prompt(request);
    let first_system_prompt = claude_code_system_prompt(request, None);
    let first_output = run_claude_code_subprocess(ClaudeCodeSubprocessRequest {
        program,
        model: &request.model,
        session,
        system_prompt: &first_system_prompt,
        prompt: &first_prompt,
        resume_prompt: Some(&first_resume_prompt),
        reasoning_effort: request.reasoning_effort.as_deref(),
        timeout_ms,
        json_output: true,
        json_schema: Some(&maap_json_schema),
    })
    .await?;
    if !claude_code_output_needs_corrective_retry(
        request,
        &first_output.assistant_text,
        first_output.structured_output.as_deref(),
    ) {
        return Ok(ClaudeCodeRequestOutput {
            raw_text: first_output.assistant_text,
            structured_output: first_output.structured_output,
            usage: first_output.usage,
            latest_request_usage: None,
        });
    }
    let retry_instruction = if first_output.assistant_text.is_empty() {
        CLAUDE_CODE_EMPTY_OUTPUT_RETRY_INSTRUCTION
    } else {
        CLAUDE_CODE_MAAP_RETRY_INSTRUCTION
    };
    let retry_prompt = claude_code_prompt(request, Some(retry_instruction));
    let retry_resume_prompt = claude_code_current_turn_prompt(request);
    let retry_system_prompt = claude_code_system_prompt(request, Some(retry_instruction));
    let retry_output = run_claude_code_subprocess(ClaudeCodeSubprocessRequest {
        program,
        model: &request.model,
        session,
        system_prompt: &retry_system_prompt,
        prompt: &retry_prompt,
        resume_prompt: Some(&retry_resume_prompt),
        reasoning_effort: request.reasoning_effort.as_deref(),
        timeout_ms,
        json_output: true,
        json_schema: Some(&maap_json_schema),
    })
    .await?;
    if retry_output.assistant_text.is_empty() && retry_output.structured_output.is_none() {
        return Err(claude_code_empty_output_error(&retry_output.stderr));
    }
    let latest_request_usage = retry_output.usage;
    let mut usage = first_output.usage;
    usage.add_assign(latest_request_usage);
    Ok(ClaudeCodeRequestOutput {
        raw_text: retry_output.assistant_text,
        structured_output: retry_output.structured_output,
        usage,
        latest_request_usage: Some(latest_request_usage),
    })
}

/// Invokes Claude Code in print mode with direct tool use disabled.
async fn run_claude_code_subprocess(
    request: ClaudeCodeSubprocessRequest<'_>,
) -> Result<ClaudeCodeSubprocessOutput> {
    let Some(session) = request.session else {
        return run_claude_code_subprocess_with_session_invocation(
            ClaudeCodeSessionInvocationRequest {
                program: request.program,
                model: request.model,
                session: None,
                system_prompt: request.system_prompt,
                prompt: request.prompt,
                reasoning_effort: request.reasoning_effort,
                timeout_ms: request.timeout_ms,
                json_output: request.json_output,
                json_schema: request.json_schema,
            },
        )
        .await;
    };

    let resume_prompt = request.resume_prompt.unwrap_or(request.prompt);
    run_claude_code_resume_subprocess(ClaudeCodeResumeSubprocessRequest {
        program: request.program,
        model: request.model,
        session,
        system_prompt: request.system_prompt,
        resume_prompt,
        create_prompt: request.prompt,
        reasoning_effort: request.reasoning_effort,
        timeout_ms: request.timeout_ms,
        json_output: request.json_output,
        json_schema: request.json_schema,
    })
    .await
}

/// Invokes an initialized Claude Code conversation through `--resume`.
async fn run_claude_code_resume_subprocess(
    request: ClaudeCodeResumeSubprocessRequest<'_>,
) -> Result<ClaudeCodeSubprocessOutput> {
    for attempt in 0..=CLAUDE_CODE_SESSION_LOCK_RETRY_ATTEMPTS {
        let result = run_claude_code_subprocess_with_session_invocation(
            ClaudeCodeSessionInvocationRequest {
                program: request.program,
                model: request.model,
                session: Some(ClaudeCodeSessionInvocation::Resume {
                    session_id: request.session.session_id,
                }),
                system_prompt: request.system_prompt,
                prompt: request.resume_prompt,
                reasoning_effort: request.reasoning_effort,
                timeout_ms: request.timeout_ms,
                json_output: request.json_output,
                json_schema: request.json_schema,
            },
        )
        .await;
        match result {
            Ok(output) => return Ok(output),
            Err(error)
                if claude_code_error_indicates_active_session(&error)
                    && attempt < CLAUDE_CODE_SESSION_LOCK_RETRY_ATTEMPTS =>
            {
                tokio::time::sleep(Duration::from_millis(
                    CLAUDE_CODE_SESSION_LOCK_RETRY_DELAY_MS,
                ))
                .await;
            }
            Err(error) if claude_code_error_indicates_missing_session(&error) => {
                let create_result = run_claude_code_subprocess_with_session_invocation(
                    ClaudeCodeSessionInvocationRequest {
                        program: request.program,
                        model: request.model,
                        session: Some(ClaudeCodeSessionInvocation::Create {
                            session_id: request.session.session_id,
                        }),
                        system_prompt: request.system_prompt,
                        prompt: request.create_prompt,
                        reasoning_effort: request.reasoning_effort,
                        timeout_ms: request.timeout_ms,
                        json_output: request.json_output,
                        json_schema: request.json_schema,
                    },
                )
                .await;
                match create_result {
                    Ok(output) => return Ok(output),
                    Err(error)
                        if claude_code_error_indicates_existing_session(&error)
                            && attempt < CLAUDE_CODE_SESSION_LOCK_RETRY_ATTEMPTS =>
                    {
                        tokio::time::sleep(Duration::from_millis(
                            CLAUDE_CODE_SESSION_LOCK_RETRY_DELAY_MS,
                        ))
                        .await;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded Claude Code resume retry loop always returns")
}

/// Invokes Claude Code in print mode with an explicit session flag selection.
async fn run_claude_code_subprocess_with_session_invocation(
    request: ClaudeCodeSessionInvocationRequest<'_>,
) -> Result<ClaudeCodeSubprocessOutput> {
    let mut spawn_attempt = 0;
    let mut child = loop {
        let mut command = Command::new(request.program);
        command.arg("--print").arg("--model").arg(request.model);
        match request.session {
            Some(ClaudeCodeSessionInvocation::Create { session_id }) => {
                command.arg("--session-id").arg(session_id);
            }
            Some(ClaudeCodeSessionInvocation::Resume { session_id }) => {
                command.arg("--resume").arg(session_id);
            }
            None => {}
        }
        if !request.system_prompt.is_empty() {
            command.arg("--system-prompt").arg(request.system_prompt);
        }
        if let Some(effort) = request.reasoning_effort.filter(|effort| !effort.is_empty()) {
            command.arg("--effort").arg(effort);
        }
        if request.json_output {
            command.arg("--output-format").arg("json");
        }
        if let Some(schema) = request.json_schema.filter(|schema| !schema.is_empty()) {
            command.arg("--json-schema").arg(schema);
        }
        match command
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
        stdin
            .write_all(request.prompt.as_bytes())
            .await
            .map_err(|error| {
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

    let output = tokio::time::timeout(
        Duration::from_millis(request.timeout_ms),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| {
        MezError::invalid_state(format!(
            "Claude Code subprocess timed out after {}ms; you can retry the request",
            request.timeout_ms
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
    let (assistant_text, structured_output, usage) = if request.json_output {
        parse_claude_code_json_output(&stdout)?
    } else {
        (stdout, None, ModelTokenUsage::default())
    };
    Ok(ClaudeCodeSubprocessOutput {
        assistant_text,
        structured_output,
        stderr,
        usage,
    })
}

/// Reports whether a subprocess spawn failure is likely transient and worth
/// one immediate retry before surfacing a provider setup failure.
fn claude_code_spawn_error_is_transient(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(26)
}

/// Reports whether Claude Code says the requested session already exists or is
/// still locked by a recent invocation.
fn claude_code_error_indicates_existing_session(error: &MezError) -> bool {
    let text = claude_code_error_search_text(error);
    claude_code_error_indicates_active_session_text(&text)
        || text.contains("already exists")
        || (text.contains("session id") && text.contains("already"))
}

/// Reports whether Claude Code says the requested session is currently locked.
fn claude_code_error_indicates_active_session(error: &MezError) -> bool {
    claude_code_error_indicates_active_session_text(&claude_code_error_search_text(error))
}

/// Reports whether Claude Code says a resume target is absent.
fn claude_code_error_indicates_missing_session(error: &MezError) -> bool {
    let text = claude_code_error_search_text(error);
    (text.contains("session") || text.contains("conversation"))
        && (text.contains("not found")
            || text.contains("does not exist")
            || text.contains("could not find")
            || text.contains("no conversation")
            || text.contains("unknown"))
}

/// Reports whether normalized diagnostic text describes an active session lock.
fn claude_code_error_indicates_active_session_text(text: &str) -> bool {
    (text.contains("session") || text.contains("conversation"))
        && (text.contains("already in use")
            || text.contains("currently in use")
            || text.contains("is in use")
            || text.contains("locked"))
}

/// Builds lowercase diagnostic text for session error classifiers.
fn claude_code_error_search_text(error: &MezError) -> String {
    let mut text = error.message().to_string();
    if let Some(raw) = error.provider_raw_text() {
        text.push('\n');
        text.push_str(raw);
    }
    text.to_ascii_lowercase()
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

    /// Verifies Claude Code session ids are stable per Mezzanine session and
    /// still satisfy Claude's UUID argument contract when Mezzanine only has a
    /// non-UUID fallback key.
    #[test]
    fn claude_code_session_id_uses_stable_mez_session_key() {
        let mut request = claude_request();
        assert_eq!(claude_code_session_id(&request), None);

        request.prompt_cache_session_id = Some("018f6b3a-1b2c-7000-9000-cafebabefeed".to_string());

        assert_eq!(
            claude_code_session_id(&request),
            Some("018f6b3a-1b2c-7000-9000-cafebabefeed".to_string())
        );

        request.prompt_cache_session_id = Some("mez-session-A".to_string());
        let derived_a = claude_code_session_id(&request).unwrap();
        let derived_a_again = claude_code_session_id(&request).unwrap();
        request.prompt_cache_session_id = Some("mez-session-B".to_string());
        let derived_b = claude_code_session_id(&request).unwrap();

        assert_eq!(derived_a, derived_a_again);
        assert_ne!(derived_a, derived_b);
        assert!(claude_code_uuid_is_valid(&derived_a));
        assert!(claude_code_uuid_is_valid(&derived_b));
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

        let system_prompt =
            claude_code_system_prompt(&request, Some("Retry with a valid MAAP batch."));
        let prompt = claude_code_prompt(&request, Some("Retry with a valid MAAP batch."));

        assert!(
            system_prompt.contains("Instruction framing for Claude Code:"),
            "{system_prompt}"
        );
        assert!(
            system_prompt.contains("System instruction:\nSystem authority."),
            "{system_prompt}"
        );
        assert!(
            system_prompt.contains("Developer instruction:\nDeveloper authority."),
            "{system_prompt}"
        );
        assert!(
            system_prompt.contains("Developer retry instruction:\nRetry with a valid MAAP batch."),
            "{system_prompt}"
        );
        assert!(
            system_prompt.contains("Claude Code direct-tool boundary:"),
            "{system_prompt}"
        );
        assert!(
            system_prompt
                .contains("the only Claude Code tool Mezzanine may allow is StructuredOutput"),
            "{system_prompt}"
        );
        assert!(
            system_prompt.contains("Output contract:"),
            "{system_prompt}"
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
        assert!(!prompt.contains("System instruction:"), "{prompt}");
        assert!(!prompt.contains("Developer instruction:"), "{prompt}");
        assert!(!prompt.contains("Developer retry instruction:"), "{prompt}");
        assert!(!prompt.contains("<system>"), "{prompt}");
        assert!(!prompt.contains("<developer>"), "{prompt}");
        assert!(!prompt.contains("<assistant>"), "{prompt}");
        assert!(!prompt.contains("<tool>"), "{prompt}");
    }

    /// Verifies instruction-only Claude Code requests still produce a current
    /// request section instead of recreating role-tagged transcript blocks.
    #[test]
    fn claude_code_prompt_handles_instruction_only_requests() {
        let system_prompt = claude_code_system_prompt(&claude_request(), None);
        let prompt = claude_code_prompt(&claude_request(), None);

        assert!(
            system_prompt.contains("Developer instruction:\nReturn a final say action."),
            "{system_prompt}"
        );
        assert!(
            prompt.contains("Current user request:\nFollow the system prompt."),
            "{prompt}"
        );
        assert!(!prompt.contains("Developer instruction:"), "{prompt}");
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
{"type":"result","subtype":"success","is_error":false,"result":"hello","structured_output":{"rationale":"return final text","thought":null,"actions":[{"type":"say","status":"final","text":"hello","content_type":"text/plain; charset=utf-8"}]},"usage":{"input_tokens":2,"output_tokens":12}}
EOF
"#,
        );
        let provider = fixture.provider(1_000);
        let mut request = claude_request();
        request.reasoning_effort = Some("high".to_string());

        let response = provider.send_request_async(&request).await.unwrap();

        assert_eq!(response.provider, "claude-code");
        assert_eq!(response.model, "claude-sonnet-test");
        assert!(response.action_batch.is_some());
        let args = fs::read_to_string(fixture.program.with_extension("args")).unwrap();
        assert!(args.contains("--print"), "{args}");
        assert!(args.contains("--model"), "{args}");
        assert!(args.contains("claude-sonnet-test"), "{args}");
        assert!(!args.contains("--bare"), "{args}");
        assert!(!args.contains("--permission-mode"), "{args}");
        assert!(!args.contains("dontAsk"), "{args}");
        assert!(!args.contains("--disallowedTools"), "{args}");
        assert!(!args.contains("--session-id"), "{args}");
        assert!(!args.contains("--resume"), "{args}");
        assert!(args.contains("--system-prompt"), "{args}");
        assert!(
            args.contains("Developer instruction:\nReturn a final say action."),
            "{args}"
        );
        assert!(args.contains("--effort"), "{args}");
        assert!(args.contains("high"), "{args}");
        assert!(args.contains("--output-format"), "{args}");
        assert!(args.contains("json"), "{args}");
        assert!(!args.contains("--allowedTools"), "{args}");
        let stdin = fs::read_to_string(fixture.program.with_extension("stdin")).unwrap();
        assert!(
            stdin.contains("Current user request:\nFollow the system prompt."),
            "{stdin}"
        );
        assert!(!stdin.contains("Developer instruction:"), "{stdin}");
    }

    /// Verifies repeated Claude Code turns with the same Mez session id create
    /// the Claude conversation once and then resume it.
    ///
    /// Claude Code distinguishes `--session-id` from `--resume`; repeatedly
    /// passing `--session-id` can collide with Claude's active-session lock
    /// instead of behaving like a conversation resume.
    #[tokio::test]
    async fn claude_code_provider_resumes_stable_session_after_creation() {
        let fixture = ClaudeCodeFixture::new("session-resume");
        fixture.write_claude_script(
            r#"#!/bin/sh
count_file="$0.count"
count=0
if [ -f "$count_file" ]; then
    count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
printf '%s\n' "$@" > "$0.args.$count"
cat > "$0.stdin.$count"
case " $* " in
    *" --resume "*)
        if [ "$count" -eq 1 ]; then
            printf '%s\n' 'Error: No conversation found for session.' >&2
            exit 1
        fi
        ;;
esac
cat <<'EOF'
{"type":"result","subtype":"success","is_error":false,"result":"hello","structured_output":{"rationale":"return final text","thought":null,"actions":[{"type":"say","status":"final","text":"hello","content_type":"text/plain; charset=utf-8"}]},"usage":{"input_tokens":2,"output_tokens":12}}
EOF
"#,
        );
        let provider = fixture.provider(1_000);
        let mut request = claude_request();
        request.prompt_cache_session_id = Some(format!("mez-session-{}", current_test_nonce()));

        provider.send_request_async(&request).await.unwrap();
        provider.send_request_async(&request).await.unwrap();

        let first_resume_args =
            fs::read_to_string(fixture.program.with_extension("args.1")).unwrap();
        let create_args = fs::read_to_string(fixture.program.with_extension("args.2")).unwrap();
        let second_turn_args =
            fs::read_to_string(fixture.program.with_extension("args.3")).unwrap();
        assert!(
            first_resume_args.contains("--resume"),
            "{first_resume_args}"
        );
        assert!(
            !first_resume_args.contains("--session-id"),
            "{first_resume_args}"
        );
        assert!(create_args.contains("--session-id"), "{create_args}");
        assert!(!create_args.contains("--resume"), "{create_args}");
        assert!(second_turn_args.contains("--resume"), "{second_turn_args}");
        assert!(
            !second_turn_args.contains("--session-id"),
            "{second_turn_args}"
        );
        let first_resume_stdin =
            fs::read_to_string(fixture.program.with_extension("stdin.1")).unwrap();
        let create_stdin = fs::read_to_string(fixture.program.with_extension("stdin.2")).unwrap();
        let second_turn_stdin =
            fs::read_to_string(fixture.program.with_extension("stdin.3")).unwrap();
        assert!(
            first_resume_stdin.contains("Current user request:\nFollow the system prompt."),
            "{first_resume_stdin}"
        );
        assert!(
            create_stdin.contains("Current user request:\nFollow the system prompt."),
            "{create_stdin}"
        );
        assert!(
            second_turn_stdin.contains("Current user request:\nFollow the system prompt."),
            "{second_turn_stdin}"
        );
        assert!(
            !second_turn_stdin.contains("Prior conversation context"),
            "{second_turn_stdin}"
        );
    }

    /// Verifies corrective retries resume the just-created Claude session.
    ///
    /// The first subprocess may return malformed MAAP while still creating the
    /// Claude conversation. The retry must use `--resume` so it can benefit from
    /// that prompt context without colliding on `--session-id`.
    #[tokio::test]
    async fn claude_code_provider_corrective_retry_resumes_created_session() {
        let fixture = ClaudeCodeFixture::new("session-retry-resume");
        fixture.write_claude_script(
            r#"#!/bin/sh
count_file="$0.count"
count=0
if [ -f "$count_file" ]; then
    count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
printf '%s\n' "$@" > "$0.args.$count"
cat >/dev/null
if [ "$count" -eq 1 ]; then
    printf '%s\n' 'Error: No conversation found for session.' >&2
    exit 1
fi
if [ "$count" -eq 2 ]; then
    printf '%s\n' 'plain assistant text without a MAAP block'
    exit 0
fi
cat <<'EOF'
{"type":"result","subtype":"success","is_error":false,"result":"hello","structured_output":{"rationale":"return final text","thought":null,"actions":[{"type":"say","status":"final","text":"hello","content_type":"text/plain; charset=utf-8"}]},"usage":{"input_tokens":2,"output_tokens":12}}
EOF
"#,
        );
        let provider = fixture.provider(1_000);
        let mut request = claude_request();
        request.prompt_cache_session_id = Some(format!("mez-retry-{}", current_test_nonce()));

        let response = provider.send_request_async(&request).await.unwrap();

        assert!(response.action_batch.is_some());
        let initial_resume_args =
            fs::read_to_string(fixture.program.with_extension("args.1")).unwrap();
        let create_args = fs::read_to_string(fixture.program.with_extension("args.2")).unwrap();
        let retry_args = fs::read_to_string(fixture.program.with_extension("args.3")).unwrap();
        assert!(
            initial_resume_args.contains("--resume"),
            "{initial_resume_args}"
        );
        assert!(
            !initial_resume_args.contains("--session-id"),
            "{initial_resume_args}"
        );
        assert!(create_args.contains("--session-id"), "{create_args}");
        assert!(!create_args.contains("--resume"), "{create_args}");
        assert!(retry_args.contains("--resume"), "{retry_args}");
        assert!(!retry_args.contains("--session-id"), "{retry_args}");
    }

    /// Verifies an active-session failure from `--resume` gets a short retry.
    ///
    /// This covers the provider error where Claude reports `Session ID ... is
    /// already in use` before producing a MAAP action batch.
    #[tokio::test]
    async fn claude_code_provider_resumes_after_active_session_id_failure() {
        let fixture = ClaudeCodeFixture::new("session-active-fallback");
        fixture.write_claude_script(
            r#"#!/bin/sh
count_file="$0.count"
count=0
if [ -f "$count_file" ]; then
    count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
printf '%s\n' "$@" > "$0.args.$count"
cat >/dev/null
case " $* " in
    *" --resume "*)
        if [ "$count" -eq 1 ]; then
            printf '%s\n' 'Error: Session ID 10221f2b-78e3-557a-b2aa-bd3c9049c983 is already in use.' >&2
            exit 1
        fi
        ;;
esac
cat <<'EOF'
{"type":"result","subtype":"success","is_error":false,"result":"hello","structured_output":{"rationale":"return final text","thought":null,"actions":[{"type":"say","status":"final","text":"hello","content_type":"text/plain; charset=utf-8"}]},"usage":{"input_tokens":2,"output_tokens":12}}
EOF
"#,
        );
        let provider = fixture.provider(1_000);
        let mut request = claude_request();
        request.prompt_cache_session_id = Some(format!("mez-active-{}", current_test_nonce()));

        let response = provider.send_request_async(&request).await.unwrap();

        assert!(response.action_batch.is_some());
        let first_args = fs::read_to_string(fixture.program.with_extension("args.1")).unwrap();
        let retry_args = fs::read_to_string(fixture.program.with_extension("args.2")).unwrap();
        assert!(first_args.contains("--resume"), "{first_args}");
        assert!(!first_args.contains("--session-id"), "{first_args}");
        assert!(retry_args.contains("--resume"), "{retry_args}");
        assert!(!retry_args.contains("--session-id"), "{retry_args}");
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
{"type":"result","subtype":"success","is_error":false,"result":"hello","structured_output":{"rationale":"return final text","thought":null,"actions":[{"type":"say","status":"final","text":"hello","content_type":"text/plain; charset=utf-8"}]},"usage":{"input_tokens":2,"output_tokens":12}}
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
        assert!(stdin.ends_with("Follow the system prompt.\n\n"), "{stdin}");
    }

    /// Verifies Claude Code JSON print envelopes populate provider token usage
    /// counters while preserving assistant text for the existing MAAP parser.
    #[tokio::test]
    async fn claude_code_provider_parses_json_usage_accounting() {
        let fixture = ClaudeCodeFixture::new("json-usage");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
cat <<'EOF'
{"type":"result","subtype":"success","is_error":false,"result":"hello","structured_output":{"rationale":"return final text","thought":null,"actions":[{"type":"say","status":"final","text":"hello","content_type":"text/plain; charset=utf-8"}]},"usage":{"input_tokens":2,"output_tokens":12,"cache_creation_input_tokens":6112,"cache_read_input_tokens":10496}}
EOF
"#,
        );
        let provider = fixture.provider(1_000);

        let response = provider
            .send_request_async(&claude_request())
            .await
            .unwrap();

        assert!(response.action_batch.is_some());
        assert_eq!(response.usage.input_tokens, 10_498);
        assert_eq!(response.usage.billed_input_tokens(), 6_114);
        assert_eq!(response.usage.output_tokens, 12);
        assert_eq!(response.usage.reasoning_tokens, 0);
        assert_eq!(response.usage.cached_input_tokens, Some(10_496));
        assert_eq!(response.usage.cached_input_hit_ratio_display(), "99.98%");
        assert_eq!(response.usage.cache_write_input_tokens, Some(6_112));
        assert_eq!(response.usage.total_tokens(), 16_622);
        assert_eq!(response.latest_request_usage, None);
    }

    /// Verifies Claude Code structured output is requested with the active MAAP
    /// schema and parsed as the authoritative response payload.
    ///
    /// Claude Code can return schema-constrained data in `structured_output`;
    /// this regression protects the subprocess adapter from ignoring that
    /// channel or treating plain assistant text as the authoritative MAAP batch
    /// when structured JSON is already available.
    #[tokio::test]
    async fn claude_code_provider_parses_structured_output_action_batch() {
        let fixture = ClaudeCodeFixture::new("structured-output");
        fixture.write_claude_script(
            r#"#!/bin/sh
printf '%s\n' "$@" > "$0.args"
cat >/dev/null
cat <<'EOF'
{"type":"result","subtype":"success","is_error":false,"result":"not fenced","structured_output":{"rationale":"return final text","thought":null,"actions":[{"type":"say","status":"final","text":"hello","content_type":"text/plain; charset=utf-8"}]},"usage":{"input_tokens":2,"output_tokens":12}}
EOF
"#,
        );
        let provider = fixture.provider(1_000);

        let response = provider
            .send_request_async(&claude_request())
            .await
            .unwrap();

        assert!(response.action_batch.is_some());
        let args = fs::read_to_string(fixture.program.with_extension("args")).unwrap();
        assert!(!args.contains("--bare"), "{args}");
        assert!(!args.contains("--permission-mode"), "{args}");
        assert!(!args.contains("dontAsk"), "{args}");
        assert!(args.contains("--output-format"), "{args}");
        assert!(args.contains("json"), "{args}");
        assert!(!args.contains("--allowedTools"), "{args}");
        assert!(args.contains("--json-schema"), "{args}");
        assert!(args.contains("\"actions\""), "{args}");
        assert_eq!(response.raw_text, "not fenced");
        assert_eq!(response.latest_request_usage, None);
    }

    /// Verifies schema-enforced MAAP turns reject Claude Code JSON envelopes
    /// that omit `structured_output` even when the CLI reports success.
    ///
    /// Claude Code MAAP turns launch with `--json-schema`, so the provider must
    /// fail closed when the validated payload is missing instead of treating
    /// plain `result` text as a successful assistant response.
    #[tokio::test]
    async fn claude_code_provider_requires_structured_output_for_maap_turns() {
        let fixture = ClaudeCodeFixture::new("missing-structured-output");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
cat <<'EOF'
{"type":"result","subtype":"success","is_error":false,"result":"hello","usage":{"input_tokens":2,"output_tokens":12}}
EOF
"#,
        );
        let provider = fixture.provider(1_000);

        let error = provider
            .send_request_async(&claude_request())
            .await
            .unwrap_err();

        assert!(
            error
                .message()
                .contains("did not include structured_output"),
            "{}",
            error.message()
        );
        assert_eq!(error.provider_raw_text(), Some("hello"));
    }

    /// Verifies schema-enforced MAAP turns surface actionable login guidance
    /// when Claude Code returns success text instead of structured output
    /// because the CLI is not authenticated for the invocation path.
    #[tokio::test]
    async fn claude_code_provider_reports_login_guidance_for_missing_structured_output() {
        let fixture = ClaudeCodeFixture::new("missing-structured-output-login");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
cat <<'EOF'
{"type":"result","subtype":"success","is_error":false,"result":"Not logged in · Please run /login","usage":{"input_tokens":2,"output_tokens":12}}
EOF
"#,
        );
        let provider = fixture.provider(1_000);

        let error = provider
            .send_request_async(&claude_request())
            .await
            .unwrap_err();

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
        assert_eq!(
            error.provider_raw_text(),
            Some("Not logged in · Please run /login")
        );
    }

    /// Verifies StructuredOutput permission denials surface as explicit
    /// provider errors instead of generic missing-`structured_output` failures.
    ///
    /// Live Claude CLI runs can report a success envelope while withholding the
    /// schema-backed payload behind `permission_denials`. This regression keeps
    /// that denial mode diagnosable at the provider boundary.
    #[tokio::test]
    async fn claude_code_provider_reports_structured_output_permission_denials() {
        let fixture = ClaudeCodeFixture::new("structured-output-denied");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
cat <<'EOF'
{"type":"result","subtype":"success","is_error":false,"result":"Permission to use StructuredOutput has been denied.","permission_denials":[{"tool_name":"StructuredOutput"}],"usage":{"input_tokens":2,"output_tokens":12}}
EOF
"#,
        );
        let provider = fixture.provider(1_000);

        let error = provider
            .send_request_async(&claude_request())
            .await
            .unwrap_err();

        assert!(
            error.message().contains(
                "denied StructuredOutput permission required for schema-enforced responses"
            ),
            "{}",
            error.message()
        );
        assert!(
            error
                .provider_raw_text()
                .unwrap_or_default()
                .contains("\"permission_denials\""),
            "{:?}",
            error.provider_raw_text()
        );
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

        let (_, _, omitted_usage) = parse_claude_code_json_output(omitted).unwrap();
        let (_, _, explicit_zero_usage) = parse_claude_code_json_output(explicit_zero).unwrap();

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
        let raw = r#"{"type":"result","subtype":"error_auth","is_error":true,"result":"Not logged in · Please run /login","usage":{"input_tokens":2,"output_tokens":12}}"#;

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
{"type":"result","subtype":"success","is_error":false,"result":"not fenced","structured_output":{"rationale":"return final text","thought":null,"actions":[{"type":"say","status":"final","text":"hello","content_type":"text/plain; charset=utf-8"}]},"usage":{"input_tokens":2,"output_tokens":12}}
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
{"type":"result","subtype":"success","is_error":false,"result":"hello","structured_output":{"rationale":"return final text","thought":null,"actions":[{"type":"say","status":"final","text":"hello","content_type":"text/plain; charset=utf-8"}]},"usage":{"input_tokens":2,"output_tokens":12}}
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
{"type":"result","subtype":"success","is_error":false,"result":"{\"version\":1,\"size\":\"medium\",\"reasoning_effort\":\"high\",\"confidence\":0.82,\"rationale\":\"coding task needs a medium model\"}","usage":{"input_tokens":7,"output_tokens":11,"cache_creation_input_tokens":13,"cache_read_input_tokens":17}}
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
        assert_eq!(response.usage.input_tokens, 24);
        assert_eq!(response.usage.output_tokens, 11);
        assert_eq!(response.usage.cached_input_tokens, Some(17));
        assert_eq!(response.usage.cache_write_input_tokens, Some(13));
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
{"type":"result","subtype":"success","is_error":false,"result":"{\"version\":1,\"size\":\"giant\",\"reasoning_effort\":\"high\",\"confidence\":1.5,\"rationale\":\"\"}","usage":{"input_tokens":3,"output_tokens":5}}
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
