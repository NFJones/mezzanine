//! Three original held monitors: genuine child inference, root compaction and
//! title generation. Native reconsideration gates precede physical HTTP, so a
//! sequential server cannot accidentally establish the serialization oracle.
//! This is finite-set held-request progress, not fresh-arrival starvation proof.

use super::*;
use futures_util::FutureExt;

#[tokio::test]
async fn budget_reconsideration_combined_descendant_compactor_title_finite_progress() {
    Box::pin(qualify(true, false)).await;
}

#[tokio::test]
async fn budget_reconsideration_combined_descendant_compactor_title_unlimited_overlap() {
    Box::pin(qualify(false, false)).await;
}

#[tokio::test]
async fn budget_reconsideration_combined_independent_root_progresses_while_root_held() {
    Box::pin(qualify(true, true)).await;
}

/// Each endpoint owns exactly one complete HTTP exchange. Distinct servers
/// allow independent roots and unlimited requests to make genuine progress.
pub(super) async fn endpoint(
    title: bool,
    sends: Arc<AtomicUsize>,
) -> (
    crate::runtime::RuntimeAgentProviderDispatchProvider,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider =
        OpenAiCompatibleChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)
            .unwrap()
            .with_provider_id("local-chat")
            .unwrap()
            .with_streaming_options_for_test(false)
            .unwrap()
            .with_endpoint(format!("http://{}/v1", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 4096];
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0 && bytes.len() + n <= 1024 * 1024);
            bytes.extend_from_slice(&chunk[..n]);
            if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                let length: usize = std::str::from_utf8(&bytes[..end])
                    .unwrap()
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                assert!(end + 4 + length <= 1024 * 1024);
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        sends.fetch_add(1, Ordering::SeqCst);
        let content = if title {
            "Combined title".to_owned()
        } else {
            serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"original combined result"}]}).to_string()
        };
        let body = serde_json::json!({"id":"combined","choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":3,"total_tokens":14}}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    (
        crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider),
        server,
    )
}

type Gate = Arc<(tokio::sync::Notify, tokio::sync::Notify)>;

async fn next_gate(gates: &[Gate], seen: &[bool; 3]) -> usize {
    tokio::select! {
        _ = gates[0].0.notified(), if !seen[0] => 0,
        _ = gates[1].0.notified(), if !seen[1] => 1,
        _ = gates[2].0.notified(), if !seen[2] => 2,
    }
}

