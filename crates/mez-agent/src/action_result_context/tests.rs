//! Direct action-result context and transcript rendering tests.
//!
//! These regressions exercise only canonical lower-crate result contracts and
//! deterministic projections. Product execution and transcript persistence
//! remain covered by integration tests in the root crate.

use super::*;
use crate::{ActionContentBlock, AgentActionResultIdentity, AgentTurnResultIdentity};

/// Stable synthetic turn identity for action-result rendering tests.
struct TestTurn;

impl AgentTurnResultIdentity for TestTurn {
    fn turn_id(&self) -> &str {
        "turn-1"
    }

    fn agent_id(&self) -> &str {
        "agent-1"
    }
}

/// Stable synthetic action identity with a configurable MAAP action type.
struct TestAction {
    id: &'static str,
    action_type: &'static str,
}

impl AgentActionResultIdentity for TestAction {
    fn action_id(&self) -> &str {
        self.id
    }

    fn action_type(&self) -> &'static str {
        self.action_type
    }
}

/// Builds one successful synthetic result without product action adapters.
fn succeeded_result(
    id: &'static str,
    action_type: &'static str,
    content: Vec<String>,
    structured_content_json: Option<String>,
) -> ActionResult {
    ActionResult::succeeded(
        &TestTurn,
        &TestAction { id, action_type },
        content,
        structured_content_json,
    )
}

#[test]
/// Verifies model-facing action result context omits audit-only MAAP structure
/// while preserving the command, status, and cleaned output needed for the next
/// model decision.
fn action_result_context_compacts_shell_observation_for_model() {
    let result = succeeded_result(
        "a1",
        "shell_command",
        vec!["shell command exited with status 0".to_string()],
        Some(
            serde_json::json!({
                "summary": "Inspect the current directory",
                "command": "pwd",
                "execution_transport": "pane_shell",
                "sent_to_pane": true,
                "stateful": false,
                "approval": null,
                "matched_rules": [],
                "terminal_observation": {
                    "source": "pty",
                    "stream": "pty_combined",
                    "marker": "abc",
                    "exit_code": 0,
                    "signal": null,
                    "timed_out": false,
                    "combined_output_bytes": 6,
                    "combined_output_preview": "/repo\n",
                    "boundary_state": "end-marker-observed",
                    "output_truncated": false
                }
            })
            .to_string(),
        ),
    );

    let context = action_result_context_content(&result);

    assert!(context.contains("[action_result a1 shell_command succeeded]"));
    assert!(context.contains("command: pwd"));
    assert!(context.contains("execution_transport: pane_shell"));
    assert!(context.contains("sent_to_pane: true"));
    assert!(context.contains("stream: pty_combined"));
    assert!(context.contains("exit_code: 0"));
    assert!(context.contains("output:\n/repo\n"), "{context}");
    assert!(!context.contains("structured_content"), "{context}");
    assert!(!context.contains("approval: null"), "{context}");
    assert!(!context.contains("matched_rules"), "{context}");
    assert!(!context.contains("marker:"), "{context}");
}

