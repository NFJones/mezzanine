//! DDG final classification through real deferred-action settlement.
//!
//! The adapter uses an offline response fixture. The runtime supplies actual
//! action/attempt ownership, provisional rendering, sibling settlement, and
//! model correction; response HTML supplies no authority or runtime identity.
//! The original service remains outside the caught scenario, so assertion
//! panics cannot bypass independent native/selector cleanup and owner checks.

use super::*;
use crate::integrations::http::AsyncProviderHttpTransport;
use futures_util::FutureExt;
use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

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
    let primary = AssertUnwindSafe(ddg_settlement_scenario(&mut service))
        .catch_unwind()
        .await;
    let cleanup = settle_fixture(
        &mut service,
        |service| service.terminate_all_pane_processes().map(|_| ()),
        |service| service.clear_agent_prompt_inputs(),
        |service| {
            let owner = service.pane_processes().retirement_owner();
            (owner.outstanding(), owner.has_cleanup_failure())
        },
    );
    finish_fixture(primary, cleanup);
}

/// Exercises only real producer/claim/progress/completion interfaces. The caller
/// retains the original manager across every setup and assertion failure.
async fn ddg_settlement_scenario(service: &mut RuntimeSessionService) {
    service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(service, "%1");
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
    // One external action is issued at a time. The sibling remains queued,
    // not independently claimable until the search's terminal result settles.
    assert!(
        service
            .claim_approved_external_action(&turn.turn_id, &fetch.id)
            .unwrap()
            .is_none()
    );
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
    wrong_attempt.presentation.execution = mez_agent::ActionPresentationExecutionIdentity::Attempt(
        format!("{}-stale", search_dispatch.attempt),
    );
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
    let fetch_dispatch = service
        .claim_approved_external_action(&turn.turn_id, &fetch.id)
        .unwrap()
        .expect("queued sibling remains owned after search failure");
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
}

/// Retains actual secondary evidence without exposing arbitrary error/panic
/// text. No reported PID, fresh lease, or replacement cleanup owner is created.
enum FixtureCleanupFailure {
    Native(crate::MezError),
    Panic(&'static str, Box<dyn Any + Send>),
    UnprovenNative { outstanding: usize, failed: bool },
}

impl std::fmt::Debug for FixtureCleanupFailure {
    /// Prints closed stage/count facts only, never retained payload contents.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native(_) => formatter.write_str("NativeError"),
            Self::Panic(stage, _) => formatter.debug_tuple("Panic").field(stage).finish(),
            Self::UnprovenNative {
                outstanding,
                failed,
            } => formatter
                .debug_struct("UnprovenNative")
                .field("outstanding", outstanding)
                .field("failed", failed)
                .finish(),
        }
    }
}

/// Owns the unchanged primary panic together with every secondary cleanup
/// failure; a later failure cannot overwrite an earlier native outcome.
struct FixtureFailure {
    primary: Option<Box<dyn Any + Send>>,
    cleanup: Vec<FixtureCleanupFailure>,
}

/// Attempts both existing cleanup owners exactly once, independently of panic
/// or error, then checks the original retirement owner without native probing.
fn settle_fixture<S>(
    state: &mut S,
    native: impl FnOnce(&mut S) -> crate::Result<()>,
    selectors: impl FnOnce(&mut S),
    owner: impl FnOnce(&S) -> (usize, bool),
) -> Vec<FixtureCleanupFailure> {
    let mut failures = Vec::new();
    match catch_unwind(AssertUnwindSafe(|| native(state))) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => failures.push(FixtureCleanupFailure::Native(error)),
        Err(payload) => failures.push(FixtureCleanupFailure::Panic("native", payload)),
    }
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| selectors(state))) {
        failures.push(FixtureCleanupFailure::Panic("selectors", payload));
    }
    match catch_unwind(AssertUnwindSafe(|| owner(state))) {
        Ok((0, false)) => {}
        Ok((outstanding, failed)) => {
            failures.push(FixtureCleanupFailure::UnprovenNative {
                outstanding,
                failed,
            });
        }
        Err(payload) => failures.push(FixtureCleanupFailure::Panic("owner", payload)),
    }
    failures
}

