//! The concrete asynchronous Claude Code model provider adapter.

use super::*;

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
    pub(super) fn with_program(mut self, program: impl Into<String>) -> Result<Self> {
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
