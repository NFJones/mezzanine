//! Canonical completed-result ownership across fallible pane publication.
//!
//! Initial external coverage uses the legal actor completion/claim boundary
//! already exercised by network tests. The terminal payload is synthetic: this
//! does NOT establish real HTTP/MCP execution counts or worker scheduling. No
//! dispatch transport, provider, live session agent or user state is exercised.
//! Publication retries operate only on the retained immutable source.

use super::*;
use mez_agent::{ActionResult, AgentAction, AgentActionPayload, AgentTurnExecution};

mod pane_retention;
mod progress_retirement;
mod scope_retention;
mod shutdown_retention;

/// Aggregate and later ordinary error rows share immutable ordered ownership.
/// This is a presentation-only source fixture, not evidence a loop guard fired
/// or an action executed. Rendering values must not modify actor canonical truth.
#[tokio::test]
async fn runtime_completion_owner_aggregate_outcome_keeps_source_and_successful_prefix() {
    for first_install_fails in [true, false] {
        let mut fixture = ordinary_bridge_fixture(true).await;
        let canonical = fixture.retained();
        let mut presentation = canonical.clone();
        presentation.action_results[0] = ActionResult::failed(
            &fixture.turn,
            &fixture.action,
            ActionStatus::Failed,
            "network_action_limit_exceeded",
            "F668_AGGREGATE_SOURCE",
        )
        .unwrap();
        if first_install_fails {
            assert!(
                fixture
                    .service
                    .present_agent_action_outcomes_to_terminal_buffer("%1", &presentation)
                    .is_err()
            );
            let source = fixture
                .service
                .presentation
                .agent_queued_provider_outcomes
                .values()
                .flatten()
                .find(|entry| entry.1.contains("F668_AGGREGATE_SOURCE"))
                .unwrap()
                .clone();
            assert!(
                fixture
                    .service
                    .present_agent_action_outcomes_to_terminal_buffer("%1", &presentation)
                    .is_err()
            );
            let retained = fixture
                .service
                .presentation
                .agent_queued_provider_outcomes
                .values()
                .flatten()
                .find(|entry| entry.1.contains("F668_AGGREGATE_SOURCE"))
                .unwrap();
            assert_eq!(retained.2.id, source.2.id);
            assert_eq!(retained.2.occurrence, source.2.occurrence);
            assert_eq!(retained.3, source.3);
        } else {
            fixture
                .service
                .hold_agent_presentation_installs_for_tests(false);
            fixture.service.fail_agent_presentation_install_for_tests(1);
            // The second component may heal within the existing bounded retry;
            // either way the successful aggregate must not be emitted again.
            let _publication = fixture
                .service
                .present_agent_action_outcomes_to_terminal_buffer("%1", &presentation);
            assert!(
                fixture
                    .service
                    .presentation
                    .agent_install_failure_countdown
                    .is_none(),
                "later component must reach the injected cut"
            );
        }
        fixture
            .service
            .hold_agent_presentation_installs_for_tests(false);
        fixture
            .service
            .present_agent_action_outcomes_to_terminal_buffer("%1", &presentation)
            .unwrap();
        fixture
            .service
            .present_agent_action_outcomes_to_terminal_buffer("%1", &presentation)
            .unwrap();
        let rows = fixture
            .service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert_eq!(rows.matches("F668_AGGREGATE_SOURCE").count(), 1, "{rows}");
        assert_eq!(fixture.retained().action_results, canonical.action_results);
        assert!(
            !fixture
                .service
                .agent_provider_task_is_pending(&fixture.turn.turn_id)
        );
    }
}

/// A trusted local preflight transition may adopt a same-owner failure without
/// granting dispatch authority. Stale expected data and changed request/batch/
/// slot identities cannot overwrite newer canonical work or its frozen source.
/// No dispatcher, provider, assessment, native worker or approval is invoked.
#[tokio::test]
async fn runtime_completion_owner_guarded_canonical_adoption_rejects_foreign_transition() {
    let mut fixture = CompletionFixture::fetch_with_final_turn(false).await;
    let expected = fixture.retained();
    let sibling = expected.response.action_batch.as_ref().unwrap().actions[1].clone();
    let mut proposed = expected.clone();
    proposed.action_results[1] = ActionResult::failed(
        &fixture.turn,
        &sibling,
        ActionStatus::Failed,
        "fixture_preflight_failure",
        "local preflight failed before dispatch",
    )
    .unwrap();
    proposed.terminal_state = AgentTurnState::Failed;
    fixture
        .service
        .adopt_completion_canonical_transition_for_tests(&fixture.turn, &expected, &proposed)
        .unwrap();
    assert_eq!(fixture.retained().action_results, proposed.action_results);
    let snapshot = fixture
        .service
        .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
        .unwrap()
        .unwrap();
    for wrong in ["stale", "request", "batch", "slot"] {
        let mut changed = proposed.clone();
        match wrong {
            "stale" => changed.action_results[1]
                .content
                .push(mez_agent::ActionContentBlock::text("new local content")),
            "request" => changed.request.turn_id.push_str("-foreign"),
            "batch" => changed
                .response
                .action_batch
                .as_mut()
                .unwrap()
                .rationale
                .push_str(" foreign"),
            "slot" => changed.action_results[1].action_id.push_str("-foreign"),
            _ => unreachable!(),
        }
        let original = if wrong == "stale" {
            &expected
        } else {
            &proposed
        };
        assert!(
            fixture
                .service
                .adopt_completion_canonical_transition_for_tests(&fixture.turn, original, &changed)
                .is_err()
        );
        assert_eq!(fixture.retained().action_results, proposed.action_results);
        assert_eq!(
            fixture
                .service
                .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
                .unwrap()
                .unwrap(),
            snapshot
        );
    }
    assert!(fixture.service.approved_external_action_attempt_is_current(
        &fixture.turn.turn_id,
        &fixture.action.id,
        &fixture.attempt
    ));
    assert!(
        !fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id)
    );
    assert!(fixture.service.claim_bookkeeping_candidates().is_empty());
}

