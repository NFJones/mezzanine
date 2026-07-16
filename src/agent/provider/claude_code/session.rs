//! Process-local Claude Code session serialization and corrective retry.

use super::{
    CLAUDE_CODE_SESSION_STATES, ClaudeCodeRequestOutput, ClaudeCodeSubprocessRequest, ModelRequest,
    Result, claude_code_corrective_retry_instruction, claude_code_empty_output_error,
    claude_code_maap_json_schema, claude_code_prompt, claude_code_resume_prompt,
    claude_code_session_id, claude_code_system_prompt, parse_claude_code_maap_output,
    run_claude_code_subprocess,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Tracks one Claude Code conversation id across short-lived print subprocesses.
pub(super) struct ClaudeCodeSessionState {
    pub(super) lock: tokio::sync::Mutex<()>,
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
pub(super) struct ClaudeCodeSessionRef<'a> {
    pub(super) session_id: &'a str,
}

/// Selects the Claude Code CLI session flag for one subprocess.
#[derive(Clone, Copy)]
pub(super) enum ClaudeCodeSessionInvocation<'a> {
    /// Create a subprocess conversation with a caller-provided id.
    Create { session_id: &'a str },
    /// Resume an existing subprocess conversation.
    Resume { session_id: &'a str },
}

/// Returns shared process-local state for one Claude Code session id.
pub(super) fn claude_code_session_state(session_id: &str) -> Arc<ClaudeCodeSessionState> {
    let registry = CLAUDE_CODE_SESSION_STATES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut sessions = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    sessions
        .entry(session_id.to_string())
        .or_insert_with(|| Arc::new(ClaudeCodeSessionState::new()))
        .clone()
}

/// Runs one bounded Claude Code request and applies one corrective retry for
/// empty or malformed MAAP output.
pub(super) async fn run_claude_code_request_with_corrective_retry(
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
    let first_resume_prompt = claude_code_resume_prompt(request, None);
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
    if let Ok(action_batch) = parse_claude_code_maap_output(
        request,
        &first_output.assistant_text,
        first_output.structured_output.as_deref(),
    ) {
        return Ok(ClaudeCodeRequestOutput {
            raw_text: first_output.assistant_text,
            action_batch,
            usage: first_output.usage,
            latest_request_usage: None,
        });
    }
    let retry_instruction = claude_code_corrective_retry_instruction(&first_output.assistant_text);
    let retry_prompt = claude_code_prompt(request, Some(retry_instruction));
    let retry_resume_prompt = claude_code_resume_prompt(request, Some(retry_instruction));
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
        return Err(claude_code_empty_output_error(&retry_output.stderr).into());
    }
    let action_batch = parse_claude_code_maap_output(
        request,
        &retry_output.assistant_text,
        retry_output.structured_output.as_deref(),
    )?;
    let latest_request_usage = retry_output.usage;
    let mut usage = first_output.usage;
    usage.add_assign(latest_request_usage);
    Ok(ClaudeCodeRequestOutput {
        raw_text: retry_output.assistant_text,
        action_batch,
        usage,
        latest_request_usage: Some(latest_request_usage),
    })
}
