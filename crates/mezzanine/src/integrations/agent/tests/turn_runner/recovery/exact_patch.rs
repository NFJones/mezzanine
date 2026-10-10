//! Exact decoded patch-shape coverage at the canonical MAAP repair boundary.
//!
//! Valid outer JSON is parsed through the real MAAP parser. Invalid patch
//! arguments at a nonzero action index must reject every sibling before any
//! planning/executor call. Correction stays on the existing bounded scheduler;
//! these tests do not attribute a private historical provider incident.

use super::*;

const TERMINATOR_ERROR: &str = "apply_patch Mezzanine patch blocks must end with *** End Patch";

/// Parses a real-shaped JSON batch with valid siblings around the patch.
/// JSON escaping must preserve the decoded argument bytes exactly; literal
/// backslash-n and physical newlines are deliberately different fixtures.
fn malformed_response(patch: &str) -> ModelResponse {
    let wire = serde_json::json!({
        "rationale":"validate complete replacement atomically",
        "actions":[
            {"type":"shell_command","summary":"first sibling","command":"printf first"},
            {"type":"apply_patch","patch":patch},
            {"type":"shell_command","summary":"last sibling","command":"printf last"}
        ]
    })
    .to_string();
    let batch = mez_agent::parse_maap_action_batch_json(&wire).unwrap();
    let AgentActionPayload::ApplyPatch { patch: decoded, .. } = &batch.actions[1].payload else {
        unreachable!();
    };
    assert_eq!(decoded, patch);
    ModelResponse {
        provider: "batch".into(),
        model: "test".into(),
        raw_text: wire,
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(batch),
        provider_transcript_events: Vec::new(),
    }
}

/// Supplies only the model's complete corrected batch, never an appended suffix
/// or automatically inserted patch terminator. Earlier invalid siblings cannot
/// survive this replacement or enter planning beside it.
fn corrected_response() -> ModelResponse {
    ModelResponse {
        provider: "batch".into(),
        model: "test".into(),
        raw_text: "corrected replacement".into(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            rationale: "choose one valid replacement".into(),
            actions: vec![shell_action("corrected")],
        }),
        provider_transcript_events: Vec::new(),
    }
}

/// Minimal valid current prompt for canonical repair fixture runs.
fn context() -> AgentContext {
    AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: mez_agent::ContextPlacement::ConversationAppend,
        label: "user".into(),
        content: "repair the malformed batch".into(),
    }])
    .unwrap()
}

/// Missing terminator, concatenated directives and literal backslash-n are
/// distinct decoded invalid patch shapes in otherwise valid JSON. Each must
/// offer one same-model/same-turn corrective request and dispatch only the
/// fully validated replacement, with zero invalid-batch sibling executor calls.
#[test]
fn turn_runner_repairs_exact_patch_terminator_and_newline_shapes_atomically() {
    for patch in [
        "*** Begin Patch\n*** Add File: note.txt\n+content\n",
        "*** Begin Patch*** Add File: note.txt+content*** End Patch",
        "*** Begin Patch\\n*** Add File: note.txt\\n+content\\n*** End Patch",
    ] {
        let provider = SequencedProvider::new(vec![
            Ok(malformed_response(patch)),
            Ok(corrected_response()),
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
            available_mcp_servers: vec![],
            available_mcp_tools: &[],
            memory_actions_enabled: false,
            issue_actions_enabled: true,
        };
        let mut ledger = AgentTurnLedger::new(false);
        let mut executor = FakePaneShellExecutor::default();
        let execution = runner
            .run_turn_with_shell_executor(
                &mut ledger,
                turn(),
                context(),
                Path::new("/bin/sh"),
                &mut executor,
                |_| Ok(marker()),
            )
            .unwrap();
        assert_eq!(execution.terminal_state, AgentTurnState::Running);
        let facts = execution.recovery_facts().unwrap();
        assert_eq!(facts.attempts_reserved, 1);
        assert_eq!(facts.repair_calls_started, 1);
        assert_eq!(facts.provider_calls_started, 2);
        assert_eq!(
            facts.disposition,
            mez_agent::recovery_attribution::RecoveryDisposition::CorrectedBatchAccepted
        );
        assert!(facts.batch_planned);
        let mut copied = execution.clone();
        copied.request.turn_id = "another-turn".into();
        assert!(copied.recovery_facts().is_none());
        let records = mez_agent::transcript_entries_for_execution(
            "conversation-1",
            1,
            1,
            &turn(),
            &execution,
        )
        .unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|entry| entry.content.contains("canonical recovery facts"))
                .count(),
            1
        );
        copied = execution.clone();
        copied.request.messages.push(mez_agent::ModelMessage {
            role: mez_agent::ModelMessageRole::Context,
            source: ContextSourceKind::RuntimeHint,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            content: "new request state".into(),
        });
        assert!(copied.recovery_facts().is_none());
        assert_eq!(execution.action_results.len(), 1);
        assert_eq!(execution.action_results[0].action_id, "corrected");
        assert_eq!(executor.requests.len(), 1);
        assert_eq!(executor.requests[0].action_id, "corrected");
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].interaction_kind,
            mez_agent::ModelInteractionKind::MaapRepair
        );
        assert_eq!(requests[1].turn_id, requests[0].turn_id);
        assert_eq!(requests[1].agent_id, requests[0].agent_id);
        assert_eq!(requests[1].model, requests[0].model);
        assert_eq!(requests[1].allowed_actions, requests[0].allowed_actions);
        assert!(
            requests[1]
                .messages
                .iter()
                .any(|message| message.content.contains(TERMINATOR_ERROR))
        );
        assert!(
            execution
                .request
                .messages
                .iter()
                .all(|message| !message.content.contains("[MAAP repair state]"))
        );
    }
}

