//! Committed provider-retry timer custody after a real issued claim fails.
//!
//! The real provider preparation and actor-issued claim run without HTTP or a
//! native child. Failure must preserve its original retry attempt and backoff
//! across lane refusal, not replay failure or start inference during recovery.

use super::super::*;
use super::provider_source_handoff::pending_provider_at_capacity;

/// Prepares and claims an actual prompt, then occupies every transient slot.
/// Auth metadata is isolated in a nonexistent generated root; no credential or
/// provider endpoint is consulted and no inference future is run.
async fn claimed_provider_at_capacity() -> (
    AsyncRuntimeSessionActor,
    RuntimeAgentProviderDispatch,
    ClientId,
) {
    let (mut actor, turn, primary) = pending_provider_at_capacity();
    actor
        .service
        .configure_provider_retry_policy(mez_agent::ProviderRetryPolicy {
            max_attempts: 3,
            unlimited: false,
            initial_delay_ms: 1_000,
            max_delay_ms: 1_000,
        });
    actor
        .service
        .set_auth_store(crate::security::auth::AuthStore::new(
            crate::security::auth::AuthPaths::under_config_root(&std::env::temp_dir().join(
                format!(
                    "mez-retry-handoff-{}",
                    crate::storage::token_usage::new_token_usage_event_id()
                ),
            )),
        ));
    actor.drain_timer_side_effects(64).unwrap();
    let dispatch = claim_provider(&mut actor, &turn).await;
    actor.drain_timer_side_effects(64).unwrap();
    fill_pressure(&mut actor);
    (actor, dispatch, primary)
}

/// Uses ordinary off-actor preparation and strict actor claim admission for
/// each actual retry task. It never executes the returned inference request.
async fn claim_provider(
    actor: &mut AsyncRuntimeSessionActor,
    turn: &str,
) -> RuntimeAgentProviderDispatch {
    let agent = AgentId::opaque("agent-%1").unwrap();
    let (reply, response) = oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::PrepareConfiguredAgentProviderTask {
            agent_id: agent.clone(),
            turn_id: turn.to_string(),
            reply,
        })
        .await;
    let work = response.await.unwrap().unwrap().unwrap();
    assert!(work.mcp_plans.is_empty());
    let preparation = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    assert!(preparation.provider_refresh_error.is_none());
    let (reply, response) = oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ClaimConfiguredAgentProviderTask {
            agent_id: agent,
            turn_id: turn.to_string(),
            preparation,
            reply,
        })
        .await;
    response.await.unwrap().unwrap().unwrap()
}

/// Occupies the unchanged nominal capacity plus its finite committed reserve.
fn fill_pressure(actor: &mut AsyncRuntimeSessionActor) {
    let occupied = 1 + actor.committed_side_effect_overshoot();
    actor
        .admit_committed_side_effects(
            (0..occupied)
                .map(|index| RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::PanePipeHealth,
                        format!("retry-pressure-{index}"),
                        1,
                    ),
                    delay_ms: 60_000,
                })
                .collect(),
        )
        .unwrap();
}

/// Reports one genuine issued claim's retryable transport failure through the
/// actual event batch. The claim is not fabricated and cannot be replayed to
/// reconstruct the already accepted retry transition.
async fn fail_provider(
    actor: &mut AsyncRuntimeSessionActor,
    dispatch: &RuntimeAgentProviderDispatch,
) -> Result<RuntimeEventIngressReport> {
    let mut batch = RuntimeEventBatch::new();
    batch.push(retry_failure(dispatch));
    actor.apply_runtime_event_batch(batch).await
}

/// Returns failure evidence for the exact issued claim, never a new invocation.
fn retry_failure(dispatch: &RuntimeAgentProviderDispatch) -> RuntimeEvent {
    RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
        agent_id: AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap(),
        turn_id: dispatch.turn.turn_id.clone(),
        claim_generation: dispatch.claim_generation,
        kind: "io".into(),
        message: "connection reset".into(),
        provider_failure_json: None,
        provider_raw_text: None,
        provider_output_limit_state: None,
    })
}

