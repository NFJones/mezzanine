//! Stateless Claude Code corrective retry.

use super::{
    ClaudeCodeRequestOutput, ClaudeCodeSubprocessRequest, ModelRequest, Result,
    claude_code_corrective_retry_instruction, claude_code_empty_output_error,
    claude_code_maap_json_schema, claude_code_prompt, claude_code_system_prompt,
    parse_claude_code_maap_output, run_claude_code_subprocess,
};

/// Runs one bounded Claude Code request and applies one corrective retry for
/// empty or malformed MAAP output.
pub(super) async fn run_claude_code_request_with_corrective_retry(
    program: &str,
    request: &ModelRequest,
    timeout_ms: u64,
) -> Result<ClaudeCodeRequestOutput> {
    let maap_json_schema = claude_code_maap_json_schema(request)?;
    let first_prompt = claude_code_prompt(request, None);
    let first_system_prompt = claude_code_system_prompt(request, None);
    let first_output = run_claude_code_subprocess(ClaudeCodeSubprocessRequest {
        program,
        model: &request.model,
        system_prompt: &first_system_prompt,
        prompt: &first_prompt,
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
    let retry_system_prompt = claude_code_system_prompt(request, Some(retry_instruction));
    let retry_output = run_claude_code_subprocess(ClaudeCodeSubprocessRequest {
        program,
        model: &request.model,
        system_prompt: &retry_system_prompt,
        prompt: &retry_prompt,
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
