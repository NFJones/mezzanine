//! Original positive Held -> runtime scheduler wait -> same-claim reacquisition.
//! These port tests do not activate host waiting or claim physical HTTP proof.

use super::*;
use crate::runtime::agent::token_budget::admission_work::RuntimeBudgetHeldAttempt;
use crate::runtime::{RuntimeBudgetAttemptAdmission, RuntimeBudgetAttemptWork, RuntimeSideEffect};

/// Human inhibition is independent of a positively nonwriting provider hold.
/// Exercise the actual authorized lifecycle operation, not scheduler flag edits.
#[tokio::test]
async fn runtime_budget_provider_wait_genuine_human_resume_preserves_original_claim() {
    for stage in 0..3 {
        let (mut scene, dispatch, work, capture) = held().await;
        let turn = &dispatch.turn;
        let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
        let primary = scene
            .service
            .attach_primary("budget-human", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        scene
            .service
            .session
            .set_pane_live_state(&turn.pane_id, false)
            .unwrap();
        let target = scene
            .service
            .capture_agent_lifecycle_target(&primary, &turn.pane_id)
            .unwrap();
        let now = crate::runtime::current_unix_millis();
        let deadline = scene
            .service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .deadline_at_unix_millis;
        let early_pause = if stage == 0 {
            Some(
                scene
                    .service
                    .pause_agent_lifecycle_target(&primary, &target)
                    .unwrap(),
            )
        } else {
            None
        };
        scene
            .service
            .park_budget_provider_claim(&agent, &turn.turn_id, 7, capture.clone(), now)
            .unwrap();
        if stage == 2 {
            scene
                .service
                .requeue_budget_provider_claim(&agent, &turn.turn_id, 7, &capture)
                .unwrap();
        }
        let generation = early_pause.unwrap_or_else(|| {
            scene
                .service
                .pause_agent_lifecycle_target(&primary, &target)
                .unwrap()
        });
        assert_eq!(
            scene.service.agent_human_pause_status(&turn.pane_id),
            Some("paused"),
            "positive hold is not an issued inference: stage {stage}"
        );
        assert!(scene.service.agent_turn_is_running(&turn.turn_id));
        // Positive budget quiescence does not settle independent persistence.
        scene
            .service
            .mark_agent_provider_persistence_pending(&turn.turn_id);
        assert_eq!(
            scene.service.agent_human_pause_status(&turn.pane_id),
            Some("pausing")
        );
        assert!(
            scene
                .service
                .resume_agent_lifecycle_target(&primary, &target, generation)
                .is_err()
        );
        scene
            .service
            .clear_agent_provider_persistence_pending(&turn.turn_id);
        assert!(
            !scene
                .service
                .budget_provider_claim_can_resume(&turn.turn_id, 7)
        );
        assert!(
            scene
                .service
                .resume_budget_provider_claim(&agent, &turn.turn_id, 7, &capture, now + 2000)
                .is_err()
        );
        assert!(
            scene
                .service
                .resume_agent_lifecycle_target(&primary, &target, generation + 1)
                .is_err()
        );
        assert!(
            scene
                .service
                .resume_agent_lifecycle_target(&primary, &target, generation)
                .unwrap()
        );
        // Resume clears HUMAN inhibition only. The held claim/future and its
        // budget wait remain owned; no provider continuation is synthesized.
        assert!(
            scene
                .service
                .budget_provider_claim_is_parked(&turn.turn_id, 7)
        );
        assert!(
            scene
                .service
                .agent_provider_claim_matches(&agent, &turn.turn_id, 7)
        );
        assert!(!scene.service.agent_provider_task_is_pending(&turn.turn_id));
        assert!(capture.matches_owner(dispatch.budget_owner.as_ref().unwrap()));
        scene
            .service
            .requeue_budget_provider_claim(&agent, &turn.turn_id, 7, &capture)
            .unwrap();
        scene
            .service
            .resume_budget_provider_claim(&agent, &turn.turn_id, 7, &capture, now + 5000)
            .unwrap();
        if deadline != 0 {
            assert_eq!(
                scene
                    .service
                    .agent_turn_ledger()
                    .turn(&turn.turn_id)
                    .unwrap()
                    .deadline_at_unix_millis,
                deadline + 5000
            );
        }
        // Once active again, a mere retained entry is not nonwriting proof.
        let generation = scene
            .service
            .pause_agent_lifecycle_target(&primary, &target)
            .unwrap();
        assert_eq!(
            scene.service.agent_human_pause_status(&turn.pane_id),
            Some("pausing")
        );
        assert!(
            scene
                .service
                .resume_agent_lifecycle_target(&primary, &target, generation)
                .is_err()
        );
        assert!(matches!(
            work.admission_owner().retry_held().await.unwrap(),
            RuntimeBudgetAttemptAdmission::Held(_)
        ));
    }
}

/// A revoked positive hold is no longer proof that the original native work is
/// nonwriting. Human Resume must remain inhibited rather than borrowing it.
#[tokio::test]
async fn runtime_budget_provider_wait_human_resume_refuses_revoked_hold() {
    let (mut scene, dispatch, work, capture) = held().await;
    let turn = &dispatch.turn;
    let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let primary = scene
        .service
        .attach_primary("budget-revoked", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    scene
        .service
        .session
        .set_pane_live_state(&turn.pane_id, false)
        .unwrap();
    let target = scene
        .service
        .capture_agent_lifecycle_target(&primary, &turn.pane_id)
        .unwrap();
    scene
        .service
        .park_budget_provider_claim(
            &agent,
            &turn.turn_id,
            7,
            capture,
            crate::runtime::current_unix_millis(),
        )
        .unwrap();
    let generation = scene
        .service
        .pause_agent_lifecycle_target(&primary, &target)
        .unwrap();
    assert!(
        scene
            .service
            .budget_provider_claim_is_quiescent(&turn.turn_id)
    );
    assert!(matches!(
        work.admission_owner().retry_held().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    assert!(
        !scene
            .service
            .budget_provider_claim_is_quiescent(&turn.turn_id)
    );
    assert_eq!(
        scene.service.agent_human_pause_status(&turn.pane_id),
        Some("pausing")
    );
    assert!(
        scene
            .service
            .resume_agent_lifecycle_target(&primary, &target, generation)
            .is_err()
    );
    assert!(scene.service.agent_turn_is_human_paused(&turn.turn_id));
    assert!(
        scene
            .service
            .agent_provider_claim_matches(&agent, &turn.turn_id, 7)
    );
    assert!(!scene.service.agent_provider_task_is_pending(&turn.turn_id));
}

/// Public host requests must adopt their timer effects before replying. An old
/// callback after the cancellation cannot fail the still-retained producer;
/// duplicate reacquisition replies cannot create another timer or inference.
#[tokio::test]
async fn runtime_budget_provider_wait_host_adopts_original_timer_edges() {
    use crate::runtime::{
        RuntimeEvent, RuntimeEventBatch, RuntimeTimerKey, RuntimeTimerKind, TimerEvent,
    };
    let (mut scene, dispatch, _work, capture) = held().await;
    let turn = &dispatch.turn;
    let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderClaim, &turn.turn_id, 7);
    let service = std::mem::replace(&mut scene.service, test_runtime_service());
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                key: key.clone(),
                delay_ms: 30_000,
            }])
            .await
            .unwrap();
        let _ = handle.drain_timer_side_effects(64).await.unwrap();
        assert!(handle.park_budget_provider_claim(
            agent.clone(), turn.turn_id.clone(), 8, capture.clone(),
        ).await.is_err());
        // Polling submits the typed request, then dropping its borrowed waiter
        // discards the reply, not actor mutation. Same-lane observation below
        // orders the retry after that original accepted request.
        let mut lost_reply = Box::pin(handle.park_budget_provider_claim(
            agent.clone(),
            turn.turn_id.clone(),
            7,
            capture.clone(),
        ));
        assert!(matches!(
            futures_util::poll!(&mut lost_reply),
            std::task::Poll::Pending
        ));
        drop(lost_reply);
        assert!(handle.agent_turn_is_running(&turn.turn_id).await.unwrap());
        let canceled = handle.drain_timer_side_effects(64).await.unwrap();
        assert_eq!(
            canceled
                .iter()
                .filter(|effect| matches!(effect,
            RuntimeSideEffect::CancelTimer { key: actual } if actual == &key))
                .count(),
            1
        );
        assert!(handle.park_budget_provider_claim(
            agent.clone(), turn.turn_id.clone(), 7, capture.clone(),
        ).await.unwrap());
        assert!(
            handle
                .drain_timer_side_effects(64)
                .await
                .unwrap()
                .is_empty()
        );
        let mut stale = RuntimeEventBatch::new();
        stale.push(RuntimeEvent::Timer(TimerEvent {
            key: key.clone(),
            now_ms: crate::runtime::current_unix_millis() + 60_000,
        }));
        handle.submit_runtime_events(stale).await.unwrap();
        assert!(handle.agent_turn_is_running(&turn.turn_id).await.unwrap());
        assert!(
            handle
                .reacquire_budget_provider_claim(
                    agent.clone(),
                    turn.turn_id.clone(),
                    7,
                    capture.clone()
                )
                .await
                .unwrap()
        );
        let resumed = handle.drain_timer_side_effects(64).await.unwrap();
        assert_eq!(resumed.iter().filter(|effect| matches!(effect,
            RuntimeSideEffect::ScheduleTimer { key: actual, delay_ms } if actual == &key && *delay_ms > 0
        )).count(), 1);
        assert!(
            handle
                .reacquire_budget_provider_claim(agent, turn.turn_id.clone(), 7, capture)
                .await
                .unwrap()
        );
        let duplicate = handle.drain_timer_side_effects(64).await.unwrap();
        assert!(!duplicate.iter().any(|effect| matches!(effect,
            RuntimeSideEffect::ScheduleTimer { key: actual, .. } if actual == &key)));
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(client, actor.run());
    scene.service = exit.service;
}

