//! Original monitored inference waits at native admission, then either sends
//! once after policy release or retires on Stop. No new dispatch is constructed.

use super::*;
use crate::integrations::http::ReqwestProviderHttpTransport;
use futures_util::FutureExt;
use mez_agent::token_budget::TokenLimit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn budget_monitored_original_future_resumes_after_policy_release() {
    qualify(false, false).await;
}

#[tokio::test]
async fn budget_monitored_original_future_stop_never_sends_after_release() {
    qualify(true, false).await;
}

#[tokio::test]
async fn budget_monitored_original_future_survives_full_fair_queue() {
    qualify(false, true).await;
}

async fn publish(store: TokenUsageStore) {
    tokio::task::spawn_blocking(move || {
        let projection = store.budget_policy_projection().unwrap();
        let previous = projection.committed.unwrap();
        let operation = BudgetPolicyPublication {
            namespace: projection.namespace,
            id: BudgetReceiptId::new("original-wait-release").unwrap(),
            expected_revision: projection.revision,
            previous_digest: previous.digest,
            proposed: BudgetConfigPolicy {
                source: previous.source,
                digest: "d".repeat(64),
                limits: BudgetLimits {
                    input: TokenLimit::limited(100).unwrap(),
                    output: TokenLimit::Unlimited,
                },
            },
            supersedes: None,
        };
        store.begin_budget_policy_publication(&operation).unwrap();
        store
            .reconcile_budget_policy_publication(&operation, &operation.proposed.digest)
            .unwrap();
    })
    .await
    .unwrap();
}

