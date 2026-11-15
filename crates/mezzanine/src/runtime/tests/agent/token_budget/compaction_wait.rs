//! Genuine manual compaction claim and positive native hold, with controlled
//! lease time. No provider request or replacement ordinary turn is synthesized.

use crate::runtime::{
    RuntimeBudgetAttemptAdmission, RuntimeBudgetAuxiliarySource, RuntimeSideEffect,
};
use crate::storage::token_usage::budget::receipts::BudgetAttemptPurpose;
use std::sync::Arc;

async fn claimed() -> (
    super::Scene,
    mez_core::ids::ClientId,
    u64,
    crate::runtime::RuntimeBudgetAdmissionWork,
) {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};
    let (mut scene, primary, conversation) = super::auxiliary::setup();
    for sequence in 1..=3 {
        scene
            .transcripts
            .append(&TranscriptEntry {
                conversation_id: conversation.clone(),
                sequence,
                created_at_unix_seconds: sequence,
                role: TranscriptRole::User,
                turn_id: format!("original-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: format!("original source {sequence} {}", "reference ".repeat(5_000)),
            })
            .unwrap();
    }
    scene
        .service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 3)
        .unwrap();
    scene
        .service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    let generation = scene
        .service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    super::auxiliary::prepare(
        &mut scene,
        &RuntimeBudgetAuxiliarySource::Compaction {
            pane: "%1".into(),
            generation,
        },
    )
    .await
    .unwrap();
    let dispatch = scene
        .service
        .claim_agent_compaction_task("%1", generation)
        .unwrap()
        .unwrap();
    let admission = dispatch
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            mez_agent::ModelTokenUsageKey::new("openai", "gpt-test"),
            BudgetAttemptPurpose::Compaction,
        )
        .unwrap()
        .admission_owner();
    assert!(matches!(
        admission.claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    (scene, primary, generation, admission)
}

fn delay(transition: &crate::runtime::RuntimeTransition) -> Option<u64> {
    transition
        .side_effects
        .iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::ScheduleTimer { delay_ms, .. } => Some(*delay_ms),
            _ => None,
        })
}