/// Resumes the identical primary on successful cleanup. Compound failure owns
/// both primary and secondary payloads instead of fabricating a healthy exit.
fn finish_fixture<T>(primary: std::thread::Result<T>, cleanup: Vec<FixtureCleanupFailure>) -> T {
    if cleanup.is_empty() {
        return primary.unwrap_or_else(|payload| resume_unwind(payload));
    }
    eprintln!(
        "DDG fixture cleanup: primary_failed={} secondary={cleanup:?}",
        primary.is_err()
    );
    resume_unwind(Box::new(FixtureFailure {
        primary: primary.err(),
        cleanup,
    }));
}

/// Process-free success and primary-panic controls protect cleanup ordering and
/// identical primary payload custody without spawning a shell or provider.
#[test]
fn ddg_fixture_cleanup_success_preserves_primary_and_runs_both_owners() {
    let mut stages = Vec::new();
    let cleanup = settle_fixture(
        &mut stages,
        |stages| {
            stages.push("native");
            Ok(())
        },
        |stages| stages.push("selectors"),
        |stages| {
            assert_eq!(stages, &["native", "selectors"]);
            (0, false)
        },
    );
    assert!(cleanup.is_empty());
    assert_eq!(finish_fixture(Ok(7), cleanup), 7);
    let original = Box::new(42_u32) as Box<dyn Any + Send>;
    let primary = catch_unwind(AssertUnwindSafe(|| {
        finish_fixture::<()>(Err(original), vec![])
    }))
    .unwrap_err();
    assert_eq!(*primary.downcast::<u32>().unwrap(), 42);
}

/// Process-free injected panics prove selector cleanup and owner inspection
/// still run after native failure, with every primary/secondary payload held.
#[test]
fn ddg_fixture_cleanup_compound_failure_retains_original_evidence() {
    let mut stages = Vec::new();
    let cleanup = settle_fixture(
        &mut stages,
        |stages| {
            stages.push("native");
            std::panic::panic_any(11_u32)
        },
        |stages| {
            stages.push("selectors");
            std::panic::panic_any(12_u32)
        },
        |stages| {
            assert_eq!(stages, &["native", "selectors"]);
            (1, true)
        },
    );
    let failure = catch_unwind(AssertUnwindSafe(|| {
        finish_fixture::<()>(Err(Box::new(10_u32)), cleanup);
    }))
    .unwrap_err()
    .downcast::<FixtureFailure>()
    .unwrap();
    assert_eq!(*failure.primary.unwrap().downcast::<u32>().unwrap(), 10);
    let mut cleanup = failure.cleanup.into_iter();
    let Some(FixtureCleanupFailure::Panic("native", native)) = cleanup.next() else {
        panic!("native evidence missing")
    };
    assert_eq!(*native.downcast::<u32>().unwrap(), 11);
    let Some(FixtureCleanupFailure::Panic("selectors", selectors)) = cleanup.next() else {
        panic!("selector evidence missing")
    };
    assert_eq!(*selectors.downcast::<u32>().unwrap(), 12);
    assert!(matches!(
        cleanup.next(),
        Some(FixtureCleanupFailure::UnprovenNative {
            outstanding: 1,
            failed: true
        })
    ));
    assert!(cleanup.next().is_none());
}

/// A returned native error remains a failure even when the original owner's
/// counts are zero. Selector cleanup still runs, and safe diagnostics cannot
/// leak the retained error's synthetic private-message canary.
#[test]
fn ddg_fixture_cleanup_native_error_is_not_erased_by_zero_owner_counts() {
    let mut selectors_ran = false;
    let cleanup = settle_fixture(
        &mut selectors_ran,
        |_| Err(crate::MezError::invalid_state("PRIVATE_CLEANUP_CANARY")),
        |ran| *ran = true,
        |_| (0, false),
    );
    assert!(selectors_ran);
    assert_eq!(format!("{cleanup:?}"), "[NativeError]");
    let failure = catch_unwind(AssertUnwindSafe(|| finish_fixture(Ok(()), cleanup)))
        .unwrap_err()
        .downcast::<FixtureFailure>()
        .unwrap();
    assert!(failure.primary.is_none());
    let mut cleanup = failure.cleanup.into_iter();
    let Some(FixtureCleanupFailure::Native(error)) = cleanup.next() else {
        panic!("native error evidence missing")
    };
    assert_eq!(error.to_string(), "InvalidState: PRIVATE_CLEANUP_CANARY");
    assert!(cleanup.next().is_none());
}