async fn qualify(stop: bool, pressure: bool) {
    let Prepared {
        mut service,
        mut dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits {
        input: TokenLimit::limited(0).unwrap(),
        output: TokenLimit::Unlimited,
    })
    .await;
    if !stop {
        let deadline = crate::runtime::current_unix_millis() + 3_000;
        service
            .agent_turn_ledger_mut()
            .set_turn_deadline(&dispatch.turn.turn_id, deadline)
            .unwrap();
        dispatch.turn.deadline_at_unix_millis = deadline;
    }
    let primary = service
        .session()
        .clients()
        .iter()
        .find(|client| service.session().is_attached_primary(&client.id))
        .unwrap()
        .id
        .clone();
    let sibling = if pressure {
        service
            .agent_scheduler_mut()
            .set_max_concurrent_agents(1)
            .unwrap();
        service
            .agent_scheduler_mut()
            .set_queue_limits(1, usize::MAX)
            .unwrap();
        let first = service
            .session_mut_for_tests()
            .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
            .unwrap();
        let second = service
            .session_mut_for_tests()
            .split_active_pane(&primary, mez_mux::layout::SplitDirection::Horizontal)
            .unwrap();
        for pane in [&first, &second] {
            service
                .agent_shell_store_mut()
                .enter_or_resume(pane.as_str())
                .unwrap();
            service.set_pane_readiness(pane.as_str(), mez_agent::PaneReadinessState::Ready);
        }
        service
            .execute_terminal_command(&primary, &format!("select-pane -t {first}"))
            .unwrap();
        service
            .execute_agent_shell_command(&primary, "useful sibling work")
            .unwrap();
        let sibling_turn = service
            .agent_turn_ledger()
            .turns()
            .last()
            .unwrap()
            .turn_id
            .clone();
        service
            .execute_terminal_command(&primary, &format!("select-pane -t {second}"))
            .unwrap();
        Some((first, sibling_turn))
    } else {
        None
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    dispatch.provider = crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(
        OpenAiCompatibleChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)
            .unwrap()
            .with_provider_id("local-chat")
            .unwrap()
            .with_streaming_options_for_test(false)
            .unwrap()
            .with_endpoint(format!("http://{}/v1", listener.local_addr().unwrap())),
    );
    let sends = Arc::new(AtomicUsize::new(0));
    let received = sends.clone();
    let writer = store.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let end = loop {
            if let Some(index) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                break index + 4;
            }
            let mut chunk = [0; 4096];
            let read = socket.read(&mut chunk).await.unwrap();
            assert!(read > 0 && bytes.len() + read <= 1024 * 1024);
            bytes.extend_from_slice(&chunk[..read]);
        };
        let length: usize = std::str::from_utf8(&bytes[..end])
            .unwrap()
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap();
        assert!(end + length <= 1024 * 1024);
        while bytes.len() < end + length {
            let mut chunk = [0; 4096];
            let read = socket.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&chunk[..read]);
        }
        received.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            rows(&writer, "budget_attempts"),
            1,
            "original pre-send registration"
        );
        let text = serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"done"}]}).to_string();
        let body = serde_json::json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}],"usage":{"prompt_tokens":11,"completion_tokens":3}}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let turn = dispatch.turn.turn_id.clone();
    let agent = AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap();
    let drains: ProviderObservationDrains = Default::default();
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let mut worker = Some(tokio::spawn(monitor_runtime_agent_provider_dispatch(
        handle.clone(),
        agent,
        turn.clone(),
        dispatch,
        drains.clone(),
    )));
    let control = async {
        let result = {
            let client = async {
                loop {
                    let effects = handle.drain_timer_side_effects(64).await.unwrap();
                    if effects.iter().any(|effect| matches!(effect,
                        crate::runtime::RuntimeSideEffect::CancelTimer { key }
                            if key.kind == crate::runtime::RuntimeTimerKind::ProviderClaim && key.owner_id == turn
                    )) { break; }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                assert_eq!(sends.load(Ordering::SeqCst), 0);
                assert_eq!(rows(&store, "budget_attempts"), 0);
                assert!(handle.agent_turn_is_running(&turn).await.unwrap());
                if !stop {
                    // Exceed the ORIGINAL actor/runner deadline while the same
                    // worker is parked. A fixed wall-clock wrapper fails here.
                    tokio::time::sleep(Duration::from_millis(3_500)).await;
                    assert!(
                        !worker.as_ref().unwrap().is_finished(),
                        "park consumed active allowance"
                    );
                    assert_eq!(sends.load(Ordering::SeqCst), 0);
                }
                if stop {
                    handle
                        .execute_agent_shell_command(primary.clone(), "/stop".into())
                        .await
                        .unwrap();
                }
                if let Some((_, sibling_turn)) = &sibling {
                    assert!(handle.agent_turn_is_running(sibling_turn).await.unwrap());
                    handle
                        .execute_agent_shell_command(primary.clone(), "queued sibling".into())
                        .await
                        .unwrap();
                }
                publish(store.clone()).await;
                if let Some((first, _)) = &sibling {
                    // Beyond the driver's maximum paced poll interval: release
                    // has reached real QueueFull, not merely a parked policy.
                    tokio::time::sleep(Duration::from_millis(2_100)).await;
                    assert!(!worker.as_ref().unwrap().is_finished());
                    assert_eq!(sends.load(Ordering::SeqCst), 0);
                    handle
                        .execute_agent_shell_command(primary.clone(), "/stop".into())
                        .await
                        .unwrap();
                    handle
                        .execute_terminal_command(
                            primary.clone(),
                            format!("select-pane -t {first}"),
                        )
                        .await
                        .unwrap();
                    handle
                        .execute_agent_shell_command(primary.clone(), "/stop".into())
                        .await
                        .unwrap();
                }
                let joined = worker.as_mut().unwrap().await;
                worker = None; // clear consumed JoinHandle before interpreting JoinError
                let event = joined.unwrap().unwrap();
                assert_eq!(event.is_some(), !stop);
                assert!(
                    handle
                        .pending_agent_provider_tasks()
                        .await
                        .unwrap()
                        .is_empty()
                );
            };
            tokio::time::timeout(
                Duration::from_secs(15),
                std::panic::AssertUnwindSafe(client).catch_unwind(),
            )
            .await
        };
        if let Some(worker) = worker.take() {
            worker.abort();
            let _ = worker.await;
        }
        // Keep original drain clones until their exact producer and native
        // bookkeeping are observed; a timeout never authorizes store deletion.
        let retained: Vec<_> = drains.lock().unwrap().values().cloned().collect();
        let cleanup = tokio::time::timeout(Duration::from_secs(10), async {
            for drain in &retained {
                drain.abort.abort();
                drain.completed.observe().await;
                flush_pending_provider_observations(&handle, &drain.pending).await?;
            }
            Ok::<_, MezError>(())
        })
        .await;
        server.abort();
        let served = server.await;
        handle.shutdown().await.unwrap();
        (result, cleanup, served)
    };
    let ((result, cleanup, served), mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    let known = store.budget_account(&member.root).unwrap().known;
    let receipts = rows(&store, "budget_receipts");
    drop((exit, handle, drains, worker));
    if matches!(cleanup, Ok(Ok(()))) {
        root.cleanup();
    }
    cleanup.unwrap().unwrap();
    if let Err(panic) = result.expect("original monitor timed out") {
        std::panic::resume_unwind(panic);
    }
    if stop {
        assert!(served.unwrap_err().is_cancelled());
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        assert_eq!((known, receipts), (UsageTotals::default(), 0));
    } else {
        served.unwrap();
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        assert_eq!(
            (known, receipts),
            (
                UsageTotals {
                    input: 11,
                    output: 3
                },
                2
            )
        );
    }
}