/// Required-error latching must not retire a different already-issued delivery.
/// A trusted local preflight failure is adopted while the first external claim
/// remains live. After a genuine post-append lost acknowledgement, its synthetic
/// sibling result still reaches canonical custody; primary error and frozen
/// pre-error source remain unchanged. No transport or assessment executes.
#[tokio::test]
async fn runtime_completion_owner_required_error_keeps_admitted_sibling_delivery_live() {
    let mut fixture = CompletionFixture::fetch_with_final_turn(false).await;
    let expected = fixture.retained();
    let sibling = expected.response.action_batch.as_ref().unwrap().actions[1].clone();
    let mut proposed = expected.clone();
    proposed.action_results[1] = ActionResult::failed(
        &fixture.turn,
        &sibling,
        ActionStatus::Failed,
        "fixture_preflight_failure",
        "unissued local preflight failure",
    )
    .unwrap();
    proposed.terminal_state = AgentTurnState::Failed;
    fixture
        .service
        .adopt_completion_canonical_transition_for_tests(&fixture.turn, &expected, &proposed)
        .unwrap();
    let path = fixture._root.0.join("admitted-sibling-audit.jsonl");
    fixture
        .service
        .set_audit_log(crate::security::audit::AuditLog::new(
            crate::security::audit::AuditConfig {
                enabled: true,
                path: path.clone(),
                hash_chain: false,
                required: true,
            },
        ));
    fixture
        .service
        .reject_completion_failure_audit_ack_for_tests();
    let primary = fixture
        .service
        .append_runtime_agent_execution_failure_audit(&fixture.turn, &proposed)
        .unwrap_err();
    let snapshot = fixture
        .service
        .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
        .unwrap()
        .unwrap();
    let outcome = fixture.outcome();
    let repeated = fixture
        .service
        .complete_approved_external_action(outcome)
        .unwrap_err();
    assert_eq!(repeated.kind(), primary.kind());
    assert_eq!(repeated.message(), primary.message());
    let retained = fixture
        .service
        .retained_completion_execution_for_tests(&fixture.turn.turn_id)
        .unwrap();
    assert_eq!(retained.action_results[0].status, ActionStatus::Succeeded);
    assert_eq!(
        retained.action_results[0].content_text(),
        fixture.expected.content_text()
    );
    assert_eq!(retained.action_results[1], proposed.action_results[1]);
    assert_eq!(
        fixture
            .service
            .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
            .unwrap()
            .unwrap(),
        snapshot
    );
    assert_eq!(
        fixture
            .service
            .completion_failure_audit_phase_for_tests(&fixture.turn.turn_id),
        Some("entered")
    );
    let audit_before_retry = fs::read(&path).unwrap();
    assert!(
        fixture
            .service
            .append_runtime_agent_execution_failure_audit(&fixture.turn, &proposed)
            .is_err()
    );
    assert_eq!(fixture.service.retry_completion_publications(), 0);
    assert_eq!(fs::read(&path).unwrap(), audit_before_retry);
    assert!(
        !fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id)
    );
}

/// Missing audit configuration is explicitly NotRequired, not a claimed write.
/// Attaching a real audit requirement afterward can append once for the same
/// failure; the earlier no-op must not suppress the first required operation.
#[tokio::test]
async fn runtime_completion_owner_failure_audit_not_required_is_not_append_acceptance() {
    let mut fixture = ordinary_bridge_fixture(true).await;
    let execution = fixture.retained();
    fixture
        .service
        .append_runtime_agent_execution_failure_audit(&fixture.turn, &execution)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .completion_failure_audit_phase_for_tests(&fixture.turn.turn_id),
        Some("not_required")
    );
    let path = fixture._root.0.join("attached-required-audit.jsonl");
    fixture
        .service
        .set_audit_log(crate::security::audit::AuditLog::new(
            crate::security::audit::AuditConfig {
                enabled: true,
                path: path.clone(),
                hash_chain: false,
                required: true,
            },
        ));
    fixture
        .service
        .append_runtime_agent_execution_failure_audit(&fixture.turn, &execution)
        .unwrap();
    fixture
        .service
        .append_runtime_agent_execution_failure_audit(&fixture.turn, &execution)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .completion_failure_audit_phase_for_tests(&fixture.turn.turn_id),
        Some("accepted")
    );
    assert_eq!(fs::read_to_string(path).unwrap().lines().count(), 1);
    assert_eq!(
        fixture.service.failure_feedback_attempt_total_for_tests(),
        0
    );
}