/// JSON escapes decoding into physical newlines are a valid patch control,
/// whereas literal backslash-n remains malformed. This pins byte semantics
/// without teaching the runner to mechanically rewrite provider payloads.
#[test]
fn turn_runner_exact_patch_json_newline_control_is_valid() {
    let valid = "*** Begin Patch\n*** Add File: note.txt\n+content\n*** End Patch";
    let response = malformed_response(valid);
    let AgentActionPayload::ApplyPatch { patch, .. } =
        &response.action_batch.as_ref().unwrap().actions[1].payload
    else {
        unreachable!();
    };
    assert!(mez_agent::validate_apply_patch_payload(patch).is_ok());
    let literal = malformed_response(&valid.replace('\n', "\\n"));
    let AgentActionPayload::ApplyPatch { patch, .. } =
        &literal.action_batch.as_ref().unwrap().actions[1].payload
    else {
        unreachable!();
    };
    assert!(mez_agent::validate_apply_patch_payload(patch).is_err());
}

/// The exact missing-terminator error consumes the existing two-repair phase
/// budget in async canonical orchestration. A failure summary is a distinct
/// response-only phase with its own bounded malformed-summary repair. Neither
/// summary request can authorize patch dispatch or restart patch correction.
#[tokio::test]
async fn async_turn_runner_exact_patch_terminator_exhausts_two_repairs() {
    for hostile_summary in [false, true] {
        qualify_exhaustion(hostile_summary).await;
    }
}