/// Selects only provider-retry schedules, leaving unrelated timer work intact.
fn retry_schedules(effects: &[RuntimeSideEffect]) -> Vec<(RuntimeTimerKey, u64)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                if key.kind == RuntimeTimerKind::ProviderRetry =>
            {
                Some((key.clone(), *delay_ms))
            }
            _ => None,
        })
        .collect()
}

/// A real retryable failure consumes its issued claim before raw timer
/// admission fails. Real capacity progress must recover the exact first retry
/// with only 250ms left, and the matching timer must yield one provider offer.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn refused_provider_retry_timer_recovers_original_attempt_and_deadline() {
    let (mut actor, dispatch, primary) = claimed_provider_at_capacity().await;
    assert!(
        fail_provider(&mut actor, &dispatch)
            .await
            .unwrap_err()
            .message()
            .contains("consumed=1")
    );
    assert!(
        actor
            .actor_owned_agent_progress_turn_ids()
            .contains(&dispatch.turn.turn_id)
    );
    let progress = actor.actor_owned_agent_progress_turn_ids();
    assert_eq!(
        actor
            .service
            .reconcile_agent_runtime_progress_paths_with_actor_progress(&progress)
            .unwrap(),
        0
    );
    assert!(
        !actor
            .service
            .agent_provider_task_is_pending(&dispatch.turn.turn_id)
    );
    tokio::time::advance(std::time::Duration::from_millis(750)).await;
    actor.drain_timer_side_effects(64).unwrap();
    let effects = actor.drain_timer_side_effects(64).unwrap();
    let retry = effects
        .iter()
        .filter_map(|effect| match effect {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                if key.kind == RuntimeTimerKind::ProviderRetry =>
            {
                Some((key.clone(), *delay_ms))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(retry.len(), 1, "{effects:?}");
    assert_eq!(retry[0].0.owner_id, dispatch.turn.turn_id);
    assert_eq!(retry[0].0.generation, 1);
    assert_eq!(retry[0].1, 250);
    tokio::time::advance(std::time::Duration::from_millis(250)).await;
    let mut elapsed = RuntimeEventBatch::new();
    elapsed.push(RuntimeEvent::Timer(TimerEvent {
        key: retry[0].0.clone(),
        now_ms: 1_000,
    }));
    assert_eq!(
        actor
            .apply_runtime_event_batch(elapsed)
            .await
            .unwrap()
            .applied,
        1
    );
    let offered = actor.drain_agent_provider_dispatch_side_effects(8).unwrap();
    assert!(
        matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentProvider { turn_id, .. }] if turn_id == &dispatch.turn.turn_id)
    );
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(8)
            .unwrap()
            .is_empty()
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// Stopping the actual turn before capacity returns retires its reducer source
/// and pending deadline. Later capacity and a stale callback cannot redispatch.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn provider_retry_handoff_stop_retires_unissued_source() {
    let (mut actor, dispatch, primary) = claimed_provider_at_capacity().await;
    fail_provider(&mut actor, &dispatch).await.unwrap_err();
    assert_eq!(actor.timers.pending_provider_retry.len(), 1);
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert_eq!(
        actor
            .service
            .pending_agent_provider_retry_timer(&dispatch.turn.turn_id),
        None
    );
    actor.drain_timer_side_effects(64).unwrap();
    assert!(retry_schedules(&actor.drain_timer_side_effects(64).unwrap()).is_empty());
    assert!(actor.timers.pending_provider_retry.is_empty());
    let late = actor
        .apply_runtime_timer_event(TimerEvent {
            key: RuntimeTimerKey::new(RuntimeTimerKind::ProviderRetry, &dispatch.turn.turn_id, 1),
            now_ms: 1_000,
        })
        .unwrap();
    assert!(!late.applied);
    assert!(
        !actor
            .service
            .agent_provider_task_is_pending(&dispatch.turn.turn_id)
    );
}

