//! Settled local-effect conservation across exact incomplete-inference retries.
//!
//! The existing pane executor records dispatches rather than touching a shell.
//! Complete earlier siblings settle once; plausible incomplete native calls
//! cannot reach that executor, even when their commands repeat earlier effects.

use super::*;

/// Earlier settled shell siblings are evidence for later inference, never a
/// replay queue. An incomplete native batch repeating those effects is rejected
/// before planning; a complete replacement final settles with zero new local
/// dispatches and byte-identical failed/replacement inference requests.
#[test]
fn max_messages_never_redispatches_settled_shell_siblings() {
    let response = |actions| ModelResponse {
        provider: "batch".into(),
        model: "test".into(),
        raw_text: "fixture".into(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        provider_transcript_events: Vec::new(),
        action_batch: Some(MaapBatch {
            rationale: "settle exactly once".into(),
            actions,
        }),
    };
    let incomplete = serde_json::json!({"status":"incomplete","incomplete_details":{"reason":"max_messages"},
        "output":[{"type":"function_call","name":"submit_maap_action_batch","call_id":"partial",
            "arguments":"{\"rationale\":\"repeat\",\"actions\":[{\"type\":\"shell_command\",\"summary\":\"must not run\",\"command\":\"printf task\"}]}"}]}).to_string();
    let provider = SequencedProvider::new(vec![
        Ok(response(vec![
            shell_action("settled-1"),
            shell_action("settled-2"),
        ])),
        Err(
            mez_agent::parse_openai_responses_http_body(&incomplete, "test")
                .unwrap_err()
                .into(),
        ),
        Ok(response(vec![say_action("final", "Complete replacement.")])),
    ]);
    let policy =
        PermissionPolicy::default().with_approval_policy(mez_agent::ApprovalPolicy::FullAccess);
    let approvals = SessionApprovalStore::default();
    let permissions =
        crate::security::permissions::ProductPermissionPlanning::new(&policy, &approvals, None);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".into(),
            model: "test".into(),
            model_capabilities: Default::default(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: Default::default(),
            safety_tier: None,
        },
        permissions: &permissions,
        subagent_scope: None,
        subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: false,
    };
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(0),
            signal: None,
            stdout: framed_shell_output("settled\n"),
            stderr: String::new(),
            timed_out: false,
            interrupted: false,
            transport_diagnostics: Default::default(),
        }),
        ..Default::default()
    };
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: mez_agent::ContextPlacement::ConversationAppend,
        label: "user".into(),
        content: "inspect once".into(),
    }])
    .unwrap();
    let first = runner
        .run_turn_with_shell_executor(
            &mut AgentTurnLedger::new(false),
            turn(),
            context.clone(),
            Path::new("/bin/sh"),
            &mut executor,
            |_| Ok(marker()),
        )
        .unwrap();
    assert_eq!(executor.requests.len(), 2);
    assert!(
        first
            .action_results
            .iter()
            .all(|result| result.status == ActionStatus::Succeeded)
    );
    let mut blocks = context.blocks().to_vec();
    blocks.push(ContextBlock {
        source: ContextSourceKind::ActionResult,
        placement: mez_agent::ContextPlacement::ConversationAppend,
        label: "settled observations".into(),
        content: "settled-1 and settled-2 executed once; do not replay".into(),
    });
    let replacement = runner
        .run_turn_with_shell_executor(
            &mut AgentTurnLedger::new(false),
            turn(),
            AgentContext::new(blocks).unwrap(),
            Path::new("/bin/sh"),
            &mut executor,
            |_| Ok(marker()),
        )
        .unwrap();
    assert_eq!(replacement.terminal_state, AgentTurnState::Completed);
    assert_eq!(
        executor.requests.len(),
        2,
        "neither partial output nor prior siblings may execute again"
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1], requests[2]);
}
