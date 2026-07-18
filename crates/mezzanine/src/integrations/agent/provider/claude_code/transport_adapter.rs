//! Bounded stateless Claude Code subprocess transport.

use super::{
    ClaudeCodeOutput, ClaudeCodeSettingsFile, ClaudeCodeSystemPromptFile, MaapBatch, MezError,
    ModelTokenUsage, Result, bound_claude_code_text, parse_claude_code_json_output,
    redact_claude_code_text,
};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Captures one Claude Code subprocess completion for retry and validation.
pub(super) struct ClaudeCodeSubprocessOutput {
    pub(super) assistant_text: String,
    pub(super) structured_output: Option<String>,
    pub(super) stderr: String,
    pub(super) usage: ModelTokenUsage,
}

/// Stores the Claude Code request result after optional corrective retry.
pub(super) struct ClaudeCodeRequestOutput {
    pub(super) raw_text: String,
    pub(super) action_batch: MaapBatch,
    pub(super) usage: ModelTokenUsage,
    pub(super) latest_request_usage: Option<ModelTokenUsage>,
}

/// Carries the shared subprocess inputs for one Claude Code print invocation.
pub(super) struct ClaudeCodeSubprocessRequest<'a> {
    pub(super) program: &'a str,
    pub(super) model: &'a str,
    pub(super) system_prompt: &'a str,
    pub(super) prompt: &'a str,
    pub(super) reasoning_effort: Option<&'a str>,
    pub(super) timeout_ms: u64,
    pub(super) json_output: bool,
    pub(super) json_schema: Option<&'a str>,
}

/// Invokes one stateless Claude Code print subprocess with direct tool use
/// disabled and the complete canonical history supplied exactly once.
pub(super) async fn run_claude_code_subprocess(
    request: ClaudeCodeSubprocessRequest<'_>,
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
pub(super) fn claude_code_spawn_error_is_transient(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(26)
}
