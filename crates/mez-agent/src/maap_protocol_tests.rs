//! Provider-independent MAAP parsing and contract-validation tests.
//!
//! These scenarios exercise the canonical lower-crate protocol directly. The
//! product crate retains only tests for its injected shell-command policy.

use crate::*;

/// Minimal active-turn identity used by the moved contract tests.
struct TestTurn;

/// Returns the stable active-turn identity expected by these scenarios.
fn turn() -> TestTurn {
    TestTurn
}

/// Provides the former product validation call shape over the lower contract.
trait MaapBatchProductValidation {
    /// Validates a batch with an accepting shell policy and supplied MCP facts.
    fn validate(
        &self,
        turn: &TestTurn,
        available_mcp_servers: &[String],
        available_mcp_tools: &[McpPromptTool],
    ) -> MaapContractResult<()>;
}

impl MaapBatchProductValidation for MaapBatch {
    fn validate(
        &self,
        _turn: &TestTurn,
        available_mcp_servers: &[String],
        available_mcp_tools: &[McpPromptTool],
    ) -> MaapContractResult<()> {
        self.validate_contract(&MaapValidationContext {
            turn_id: "turn-1",
            agent_id: "agent-1",
            available_mcp_servers,
            available_mcp_tools,
            validate_shell_command: &|_| Ok(()),
        })
    }
}

/// Builds the canonical shell action fixture used by validation scenarios.
fn shell_action(id: &str) -> AgentAction {
    AgentAction {
        id: id.to_string(),
        rationale: "inspect current directory".to_string(),
        payload: AgentActionPayload::ShellCommand {
            summary: "Inspect the current directory".to_string(),
            command: "pwd".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: Some(1000),
        },
    }
}

#[test]
/// Verifies that the fallback parser extracts the one required fenced
/// `mezzanine-action-json` block and maps its JSON schema into MAAP structs.
fn fenced_maap_parser_extracts_shell_action_batch() {
    let raw_text = r#"I will inspect the workspace.
```mezzanine-action-json
{
  "protocol": "maap/1",
  "turn_id": "turn-1",
  "agent_id": "agent-1",
  "rationale": "test action batch rationale",
  "actions": [
    {
      "id": "a1",
      "type": "shell_command",
      "rationale": "List files",
      "summary": "List files in the current directory",
      "command": "ls",
      "interactive": false,
      "stateful": false,
      "timeout_ms": null
    }
  ],
  "final": false
}
```
"#;

    let batch = parse_fenced_maap_action_batch(raw_text).unwrap().unwrap();

    assert_eq!(batch.protocol, "maap/1");
    assert_eq!(batch.turn_id, "turn-1");
    assert!(!batch.final_turn);
    assert_eq!(batch.actions.len(), 1);
    match &batch.actions[0].payload {
        AgentActionPayload::ShellCommand {
            command,
            timeout_ms,
            ..
        } => {
            assert_eq!(command, "ls");
            assert_eq!(*timeout_ms, None);
        }
        payload => panic!("unexpected payload: {payload:?}"),
    }
}