/// Builds canonical ordinary settlement under real serialized external claims.
/// Returned bytes are synthetic and no network/provider/assessment executes.
/// A genuine provisional-body rollback keeps failure publication held.
async fn ordinary_bridge_fixture(failed: bool) -> CompletionFixture {
    let mut fixture = CompletionFixture::fetch_with_final_turn(false).await;
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    let first = fixture.outcome();
    assert!(
        fixture
            .service
            .complete_approved_external_action(first)
            .unwrap()
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    let sibling = fixture.retained().response.action_batch.unwrap().actions[1].clone();
    let dispatch = fixture
        .service
        .claim_approved_external_action(&fixture.turn.turn_id, &sibling.id)
        .unwrap()
        .unwrap();
    if failed {
        let progress = mez_agent::ActionPresentationProgress::new(
            &fixture.turn.turn_id,
            &sibling.id,
            mez_agent::ActionPresentationExecutionIdentity::Attempt(dispatch.attempt.clone()),
            1,
            mez_agent::ActionPresentationComponentIdentity::ProvisionalReadBody,
            "unconfirmed ordinary bridge bytes",
        );
        assert!(
            fixture
                .service
                .apply_action_presentation_progress(progress)
                .unwrap()
        );
    }
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    let result = if failed {
        ActionResult::failed(
            &fixture.turn,
            &sibling,
            ActionStatus::Failed,
            "fixture_shared_failure",
            "F668_SHARED_FAILURE",
        )
        .unwrap()
    } else {
        ActionResult::succeeded(
            &fixture.turn,
            &sibling,
            vec!["F668_SHARED_SUCCESS".to_string()],
            None,
        )
    };
    assert!(
        fixture
            .service
            .complete_approved_external_action(
                crate::runtime::RuntimeApprovedExternalActionOutcome {
                    turn_id: fixture.turn.turn_id.clone(),
                    action_id: sibling.id,
                    attempt: dispatch.attempt,
                    result: Ok(result),
                    mcp_transport: None,
                }
            )
            .unwrap()
    );
    // A prior source may already be installed by claim preparation. Acknowledge
    // that exact receipt without pretending the still-failing sibling is ready.
    fixture.service.retry_completion_publications();
    assert!(
        !fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id)
    );
    fixture
}

/// Ordinary success retains an immutable original candidate but does not accept
/// an archive/provider milestone while paint is held. Recovery performs one
/// actual insertion, with no assessment or native invocation.
#[tokio::test]
async fn runtime_completion_owner_ordinary_success_bridge_retains_then_queues() {
    let mut fixture = ordinary_bridge_fixture(false).await;
    assert!(
        !fixture
            .service
            .admit_completion_provider_continuation(&fixture.turn.turn_id)
            .unwrap()
    );
    assert!(
        !fixture
            .service
            .admit_completion_provider_continuation(&fixture.turn.turn_id)
            .unwrap()
    );
    assert!(
        fixture
            .service
            .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
            .unwrap()
            .is_some()
    );
    assert!(
        fixture.service.claim_bookkeeping_candidates().is_empty(),
        "staging is not producer acceptance"
    );
    assert_eq!(
        fixture.service.failure_feedback_attempt_total_for_tests(),
        0
    );
    assert!(
        !fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id)
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    assert!(fixture.service.retry_completion_publications() > 0);
    assert!(
        fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id)
    );
    assert_eq!(fixture.service.retry_completion_publications(), 0);
    fixture.service.ensure_completion_admission_open().unwrap();
}

/// Required failure audit is shared by initial settlement and recovery. Two
/// retained calls/pump attempts cannot append again, consume correction budget,
/// change canonical context or claim Queued. The error variant exhausts the
/// existing candidate generation port, preserving the exact retained payload/read
/// and typed failure rather than queueing a provider or replaying audit.
#[tokio::test]
async fn runtime_completion_owner_failure_bridge_audits_once_and_retains_handoff_error() {
    use crate::runtime::agent::CompletionSettlementDisposition;
    for handoff_error in [false, true] {
        let mut fixture = ordinary_bridge_fixture(true).await;
        let audit_path = fixture._root.0.join("ordinary-failure-audit.jsonl");
        fixture
            .service
            .set_audit_log(crate::security::audit::AuditLog::new(
                crate::security::audit::AuditConfig {
                    enabled: true,
                    path: audit_path.clone(),
                    hash_chain: false,
                    required: true,
                },
            ));
        let execution = fixture.retained();
        assert!(
            fixture
                .service
                .present_agent_action_outcomes_to_terminal_buffer("%1", &execution)
                .is_err()
        );
        fixture
            .service
            .append_runtime_agent_execution_failure_audit(&fixture.turn, &execution)
            .unwrap();
        fixture
            .service
            .append_runtime_agent_execution_failure_audit(&fixture.turn, &execution)
            .unwrap();
        assert_eq!(fs::read_to_string(&audit_path).unwrap().lines().count(), 1);
        assert_eq!(
            fixture
                .service
                .completion_failure_audit_phase_for_tests(&fixture.turn.turn_id),
            Some("accepted")
        );
        let context_before =
            runtime_prepared_context_for_turn(&fixture.service, &fixture.turn.turn_id);
        for _ in 0..2 {
            let mut current = fixture.retained();
            assert_eq!(
                fixture
                    .service
                    .settle_agent_failure_feedback_for_correction(
                        &fixture.turn,
                        &mut current,
                        "ordinary_bridge_fixture"
                    )
                    .unwrap(),
                CompletionSettlementDisposition::Retained
            );
            assert_eq!(
                fixture.service.failure_feedback_attempt_total_for_tests(),
                0
            );
            assert_eq!(fixture.service.retry_completion_publications(), 0);
        }
        let context_after =
            runtime_prepared_context_for_turn(&fixture.service, &fixture.turn.turn_id);
        assert_eq!(
            canonical_event_oracle(&context_before),
            canonical_event_oracle(&context_after)
        );
        assert!(
            !fixture
                .service
                .agent_provider_task_is_pending(&fixture.turn.turn_id)
        );
        fixture
            .service
            .hold_agent_presentation_installs_for_tests(false);
        let expected_snapshot = if handoff_error {
            let source = fixture
                .service
                .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
                .unwrap();
            fixture
                .service
                .persistence
                .set_bookkeeping_generation_for_tests(u64::MAX);
            let mut current = fixture.retained();
            let error = fixture
                .service
                .settle_agent_failure_feedback_for_correction(
                    &fixture.turn,
                    &mut current,
                    "ordinary_bridge_handoff_error",
                )
                .unwrap_err();
            assert!(error.message().contains("generation exhausted"));
            Some(source)
        } else {
            fixture.service.retry_completion_publications();
            None
        };
        assert_eq!(fs::read_to_string(&audit_path).unwrap().lines().count(), 1);
        if handoff_error {
            let error = fixture
                .service
                .ensure_completion_admission_open()
                .unwrap_err();
            assert!(error.message().contains("generation exhausted"));
            assert!(
                !fixture
                    .service
                    .agent_provider_task_is_pending(&fixture.turn.turn_id)
            );
            let retained = fixture
                .service
                .retained_completion_execution_for_tests(&fixture.turn.turn_id)
                .unwrap();
            assert_eq!(retained.action_results, execution.action_results);
            let snapshot = fixture
                .service
                .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                snapshot,
                expected_snapshot.unwrap(),
                "failed producer must retain the exact pre-error source/read bytes"
            );
            let attempts = fixture.service.failure_feedback_attempt_total_for_tests();
            assert_eq!(fixture.service.retry_completion_publications(), 0);
            assert_eq!(
                fixture.service.failure_feedback_attempt_total_for_tests(),
                attempts
            );
        } else {
            assert!(
                fixture
                    .service
                    .agent_provider_task_is_pending(&fixture.turn.turn_id)
            );
            let attempts = fixture.service.failure_feedback_attempt_total_for_tests();
            assert!(attempts > 0);
            assert_eq!(fixture.service.retry_completion_publications(), 0);
            assert_eq!(
                fixture.service.failure_feedback_attempt_total_for_tests(),
                attempts
            );
            fixture.service.ensure_completion_admission_open().unwrap();
        }
    }
}

