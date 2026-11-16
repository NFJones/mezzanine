//! Two actual original monitors compete before fresh native authorization.
//! This tests held-request ordering, not fairness against new initial requests.

use super::*;
use crate::host::async_runtime::AsyncRuntimeSessionActor;
use crate::integrations::http::ReqwestProviderHttpTransport;
use mez_agent::token_budget::TokenLimit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod combined;
mod descendants;
mod fresh;

#[tokio::test]
async fn budget_reconsideration_physical_descendant_and_root_share_finite_window() {
    Box::pin(qualify(true, false, Contender::Descendant)).await;
}

#[tokio::test]
async fn budget_reconsideration_physical_descendant_unlimited_preserves_concurrent_windows() {
    Box::pin(qualify(false, false, Contender::Descendant)).await;
}

#[tokio::test]
async fn budget_reconsideration_physical_descendant_independent_finite_root_progresses() {
    Box::pin(qualify(true, true, Contender::Descendant)).await;
}

#[tokio::test]
async fn budget_reconsideration_physical_title_and_memory_share_finite_window() {
    Box::pin(qualify(true, false, Contender::Memory)).await;
}

#[tokio::test]
async fn budget_reconsideration_physical_unlimited_preserves_concurrent_windows() {
    Box::pin(qualify(false, false, Contender::Memory)).await;
}

#[tokio::test]
async fn budget_reconsideration_physical_independent_finite_roots_progress() {
    Box::pin(qualify(true, true, Contender::Memory)).await;
}

#[tokio::test]
async fn budget_reconsideration_physical_ordinary_and_title_share_finite_window() {
    Box::pin(qualify(true, false, Contender::Ordinary)).await;
}

#[tokio::test]
async fn budget_reconsideration_physical_ordinary_unlimited_preserves_concurrent_windows() {
    Box::pin(qualify(false, false, Contender::Ordinary)).await;
}

#[tokio::test]
async fn budget_reconsideration_physical_ordinary_independent_finite_roots_progress() {
    Box::pin(qualify(true, true, Contender::Ordinary)).await;
}

#[tokio::test]
async fn budget_reconsideration_physical_compaction_and_title_share_finite_window() {
    Box::pin(qualify(true, false, Contender::Compaction)).await;
}

#[tokio::test]
async fn budget_reconsideration_physical_compaction_unlimited_preserves_concurrent_windows() {
    Box::pin(qualify(false, false, Contender::Compaction)).await;
}