#[test]
/// Verifies that fallback parsing still rejects action objects missing the
/// compact common MAAP fields instead of inventing action types for the model.
fn fenced_maap_parser_rejects_missing_required_action_fields() {
    let raw_text = r#"```mezzanine-action-json
{
  "protocol": "maap/1",
  "turn_id": "turn-1",
  "agent_id": "agent-1",
  "rationale": "test action batch rationale",
  "actions": [
    {
      "id": "say-1",
      "text": "hello"
    }
  ],
  "final": true
}
```"#;

    let error = parse_fenced_maap_action_batch(raw_text).unwrap_err();
    assert!(error.message().contains("type"), "{}", error.message());
}

#[test]
/// Verifies that fallback model output is rejected when it contains multiple
/// action blocks, since the spec requires exactly one fenced MAAP batch.
fn fenced_maap_parser_rejects_multiple_action_blocks() {
    let raw_text = "```mezzanine-action-json\n{}\n```\n```mezzanine-action-json\n{}\n```";

    let error = parse_fenced_maap_action_batch(raw_text).unwrap_err();
    assert!(
        error.message().contains("exactly one"),
        "{}",
        error.message()
    );
}

#[test]
/// Verifies that `apply_patch` accepts Codex block patches during MAAP
/// validation.
///
/// The semantic patch action has a single model-facing format so provider
/// output is validated before any shell-backed mutation is dispatched.
fn maap_batch_accepts_codex_style_apply_patch_blocks() {
    let raw_text = serde_json::json!({
        "rationale": "test action batch rationale",
        "actions": [
            {
                "type": "apply_patch",
                "patch": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
            }
        ]
    })
    .to_string();
    let batch = parse_maap_action_batch_json_for_turn(&raw_text, "turn-1", "agent-1").unwrap();

    batch.validate(&turn(), &[], &[]).unwrap();
}

#[test]
/// Verifies MAAP issue update actions preserve mutable progress notes at the
/// parse boundary and validate through the shared issue-update rules.
fn maap_batch_accepts_issue_update_actions() {
    let raw_text = serde_json::json!({
        "rationale": "test issue update action",
        "actions": [
            {
                "type": "issue_update",
                "id": "issue-1",
                "kind": null,
                "title": null,
                "body": null,
                "clear_body": false,
                "notes": "documented the next step",
                "clear_notes": false,
                "depends_on": ["issue-0"],
                "clear_depends_on": false
            }
        ]
    })
    .to_string();

    let batch = parse_maap_action_batch_json_for_turn(&raw_text, "turn-1", "agent-1").unwrap();
    batch.validate(&turn(), &[], &[]).unwrap();

    match &batch.actions[0].payload {
        AgentActionPayload::IssueUpdate {
            id,
            notes,
            clear_notes,
            depends_on,
            clear_depends_on,
            ..
        } => {
            assert_eq!(id, "issue-1");
            assert_eq!(notes.as_deref(), Some("documented the next step"));
            assert!(!clear_notes);
            assert_eq!(
                depends_on.as_deref(),
                Some(["issue-0".to_string()].as_slice())
            );
            assert!(!clear_depends_on);
        }
        payload => panic!("expected issue_update payload, got {payload:?}"),
    }
}

#[test]
/// Verifies that a non-final model response may contain only conversational
/// output. The runner completes such batches after displaying the text instead
/// of treating a minor `final` flag mismatch as a protocol error.
fn maap_batch_accepts_nonfinal_say_only_actions() {
    let batch = MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-1".to_string(),
        actions: vec![AgentAction {
            id: "say-1".to_string(),
            rationale: "reply to user".to_string(),
            payload: AgentActionPayload::Say {
                status: crate::SayStatus::Progress,
                text: "I will search now".to_string(),
                content_type: crate::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        }],
        final_turn: false,
    };

    batch.validate(&turn(), &[], &[]).unwrap();
}

#[test]
/// Verifies skill discovery and invocation actions parse at the MAAP boundary.
///
/// These actions are non-effecting runtime context actions, so the parser must
/// preserve the model's requested skill name and semantic argument for the
/// runtime skill loader rather than routing them through shell execution.
fn maap_batch_accepts_skill_actions() {
    let raw_text = serde_json::json!({
        "rationale": "test skill action batch rationale",
        "actions": [
            { "type": "request_skills" },
            {
                "type": "call_skill",
                "name": "openai-docs",
                "additional_context": "focus on Responses API examples"
            }
        ]
    })
    .to_string();

    let batch = parse_maap_action_batch_json_for_turn(&raw_text, "turn-1", "agent-1").unwrap();

    assert!(matches!(
        batch.actions[0].payload,
        AgentActionPayload::RequestSkills
    ));
    match &batch.actions[1].payload {
        AgentActionPayload::CallSkill {
            name,
            additional_context,
        } => {
            assert_eq!(name, "openai-docs");
            assert_eq!(
                additional_context.as_deref(),
                Some("focus on Responses API examples")
            );
        }
        payload => panic!("expected call_skill payload, got {payload:?}"),
    }
}

#[test]
/// Verifies maap batch rejects duplicate action ids.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn maap_batch_rejects_duplicate_action_ids() {
    let batch = MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-1".to_string(),
        actions: vec![shell_action("a1"), shell_action("a1")],
        final_turn: false,
    };

    let error = batch.validate(&turn(), &[], &[]).unwrap_err();

    assert!(error.message().contains("duplicate action id"), "{error}");
}

#[test]
/// Verifies that every MAAP batch carries a concise action-batch rationale.
///
/// Normal-mode logging renders this value as the batch-level thinking line, so
/// empty values are rejected before execution can otherwise appear silent.
fn maap_batch_rejects_empty_batch_rationale() {
    let batch = MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "   ".to_string(),
        thought: None,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-1".to_string(),
        actions: vec![shell_action("a1")],
        final_turn: false,
    };

    let error = batch.validate(&turn(), &[], &[]).unwrap_err();
    assert!(error.message().contains("rationale"), "{}", error.message());
}

#[test]
/// Verifies that MAAP shell actions carry explicit user-facing progress text.
/// The runtime displays this summary in the default pane buffer instead of a
/// generic shell-status line, so empty summaries must be rejected before a turn
/// can dispatch.
fn maap_batch_rejects_empty_shell_command_summary() {
    let mut action = shell_action("a1");
    if let AgentActionPayload::ShellCommand { summary, .. } = &mut action.payload {
        summary.clear();
    }
    let batch = MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-1".to_string(),
        actions: vec![action],
        final_turn: false,
    };

    let error = batch.validate(&turn(), &[], &[]).unwrap_err();
    assert!(
        error.message().contains("shell command summary"),
        "{}",
        error.message()
    );
}

#[test]
/// Verifies MAAP validation rejects skill names that cannot map to local skill
/// directories. This protects the runtime loader from path-like names while
/// still keeping skills available as ordinary model-selected context actions.
fn maap_batch_rejects_invalid_skill_names() {
    let raw_text = r#"{"rationale":"test skill validation","actions":[{"type":"call_skill","name":"../bad","additional_context":null}]}"#;

    let batch = parse_maap_action_batch_json_for_turn(raw_text, "turn-1", "agent-1").unwrap();
    let error = batch.validate(&turn(), &[], &[]).unwrap_err();

    assert!(
        error
            .message()
            .contains("call_skill name must contain only lowercase"),
        "{}",
        error.message()
    );
}

#[test]
/// Verifies `fetch_url` remains restricted to HTTP(S) external content for
/// unsupported non-file schemes.
fn maap_batch_rejects_non_http_fetch_url_scheme() {
    let raw_text = serde_json::json!({
        "rationale": "test action batch rationale",
        "actions": [
            {
                "type": "fetch_url",
                "url": "ftp://example.test/data.txt"
            }
        ]
    })
    .to_string();

    let batch = parse_maap_action_batch_json_for_turn(&raw_text, "turn-1", "agent-1").unwrap();
    let error = batch.validate(&turn(), &[], &[]).unwrap_err();

    assert!(error.message().contains("http:// or https://"), "{error}");
    assert!(error.message().contains("shell_command"), "{error}");
}

#[test]
/// Verifies maap batch rejects unavailable mcp server.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn maap_batch_rejects_unavailable_mcp_server() {
    let batch = MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-1".to_string(),
        actions: vec![AgentAction {
            id: "mcp-1".to_string(),
            rationale: "call tool".to_string(),
            payload: AgentActionPayload::McpCall {
                server: "fs".to_string(),
                tool: "read".to_string(),
                arguments_json: "{}".to_string(),
            },
        }],
        final_turn: false,
    };

    let error = batch
        .validate(&turn(), &["git".to_string()], &[])
        .unwrap_err();

    assert!(error.message().contains("unavailable server"), "{error}");
}

#[test]
/// Verifies that MAAP validation rejects MCP actions for tools that were not
/// advertised as currently available, even when the server itself is available.
fn maap_batch_rejects_unavailable_mcp_tool() {
    let batch = MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-1".to_string(),
        actions: vec![AgentAction {
            id: "mcp-1".to_string(),
            rationale: "call disabled tool".to_string(),
            payload: AgentActionPayload::McpCall {
                server: "fs".to_string(),
                tool: "write_file".to_string(),
                arguments_json: "{}".to_string(),
            },
        }],
        final_turn: false,
    };
    let available_tools = vec![McpPromptTool {
        server_id: "fs".to_string(),
        tool_name: "read_file".to_string(),
        description: "Read file".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_string(),
    }];

    let error = batch
        .validate(&turn(), &["fs".to_string()], &available_tools)
        .unwrap_err();
    assert!(
        error.message().contains("unavailable or disabled tool"),
        "{}",
        error.message()
    );
}

#[test]
/// Verifies shell command timeout validation rejects zero values.
///
/// A zero timeout would either expire immediately before the pane shell can
/// consume the wrapper or accidentally collapse into an unbounded/default path.
/// The MAAP boundary should require positive timeout values.
fn maap_batch_rejects_zero_shell_command_timeout() {
    let mut action = shell_action("a1");
    if let AgentActionPayload::ShellCommand { timeout_ms, .. } = &mut action.payload {
        *timeout_ms = Some(0);
    }
    let batch = MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-1".to_string(),
        actions: vec![action],
        final_turn: false,
    };

    let error = batch.validate(&turn(), &[], &[]).unwrap_err();
    assert!(
        error.message().contains("timeout_ms"),
        "{}",
        error.message()
    );
}

#[test]
/// Verifies MAAP issue query validation matches the provider-advertised result
/// bound instead of accepting schema-invalid limits that the store later clamps.
fn maap_batch_validates_issue_query_limit_bounds() {
    let accepted_text = serde_json::json!({
        "rationale": "test issue query upper limit",
        "actions": [
            {
                "type": "issue_query",
                "kind": null,
                "text": null,
                "limit": 200
            }
        ]
    })
    .to_string();
    let accepted =
        parse_maap_action_batch_json_for_turn(&accepted_text, "turn-1", "agent-1").unwrap();
    accepted.validate(&turn(), &[], &[]).unwrap();

    for limit in [0usize, 201usize] {
        let rejected_text = serde_json::json!({
            "rationale": "test issue query invalid limit",
            "actions": [
                {
                    "type": "issue_query",
                    "kind": null,
                    "text": null,
                    "limit": limit
                }
            ]
        })
        .to_string();
        let rejected =
            parse_maap_action_batch_json_for_turn(&rejected_text, "turn-1", "agent-1").unwrap();
        let error = rejected.validate(&turn(), &[], &[]).unwrap_err();

        assert!(
            error.message().contains("issue query limit"),
            "{}",
            error.message()
        );
    }
}

#[test]
/// Verifies compact provider-native MAAP output can carry an optional durable
/// thought field without making it part of the required compact envelope.
fn maap_parser_accepts_optional_batch_thought() {
    let raw_text = serde_json::json!({
        "rationale": "test action batch rationale",
        "thought": "  The display path is separate from durable context.  \nUse verbose logs only.",
        "actions": [
            {
                "type": "say",
                "status": "final",
                "text": "done"
            }
        ]
    })
    .to_string();

    let batch = parse_maap_action_batch_json_for_turn(&raw_text, "turn-1", "agent-1").unwrap();

    assert_eq!(
        batch.thought.as_deref(),
        Some("The display path is separate from durable context.  \nUse verbose logs only.")
    );
    batch.validate(&turn(), &[], &[]).unwrap();
}

#[test]
/// Verifies compact provider-native MAAP output can omit runtime-owned batch
/// fields and default shell fields. Mezzanine stamps identity locally and
/// infers that executable actions require a follow-up provider continuation.
fn maap_parser_fills_compact_provider_defaults() {
    let raw_text = serde_json::json!({
        "rationale": "test action batch rationale",
        "actions": [
            {
                "type": "shell_command",
                "summary": "List files in the current directory",
                "command": "ls"
            }
        ]
    })
    .to_string();

    let batch = parse_maap_action_batch_json_for_turn(&raw_text, "turn-1", "agent-1").unwrap();

    assert_eq!(batch.protocol, "maap/1");
    assert_eq!(batch.rationale, "test action batch rationale");
    assert_eq!(batch.thought, None);
    assert_eq!(batch.turn_id, "turn-1");
    assert_eq!(batch.agent_id, "agent-1");
    assert!(!batch.final_turn);
    assert_eq!(batch.actions[0].id, "action-1");
    assert_eq!(batch.actions[0].rationale, "");
    match &batch.actions[0].payload {
        AgentActionPayload::ShellCommand {
            interactive,
            stateful,
            timeout_ms,
            ..
        } => {
            assert!(!interactive);
            assert!(!stateful);
            assert_eq!(*timeout_ms, None);
        }
        payload => panic!("unexpected payload: {payload:?}"),
    }
    batch.validate(&turn(), &[], &[]).unwrap();
}

#[test]
/// Verifies `say` content types are normalized at the MAAP boundary.
///
/// New provider prompts require models to declare the presentation media type,
/// but the parser still accepts older plain-text responses and canonicalizes
/// common markdown aliases so rendering decisions do not depend on exact model
/// spelling.
fn maap_parser_normalizes_say_content_type() {
    let batch = parse_maap_action_batch_json_for_turn(
        r#"{"rationale":"test action batch rationale","actions":[{"type":"say","status":"final","text":"plain"},{"type":"say","status":"final","content_type":"text/markdown","text":"**rich**"},{"type":"say","status":"final","content_type":"text/diff","text":"--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new"}]}"#,
        "turn-1",
        "agent-1",
    )
    .unwrap();

    match &batch.actions[0].payload {
        AgentActionPayload::Say { content_type, .. } => {
            assert_eq!(content_type, crate::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE);
        }
        payload => panic!("expected say payload, got {payload:?}"),
    }
    match &batch.actions[1].payload {
        AgentActionPayload::Say { content_type, .. } => {
            assert_eq!(content_type, crate::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE);
        }
        payload => panic!("expected say payload, got {payload:?}"),
    }
    match &batch.actions[2].payload {
        AgentActionPayload::Say { content_type, .. } => {
            assert_eq!(content_type, crate::AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE);
        }
        payload => panic!("expected say payload, got {payload:?}"),
    }
}

#[test]
/// Verifies that empty provider-native `say` actions are rejected before batch
/// validation.
///
/// Blank visible text previously disappeared before validation, allowing the
/// runtime to execute the remaining batch without telling the provider which
/// visible action was malformed.
fn maap_parser_rejects_empty_say_actions_before_validation() {
    let raw_text = serde_json::json!({
        "protocol": "maap/1",
        "turn_id": "turn-1",
        "agent_id": "agent-1",
        "rationale": "test action batch rationale",
        "actions": [
            {
                "id": "blank-say",
                "type": "say",
                "status": "progress",
                "rationale": "empty placeholder",
                "text": ""
            },
            {
                "id": "list-files",
                "type": "shell_command",
                "rationale": "list files",
                "summary": "List files in the current directory",
                "command": "ls",
                "interactive": false,
                "stateful": false,
                "timeout_ms": null
            }
        ],
        "final": false
    })
    .to_string();

    let error = parse_maap_action_batch_json(&raw_text).unwrap_err();

    assert!(
        error
            .message()
            .contains("maap action action-1 say text must not be empty"),
        "{}",
        error.message()
    );
}

#[test]
/// Verifies empty `say` text is rejected even when other actions are present.
///
/// Mixed batches used to silently drop malformed visible actions and continue
/// executing the remaining batch. The parser should instead surface a direct
/// diagnostic so the provider can repair the invalid `say` action.
fn maap_parser_rejects_empty_say_text_in_mixed_batch() {
    let error = parse_maap_action_batch_json_for_turn(
        r#"{"rationale":"test action batch rationale","actions":[{"type":"say","status":"progress","text":"   "},{"type":"say","status":"final","text":"done"}]}"#,
        "turn-1",
        "agent-1",
    )
    .unwrap_err();

    assert!(
        error
            .message()
            .contains("maap action action-1 say text must not be empty"),
        "{}",
        error.message()
    );
}

#[test]
/// Verifies compact provider-native MAAP output must include the batch
/// rationale field.
///
/// The provider schema requires this value so normal-mode logging can present a
/// bounded `thinking:` line for the complete action strategy.
fn maap_parser_rejects_missing_batch_rationale() {
    let raw_text = serde_json::json!({
        "actions": [
            {
                "type": "say",
                "status": "final",
                "content_type": crate::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
                "text": "hello"
            }
        ]
    })
    .to_string();

    let error = parse_maap_action_batch_json_for_turn(&raw_text, "turn-1", "agent-1").unwrap_err();
    assert!(error.message().contains("rationale"), "{}", error.message());
}

#[test]
/// Verifies `say.status` is required and restricted to the three terminal
/// intent values the runtime understands.
fn maap_parser_requires_valid_say_status() {
    let missing = parse_maap_action_batch_json_for_turn(
        r#"{"rationale":"test action batch rationale","actions":[{"type":"say","text":"hello"}]}"#,
        "turn-1",
        "agent-1",
    )
    .unwrap_err();
    assert!(missing.message().contains("status"), "{missing}");

    let invalid = parse_maap_action_batch_json_for_turn(
        r#"{"rationale":"test action batch rationale","actions":[{"type":"say","status":"done","text":"hello"}]}"#,
        "turn-1",
        "agent-1",
    )
    .unwrap_err();
    assert!(invalid.message().contains("progress"), "{invalid}");

    let progress = parse_maap_action_batch_json_for_turn(
        r#"{"rationale":"test action batch rationale","actions":[{"type":"say","status":"progress","text":"I will inspect now."}]}"#,
        "turn-1",
        "agent-1",
    )
    .unwrap();
    assert!(!progress.final_turn);

    let blocked = parse_maap_action_batch_json_for_turn(
        r#"{"rationale":"test action batch rationale","actions":[{"type":"say","status":"blocked","text":"I need the missing path."}]}"#,
        "turn-1",
        "agent-1",
    )
    .unwrap();
    assert!(blocked.final_turn);
}

#[test]
/// Verifies that parser compatibility keeps older provider responses usable when
/// they omit the newly required shell summary field. The provider schema and
/// prompt still require `summary`, but a missing summary can be recovered from
/// the required rationale so the user sees a useful progress line instead of a
/// MAAP invalid-args failure.
fn maap_parser_uses_rationale_when_shell_summary_is_missing() {
    let raw_text = serde_json::json!({
        "protocol": "maap/1",
        "turn_id": "turn-1",
        "agent_id": "agent-1",
        "rationale": "test action batch rationale",
        "actions": [
            {
                "id": "list-files",
                "type": "shell_command",
                "rationale": "List files in the current directory",
                "command": "ls",
                "interactive": false,
                "stateful": false,
                "timeout_ms": null
            }
        ],
        "final": false
    })
    .to_string();

    let batch = parse_maap_action_batch_json(&raw_text).unwrap();

    match &batch.actions[0].payload {
        AgentActionPayload::ShellCommand { summary, .. } => {
            assert_eq!(summary, "List files in the current directory");
        }
        payload => panic!("unexpected payload: {payload:?}"),
    }
    batch.validate(&turn(), &[], &[]).unwrap();
}

#[test]
/// Verifies that model-supplied action ids are ignored at the MAAP boundary.
/// Mezzanine assigns stable local ids so downstream action results still have
/// bookkeeping keys without trusting provider-generated identifiers.
fn parser_synthesizes_action_ids_and_ignores_model_ids() {
    let batch = parse_maap_action_batch_json(
        r#"{
          "protocol": "maap/1",
          "turn_id": "turn-1",
          "agent_id": "agent-1",
          "rationale": "test action batch rationale",
          "actions": [
            {"id":"model-picked","type":"say","status":"final","rationale":"Reply","text":"hello"},
            {"type":"say","status":"final","rationale":"Reply again","text":"again"}
          ],
          "final": true
        }"#,
    )
    .unwrap();

    assert_eq!(batch.actions[0].id, "action-1");
    assert_eq!(batch.actions[1].id, "action-2");
}