/// Lost acknowledgement after the unchanged audit append is explicitly
/// Entered/unproven, not healthy Accepted or paint work. Original backing and
/// staged snapshot survive, and retry cannot append or consume budget again.
#[tokio::test]
async fn runtime_completion_owner_failure_audit_lost_ack_never_replays() {
    let mut fixture = ordinary_bridge_fixture(true).await;
    let audit_path = fixture._root.0.join("ordinary-lost-audit-ack.jsonl");
    fixture
        .service
        .set_audit_log(crate::security::audit::AuditLog::new(
            crate::security::audit::AuditConfig {
                enabled: true,
                path: audit_path.clone(),
                hash_chain: false,
                required: true,
            },
        ));
    let execution = fixture.retained();
    fixture
        .service
        .reject_completion_failure_audit_ack_for_tests();
    let error = fixture
        .service
        .append_runtime_agent_execution_failure_audit(&fixture.turn, &execution)
        .unwrap_err();
    assert!(error.message().contains("after audit append"));
    assert_eq!(fs::read_to_string(&audit_path).unwrap().lines().count(), 1);
    assert_eq!(
        fixture
            .service
            .completion_failure_audit_phase_for_tests(&fixture.turn.turn_id),
        Some("entered")
    );
    assert_eq!(
        fixture
            .service
            .retained_completion_execution_for_tests(&fixture.turn.turn_id)
            .unwrap()
            .action_results,
        execution.action_results
    );
    assert!(
        fixture
            .service
            .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        fixture.service.failure_feedback_attempt_total_for_tests(),
        0
    );
    assert!(
        !fixture
            .service
            .completion_publication_retry_needed_for_turn(&fixture.turn.turn_id)
    );
    assert!(
        fixture
            .service
            .append_runtime_agent_execution_failure_audit(&fixture.turn, &execution)
            .is_err()
    );
    assert_eq!(fixture.service.retry_completion_publications(), 0);
    assert_eq!(fs::read_to_string(&audit_path).unwrap().lines().count(), 1);
}

/// An ordinary issued or publication-held action has no assessment owner and
/// cannot use the new purpose-qualified integration to skip its original
/// handoff/continuation gates. This read-only negative oracle creates no
/// assessment record/request, invokes no backend and leaves canonical custody
/// unchanged. Positive assessment/lease behavior remains a separate held gate.
#[tokio::test]
async fn runtime_completion_owner_ordinary_claim_cannot_skip_custody_gates() {
    for held in [false, true] {
        let mut fixture = CompletionFixture::fetch().await;
        if held {
            fixture
                .service
                .hold_agent_presentation_installs_for_tests(true);
            let outcome = fixture.outcome();
            assert!(
                fixture
                    .service
                    .complete_approved_external_action(outcome)
                    .unwrap()
            );
        }
        let before = fixture.retained().action_results;
        let error = fixture
            .service
            .validate_completion_claim_readiness_for_tests(&fixture.turn.turn_id)
            .unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
        assert!(
            !fixture
                .service
                .agent_provider_task_is_pending(&fixture.turn.turn_id)
        );
        assert_eq!(fixture.retained().action_results, before);
    }
}

