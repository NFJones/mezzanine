//! Agent tests for action results behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies model-facing action result context omits audit-only MAAP structure
/// while preserving the command, status, and cleaned output needed for the next
/// model decision.
fn action_result_context_compacts_shell_observation_for_model() {
    let turn = turn();
    let action = shell_action("a1");
    let result = ActionResult::succeeded(
        &turn,
        &action,
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
    let turn = turn();
    let action = shell_action("a1");
    let command = "sed -n '1,3p' note.txt";
    let result = ActionResult::succeeded(
        &turn,
        &action,
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
        context
            .contains(&format!(
                "$ {command}\n$ literal prompt line\n> literal continuation line\ntrailing spaces   \nMEZ_MARKER_TOKEN=abc\n__mez_tx_status=0\nordinary output\n"
            )),
        "{context}"
    );
}

#[test]
/// Verifies model-facing shell context serializes structured read observations
/// as JSON so queries and targets with spaces survive later ledger parsing.
fn action_result_context_preserves_structured_read_observations_with_spaces() {
    let turn = turn();
    let action = shell_action("a1");
    let command = r#"rg -n "overlay style" "docs/reference/issue backlog.md""#;
    let result = ActionResult::succeeded(
        &turn,
        &action,
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
    let turn = turn();
    let action = say_action("say-1", "hello");
    let result = ActionResult::succeeded(
        &turn,
        &action,
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
    use mez_agent::ActionContentBlock;

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
/// Verifies action result invariants match status.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn action_result_invariants_match_status() {
    let turn = turn();
    let action = shell_action("a1");
    let running = ActionResult::running(
        &turn,
        &action,
        vec!["accepted".to_string()],
        Some("{\"command\":\"pwd\"}".to_string()),
    );
    let succeeded = ActionResult::succeeded(
        &turn,
        &action,
        vec!["ok".to_string()],
        Some("{\"command\":\"pwd\"}".to_string()),
    );
    let blocked = ActionResult::blocked(
        &turn,
        &action,
        vec!["approval pending".to_string()],
        "{\"approval\":{\"state\":\"pending\"}}".to_string(),
    );
    let failed = ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Denied,
        "policy_forbidden",
        "command denied",
    )
    .unwrap();

    running.validate_invariants().unwrap();
    succeeded.validate_invariants().unwrap();
    blocked.validate_invariants().unwrap();
    failed.validate_invariants().unwrap();
}

#[test]
/// Verifies semantic file actions keep completion output available for elevated
/// action-result display.
///
/// Normal mode logs a single human-readable action line, but debug-style views
/// still need the semantic lowerings to expose their cleaned output payloads
/// after the hidden shell transaction completes.
fn semantic_file_actions_keep_displayable_completion_output_available() {
    let patch = AgentAction {
        id: "patch-1".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch {
            patch: add_file_patch("note.txt", "one\ntwo\n"),
            strip: None,
        },
    };

    let patch_plan = local_action_plan(&patch).unwrap().unwrap();

    assert!(patch_plan.display_output_after_completion);
    assert_eq!(patch_plan.policy_command, "apply_patch");
    assert!(patch_plan.command.contains("base64"));
    assert!(!patch_plan.command.contains("python3"));
}
