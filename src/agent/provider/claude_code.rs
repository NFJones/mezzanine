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
/// Maximum stderr bytes retained in provider diagnostics.
const CLAUDE_CODE_STDERR_LIMIT: usize = 8192;

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
            let raw_text =
                run_claude_code_subprocess(&self.program, request, self.timeout_ms).await?;
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
fn claude_code_prompt(request: &ModelRequest) -> String {
    let mut prompt = String::new();
    for message in &request.messages {
        let role = match message.role {
            ModelMessageRole::System => "system",
            ModelMessageRole::Developer => "developer",
            ModelMessageRole::User => "user",
            ModelMessageRole::Assistant => "assistant",
            ModelMessageRole::Tool => "tool",
        };
        prompt.push('<');
        prompt.push_str(role);
        prompt.push_str(">\n");
        prompt.push_str(&message.content);
        prompt.push_str("\n</");
        prompt.push_str(role);
        prompt.push_str(">\n\n");
    }
    prompt.push_str(
        "Respond with the validated Mezzanine MAAP action batch text only. Do not run tools or mutate files directly.\n",
    );
    prompt
}

/// Invokes Claude Code in print mode with direct tool use disabled.
async fn run_claude_code_subprocess(
    program: &str,
    request: &ModelRequest,
    timeout_ms: u64,
) -> Result<String> {
    let prompt = claude_code_prompt(request);
    let mut child = Command::new(program)
        .arg("--print")
        .arg("--model")
        .arg(&request.model)
        .arg("--disallowedTools")
        .arg("*")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code subprocess failed to start: {}",
                redact_claude_code_text(&error.to_string())
            ))
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes()).await.map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code subprocess stdin write failed: {}",
                redact_claude_code_text(&error.to_string())
            ))
        })?;
    }

    let output = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output())
        .await
        .map_err(|_| {
            MezError::invalid_state(format!(
                "Claude Code subprocess timed out after {timeout_ms}ms"
            ))
        })?
        .map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code subprocess wait failed: {}",
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
    if stdout.is_empty() {
        return Err(
            MezError::invalid_state("Claude Code subprocess produced no assistant output")
                .with_provider_raw_text(stderr),
        );
    }
    Ok(stdout)
}

/// Redacts common secret-bearing fragments from subprocess diagnostics.
fn redact_claude_code_text(value: &str) -> String {
    value
        .split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if lower.contains("token")
                || lower.contains("secret")
                || lower.contains("apikey")
                || lower.contains("api_key")
                || lower.contains("authorization")
            {
                "[redacted]".to_string()
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
    use crate::agent::{AllowedActionSet, ContextSourceKind, ModelMessage, ModelRequest};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

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
        assert!(stdin.contains("<developer>"), "{stdin}");
        assert!(
            stdin.contains("Do not run tools or mutate files directly"),
            "{stdin}"
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
    }

    /// Verifies malformed Claude Code output is preserved as provider raw text
    /// through the existing MAAP repair path.
    #[tokio::test]
    async fn claude_code_provider_preserves_malformed_output() {
        let fixture = ClaudeCodeFixture::new("malformed");
        fixture.write_claude_script(
            r#"#!/bin/sh
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
            fs::write(&self.program, script).unwrap();
            let mut permissions = fs::metadata(&self.program).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&self.program, permissions).unwrap();
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