/// Exact human resume may continue a settled failure without relabelling it or
/// reissuing its effect. Unissued siblings become Cancelled, and both those
/// facts plus new guidance must be archived at the original store before the
/// shared scheduler admits provider continuation. A live repository replacement
/// cannot receive these RAW rows. This uses synthetic external completion only.
#[tokio::test]
async fn runtime_completion_owner_paused_outcome_resumes_with_original_archive_evidence() {
    for (failed, held) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut fixture = CompletionFixture::fetch().await;
        let original = fixture
            .service
            .persistence
            .cloned_transcript_store()
            .unwrap();
        if failed && held {
            let progress = mez_agent::ActionPresentationProgress::new(
                &fixture.turn.turn_id,
                &fixture.action.id,
                mez_agent::ActionPresentationExecutionIdentity::Attempt(fixture.attempt.clone()),
                1,
                mez_agent::ActionPresentationComponentIdentity::ProvisionalReadBody,
                "provisional failed fetch bytes",
            );
            assert!(
                fixture
                    .service
                    .apply_action_presentation_progress(progress)
                    .unwrap()
            );
        }
        let target = fixture
            .service
            .capture_agent_lifecycle_target(&fixture.primary, "%1")
            .unwrap();
        let generation = fixture
            .service
            .pause_agent_lifecycle_target(&fixture.primary, &target)
            .unwrap();
        fixture
            .service
            .hold_agent_presentation_installs_for_tests(held);
        let mut outcome = fixture.outcome();
        if failed {
            outcome.result = Ok(ActionResult::failed(
                &fixture.turn,
                &fixture.action,
                ActionStatus::Failed,
                "fixture_paused_failure",
                "actual failed result remains failed",
            )
            .unwrap());
        }
        assert!(
            fixture
                .service
                .complete_approved_external_action(outcome)
                .unwrap()
        );
        fixture.service.reconcile_human_pauses().unwrap();
        assert_eq!(
            fixture.service.agent_human_pause_status("%1"),
            Some("paused")
        );
        assert!(
            !fixture
                .service
                .agent_provider_task_is_pending(&fixture.turn.turn_id)
        );
        let replacement = AgentTranscriptStore::new(fixture._root.0.join("resume-replacement"));
        fixture
            .service
            .set_agent_transcript_store(replacement.clone());
        let target = fixture
            .service
            .capture_agent_lifecycle_target(&fixture.primary, "%1")
            .unwrap();
        assert!(
            fixture
                .service
                .resume_agent_lifecycle_target(&fixture.primary, &target, generation)
                .unwrap()
        );
        if held {
            assert!(
                !fixture
                    .service
                    .agent_provider_task_is_pending(&fixture.turn.turn_id)
            );
            assert!(
                fixture
                    .service
                    .completion_publication_retry_needed_for_turn(&fixture.turn.turn_id)
            );
            assert_eq!(fixture.service.retry_completion_publications(), 0);
            assert_eq!(fixture.service.retry_completion_publications(), 0);
            fixture
                .service
                .hold_agent_presentation_installs_for_tests(false);
            assert_eq!(fixture.service.retry_completion_publications(), 1);
            assert_eq!(fixture.service.retry_completion_publications(), 0);
        }
        assert!(
            fixture
                .service
                .agent_provider_task_is_pending(&fixture.turn.turn_id)
        );
        let execution = fixture.retained();
        assert_eq!(
            execution.action_results[0].status,
            if failed {
                ActionStatus::Failed
            } else {
                ActionStatus::Succeeded
            }
        );
        assert_eq!(execution.action_results[1].status, ActionStatus::Cancelled);
        let rows = original.inspect(&fixture.turn.conversation_id).unwrap();
        assert_eq!(
            rows.iter()
                .filter(|row| row.content.contains("action result pending-sibling"))
                .count(),
            1
        );
        assert!(rows.iter().any(|row| {
            row.content
                .contains("Continue your retained task from its current state")
        }));
        if failed {
            assert!(
                rows.iter()
                    .any(|row| row.content.contains("actual failed result remains failed"))
            );
        }
        assert!(
            !replacement
                .transcript_path(&fixture.turn.conversation_id)
                .unwrap()
                .exists()
        );
        let agent = AgentId::opaque(fixture.turn.agent_id.clone()).unwrap();
        assert!(
            fixture
                .service
                .ready_agent_provider_turn(&agent, &fixture.turn.turn_id)
                .unwrap()
                .is_some()
        );
        assert!(
            !fixture
                .service
                .admit_completion_provider_continuation(&fixture.turn.turn_id)
                .unwrap(),
            "readiness and repeat admission cannot insert another continuation"
        );
        fixture.service.ensure_completion_admission_open().unwrap();
    }
}

/// Original body, entry, occurrence, transaction and rendering flavor survive
/// repeated preinstall errors in the SAME RESULT queue. This is a presentation
/// ownership oracle for native activity/shell-view source shapes, not evidence
/// that a native command or capability probe was executed or authorized.
#[tokio::test]
async fn runtime_completion_owner_result_source_keeps_native_transaction_and_raw_mode() {
    for raw_diagnostic in [false, true] {
        let mut fixture = CompletionFixture::fetch().await;
        let execution = fixture.retained();
        let source = "F668_ORIGINAL_NATIVE_BODY\n";
        fixture.service.fail_agent_presentation_install_for_tests(0);
        assert!(
            fixture
                .service
                .queue_ordered_provider_result_source(
                    "%1",
                    &execution,
                    &fixture.action,
                    &fixture.expected,
                    (source, Some(&fixture.attempt), raw_diagnostic)
                )
                .is_err()
        );
        let accepted = fixture
            .service
            .presentation
            .agent_queued_provider_results
            .values()
            .next()
            .unwrap()
            .clone();
        assert_eq!(accepted.2, source);
        assert_eq!(accepted.4.as_deref(), Some(fixture.attempt.as_str()));
        assert_eq!(accepted.5, raw_diagnostic);
        fixture.service.fail_agent_presentation_install_for_tests(0);
        assert!(
            fixture
                .service
                .queue_ordered_provider_result_source(
                    "%1",
                    &execution,
                    &fixture.action,
                    &fixture.expected,
                    (source, Some(&fixture.attempt), raw_diagnostic)
                )
                .is_err()
        );
        let retained = fixture
            .service
            .presentation
            .agent_queued_provider_results
            .values()
            .next()
            .unwrap();
        assert_eq!(retained.3.id, accepted.3.id);
        assert_eq!(retained.3.occurrence, accepted.3.occurrence);
        assert_eq!(retained.4, accepted.4);
        assert_eq!(retained.5, accepted.5);
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
            rows.matches("F668_ORIGINAL_NATIVE_BODY").count(),
            1,
            "{rows}"
        );
        assert_eq!(
            fixture.retained().action_results[0].status,
            ActionStatus::Running,
            "presentation acceptance cannot manufacture canonical completion"
        );
    }
}