#[tokio::test]
async fn runtime_budget_compaction_lease_conserves_allowance_across_repeated_holds() {
    let (mut scene, primary, generation, admission) = claimed().await;
    let capture = admission.capture_held().await.unwrap();
    let waiter = Arc::new(());
    assert!(
        scene
            .service
            .park_budget_compaction_claim(capture.clone(), Arc::downgrade(&waiter), 100)
            .is_err()
    );
    scene
        .service
        .note_compaction_budget_lease("%1", generation, 100, 1000);
    let first = scene
        .service
        .park_budget_compaction_claim(capture.clone(), Arc::downgrade(&waiter), 300)
        .unwrap();
    assert_eq!(delay(&first), Some(1000)); // Watchdog, not renewed active allowance.
    assert!(
        scene
            .service
            .park_budget_compaction_claim(capture.clone(), Arc::downgrade(&waiter), 90_000)
            .unwrap()
            .side_effects
            .is_empty()
    );
    assert_eq!(
        scene
            .service
            .compaction_budget_timer_delay("%1", generation, 99_999),
        Some(1000)
    );
    let resumed = scene
        .service
        .resume_budget_compaction_claim(&capture, 100_000)
        .unwrap()
        .unwrap();
    assert_eq!(delay(&resumed), Some(800));
    assert!(
        scene
            .service
            .resume_budget_compaction_claim(&capture, 100_050)
            .unwrap()
            .unwrap()
            .side_effects
            .is_empty()
    );
    scene
        .service
        .note_compaction_budget_lease("%1", generation, 100_050, 1000);
    assert_eq!(
        scene
            .service
            .compaction_budget_timer_delay("%1", generation, 100_100),
        Some(700)
    );
    assert!(
        scene
            .service
            .park_budget_compaction_claim(capture.clone(), Arc::downgrade(&waiter), 100_100)
            .is_err()
    );
    assert!(matches!(
        admission.retry_held().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    assert!(
        scene
            .service
            .resume_budget_compaction_claim(&capture, 100_100)
            .is_err()
    );
    let second = admission.capture_held().await.unwrap();
    scene
        .service
        .park_budget_compaction_claim(second.clone(), Arc::downgrade(&waiter), 100_100)
        .unwrap();
    let resumed = scene
        .service
        .resume_budget_compaction_claim(&second, 200_000)
        .unwrap()
        .unwrap();
    assert_eq!(delay(&resumed), Some(700));
    assert_eq!(
        scene
            .service
            .compaction_budget_timer_delay("%1", generation, 200_699),
        Some(1)
    );
    assert_eq!(
        scene
            .service
            .compaction_budget_timer_delay("%1", generation, 200_700),
        None
    );
    assert!(matches!(
        admission.retry_held().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    let exhausted = admission.capture_held().await.unwrap();
    assert!(
        scene
            .service
            .park_budget_compaction_claim(exhausted, Arc::downgrade(&waiter), 200_700)
            .is_err()
    );
    scene
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(
        scene
            .service
            .resume_budget_compaction_claim(&second, 200_701)
            .is_err()
    );
}

#[tokio::test]
async fn runtime_budget_compaction_watchdog_refuses_dead_waiter_or_revoked_hold() {
    for revoke in [false, true] {
        let (mut scene, _primary, generation, admission) = claimed().await;
        let capture = admission.capture_held().await.unwrap();
        let waiter = Arc::new(());
        let other = Arc::new(());
        scene
            .service
            .note_compaction_budget_lease("%1", generation, 100, 1000);
        scene
            .service
            .park_budget_compaction_claim(capture.clone(), Arc::downgrade(&waiter), 200)
            .unwrap();
        assert!(
            scene
                .service
                .park_budget_compaction_claim(capture.clone(), Arc::downgrade(&other), 201)
                .is_err()
        );
        if revoke {
            let _ = admission.retry_held().await.unwrap();
        } else {
            drop(waiter);
        }
        assert!(
            !scene
                .service
                .budget_compaction_claim_is_quiescent("%1", generation)
        );
        assert_eq!(
            scene
                .service
                .compaction_budget_timer_delay("%1", generation, 202),
            None
        );
        assert!(
            scene
                .service
                .resume_budget_compaction_claim(&capture, 202)
                .is_err()
        );
    }
}

/// Deliver a genuinely due callback at exhausted committed capacity, recover
/// through the normal timer drain, then prove dead/revoked work still expires.
#[tokio::test]
async fn runtime_budget_compaction_watchdog_pressure_recovers_exact_rearm() {
    use crate::runtime::{
        RuntimeEvent, RuntimeEventBatch, RuntimeTimerKey, RuntimeTimerKind, TimerEvent,
    };
    for resumed in [false, true] {
        let (mut scene, _primary, generation, admission) = claimed().await;
        let capture = admission.capture_held().await.unwrap();
        let waiter = Arc::new(());
        let now = crate::runtime::current_unix_millis();
        scene
            .service
            .note_compaction_budget_lease("%1", generation, now, 1000);
        scene
            .service
            .park_budget_compaction_claim(capture.clone(), Arc::downgrade(&waiter), now + 200)
            .unwrap();
        if resumed {
            scene
                .service
                .resume_budget_compaction_claim(&capture, now + 10_000)
                .unwrap()
                .unwrap();
        }
        let key = RuntimeTimerKey::new(RuntimeTimerKind::CompactionClaim, "%1", generation);
        let service = std::mem::replace(&mut scene.service, super::test_runtime_service());
        let (handle, mut actor) =
            crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
                .unwrap();
        actor.seed_budget_timer_pressure_for_tests(key.clone(), 0);
        let control = async {
            let mut tick = RuntimeEventBatch::new();
            tick.push(RuntimeEvent::Timer(TimerEvent {
                key: key.clone(),
                now_ms: now + 1100,
            }));
            let first = handle.submit_runtime_events(tick).await;
            // Draining one descriptor creates real capacity progress. The
            // retained exact rearm must follow, without resubmitting the event.
            let mut effects = handle.drain_timer_side_effects(1).await.unwrap();
            effects.extend(handle.drain_timer_side_effects(4096).await.unwrap());
            let recovered = effects
                .iter()
                .filter(|effect| {
                    matches!(effect,
                RuntimeSideEffect::ScheduleTimer { key: candidate, .. } if candidate == &key)
                })
                .count();
            // One original schedule was drained plus exactly one new rearm.
            let recovered = recovered == 2;
            if resumed {
                let _ = admission.retry_held().await.unwrap();
            } else {
                drop(waiter);
            }
            let mut dead = RuntimeEventBatch::new();
            dead.push(RuntimeEvent::Timer(TimerEvent {
                key: key.clone(),
                now_ms: now + 20_000,
            }));
            let retired = handle.submit_runtime_events(dead).await;
            let unrelated =
                RuntimeTimerKey::new(RuntimeTimerKind::ResizeDebounce, "healthy-control", 1);
            let healthy = handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                    key: unrelated.clone(),
                    delay_ms: 10,
                }])
                .await;
            let healthy = healthy.is_ok() && handle.drain_timer_side_effects(4096).await.unwrap().iter()
                .any(|effect| matches!(effect, RuntimeSideEffect::ScheduleTimer { key, .. } if key == &unrelated));
            handle.shutdown().await.unwrap();
            (first, recovered, retired, healthy)
        };
        let ((first, recovered, retired, healthy), exit) = tokio::join!(control, actor.run());
        scene.service = exit.service;
        let absent = !scene
            .service
            .agent_compaction_task_is_claimed("%1", generation)
            && !scene
                .service
                .compaction_budget_watchdog_pending("%1", generation);
        drop((handle, capture, admission));
        drop(scene); // Native owner and private root retire before assertions.
        assert_eq!(first.unwrap().applied, 0);
        assert!(
            recovered,
            "watchdog rearm lost or duplicated under pressure"
        );
        assert_eq!(retired.unwrap().applied, 1);
        assert!(absent && healthy);
    }
}
