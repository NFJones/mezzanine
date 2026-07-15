//! Claude Code subprocess provider adapter.
//!
//! This module owns the experimental provider boundary for Claude Code
//! subscription-backed execution. The adapter invokes the local `claude` CLI in
//! noninteractive print mode, owns temporary settings and system-prompt files,
//! serializes session access, and projects lower Claude policy results into
//! product responses without granting direct tool or filesystem authority.

use super::{
    AsyncModelProvider, MaapBatch, MezError, ModelInteractionKind, ModelRequest, ModelResponse,
    ModelTokenUsage, ProviderModelCatalog, Result, validate_non_empty,
};
use mez_agent::{
    CLAUDE_CODE_STRUCTURED_OUTPUT_TOOL, ClaudeCodeOutput, ClaudeCodeSessionErrorKind,
    bound_claude_code_text, claude_code_auto_sizing_json_schema,
    claude_code_corrective_retry_instruction, claude_code_empty_output_error,
    claude_code_maap_json_schema, claude_code_macro_judge_json_schema, claude_code_prompt,
    claude_code_resume_prompt, claude_code_session_error_kind, claude_code_session_id,
    claude_code_system_prompt, parse_claude_code_json_output, parse_claude_code_maap_output,
    redact_claude_code_text, validate_claude_code_auto_sizing_output,
};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Executable name used for Claude Code subprocess requests.
const CLAUDE_CODE_PROGRAM: &str = "claude";
/// Claude Code native tools that must stay unavailable under Mezzanine-managed
/// execution.
const CLAUDE_CODE_DISALLOWED_NATIVE_TOOLS: &str = concat!(
    "SendUserMessage,Bash,Edit,Read,Agent,Artifact,AskUserQuestion,CronCreate,CronDelete,",
    "CronList,EnterPlanMode,EnterWorktree,ExitPlanMode,ExitWorktree,Glob,Grep,",
    "LSP,Monitor,NotebookEdit,PushNotification,",
    "ReadMcpResourceTool,RemoteTrigger,ScheduleWakeup,SendMessage,",
    "SendUserFile,ShareOnboardingGuide,Skill,TaskCreate,TaskGet,TaskList,TaskOutput,",
    "TaskStop,TaskUpdate,TodoWrite,ToolSearch,WaitForMcpServers,Workflow,Write,",
    "WebFetch,WebSearch",
);
/// Number of short retries after Claude reports a session lock is still active.
const CLAUDE_CODE_SESSION_LOCK_RETRY_ATTEMPTS: usize = 4;
/// Delay between Claude Code session-lock retries.
const CLAUDE_CODE_SESSION_LOCK_RETRY_DELAY_MS: u64 = 50;

/// Process-local registry for serializing Claude Code print invocations by
/// stable Claude session id.
static CLAUDE_CODE_SESSION_STATES: OnceLock<Mutex<BTreeMap<String, Arc<ClaudeCodeSessionState>>>> =
    OnceLock::new();
/// Monotonic suffix used to create per-invocation Claude settings files.
static CLAUDE_CODE_SETTINGS_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Owns a per-invocation Claude Code settings file and removes it when the
/// subprocess invocation completes or fails before spawn.
struct ClaudeCodeSettingsFile {
    path: PathBuf,
}

impl ClaudeCodeSettingsFile {
    /// Writes the Claude settings backed fields for one subprocess invocation.
    fn write(model: &str, structured_output_allowed: bool) -> Result<Self> {
        let suffix = CLAUDE_CODE_SETTINGS_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mez-claude-code-settings-{}-{suffix}.json",
            std::process::id()
        ));
        let mut allowed_tools = Vec::new();
        if structured_output_allowed {
            allowed_tools.push(CLAUDE_CODE_STRUCTURED_OUTPUT_TOOL);
        }
        let settings = serde_json::json!({
            "model": model,
            "permissions": {
                "allow": allowed_tools,
                "deny": CLAUDE_CODE_DISALLOWED_NATIVE_TOOLS
                    .split(',')
                    .collect::<Vec<_>>(),
            },
        });
        let bytes = serde_json::to_vec_pretty(&settings).map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code settings serialization failed: {}",
                redact_claude_code_text(&error.to_string())
            ))
        })?;
        std::fs::write(&path, bytes).map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code settings file write failed: {}",
                redact_claude_code_text(&error.to_string())
            ))
        })?;
        Ok(Self { path })
    }

    /// Returns the filesystem path passed to Claude Code with `--settings`.
    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for ClaudeCodeSettingsFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Owns a per-invocation Claude Code system prompt file and removes it when
/// the subprocess invocation completes or fails before spawn.
struct ClaudeCodeSystemPromptFile {
    path: PathBuf,
}