/// A terminal payload naming a different agent is not an authorized result for
/// this exact claimed action. Preserve it only in the original failed receipt,
/// keep canonical context untouched, and report the same typed failure on an
/// exact duplicate rather than adopting or resubmitting it.
#[tokio::test]
async fn runtime_completion_owner_rejects_foreign_result_agent_without_losing_raw() {
    let mut fixture = CompletionFixture::fetch().await;
    let mut foreign = fixture.expected.clone();
    foreign.agent_id = "agent-foreign-fixture".to_string();
    let mut outcome = fixture.outcome();
    outcome.result = Ok(foreign.clone());
    let first = fixture
        .service
        .complete_approved_external_action(outcome)
        .unwrap_err();
    assert_eq!(first.kind(), crate::error::MezErrorKind::InvalidState);
    assert_eq!(
        fixture.retained().action_results[0].status,
        ActionStatus::Running
    );
    assert_eq!(
        fixture.service.retained_external_result_for_tests(
            &fixture.turn.turn_id,
            &fixture.action.id,
            &fixture.attempt,
        ),
        Some(foreign.clone())
    );
    let context = runtime_prepared_context_for_turn(&fixture.service, &fixture.turn.turn_id);
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.source == ContextSourceKind::ActionResult
                && block.content.contains("F668_EXACT_COMPLETED_BODY"))
    );
    let mut duplicate = fixture.outcome();
    duplicate.result = Ok(foreign);
    let repeated = fixture
        .service
        .complete_approved_external_action(duplicate)
        .unwrap_err();
    assert_eq!(repeated.kind(), first.kind());
    assert_eq!(repeated.message(), first.message());
}

/// A real shell-store rebind can precede higher-level ledger reconciliation.
/// The legacy Running-result predicate is not enough: old exact attempt output
/// must be rejected before capture, its matching claim retired, and replacement
/// conversation/presentation left untouched. This boundary test intentionally
/// keeps the original ledger Running; it does not simulate a native process.
#[tokio::test]
async fn runtime_completion_owner_rebind_rejects_old_attempt_and_duplicate() {
    let mut fixture = CompletionFixture::fetch().await;
    fixture
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", "replacement-completion-fixture", 0)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&fixture.turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    let outcome = fixture.outcome();
    assert!(
        !fixture
            .service
            .complete_approved_external_action(outcome)
            .unwrap()
    );
    assert!(
        !fixture.service.approved_external_action_attempt_is_current(
            &fixture.turn.turn_id,
            &fixture.action.id,
            &fixture.attempt,
        )
    );
    let duplicate = fixture.outcome();
    assert!(
        !fixture
            .service
            .complete_approved_external_action(duplicate)
            .unwrap()
    );
    assert_eq!(
        fixture
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id,
        "replacement-completion-fixture"
    );
    assert_eq!(
        fixture.retained().action_results[0].status,
        ActionStatus::Running
    );
    assert!(
        fixture
            .service
            .retained_external_result_for_tests(
                &fixture.turn.turn_id,
                &fixture.action.id,
                &fixture.attempt,
            )
            .is_none()
    );
    let rows = fixture
        .service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!rows.contains("F668_EXACT_COMPLETED_BODY"), "{rows}");
}

/// Owns the generated directory even if fixture construction fails or panics.
struct CompletionRoot(PathBuf);

impl Drop for CompletionRoot {
    /// Removes only the unique root created by the existing test helper.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Isolated actor state with one accepted claimed fetch and a pending sibling.
/// Keeping the sibling running leaves exact group-owned result/context evidence
/// inspectable without confusing terminal cleanup with result acceptance.
struct CompletionFixture {
    service: RuntimeSessionService,
    primary: mez_core::ids::ClientId,
    _root: CompletionRoot,
    turn: AgentTurnRecord,
    action: AgentAction,
    attempt: String,
    cancellation: crate::runtime::processes::native_cancellation::NativeActionCancellation,
    group: String,
    expected: ActionResult,
}

impl CompletionFixture {
    /// Admits a real turn and trusted running-worker execution through the
    /// production actor API, then claims its exact generated attempt. No role,
    /// permission, sandbox or admission bypass is enabled and no network runs.
    async fn fetch() -> Self {
        Self::fetch_with_final_turn(true).await
    }

    /// Chooses terminal versus provider-continuation semantics before the
    /// trusted provider fixture is admitted; never rewrites a settled receipt.
    async fn fetch_with_final_turn(final_turn: bool) -> Self {
        Self::fetch_with_save_policy(final_turn, true).await
    }