/// Repeated committed refusal cannot reset the original deadline or adopt a
/// later configured backoff. Queue admission also cannot acknowledge receipt:
/// both offer adapters must deduct additional waiting in the queue itself.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn provider_retry_handoff_repeated_refusal_and_queued_wait_preserve_clock() {
    for generic in [false, true] {
        let (mut actor, dispatch, primary) = claimed_provider_at_capacity().await;
        fail_provider(&mut actor, &dispatch).await.unwrap_err();
        actor
            .service
            .configure_provider_retry_policy(mez_agent::ProviderRetryPolicy {
                max_attempts: 3,
                unlimited: false,
                initial_delay_ms: 10_000,
                max_delay_ms: 10_000,
            });
        assert_eq!(
            actor
                .service
                .pending_agent_provider_retry_timer(&dispatch.turn.turn_id),
            Some((1, 1_000))
        );
        tokio::time::advance(std::time::Duration::from_millis(250)).await;
        for _ in 0..3 {
            assert!(
                actor
                    .admit_committed_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                        key: RuntimeTimerKey::new(
                            RuntimeTimerKind::ProviderRetry,
                            &dispatch.turn.turn_id,
                            1
                        ),
                        delay_ms: 1_000
                    }])
                    .is_err()
            );
        }
        tokio::time::advance(std::time::Duration::from_millis(500)).await;
        actor.drain_timer_side_effects(64).unwrap();
        assert_eq!(actor.timers.pending_provider_retry.len(), 1);
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        let offered = if generic {
            actor.drain_runtime_side_effects(64).unwrap()
        } else {
            actor.drain_timer_side_effects(64).unwrap()
        };
        let schedules = retry_schedules(&offered);
        assert_eq!(schedules.len(), 1);
        assert_eq!(schedules[0].1, 150);
        assert!(actor.timers.pending_provider_retry.is_empty());
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
    }
}

