//! Agent tests for agent runtime behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies the shared default action-gate helper exposes the same concrete
/// MCP and memory actions that the selected-model runner adds before provider
/// submission.
///
/// Runtime request-shape diagnostics use this helper without executing a full
/// turn. This regression keeps those diagnostics aligned with the live runner
/// and the SPEC-defined mixed default surface so an initial selected-model
/// request with MCP tools is not reported as a capability-only or memory-only
/// surface.
fn default_action_gates_expose_mcp_and_memory_for_diagnostic_request_shapes() {
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "use any helpful MCP integration before answering".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let tools = vec![McpPromptTool {
        server_id: "gitlab".to_string(),
        tool_name: "get_issue".to_string(),
        description: "Read one GitLab issue".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object"}"#.to_string(),
    }];

    super::apply_default_action_gates(&mut request, &tools, true, false);

    let allowed_actions = request.allowed_actions.action_type_names();
    assert!(allowed_actions.contains(&"mcp_call"));
    assert!(allowed_actions.contains(&"memory_search"));
    assert!(allowed_actions.contains(&"memory_store"));
    assert!(allowed_actions.contains(&"request_capability"));
    assert_eq!(request.available_mcp_tools, tools);
    assert!(request.memory_actions_enabled);
    assert!(!request.issue_actions_enabled);
}

#[test]
/// Verifies available MCP tools do not suppress the persistent-memory surface.
///
/// MCP availability is not a global reason to hide other enabled capabilities.
/// This keeps memory usable for turns that legitimately need durable prior
/// context even when MCP servers are configured.
fn default_action_gates_keep_memory_when_mcp_is_available() {
    let mcp_tool = McpPromptTool {
        server_id: "githubcopilot".to_string(),
        tool_name: "list_ci_results".to_string(),
        description: "Read GitHub CI check results for a repository. User-configured non-authoritative server purpose: GitHub repository and CI operations.".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object"}"#.to_string(),
    };
    let context = crate::agent::append_mcp_context(
        AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "use the githubcopilot mcp server to inspect CI".to_string(),
        }])
        .unwrap(),
        &crate::mcp::McpPromptSummary {
            available_servers: vec![crate::mcp::McpPromptServer {
                server_id: "githubcopilot".to_string(),
                display_name: "GitHub Copilot".to_string(),
                purpose: "GitHub repository and CI operations".to_string(),
                usage_instructions: String::new(),
                tool_count: 1,
                approval_required_tool_count: 0,
            }],
            available_tools: vec![mcp_tool.clone()],
            unavailable_servers: Vec::new(),
        },
    )
    .unwrap();
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &context,
    )
    .unwrap();

    super::apply_default_action_gates(&mut request, std::slice::from_ref(&mcp_tool), true, false);

    let allowed_actions = request.allowed_actions.action_type_names();
    assert!(allowed_actions.contains(&"mcp_call"));
    assert!(allowed_actions.contains(&"memory_search"));
    assert!(allowed_actions.contains(&"memory_store"));
    assert!(allowed_actions.contains(&"request_capability"));
    assert_eq!(request.available_mcp_tools, vec![mcp_tool]);
    assert!(request.memory_actions_enabled);
}

#[test]
/// Verifies that stateful Fish wrappers run through a Fish-native block and
/// evaluate the command in the active shell context, so stateful operations can
/// persist while still reporting OSC 133 transaction boundaries.
fn fish_stateful_wrapper_uses_active_shell_eval_block() {
    let transaction = ShellTransaction::new(
        marker(),
        "t1",
        "a1",
        "p1",
        Path::new("/bin/fish"),
        "cd /tmp",
    )
    .unwrap();

    let wrapper = transaction.render_stateful_for_classification(ShellClassification::Fish);

    assert!(wrapper.contains("begin\n"));
    assert!(wrapper.contains("eval 'cd /tmp'"));
    assert!(wrapper.contains("set -l MEZ_STATUS $status"));
    assert!(!wrapper.contains("command '/bin/fish' -c"));
}

#[test]
/// Verifies shell action result context preserves the recorded output preview
/// bytes exactly instead of stripping echoed commands or Mezzanine wrapper
/// lines.
fn shell_action_result_context_preserves_raw_recorded_output_preview() {
    use crate::agent::ActionContentBlock;

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
        is_error: false,
        error: None,
    };

    let context = action_result_context_content(&result);
    assert!(context.contains("output:\n$ printf 'hello\\n'\nMEZ_MARKER_TOKEN=abc\nhello\n"));
}

