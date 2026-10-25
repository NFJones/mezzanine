//! Request-side service custody through refusal and ordinary capacity recovery.
//!
//! These tests execute the shared request admission boundary and real typed
//! drains without running a provider, pane process, timer or persistence worker.

use super::super::*;

/// Builds empty inert lanes with the smallest producer capacity. Startup work
/// is drained before seeding accepted source, so reserve arithmetic is exact.
fn actor_at_request_boundary() -> AsyncRuntimeSessionActor {
    let service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
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
    actor
}

/// Full transient reserve must refuse the shared request boundary without
/// destroying unrelated accepted source. Equal input bytes are two occurrences,
/// not coalescible status. Durable work must transfer independently, and actual
/// capacity drains alone must recover every original once. No file is written.
#[tokio::test(flavor = "current_thread")]
async fn request_refusal_retains_original_inputs_and_durable_source() {
    let mut actor = actor_at_request_boundary();
    let capacity = 1 + actor.committed_side_effect_overshoot();
    actor
        .admit_committed_side_effects(
            (0..capacity)
                .map(|index| RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::PanePipeHealth,
                        format!("request-blocker-{index}"),
                        1,
                    ),
                    delay_ms: 25,
                })
                .collect(),
        )
        .unwrap();
    let input = RuntimeSideEffect::WritePaneInput {
        pane_id: "%1".into(),
        bytes: b"request-owned-occurrence".to_vec(),
    };
    actor
        .service
        .queue_deferred_pane_input_for_tests(input.clone());
    actor
        .service
        .queue_deferred_pane_input_for_tests(input.clone());
    let durable = RuntimeSideEffect::Persist {
        target: crate::runtime::PersistenceTarget::Transcript,
        path: std::path::PathBuf::from("request-custody-never-written"),
        bytes: b"original-durable-source".to_vec(),
        mode: crate::runtime::PersistenceWriteMode::Append,
    };
    actor.service.queue_transcript_for_tests(durable.clone());
    let error = actor
        .queue_deferred_pane_io_side_effects_from_service()
        .expect_err("exhausted reserve retains producer pressure feedback");
    let blockers = actor.drain_timer_side_effects(64).unwrap();
    assert_eq!(blockers.len(), capacity);
    let recovered = actor.drain_pane_io_side_effects("%1", 64).unwrap();
    assert_eq!(recovered, vec![input.clone(), input]);
    assert_eq!(
        actor.drain_persistence_side_effects(64).unwrap(),
        vec![durable]
    );
    assert!(
        actor
            .drain_pane_io_side_effects("%1", 64)
            .unwrap()
            .is_empty()
    );
    assert!(actor.drain_persistence_side_effects(64).unwrap().is_empty());
    assert!(error.message().contains("consumed=1"), "{error}");
    assert!(actor.service.pane_processes().is_empty());
}

/// The real terminal request must retain applied/consumed pressure feedback,
/// while an empty read behind the same full reserve must remain usable. Recovery
/// drains original input once, without submitting the command a second time.
#[tokio::test(flavor = "current_thread")]
async fn request_command_pressure_reply_preserves_original_source() {
    let mut actor = actor_at_request_boundary();
    let primary = actor
        .service
        .attach_primary("request-primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let capacity = 1 + actor.committed_side_effect_overshoot();
    actor
        .admit_committed_side_effects(
            (0..capacity)
                .map(|index| RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::PanePipeHealth,
                        format!("command-{index}"),
                        1,
                    ),
                    delay_ms: 25,
                })
                .collect(),
        )
        .unwrap();
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ExecuteTerminalCommand {
            primary_client_id: primary.clone(),
            input: "show-metrics".into(),
            reply,
        })
        .await;
    assert!(
        response.await.unwrap().is_ok(),
        "empty source is not pressure"
    );
    let input = RuntimeSideEffect::WritePaneInput {
        pane_id: "%1".into(),
        bytes: b"command-original".to_vec(),
    };
    actor
        .service
        .queue_deferred_pane_input_for_tests(input.clone());
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ExecuteTerminalCommand {
            primary_client_id: primary,
            input: "show-metrics".into(),
            reply,
        })
        .await;
    let error = response
        .await
        .unwrap()
        .expect_err("retained source reports pressure");
    assert!(error.message().contains("consumed=1"), "{error}");
    assert_eq!(actor.drain_timer_side_effects(64).unwrap().len(), capacity);
    assert_eq!(
        actor.drain_pane_io_side_effects("%1", 64).unwrap(),
        vec![input]
    );
    assert!(
        actor
            .drain_pane_io_side_effects("%1", 64)
            .unwrap()
            .is_empty()
    );
}

