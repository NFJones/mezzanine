//! Committed-effect admission above the configured lane capacity.
//!
//! Event application is irreversible, so the effects a batch already produced
//! must reach their ordinary lanes even when that momentarily exceeds the
//! capacity that bounds producer-submitted work. These tests construct inert
//! actor state and never run its loop, timer, pane or persistence workers.

use super::super::*;

/// Returns one exact timer effect owned by the non-persistence timer lane.
fn timer_effect(owner: &str) -> RuntimeSideEffect {
    RuntimeSideEffect::ScheduleTimer {
        key: RuntimeTimerKey::new(RuntimeTimerKind::PanePipeHealth, owner, 1),
        delay_ms: 25,
    }
}

/// Returns the exact owner of one scheduled timer effect.
fn timer_owner(effect: &RuntimeSideEffect) -> &str {
    match effect {
        RuntimeSideEffect::ScheduleTimer { key, .. } => key.owner_id.as_str(),
        other => panic!("expected one scheduled timer, found {other:?}"),
    }
}

/// Builds inert actor state with an exact lane capacity and empty lanes.
fn inert_actor(capacity: usize) -> AsyncRuntimeSessionActor {
    let service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let (_, mut actor) = AsyncRuntimeSessionActor::new(
        service,
        AsyncRuntimeActorConfig {
            side_effect_buffer: capacity,
            ..Default::default()
        },
    )
    .unwrap();
    // Construction may schedule owner maintenance timers. Drain every lane so
    // the capacity arithmetic below is exact and independent of startup policy.
    assert!(actor.drain_runtime_side_effects(64).unwrap().is_empty());
    assert!(actor.drain_timer_side_effects(64).unwrap().is_empty());
    assert!(actor.drain_persistence_side_effects(64).unwrap().is_empty());
    actor
}

/// Provisional executor ingress must not take unrelated service-owned replies.
///
/// The lane and its committed reserve are both full. Stale preview identities
/// deliberately have no execution authority: they must remain inert rather than
/// draining a terminal reply into a rejected batch. The original reply remains
/// available exactly once from its existing service owner after capacity drains.
/// No process, timer worker, or persistence worker is started by this fixture.
#[tokio::test(flavor = "current_thread")]
async fn provisional_executor_progress_preserves_service_custody() {
    for native in [true, false] {
        let mut actor = inert_actor(1);
        let occupied = 1 + actor.committed_side_effect_overshoot();
        actor
            .admit_committed_side_effects(
                (0..occupied)
                    .map(|index| timer_effect(&format!("occupied-{index}")))
                    .collect(),
            )
            .unwrap();
        actor.service.queue_deferred_pane_input_for_tests(
            RuntimeSideEffect::WritePaneInputPriority {
                pane_id: "%1".to_string(),
                bytes: b"\x1b[3;5R".to_vec(),
            },
        );
        let presentation = mez_agent::ActionPresentationProgress::new(
            "stale-turn",
            "stale-action",
            mez_agent::ActionPresentationExecutionIdentity::Attempt("stale-attempt".into()),
            1,
            if native {
                mez_agent::ActionPresentationComponentIdentity::ShellOutput
            } else {
                mez_agent::ActionPresentationComponentIdentity::ProvisionalReadBody
            },
            "preview",
        );
        let mut batch = RuntimeEventBatch::new();
        batch.push(if native {
            RuntimeEvent::NativeShellProgress(crate::runtime::RuntimeNativeShellProgress {
                presentation,
            })
        } else {
            RuntimeEvent::ApprovedExternalActionProgress(
                crate::runtime::RuntimeApprovedExternalActionProgress { presentation },
            )
        });
        let report = actor.apply_runtime_event_batch(batch).await.unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(actor.drain_timer_side_effects(64).unwrap().len(), occupied);
        let original = actor.service.drain_pane_io_transition().side_effects;
        assert_eq!(original.len(), 1, "native={native}: {original:?}");
        assert!(matches!(
            &original[0],
            RuntimeSideEffect::WritePaneInputPriority { bytes, .. }
                if bytes == b"\x1b[3;5R"
        ));
        assert!(
            actor
                .service
                .drain_pane_io_transition()
                .side_effects
                .is_empty()
        );
    }
}