#[test]
/// Verifies semantic action names remain valid as ordinary shell arguments.
///
/// The semantic-action guard should reject command-position mistakes without
/// blocking legitimate repository searches for action names or prompt text.
fn shell_command_allows_semantic_action_names_as_arguments() {
    let mut action = shell_action("semantic-argument");
    if let AgentActionPayload::ShellCommand { command, .. } = &mut action.payload {
        *command = "rg apply_patch src/agent".to_string();
    }

    assert!(local_action_plan(&action).unwrap().is_some());
}

#[test]
/// Verifies shell command heredoc validation is lexical rather than a raw
/// substring ban.
///
/// Search commands and diagnostics may need to mention `<<` as quoted data or
/// comments. Those should remain valid, while unquoted here-string forms are
/// rejected with the same repair guidance as heredocs.
fn shell_command_heredoc_validation_allows_quoted_mentions_and_rejects_here_strings() {
    let mut quoted = shell_action("quoted");
    if let AgentActionPayload::ShellCommand { command, .. } = &mut quoted.payload {
        *command = "printf '%s\\n' '<<EOF' # <<comment".to_string();
    }
    assert!(local_action_plan(&quoted).unwrap().is_some());

    let mut here_string = shell_action("here-string");
    if let AgentActionPayload::ShellCommand { command, .. } = &mut here_string.payload {
        *command = "cat <<< value".to_string();
    }
    let error = local_action_plan(&here_string).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("heredoc"), "{}", error.message());
    assert!(
        error.message().contains("apply_patch"),
        "{}",
        error.message()
    );
}

#[test]
/// Verifies model-authored shell commands cannot invoke MAAP action names as
/// shell programs.
///
/// Semantic actions are lowered by Mezzanine, not installed into the pane shell.
/// Rejecting command-position invocations before dispatch prevents the model
/// from turning a recoverable action-choice mistake into `command not found`
/// terminal traffic.
fn shell_command_rejects_semantic_action_invocation_as_shell_program() {
    let mut action = shell_action("semantic-shell");
    if let AgentActionPayload::ShellCommand { command, .. } = &mut action.payload {
        *command = "printf '%s\\n' '*** Begin Patch' | apply_patch".to_string();
    }

    let error = local_action_plan(&action).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        error.message().contains("MAAP action `apply_patch`"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("emit a `apply_patch` action"),
        "{}",
        error.message()
    );
}

#[test]
/// Verifies structured shell-read extraction scopes targets to each shell
/// segment instead of stealing the last file-looking token from a later
/// unrelated command.
fn shell_read_observations_scope_targets_per_shell_segment() {
    let observations = crate::agent::shell_read_observations_for_command(
        "sed -n '300,420p' src/runtime/render/overlay.rs && cat README.md",
    );

    assert_eq!(observations.len(), 2, "{observations:?}");
    assert_eq!(
        observations[0].kind,
        crate::agent::ShellReadObservationKind::Read
    );
    assert_eq!(observations[0].target, "src/runtime/render/overlay.rs");
    assert_eq!(observations[0].ranges.len(), 1);
    assert_eq!(observations[0].ranges[0].start_line, 300);
    assert_eq!(observations[0].ranges[0].end_line, 420);
    assert_eq!(
        observations[1].kind,
        crate::agent::ShellReadObservationKind::Read
    );
    assert_eq!(observations[1].target, "README.md");
    assert!(observations[1].ranges.is_empty());
}

#[test]
/// Verifies turn execution can be converted to transcript entries.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_execution_can_be_converted_to_transcript_entries() {
    let turn = turn();
    let action = shell_action("a1");
    let execution = AgentTurnExecution {
        request: assemble_model_request(
            &ModelProfile {
                provider: "openai".to_string(),
                model: "default".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
            &turn,
            &AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "run pwd".to_string(),
            }])
            .unwrap(),
        )
        .unwrap(),
        response: ModelResponse {
            provider: "openai".to_string(),
            model: "default".to_string(),
            raw_text: "I will inspect the directory.".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![ActionResult::running(
            &turn,
            &action,
            vec!["shell command accepted for pane execution".to_string()],
            None,
        )],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    let entries = transcript_entries_for_execution("conv1", 1, 200, &turn, &execution).unwrap();

    assert_eq!(entries[0].sequence, 1);
    assert_eq!(entries[0].role, TranscriptRole::User);
    assert_eq!(entries[0].content, "run pwd");
    assert!(
        entries
            .iter()
            .all(|entry| entry.role != TranscriptRole::System)
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.role == TranscriptRole::Assistant)
    );
    assert!(entries.iter().any(|entry| {
        entry.role == TranscriptRole::Tool
            && entry
                .content
                .contains("[action_result a1 shell_command running]")
    }));
}
