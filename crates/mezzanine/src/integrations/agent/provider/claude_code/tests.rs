//! Claude Code provider and subprocess integration tests.

use super::{
    AsyncModelProvider, CLAUDE_CODE_DISALLOWED_NATIVE_TOOLS, ClaudeCodeProvider,
    ModelInteractionKind, claude_code_spawn_error_is_transient,
};

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
use crate::integrations::agent::{
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

    let first_resume_args = fs::read_to_string(fixture.program.with_extension("args.1")).unwrap();
    let create_args = fs::read_to_string(fixture.program.with_extension("args.2")).unwrap();
    let second_turn_args = fs::read_to_string(fixture.program.with_extension("args.3")).unwrap();
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
    let first_resume_stdin = fs::read_to_string(fixture.program.with_extension("stdin.1")).unwrap();
    let create_stdin = fs::read_to_string(fixture.program.with_extension("stdin.2")).unwrap();
    let second_turn_stdin = fs::read_to_string(fixture.program.with_extension("stdin.3")).unwrap();
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
    request.prompt_cache_session_id = Some(format!("mez-tool-context-{}", current_test_nonce()));
    request.messages = vec![
        ModelMessage {
            role: ModelMessageRole::System,
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            content: "System authority.".to_string(),
        },
        ModelMessage {
            role: ModelMessageRole::Developer,
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            content: "Developer authority.".to_string(),
        },
        ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            content: "Earlier user turn.".to_string(),
        },
        ModelMessage {
            role: ModelMessageRole::Assistant,
            source: ContextSourceKind::RuntimeHint,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            content: "Earlier assistant turn.".to_string(),
        },
        ModelMessage {
            role: ModelMessageRole::Tool,
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            content: "Prior tool result.".to_string(),
        },
        ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            content: "Final user request.".to_string(),
        },
    ];

    provider.send_request_async(&request).await.unwrap();
    provider.send_request_async(&request).await.unwrap();

    let second_turn_args = fs::read_to_string(fixture.program.with_extension("args.3")).unwrap();
    let second_turn_stdin = fs::read_to_string(fixture.program.with_extension("stdin.3")).unwrap();

    assert!(second_turn_args.contains("--resume"), "{second_turn_args}");
    assert!(
        second_turn_stdin.contains("Prior conversation context (not the current user request):"),
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
    let initial_resume_args = fs::read_to_string(fixture.program.with_extension("args.1")).unwrap();
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
        error
            .message()
            .contains("denied StructuredOutput permission required for schema-enforced responses"),
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
            placement: mez_agent::ContextPlacement::EphemeralTail,
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