/// Nine existing divider timers require eighteen inseparable cancel/schedule
/// outputs. One free slot cannot consume any intent or generation. Once real
/// capacity drains, the seventeen-slot reserve transfers sixteen outputs then
/// the final pair, without splitting a client or raising the configured bound.
#[tokio::test(flavor = "current_thread")]
async fn request_divider_refill_preserves_atomic_pairs_and_generation() {
    let mut actor = actor_at_request_boundary();
    let mut clients = Vec::new();
    for index in 0..9 {
        let client = actor
            .service
            .attach_primary(
                format!("divider-{index}"),
                true,
                Size::new(80, 24).unwrap(),
                120,
            )
            .unwrap();
        actor.timers.resize_debounce.insert(
            client.to_string(),
            RuntimeTimerKey::new(RuntimeTimerKind::ResizeDebounce, client.as_str(), 40),
        );
        actor
            .service
            .queue_divider_debounce_for_tests(client.clone());
        clients.push(client);
    }
    actor.timers.next_resize_debounce_generation = 40;
    let capacity = 1 + actor.committed_side_effect_overshoot();
    actor
        .admit_committed_side_effects(
            (0..capacity - 1)
                .map(|index| RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::PanePipeHealth,
                        format!("divider-blocker-{index}"),
                        1,
                    ),
                    delay_ms: 25,
                })
                .collect(),
        )
        .unwrap();
    assert_eq!(actor.queue_fitting_divider_resize_debounce().unwrap(), 0);
    assert_eq!(actor.timers.next_resize_debounce_generation, 40);
    assert!(
        actor
            .service
            .next_divider_resize_debounce_request()
            .is_some()
    );
    assert_eq!(
        actor.drain_timer_side_effects(64).unwrap().len(),
        capacity - 1
    );
    assert_eq!(actor.timers.next_resize_debounce_generation, 48);
    assert!(
        actor
            .service
            .next_divider_resize_debounce_request()
            .is_some()
    );
    let first = actor.drain_timer_side_effects(64).unwrap();
    assert_eq!(first.len(), 16);
    let second = actor.drain_timer_side_effects(64).unwrap();
    assert_eq!(second.len(), 2);
    let effects = first.into_iter().chain(second).collect::<Vec<_>>();
    let (pairs, remainder) = effects.as_chunks::<2>();
    assert!(remainder.is_empty());
    for pair in pairs {
        let [
            RuntimeSideEffect::CancelTimer { key: old },
            RuntimeSideEffect::ScheduleTimer { key: new, .. },
        ] = pair
        else {
            panic!("divider transfer must contain whole ordered pairs: {pair:?}");
        };
        assert_eq!(old.owner_id, new.owner_id);
        assert_eq!(old.generation, 40);
        assert!(new.generation > 40);
    }
    assert_eq!(actor.timers.next_resize_debounce_generation, 49);
    assert!(
        actor
            .service
            .next_divider_resize_debounce_request()
            .is_none()
    );
    assert_eq!(actor.timers.resize_debounce.len(), clients.len());
    assert!(actor.drain_timer_side_effects(64).unwrap().is_empty());
}

