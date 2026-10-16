//! Real timer-service and supervisor acceptance for receive bookkeeping fences.
//!
//! A claimed chronology candidate remains owned while ordinary mail enters the
//! serialized actor. The production timer service must not fail supervision,
//! and its existing settlement wake must later admit the same envelope once.

use super::super::*;
use crate::host::async_runtime::{AsyncRuntimeRequest, AsyncRuntimeRequestEnvelope};
use mez_agent::messaging::{Envelope, MessageScope, Recipient};

/// The actual timer worker submits a peer-delivery event while one idle pane
/// retains a genuine claimed interruption candidate. Its unrelated recipient,
/// control ingress, actor and live PTYs remain usable under the real service
/// supervisor. Checked completion wakes the quiet deferred recipient through
/// the existing timer, without another envelope or action/provider replay.
#[tokio::test]
async fn async_peer_bookkeeping_deferral_preserves_timer_supervisor_and_panes() {
    let store = AgentTranscriptStore::new(std::env::temp_dir().join(format!(
        "mez-peer-bookkeeping-service-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    )));
    let mut service = test_service();
    service.set_agent_transcript_store(store);
    service.use_transcript_effect_adapter();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 1)
        .unwrap();
    let command = "/bin/sh -c 'stty -echo && printf \"peer-ready\\n\" && exec cat'";
    service.start_initial_pane_process(Some(command)).unwrap();
    service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some(command))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let stopped = service
        .start_agent_prompt_turn("%1", "retain interrupted chronology")
        .unwrap();
    service.stop_agent_turn_for_pane("%1").unwrap();
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    let now = crate::runtime::current_unix_millis();
    accept_service_peer(&mut service, "%1", now);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%2")
        .unwrap();
    accept_service_peer(&mut service, "%2", now);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let actor_task = tokio::spawn(actor.run());
    // Clear unrelated initial timer effects; install one exact actor-owned
    // delivery timer with a short test delay, never a changed product cadence.
    handle.drain_timer_side_effects(64).await.unwrap();
    handle
        .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
            key: RuntimeTimerKey::new(
                RuntimeTimerKind::PeerMessageDelivery,
                "peer-message-delivery",
                100,
            ),
            delay_ms: 1,
        }])
        .await
        .unwrap();
    let (deferred_tx, deferred_rx) = tokio::sync::oneshot::channel();
    let (settle_tx, settle_rx) = tokio::sync::oneshot::channel();
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let timer_handle = handle.clone();
    let shutdown_handle = handle.clone();
    let supervision = tokio::spawn(async move {
        let result = supervise_async_runtime_services(
            vec![
                AsyncRuntimeService::new_auxiliary("peer-delivery-timers", async move {
                    let config = AsyncRuntimeSideEffectServiceConfig {
                        max_polls: 4,
                        drain_limit: 64,
                        idle_interval: Duration::from_millis(1),
                    };
                    let first = run_async_runtime_timer_side_effect_service(
                        &timer_handle,
                        config,
                        now,
                        |_, _| false,
                    )
                    .await?;
                    let _ = deferred_tx.send(first);
                    let _ = settle_rx.await;
                    let second = run_async_runtime_timer_side_effect_service(
                        &timer_handle,
                        config,
                        crate::runtime::current_unix_millis(),
                        |_, _| false,
                    )
                    .await?;
                    let fired = second.fired;
                    let _ = delivered_tx.send(second);
                    Ok(AsyncRuntimeServiceExit::completed(fired))
                }),
                AsyncRuntimeService::new("live-session-lifetime", async move {
                    let _ = stop_rx.await;
                    Ok(AsyncRuntimeServiceExit::completed(0))
                }),
            ],
            std::future::pending(),
        )
        .await;
        // This is the SessionRuntime coupling: any service-fatal error shuts
        // down the actor, so subsequent control/PTY proofs cannot pass on a
        // cached supervisor status after a timer service failure.
        let _ = shutdown_handle.shutdown().await;
        result
    });
    let first = tokio::time::timeout(Duration::from_secs(5), deferred_rx)
        .await
        .unwrap()
        .expect("timer must defer without failing supervised service");
    assert!(first.fired >= 1);
    assert_eq!(
        handle.lifecycle_state().await.unwrap(),
        RuntimeLifecycleState::Running
    );
    assert!(!supervision.is_finished());
    let tasks = handle.pending_agent_provider_tasks().await.unwrap();
    assert_eq!(
        tasks
            .iter()
            .filter(|task| task.agent_id == "agent-%1")
            .count(),
        0
    );
    assert_eq!(
        tasks
            .iter()
            .filter(|task| task.agent_id == "agent-%2")
            .count(),
        1
    );
    let initialized = handle.handle_control_input_for_connection(
        crate::control::encode_control_body(r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":2,"client_name":"bookkeeping-survivor","client":{"name":"bookkeeping-survivor","interactive":true,"terminal":{"columns":100,"rows":30,"term":"xterm"}}}}"#),
        1024 * 1024, ControlConnectionState::new(true, true),
    ).await.unwrap();
    let shown = handle.handle_control_input_for_connection(
        crate::control::encode_control_body(r#"{"jsonrpc":"2.0","id":"show","method":"agent/shell/show","params":{"target":{"pane_id":"%2"},"idempotency_key":"bookkeeping-live-show"}}"#),
        1024 * 1024, initialized.connection,
    ).await.unwrap();
    let (body, _) = crate::control::decode_control_frame(&shown.output, 1024 * 1024).unwrap();
    assert!(body.contains(r#""visible":true"#), "{body}");
    prove_live_peer_panes(&handle).await;
    let duplicate = work.clone();
    let history = work.execute();
    handle
        .sender
        .send(AsyncRuntimeRequestEnvelope::new(
            AsyncRuntimeRequest::CompleteBookkeepingCandidate { work, history },
        ))
        .await
        .unwrap();
    // FIFO request proves completion adoption before resuming the timer worker.
    assert_eq!(
        handle.lifecycle_state().await.unwrap(),
        RuntimeLifecycleState::Running
    );
    settle_tx.send(()).unwrap();
    let second = tokio::time::timeout(Duration::from_secs(5), delivered_rx)
        .await
        .unwrap()
        .expect("checked settlement must wake retained delivery");
    assert!(second.fired >= 1, "{second:?}");
    let tasks = handle.pending_agent_provider_tasks().await.unwrap();
    assert_eq!(
        tasks
            .iter()
            .filter(|task| task.agent_id == "agent-%1")
            .count(),
        1
    );
    assert!(!tasks.iter().any(|task| task.turn_id == stopped.turn_id));
    handle
        .sender
        .send(AsyncRuntimeRequestEnvelope::new(
            AsyncRuntimeRequest::CompleteBookkeepingCandidate {
                work: duplicate,
                history: Ok((Vec::new(), 1)),
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        handle.lifecycle_state().await.unwrap(),
        RuntimeLifecycleState::Running
    );
    assert!(!supervision.is_finished());
    stop_tx.send(()).unwrap();
    assert!(!supervision.await.unwrap().unwrap().shutdown_requested);
    let mut exit = actor_task.await.unwrap();
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| turn.agent_id == "agent-%1")
            .count(),
        2
    );
    let recipient = AgentId::opaque("agent-%1").unwrap();
    let sequence = exit
        .service
        .message_service_mut()
        .subscription(&recipient)
        .unwrap()
        .last_sequence;
    assert!(sequence > 0);
    assert_eq!(
        exit.service
            .deliver_pending_runtime_agent_messages(crate::runtime::current_unix_millis())
            .unwrap(),
        0
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Accepts ordinary model-originated mail, not a runtime lifecycle bridge.
fn accept_service_peer(service: &mut RuntimeSessionService, pane: &str, now: u64) {
    let recipient = service
        .ensure_runtime_message_identity(
            &format!("agent-{pane}"),
            mez_core::ids::PaneId::opaque(pane),
            "agent",
            &[],
            now,
        )
        .unwrap();
    service
        .message_service_mut()
        .subscribe_from_retained_start(&recipient.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-service-sender", None, "agent", &[], now)
        .unwrap();
    service
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: format!("service-receive-{pane}"),
                message_type: "send".into(),
                time: format!("runtime:{now}"),
                sender: sender.clone(),
                recipient: Recipient::Agent(recipient.agent_id),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".into(),
                payload: "retained ordinary peer evidence".into(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now,
        )
        .unwrap();
}

/// Proves actual native children remain responsive after the deferred sweep.
/// Echo is disabled before readiness, so the returned marker is cat output,
/// not local terminal echo or a cached pane image. Processes are retired only
/// after this liveness proof, without modifying the actor's conversation owner.
async fn prove_live_peer_panes(handle: &crate::host::async_runtime::AsyncRuntimeSessionHandle) {
    let mut processes = handle
        .take_running_pane_process_instances_for_adapter(8)
        .await
        .unwrap();
    assert_eq!(processes.len(), 2);
    for (instance, process) in &mut processes {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut output = Vec::new();
            loop {
                output.extend(process.read_available_output(4096).unwrap());
                if String::from_utf8_lossy(&output).contains("peer-ready") {
                    break;
                }
                assert!(process.poll_exit().unwrap().is_none());
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let marker = format!("bookkeeping-live-{}", instance.pane_id);
        process
            .write_input(format!("{marker}\n").as_bytes())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut output = Vec::new();
            loop {
                output.extend(process.read_available_output(4096).unwrap());
                if String::from_utf8_lossy(&output).contains(&marker) {
                    break;
                }
                assert!(process.poll_exit().unwrap().is_none());
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        process.terminate(Duration::from_millis(10)).unwrap();
    }
}