#[test]
/// Verifies model-facing shell output preserves file-content-looking lines.
///
/// Shell action results are now the primary way models inspect files before
/// building `apply_patch` hunks. The context cleaner may remove Mezzanine
/// wrapper traffic and echoed commands, but it must not strip prompt-looking
/// prefixes, wrapper-looking lines, or trailing whitespace from real command
/// output because that makes later patch context differ from the actual file.
fn action_result_context_preserves_patch_relevant_shell_output() {
    let command = "sed -n '1,3p' note.txt";
    let result = succeeded_result(
        "a1",
        "shell_command",
        vec!["shell command exited with status 0".to_string()],
        Some(
            serde_json::json!({
                "summary": "Read a file range",
                "command": command,
                "sent_to_pane": true,
                "stateful": false,
                "approval": null,
                "matched_rules": [],
                "terminal_observation": {
                    "source": "pty",
                    "stream": "pty_combined",
                    "marker": "abc",
                    "exit_code": 0,
                    "signal": null,
                    "timed_out": false,
                    "combined_output_bytes": 128,
                    "combined_output_preview": format!("$ {command}\n$ literal prompt line\n> literal continuation line\ntrailing spaces   \nMEZ_MARKER_TOKEN=abc\n__mez_tx_status=0\nordinary output\n"),
                    "boundary_state": "end-marker-observed",
                    "output_truncated": false
                }
            })
            .to_string(),
        ),
    );

    let context = action_result_context_content(&result);

    assert!(
        context.contains(&format!(
            "$ {command}\n$ literal prompt line\n> literal continuation line\ntrailing spaces   \nMEZ_MARKER_TOKEN=abc\n__mez_tx_status=0\nordinary output\n"
        )),
        "{context}"
    );
}