/// Confirmed evidence, mixed batches, and empty drains remain authoritative.
///
/// Only an entirely provisional nonempty batch may leave service work untouched.
/// With available capacity, each other shape must transfer the original deferred
/// dispatch exactly once through its ordinary lane even for stale event owners.
/// This guards the ownership boundary without executing synthetic dispatches.
#[tokio::test(flavor = "current_thread")]
async fn authoritative_progress_shapes_keep_service_transfer() {
    for shape in ["confirmed-native", "confirmed-external", "mixed", "empty"] {
        let mut actor = inert_actor(1);
        actor.service.queue_deferred_pane_input_for_tests(
            RuntimeSideEffect::WritePaneInputPriority {
                pane_id: "%1".to_string(),
                bytes: b"original-reply".to_vec(),
            },
        );
        let mut batch = RuntimeEventBatch::new();
        if shape != "empty" {
            let presentation = mez_agent::ActionPresentationProgress::new(
                "stale-turn",
                "stale-action",
                mez_agent::ActionPresentationExecutionIdentity::Attempt("stale-attempt".into()),
                1,
                if shape == "mixed" {
                    mez_agent::ActionPresentationComponentIdentity::ShellOutput
                } else {
                    mez_agent::ActionPresentationComponentIdentity::confirmed_mutation(0, "file")
                },
                "evidence",
            );
            batch.push(if shape == "confirmed-external" {
                RuntimeEvent::ApprovedExternalActionProgress(
                    crate::runtime::RuntimeApprovedExternalActionProgress { presentation },
                )
            } else {
                RuntimeEvent::NativeShellProgress(crate::runtime::RuntimeNativeShellProgress {
                    presentation,
                })
            });
        }
        if shape == "mixed" {
            batch.push(RuntimeEvent::Timer(TimerEvent {
                key: RuntimeTimerKey::new(RuntimeTimerKind::PanePipeHealth, "stale", 1),
                now_ms: 1,
            }));
        }
        actor.apply_runtime_event_batch(batch).await.unwrap();
        assert!(
            actor
                .service
                .drain_pane_io_transition()
                .side_effects
                .is_empty()
        );
        let delivered = actor.drain_runtime_side_effects(64).unwrap();
        assert_eq!(delivered.len(), 1, "shape={shape}: {delivered:?}");
        assert!(matches!(
            &delivered[0],
            RuntimeSideEffect::WritePaneInputPriority { bytes, .. }
                if bytes == b"original-reply"
        ));
        assert!(actor.drain_runtime_side_effects(64).unwrap().is_empty());
    }
}

/// Committed effects reach their ordinary lane instead of being dropped.
///
/// Producer-submitted work still observes the strict capacity verdict, while the
/// same effects are admitted for an already-committed caller. Because they enter
/// the normal ordered lane immediately, timer ownership and delivery observe the
/// committed effect once and behind the older queued work.
#[tokio::test(flavor = "current_thread")]
async fn committed_effects_admit_above_capacity_without_loss() {
    let mut actor = inert_actor(1);
    actor
        .queue_runtime_side_effects(vec![timer_effect("occupant")])
        .unwrap();
    let refused = actor
        .queue_runtime_side_effects(vec![timer_effect("producer")])
        .expect_err("producer work keeps the strict capacity verdict");
    assert!(
        refused
            .message()
            .starts_with("async runtime side-effect queue is full"),
        "{}",
        refused.message()
    );
    actor
        .admit_committed_side_effects(vec![timer_effect("committed")])
        .unwrap();
    assert_eq!(actor.metrics.runtime_side_effects_admitted_over_capacity, 1);
    let drained = actor.drain_timer_side_effects(8).unwrap();
    assert_eq!(
        drained.iter().map(timer_owner).collect::<Vec<_>>(),
        vec!["occupant", "committed"]
    );
    assert!(actor.drain_timer_side_effects(8).unwrap().is_empty());
}