/// A real request enrolls a required append receipt even when unrelated source
/// forces its consumed refusal. With no pressure it replies successfully only
/// after that same durable enrollment. The real actor/receipt worker runs, but
/// no inference, native process or persistence executor runs. Reopening twice
/// recovers one exact original row, never a replay of the command or append.
#[tokio::test(flavor = "current_thread")]
async fn request_required_append_receipt_is_exact_under_pressure() {
    for (pressure, request) in [(false, true), (true, true), (true, false)] {
        let mut actor = actor_at_request_boundary();
        let primary = actor
            .service
            .attach_primary("receipt-primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "mez-request-custody-{}",
            crate::storage::token_usage::new_token_usage_event_id(),
        ));
        let store = crate::storage::transcript::AgentTranscriptStore::new(root.clone());
        let row = mez_agent::transcript::TranscriptEntry {
            conversation_id: "request-receipt".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "original-turn".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: "original-required-row".into(),
        };
        if pressure {
            let capacity = 1 + actor.committed_side_effect_overshoot();
            actor
                .admit_committed_side_effects(
                    (0..capacity)
                        .map(|index| RuntimeSideEffect::ScheduleTimer {
                            key: RuntimeTimerKey::new(
                                RuntimeTimerKind::PanePipeHealth,
                                format!("receipt-blocker-{index}"),
                                1,
                            ),
                            delay_ms: 25,
                        })
                        .collect(),
                )
                .unwrap();
            actor
                .service
                .queue_deferred_pane_input_for_tests(RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".into(),
                    bytes: b"accepted-before-refusal".to_vec(),
                });
        }
        actor
            .service
            .queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
                path: store.transcript_path(&row.conversation_id).unwrap(),
                store: store.clone(),
                entries: vec![row.clone()],
            });
        let (reply, mut response) = tokio::sync::oneshot::channel();
        if request {
            actor
                .handle_request(AsyncRuntimeRequest::ExecuteTerminalCommand {
                    primary_client_id: primary,
                    input: "show-metrics".into(),
                    reply,
                })
                .await;
            assert!(
                response.try_recv().is_err(),
                "reply awaits real receipt admission"
            );
        } else {
            drop(reply);
            assert!(!actor.drain_timer_side_effects(1).unwrap().is_empty());
        }
        // The constructor's handle was intentionally not retained by the inert
        // helper; use the actor's exact sender to settle then stop its live loop.
        let sender = actor.sender.clone();
        let client = async {
            if request {
                let result = (&mut response).await.unwrap();
                assert_eq!(result.is_err(), pressure);
                if let Err(error) = result {
                    assert!(error.message().contains("consumed=1"), "{error}");
                }
            }
            // A typed persistence offer is impossible while its original
            // receipt is Pending/Held. Drain-only recovery must settle it too.
            let offered = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let (reply, response) = tokio::sync::oneshot::channel();
                    sender.send(AsyncRuntimeRequestEnvelope::new(
                        AsyncRuntimeRequest::DrainPersistenceSideEffects { limit: 64, reply },
                    )).await.unwrap();
                    let effects = response.await.unwrap().unwrap();
                    if let Some(effect) = effects.into_iter().find(|effect| matches!(effect,
                        RuntimeSideEffect::PersistTranscriptEntries { entries, .. } if entries == &vec![row.clone()])) {
                        break effect;
                    }
                    tokio::task::yield_now().await;
                }
            }).await.expect("receipt becomes ready without another command");
            assert!(
                matches!(offered, RuntimeSideEffect::PersistTranscriptEntries { entries, .. } if entries == vec![row.clone()])
            );
            assert_eq!(
                store
                    .pending_append_receipts()
                    .unwrap()
                    .iter()
                    .flatten()
                    .filter(|entry| *entry == &row)
                    .count(),
                1
            );
            let (reply, response) = tokio::sync::oneshot::channel();
            sender
                .send(AsyncRuntimeRequestEnvelope::new(
                    AsyncRuntimeRequest::Shutdown { reply },
                ))
                .await
                .unwrap();
            response.await.unwrap().unwrap();
        };
        let (_, exit) = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            tokio::join!(client, actor.run())
        })
        .await
        .expect("bounded receipt worker settlement");
        assert!(exit.service.pane_processes().is_empty());
        drop(exit);
        let reopened = crate::storage::transcript::AgentTranscriptStore::new(root.clone());
        reopened.recover_append_receipts().unwrap();
        assert_eq!(
            reopened.inspect(&row.conversation_id).unwrap(),
            vec![row.clone()]
        );
        reopened.recover_append_receipts().unwrap();
        assert_eq!(reopened.inspect(&row.conversation_id).unwrap(), vec![row]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
