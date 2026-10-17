//! Canonical completed-result ownership across fallible pane publication.
//!
//! Initial external coverage uses the legal actor completion/claim boundary
//! already exercised by network tests. The terminal payload is synthetic: this
//! does NOT establish real HTTP/MCP execution counts or worker scheduling. No
//! dispatch transport, provider, live session agent or user state is exercised.
//! Publication retries operate only on the retained immutable source.

use super::*;
use mez_agent::{ActionResult, AgentAction, AgentActionPayload, AgentTurnExecution};

/// Isolated actor state with one accepted claimed fetch and a pending sibling.
/// Keeping the sibling running leaves exact group-owned result/context evidence
/// inspectable without confusing terminal cleanup with result acceptance.
struct CompletionFixture {
    service: RuntimeSessionService,
    root: PathBuf,
    turn: AgentTurnRecord,
    action: AgentAction,
    attempt: String,
    group: String,
    expected: ActionResult,
}

impl CompletionFixture {
    /// Admits a real turn and trusted running-worker execution through the
    /// production actor API, then claims its exact generated attempt. No role,
    /// permission, sandbox or admission bypass is enabled and no network runs.
    async fn fetch() -> Self {
        let root = temp_root("completed-result-owner");
        fs::create_dir(root.join("transcripts")).unwrap();
        let mut service = RuntimeServiceFixture::new()
            .control_socket(root.join("control.sock"))
            .build();
        service.set_agent_transcript_store(AgentTranscriptStore::new(root.join("transcripts")));
        service
            .attach_primary("primary", true, Size::new(120, 32).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .agent_shell_store_mut()
            .set_log_level("%1", AgentLogLevel::Verbose)
            .unwrap();
        let started = service
            .start_agent_prompt_turn("%1", "fetch fixture evidence")
            .unwrap();
        service.remove_pending_agent_provider_task(&started.turn_id);
        let turn = service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .clone();
        let fetch = |id: &str, suffix: &str| AgentAction {
            id: id.to_string(),
            payload: AgentActionPayload::FetchUrl {
                url: format!("https://example.test/{suffix}"),
                format: None,
                max_bytes: None,
            },
        };
        let action = fetch("completed-fetch", "completed-owner");
        let sibling = fetch("pending-sibling", "pending-owner");
        let execution = AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".into(),
                model: "test".into(),
                raw_text: "accepted worker batch".into(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "exercise completed-result retention".into(),
                    actions: vec![action.clone(), sibling.clone()],
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: Default::default(),
            action_results: [&action, &sibling]
                .into_iter()
                .map(|action| {
                    ActionResult::running(
                        &turn,
                        action,
                        vec!["queued for the external worker".into()],
                        None,
                    )
                })
                .collect(),
            final_turn: true,
            terminal_state: AgentTurnState::Running,
        };
        service
            .apply_agent_provider_completed_event(
                &AgentId::opaque(turn.agent_id.clone()).unwrap(),
                &turn.turn_id,
                execution,
            )
            .await
            .unwrap();
        let execution = service
            .agent_turn_executions()
            .get(&turn.turn_id)
            .unwrap()
            .clone();
        service
            .flush_ordered_provider_headers("%1", &execution)
            .unwrap();
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(
            rows.contains("https://example.test/completed-owner"),
            "{rows}"
        );
        assert!(
            service
                .presentation
                .agent_queued_provider_results
                .is_empty()
        );
        assert!(!rows.contains("F668_EXACT_COMPLETED_BODY"));
        let header = service
            .presentation
            .agent_published_provider_headers
            .iter()
            .find(|key| {
                key.0 == "%1"
                    && key.1 == turn.turn_id
                    && key.2 == turn.conversation_id
                    && key.4 == 0
            })
            .expect("exact current pane/conversation/group/ordinal header is published");
        let group = header.3.as_str().to_string();
        let context = runtime_prepared_context_for_turn(&service, &turn.turn_id);
        assert!(
            canonical_event_oracle(&context)
                .iter()
                .any(|event| event.execution_group_id.as_deref() == Some(group.as_str()))
        );
        assert_eq!(execution.action_results[1].action_id, sibling.id);
        assert_eq!(execution.action_results[1].status, ActionStatus::Running);
        assert!(
            service
                .pending_approved_external_actions()
                .contains(&(turn.turn_id.clone(), sibling.id.clone()))
        );
        let dispatch = service
            .claim_approved_external_action(&turn.turn_id, &action.id)
            .unwrap()
            .expect("accepted worker attempt");
        assert!(service.approved_external_action_attempt_is_current(
            &turn.turn_id,
            &action.id,
            &dispatch.attempt,
        ));
        let expected = ActionResult::succeeded(
            &turn,
            &action,
            vec!["F668_EXACT_COMPLETED_BODY".into()],
            Some("{\"fixture_result\":\"exact-terminal\"}".into()),
        );
        Self {
            service,
            root,
            turn,
            action,
            attempt: dispatch.attempt,
            group,
            expected,
        }
    }

    /// Reconstructs only the exact completion message, never a dispatch or
    /// transport execution. Repeated messages are the lost-reply/duplicate oracle.
    fn outcome(&self) -> crate::runtime::RuntimeApprovedExternalActionOutcome {
        crate::runtime::RuntimeApprovedExternalActionOutcome {
            turn_id: self.turn.turn_id.clone(),
            action_id: self.action.id.clone(),
            attempt: self.attempt.clone(),
            result: Ok(self.expected.clone()),
            mcp_transport: None,
        }
    }

    /// Reads current canonical execution, not a returned or locally mutated copy.
    fn retained(&self) -> AgentTurnExecution {
        self.service
            .agent_turn_executions()
            .get(&self.turn.turn_id)
            .expect("retained group execution")
            .clone()
    }
}

impl Drop for CompletionFixture {
    /// Cancels/releases only resources of this generated runtime on every exit.
    fn drop(&mut self) {
        let _ = self.service.terminate_all_pane_processes();
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A terminal external payload must become exact retained execution/context
/// before a single immediate append can fail. No matching provisional body is
/// installed. Publication-only drain and exact duplicate completion cannot rerun
/// work, duplicate canonical context, or falsely report another applied result.
#[tokio::test]
async fn runtime_completion_owner_retains_fetch_before_immediate_install_failure() {
    let mut fixture = CompletionFixture::fetch().await;
    fixture.service.fail_agent_presentation_install_for_tests(0);
    let outcome = fixture.outcome();
    let applied = fixture.service.complete_approved_external_action(outcome);
    let retained = fixture.retained();
    let claim_current = fixture.service.approved_external_action_attempt_is_current(
        &fixture.turn.turn_id,
        &fixture.action.id,
        &fixture.attempt,
    );
    let result = retained
        .action_results
        .iter()
        .find(|result| result.action_id == fixture.action.id)
        .unwrap();
    assert_eq!(
        result.status,
        ActionStatus::Succeeded,
        "terminal payload lost after accepted completion: {applied:?}; claim_current={claim_current}"
    );
    assert_eq!(retained.action_results[1].status, ActionStatus::Running);
    assert_eq!(result.content_text(), fixture.expected.content_text());
    assert_eq!(
        result.structured_content_json,
        fixture.expected.structured_content_json
    );
    let context = runtime_prepared_context_for_turn(&fixture.service, &fixture.turn.turn_id);
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.source == ContextSourceKind::ActionResult
                && block.content.contains("F668_EXACT_COMPLETED_BODY"))
            .count(),
        1
    );
    let oracle = canonical_event_oracle(&context);
    let owned = oracle
        .iter()
        .filter(|event| {
            event.source == ContextSourceKind::ActionResult
                && event.label == format!("action result {}", fixture.action.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(owned.len(), 1);
    assert_eq!(
        owned[0].execution_group_id.as_deref(),
        Some(fixture.group.as_str())
    );
    let canonical_before_duplicate = oracle;
    assert!(
        applied.unwrap(),
        "first canonical retention changes actor state"
    );
    assert!(
        fixture
            .service
            .claim_approved_external_action(&fixture.turn.turn_id, &fixture.action.id)
            .unwrap()
            .is_none()
    );
    assert!(
        !fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id),
        "pending sibling still owns continuation"
    );
    fixture
        .service
        .flush_ordered_provider_headers("%1", &retained)
        .unwrap();
    fixture
        .service
        .flush_ordered_provider_headers("%1", &retained)
        .unwrap();
    let rows = fixture
        .service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(
        rows.matches("F668_EXACT_COMPLETED_BODY").count(),
        1,
        "{rows}"
    );
    let duplicate = fixture.outcome();
    assert!(
        !fixture
            .service
            .complete_approved_external_action(duplicate)
            .unwrap(),
        "an unchanged retained duplicate is not another applied state change"
    );
    let mut foreign = fixture.outcome();
    foreign.attempt.push_str("-foreign");
    assert!(
        !fixture
            .service
            .complete_approved_external_action(foreign)
            .unwrap()
    );
    assert_eq!(fixture.retained().action_results, retained.action_results);
    let context = runtime_prepared_context_for_turn(&fixture.service, &fixture.turn.turn_id);
    assert_eq!(canonical_event_oracle(&context), canonical_before_duplicate);
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.source == ContextSourceKind::ActionResult
                && block.content.contains("F668_EXACT_COMPLETED_BODY"))
            .count(),
        1
    );
    let rows = fixture
        .service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(
        rows.matches("F668_EXACT_COMPLETED_BODY").count(),
        1,
        "{rows}"
    );
}

/// The published-header immediate path must retain its exact immutable result
/// source BEFORE append. Repeated preinstall failure keeps the original entry
/// ID/occurrence; later ordinary publication drain installs that component once.
/// This is presentation ownership coverage, not external execution evidence.
#[tokio::test]
async fn runtime_completion_owner_immediate_result_retains_original_source() {
    let mut fixture = CompletionFixture::fetch().await;
    let execution = fixture.retained();
    fixture.service.fail_agent_presentation_install_for_tests(0);
    assert!(
        fixture
            .service
            .queue_ordered_provider_result("%1", &execution, &fixture.action, &fixture.expected)
            .is_err()
    );
    let pending = fixture
        .service
        .presentation
        .agent_queued_provider_results
        .values()
        .next()
        .expect("failed immediate source remains owned")
        .clone();
    assert_eq!(pending.1, fixture.expected);
    let entry_id = pending.3.id.clone();
    let occurrence = pending.3.occurrence;
    fixture.service.fail_agent_presentation_install_for_tests(0);
    assert!(
        fixture
            .service
            .queue_ordered_provider_result("%1", &execution, &fixture.action, &fixture.expected)
            .is_err()
    );
    let pending = fixture
        .service
        .presentation
        .agent_queued_provider_results
        .values()
        .next()
        .unwrap();
    assert_eq!(pending.3.id, entry_id);
    assert_eq!(pending.3.occurrence, occurrence);
    fixture
        .service
        .flush_ordered_provider_headers("%1", &execution)
        .unwrap();
    fixture
        .service
        .flush_ordered_provider_headers("%1", &execution)
        .unwrap();
    assert!(
        fixture
            .service
            .presentation
            .agent_queued_provider_results
            .is_empty()
    );
    let rows = fixture
        .service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(
        rows.matches("F668_EXACT_COMPLETED_BODY").count(),
        1,
        "{rows}"
    );
}