/// Long pressure leaves only the worker's minimum delay, never a new backoff.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn provider_retry_handoff_expired_deadline_is_not_restarted() {
    let (mut actor, dispatch, primary) = claimed_provider_at_capacity().await;
    fail_provider(&mut actor, &dispatch).await.unwrap_err();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    actor.drain_timer_side_effects(64).unwrap();
    let schedules = retry_schedules(&actor.drain_timer_side_effects(64).unwrap());
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0].1, 1);
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// A real reducer source alone is not committed transfer enrollment. Strict
/// producer refusal and forged owner/attempt/delay projections cannot acquire
/// custody or turn an ordinary future retry into an accelerated dispatch.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn provider_retry_handoff_requires_exact_committed_projection() {
    let (mut actor, dispatch, primary) = claimed_provider_at_capacity().await;
    let transition = actor
        .apply_runtime_event(retry_failure(&dispatch))
        .await
        .unwrap();
    assert!(transition.applied);
    assert!(
        actor
            .queue_runtime_side_effects(transition.side_effects)
            .is_err()
    );
    assert!(actor.timers.pending_provider_retry.is_empty());
    for (owner, attempt, delay) in [
        ("unrelated", 1, 1_000),
        (dispatch.turn.turn_id.as_str(), 2, 1_000),
        (dispatch.turn.turn_id.as_str(), 1, 999),
    ] {
        assert!(
            actor
                .admit_committed_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(RuntimeTimerKind::ProviderRetry, owner, attempt),
                    delay_ms: delay
                }])
                .is_err()
        );
        assert!(actor.timers.pending_provider_retry.is_empty());
    }
    actor.drain_timer_side_effects(64).unwrap();
    assert!(retry_schedules(&actor.drain_timer_side_effects(64).unwrap()).is_empty());
    assert_eq!(
        actor
            .service
            .pending_agent_provider_retry_timer(&dispatch.turn.turn_id),
        Some((1, 1_000))
    );
    assert!(
        !actor
            .service
            .agent_provider_task_is_pending(&dispatch.turn.turn_id)
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// The second retry follows a real first timer and second issued claim. Its
/// refusal must retain attempt two only. Duplicate old cancellation/callback
/// cannot retire its deadline or dispatch the replacement early.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn provider_retry_handoff_new_attempt_fences_old_cancellation_and_callback() {
    let (mut actor, first, primary) = claimed_provider_at_capacity().await;
    fail_provider(&mut actor, &first).await.unwrap_err();
    actor.drain_timer_side_effects(64).unwrap();
    let old = retry_schedules(&actor.drain_timer_side_effects(64).unwrap())
        .remove(0)
        .0;
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    let mut elapsed = RuntimeEventBatch::new();
    elapsed.push(RuntimeEvent::Timer(TimerEvent {
        key: old.clone(),
        now_ms: 1_000,
    }));
    assert_eq!(
        actor
            .apply_runtime_event_batch(elapsed)
            .await
            .unwrap()
            .applied,
        1
    );
    assert_eq!(
        actor
            .drain_agent_provider_dispatch_side_effects(8)
            .unwrap()
            .len(),
        1
    );
    let second = claim_provider(&mut actor, &first.turn.turn_id).await;
    assert_ne!(second.claim_generation, first.claim_generation);
    actor.drain_timer_side_effects(64).unwrap();
    fill_pressure(&mut actor);
    fail_provider(&mut actor, &second).await.unwrap_err();
    assert_eq!(
        actor
            .service
            .pending_agent_provider_retry_timer(&first.turn.turn_id),
        Some((2, 1_000))
    );
    assert_eq!(actor.timers.pending_provider_retry.len(), 1);
    tokio::time::advance(std::time::Duration::from_millis(250)).await;
    actor.drain_timer_side_effects(1).unwrap();
    actor.drain_timer_side_effects(1).unwrap();
    actor
        .admit_committed_side_effects(vec![RuntimeSideEffect::CancelTimer { key: old.clone() }])
        .unwrap();
    assert_eq!(
        actor
            .timers
            .pending_provider_retry
            .get(&first.turn.turn_id)
            .unwrap()
            .0
            .generation,
        2
    );
    assert!(
        !actor
            .apply_runtime_timer_event(TimerEvent {
                key: old,
                now_ms: 1_250
            })
            .unwrap()
            .applied
    );
    assert_eq!(
        actor
            .service
            .pending_agent_provider_retry_timer(&first.turn.turn_id),
        Some((2, 1_000))
    );
    let recovered = retry_schedules(&actor.drain_timer_side_effects(64).unwrap());
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].0.generation, 2);
    assert_eq!(recovered[0].1, 750);
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// Service backlog cannot take every newly available slot before retry
/// recovery. Four-family rotation admits the exact pending timer within four
/// real one-item drains while the total transient count stays at most 17.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn provider_retry_handoff_fair_progress_with_service_backlog() {
    let (mut actor, dispatch, primary) = claimed_provider_at_capacity().await;
    fail_provider(&mut actor, &dispatch).await.unwrap_err();
    for _ in 0..32 {
        actor.service.queue_deferred_pane_input_for_tests(
            RuntimeSideEffect::WritePaneInputPriority {
                pane_id: "%1".into(),
                bytes: b"original-service-reply".to_vec(),
            },
        );
    }
    for _ in 0..4 {
        assert_eq!(actor.drain_timer_side_effects(1).unwrap().len(), 1);
        assert!(actor.side_effects.len() + actor.side_effect_routes.non_persistence_len() <= 17);
    }
    assert_eq!(
        actor
            .timers
            .provider_retry
            .get(&dispatch.turn.turn_id)
            .unwrap()
            .generation,
        1
    );
    assert_eq!(actor.timers.pending_provider_retry.len(), 1);
    assert_eq!(
        retry_schedules(&actor.drain_timer_side_effects(64).unwrap()).len(),
        1
    );
    assert!(actor.timers.pending_provider_retry.is_empty());
    assert!(
        !actor
            .service
            .drain_pane_io_transition()
            .side_effects
            .is_empty()
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}
