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
        assert_eq!(actor.drain_timer_side_effects(64).unwrap().len(), occupied);
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

/// Saturated authoritative ingress keeps original dispatch custody until drain.
///
/// More service-owned replies than the whole finite lane can hold must transfer
/// in fitting prefixes. Confirmed executor, mixed, and empty batch shapes all
/// exercise the authoritative path. Only actual lane drains may recover the
/// remainder; no new event or replay regenerates the immutable reply identities.
#[tokio::test(flavor = "current_thread")]
async fn authoritative_service_transfer_recovers_fitting_prefixes_on_lane_drain() {
    for shape in ["confirmed-native", "confirmed-external", "mixed", "empty"] {
        let mut actor = inert_actor(1);
        let capacity = 1 + actor.committed_side_effect_overshoot();
        actor
            .admit_committed_side_effects(
                (0..capacity)
                    .map(|index| timer_effect(&format!("occupied-{index}")))
                    .collect(),
            )
            .unwrap();
        let expected = (0..capacity + 3)
            .map(|index| format!("original-{index}").into_bytes())
            .collect::<Vec<_>>();
        for bytes in &expected {
            actor.service.queue_deferred_pane_input_for_tests(
                RuntimeSideEffect::WritePaneInputPriority {
                    pane_id: "%1".to_string(),
                    bytes: bytes.clone(),
                },
            );
        }
        let mut batch = RuntimeEventBatch::new();
        if shape != "empty" {
            let presentation = mez_agent::ActionPresentationProgress::new(
                "stale-turn",
                "stale-action",
                mez_agent::ActionPresentationExecutionIdentity::Attempt("stale-attempt".into()),
                1,
                mez_agent::ActionPresentationComponentIdentity::confirmed_mutation(0, "file"),
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
        assert_eq!(actor.drain_timer_side_effects(1).unwrap().len(), 1);
        let mut observed = Vec::new();
        for _ in 0..capacity * 3 {
            for effect in actor.drain_runtime_side_effects(1).unwrap() {
                if let RuntimeSideEffect::WritePaneInputPriority { bytes, .. } = effect {
                    observed.push(bytes);
                }
            }
            assert!(
                actor.side_effects.len() + actor.side_effect_routes.non_persistence_len()
                    <= capacity,
                "shape={shape}: transfer exceeded its reserved budget"
            );
        }
        assert_eq!(observed, expected, "shape={shape}");
        assert!(
            actor
                .service
                .drain_pane_io_transition()
                .side_effects
                .is_empty()
        );
        assert!(actor.drain_runtime_side_effects(64).unwrap().is_empty());
    }
}

/// Lease arbitration must progress when all transient slots hold foreign input.
/// Both an acquire consumed without a worker dispatch and a previously active
/// lease must reach their source-owned release. Protected generated input must
/// remain ahead of all foreign input, even when it starts beyond the prefix.
#[tokio::test(flavor = "current_thread")]
async fn bounded_service_transfer_preserves_exact_process_lease_progress() {
    use crate::runtime::{PaneProcessInstance, PaneProcessIoEffect};
    for active_lease in [false, true] {
        for protected_write in [false, true] {
            let mut actor = inert_actor(1);
            let capacity = 1 + actor.committed_side_effect_overshoot();
            let instance = PaneProcessInstance {
                pane_id: "%1".into(),
                generation: 7,
            };
            let (sender, mut worker_watcher) = tokio::sync::watch::channel(0_u64);
            actor
                .pane_process_side_effect_delivery_txs
                .lock()
                .unwrap()
                .insert(instance.clone(), sender);
            let owner = "transaction-owner";
            let effect = |effect| RuntimeSideEffect::PaneProcessIo {
                instance: instance.clone(),
                effect,
            };
            if active_lease {
                actor
                    .pane_input_leases
                    .insert(instance.clone(), owner.into());
            } else {
                actor.service.queue_deferred_pane_input_for_tests(effect(
                    PaneProcessIoEffect::AcquireShellInputLease {
                        owner_id: owner.into(),
                    },
                ));
            }
            let blocked = capacity - usize::from(!active_lease);
            for index in 0..blocked {
                actor.service.queue_deferred_pane_input_for_tests(effect(
                    PaneProcessIoEffect::WriteInput {
                        bytes: vec![index as u8],
                    },
                ));
            }
            if protected_write {
                actor.service.queue_deferred_pane_input_for_tests(effect(
                    PaneProcessIoEffect::WriteShellInput {
                        delivery:
                            mez_mux::process::ShellInputDelivery::generated_source_for_transaction(
                                b"protected".to_vec(),
                                owner,
                            ),
                    },
                ));
            }
            actor.service.queue_deferred_pane_input_for_tests(effect(
                PaneProcessIoEffect::ReleaseShellInputLease {
                    owner_id: owner.into(),
                },
            ));
            actor
                .apply_runtime_event_batch(RuntimeEventBatch::new())
                .await
                .unwrap();
            worker_watcher.borrow_and_update();
            let mut observed = Vec::new();
            for index in 0..capacity + 4 {
                let revision = actor.metrics.side_effect_delivery_notifications;
                let drained = actor
                    .drain_pane_process_io_side_effects(&instance, 1)
                    .unwrap();
                if index == 0 && !protected_write {
                    assert!(drained.is_empty());
                    assert!(
                        worker_watcher.has_changed().unwrap(),
                        "lease-control progress must wake the exact-process watcher"
                    );
                    assert!(
                        actor.metrics.side_effect_delivery_notifications > revision,
                        "internal lease-control progress must wake retained input"
                    );
                }
                observed.extend(drained);
            }
            if protected_write {
                assert!(
                    matches!(
                        &observed[0],
                        RuntimeSideEffect::PaneProcessIo {
                            effect: PaneProcessIoEffect::WriteShellInput { .. },
                            ..
                        }
                    ),
                    "foreign input passed the protected write"
                );
                observed.remove(0);
            }
            assert_eq!(
                observed.len(),
                blocked,
                "active={active_lease} protected={protected_write}"
            );
            for (index, effect) in observed.iter().enumerate() {
                assert!(
                    matches!(effect, RuntimeSideEffect::PaneProcessIo { effect: PaneProcessIoEffect::WriteInput { bytes }, .. } if bytes == &[index as u8])
                );
            }
            assert!(!actor.pane_input_leases.contains_key(&instance));
            assert!(
                actor
                    .service
                    .drain_pane_io_transition()
                    .side_effects
                    .is_empty()
            );
        }
    }
}

/// A source-owned release cannot pass protected input already retained in the
/// actor lane by a one-item worker limit. All three protected writes must be handed
/// off before any older foreign input becomes eligible after lease release.
#[tokio::test(flavor = "current_thread")]
async fn bounded_lease_release_waits_for_older_queued_protected_input() {
    use crate::runtime::{PaneProcessInstance, PaneProcessIoEffect};
    let mut actor = inert_actor(1);
    let capacity = 1 + actor.committed_side_effect_overshoot();
    let instance = PaneProcessInstance {
        pane_id: "%1".into(),
        generation: 8,
    };
    let owner = "queued-protected-owner";
    let effect = |effect| RuntimeSideEffect::PaneProcessIo {
        instance: instance.clone(),
        effect,
    };
    actor.service.queue_deferred_pane_input_for_tests(effect(
        PaneProcessIoEffect::AcquireShellInputLease {
            owner_id: owner.into(),
        },
    ));
    for _ in 0..capacity - 4 {
        actor.service.queue_deferred_pane_input_for_tests(effect(
            PaneProcessIoEffect::WriteInput {
                bytes: b"foreign".to_vec(),
            },
        ));
    }
    for bytes in [b"protected-a", b"protected-b", b"protected-c"] {
        actor.service.queue_deferred_pane_input_for_tests(effect(
            PaneProcessIoEffect::WriteShellInput {
                delivery: mez_mux::process::ShellInputDelivery::generated_source_for_transaction(
                    bytes.to_vec(),
                    owner,
                ),
            },
        ));
    }
    actor.service.queue_deferred_pane_input_for_tests(effect(
        PaneProcessIoEffect::ReleaseShellInputLease {
            owner_id: owner.into(),
        },
    ));
    actor
        .apply_runtime_event_batch(RuntimeEventBatch::new())
        .await
        .unwrap();
    for _ in 0..3 {
        let drained = actor
            .drain_pane_process_io_side_effects(&instance, 1)
            .unwrap();
        assert!(
            matches!(
                &drained[..],
                [RuntimeSideEffect::PaneProcessIo {
                    effect: PaneProcessIoEffect::WriteShellInput { .. },
                    ..
                }]
            ),
            "release passed protected input: {drained:?}"
        );
    }
}

/// Neither a stale process generation nor a mismatched transaction release can
/// unlock foreign input. Once the exact release is source-owned, worker drains
/// alone reduce it and wake the already-admitted blocked lane.
#[tokio::test(flavor = "current_thread")]
async fn bounded_lease_handoff_preserves_owner_and_process_fences() {
    use crate::runtime::{PaneProcessInstance, PaneProcessIoEffect};
    for wrong_generation in [false, true] {
        let mut actor = inert_actor(1);
        let capacity = 1 + actor.committed_side_effect_overshoot();
        let instance = PaneProcessInstance {
            pane_id: "%1".into(),
            generation: 9,
        };
        actor
            .pane_input_leases
            .insert(instance.clone(), "owner".into());
        for _ in 0..capacity {
            actor
                .service
                .queue_deferred_pane_input_for_tests(RuntimeSideEffect::PaneProcessIo {
                    instance: instance.clone(),
                    effect: PaneProcessIoEffect::WriteInput {
                        bytes: b"blocked".to_vec(),
                    },
                });
        }
        actor
            .service
            .queue_deferred_pane_input_for_tests(RuntimeSideEffect::PaneProcessIo {
                instance: PaneProcessInstance {
                    generation: if wrong_generation { 10 } else { 9 },
                    ..instance.clone()
                },
                effect: PaneProcessIoEffect::ReleaseShellInputLease {
                    owner_id: if wrong_generation {
                        "owner"
                    } else {
                        "foreign-owner"
                    }
                    .into(),
                },
            });
        actor
            .apply_runtime_event_batch(RuntimeEventBatch::new())
            .await
            .unwrap();
        assert!(
            actor
                .drain_pane_process_io_side_effects(&instance, 1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            actor.pane_input_leases.get(&instance).map(String::as_str),
            Some("owner")
        );
        actor
            .service
            .queue_deferred_pane_input_for_tests(RuntimeSideEffect::PaneProcessIo {
                instance: instance.clone(),
                effect: PaneProcessIoEffect::ReleaseShellInputLease {
                    owner_id: "owner".into(),
                },
            });
        assert!(
            actor
                .drain_pane_process_io_side_effects(&instance, 1)
                .unwrap()
                .is_empty()
        );
        assert!(!actor.pane_input_leases.contains_key(&instance));
        assert_eq!(
            actor
                .drain_pane_process_io_side_effects(&instance, 1)
                .unwrap()
                .len(),
            1
        );
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

/// A rejected mixed committed vector must not discard independent durable work.
///
/// Fill the entire transient reserve, interleave immutable audit records with a
/// non-droppable timer, and require the original durable bytes in lane order
/// exactly once despite the truthful transient refusal. The producer boundary
/// remains all-or-error: it has not committed its work and may retry it intact.
/// No filesystem operation or worker is executed by this inert fixture.
#[tokio::test(flavor = "current_thread")]
async fn mixed_committed_refusal_preserves_durable_lane_without_producer_bypass() {
    for committed in [false, true] {
        let mut actor = inert_actor(1);
        let capacity = 1 + actor.committed_side_effect_overshoot();
        actor
            .admit_committed_side_effects(
                (0..capacity)
                    .map(|index| timer_effect(&format!("occupied-{index}")))
                    .collect(),
            )
            .unwrap();
        let audit = |bytes: &[u8]| RuntimeSideEffect::PersistAuditLog {
            path: std::env::temp_dir().join("mez-inert-mixed-committed-audit"),
            bytes: bytes.to_vec(),
            retention: crate::security::audit::AuditRetentionPolicy::default(),
        };
        let effects = vec![audit(b"first"), timer_effect("refused"), audit(b"second")];
        let error = if committed {
            actor.admit_committed_side_effects(effects)
        } else {
            actor.queue_runtime_side_effects(effects)
        }
        .expect_err("transient reserve must remain finite");
        assert!(super::super::queue::is_side_effect_queue_full_error(&error));
        // Applied-event wrappers append their count after the admission error.
        // Even a zero-applied wrapper cannot turn partial durable transfer into
        // permission to replay the mixed vector and append its records twice.
        let annotated = MezError::invalid_state(format!("{} applied=0", error.message()));
        assert_eq!(
            super::super::queue::is_retryable_side_effect_queue_full_error(&annotated),
            !committed
        );
        let durable = actor.drain_persistence_side_effects(8).unwrap();
        let observed = durable
            .iter()
            .map(|effect| match effect {
                RuntimeSideEffect::PersistAuditLog { bytes, .. } => bytes.as_slice(),
                other => panic!("unexpected durable effect: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            if committed {
                vec![b"first".as_slice(), b"second".as_slice()]
            } else {
                vec![]
            },
            "committed={committed}"
        );
        assert!(actor.drain_persistence_side_effects(8).unwrap().is_empty());
        let timers = actor.drain_timer_side_effects(capacity + 1).unwrap();
        assert_eq!(timers.len(), capacity);
        assert!(timers.iter().all(|effect| timer_owner(effect) != "refused"));
    }
}

/// Pre-verdict registry replacement is consumed even if no durable item returns.
///
/// Registry coalescing replaces an existing lane item before transient admission
/// decides. The returned overflow vector therefore contains only the timer, but
/// a zero-applied annotation must not authorize replay of that partial transfer.
/// This fixture exercises actual registry coalescing without running storage.
#[tokio::test(flavor = "current_thread")]
async fn coalesced_committed_durable_refusal_is_consumed() {
    use crate::runtime::RuntimeRegistryUpdatePlan;
    let mut actor = inert_actor(1);
    let capacity = 1 + actor.committed_side_effect_overshoot();
    actor
        .admit_committed_side_effects(
            (0..capacity)
                .map(|index| timer_effect(&format!("occupied-{index}")))
                .collect(),
        )
        .unwrap();
    let registry = crate::storage::registry::SessionRegistry::new(
        std::env::temp_dir().join("mez-inert-coalesced-committed-registry"),
        crate::runtime::current_effective_uid(),
    );
    let initial = actor.service.registry_update_plan();
    let RuntimeRegistryUpdatePlan::Upsert(record) = &initial else {
        panic!("live service must create an upsert");
    };
    let session_id = record.session_id.clone();
    actor
        .admit_committed_side_effects(vec![RuntimeSideEffect::PersistRegistry {
            registry: registry.clone(),
            update: initial,
        }])
        .unwrap();
    let error = actor
        .admit_committed_side_effects(vec![
            RuntimeSideEffect::PersistRegistry {
                registry,
                update: RuntimeRegistryUpdatePlan::Remove {
                    session_id: session_id.clone(),
                },
            },
            timer_effect("refused"),
        ])
        .expect_err("transient admission must still refuse");
    let annotated = MezError::invalid_state(format!("{} applied=0", error.message()));
    assert!(
        !super::super::queue::is_retryable_side_effect_queue_full_error(&annotated),
        "coalesced durable replacement was already consumed: {annotated}"
    );
    assert!(matches!(
        actor.drain_persistence_side_effects(8).unwrap().as_slice(),
        [RuntimeSideEffect::PersistRegistry {
            update: RuntimeRegistryUpdatePlan::Remove { session_id: found }, ..
        }] if found == &session_id
    ));
    assert!(actor.drain_persistence_side_effects(8).unwrap().is_empty());
}

/// Mixed refusal installs one held transcript claim and one receipt admission.
///
/// A committed immutable append must enter ordinary receipt fencing even when
/// its accompanying timer cannot fit. Scheduling the receipt twice is inert;
/// the original claim remains undrainable until its exact receipt settles, then
/// delivers the original rows and claim ID once. No file or worker is started.
#[tokio::test(flavor = "current_thread")]
async fn mixed_committed_refusal_retains_exact_transcript_receipt_claim() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};
    let mut actor = inert_actor(1);
    let capacity = 1 + actor.committed_side_effect_overshoot();
    actor
        .admit_committed_side_effects(
            (0..capacity)
                .map(|index| timer_effect(&format!("occupied-{index}")))
                .collect(),
        )
        .unwrap();
    let store = crate::storage::transcript::AgentTranscriptStore::new(
        std::env::temp_dir().join("mez-inert-mixed-committed-transcript"),
    );
    let row = TranscriptEntry {
        conversation_id: "mixed-committed-conversation".into(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".into(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        content: "immutable original row".into(),
    };
    let previous = actor.side_effect_routes.next_transcript_claim_id();
    let error = actor
        .admit_committed_side_effects(vec![
            RuntimeSideEffect::PersistTranscriptEntries {
                path: store.transcript_path(&row.conversation_id).unwrap(),
                store,
                entries: vec![row.clone()],
            },
            timer_effect("refused"),
        ])
        .expect_err("transient admission must still refuse");
    assert!(error.message().contains(" consumed=1"));
    let admissions = actor
        .side_effect_routes
        .hold_transcript_receipts_after(previous);
    assert_eq!(admissions.len(), 1);
    let (id, _, entries) = &admissions[0];
    assert_eq!(entries, &vec![row.clone()]);
    assert!(
        actor
            .side_effect_routes
            .hold_transcript_receipts_after(previous)
            .is_empty()
    );
    assert!(
        actor
            .side_effect_routes
            .drain_persistence_claims(8)
            .is_empty()
    );
    assert!(
        actor
            .side_effect_routes
            .finish_transcript_receipt(*id, true)
    );
    let drained = actor.side_effect_routes.drain_persistence_claims(8);
    assert!(matches!(
        drained.as_slice(),
        [(RuntimeSideEffect::PersistTranscriptEntries { entries, .. }, Some(found))]
            if entries == &vec![row] && found == id
    ));
    assert!(
        actor
            .side_effect_routes
            .drain_persistence_claims(8)
            .is_empty()
    );
}

/// Durable work must transfer even when full transient reserve plus an owed
/// compensation redraw leave no transient slot. The deferred reply stays at its
/// original owner while the immutable audit reaches persistence exactly once.
#[tokio::test(flavor = "current_thread")]
async fn durable_source_transfer_is_independent_of_full_transient_and_redraw_reserve() {
    let mut actor = inert_actor(1);
    let capacity = 1 + actor.committed_side_effect_overshoot();
    actor
        .admit_committed_side_effects(
            (0..capacity)
                .map(|index| timer_effect(&format!("occupied-{index}")))
                .collect(),
        )
        .unwrap();
    actor
        .admit_committed_side_effects(vec![RuntimeSideEffect::RenderClient {
            client_id: mez_core::ids::ClientId::new('c', 9002),
            reason: RenderInvalidationReason::FullRedraw,
        }])
        .unwrap();
    assert!(actor.side_effects.len() + actor.side_effect_routes.non_persistence_len() > capacity);
    actor
        .service
        .queue_deferred_pane_input_for_tests(RuntimeSideEffect::WritePaneInputPriority {
            pane_id: "%1".into(),
            bytes: b"retained".to_vec(),
        });
    actor
        .service
        .queue_transcript_for_tests(RuntimeSideEffect::PersistAuditLog {
            path: std::env::temp_dir().join("mez-inert-transfer-audit"),
            bytes: b"immutable audit".to_vec(),
            retention: crate::security::audit::AuditRetentionPolicy::default(),
        });
    actor
        .apply_runtime_event_batch(RuntimeEventBatch::new())
        .await
        .unwrap();
    let durable = actor.drain_persistence_side_effects(1).unwrap();
    assert!(
        matches!(&durable[..], [RuntimeSideEffect::PersistAuditLog { bytes, .. }] if bytes == b"immutable audit")
    );
    assert!(actor.drain_persistence_side_effects(1).unwrap().is_empty());
    assert!(
        matches!(actor.service.drain_pane_io_transition().side_effects.as_slice(),
        [RuntimeSideEffect::WritePaneInputPriority { bytes, .. }] if bytes == b"retained")
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
