//! Original-deadline custody for committed synchronized terminal output.
//!
//! A real terminal screen parses process output while every transient slot is
//! occupied. Only inert lane descriptors fill capacity; no process, provider,
//! persistence or timer worker runs. Recovery must not require output replay.

use super::super::*;

/// Installs a genuine process terminal screen without launching a native child.
fn actor_at_capacity() -> AsyncRuntimeSessionActor {
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service.set_process_pane_screen(
        "%1",
        mez_terminal::TerminalScreen::new(Size::new(80, 24).unwrap(), 120).unwrap(),
    );
    let (_, mut actor) = AsyncRuntimeSessionActor::new(
        service,
        AsyncRuntimeActorConfig {
            side_effect_buffer: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let occupied = 1 + actor.committed_side_effect_overshoot();
    actor
        .admit_committed_side_effects(
            (0..occupied)
                .map(|index| RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::PanePipeHealth,
                        format!("pressure-{index}"),
                        1,
                    ),
                    delay_ms: 60_000,
                })
                .collect(),
        )
        .unwrap();
    actor
}

/// Applies actual terminal bytes through the serialized actor ingress boundary.
async fn output(
    actor: &mut AsyncRuntimeSessionActor,
    bytes: &[u8],
) -> Result<RuntimeEventIngressReport> {
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::Pane(PaneEvent::Output {
        pane_id: "%1".into(),
        bytes: bytes.to_vec(),
    }));
    actor.apply_runtime_event_batch(batch).await
}

/// Accepted terminal begin must retain the exact epoch and original deadline
/// when its raw schedule is refused. After 750ms of pressure, lane progress
/// offers only the remaining 250ms, then the real timeout releases the screen.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn synchronized_output_refusal_recovers_original_epoch_deadline() {
    let mut actor = actor_at_capacity();
    let error = output(&mut actor, b"\x1b[?2026haccepted-screen-bytes")
        .await
        .unwrap_err();
    assert!(error.message().contains("consumed=1"));
    assert_eq!(
        actor
            .service
            .process_pane_screen("%1")
            .unwrap()
            .synchronized_output_begin_epoch(),
        Some(1)
    );
    tokio::time::advance(std::time::Duration::from_millis(750)).await;
    actor.drain_timer_side_effects(64).unwrap();
    let effects = actor.drain_timer_side_effects(64).unwrap();
    let schedules = effects
        .iter()
        .filter_map(|effect| match effect {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                if key.kind == RuntimeTimerKind::SynchronizedOutput =>
            {
                Some((key.clone(), *delay_ms))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(schedules.len(), 1, "{effects:?}");
    let (key, delay_ms) = &schedules[0];
    assert_eq!(key.generation, 1);
    assert_eq!(*delay_ms, 250);
    assert!(actor.drain_timer_side_effects(64).unwrap().is_empty());
    let transition = actor
        .apply_runtime_timer_event(TimerEvent {
            key: key.clone(),
            now_ms: 1_000,
        })
        .unwrap();
    assert!(transition.applied);
    assert_eq!(
        actor
            .service
            .process_pane_screen("%1")
            .unwrap()
            .synchronized_output_begin_epoch(),
        None
    );
}

/// Normal end and replacing the process screen with an inactive screen both
/// retire a refused source. Neither may create a phantom recovery timer after
/// capacity becomes available; a late old callback has no admitted authority.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn synchronized_output_refusal_end_or_screen_reset_retires_source() {
    for reset in [false, true] {
        let mut actor = actor_at_capacity();
        output(&mut actor, b"\x1b[?2026h").await.unwrap_err();
        if reset {
            actor.service.set_process_pane_screen(
                "%1",
                mez_terminal::TerminalScreen::new(Size::new(80, 24).unwrap(), 120).unwrap(),
            );
        } else {
            output(&mut actor, b"\x1b[?2026l").await.unwrap();
        }
        actor.drain_timer_side_effects(64).unwrap();
        assert!(actor.drain_timer_side_effects(64).unwrap().is_empty());
        assert!(actor.timers.pending_synchronized_output.is_empty());
        let late = actor
            .apply_runtime_timer_event(TimerEvent {
                key: RuntimeTimerKey::new(RuntimeTimerKind::SynchronizedOutput, "%1", 1),
                now_ms: 1_000,
            })
            .unwrap();
        assert!(!late.applied);
    }
}

/// Repeated failed projection preserves a deadline, while a genuine later begin
/// creates one fresh epoch/deadline and retires the unissued predecessor. The
/// old callback cannot release the replacement; only the current key can.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn synchronized_output_refusal_rearm_is_exact_and_bounded() {
    let mut actor = actor_at_capacity();
    output(&mut actor, b"\x1b[?2026h").await.unwrap_err();
    tokio::time::advance(std::time::Duration::from_millis(250)).await;
    for _ in 0..3 {
        let effects = actor.synchronized_output_timer_side_effects_for_pane("%1");
        assert!(
            matches!(effects.as_slice(), [RuntimeSideEffect::ScheduleTimer { key, delay_ms: 750 }] if key.generation == 1)
        );
        assert!(actor.admit_committed_side_effects(effects).is_err());
    }
    output(&mut actor, b"\x1b[?2026h").await.unwrap_err();
    assert_eq!(actor.timers.pending_synchronized_output.len(), 1);
    tokio::time::advance(std::time::Duration::from_millis(750)).await;
    actor.drain_timer_side_effects(64).unwrap();
    let effects = actor.drain_timer_side_effects(64).unwrap();
    assert!(
        matches!(effects.as_slice(), [RuntimeSideEffect::ScheduleTimer { key, delay_ms: 250 }] if key.generation == 2)
    );
    let stale = actor
        .apply_runtime_timer_event(TimerEvent {
            key: RuntimeTimerKey::new(RuntimeTimerKind::SynchronizedOutput, "%1", 1),
            now_ms: 1_000,
        })
        .unwrap();
    assert!(!stale.applied);
    assert_eq!(
        actor
            .service
            .process_pane_screen("%1")
            .unwrap()
            .synchronized_output_begin_epoch(),
        Some(2)
    );
    assert!(actor.timers.pending_synchronized_output.is_empty());
}