/// The committed overshoot is bounded and reports the refusal when exhausted.
///
/// Overshoot must not become unbounded buffering: a committed batch beyond the
/// effective capacity is refused, the refusal is attributed, and the queued work
/// that was already admitted is left untouched.
#[tokio::test(flavor = "current_thread")]
async fn committed_overshoot_is_bounded_and_reports_the_refusal() {
    let mut actor = inert_actor(1);
    actor
        .queue_runtime_side_effects(vec![timer_effect("occupant")])
        .unwrap();
    let overshoot = actor.committed_side_effect_overshoot();
    let refused = (0..=overshoot)
        .map(|index| timer_effect(&format!("overflow-{index}")))
        .collect::<Vec<_>>();
    let error = actor
        .admit_committed_side_effects(refused)
        .expect_err("committed work beyond the overshoot must not be accepted");
    assert!(
        error
            .message()
            .starts_with("async runtime side-effect queue is full"),
        "{}",
        error.message()
    );
    assert_eq!(actor.metrics.runtime_side_effects_admitted_over_capacity, 0);
    let drained = actor.drain_timer_side_effects(8).unwrap();
    assert_eq!(drained.len(), 1);
    assert_eq!(timer_owner(&drained[0]), "occupant");
    assert!(actor.drain_timer_side_effects(8).unwrap().is_empty());
}

/// Durable persistence is admissible while transient occupancy is above nominal.
///
/// Committed admission can legitimately leave non-droppable transient effects
/// above the configured capacity. A mandatory registry persistence effect must
/// still be admitted there: durable work is excluded from the transient verdict,
/// so only the effective reserve decides. Producer-submitted work keeps refusing
/// so the pre-production backpressure contract is unchanged.
#[tokio::test(flavor = "current_thread")]
async fn committed_persistence_admits_while_transient_occupancy_exceeds_capacity() {
    let mut actor = inert_actor(1);
    actor
        .admit_committed_side_effects(vec![timer_effect("first")])
        .unwrap();
    actor
        .admit_committed_side_effects(vec![timer_effect("second")])
        .unwrap();
    assert_eq!(actor.metrics.runtime_side_effects_admitted_over_capacity, 1);
    assert!(
        actor
            .queue_runtime_side_effects(vec![timer_effect("producer")])
            .is_err()
    );
    // One durable audit append stands for the mandatory persistence class: it is
    // excluded from the transient verdict but must still be admissible here.
    let persistence = vec![RuntimeSideEffect::PersistAuditLog {
        path: std::env::temp_dir().join("mez-overshoot-audit.jsonl"),
        bytes: b"{\"event\":\"overshoot\"}\n".to_vec(),
        retention: crate::security::audit::AuditRetentionPolicy::default(),
    }];
    actor.admit_committed_side_effects(persistence).unwrap();
    assert_eq!(actor.metrics.runtime_side_effects_admitted_over_capacity, 2);
    let drained = actor.drain_persistence_side_effects(8).unwrap();
    assert!(
        drained
            .iter()
            .any(|effect| matches!(effect, RuntimeSideEffect::PersistAuditLog { .. })),
        "durable persistence must reach its lane: {drained:?}"
    );
}

/// Coalescing-only committed admissions never register as reserve use.
///
/// The counter is post-coalescing: repeating the same client's render request
/// merges into the pending invalidation, installs no new queued work, and must
/// not be reported as committed work admitted above the configured capacity.
#[tokio::test(flavor = "current_thread")]
async fn coalesced_committed_admissions_do_not_count_as_reserve_use() {
    let mut actor = inert_actor(1);
    actor
        .admit_committed_side_effects(vec![timer_effect("first")])
        .unwrap();
    actor
        .admit_committed_side_effects(vec![timer_effect("second")])
        .unwrap();
    assert_eq!(actor.metrics.runtime_side_effects_admitted_over_capacity, 1);
    let client_id = mez_core::ids::ClientId::new('c', 9001);
    let render = || RuntimeSideEffect::RenderClient {
        client_id: client_id.clone(),
        reason: crate::runtime::RenderInvalidationReason::FullRedraw,
    };
    actor.admit_committed_side_effects(vec![render()]).unwrap();
    assert_eq!(actor.metrics.runtime_side_effects_admitted_over_capacity, 2);
    for _ in 0..3 {
        actor.admit_committed_side_effects(vec![render()]).unwrap();
    }
    assert_eq!(actor.metrics.runtime_side_effects_admitted_over_capacity, 2);
}