/// Separates ordinary two-repair exhaustion from the existing summary-only
/// recovery phase, including a valid but forbidden patch/shell summary batch.
async fn qualify_exhaustion(hostile_summary: bool) {
    let patch = "*** Begin Patch\n*** Add File: note.txt\n+content\n";
    let mut summary = corrected_response();
    summary.action_batch.as_mut().unwrap().actions = if hostile_summary {
        vec![
            AgentAction {
                id: "summary-must-not-patch".into(),
                payload: AgentActionPayload::ApplyPatch {
                    patch:
                        "*** Begin Patch\n*** Add File: forbidden.txt\n+must not run\n*** End Patch"
                            .into(),
                    strip: None,
                },
            },
            shell_action("summary-must-not-shell"),
        ]
    } else {
        vec![say_action(
            "summary",
            "Patch validation failed after bounded correction.",
        )]
    };
    let mut responses = vec![
        Ok(malformed_response(patch)),
        Ok(malformed_response(patch)),
        Ok(malformed_response(patch)),
        Ok(summary),
    ];
    if hostile_summary {
        let mut repaired_summary = corrected_response();
        repaired_summary.action_batch.as_mut().unwrap().actions =
            vec![say_action("summary-corrected", "No patch executed.")];
        responses.push(Ok(repaired_summary));
    }
    let provider = SequencedProvider::new(responses);
    let policy = PermissionPolicy::default();
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
        available_mcp_servers: vec![],
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };
    let mut ledger = AgentTurnLedger::new(false);
    let execution = runner
        .run_turn_async(&mut ledger, turn(), context())
        .await
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    let facts = execution.recovery_facts().unwrap();
    assert_eq!(facts.attempts_reserved, 2);
    assert_eq!(facts.repair_calls_started, 2);
    assert_eq!(facts.provider_calls_started, 3);
    assert_eq!(facts.attempt_limit, 2);
    assert_eq!(
        facts.disposition,
        mez_agent::recovery_attribution::RecoveryDisposition::BudgetExhausted
    );
    assert!(!facts.batch_planned);
    assert!(
        execution
            .action_results
            .iter()
            .all(|result| result.action_type == "say")
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), if hostile_summary { 5 } else { 4 });
    assert_eq!(
        requests[..3]
            .iter()
            .filter(
                |request| request.interaction_kind == mez_agent::ModelInteractionKind::MaapRepair
            )
            .count(),
        2
    );
    assert_eq!(
        requests[3].interaction_kind,
        mez_agent::ModelInteractionKind::FailureSummary
    );
    if hostile_summary {
        assert_eq!(
            requests[4].interaction_kind,
            mez_agent::ModelInteractionKind::MaapRepair
        );
        assert!(
            requests[4]
                .messages
                .iter()
                .any(|message| message.content.contains("say"))
        );
    }
    for request in &requests[1..3] {
        assert!(
            request
                .messages
                .iter()
                .any(|message| message.content.contains(TERMINATOR_ERROR))
        );
    }
    assert!(
        execution
            .response
            .raw_text
            .contains("controller_failure_summary:")
    );
}

/// A final summary that omits usage must retain the canonical last concrete
/// request sample (7 input tokens), never the cumulative three-request total
/// (21). This covers both decoded-batch rejection and malformed provider output
/// errors, which hand accounting through different summary adapter entry points.
#[tokio::test]
async fn async_turn_runner_omitted_summary_usage_retains_concrete_request_sample() {
    let single = mez_agent::ModelTokenUsage {
        input_tokens: 7,
        output_tokens: 4,
        reasoning_tokens: 1,
        cached_input_tokens: Some(2),
        cache_write_input_tokens: Some(1),
    };
    for provider_error in [false, true] {
        let mut responses = Vec::new();
        for _ in 0..3 {
            if provider_error {
                responses.push(Err(crate::MezError::invalid_args(
                    "provider MAAP output is malformed: synthetic parse failure",
                )
                .with_provider_incurred_usage(single)));
            } else {
                let mut response =
                    malformed_response("*** Begin Patch\n*** Add File: note.txt\n+content\n");
                response.usage = single;
                responses.push(Ok(response));
            }
        }
        let mut summary = corrected_response();
        summary.action_batch.as_mut().unwrap().actions =
            vec![say_action("summary", "No invalid action executed.")];
        responses.push(Ok(summary));
        let provider = SequencedProvider::new(responses);
        let policy = PermissionPolicy::default();
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
            available_mcp_servers: vec![],
            available_mcp_tools: &[],
            memory_actions_enabled: false,
            issue_actions_enabled: true,
        };
        let mut ledger = AgentTurnLedger::new(false);
        let execution = runner
            .run_turn_async(&mut ledger, turn(), context())
            .await
            .unwrap();
        assert_eq!(execution.terminal_state, AgentTurnState::Failed);
        assert_eq!(execution.response.usage.input_tokens, 21);
        assert_eq!(execution.response.usage.output_tokens, 12);
        assert_eq!(execution.latest_response_usage, single);
        assert_eq!(provider.requests().len(), 4);
        assert!(
            execution
                .action_results
                .iter()
                .all(|result| result.action_type == "say")
        );
    }
}