/// Expired retained deadlines use only the timer worker's existing minimum
/// delay, never a new one-second recovery window after prolonged pressure.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn synchronized_output_refusal_expired_deadline_does_not_restart() {
    let mut actor = actor_at_capacity();
    output(&mut actor, b"\x1b[?2026h").await.unwrap_err();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    actor.drain_timer_side_effects(64).unwrap();
    let effects = actor.drain_timer_side_effects(64).unwrap();
    assert!(
        matches!(effects.as_slice(), [RuntimeSideEffect::ScheduleTimer { key, delay_ms: 1 }] if key.generation == 1)
    );
}

/// An already issued epoch needs an exact worker cancellation. At one free slot
/// the cancellation can precede the replacement schedule without losing its
/// deadline. Normal end instead recovers cancellation only, with no new epoch.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn synchronized_output_refusal_issued_cancel_and_rearm_fit_single_slots() {
    for rearm in [false, true] {
        let mut actor = actor_at_capacity();
        actor.drain_timer_side_effects(64).unwrap();
        output(&mut actor, b"\x1b[?2026h").await.unwrap();
        let first = actor.drain_timer_side_effects(64).unwrap();
        let old = match first.as_slice() {
            [
                RuntimeSideEffect::ScheduleTimer {
                    key,
                    delay_ms: 1_000,
                },
            ] => key.clone(),
            other => panic!("{other:?}"),
        };
        actor
            .admit_committed_side_effects(
                (0..17)
                    .map(|index| RuntimeSideEffect::ScheduleTimer {
                        key: RuntimeTimerKey::new(
                            RuntimeTimerKind::PanePipeHealth,
                            format!("refill-{index}"),
                            1,
                        ),
                        delay_ms: 60_000,
                    })
                    .collect(),
            )
            .unwrap();
        output(
            &mut actor,
            if rearm {
                b"\x1b[?2026h"
            } else {
                b"\x1b[?2026l"
            },
        )
        .await
        .unwrap_err();
        tokio::time::advance(std::time::Duration::from_millis(750)).await;
        actor.drain_timer_side_effects(1).unwrap();
        assert!(!actor.timers.synchronized_output.contains_key("%1"));
        assert_eq!(
            actor.timers.pending_synchronized_output.len(),
            usize::from(rearm)
        );
        actor.drain_timer_side_effects(1).unwrap();
        let recovered = actor.drain_timer_side_effects(64).unwrap();
        let relevant = recovered
            .iter()
            .filter(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, .. }
                | RuntimeSideEffect::CancelTimer { key } => {
                    key.kind == RuntimeTimerKind::SynchronizedOutput
                }
                _ => false,
            })
            .collect::<Vec<_>>();
        assert!(matches!(relevant[0], RuntimeSideEffect::CancelTimer { key } if key == &old));
        assert_eq!(relevant.len(), if rearm { 2 } else { 1 });
        if rearm {
            assert!(
                matches!(relevant[1], RuntimeSideEffect::ScheduleTimer { key, delay_ms: 250 } if key.generation == 2)
            );
        }
        assert!(actor.timers.pending_synchronized_output.is_empty());
        assert!(
            !actor
                .apply_runtime_timer_event(TimerEvent {
                    key: old,
                    now_ms: 1_000
                })
                .unwrap()
                .applied
        );
    }
}

