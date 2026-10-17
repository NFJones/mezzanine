//! DDG final classification through real deferred-action settlement.
//!
//! The adapter uses an offline response fixture. The runtime supplies actual
//! action/attempt ownership, provisional rendering, sibling settlement, and
//! model correction; response HTML supplies no authority or runtime identity.

use super::*;
use crate::integrations::http::AsyncProviderHttpTransport;

/// Supplies one pending response with a complete-looking provisional anchor.
struct PendingSearchResponse;

impl AsyncProviderHttpTransport for PendingSearchResponse {
    fn send_async<'a>(
        &'a self,
        _request: &'a mez_agent::ProviderHttpRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async {
            Ok(mez_agent::ProviderHttpResponse {
                status_code: 202,
                headers: Default::default(),
                body: "<a class=\"result__a\" href=\"https://example.test/provisional\">PROVISIONAL_DDG_HIT</a>".into(),
            })
        })
    }
}

/// A complete-looking streamed hit cannot promote a pending search to success.
/// Its final failure removes provisional rows, retires the exact attempt and
/// revision, preserves the successful fetch sibling, and queues one correction
/// containing both results without retrying either external action.
#[tokio::test]
async fn runtime_unusable_ddg_result_retires_progress_and_preserves_sibling_correction() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Debug)
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "research offline sources")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|row| row.turn_id == started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    let search = mez_agent::AgentAction {
        id: "ddg-pending".into(),
        payload: mez_agent::AgentActionPayload::WebSearch {
            query: "offline fixture".into(),
            domains: vec![],
            recency_days: None,
            max_results: None,
        },
    };
    let fetch = mez_agent::AgentAction {
        id: "ddg-sibling".into(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/good".into(),
            format: None,
            max_bytes: None,
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "inspect sources".into(),
                actions: vec![search.clone(), fetch.clone()],
            }),
            provider_transcript_events: vec![],
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: [&search, &fetch]
            .into_iter()
            .map(|action| {
                mez_agent::ActionResult::running(&turn, action, vec!["queued".into()], None)
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
    let search_dispatch = service
        .claim_approved_external_action(&turn.turn_id, &search.id)
        .unwrap()
        .unwrap();
    let fetch_dispatch = service
        .claim_approved_external_action(&turn.turn_id, &fetch.id)
        .unwrap()
        .unwrap();
    let mut sources = vec![];
    let failed = crate::integrations::agent::network::execute_network_action_with_transport_async_with_progress(
        &turn, &search, &PendingSearchResponse, &mut |source| sources.push(source),
    ).await.unwrap();
    assert_eq!(failed.status, ActionStatus::Failed);
    assert_eq!(
        failed.error.as_ref().unwrap().code,
        "network_search_unusable_response"
    );
    assert_eq!(sources.len(), 1);
    let mut progress = crate::runtime::RuntimeApprovedExternalActionProgress {
        presentation: mez_agent::ActionPresentationProgress::new(
            turn.turn_id.clone(),
            search.id.clone(),
            mez_agent::ActionPresentationExecutionIdentity::Attempt(
                search_dispatch.attempt.clone(),
            ),
            1,
            mez_agent::ActionPresentationComponentIdentity::ProvisionalReadBody,
            sources.remove(0),
        ),
    };
    let mut wrong_attempt = progress.clone();
    wrong_attempt.presentation.execution =
        mez_agent::ActionPresentationExecutionIdentity::Attempt(fetch_dispatch.attempt.clone());
    assert!(
        !service
            .apply_approved_external_action_progress(wrong_attempt)
            .unwrap()
    );
    assert!(
        service
            .apply_approved_external_action_progress(progress.clone())
            .unwrap()
    );
    assert!(
        !service
            .apply_approved_external_action_progress(progress.clone())
            .unwrap()
    );
    let before = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(before.contains("PROVISIONAL_DDG_HIT"), "{before}");
    let replay = failed.clone();
    assert!(
        service
            .complete_approved_external_action(
                crate::runtime::RuntimeApprovedExternalActionOutcome {
                    turn_id: turn.turn_id.clone(),
                    action_id: search.id.clone(),
                    attempt: search_dispatch.attempt.clone(),
                    result: Ok(failed),
                    mcp_transport: None,
                }
            )
            .unwrap()
    );
    let after = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!after.contains("PROVISIONAL_DDG_HIT"), "{after}");
    assert!(!service.agent_provider_task_is_pending(&turn.turn_id));
    progress.presentation.revision = 2;
    assert!(
        !service
            .apply_approved_external_action_progress(progress)
            .unwrap()
    );
    assert!(
        service
            .complete_approved_external_action(
                crate::runtime::RuntimeApprovedExternalActionOutcome {
                    turn_id: turn.turn_id.clone(),
                    action_id: fetch.id.clone(),
                    attempt: fetch_dispatch.attempt,
                    result: Ok(mez_agent::ActionResult::succeeded(
                        &turn,
                        &fetch,
                        vec!["USABLE_SIBLING_BODY".into()],
                        None
                    )),
                    mcp_transport: None,
                }
            )
            .unwrap()
    );
    assert!(service.agent_provider_task_is_pending(&turn.turn_id));
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    for marker in ["network_search_unusable_response", "USABLE_SIBLING_BODY"] {
        assert_eq!(
            context
                .blocks()
                .iter()
                .filter(|block| block.source == ContextSourceKind::ActionResult
                    && block.content.contains(marker))
                .count(),
            1,
            "{marker}"
        );
    }
    assert!(
        context
            .blocks()
            .iter()
            .all(|block| !block.content.contains("PROVISIONAL_DDG_HIT")
                && !block.content.contains("No web results were found."))
    );
    assert!(
        !service
            .complete_approved_external_action(
                crate::runtime::RuntimeApprovedExternalActionOutcome {
                    turn_id: turn.turn_id.clone(),
                    action_id: search.id,
                    attempt: search_dispatch.attempt,
                    result: Ok(replay),
                    mcp_transport: None,
                }
            )
            .unwrap()
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    for marker in ["network_search_unusable_response", "USABLE_SIBLING_BODY"] {
        assert_eq!(
            context
                .blocks()
                .iter()
                .filter(|block| block.source == ContextSourceKind::ActionResult
                    && block.content.contains(marker))
                .count(),
            1
        );
    }
    service.terminate_all_pane_processes().unwrap();
}