    /// Captures original repository policy before the actual worker claim.
    async fn fetch_with_save_policy(final_turn: bool, attach_store: bool) -> Self {
        let root = CompletionRoot(temp_root("completed-result-owner"));
        fs::create_dir(root.0.join("transcripts")).unwrap();
        let mut service = RuntimeServiceFixture::new()
            .control_socket(root.0.join("control.sock"))
            .build();
        if attach_store {
            service
                .set_agent_transcript_store(AgentTranscriptStore::new(root.0.join("transcripts")));
        }
        let primary = service
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
            final_turn,
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
        // Approved execution text is produced by actual claim preparation,
        // not by receiving the initial Running worker execution. Claim before
        // asserting the header; never manufacture publication flags.
        let dispatch = service
            .claim_approved_external_action(&turn.turn_id, &action.id)
            .unwrap()
            .expect("accepted worker attempt");
        assert!(service.approved_external_action_attempt_is_current(
            &turn.turn_id,
            &action.id,
            &dispatch.attempt,
        ));
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
            primary,
            _root: root,
            turn,
            action,
            attempt: dispatch.attempt,
            cancellation: dispatch.cancellation,
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
    }
}

/// Exact cancellation tokens belong to admitted dispatches, not terminal paint.
/// Retain the actually claimed external token, install a compositor-owned transient
/// preview solely as a presentation failure cut, then fail its actual retirement
/// while transaction cancellation continues. The issued token must already
/// be cancelled while claims/results remain available for truthful classification.
/// The pending sibling stays unclaimed under normal per-turn serialization.
/// The preview is synthetic UI state, not native worker or invocation evidence;
/// this fixture never executes HTTP, MCP, a provider, or a native command.
#[tokio::test]
async fn runtime_completion_owner_cancels_issued_token_before_preview_retirement_error() {
    use std::sync::atomic::Ordering;
    let mut fixture = CompletionFixture::fetch().await;
    // The shared fetch fixture needs Verbose to establish its real header.
    // Transient previews intentionally do not exist in shell-view/Verbose mode;
    // choose the ordinary preview-eligible display mode before this UI-only cut.
    assert!(fixture.service.agent_shell_view_enabled("%1"));
    fixture
        .service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Normal)
        .unwrap();
    assert!(!fixture.service.agent_shell_view_enabled("%1"));
    assert!(
        fixture
            .service
            .claim_approved_external_action(&fixture.turn.turn_id, "pending-sibling",)
            .unwrap()
            .is_none(),
        "same-turn external claims remain serialized"
    );
    let first_flag = fixture.cancellation.flag();
    assert!(!first_flag.load(Ordering::SeqCst));
    let before = fixture.retained().action_results;
    let context_before = canonical_event_oracle(&runtime_prepared_context_for_turn(
        &fixture.service,
        &fixture.turn.turn_id,
    ));
    fixture
        .service
        .update_agent_shell_output_preview(
            "%1",
            crate::runtime::render::RuntimeAgentShellPreviewOwner {
                turn_id: fixture.turn.turn_id.clone(),
                action_id: fixture.action.id.clone(),
                marker: fixture.attempt.clone(),
            },
            1,
            &["presentation-only cancellation fault cut".to_string()],
        )
        .unwrap();
    let previews = fixture.service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(previews.len(), 1);
    assert_eq!(previews[0].0.turn_id, fixture.turn.turn_id);
    assert_eq!(previews[0].0.action_id, fixture.action.id);
    assert_eq!(previews[0].0.marker, fixture.attempt);
    assert!(!first_flag.load(Ordering::SeqCst));
    fixture.service.fail_agent_presentation_install_for_tests(0);
    let cancelled = fixture
        .service
        .cancel_live_shell_transactions_for_turn(&fixture.turn.turn_id)
        .unwrap();
    assert_eq!(
        cancelled, 0,
        "this external-only fixture owns no shell transaction"
    );
    assert!(first_flag.load(Ordering::SeqCst));
    assert!(fixture.service.approved_external_action_attempt_is_current(
        &fixture.turn.turn_id,
        &fixture.action.id,
        &fixture.attempt,
    ));
    assert!(
        fixture
            .service
            .pending_approved_external_actions()
            .contains(&(fixture.turn.turn_id.clone(), "pending-sibling".to_string()),)
    );
    assert_eq!(fixture.retained().action_results, before);
    assert_eq!(
        canonical_event_oracle(&runtime_prepared_context_for_turn(
            &fixture.service,
            &fixture.turn.turn_id,
        )),
        context_before
    );
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

/// Attaching a repository after the admitted group captured runtime-only policy
/// cannot create a legacy mutable-target candidate or RAW append. Completion
/// still settles canonical outcomes and terminal state; presentation durability
/// is deliberately not used as the RAW archive oracle.
#[tokio::test]
async fn runtime_completion_owner_late_repository_does_not_override_original_no_save() {
    let mut fixture = CompletionFixture::fetch_with_save_policy(true, false).await;
    let store = AgentTranscriptStore::new(fixture._root.0.join("late-repository"));
    fixture.service.set_agent_transcript_store(store.clone());
    let execution = fixture.retained();
    assert_eq!(
        fixture
            .service
            .persist_runtime_agent_turn_execution_transcript(&fixture.turn, &execution)
            .unwrap(),
        0
    );
    assert!(fixture.service.claim_bookkeeping_candidates().is_empty());
    let outcome = fixture.outcome();
    assert!(
        fixture
            .service
            .complete_approved_external_action(outcome)
            .unwrap()
    );
    let sibling = fixture.retained().response.action_batch.unwrap().actions[1].clone();
    let dispatch = fixture
        .service
        .claim_approved_external_action(&fixture.turn.turn_id, &sibling.id)
        .unwrap()
        .unwrap();
    let result = ActionResult::succeeded(
        &fixture.turn,
        &sibling,
        vec!["no-save sibling".to_string()],
        None,
    );
    assert!(
        fixture
            .service
            .complete_approved_external_action(
                crate::runtime::RuntimeApprovedExternalActionOutcome {
                    turn_id: fixture.turn.turn_id.clone(),
                    action_id: sibling.id,
                    attempt: dispatch.attempt,
                    result: Ok(result),
                    mcp_transport: None,
                }
            )
            .unwrap()
    );
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&fixture.turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert!(fixture.service.claim_bookkeeping_candidates().is_empty());
    let raw_path = store
        .transcript_path(&fixture.turn.conversation_id)
        .unwrap();
    assert!(
        !raw_path.exists(),
        "no-save policy must not manufacture an empty RAW archive"
    );
    assert!(
        fixture
            .service
            .persistence
            .pending_transcript_entries_at_path(&raw_path, &fixture.turn.conversation_id)
            .unwrap()
            .is_empty()
    );
    fixture.service.ensure_completion_admission_open().unwrap();
}

/// The original archive target, not the repository attached at callback time,
/// owns exactly one frozen candidate. Worker acceptance must queue its RAW rows
/// only at that original path; legacy count-returning producers cannot add a
/// parallel mutable candidate or send bytes to the replacement repository.
#[tokio::test]
async fn runtime_completion_owner_repository_replacement_has_one_original_candidate() {
    let mut fixture = CompletionFixture::fetch().await;
    let original = fixture
        .service
        .persistence
        .cloned_transcript_store()
        .unwrap();
    let replacement = AgentTranscriptStore::new(fixture._root.0.join("replacement-repository"));
    fixture.service.persistence.enable_transcript_adapter();
    fixture
        .service
        .set_agent_transcript_store(replacement.clone());
    let outcome = fixture.outcome();
    assert!(
        fixture
            .service
            .complete_approved_external_action(outcome)
            .unwrap()
    );
    let sibling = fixture.retained().response.action_batch.unwrap().actions[1].clone();
    let dispatch = fixture
        .service
        .claim_approved_external_action(&fixture.turn.turn_id, &sibling.id)
        .unwrap()
        .unwrap();
    let result = ActionResult::succeeded(
        &fixture.turn,
        &sibling,
        vec!["original-store sibling".to_string()],
        None,
    );
    assert!(
        fixture
            .service
            .complete_approved_external_action(
                crate::runtime::RuntimeApprovedExternalActionOutcome {
                    turn_id: fixture.turn.turn_id.clone(),
                    action_id: sibling.id,
                    attempt: dispatch.attempt,
                    result: Ok(result),
                    mcp_transport: None,
                }
            )
            .unwrap()
    );
    let mut work = fixture.service.claim_bookkeeping_candidates();
    assert_eq!(
        work.len(),
        1,
        "the group must not also use a legacy candidate"
    );
    let work = work.pop().unwrap();
    assert_eq!(work.read, work.read.clone().freeze_completion());
    let checked = work.execute();
    assert!(
        checked.is_ok(),
        "original checked history must be readable: {checked:?}"
    );
    assert!(
        fixture
            .service
            .complete_bookkeeping_candidate(work, checked)
            .unwrap()
    );
    let original_path = original
        .transcript_path(&fixture.turn.conversation_id)
        .unwrap();
    let replacement_path = replacement
        .transcript_path(&fixture.turn.conversation_id)
        .unwrap();
    assert!(
        !fixture
            .service
            .persistence
            .pending_transcript_entries_at_path(&original_path, &fixture.turn.conversation_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .service
            .persistence
            .pending_transcript_entries_at_path(&replacement_path, &fixture.turn.conversation_id)
            .unwrap()
            .is_empty()
    );
}

/// A held first result and a successful serialized sibling cannot bypass group
/// publication or strand the provider-claim milestone. The ordinary pump must
/// acknowledge retained presentation, then either finalize once or permit the
/// actual next-provider claim. Outcomes are synthetic; neither claim executes
/// network/provider work, and no completed action is dispatched a second time.
#[tokio::test]
async fn runtime_completion_owner_recovers_held_sibling_before_group_continuation() {
    for final_turn in [false, true] {
        let mut fixture = CompletionFixture::fetch_with_final_turn(final_turn).await;
        fixture.service.fail_agent_presentation_install_for_tests(0);
        let outcome = fixture.outcome();
        assert!(
            fixture
                .service
                .complete_approved_external_action(outcome)
                .unwrap()
        );
        assert!(
            !fixture
                .service
                .completion_publication_retry_needed_for_turn(&fixture.turn.turn_id),
            "the real pending sibling, not paint, owns progress"
        );
        let sibling = fixture.retained().response.action_batch.unwrap().actions[1].clone();
        let dispatch = fixture
            .service
            .claim_approved_external_action(&fixture.turn.turn_id, &sibling.id)
            .unwrap()
            .expect("the exact serialized sibling is now claimable");
        let sibling_result = ActionResult::succeeded(
            &fixture.turn,
            &sibling,
            vec!["F668_SIBLING_COMPLETED_BODY".to_string()],
            None,
        );
        assert!(
            fixture
                .service
                .complete_approved_external_action(
                    crate::runtime::RuntimeApprovedExternalActionOutcome {
                        turn_id: fixture.turn.turn_id.clone(),
                        action_id: sibling.id.clone(),
                        attempt: dispatch.attempt.clone(),
                        result: Ok(sibling_result),
                        mcp_transport: None,
                    }
                )
                .unwrap()
        );
        assert!(
            !fixture
                .service
                .agent_provider_task_is_pending(&fixture.turn.turn_id)
        );
        assert!(
            fixture
                .service
                .completion_publication_retry_needed_for_turn(&fixture.turn.turn_id)
        );
        assert_eq!(fixture.service.retry_completion_publications(), 1);
        assert_eq!(fixture.service.retry_completion_publications(), 0);
        assert!(
            !fixture
                .service
                .completion_publication_retry_needed_for_turn(&fixture.turn.turn_id)
        );
        if final_turn {
            assert_eq!(
                fixture
                    .service
                    .agent_turn_ledger()
                    .turn(&fixture.turn.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Completed
            );
        } else {
            assert!(
                fixture
                    .service
                    .agent_provider_task_is_pending(&fixture.turn.turn_id)
            );
            let agent_id = AgentId::opaque(fixture.turn.agent_id.clone()).unwrap();
            assert!(
                fixture
                    .service
                    .ready_agent_provider_turn(&agent_id, &fixture.turn.turn_id)
                    .unwrap()
                    .is_some(),
                "publication must permit provider readiness without consuming a claim"
            );
        }
        assert!(
            !fixture
                .service
                .complete_approved_external_action(fixture.outcome())
                .unwrap()
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
        assert_eq!(
            rows.matches("F668_SIBLING_COMPLETED_BODY").count(),
            1,
            "{rows}"
        );
        fixture.service.ensure_completion_admission_open().unwrap();
    }
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