/// A sustained existing service-source backlog cannot take every newly freed
/// slot ahead of timer recovery. The shared source rotation admits the pending
/// epoch within three real one-item drains without changing the 17-slot bound.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn synchronized_output_refusal_fair_progress_with_service_backlog() {
    let mut actor = actor_at_capacity();
    output(&mut actor, b"\x1b[?2026h").await.unwrap_err();
    for _ in 0..32 {
        actor.service.queue_deferred_pane_input_for_tests(
            RuntimeSideEffect::WritePaneInputPriority {
                pane_id: "%1".into(),
                bytes: b"owned-service-reply".to_vec(),
            },
        );
    }
    for _ in 0..3 {
        assert_eq!(actor.drain_timer_side_effects(1).unwrap().len(), 1);
        assert!(actor.side_effects.len() + actor.side_effect_routes.non_persistence_len() <= 17);
    }
    assert_eq!(actor.timers.pending_synchronized_output.len(), 1);
    assert_eq!(
        actor
            .timers
            .synchronized_output
            .get("%1")
            .unwrap()
            .generation,
        1
    );
    actor.drain_timer_side_effects(64).unwrap();
    assert!(actor.timers.pending_synchronized_output.is_empty());
    assert!(
        !actor
            .service
            .drain_pane_io_transition()
            .side_effects
            .is_empty()
    );
}

/// Queue admission is not worker receipt. Both generic and typed drains must
/// subtract subsequent queued delay before offering the exact schedule, even
/// on the immediate successful path without an initial admission refusal.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn synchronized_output_offered_delay_includes_queued_wait() {
    for generic in [false, true] {
        let mut actor = actor_at_capacity();
        actor.drain_timer_side_effects(64).unwrap();
        output(&mut actor, b"\x1b[?2026h").await.unwrap();
        assert_eq!(actor.timers.pending_synchronized_output.len(), 1);
        tokio::time::advance(std::time::Duration::from_millis(750)).await;
        let effects = if generic {
            actor.drain_runtime_side_effects(64).unwrap()
        } else {
            actor.drain_timer_side_effects(64).unwrap()
        };
        assert!(
            matches!(effects.as_slice(), [RuntimeSideEffect::ScheduleTimer { key, delay_ms: 250 }] if key.kind == RuntimeTimerKind::SynchronizedOutput && key.generation == 1)
        );
        assert!(actor.timers.pending_synchronized_output.is_empty());
    }
}