/// Both queue pressure and a sibling's post-admission startup failure must keep
/// the original claim and mandatory cancellation. Recovery uses the SAME host
/// request/capture, not a replacement inference or a new scheduler claim.
#[tokio::test]
async fn runtime_budget_provider_wait_host_retains_pressure_and_sibling_failure() {
    for inject_failure in [false, true] {
        let (mut scene, dispatch, _work, capture) = held().await;
        let turn = &dispatch.turn;
        let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
        let primary = scene
            .service
            .attach_primary(
                "budget-host-pressure",
                true,
                Size::new(80, 24).unwrap(),
                120,
            )
            .unwrap();
        let first = scene
            .service
            .session
            .split_active_pane(&primary, SplitDirection::Vertical)
            .unwrap();
        let second = scene
            .service
            .session
            .split_active_pane(&primary, SplitDirection::Horizontal)
            .unwrap();
        for pane in [&first, &second] {
            scene
                .service
                .agent_shell_store_mut()
                .enter_or_resume(pane.as_str())
                .unwrap();
            mark_test_pane_ready(&mut scene.service, pane.as_str());
        }
        scene
            .service
            .agent_scheduler_mut()
            .set_queue_limits(1, usize::MAX)
            .unwrap();
        let sibling = scene
            .service
            .start_agent_prompt_turn(first.as_str(), "first sibling")
            .unwrap();
        if inject_failure {
            scene
                .service
                .fail_next_scheduler_start_post_admission_for_tests();
        }
        let service = std::mem::replace(&mut scene.service, test_runtime_service());
        let (handle, actor) =
            crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
                .unwrap();
        let client = async {
            assert!(
                handle
                    .park_budget_provider_claim(
                        agent.clone(),
                        turn.turn_id.clone(),
                        7,
                        capture.clone()
                    )
                    .await
                    .unwrap()
            );
            let effects = handle.drain_timer_side_effects(64).await.unwrap();
            assert_eq!(effects.iter().filter(|effect| matches!(effect,
                RuntimeSideEffect::CancelTimer { key } if key.owner_id == turn.turn_id && key.generation == 7
            )).count(), 1);
            assert!(handle.agent_turn_is_running(&turn.turn_id).await.unwrap());
            if !inject_failure {
                assert!(
                    handle
                        .agent_turn_is_running(&sibling.turn_id)
                        .await
                        .unwrap()
                );
                handle
                    .execute_agent_shell_command(primary.clone(), "queued sibling".into())
                    .await
                    .unwrap();
                // This is the connected host result consumed by the paced
                // admission loop: false, not a terminal QueueFull error.
                assert!(
                    !handle
                        .reacquire_budget_provider_claim(
                            agent.clone(),
                            turn.turn_id.clone(),
                            7,
                            capture.clone()
                        )
                        .await
                        .unwrap()
                );
                assert!(handle.agent_turn_is_running(&turn.turn_id).await.unwrap());
                handle
                    .execute_agent_shell_command(primary.clone(), "/stop".into())
                    .await
                    .unwrap();
                handle
                    .execute_terminal_command(primary.clone(), format!("select-pane -t {first}"))
                    .await
                    .unwrap();
                handle
                    .execute_agent_shell_command(primary.clone(), "/stop".into())
                    .await
                    .unwrap();
            } else {
                assert!(
                    !handle
                        .agent_turn_is_running(&sibling.turn_id)
                        .await
                        .unwrap()
                );
            }
            assert!(
                handle
                    .reacquire_budget_provider_claim(agent, turn.turn_id.clone(), 7, capture)
                    .await
                    .unwrap()
            );
            assert!(handle.agent_turn_is_running(&turn.turn_id).await.unwrap());
            assert!(
                handle
                    .pending_agent_provider_tasks()
                    .await
                    .unwrap()
                    .iter()
                    .all(|task| task.turn_id != turn.turn_id)
            );
            handle.shutdown().await.unwrap();
        };
        let ((), exit) = tokio::join!(client, actor.run());
        scene.service = exit.service;
    }
}

