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
/// Verifies durable skill action results keep metadata, not skill text.
///
/// `request_skills` and `call_skill` action bodies can contain complete
/// catalogs or full `SKILL.md` documents. Transcript storage should retain a
/// compact audit summary without letting those workflow instructions become
/// future context payload.
fn skill_action_result_transcript_content_summarizes_skill_payloads() {
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

    assert!(call_transcript.contains("action_type=call_skill"));
    assert!(call_transcript.contains("name=review"));
    assert!(call_transcript.contains("skill_bytes=1024"));
    assert!(!call_transcript.contains("# Skill:"), "{call_transcript}");
    assert!(
        !call_transcript.contains("Do a deep review"),
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

    assert!(catalog_transcript.contains("action_type=request_skills"));
    assert!(catalog_transcript.contains("skills=1"));
    assert!(catalog_transcript.contains("names=review"));
    assert!(
        !catalog_transcript.contains("long description"),
        "{catalog_transcript}"
    );
    assert!(!catalog_transcript.contains("Available skills"));
}