impl ClaudeCodeSystemPromptFile {
    /// Writes the generated system prompt for one subprocess invocation.
    fn write(system_prompt: &str) -> Result<Option<Self>> {
        if system_prompt.is_empty() {
            return Ok(None);
        }
        let suffix = CLAUDE_CODE_SETTINGS_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mez-claude-code-system-prompt-{}-{suffix}.md",
            std::process::id()
        ));
        std::fs::write(&path, system_prompt).map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code system prompt file write failed: {}",
                redact_claude_code_text(&error.to_string())
            ))
        })?;
        Ok(Some(Self { path }))
    }

    /// Returns the filesystem path passed to Claude Code with
    /// `--append-system-prompt-file`.
    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for ClaudeCodeSystemPromptFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

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
    action_batch: MaapBatch,
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
            let (raw_text, usage, latest_request_usage, action_batch) =
                if request.interaction_kind.expects_structured_json() {
                    let prompt = claude_code_prompt(request, None);
                    let system_prompt = claude_code_system_prompt(request, None);
                    let json_schema = match request.interaction_kind {
                        ModelInteractionKind::AutoSizing => claude_code_auto_sizing_json_schema()?,
                        ModelInteractionKind::MacroJudge => claude_code_macro_judge_json_schema()?,
                        _ => unreachable!(
                            "structured JSON branch is limited to structured interactions"
                        ),
                    };
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
                        json_schema: Some(&json_schema),
                    })
                    .await?;
                    if output.assistant_text.is_empty() && output.structured_output.is_none() {
                        return Err(claude_code_empty_output_error(&output.stderr).into());
                    }
                    let raw_text = if request.interaction_kind == ModelInteractionKind::AutoSizing {
                        validate_claude_code_auto_sizing_output(
                            &output.assistant_text,
                            output.structured_output.as_deref(),
                        )?
                    } else {
                        output.structured_output.unwrap_or(output.assistant_text)
                    };
                    (raw_text, output.usage, None, None)
                } else {
                    let output = run_claude_code_request_with_corrective_retry(
                        &self.program,
                        request,
                        self.timeout_ms,
                    )
                    .await?;
                    (
                        output.raw_text,
                        output.usage,
                        output.latest_request_usage,
                        Some(output.action_batch),
                    )
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
                if claude_code_session_error_kind(error.message(), error.provider_raw_text())
                    == ClaudeCodeSessionErrorKind::Active
                    && attempt < CLAUDE_CODE_SESSION_LOCK_RETRY_ATTEMPTS =>
            {
                tokio::time::sleep(Duration::from_millis(
                    CLAUDE_CODE_SESSION_LOCK_RETRY_DELAY_MS,
                ))
                .await;
            }
            Err(error)
                if claude_code_session_error_kind(error.message(), error.provider_raw_text())
                    == ClaudeCodeSessionErrorKind::Missing =>
            {
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
                        if matches!(
                            claude_code_session_error_kind(
                                error.message(),
                                error.provider_raw_text()
                            ),
                            ClaudeCodeSessionErrorKind::Active
                                | ClaudeCodeSessionErrorKind::Existing
                        ) && attempt < CLAUDE_CODE_SESSION_LOCK_RETRY_ATTEMPTS =>
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
    let json_schema = request.json_schema.filter(|schema| !schema.is_empty());
    let settings_file = ClaudeCodeSettingsFile::write(request.model, json_schema.is_some())?;
    let system_prompt_file = ClaudeCodeSystemPromptFile::write(request.system_prompt)?;
    let mut child = loop {
        let mut command = Command::new(request.program);
        command.arg("--print");
        command
            .arg("--settings")
            .arg(settings_file.path())
            .arg("--permission-mode")
            .arg("dontAsk");
        match request.session {
            Some(ClaudeCodeSessionInvocation::Create { session_id }) => {
                command.arg("--session-id").arg(session_id);
            }
            Some(ClaudeCodeSessionInvocation::Resume { session_id }) => {
                command.arg("--resume").arg(session_id);
            }
            None => {}
        }
        if let Some(system_prompt_file) = &system_prompt_file {
            command
                .arg("--append-system-prompt-file")
                .arg(system_prompt_file.path());
        }
        if let Some(effort) = request.reasoning_effort.filter(|effort| !effort.is_empty()) {
            command.arg("--effort").arg(effort);
        }
        if request.json_output {
            command.arg("--output-format").arg("json");
        }
        if let Some(schema) = json_schema {
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
    let ClaudeCodeOutput {
        assistant_text,
        structured_output,
        usage,
    } = if request.json_output {
        parse_claude_code_json_output(&stdout)?
    } else {
        ClaudeCodeOutput {
            assistant_text: stdout,
            structured_output: None,
            usage: ModelTokenUsage::default(),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the Claude Code disallowed tool list omits stale tool names
    /// that are not part of the current Claude native tool surface.
    #[test]
    fn claude_code_disallowed_tools_omit_nonexistent_native_tools() {
        for stale_tool in ["ListMcpResourceTool", "Powershell", "ReportFindings"] {
            assert!(
                !CLAUDE_CODE_DISALLOWED_NATIVE_TOOLS
                    .split(',')
                    .any(|tool| tool == stale_tool),
                "{stale_tool} should not be listed in disallowed Claude Code tools"
            );
        }
    }
    use crate::agent::{
        AllowedActionSet, ContextSourceKind, ModelMessage, ModelRequest, provider_error_retry_class,
    };
    use mez_agent::{ModelMessageRole, ProviderErrorRetryClass};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// Verifies executable-busy subprocess spawn failures are treated as
    /// transient so parallel test fixtures and real CLI upgrades can recover
    /// with one bounded retry.
    #[test]
    fn claude_code_spawn_error_classifies_executable_busy_as_transient() {
        let error = std::io::Error::from_raw_os_error(26);

        assert!(claude_code_spawn_error_is_transient(&error));
    }

    /// Verifies that Claude Code subprocess output is parsed as MAAP and that
    /// the adapter invokes a model-only print request with direct tools denied.
    #[tokio::test]
    async fn claude_code_provider_parses_print_output_and_denies_direct_tools() {
        let fixture = ClaudeCodeFixture::new("success");
        fixture.write_claude_script(
            r#"#!/bin/sh
printf '%s\n' "$@" > "$0.args"
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--settings" ]; then
        shift
        cat "$1" > "$0.settings"
    elif [ "$1" = "--append-system-prompt-file" ]; then
        shift
        cat "$1" > "$0.system-prompt"
    fi
    shift
done
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
        assert!(!args.contains("--model"), "{args}");
        assert!(!args.contains("--bare"), "{args}");
        assert!(args.contains("--settings"), "{args}");
        assert!(args.contains("--permission-mode"), "{args}");
        assert!(args.contains("dontAsk"), "{args}");
        assert!(!args.contains("--session-id"), "{args}");
        assert!(!args.contains("--resume"), "{args}");
        assert!(!args.contains("--system-prompt"), "{args}");
        assert!(args.contains("--append-system-prompt-file"), "{args}");
        let system_prompt =
            fs::read_to_string(fixture.program.with_extension("system-prompt")).unwrap();
        assert!(
            system_prompt.contains("Developer instruction:\nReturn a final say action."),
            "{system_prompt}"
        );
        assert!(args.contains("--effort"), "{args}");
        assert!(args.contains("high"), "{args}");
        assert!(args.contains("--output-format"), "{args}");
        assert!(args.contains("json"), "{args}");
        assert!(!args.contains("--disallowedTools"), "{args}");
        assert!(!args.contains("--allowedTools"), "{args}");
        let settings = fs::read_to_string(fixture.program.with_extension("settings")).unwrap();
        assert!(
            settings.contains("\"model\": \"claude-sonnet-test\""),
            "{settings}"
        );
        assert!(settings.contains("\"allow\""), "{settings}");
        assert!(settings.contains("\"StructuredOutput\""), "{settings}");
        assert!(settings.contains("\"deny\""), "{settings}");
        assert!(settings.contains("\"SendUserMessage\""), "{settings}");
        assert!(settings.contains("\"Bash\""), "{settings}");
        assert!(settings.contains("\"WebSearch\""), "{settings}");
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

    /// Verifies resumed Claude Code turns replay Mezzanine-managed tool
    /// results through stdin so `--resume` requests keep local execution
    /// context that Claude's native session history does not know about.
    #[tokio::test]
    async fn claude_code_provider_resume_prompt_replays_prior_tool_results() {
        let fixture = ClaudeCodeFixture::new("session-tool-context");
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
        request.prompt_cache_session_id =
            Some(format!("mez-tool-context-{}", current_test_nonce()));
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

        provider.send_request_async(&request).await.unwrap();
        provider.send_request_async(&request).await.unwrap();

        let second_turn_args =
            fs::read_to_string(fixture.program.with_extension("args.3")).unwrap();
        let second_turn_stdin =
            fs::read_to_string(fixture.program.with_extension("stdin.3")).unwrap();

        assert!(second_turn_args.contains("--resume"), "{second_turn_args}");
        assert!(
            second_turn_stdin
                .contains("Prior conversation context (not the current user request):"),
            "{second_turn_stdin}"
        );
        assert!(
            second_turn_stdin.contains("Previous tool result:\nPrior tool result."),
            "{second_turn_stdin}"
        );
        assert!(
            second_turn_stdin.contains("Current user request:\nFinal user request."),
            "{second_turn_stdin}"
        );
        assert!(
            !second_turn_stdin.contains("System instruction:"),
            "{second_turn_stdin}"
        );
        assert!(
            !second_turn_stdin.contains("Developer instruction:"),
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
        assert_eq!(response.usage.input_tokens, 2);
        assert_eq!(response.usage.billed_input_tokens(), 6_114);
        assert_eq!(response.usage.output_tokens, 12);
        assert_eq!(response.usage.reasoning_tokens, 0);
        assert_eq!(response.usage.cached_input_tokens, Some(10_496));
        assert_eq!(response.usage.cached_input_hit_ratio_display(), "63.19%");
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
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--settings" ]; then
        shift
        cat "$1" > "$0.settings"
        break
    fi
    shift
done
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
        assert!(args.contains("--settings"), "{args}");
        assert!(args.contains("--permission-mode"), "{args}");
        assert!(args.contains("dontAsk"), "{args}");
        assert!(args.contains("--output-format"), "{args}");
        assert!(args.contains("json"), "{args}");
        assert!(!args.contains("--disallowedTools"), "{args}");
        assert!(!args.contains("--allowedTools"), "{args}");
        let settings = fs::read_to_string(fixture.program.with_extension("settings")).unwrap();
        assert!(
            settings.contains("\"model\": \"claude-sonnet-test\""),
            "{settings}"
        );
        assert!(settings.contains("\"StructuredOutput\""), "{settings}");
        assert!(settings.contains("\"SendUserMessage\""), "{settings}");
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
printf '%s\n' "$@" > "$0.args"
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
        let args = fs::read_to_string(fixture.program.with_extension("args")).unwrap();
        let arg_lines: Vec<&str> = args.lines().collect();

        assert_eq!(response.action_batch, None);
        assert!(arg_lines.contains(&"--output-format"), "{args}");
        assert!(arg_lines.contains(&"json"), "{args}");
        assert!(!arg_lines.contains(&"--allowedTools"), "{args}");
        assert!(arg_lines.contains(&"--json-schema"), "{args}");
        assert!(args.contains("\"reasoning_effort\""), "{args}");
        assert!(args.contains("\"small\""), "{args}");
        assert_eq!(
            response.raw_text.trim(),
            "{\"version\":1,\"size\":\"medium\",\"reasoning_effort\":\"high\",\"confidence\":0.82,\"rationale\":\"coding task needs a medium model\"}"
        );
        assert_eq!(response.usage.input_tokens, 7);
        assert_eq!(response.usage.output_tokens, 11);
        assert_eq!(response.usage.cached_input_tokens, Some(17));
        assert_eq!(response.usage.cache_write_input_tokens, Some(13));
    }

    /// Verifies Claude Code auto-sizing prefers `structured_output` when the
    /// CLI answers the task in prose while also returning a validated router
    /// payload.
    ///
    /// Claude Code can surface parsed JSON separately from the human-readable
    /// `result` field. The provider must treat that structured channel as the
    /// authoritative router decision instead of letting task-answering prose
    /// become the router result.
    #[tokio::test]
    async fn claude_code_provider_prefers_structured_output_for_auto_sizing() {
        let fixture = ClaudeCodeFixture::new("auto-sizing-structured-output");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
cat <<'EOF'
{"type":"result","subtype":"success","is_error":false,"result":"I can implement this by editing the provider and adding tests.","structured_output":{"version":1,"size":"large","reasoning_effort":"high","confidence":0.91,"rationale":"structured output should win"},"usage":{"input_tokens":5,"output_tokens":7}}
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
            serde_json::from_str::<serde_json::Value>(response.raw_text.trim()).unwrap(),
            serde_json::json!({
                "version": 1,
                "size": "large",
                "reasoning_effort": "high",
                "confidence": 0.91,
                "rationale": "structured output should win"
            })
        );
    }

    /// Verifies Claude Code auto-sizing tolerates mixed assistant prose when
    /// exactly one valid router object is embedded in the response text.
    ///
    /// Some routing failures come from Claude Code answering the task before
    /// emitting the router decision. The provider should recover the first
    /// valid top-level JSON object instead of rejecting the whole response.
    #[tokio::test]
    async fn claude_code_provider_accepts_mixed_prose_auto_sizing_output() {
        let fixture = ClaudeCodeFixture::new("auto-sizing-mixed-prose");
        fixture.write_claude_script(
            r#"#!/bin/sh
cat >/dev/null
cat <<'EOF'
{"type":"result","subtype":"success","is_error":false,"result":"I will classify this now.\n{\"version\":1,\"size\":\"medium\",\"reasoning_effort\":\"high\",\"confidence\":0.82,\"rationale\":\"coding task needs a medium model\"}\nUsing that routing choice.","usage":{"input_tokens":7,"output_tokens":11}}
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