#[tokio::test]
async fn budget_reconsideration_physical_compaction_independent_finite_roots_progress() {
    Box::pin(qualify(true, true, Contender::Compaction)).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Contender {
    Memory,
    Ordinary,
    Descendant,
    Compaction,
}

async fn qualify(finite: bool, independent: bool, contender: Contender) {
    qualify_case(finite, independent, contender, None).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CancelAt {
    BeforeDrain,
    Held,
}

struct MonitorRetirement(Arc<std::sync::atomic::AtomicBool>);

impl Drop for MonitorRetirement {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn budget_reconsideration_physical_compaction_cancellation_joins_before_drain() {
    Box::pin(qualify_case(
        true,
        false,
        Contender::Compaction,
        Some(CancelAt::BeforeDrain),
    ))
    .await;
}

#[tokio::test]
async fn budget_reconsideration_physical_compaction_cancellation_joins_held_monitor() {
    Box::pin(qualify_case(
        true,
        false,
        Contender::Compaction,
        Some(CancelAt::Held),
    ))
    .await;
}

async fn qualify_case(
    finite: bool,
    independent: bool,
    contender: Contender,
    cancel: Option<CancelAt>,
) {
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
    let (mut ordinary_dispatch, mut memory, compaction) = if contender == Contender::Ordinary {
        (Some(dispatch), None, None)
    } else if contender == Contender::Descendant {
        let child = descendants::prepare(&mut service, &dispatch, &member).await;
        drop(dispatch);
        (Some(child), None, None)
    } else if contender == Contender::Compaction {
        drop(dispatch);
        service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        let generation = super::compaction_wait::queue_original_wave(&mut service, &primary, true);
        (None, None, Some(generation))
    } else {
        drop(dispatch);
        service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        service
            .execute_agent_shell_command(
                &primary,
                "/remember Preserve original competition custody.",
            )
            .unwrap();
        let observation = service
            .pending_agent_remember_observation_id("%1")
            .unwrap()
            .to_owned();
        let preparation = service
            .prepare_runtime_budget_auxiliary(
                &crate::runtime::RuntimeBudgetAuxiliarySource::Remember {
                    pane: "%1".into(),
                    observation: observation.clone(),
                },
            )
            .unwrap()
            .unwrap();
        let prepared = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
        service.apply_agent_provider_preparation(prepared).unwrap();
        (
            None,
            Some(
                service
                    .claim_agent_remember_task("%1", &observation)
                    .unwrap()
                    .unwrap(),
            ),
            None,
        )
    };
    let ordinary_turn = ordinary_dispatch
        .as_ref()
        .map(|dispatch| dispatch.turn.turn_id.clone());
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
        .schedule_runtime_agent_session_title(&title_conversation, Some("Original competing title"))
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
    let gates: Vec<_> = (0..2)
        .map(|_| Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new())))
        .collect();
    if let Some(owner) = ordinary_dispatch
        .as_mut()
        .map(|dispatch| &mut dispatch.budget_owner)
        .or_else(|| memory.as_mut().map(|dispatch| &mut dispatch.budget_owner))
    {
        owner
            .as_mut()
            .unwrap()
            .gate_title_grant_for_tests(gates[0].clone());
    }
    title
        .budget_owner
        .as_mut()
        .unwrap()
        .gate_title_grant_for_tests(gates[1].clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider =
        OpenAiCompatibleChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)
            .unwrap()
            .with_provider_id("local-chat")
            .unwrap()
            .with_streaming_options_for_test(false)
            .unwrap()
            .with_endpoint(format!("http://{}/v1", listener.local_addr().unwrap()));
    if let Some(dispatch) = &mut ordinary_dispatch {
        dispatch.provider = crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(
            provider.clone(),
        );
    } else if let Some(memory) = &mut memory {
        memory.provider = crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(
            provider.clone(),
        );
    }
    title.provider =
        crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider.clone());
    let sends = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = sends.clone();
    // Gate release order identifies the next exchange without guessing from
    // provider-specific HTTP body markers. At most two response choices exist.
    let (responses, mut response_kind) = tokio::sync::mpsc::channel::<bool>(2);
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0; 2048];
                let n = socket.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..n]);
                if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:").map(str::trim))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
                assert!(bytes.len() < 1024 * 1024);
            }
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let ordinary_response = response_kind.recv().await.unwrap();
            let content = if ordinary_response {
                serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"original ordinary completed"}]}).to_string()
            } else {
                "Original title".to_owned()
            };
            let body = serde_json::json!({"id":"competition","choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":3,"total_tokens":14}}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let drains = ProviderObservationDrains::default();
    let monitor_retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let monitor_entered = Arc::new(tokio::sync::Notify::new());
    let control = async {
        let mh = handle.clone();
        let th = handle.clone();
        let mut compaction_workers = JoinSet::new();
        let mut compaction_owners = Default::default();
        let first_worker = if let Some(dispatch) = ordinary_dispatch {
            let agent = AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap();
            let turn = dispatch.turn.turn_id.clone();
            Some(tokio::spawn(monitor_runtime_agent_provider_dispatch(
                handle.clone(),
                agent,
                turn,
                dispatch,
                drains.clone(),
            )))
        } else if let Some(generation) = compaction {
            // Claim through the actor, which adopts the actual original lease.
            let mut dispatch = handle
                .claim_agent_compaction_task("%1".into(), generation)
                .await
                .unwrap()
                .unwrap();
            let lease = handle.drain_timer_side_effects(64).await.unwrap().iter().any(|effect| matches!(effect,
                RuntimeSideEffect::ScheduleTimer { key, .. }
                if key.kind == crate::runtime::RuntimeTimerKind::CompactionClaim && key.owner_id == "%1" && key.generation == generation
            ));
            assert!(lease, "original compaction lease was not adopted");
            assert_eq!(
                dispatch.budget_owner.as_ref().unwrap().member.root,
                member.root
            );
            dispatch
                .budget_owner
                .as_mut()
                .unwrap()
                .gate_title_grant_for_tests(gates[0].clone());
            dispatch.provider =
                crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider);
            let owner = ProviderWorkerOwner::compaction(&dispatch);
            let ch = handle.clone();
            let cd = drains.clone();
            let waiter = ch.clone();
            let monitor = monitor_runtime_agent_compaction_dispatch(
                ch,
                "%1".into(),
                dispatch,
                cd,
                move |dispatch, observer| {
                    execute_runtime_agent_compaction_dispatch_with_waiter(
                        dispatch,
                        observer,
                        Some(waiter),
                    )
                },
            );
            let retired = MonitorRetirement(monitor_retired.clone());
            let entered = monitor_entered.clone();
            // The control owns the actual registered monitor, not a wrapper
            // task whose JoinSet could be dropped without joining destruction.
            let admitted = spawn_owned_agent_provider_worker(
                &mut compaction_workers,
                &mut compaction_owners,
                Some(owner),
                async move {
                    let _retired = retired;
                    if cancel == Some(CancelAt::BeforeDrain) {
                        entered.notify_one();
                        // The real monitor owns its original dispatch but has
                        // not yet polled the initial claim check or made a drain.
                        std::future::pending::<()>().await;
                    }
                    monitor.await
                },
            );
            assert!(
                admitted.is_some(),
                "original compaction producer refused registration"
            );
            None
        } else {
            Some(tokio::spawn(monitor_runtime_agent_remember_dispatch(
                handle.clone(),
                "%1".into(),
                memory.unwrap(),
                drains.clone(),
                move |dispatch, observer| {
                    Box::pin(execute_runtime_agent_remember_dispatch_with_waiter(
                        dispatch,
                        observer,
                        Some(mh),
                    ))
                },
            )))
        };
        let mut workers = vec![
            first_worker,
            Some(tokio::spawn(monitor_runtime_agent_session_title_dispatch(
                handle.clone(),
                title,
                drains.clone(),
                move |dispatch, observer| {
                    Box::pin(execute_runtime_agent_session_title_dispatch_with_waiter(
                        dispatch,
                        observer,
                        Some(th),
                    ))
                },
            ))),
        ];
        let exercise: std::result::Result<Option<bool>, String> =
            async {
                if cancel == Some(CancelAt::BeforeDrain) {
                    tokio::time::timeout(Duration::from_secs(5), monitor_entered.notified()).await
                        .map_err(|_| "registered monitor did not enter pre-drain gate".to_owned())?;
                    if drains.lock().unwrap().contains_key(&ProviderObservationOwner::Compaction("%1".into(), compaction.unwrap())) {
                        return Err("pre-drain monitor unexpectedly registered a producer drain".to_owned());
                    }
                    return Ok(None);
                }
                let mut ordinary_parked = false;
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let first_parked = if let Some(turn) = &ordinary_turn {
                            ordinary_parked |= handle.drain_timer_side_effects(64).await.unwrap().iter().any(|effect| matches!(effect,
                                crate::runtime::RuntimeSideEffect::CancelTimer { key }
                                if key.kind == crate::runtime::RuntimeTimerKind::ProviderClaim && key.owner_id == *turn && key.generation == 7
                            ));
                            ordinary_parked
                        } else if let Some(generation) = compaction {
                            handle.budget_compaction_claim_is_quiescent("%1".into(), generation).await.unwrap()
                        } else {
                            handle.budget_remember_claim_is_quiescent("%1".into()).await.unwrap()
                        };
                        if first_parked
                            && handle
                                .budget_title_claim_is_quiescent(title_conversation.clone())
                                .await
                                .unwrap()
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .map_err(|_| "original claims did not park".to_owned())?;
                if cancel == Some(CancelAt::Held) {
                    if !drains.lock().unwrap().contains_key(&ProviderObservationOwner::Compaction("%1".into(), compaction.unwrap())) {
                        return Err("held original monitor has no actual producer drain".to_owned());
                    }
                    return Ok(None);
                }
                super::remember_retirement::publish_policy(
                    &store,
                    "competition-release",
                    'c',
                    if finite {
                        BudgetLimits {
                            input: TokenLimit::Limited(1000),
                            output: TokenLimit::Unlimited,
                        }
                    } else {
                        BudgetLimits::default()
                    },
                )
                .await;
                let first = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! { _ = gates[0].0.notified() => 0, _ = gates[1].0.notified() => 1 }
            }).await.map_err(|_| "no original reached native reconsideration".to_owned())?;
                let second = 1 - first;
                let concurrent =
                    tokio::time::timeout(Duration::from_millis(2200), gates[second].0.notified())
                        .await
                        .is_ok();
                responses.send(contender != Contender::Memory && first == 0).await.unwrap();
                gates[first].1.notify_one();
                tokio::time::timeout(Duration::from_secs(5), async {
                    while sends.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }).await.map_err(|_| "first physical request missing".to_owned())?;
                if finite && !independent && concurrent {
                    // A disabled-ordering regression probe may resume both.
                    // Settle the first exchange before releasing the second,
                    // so the final oracle diagnoses ordering, not a retry race.
                    tokio::time::timeout(Duration::from_secs(5), async {
                        while store.budget_account(&member.root).unwrap().known.input < 11 {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .map_err(|_| "first exchange did not settle".to_owned())?;
                }
                if !concurrent {
                    tokio::time::timeout(Duration::from_secs(8), gates[second].0.notified())
                        .await
                        .map_err(|_| "second original did not recover".to_owned())?;
                }
                responses.send(contender != Contender::Memory && second == 0).await.unwrap();
                gates[second].1.notify_one();
                for (index, worker) in workers.iter_mut().enumerate() {
                    let joined = if index == 0 && compaction.is_some() {
                        tokio::time::timeout(Duration::from_secs(10), compaction_workers.join_next()).await
                            .map_err(|_| "original compaction monitor did not finish".to_owned())?
                            .ok_or_else(|| "original compaction monitor disappeared".to_owned())?
                    } else {
                        tokio::time::timeout(Duration::from_secs(10), worker.as_mut().unwrap())
                            .await
                            .map_err(|_| "original monitor did not finish".to_owned())?
                    };
                    worker.take();
                    let completion = joined
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| "original completion missing".to_owned())?;
                    if index == 0 {
                        let successful = match contender {
                            Contender::Ordinary | Contender::Descendant => matches!(&completion.0, RuntimeEvent::AgentProvider(AgentProviderEvent::BudgetSettled { completion, .. }) if matches!(completion.as_ref(), AgentProviderEvent::Completed { .. })),
                            Contender::Compaction => matches!(completion.0, RuntimeEvent::AgentCompaction(AgentCompactionEvent::Completed { .. })),
                            Contender::Memory => true,
                        };
                        if !successful {
                            return Err("original contender did not complete successfully".to_owned());
                        }
                    }
                }
                Ok(Some(concurrent))
            }
            .await;
        for worker in workers.into_iter().flatten() {
            worker.abort();
            let _ = worker.await;
        }
        compaction_workers.abort_all();
        while compaction_workers.join_next().await.is_some() {}
        drop(compaction_owners);
        let monitor_joined = compaction.is_none() || monitor_retired.load(Ordering::SeqCst);
        server.abort();
        let server_result = server.await;
        let cleanup = async {
            let pending: Vec<_> = drains.lock().unwrap().values().cloned().collect();
            for drain in &pending {
                drain.abort.abort();
                drain.completed.observe().await;
            }
            for drain in &pending {
                flush_pending_provider_observations(&handle, &drain.pending).await?;
            }
            handle
                .execute_agent_shell_command(primary, "/stop".into())
                .await?;
            handle.shutdown().await
        }
        .await;
        (exercise, server_result, cleanup, monitor_joined)
    };
    let ((exercise, server_result, cleanup, monitor_joined), mut exit) =
        tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    let known = store.budget_account(&member.root).unwrap().known;
    let title_known = store.budget_account(&title_root).unwrap().known;
    let outstanding: i64 = rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COUNT(*) FROM (SELECT id FROM budget_attempts WHERE terminal=0 UNION SELECT attempt FROM budget_admissions WHERE retired_receipt IS NULL)", [], |row| row.get(0),
    ).unwrap();
    drop((exit, handle, drains));
    root.cleanup();
    cleanup.unwrap();
    assert!(
        monitor_joined,
        "actual compaction monitor must be destroyed before final drain snapshot/root cleanup"
    );
    if cancel.is_some() {
        assert_eq!(exercise.unwrap(), None);
        assert!(server_result.unwrap_err().is_cancelled());
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        assert_eq!(known, UsageTotals::default());
        assert_eq!(title_known, UsageTotals::default());
        assert_eq!(outstanding, 0);
        return;
    }
    assert!(
        server_result.is_ok(),
        "loopback did not finish both requests: {server_result:?}"
    );
    assert_eq!(exercise.unwrap(), Some(!finite || independent));
    assert_eq!(sends.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        (known.input, known.output),
        if independent { (11, 3) } else { (22, 6) }
    );
    assert_eq!(
        (title_known.input, title_known.output),
        if independent { (11, 3) } else { (22, 6) }
    );
    assert_eq!(outstanding, 0);
}