async fn held() -> (
    Scene,
    crate::runtime::RuntimeAgentProviderDispatch,
    RuntimeBudgetAttemptWork,
    RuntimeBudgetHeldAttempt,
) {
    let mut scene = Scene::new();
    scene
        .service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let dispatch = issued(&mut scene).await;
    scene
        .service
        .record_claimed_agent_provider_task(&dispatch, 7, 30_000)
        .unwrap();
    let work = dispatch
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            ModelTokenUsageKey::new("openai", "gpt-test"),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap();
    // Bind exactly the same content-free subject the real observer plans before
    // awaiting admission. Conversation membership alone cannot identify a claim.
    work.plan_wire_receipt_subject(
        &crate::integrations::agent::provider::ProviderWireRequestObservation {
            request_id: "original-budget-wait".into(),
            claim_generation: Some(7),
            attempt_index: 1,
            retry_reason: None,
            conversation_id: dispatch.turn.conversation_id.clone(),
            turn_id: dispatch.turn.turn_id.clone(),
            agent_id: dispatch.turn.agent_id.clone(),
            pane_id: dispatch.turn.pane_id.clone(),
            provider: "openai".into(),
            cache_namespace: "fixture".into(),
            model: "gpt-test".into(),
            prompt_cache_lineage_id: None,
            interaction_kind: "execution".into(),
            allowed_actions: String::new(),
            schema_digest: String::new(),
            max_output_tokens: None,
            output_limit_retry_override_tokens: None,
            continuity_warning: None,
            purpose: crate::integrations::agent::provider::ProviderRequestPurpose::Execution,
            message_count: 0,
            message_bytes: 0,
            mcp_directory_bytes: 0,
            mcp_search_result_bytes: 0,
            mcp_retrieved_contract_bytes: 0,
            mcp_action_result_bytes: 0,
            action_result_bytes: 0,
            final_wire_diagnostics: None,
            response_diagnostics: None,
            openai_diagnostics: None,
            diagnostics_failed: false,
            usage: None,
            usage_presence: None,
            succeeded: false,
            failure_kind: None,
            elapsed_ms: None,
        },
    )
    .unwrap();
    let admission = work.admission_owner();
    assert!(admission.capture_held().await.is_err());
    assert!(matches!(
        admission.claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    let capture = admission.capture_held().await.unwrap();
    (scene, dispatch, work, capture)
}

#[tokio::test]
async fn runtime_budget_provider_wait_reacquires_same_claim_without_continuation() {
    let (mut scene, dispatch, work, capture) = held().await;
    let turn = &dispatch.turn;
    let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let now = crate::runtime::current_unix_millis();
    let deadline = scene
        .service
        .agent_turn_ledger()
        .turn(&turn.turn_id)
        .unwrap()
        .deadline_at_unix_millis;
    assert!(capture.matches_owner(dispatch.budget_owner.as_ref().unwrap()));
    let transition = scene
        .service
        .park_budget_provider_claim(&agent, &turn.turn_id, 7, capture.clone(), now)
        .unwrap();
    assert!(transition.applied);
    assert!(matches!(
        transition.side_effects.as_slice(),
        [RuntimeSideEffect::CancelTimer { .. }]
    ));
    assert_eq!(
        scene
            .service
            .agent_scheduler()
            .snapshot()
            .active_capacity_used,
        0
    );
    assert_eq!(scene.service.agent_scheduler().snapshot().waiting, 1);
    assert!(scene.service.agent_turn_is_running(&turn.turn_id));
    assert!(
        scene
            .service
            .agent_provider_claim_matches(&agent, &turn.turn_id, 7)
    );
    assert!(!scene.service.agent_provider_task_is_pending(&turn.turn_id));
    assert!(
        !scene
            .service
            .fail_expired_claimed_agent_provider_task(&turn.turn_id, 7)
            .unwrap()
    );
    assert!(
        !scene
            .service
            .park_budget_provider_claim(&agent, &turn.turn_id, 7, capture.clone(), now + 1000)
            .unwrap()
            .applied
    );
    assert!(
        scene
            .service
            .resume_budget_provider_claim(&agent, &turn.turn_id, 7, &capture, now + 1000)
            .is_err()
    );
    scene
        .service
        .requeue_budget_provider_claim(&agent, &turn.turn_id, 7, &capture)
        .unwrap();
    assert_eq!(
        scene
            .service
            .agent_scheduler()
            .snapshot()
            .active_capacity_used,
        1
    );
    assert!(
        !scene.service.agent_provider_task_is_pending(&turn.turn_id),
        "fair reacquisition offered a second inference"
    );
    let resumed = scene
        .service
        .resume_budget_provider_claim(&agent, &turn.turn_id, 7, &capture, now + 5000)
        .unwrap();
    assert!(resumed.applied);
    assert!(
        matches!(resumed.side_effects.as_slice(), [RuntimeSideEffect::ScheduleTimer { delay_ms, .. }] if *delay_ms <= 30_000 && *delay_ms > 0)
    );
    assert!(
        !scene
            .service
            .resume_budget_provider_claim(&agent, &turn.turn_id, 7, &capture, now + 6000)
            .unwrap()
            .applied
    );
    if deadline != 0 {
        assert_eq!(
            scene
                .service
                .agent_turn_ledger()
                .turn(&turn.turn_id)
                .unwrap()
                .deadline_at_unix_millis,
            deadline + 5000
        );
    }
    assert!(
        !scene
            .service
            .fail_expired_claimed_agent_provider_task(&turn.turn_id, 7)
            .unwrap(),
        "old timer shortened the resumed claim"
    );
    assert!(
        scene
            .service
            .park_budget_provider_claim(&agent, &turn.turn_id, 7, capture.clone(), now + 6000)
            .is_err()
    );
    // Explicit reconsideration revokes all captures of that hold, even if
    // policy still refuses it. No old Ready observation permits another park.
    assert!(matches!(
        work.admission_owner().retry_held().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    assert!(!capture.matches_owner(dispatch.budget_owner.as_ref().unwrap()));
    let next = work.admission_owner().capture_held().await.unwrap();
    scene
        .service
        .park_budget_provider_claim(&agent, &turn.turn_id, 7, next, now + 6000)
        .unwrap();
    scene
        .service
        .clear_claimed_agent_provider_task(&turn.turn_id);
    assert!(
        !scene
            .service
            .budget_provider_claim_is_parked(&turn.turn_id, 7)
    );
    assert!(
        scene
            .service
            .requeue_budget_provider_claim(&agent, &turn.turn_id, 7, &capture)
            .is_err()
    );
}

/// Actual sibling prompt work takes the released capacity. A full fair queue
/// cannot consume the original wait, and scheduler pause still inhibits it.
#[tokio::test]
async fn runtime_budget_provider_wait_preserves_queue_pressure_and_pause() {
    let (mut scene, dispatch, _work, capture) = held().await;
    let turn = &dispatch.turn;
    let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let now = crate::runtime::current_unix_millis();
    let deadline = scene
        .service
        .agent_turn_ledger()
        .turn(&turn.turn_id)
        .unwrap()
        .deadline_at_unix_millis;
    scene
        .service
        .park_budget_provider_claim(&agent, &turn.turn_id, 7, capture.clone(), now)
        .unwrap();
    // A different wait consumer cannot clear budget-owned inactive time.
    scene
        .service
        .resume_agent_turn_deadline(&turn.turn_id, now + 2000)
        .unwrap();
    assert_eq!(
        scene
            .service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .deadline_at_unix_millis,
        deadline
    );
    let primary = scene
        .service
        .attach_primary("budget-fairness", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let sibling_pane = scene
        .service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let queued_pane = scene
        .service
        .session
        .split_active_pane(&primary, SplitDirection::Horizontal)
        .unwrap();
    for pane in [&sibling_pane, &queued_pane] {
        scene
            .service
            .agent_shell_store_mut()
            .enter_or_resume(pane.as_str())
            .unwrap();
        mark_test_pane_ready(&mut scene.service, pane.as_str());
    }
    scene
        .service
        .agent_scheduler_mut()
        .set_queue_limits(1, usize::MAX)
        .unwrap();
    let sibling = scene
        .service
        .start_agent_prompt_turn(sibling_pane.as_str(), "unrelated useful work")
        .unwrap();
    let queued = scene
        .service
        .start_agent_prompt_turn(queued_pane.as_str(), "queued sibling")
        .unwrap();
    assert!(scene.service.agent_turn_is_running(&sibling.turn_id));
    assert!(
        scene
            .service
            .requeue_budget_provider_claim(&agent, &turn.turn_id, 7, &capture)
            .is_err()
    );
    assert_eq!(scene.service.agent_scheduler().snapshot().waiting, 1);
    assert!(
        scene
            .service
            .agent_provider_claim_matches(&agent, &turn.turn_id, 7)
    );
    assert!(scene.service.cancel_agent_work(&queued.turn_id));
    scene
        .service
        .requeue_budget_provider_claim(&agent, &turn.turn_id, 7, &capture)
        .unwrap();
    assert_eq!(scene.service.agent_scheduler().snapshot().reacquiring, 1);
    assert!(
        scene
            .service
            .resume_budget_provider_claim(&agent, &turn.turn_id, 7, &capture, now + 3000)
            .is_err()
    );
    scene
        .service
        .agent_scheduler_mut()
        .set_human_paused(&turn.turn_id, true);
    assert!(scene.service.cancel_agent_work(&sibling.turn_id));
    scene.service.start_ready_agent_turns().unwrap();
    assert_eq!(scene.service.agent_scheduler().snapshot().reacquiring, 1);
    assert!(!scene.service.agent_provider_task_is_pending(&turn.turn_id));
    scene
        .service
        .agent_scheduler_mut()
        .set_human_paused(&turn.turn_id, false);
    scene.service.start_ready_agent_turns().unwrap();
    assert_eq!(
        scene
            .service
            .agent_scheduler()
            .snapshot()
            .active_capacity_used,
        1
    );
    assert!(!scene.service.agent_provider_task_is_pending(&turn.turn_id));
    scene
        .service
        .resume_budget_provider_claim(&agent, &turn.turn_id, 7, &capture, now + 5000)
        .unwrap();
    if deadline != 0 {
        assert_eq!(
            scene
                .service
                .agent_turn_ledger()
                .turn(&turn.turn_id)
                .unwrap()
                .deadline_at_unix_millis,
            deadline + 5000
        );
    }
}

#[tokio::test]
async fn runtime_budget_provider_wait_rejects_foreign_hold_and_stale_claim() {
    let (mut scene, dispatch, _work, capture) = held().await;
    let (foreign, other, _other_work, other_capture) = held().await;
    let turn = &dispatch.turn;
    let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let now = crate::runtime::current_unix_millis();
    assert!(!capture.matches_owner(other.budget_owner.as_ref().unwrap()));
    assert!(
        scene
            .service
            .park_budget_provider_claim(&agent, &turn.turn_id, 7, other_capture, now)
            .is_err()
    );
    assert!(
        scene
            .service
            .park_budget_provider_claim(&agent, &turn.turn_id, 8, capture.clone(), now)
            .is_err()
    );
    assert_eq!(
        scene
            .service
            .agent_scheduler()
            .snapshot()
            .active_capacity_used,
        1
    );
    assert!(
        scene
            .service
            .agent_provider_claim_matches(&agent, &turn.turn_id, 7)
    );
    scene
        .service
        .park_budget_provider_claim(&agent, &turn.turn_id, 7, capture, now)
        .unwrap();
    scene
        .service
        .clear_claimed_agent_provider_task(&turn.turn_id);
    assert!(
        !scene
            .service
            .fail_expired_claimed_agent_provider_task(&turn.turn_id, 7)
            .unwrap()
    );
    drop(foreign);
}

/// A new claim in the same conversation cannot adopt an older attempt's hold.
#[tokio::test]
async fn runtime_budget_provider_wait_expiry_and_long_park_do_not_reset_lease() {
    let (mut scene, dispatch, _work, capture) = held().await;
    let turn = &dispatch.turn;
    let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let now = crate::runtime::current_unix_millis();
    assert!(
        scene
            .service
            .park_budget_provider_claim(&agent, &turn.turn_id, 7, capture.clone(), now + 30_000)
            .is_err()
    );
    assert_eq!(
        scene
            .service
            .agent_scheduler()
            .snapshot()
            .active_capacity_used,
        1
    );
    scene
        .service
        .park_budget_provider_claim(&agent, &turn.turn_id, 7, capture.clone(), now)
        .unwrap();
    assert!(
        !scene
            .service
            .park_budget_provider_claim(&agent, &turn.turn_id, 7, capture.clone(), now + 300_000)
            .unwrap()
            .applied
    );
    scene
        .service
        .requeue_budget_provider_claim(&agent, &turn.turn_id, 7, &capture)
        .unwrap();
    let resumed = scene
        .service
        .resume_budget_provider_claim(&agent, &turn.turn_id, 7, &capture, now + 300_000)
        .unwrap();
    assert!(matches!(resumed.side_effects.as_slice(),
        [RuntimeSideEffect::ScheduleTimer { delay_ms, .. }] if *delay_ms > 0 && *delay_ms <= 30_000));
}

/// A new claim in the same conversation cannot adopt an older attempt's hold.
#[tokio::test]
async fn runtime_budget_provider_wait_old_hold_cannot_park_new_claim() {
    let (mut scene, dispatch, _work, capture) = held().await;
    let turn = &dispatch.turn;
    let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
    scene
        .service
        .clear_claimed_agent_provider_task(&turn.turn_id);
    scene
        .service
        .record_claimed_agent_provider_task(&dispatch, 8, 30_000)
        .unwrap();
    assert!(
        scene
            .service
            .park_budget_provider_claim(
                &agent,
                &turn.turn_id,
                8,
                capture,
                crate::runtime::current_unix_millis()
            )
            .is_err(),
        "old hold parked replacement claim"
    );
}

/// A callback queued before parking can share the resumed timer key. Actual
/// actor ingress must rearm the remaining original lease, not consume it or
/// fail the original worker early. No physical provider is involved here.
#[tokio::test]
async fn runtime_budget_provider_wait_old_actor_timer_rearms_original_claim() {
    use crate::runtime::{RuntimeEvent, RuntimeEventBatch, TimerEvent};
    let (mut scene, dispatch, _work, capture) = held().await;
    let turn = &dispatch.turn;
    let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let now = crate::runtime::current_unix_millis();
    scene
        .service
        .park_budget_provider_claim(&agent, &turn.turn_id, 7, capture.clone(), now)
        .unwrap();
    scene
        .service
        .requeue_budget_provider_claim(&agent, &turn.turn_id, 7, &capture)
        .unwrap();
    let resumed = scene
        .service
        .resume_budget_provider_claim(&agent, &turn.turn_id, 7, &capture, now + 5000)
        .unwrap();
    let key = match resumed.side_effects.as_slice() {
        [RuntimeSideEffect::ScheduleTimer { key, .. }] => key.clone(),
        _ => panic!("original claim rearm missing"),
    };
    let remaining = scene
        .service
        .budget_provider_claim_timer_remaining(&turn.turn_id, 7, now + 5000)
        .unwrap();
    let expires_at = now + 5000 + remaining;
    let service = std::mem::replace(&mut scene.service, test_runtime_service());
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        handle
            .queue_runtime_side_effects(resumed.side_effects)
            .await
            .unwrap();
        let _ = handle.drain_timer_side_effects(64).await.unwrap();
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: key.clone(),
            now_ms: now,
        }));
        handle.submit_runtime_events(batch).await.unwrap();
        let effects = handle.drain_timer_side_effects(64).await.unwrap();
        let matching = effects.iter().filter(|effect| matches!(effect,
            RuntimeSideEffect::ScheduleTimer { key: actual, delay_ms } if actual == &key && *delay_ms > 0 && *delay_ms <= 30_000
        )).count();
        let running = handle.agent_turn_is_running(&turn.turn_id).await.unwrap();
        let mut before = RuntimeEventBatch::new();
        before.push(RuntimeEvent::Timer(TimerEvent {
            key: key.clone(),
            now_ms: expires_at - 1,
        }));
        handle.submit_runtime_events(before).await.unwrap();
        let boundary = handle.drain_timer_side_effects(64).await.unwrap();
        let last_millisecond = boundary
            .iter()
            .filter(|effect| {
                matches!(effect,
                    RuntimeSideEffect::ScheduleTimer { key: actual, delay_ms: 1 } if actual == &key
                )
            })
            .count();
        let mut elapsed = RuntimeEventBatch::new();
        elapsed.push(RuntimeEvent::Timer(TimerEvent {
            key: key.clone(),
            now_ms: expires_at,
        }));
        handle.submit_runtime_events(elapsed).await.unwrap();
        let still_running = handle.agent_turn_is_running(&turn.turn_id).await.unwrap();
        handle.shutdown().await.unwrap();
        (matching, running, last_millisecond, still_running)
    };
    let ((matching, running, last_millisecond, still_running), exit) =
        tokio::join!(client, actor.run());
    scene.service = exit.service;
    assert_eq!(matching, 1);
    assert!(running);
    assert_eq!(last_millisecond, 1);
    assert!(!still_running, "resumed claim lost its eventual expiry");
}