#[test]
/// Verifies model-facing shell context serializes structured read observations
/// as JSON so queries and targets with spaces survive later ledger parsing.
fn action_result_context_preserves_structured_read_observations_with_spaces() {
    let command = r#"rg -n "overlay style" "docs/reference/issue backlog.md""#;
    let result = succeeded_result(
        "a1",
        "shell_command",
        vec!["shell command exited with status 0".to_string()],
        Some(
            serde_json::json!({
                "summary": "Search an issue backlog",
                "command": command,
                "read_observations": [
                    {
                        "kind": "search",
                        "target": "docs/reference/issue backlog.md",
                        "query": "overlay style"
                    }
                ],
                "terminal_observation": {
                    "source": "pty",
                    "stream": "pty_combined",
                    "marker": "abc",
                    "exit_code": 0,
                    "signal": null,
                    "timed_out": false,
                    "combined_output_bytes": 16,
                    "combined_output_preview": "12: overlay style\n",
                    "boundary_state": "end-marker-observed",
                    "output_truncated": false
                }
            })
            .to_string(),
        ),
    );

    let context = action_result_context_content(&result);

    assert!(context.contains("read_observation_json:"), "{context}");
    assert!(
        context.contains(r#""target":"docs/reference/issue backlog.md""#),
        "{context}"
    );
    assert!(context.contains(r#""query":"overlay style""#), "{context}");
}

#[test]
/// Verifies non-shell action result context keeps useful content while pruning
/// null and empty structured fields before feeding it back to the model.
fn action_result_context_prunes_empty_non_shell_data() {
    let result = succeeded_result(
        "say-1",
        "say",
        vec!["hello".to_string()],
        Some(
            r#"{"kind":"say","text":"hello","empty":[],"none":null,"approval":{"required":false},"matched_rules":[],"policy_command":"echo hello","sent_to_pane":false}"#
                .to_string(),
        ),
    );

    let context = action_result_context_content(&result);

    assert!(context.contains("[action_result say-1 say succeeded]"));
    assert!(context.contains("content:\nhello"));
    assert!(context.contains(r#"data: {"kind":"say","text":"hello"}"#));
    assert!(!context.contains("empty"), "{context}");
    assert!(!context.contains("none"), "{context}");
    assert!(!context.contains("approval"), "{context}");
    assert!(!context.contains("matched_rules"), "{context}");
    assert!(!context.contains("policy_command"), "{context}");
    assert!(!context.contains("sent_to_pane"), "{context}");
}

#[test]
/// Verifies model-facing action-result context remains independently bounded at
/// the configured byte ceiling even when the underlying action result retains a
/// larger body. The durable result can keep the full payload while the next
/// provider request receives a compact, marked preview.
fn action_result_context_truncates_large_result_body_at_256k() {
    let result = ActionResult {
        protocol: "maap/1".to_string(),
        turn_id: "turn-1".to_string(),
        agent_id: "agent-1".to_string(),
        action_id: "fetch-large-explicit".to_string(),
        action_type: "fetch_url",
        status: ActionStatus::Succeeded,
        content: vec![ActionContentBlock::text(format!(
            "{}tail-marker",
            "b".repeat(300 * 1024)
        ))],
        structured_content_json: None,
        permission_evaluation: None,
        is_error: false,
        error: None,
    };

    assert!(result.content_text().contains("tail-marker"));
    let context = action_result_context_content(&result);
    assert!(context.contains("[mez: action result content truncated after 262144 bytes]"));
    assert!(!context.contains("tail-marker"), "{context}");
    assert!(
        context.len() < 264 * 1024,
        "context bytes={}",
        context.len()
    );
}

#[test]
/// Verifies shell action result context preserves the recorded output preview
/// bytes exactly instead of stripping echoed commands or Mezzanine wrapper
/// lines.
fn shell_action_result_context_preserves_raw_recorded_output_preview() {
    let result = ActionResult {
        protocol: "maap/1".to_string(),
        turn_id: "turn-1".to_string(),
        agent_id: "agent-1".to_string(),
        action_id: "shell-raw".to_string(),
        action_type: "shell_command",
        status: ActionStatus::Succeeded,
        content: vec![ActionContentBlock::text(
            "shell command exited with status 0".to_string(),
        )],
        structured_content_json: Some(
            serde_json::json!({
                "command": "printf 'hello\\n'",
                "terminal_observation": {
                    "exit_code": 0,
                    "combined_output_preview": "$ printf 'hello\\n'\nMEZ_MARKER_TOKEN=abc\nhello\n"
                }
            })
            .to_string(),
        ),
        permission_evaluation: None,
        is_error: false,
        error: None,
    };

    let context = action_result_context_content(&result);
    assert!(context.contains("output:\n$ printf 'hello\\n'\nMEZ_MARKER_TOKEN=abc\nhello\n"));
}

#[test]
/// Verifies live tool context keeps current-turn evidence while durable shell
/// and MCP transcript projections omit raw bodies and secret sentinels.
fn durable_tool_transcripts_omit_shell_and_mcp_bodies() {
    let shell = succeeded_result(
        "shell-1",
        "shell_command",
        vec!["shell command exited with status 0".to_string()],
        Some(
            serde_json::json!({
                "command": "printf secret",
                "terminal_observation": {
                    "exit_code": 0,
                    "combined_output_preview": "shell-secret-sentinel"
                }
            })
            .to_string(),
        ),
    );
    let mcp = succeeded_result(
        "mcp-1",
        "mcp_call",
        vec!["mcp-secret-sentinel".to_string()],
        Some(r#"{"result":"mcp-secret-sentinel"}"#.to_string()),
    );
    let fetch = succeeded_result(
        "fetch-1",
        "fetch_url",
        vec!["web-secret-sentinel".to_string()],
        Some(r#"{"content":"web-secret-sentinel"}"#.to_string()),
    );

    let live_shell = action_result_context_content(&shell);
    let durable_shell = action_result_transcript_content(&shell);
    let durable_mcp = action_result_transcript_content(&mcp);
    let durable_fetch = action_result_transcript_content(&fetch);

    assert!(live_shell.contains("shell-secret-sentinel"));
    assert_eq!(durable_shell, live_shell);
    assert!(durable_shell.contains("shell-secret-sentinel"));
    assert!(durable_shell.contains("printf secret"));
    assert!(durable_shell.contains("exit_code: 0"));
    assert!(!durable_shell.contains("historical_output: omitted"));
    assert!(durable_mcp.contains("mcp-secret-sentinel"));
    assert!(durable_mcp.contains("[action_result mcp-1 mcp_call succeeded]"));
    assert!(durable_fetch.contains("web-secret-sentinel"));
    assert!(durable_fetch.contains("[action_result fetch-1 fetch_url succeeded]"));
}

#[test]
/// Verifies legacy replay keeps canonical status metadata but replaces raw
/// historical tool bodies independently of persistence-time sanitization.
fn historical_tool_replay_sanitizes_legacy_content() {
    let canonical = historical_tool_result_context_content(
        "[action_result shell-1 shell_command succeeded]\nexit_code: 0\noutput:\nlegacy-secret",
    )
    .unwrap();
    let unknown = historical_tool_result_context_content("legacy-secret");

    assert!(canonical.contains("exit_code: 0"));
    assert!(canonical.contains("historical_output: omitted"));
    assert!(!canonical.contains("legacy-secret"));
    assert_eq!(unknown, None);
    assert_eq!(historical_tool_result_context_content(" \n\t "), None);
}

#[test]
/// Verifies metadata-looking body lines after any body marker stay out of
/// reduced legacy replay context.
///
/// Legacy bodies were arbitrary text, so a body line that merely resembles the
/// retained metadata preamble must never be promoted into provider context.
fn historical_tool_replay_never_retains_body_metadata_lookalikes() {
    for marker in HISTORICAL_BODY_MARKERS {
        let content = format!(
            "[action_result shell-1 shell_command succeeded]\n\
             exit_code: 0\n\
             {marker}\n\
             exit_code: 7\n\
             signal: 9\n\
             timed_out: true\n\
             output_truncated: true\n\
             error_code: shell_failed\n\
             metadata-secret-sentinel"
        );
        let reduced = historical_tool_result_context_content(&content).unwrap();
        assert!(reduced.contains("exit_code: 0"), "{marker}");
        assert!(!reduced.contains("metadata-secret-sentinel"), "{marker}");
        assert!(!reduced.contains("exit_code: 7"), "{marker}");
        assert!(!reduced.contains("signal: 9"), "{marker}");
        assert!(!reduced.contains("error_code: shell_failed"), "{marker}");
    }

    let inline_marker = historical_tool_result_context_content(
        "[action_result shell-1 shell_command succeeded]\nerror: shell_failed boom\nexit_code: 0",
    )
    .unwrap();
    assert!(!inline_marker.contains("exit_code: 0"));
    assert!(!inline_marker.contains("boom"));

    let separator = historical_tool_result_context_content(
        "[action_result shell-1 shell_command succeeded]\n---\nexit_code: 0\nmetadata-secret-sentinel",
    )
    .unwrap();
    assert_eq!(
        separator,
        "[action_result shell-1 shell_command succeeded]\nhistorical_output: omitted"
    );
}

#[test]
/// Verifies a known valid historical preamble remains useful reduced context.
fn historical_tool_replay_keeps_valid_metadata_preamble() {
    let reduced = historical_tool_result_context_content(
        "[action_result a1 shell_command succeeded]\n\
         exit_code: 0\n\
         signal: 9\n\
         timed_out: true\n\
         output_truncated: true\n\
         error_code: shell_failed\n\
         historical_output: omitted\n\
         legacy-body-after-marker",
    )
    .unwrap();

    assert_eq!(
        reduced,
        concat!(
            "[action_result a1 shell_command succeeded]\n",
            "exit_code: 0\n",
            "signal: 9\n",
            "timed_out: true\n",
            "output_truncated: true\n",
            "error_code: shell_failed\n",
            "historical_output: omitted"
        )
    );
    assert!(!reduced.contains("legacy-body-after-marker"));
}

#[test]
/// Verifies ambiguous headers, control characters, duplicated or unknown
/// fields, and invalid scalars are omitted rather than reduced.
fn historical_tool_replay_omits_malformed_legacy_content() {
    let omitted = [
        "[action_result shell-1 shell_command]\noutput:\nsecret",
        "[action_result shell-1 shell_command succeeded extra]\noutput:\nsecret",
        "[action_result shell 1 shell_command succeeded]\noutput:\nsecret",
        "[action_result  shell-1 shell_command succeeded]\noutput:\nsecret",
        "[action_result shell-1 shell_command succeeded-extra]\noutput:\nsecret",
        "[action_result shell\u{7}1 shell_command succeeded]\noutput:\nsecret",
        "[action_result shell-1 shell_command success]\noutput:\nsecret",
        "[action_result \"shell-1\" shell_command succeeded]\noutput:\nsecret",
        "legacy-secret",
    ];
    for content in omitted {
        assert_eq!(
            historical_tool_result_context_content(content),
            None,
            "{content:?}"
        );
    }

    let stopped = [
        "[action_result a1 shell_command succeeded]\nexit_code: 256",
        "[action_result a1 shell_command succeeded]\nexit_code: -1",
        "[action_result a1 shell_command succeeded]\nexit_code: 0x0",
        "[action_result a1 shell_command succeeded]\nexit_code: 0 1",
        "[action_result a1 shell_command succeeded]\nsignal: 0",
        "[action_result a1 shell_command succeeded]\nsignal: -1",
        "[action_result a1 shell_command succeeded]\nsignal: 256",
        "[action_result a1 shell_command succeeded]\nsignal: 9999",
        "[action_result a1 shell_command succeeded]\nsignal: nine",
        "[action_result a1 shell_command succeeded]\ntimed_out: false",
        "[action_result a1 shell_command succeeded]\noutput_truncated: true \nexit_code: 0",
        "[action_result a1 shell_command succeeded]\nerror_code: AKIA-SECRET-SENTINEL",
        "[action_result a1 shell_command succeeded]\nunknown_field: secret\nerror_code: shell_failed",
    ];
    for content in stopped {
        assert_eq!(
            historical_tool_result_context_content(content).unwrap(),
            "[action_result a1 shell_command succeeded]\nhistorical_output: omitted",
            "{content:?}"
        );
    }

    // A validated scalar before the stopping line survives; everything after the
    // first duplicate, unknown, or invalid line does not.
    let stopped_after_scalar = [
        "[action_result a1 shell_command succeeded]\nexit_code: 0\nexit_code: 0",
        "[action_result a1 shell_command succeeded]\nexit_code: 0\ncommand: printf secret",
        "[action_result a1 shell_command succeeded]\nexit_code: 0\noutput:\nexit_code: 7",
        "[action_result a1 shell_command succeeded]\nexit_code: 0\nsignal: 300",
    ];
    for content in stopped_after_scalar {
        assert_eq!(
            historical_tool_result_context_content(content).unwrap(),
            "[action_result a1 shell_command succeeded]\nexit_code: 0\nhistorical_output: omitted",
            "{content:?}"
        );
    }
}

#[test]
/// Verifies adversarial legacy lengths and shapes never panic and always stay
/// bounded.
fn historical_tool_replay_stays_bounded_for_adversarial_input() {
    let long_body = "body-secret-sentinel".repeat(50_000);
    let long_token = "a".repeat(50_000);
    let cases = [
        format!("[action_result a1 shell_command succeeded]\noutput:\n{long_body}"),
        format!("[action_result {long_token} shell_command succeeded]\noutput:\n{long_body}"),
        format!("[action_result a1 {long_token} succeeded]\noutput:\n{long_body}"),
        format!(
            "[action_result a1 shell_command succeeded]\nexit_code: {}\n",
            "9".repeat(4096)
        ),
        format!(
            "[action_result a1 shell_command succeeded]\n{}",
            "exit_code: 0\n".repeat(2048)
        ),
        format!(
            "[action_result a1 shell_command succeeded]\n{}",
            "\u{0}".repeat(4096)
        ),
        long_body.clone(),
        String::new(),
        "[action_result".to_string(),
        "[action_result ]".to_string(),
        "[action_result ] ".to_string(),
        "[".repeat(4096),
    ];
    for content in cases {
        if let Some(reduced) = historical_tool_result_context_content(&content) {
            assert!(reduced.len() <= 4096, "{reduced:?}");
            assert!(!reduced.contains("body-secret-sentinel"));
        }
    }
    assert_eq!(historical_tool_result_context_content(""), None);
}

#[test]
/// Verifies error codes that real producers pass to `ActionResult::failed` and
/// its shell-transaction failure carriers survive legacy preamble reduction.
///
/// These codes were missing from the allowlist, so real historical preambles
/// were cut off at the `error_code` line and lost every retained scalar that
/// followed it.
fn historical_tool_replay_retains_producer_error_codes() {
    let producer_codes = [
        "agent_aborted",
        "message_recipient_forbidden",
        "invalid_message_recipient",
        "transport_error",
        "permission_denied",
        "macro_bridge_error",
        "macro_step_ordering",
        "pane_not_ready",
        "foreground_process_blocked_dispatch",
        "issues_disabled",
        "memory_disabled",
        "memory_store_unavailable",
        "approval_disapproved",
        "user_only_host_access",
        "user_only_sandbox_policy",
        "seatbelt_probe_timeout",
        "bubblewrap_probe_output_truncated",
        "skill_not_found",
        "invalid_state",
        "forbidden",
    ];
    for code in producer_codes {
        let reduced = historical_tool_result_context_content(&format!(
            "[action_result a1 shell_command succeeded]\nexit_code: 7\nerror_code: {code}\nlegacy-secret-sentinel"
        ))
        .unwrap();
        assert_eq!(
            reduced,
            format!(
                "[action_result a1 shell_command succeeded]\nexit_code: 7\nerror_code: {code}\nhistorical_output: omitted"
            ),
            "{code}"
        );
        assert!(!reduced.contains("legacy-secret-sentinel"), "{code}");
    }
}

#[test]
/// Verifies skill action results use the same exact representation in active
/// context and durable chronology.
///
/// Once a skill result has been shown to the model, later turns must replay
/// the same bytes rather than replacing its catalog or document body.
fn skill_action_result_transcript_content_preserves_exact_payloads() {
    let call_result = succeeded_result(
        "skill-1",
        "call_skill",
        vec!["# Skill: review\n\nDo a deep review.".to_string()],
        Some(
            serde_json::json!({
                "name": "review",
                "source": "project",
                "path": "/repo/.mez/skills/review/SKILL.md",
                "skill_bytes": 1024,
                "additional_context_bytes": 9,
            })
            .to_string(),
        ),
    );
    let call_transcript = action_result_transcript_content(&call_result);

    assert_eq!(call_transcript, action_result_context_content(&call_result));
    assert!(call_transcript.contains("[action_result skill-1 call_skill succeeded]"));
    assert!(call_transcript.contains(r#""name":"review""#));
    assert!(call_transcript.contains(r#""skill_bytes":1024"#));
    assert!(call_transcript.contains("# Skill:"), "{call_transcript}");
    assert!(
        call_transcript.contains("Do a deep review"),
        "{call_transcript}"
    );

    let catalog_result = succeeded_result(
        "catalog-1",
        "request_skills",
        vec!["Available skills:\n- review (project) - long description".to_string()],
        Some(
            serde_json::json!({
                "skills": [
                    {
                        "name": "review",
                        "description": "long description that should not persist",
                        "source": "project",
                        "path": "/repo/.mez/skills/review/SKILL.md",
                    }
                ],
                "diagnostics": [],
            })
            .to_string(),
        ),
    );
    let catalog_transcript = action_result_transcript_content(&catalog_result);

    assert_eq!(
        catalog_transcript,
        action_result_context_content(&catalog_result)
    );
    assert!(catalog_transcript.contains("[action_result catalog-1 request_skills succeeded]"));
    assert!(
        catalog_transcript.contains("long description"),
        "{catalog_transcript}"
    );
    assert!(catalog_transcript.contains("Available skills"));
}

#[test]
/// Pins the exact producer error-code allowlist used by legacy replay
/// reduction.
///
/// `HISTORICAL_SAFE_ERROR_CODES` is the single shared authority for which
/// `error_code` values stay in reduced legacy replay. It must name every code a
/// durable producer passes to `ActionResult::failed`,
/// `RuntimeShellTransactionActionFailure::code`, or
/// `RuntimeNativeShellFailure::kind`; losing one truncates real historical
/// preambles at the `error_code` line. Update this pin together with the
/// allowlist whenever a producer adds a code.
fn historical_safe_error_codes_pin_full_producer_set() {
    let expected: &[&str] = &[
        "action_failed",
        "agent_aborted",
        "apply_patch_authority_changed",
        "apply_patch_execution_mode_changed",
        "apply_patch_hunk_context_mismatch",
        "apply_patch_hunk_mismatch",
        "apply_patch_payload_cap_exceeded",
        "apply_patch_read_transport_incomplete",
        "apply_patch_snapshot_byte_count_mismatch",
        "apply_patch_snapshot_checksum_mismatch",
        "apply_patch_transport_failed",
        "apply_patch_transport_incomplete",
        "apply_patch_unsafe_path",
        "apply_patch_validation_failed",
        "apply_patch_write_failed",
        "approval_denied",
        "approval_disapproved",
        "bubblewrap_path_resolution_failed",
        "bubblewrap_path_resolution_stale",
        "bubblewrap_pre_payload_failure",
        "bubblewrap_probe_identity_mismatch",
        "bubblewrap_probe_nonzero_exit",
        "bubblewrap_probe_output_mismatch",
        "bubblewrap_probe_output_truncated",
        "bubblewrap_probe_protocol_violation",
        "bubblewrap_probe_stale_identity",
        "bubblewrap_probe_timeout",
        "bubblewrap_probe_write_failed",
        "bubblewrap_status_invalid",
        "bubblewrap_status_mismatch",
        "cancelled",
        "config",
        "config_change_failed",
        "config_invalid",
        "conflict",
        "denied",
        "forbidden",
        "foreground_process_blocked_dispatch",
        "hook_blocked",
        "internal_error",
        "interrupted",
        "invalid_message_payload",
        "invalid_message_recipient",
        "invalid_params",
        "invalid_skill_name",
        "invalid_state",
        "invalidargs",
        "invalidstate",
        "io",
        "issue_dependency_validation_failed",
        "issue_store_unavailable",
        "issues_disabled",
        "macro_bridge_error",
        "macro_step_failed",
        "macro_step_ordering",
        "mcp_blacklisted",
        "mcp_invalid_args",
        "mcp_protocol_error",
        "mcp_schema_changed",
        "mcp_schema_unbound",
        "mcp_server_changed",
        "mcp_tool_error",
        "memory_disabled",
        "memory_store_unavailable",
        "message_recipient_forbidden",
        "method_not_found",
        "network_action_no_progress",
        "network_http_error",
        "network_request_failed",
        "not_found",
        "not_implemented",
        "notfound",
        "notimplemented",
        "pane_input_write_failed",
        "pane_not_ready",
        "permission_denied",
        "policy_forbidden",
        "rate_limited",
        "ratelimited",
        "readiness_probe_timeout",
        "sandbox_failure",
        "seatbelt_established_payload_incomplete",
        "seatbelt_pre_payload_failure",
        "seatbelt_probe_nonzero_exit",
        "seatbelt_probe_output_mismatch",
        "seatbelt_probe_output_truncated",
        "seatbelt_probe_protocol_violation",
        "seatbelt_probe_stale_identity",
        "seatbelt_probe_timeout",
        "seatbelt_probe_write_failed",
        "seatbelt_status_invalid",
        "seatbelt_status_mismatch",
        "shell_command_failed",
        "shell_dispatch_limit_exceeded",
        "shell_executable_not_os_verified",
        "shell_exit_nonzero",
        "shell_failed",
        "shell_identity_probe_failed",
        "shell_interrupted",
        "shell_protocol_violation",
        "shell_timeout",
        "shell_unavailable",
        "skill_catalog_already_requested",
        "skill_context_already_loaded",
        "skill_not_found",
        "timeout",
        "transport_error",
        "unauthorized",
        "unavailable",
        "unsupported",
        "unsupported_url_scheme",
        "user_cancelled",
        "user_only_host_access",
        "user_only_host_policy",
        "user_only_host_power_policy",
        "user_only_sandbox_policy",
        "user_only_transport_policy",
    ];
    assert_eq!(HISTORICAL_SAFE_ERROR_CODES, expected);
    assert!(
        HISTORICAL_SAFE_ERROR_CODES
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "the producer error-code allowlist must stay sorted and duplicate-free"
    );
}

#[test]
/// Verifies legacy replay keeps native signal numbers above the standard
/// 1..=64 range while still rejecting zero, negative, and absurd values.
///
/// The historical producer wrote the raw `ExitStatusExt::signal()` value, an OS
/// `i32` that can exceed the standard signal window, so a real-time signal
/// number must not truncate an otherwise valid metadata preamble.
fn historical_tool_replay_accepts_signal_numbers_above_standard_range() {
    for signal in ["65", "127", "255"] {
        let reduced = historical_tool_result_context_content(&format!(
            "[action_result a1 shell_command succeeded]\nsignal: {signal}\nexit_code: 0\nlegacy-secret-sentinel"
        ))
        .unwrap();
        assert_eq!(
            reduced,
            format!(
                "[action_result a1 shell_command succeeded]\nsignal: {signal}\nexit_code: 0\nhistorical_output: omitted"
            ),
            "{signal}"
        );
        assert!(!reduced.contains("legacy-secret-sentinel"), "{signal}");
    }

    for signal in ["0", "-1", "256", "9999", "nine", "1.0"] {
        let reduced = historical_tool_result_context_content(&format!(
            "[action_result a1 shell_command succeeded]\nsignal: {signal}\nexit_code: 0\nlegacy-secret-sentinel"
        ))
        .unwrap();
        assert_eq!(
            reduced, "[action_result a1 shell_command succeeded]\nhistorical_output: omitted",
            "{signal}"
        );
    }
}

#[test]
/// Verifies a printable punctuated action id keeps its legacy header valid
/// while delimiters, control characters, and over-long tokens stay omitted.
///
/// Action ids are model-supplied strings, so the header grammar must not turn a
/// legitimate header into an omission for punctuation that cannot carry a
/// secret.
fn historical_tool_replay_accepts_punctuated_header_identity_within_byte_cap() {
    let punctuated = historical_tool_result_context_content(
        "[action_result action-1~2#3/4%5@6?7 shell_command succeeded]\nexit_code: 0\nlegacy-secret",
    )
    .unwrap();
    assert_eq!(
        punctuated,
        "[action_result action-1~2#3/4%5@6?7 shell_command succeeded]\nexit_code: 0\nhistorical_output: omitted"
    );

    let at_byte_cap = historical_tool_result_context_content(&format!(
        "[action_result {} shell_command succeeded]\nexit_code: 0",
        "a".repeat(HISTORICAL_HEADER_TOKEN_MAX_BYTES)
    ))
    .unwrap();
    assert!(at_byte_cap.contains("exit_code: 0"), "{at_byte_cap}");

    let over_byte_cap = "a".repeat(HISTORICAL_HEADER_TOKEN_MAX_BYTES + 1);
    let mut rejected = vec![over_byte_cap];
    for delimiter in ['[', ']', '"', '\\'] {
        rejected.push(format!("action{delimiter}1"));
    }
    rejected.push("action\u{7}1".to_string());
    rejected.push("action\u{0}1".to_string());
    for token in rejected {
        assert_eq!(
            historical_tool_result_context_content(&format!(
                "[action_result {token} shell_command succeeded]\noutput:\nsecret"
            )),
            None,
            "{token:?}"
        );
    }
}
