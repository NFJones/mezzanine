//! Source-owned provider dispatch recovery at real capacity-drain boundaries.
//!
//! These fixtures submit a genuine prompt but never execute a provider, spawn a
//! child, or run timer/persistence workers. A descriptor refused by the finite
//! lane must remain recoverable from its service task, and a descriptor already
//! offered to a worker must not be regenerated before preparation/claim settles.

use super::super::*;

/// Creates a real pending provider task with a saturated finite transient lane.
fn pending_provider_at_capacity() -> (AsyncRuntimeSessionActor, String, ClientId) {
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let primary = service
        .attach_primary("handoff-primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "source handoff")
        .unwrap();
    let task = service.pending_agent_provider_tasks().remove(0);
    let (_, mut actor) = AsyncRuntimeSessionActor::new(
        service,
        AsyncRuntimeActorConfig {
            side_effect_buffer: 1,
            ..Default::default()
        },
    )
    .unwrap();
    actor.drain_runtime_side_effects(64).unwrap();
    actor.drain_timer_side_effects(64).unwrap();
    actor.drain_persistence_side_effects(64).unwrap();
    let capacity = 1 + actor.committed_side_effect_overshoot();
    actor
        .admit_committed_side_effects(
            (0..capacity)
                .map(|index| RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::PanePipeHealth,
                        format!("occupied-{index}"),
                        1,
                    ),
                    delay_ms: 25,
                })
                .collect(),
        )
        .unwrap();
    (actor, task.turn_id, primary)
}

/// Rejection cannot strand a real pending provider task once another lane frees
/// capacity. Recovery uses its existing owner without another runtime event.
/// Draining that recovered descriptor is only an offer, not a claim: repeated
/// drains and ordinary pending-task projection must not duplicate it while a
/// worker still owns preparation. No request or action is actually executed.
#[tokio::test(flavor = "current_thread")]
async fn refused_provider_dispatch_recovers_once_on_capacity_drain() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert_eq!(effects.len(), 1);
    assert!(actor.admit_committed_side_effects(effects).is_err());
    assert!(actor.service.agent_provider_task_is_pending(&turn_id));
    assert_eq!(actor.drain_timer_side_effects(1).unwrap().len(), 1);
    let offered = actor.drain_agent_provider_dispatch_side_effects(1).unwrap();
    assert!(
        matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentProvider { turn_id: owner, .. }] if owner == &turn_id),
        "{offered:?}"
    );
    assert!(actor.service.agent_provider_task_is_pending(&turn_id));
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .is_empty()
    );
    assert!(
        actor
            .pending_provider_dispatch_side_effects()
            .unwrap()
            .is_empty()
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// Cancellation retires the original source before recovery. Releasing lane
/// capacity afterward must not recreate a dispatch for the stopped turn, even
/// though the earlier capacity refusal consumed the event-side descriptor.
#[tokio::test(flavor = "current_thread")]
async fn cancelled_provider_source_is_not_recovered_after_refusal() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(!actor.service.agent_provider_task_is_owned(&turn_id));
    actor.drain_timer_side_effects(1).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .is_empty()
    );
}

/// Normal pending tasks have not suffered committed refusal. A timer drain
/// cannot accelerate their poll scheduling, and strict producer refusal cannot
/// silently convert them into committed recovery work.
#[tokio::test(flavor = "current_thread")]
async fn ordinary_pending_provider_is_not_accelerated_by_lane_drain() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.queue_runtime_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        actor
            .pending_provider_dispatch_side_effects()
            .unwrap()
            .len(),
        1
    );
    assert!(actor.refused_provider_turns.is_empty());
    assert!(actor.offered_provider_turns.is_empty());
    assert!(actor.service.agent_provider_task_is_pending(&turn_id));
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// The exact recovered offer may return no work during preparation. Its source
/// remains recoverable without duplicate offers while held, then becomes ready
/// again after settlement. Cancellation prunes both small identity sets on real
/// progress, rather than accumulating obsolete turn IDs over the actor lifetime.
#[tokio::test(flavor = "current_thread")]
async fn recovered_offer_returns_to_pending_source_and_retires_on_stop() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert_eq!(actor.drain_runtime_side_effects(1).unwrap().len(), 1);
    assert!(actor.offered_provider_turns.contains(&turn_id));
    actor.settle_provider_offer(&turn_id);
    assert!(!actor.offered_provider_turns.contains(&turn_id));
    assert!(actor.refused_provider_turns.contains(&turn_id));
    actor.drain_timer_side_effects(1).unwrap();
    assert_eq!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .len(),
        1
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    actor.drain_timer_side_effects(1).unwrap();
    assert!(actor.offered_provider_turns.is_empty());
    assert!(actor.refused_provider_turns.is_empty());
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .is_empty()
    );
}

/// Continuously ready service input cannot consume every newly freed slot ahead
/// of refused provider work. The next two single-slot drains give each source
/// family first access once, while both stay within the original finite reserve.
#[tokio::test(flavor = "current_thread")]
async fn provider_recovery_alternates_capacity_with_deferred_service_input() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let capacity = 1 + actor.committed_side_effect_overshoot();
    for _ in 0..capacity + 1 {
        actor.service.queue_deferred_pane_input_for_tests(
            RuntimeSideEffect::WritePaneInputPriority {
                pane_id: "%1".into(),
                bytes: b"owned-input".to_vec(),
            },
        );
    }
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    for _ in 0..2 {
        assert_eq!(actor.drain_timer_side_effects(1).unwrap().len(), 1);
        assert_eq!(actor.available_committed_side_effect_capacity(), 0);
    }
    let offered = actor.drain_agent_provider_dispatch_side_effects(1).unwrap();
    assert!(
        matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentProvider { turn_id: owner, .. }] if owner == &turn_id)
    );
    let service = actor.drain_runtime_side_effects(1).unwrap();
    assert!(
        matches!(service.as_slice(), [RuntimeSideEffect::WritePaneInputPriority { bytes, .. }] if bytes == b"owned-input")
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// A recovered descriptor can be cancelled while held by a worker. Its actual
/// preparation request must return no work and release the offer at the request
/// boundary, without depending on a later lane drain to repair stale metadata.
/// The cancelled turn cannot regain provider authority or create another offer.
#[tokio::test(flavor = "current_thread")]
async fn cancelled_recovered_offer_retires_at_real_preparation_boundary() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    let offered = actor.drain_agent_provider_dispatch_side_effects(1).unwrap();
    let RuntimeSideEffect::DispatchAgentProvider { agent_id, .. } =
        offered.into_iter().next().unwrap()
    else {
        panic!("expected the recovered provider descriptor");
    };
    actor.drain_timer_side_effects(64).unwrap();
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    let (reply, response) = tokio::sync::oneshot::channel();
    assert!(
        !actor
            .handle_request(AsyncRuntimeRequest::PrepareConfiguredAgentProviderTask {
                agent_id,
                turn_id: turn_id.clone(),
                reply,
            })
            .await
    );
    assert!(response.await.unwrap().unwrap().is_none());
    assert!(!actor.offered_provider_turns.contains(&turn_id));
    assert!(!actor.refused_provider_turns.contains(&turn_id));
    assert!(!actor.service.agent_provider_task_is_owned(&turn_id));
}
