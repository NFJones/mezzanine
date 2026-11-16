//! Original monitored inference waits at native admission, then either sends
//! once after policy release or retires on Stop. No new dispatch is constructed.

use super::*;
use crate::integrations::http::ReqwestProviderHttpTransport;
use futures_util::FutureExt;
use mez_agent::token_budget::TokenLimit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn budget_monitored_original_future_resumes_after_policy_release() {
    qualify(false, false, None).await;
}

#[tokio::test]
async fn budget_monitored_original_future_stop_never_sends_after_release() {
    qualify(true, false, None).await;
}

#[tokio::test]
async fn budget_monitored_original_future_survives_full_fair_queue() {
    qualify(false, true, None).await;
}

#[derive(Clone, Copy)]
enum HumanRelease {
    BudgetFirst,
    HumanFirst,
}

/// Actual attached browser input clears only its own inhibition reason. The
/// same physical producer must outlive both orders without a replacement offer.
#[tokio::test]
async fn budget_monitored_original_future_requires_human_and_budget_release() {
    for order in [HumanRelease::BudgetFirst, HumanRelease::HumanFirst] {
        qualify(false, false, Some(order)).await;
    }
}

pub(super) async fn browser_input(
    handle: &crate::host::async_runtime::AsyncRuntimeSessionHandle,
    primary: &mez_core::ids::ClientId,
    bytes: &[u8],
) {
    use crate::host::terminal::{AttachedTerminalClientStepPlan, TerminalClientLoopAction};
    handle
        .apply_attached_terminal_step_plan(
            primary.clone(),
            AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(bytes.to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .await
        .unwrap();
}

/// Reassemble the sole fixture row's wrapped cells, excluding browser help.
pub(super) async fn assert_browser_paused(
    handle: &crate::host::async_runtime::AsyncRuntimeSessionHandle,
    pane: &str,
    expected: bool,
) {
    let view = handle
        .render_client_view(
            mez_mux::presentation::ClientViewRole::Primary,
            mez_mux::layout::Size::new(120, 40).unwrap(),
            crate::host::terminal::TerminalClientLoopConfig::default(),
        )
        .await
        .unwrap()
        .unwrap();
    let text = view.lines.join("\n");
    assert!(
        text.contains("Live session agents"),
        "agent browser was not mounted: {text}"
    );
    let cell = |column| -> String {
        view.lines
            .iter()
            .skip_while(|line| !line.contains('├'))
            .skip(1)
            .take_while(|line| line.contains('│'))
            .filter_map(|line| line.split('│').nth(column))
            .map(str::trim)
            .collect()
    };
    assert_eq!(
        cell(1),
        format!("agent-{pane}"),
        "wrong browser target: {text}"
    );
    assert_eq!(
        cell(7),
        if expected { "paused" } else { "running" },
        "unexpected original agent pause state: {text}"
    );
}

/// Observe the actual local log, not a synthetic notice predicate. Repeated
/// actor park/read polling must not append another notice to the same hold.
pub(super) async fn assert_budget_notice_once(handle: &AsyncRuntimeSessionHandle) {
    let view = handle
        .render_client_view(
            mez_mux::presentation::ClientViewRole::Primary,
            mez_mux::layout::Size::new(160, 60).unwrap(),
            crate::host::terminal::TerminalClientLoopConfig::default(),
        )
        .await
        .unwrap()
        .unwrap();
    let text = view.lines.join("\n");
    assert_eq!(
        text.matches("session token budget hold").count(),
        1,
        "{text}"
    );
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

async fn qualify(stop: bool, pressure: bool, human: Option<HumanRelease>) {
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
    if human.is_some() {
        // The fixture has no physical pane shell. Qualify the existing native
        // lifecycle target by declaring that process-free pane, not a fake PID.
        service
            .session_mut_for_tests()
            .set_pane_live_state(&dispatch.turn.pane_id, false)
            .unwrap();
    }
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
    let dispatch_pane = dispatch.turn.pane_id.clone();
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
                if human.is_none() && !pressure {
                    assert_budget_notice_once(&handle).await;
                    tokio::time::sleep(Duration::from_millis(350)).await;
                    assert_budget_notice_once(&handle).await;
                }
                if human.is_some() {
                    browser_input(&handle, &primary, b"/list-agents --all\r").await;
                    // The sole row is the original registered agent; verify
                    // its ID and state before using its retained target.
                    assert_browser_paused(&handle, &dispatch_pane, false).await;
                    browser_input(&handle, &primary, b"p").await;
                    assert_browser_paused(&handle, &dispatch_pane, true).await;
                }
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
                if let Some(order) = human {
                    match order {
                        HumanRelease::BudgetFirst => publish(store.clone()).await,
                        HumanRelease::HumanFirst => {
                            browser_input(&handle, &primary, b"p").await;
                            assert_browser_paused(&handle, &dispatch_pane, false).await;
                        }
                    }
                    // Beyond the maximum paced admission poll: lifting only
                    // one reason must not issue HTTP or destroy this worker.
                    tokio::time::sleep(Duration::from_millis(2_100)).await;
                    if worker.as_ref().unwrap().is_finished() {
                        let ended = worker.take().unwrap().await.unwrap();
                        panic!(
                            "worker ended after only one release: sends={}, outcome={:?}",
                            sends.load(Ordering::SeqCst),
                            ended.map(|event| event.is_some())
                        );
                    }
                    assert_eq!(sends.load(Ordering::SeqCst), 0);
                    assert_eq!(rows(&store, "budget_attempts"), 0);
                    match order {
                        HumanRelease::BudgetFirst => {
                            assert_browser_paused(&handle, &dispatch_pane, true).await;
                            browser_input(&handle, &primary, b"p").await;
                            assert_browser_paused(&handle, &dispatch_pane, false).await;
                        }
                        HumanRelease::HumanFirst => publish(store.clone()).await,
                    }
                } else {
                    publish(store.clone()).await;
                }
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