async fn qualify(finite: bool, independent: bool) {
    let Prepared {
        mut service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits {
        input: TokenLimit::Limited(0),
        output: TokenLimit::Unlimited,
    })
    .await;
    let primary = service
        .session()
        .clients()
        .iter()
        .find(|client| service.session().is_attached_primary(&client.id))
        .unwrap()
        .id
        .clone();
    service.set_config_root(root.0.as_ref().unwrap().clone());
    let mut child = descendants::prepare(&mut service, &dispatch, &member).await;
    drop(dispatch);
    let child_turn = child.turn.turn_id.clone();
    let generation =
        super::super::compaction_wait::queue_original_wave(&mut service, &primary, true);
    // Spawn and source admission may queue their own optional titles. This
    // scenario owns one explicit title, not additional unpolled contenders.
    service.cancel_agent_session_title_task(&child.turn.conversation_id);
    service.cancel_agent_session_title_task(&member.conversation);
    let title_conversation = if independent {
        let pane = service
            .session_mut_for_tests()
            .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane.as_str())
            .unwrap()
            .session_id
            .clone()
    } else {
        member.conversation.clone()
    };
    service.cancel_agent_session_title_task(&title_conversation);
    service
        .schedule_runtime_agent_session_title(&title_conversation, Some("Combined title"))
        .unwrap();
    let id = service
        .pending_agent_session_title_dispatch_id(&title_conversation)
        .unwrap()
        .to_owned();
    let preparation = service
        .prepare_runtime_budget_auxiliary(&crate::runtime::RuntimeBudgetAuxiliarySource::Title {
            conversation: title_conversation.clone(),
            dispatch: id.clone(),
        })
        .unwrap()
        .unwrap();
    let prepared = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(prepared).unwrap();
    let mut title = service
        .claim_agent_session_title_dispatch(&title_conversation, &id)
        .unwrap()
        .unwrap();
    let title_root = title.budget_owner.as_ref().unwrap().member.root.clone();
    assert_eq!(title_root != member.root, independent);
    let gates: Vec<Gate> = (0..3)
        .map(|_| Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new())))
        .collect();
    child
        .budget_owner
        .as_mut()
        .unwrap()
        .gate_title_grant_for_tests(gates[0].clone());
    title
        .budget_owner
        .as_mut()
        .unwrap()
        .gate_title_grant_for_tests(gates[2].clone());
    let sends = Arc::new(AtomicUsize::new(0));
    let (child_provider, child_server) = endpoint(false, sends.clone()).await;
    let (compactor_provider, compactor_server) = endpoint(false, sends.clone()).await;
    let (title_provider, title_server) = endpoint(true, sends.clone()).await;
    child.provider = child_provider;
    title.provider = title_provider;
    let mut servers = vec![child_server, compactor_server, title_server];
    let drains = ProviderObservationDrains::default();
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let control = async {
        let mut workers = JoinSet::new();
        let mut owners = Default::default();
        let exercise = std::panic::AssertUnwindSafe(async {
            let mut compactor = handle.claim_agent_compaction_task("%1".into(), generation).await.unwrap().unwrap();
            assert!(handle.drain_timer_side_effects(64).await.unwrap().iter().any(|effect| matches!(effect,
                RuntimeSideEffect::ScheduleTimer { key, .. } if key.kind == crate::runtime::RuntimeTimerKind::CompactionClaim && key.owner_id == "%1" && key.generation == generation
            )), "real compaction lease must be adopted");
            assert_eq!(compactor.budget_owner.as_ref().unwrap().member.root, member.root);
            compactor.budget_owner.as_mut().unwrap().gate_title_grant_for_tests(gates[1].clone());
            compactor.provider = compactor_provider;
            let child_agent = AgentId::opaque(child.turn.agent_id.clone()).unwrap();
            let child_owner = ProviderWorkerOwner::inference(child_agent.clone(), &child);
            assert!(spawn_owned_agent_provider_worker(&mut workers, &mut owners, Some(child_owner),
                monitor_runtime_agent_provider_dispatch(handle.clone(), child_agent, child_turn.clone(), child, drains.clone())
            ).is_some());
            let compactor_owner = ProviderWorkerOwner::compaction(&compactor);
            let waiter = handle.clone();
            assert!(spawn_owned_agent_provider_worker(&mut workers, &mut owners, Some(compactor_owner),
                monitor_runtime_agent_compaction_dispatch(handle.clone(), "%1".into(), compactor, drains.clone(), move |dispatch, observer| {
                    execute_runtime_agent_compaction_dispatch_with_waiter(dispatch, observer, Some(waiter))
                })
            ).is_some());
            let title_owner = ProviderWorkerOwner::title(&title);
            let waiter = handle.clone();
            assert!(spawn_owned_agent_provider_worker(&mut workers, &mut owners, Some(title_owner),
                monitor_runtime_agent_session_title_dispatch(handle.clone(), title, drains.clone(), move |dispatch, observer| {
                    Box::pin(execute_runtime_agent_session_title_dispatch_with_waiter(dispatch, observer, Some(waiter)))
                })
            ).is_some());
            let mut child_parked = false;
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    child_parked |= handle.drain_timer_side_effects(64).await.unwrap().iter().any(|effect| matches!(effect,
                        RuntimeSideEffect::CancelTimer { key } if key.kind == crate::runtime::RuntimeTimerKind::ProviderClaim && key.owner_id == child_turn && key.generation == 7
                    ));
                    if child_parked && handle.budget_compaction_claim_is_quiescent("%1".into(), generation).await.unwrap()
                        && handle.budget_title_claim_is_quiescent(title_conversation.clone()).await.unwrap() { break; }
                    assert_eq!(workers.len(), 3, "an original monitor disappeared before release");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("all three original monitors must park");
            assert_eq!(sends.load(Ordering::SeqCst), 0);
            super::super::remember_retirement::publish_policy(&store, "combined-release", 'c', if finite {
                BudgetLimits { input: TokenLimit::Limited(1000), output: TokenLimit::Unlimited }
            } else { BudgetLimits::default() }).await;
            let mut seen = [false; 3];
            let initial_count = if !finite { 3 } else if independent { 2 } else { 1 };
            for _ in 0..initial_count {
                let next = tokio::time::timeout(Duration::from_secs(8), next_gate(&gates, &seen)).await.expect("eligible root did not reach reconsideration");
                seen[next] = true;
            }
            if finite {
                assert!(!(seen[0] && seen[1]), "child and root compactor overlap before native authorization");
                if independent { assert!(seen[2], "independent root did not progress while original root held"); }
                assert!(tokio::time::timeout(Duration::from_millis(2200), next_gate(&gates, &seen)).await.is_err(), "finite root resumed a second original window");
            }
            for (index, entered) in seen.iter().enumerate() { if *entered { gates[index].1.notify_one(); } }
            while seen.iter().any(|seen| !seen) {
                let next = tokio::time::timeout(Duration::from_secs(10), next_gate(&gates, &seen)).await.expect("held original starved after earlier settlement");
                seen[next] = true;
                gates[next].1.notify_one();
            }
            let mut completed = [false; 3];
            while let Some(result) = tokio::time::timeout(Duration::from_secs(10), workers.join_next()).await.expect("original monitor did not retire") {
                let (event, _) = result.unwrap().unwrap().expect("original terminal event missing");
                let index = match event {
                    RuntimeEvent::AgentProvider(AgentProviderEvent::BudgetSettled { completion, .. }) if matches!(completion.as_ref(), AgentProviderEvent::Completed { .. }) => 0,
                    RuntimeEvent::AgentCompaction(AgentCompactionEvent::Completed { .. }) => 1,
                    RuntimeEvent::AgentSessionTitle(AgentSessionTitleEvent::WorkerSettled { outcome: AgentSessionTitleOutcome::Generated(title), .. }) if title == "Combined title" => 2,
                    other => panic!("original family failed: {other:?}"),
                };
                assert!(!completed[index], "duplicate original family completion");
                completed[index] = true;
            }
            assert_eq!(completed, [true; 3]);
            assert_eq!(sends.load(Ordering::SeqCst), 3);
        }).catch_unwind().await;
        workers.abort_all();
        while workers.join_next().await.is_some() {}
        drop(owners);
        let mut server_results = Vec::new();
        for server in &mut servers {
            server.abort();
            server_results.push(server.await);
        }
        let pending: Vec<_> = drains.lock().unwrap().values().cloned().collect();
        for drain in &pending {
            drain.abort.abort();
            drain.completed.observe().await;
        }
        for drain in &pending {
            flush_pending_provider_observations(&handle, &drain.pending)
                .await
                .unwrap();
        }
        handle
            .execute_agent_shell_command(primary, "/stop".into())
            .await
            .unwrap();
        handle.shutdown().await.unwrap();
        (exercise, server_results)
    };
    let ((exercise, server_results), mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    let known = store.budget_account(&member.root).unwrap().known;
    let title_known = store.budget_account(&title_root).unwrap().known;
    let outstanding: i64 = rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COUNT(*) FROM (SELECT id FROM budget_attempts WHERE terminal=0 UNION SELECT attempt FROM budget_admissions WHERE retired_receipt IS NULL)", [], |row| row.get(0),
    ).unwrap();
    drop((exit, handle, drains));
    root.cleanup();
    if let Err(panic) = exercise {
        std::panic::resume_unwind(panic);
    }
    assert!(
        server_results.iter().all(std::result::Result::is_ok),
        "loopback server did not finish: {server_results:?}"
    );
    assert_eq!(
        known,
        UsageTotals {
            input: if independent { 22 } else { 33 },
            output: if independent { 6 } else { 9 }
        }
    );
    assert_eq!(
        title_known,
        UsageTotals {
            input: if independent { 11 } else { 33 },
            output: if independent { 3 } else { 9 }
        }
    );
    assert_eq!(outstanding, 0);
}
