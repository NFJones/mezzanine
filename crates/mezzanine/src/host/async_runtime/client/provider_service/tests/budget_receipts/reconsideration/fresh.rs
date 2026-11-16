//! A fresh title cannot bypass an older ordinary request's selected native
//! authorization window. Separate real servers prevent network serialization
//! from supplying the ordering oracle. Cleanup precedes the regression verdict.

use super::*;
use futures_util::FutureExt;

#[tokio::test]
async fn budget_fresh_title_cannot_overtake_selected_ordinary_window() {
    Box::pin(qualify(false)).await;
}

#[tokio::test]
async fn budget_fresh_title_stop_while_locally_queued_cannot_send() {
    Box::pin(qualify(true)).await;
}

async fn qualify(stopped: bool) {
    let Prepared {
        mut service,
        mut dispatch,
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
    service.cancel_agent_session_title_task(&member.conversation);
    service
        .schedule_runtime_agent_session_title(&member.conversation, Some("Fresh contender"))
        .unwrap();
    let id = service
        .pending_agent_session_title_dispatch_id(&member.conversation)
        .unwrap()
        .to_owned();
    let preparation = service
        .prepare_runtime_budget_auxiliary(&crate::runtime::RuntimeBudgetAuxiliarySource::Title {
            conversation: member.conversation.clone(),
            dispatch: id.clone(),
        })
        .unwrap()
        .unwrap();
    let prepared = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(prepared).unwrap();
    let mut title = service
        .claim_agent_session_title_dispatch(&member.conversation, &id)
        .unwrap()
        .unwrap();
    assert_eq!(
        title.budget_owner.as_ref().unwrap().member.root,
        member.root
    );
    let ordinary_gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    let title_gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    dispatch
        .budget_owner
        .as_mut()
        .unwrap()
        .gate_title_grant_for_tests(ordinary_gate.clone());
    title
        .budget_owner
        .as_mut()
        .unwrap()
        .gate_title_grant_for_tests(title_gate.clone());
    let sends = Arc::new(AtomicUsize::new(0));
    let (ordinary_provider, ordinary_server) = combined::endpoint(false, sends.clone()).await;
    let (title_provider, title_server) = combined::endpoint(true, sends.clone()).await;
    dispatch.provider = ordinary_provider;
    title.provider = title_provider;
    let turn = dispatch.turn.turn_id.clone();
    let agent = AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap();
    let drains = ProviderObservationDrains::default();
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let control = async {
        let mut ordinary = Some(tokio::spawn(monitor_runtime_agent_provider_dispatch(
            handle.clone(),
            agent,
            turn.clone(),
            dispatch,
            drains.clone(),
        )));
        let mut fresh = None;
        let exercise = std::panic::AssertUnwindSafe(async {
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    if handle.drain_timer_side_effects(64).await.unwrap().iter().any(|effect| matches!(effect,
                        RuntimeSideEffect::CancelTimer { key } if key.kind == crate::runtime::RuntimeTimerKind::ProviderClaim && key.owner_id == turn
                    )) { break; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            super::super::remember_retirement::publish_policy(&store, "fresh-order-release", 'c', BudgetLimits {
                input: TokenLimit::Limited(1000), output: TokenLimit::Unlimited,
            }).await;
            tokio::time::timeout(Duration::from_secs(8), ordinary_gate.0.notified()).await.unwrap();
            assert_eq!(sends.load(Ordering::SeqCst), 0);
            let waiter = handle.clone();
            fresh = Some(tokio::spawn(monitor_runtime_agent_session_title_dispatch(
                handle.clone(), title, drains.clone(), move |dispatch, observer| {
                    Box::pin(execute_runtime_agent_session_title_dispatch_with_waiter(dispatch, observer, Some(waiter)))
                },
            )));
            if stopped {
                tokio::time::timeout(Duration::from_secs(8), async {
                    while !handle.budget_title_claim_is_quiescent(member.conversation.clone()).await.unwrap() {
                        assert!(!fresh.as_ref().unwrap().is_finished());
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }).await.unwrap();
                assert_eq!(sends.load(Ordering::SeqCst), 0);
                let attempts: i64 = rusqlite::Connection::open(store.path()).unwrap()
                    .query_row("SELECT COUNT(*) FROM budget_attempts", [], |row| row.get(0)).unwrap();
                assert_eq!(attempts, 0, "local queue fabricated native registration");
                handle.execute_agent_shell_command(primary.clone(), "/stop".into()).await.unwrap();
                ordinary_gate.1.notify_one();
                title_gate.1.notify_one();
                let mut report = AsyncAgentProviderPollReport {
                    polls: 0, executions: 0, idle_polls: 0,
                    terminal_state: *handle.lifecycle_state_watcher().borrow(),
                };
                for worker in [&mut ordinary, &mut fresh] {
                    let joined = tokio::time::timeout(Duration::from_secs(10), worker.as_mut().unwrap()).await.unwrap();
                    worker.take();
                    record_joined_agent_provider_worker(joined, &handle, &mut report).await.unwrap();
                }
                return (false, 0, false);
            }
            let early_window = tokio::time::timeout(Duration::from_millis(2200), title_gate.0.notified()).await.is_ok();
            let early_sends = sends.load(Ordering::SeqCst);
            ordinary_gate.1.notify_one();
            let later_window = early_window || tokio::time::timeout(Duration::from_secs(8), title_gate.0.notified()).await.is_ok();
            title_gate.1.notify_one();
            let ordinary_result = tokio::time::timeout(Duration::from_secs(10), ordinary.as_mut().unwrap()).await.unwrap();
            ordinary.take();
            let (event, _) = ordinary_result.unwrap().unwrap().unwrap();
            assert!(matches!(event, RuntimeEvent::AgentProvider(AgentProviderEvent::BudgetSettled { completion, .. }) if matches!(completion.as_ref(), AgentProviderEvent::Completed { .. })));
            let title_result = tokio::time::timeout(Duration::from_secs(10), fresh.as_mut().unwrap()).await.unwrap();
            fresh.take();
            let (event, _) = title_result.unwrap().unwrap().unwrap();
            assert!(matches!(event, RuntimeEvent::AgentSessionTitle(AgentSessionTitleEvent::WorkerSettled { outcome: AgentSessionTitleOutcome::Generated(_), .. })));
            (early_window, early_sends, later_window)
        }).catch_unwind().await;
        ordinary_gate.1.notify_one();
        title_gate.1.notify_one();
        for worker in [ordinary, fresh].into_iter().flatten() {
            worker.abort();
            let _ = worker.await;
        }
        let mut servers = Vec::new();
        for server in [ordinary_server, title_server] {
            servers.push(join_server(server, Duration::from_secs(2)).await);
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
        (exercise, servers)
    };
    let ((exercise, servers), mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    let known = store.budget_account(&member.root).unwrap().known;
    let outstanding: i64 = rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COUNT(*) FROM (SELECT id FROM budget_attempts WHERE terminal=0 UNION SELECT attempt FROM budget_admissions WHERE retired_receipt IS NULL)", [], |row| row.get(0),
    ).unwrap();
    drop((exit, handle, drains));
    root.cleanup();
    let observed = match exercise {
        Ok(observed) => observed,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert_eq!(outstanding, 0);
    if stopped {
        assert!(
            servers
                .iter()
                .all(|result| result.as_ref().is_err_and(|error| error.is_cancelled()))
        );
    } else {
        assert!(servers.iter().all(std::result::Result::is_ok));
    }
    assert_eq!(
        known,
        UsageTotals {
            input: if stopped { 0 } else { 22 },
            output: if stopped { 0 } else { 6 }
        }
    );
    assert_eq!(sends.load(Ordering::SeqCst), if stopped { 0 } else { 2 });
    assert_eq!(
        observed,
        (false, 0, !stopped),
        "fresh title bypassed original selected window"
    );
}

/// Observe one actual server retirement, including completion during the grace
/// interval. A successful join consumes the handle and must never be polled again.
async fn join_server(
    mut server: tokio::task::JoinHandle<()>,
    grace: Duration,
) -> std::result::Result<(), tokio::task::JoinError> {
    match tokio::time::timeout(grace, &mut server).await {
        Ok(joined) => joined,
        Err(_) => {
            server.abort();
            server.await
        }
    }
}

#[tokio::test(start_paused = true)]
async fn budget_fresh_title_server_join_observes_completion_during_grace_once() {
    let (release, blocked) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move { blocked.await.unwrap() });
    let retirement = join_server(server, Duration::from_secs(2));
    tokio::pin!(retirement);
    assert!(futures_util::poll!(&mut retirement).is_pending());
    release.send(()).unwrap();
    assert!(retirement.await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn budget_fresh_title_server_join_observes_abort_after_grace() {
    let server = tokio::spawn(std::future::pending::<()>());
    let joined = join_server(server, Duration::from_secs(2)).await;
    assert!(joined.unwrap_err().is_cancelled());
}
