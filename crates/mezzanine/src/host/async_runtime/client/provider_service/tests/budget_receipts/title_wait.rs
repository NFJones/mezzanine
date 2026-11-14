//! Actual title claim -> original monitored future -> actor park -> physical
//! HTTP release -> receipt/terminal adoption. No ordinary turn is fabricated.

use super::*;
use crate::integrations::http::ReqwestProviderHttpTransport;
use futures_util::FutureExt;
use mez_agent::token_budget::TokenLimit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn budget_monitored_title_original_future_resumes_after_policy_release() {
    qualify(Release::Budget).await;
}

#[tokio::test]
async fn budget_monitored_title_retirement_cannot_resume_after_policy_release() {
    qualify(Release::Retired).await;
}

#[tokio::test]
async fn budget_monitored_title_post_resume_retirement_withdraws_native_grant() {
    qualify(Release::RetiredAfterResume).await;
}

#[tokio::test]
async fn budget_monitored_title_stop_without_turn_cannot_resume() {
    qualify(Release::Stopped).await;
}

#[tokio::test]
async fn budget_monitored_title_navigation_stop_cannot_bypass_retirement() {
    qualify(Release::NavigationStopped).await;
}

#[tokio::test]
async fn budget_monitored_title_human_pause_is_orthogonal_in_both_release_orders() {
    qualify(Release::BudgetFirst).await;
    qualify(Release::HumanFirst).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Release {
    Budget,
    Retired,
    RetiredAfterResume,
    Stopped,
    NavigationStopped,
    BudgetFirst,
    HumanFirst,
}

async fn qualify(release: Release) {
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
    service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    if release == Release::NavigationStopped {
        service
            .execute_agent_shell_command(&primary, "/shell-mode native")
            .unwrap();
    }
    if !matches!(release, Release::Budget | Release::NavigationStopped) {
        // There is no physical shell in this fixture; do not invent a PID for
        // the browser's production native lifecycle target checks.
        service
            .session_mut_for_tests()
            .set_pane_live_state("%1", false)
            .unwrap();
    }
    if matches!(release, Release::Retired | Release::RetiredAfterResume) {
        // Keep a distinct safe terminal/control target when the original pane
        // is retired through the real command ingress, not a test map edit.
        service
            .session_mut_for_tests()
            .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
            .unwrap();
        service
            .execute_terminal_command(&primary, "select-pane -t %1")
            .unwrap();
    }
    drop(dispatch);
    service.cancel_agent_session_title_task(&member.conversation);
    service
        .schedule_runtime_agent_session_title(&member.conversation, Some("Original parked title"))
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
    let attempt = title.task.attempt_id.clone();
    let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    if release == Release::RetiredAfterResume {
        title
            .budget_owner
            .as_mut()
            .unwrap()
            .gate_title_grant_for_tests(gate.clone());
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    title.provider = crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(
        OpenAiCompatibleChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)
            .unwrap()
            .with_provider_id("local-chat")
            .unwrap()
            .with_streaming_options_for_test(false)
            .unwrap()
            .with_endpoint(format!("http://{}/v1", listener.local_addr().unwrap())),
    );
    let sends = Arc::new(AtomicUsize::new(0));
    let count = sends.clone();
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
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
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
        count.fetch_add(1, Ordering::SeqCst);
        let body = serde_json::json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"Original parked title"}}],"usage":{"prompt_tokens":11,"completion_tokens":3}}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let drains: ProviderObservationDrains = Default::default();
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let waiter = handle.clone();
    let mut worker = Some(tokio::spawn(monitor_runtime_agent_session_title_dispatch(
        handle.clone(),
        title,
        drains.clone(),
        move |dispatch, observer| {
            execute_runtime_agent_session_title_dispatch_with_waiter(
                dispatch,
                observer,
                Some(waiter),
            )
        },
    )));
    let control = async {
        let client = async {
            loop {
                if handle
                    .budget_title_claim_is_quiescent(member.conversation.clone())
                    .await
                    .unwrap()
                {
                    break;
                }
                assert!(
                    !worker.as_ref().unwrap().is_finished(),
                    "original title was terminalized instead of parked"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(sends.load(Ordering::SeqCst), 0);
            assert_eq!(rows(&store, "budget_attempts"), 0);
            handle
                .execute_agent_shell_command(primary.clone(), "/help".into())
                .await
                .unwrap();
            if release == Release::NavigationStopped {
                let accepted = handle
                    .execute_agent_shell_command(primary.clone(), "/cd /".into())
                    .await
                    .unwrap();
                assert!(accepted.contains(r#""body":null"#), "{accepted}");
            }
            if matches!(release, Release::Stopped | Release::NavigationStopped) {
                handle
                    .execute_agent_shell_command(primary.clone(), "/stop".into())
                    .await
                    .unwrap();
            }
            if release == Release::Retired {
                handle
                    .execute_terminal_command(primary.clone(), "kill-pane -t %1".into())
                    .await
                    .unwrap();
                handle
                    .execute_terminal_command(primary.clone(), "list-panes".into())
                    .await
                    .unwrap();
            }
            let human = matches!(release, Release::BudgetFirst | Release::HumanFirst);
            if human {
                waiting::browser_input(&handle, &primary, b"/list-agents --all\r").await;
                waiting::browser_input(&handle, &primary, b"p").await;
                waiting::assert_browser_paused(&handle, "%1", true).await;
                if release == Release::HumanFirst {
                    waiting::browser_input(&handle, &primary, b"p").await;
                    tokio::time::sleep(Duration::from_millis(2100)).await;
                    assert!(
                        handle
                            .budget_title_claim_is_quiescent(member.conversation.clone())
                            .await
                            .unwrap()
                    );
                    assert_eq!(sends.load(Ordering::SeqCst), 0);
                    assert_eq!(rows(&store, "budget_attempts"), 0);
                }
            }
            let writer = store.clone();
            tokio::task::spawn_blocking(move || {
                let projection = writer.budget_policy_projection().unwrap();
                let prior = projection.committed.unwrap();
                let operation = BudgetPolicyPublication {
                    namespace: projection.namespace,
                    id: BudgetReceiptId::new("title-release").unwrap(),
                    expected_revision: projection.revision,
                    previous_digest: prior.digest,
                    proposed: BudgetConfigPolicy {
                        source: prior.source,
                        digest: "e".repeat(64),
                        limits: BudgetLimits {
                            input: TokenLimit::Limited(100),
                            output: TokenLimit::Unlimited,
                        },
                    },
                    supersedes: None,
                };
                writer.begin_budget_policy_publication(&operation).unwrap();
                writer
                    .reconcile_budget_policy_publication(&operation, &operation.proposed.digest)
                    .unwrap();
            })
            .await
            .unwrap();
            if release == Release::BudgetFirst {
                tokio::time::sleep(Duration::from_millis(2100)).await;
                waiting::assert_browser_paused(&handle, "%1", true).await;
                assert!(
                    handle
                        .budget_title_claim_is_quiescent(member.conversation.clone())
                        .await
                        .unwrap()
                );
                assert_eq!(sends.load(Ordering::SeqCst), 0);
                assert_eq!(rows(&store, "budget_attempts"), 0);
                waiting::browser_input(&handle, &primary, b"p").await;
            }
            if release == Release::RetiredAfterResume {
                gate.0.notified().await;
                assert!(
                    !handle
                        .budget_title_claim_is_quiescent(member.conversation.clone())
                        .await
                        .unwrap()
                );
                assert_eq!(rows(&store, "budget_attempts"), 0);
                handle
                    .execute_terminal_command(primary.clone(), "kill-pane -t %1".into())
                    .await
                    .unwrap();
                handle
                    .execute_terminal_command(primary.clone(), "list-panes".into())
                    .await
                    .unwrap();
                gate.1.notify_one();
            }
            let joined = worker.as_mut().unwrap().await;
            worker = None;
            let mut report = AsyncAgentProviderPollReport {
                polls: 0,
                executions: 0,
                idle_polls: 0,
                terminal_state: *handle.lifecycle_state_watcher().borrow(),
            };
            record_joined_agent_provider_worker(joined, &handle, &mut report)
                .await
                .unwrap();
        };
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            std::panic::AssertUnwindSafe(client).catch_unwind(),
        )
        .await;
        gate.1.notify_one();
        if let Some(worker) = worker.take() {
            worker.abort();
            let _ = worker.await;
        }
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
        let mut server = server;
        let served = tokio::time::timeout(Duration::from_secs(2), &mut server).await;
        if served.is_err() {
            server.abort();
            let _ = server.await;
        }
        handle.shutdown().await.unwrap();
        (result, cleanup, served)
    };
    let ((result, cleanup, served), mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    let known = store.budget_account(&member.root).unwrap().known;
    let receipts = rows(&store, "budget_receipts");
    let attempts = rows(&store, "budget_attempts");
    let outstanding: i64 = rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COUNT(*) FROM (SELECT id FROM budget_attempts WHERE terminal=0 UNION SELECT attempt FROM budget_admissions WHERE retired_receipt IS NULL)",
        [], |row| row.get(0),
    ).unwrap();
    let no_claim = exit
        .service
        .claimed_agent_session_title_task_ids()
        .is_empty();
    let duplicate = exit
        .service
        .apply_agent_session_title_transition(AgentSessionTitleEvent::WorkerSettled {
            conversation_id: member.conversation.clone(),
            attempt_id: attempt,
            usage: Default::default(),
            outcome: AgentSessionTitleOutcome::Generated("Duplicate must remain inert".into()),
        })
        .unwrap();
    drop((exit, handle, drains, worker));
    if matches!(cleanup, Ok(Ok(()))) {
        root.cleanup();
    }
    cleanup.unwrap().unwrap();
    if let Err(panic) = result.expect("original title monitor timed out") {
        std::panic::resume_unwind(panic);
    }
    assert!(no_claim && !duplicate.applied);
    if matches!(
        release,
        Release::Retired
            | Release::RetiredAfterResume
            | Release::Stopped
            | Release::NavigationStopped
    ) {
        assert!(
            served.is_err(),
            "retired title unexpectedly contacted the provider"
        );
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        assert_eq!(known, UsageTotals::default());
        if release == Release::RetiredAfterResume {
            assert_eq!(attempts, 1);
            assert!(receipts > 0 && outstanding == 0);
        } else {
            assert_eq!((attempts, receipts), (0, 0));
        }
        return;
    }
    served.unwrap().unwrap();
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
