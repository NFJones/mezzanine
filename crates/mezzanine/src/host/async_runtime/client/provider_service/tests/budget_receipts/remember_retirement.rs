//! A claimed memory request retains expense custody after Stop, not permission
//! to start its first physical exchange. Exercise the real command, configured
//! adapter, monitor and actor receipt path; never issue a paid request.

use super::*;
use crate::integrations::http::ReqwestProviderHttpTransport;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn budget_remember_stopped_claim_cannot_start_physical_http() {
    qualify(Disposition::Stopped).await;
}

#[tokio::test]
async fn budget_remember_current_claim_starts_physical_http_once() {
    qualify(Disposition::Current).await;
}

#[tokio::test]
async fn budget_remember_replacement_claim_cannot_revive_original_http() {
    qualify(Disposition::Replaced).await;
}

#[tokio::test]
async fn budget_remember_stop_after_send_preserves_original_expense() {
    qualify(Disposition::IssuedStop).await;
}

#[tokio::test]
async fn budget_remember_original_future_resumes_after_policy_release() {
    qualify(Disposition::HeldRelease).await;
}

#[tokio::test]
async fn budget_remember_held_stop_cannot_resume_after_policy_release() {
    qualify(Disposition::HeldStop).await;
}

#[tokio::test]
async fn budget_remember_human_pause_is_orthogonal_in_both_release_orders() {
    qualify(Disposition::BudgetFirst).await;
    qualify(Disposition::HumanFirst).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Disposition {
    Current,
    Stopped,
    Replaced,
    IssuedStop,
    HeldRelease,
    HeldStop,
    BudgetFirst,
    HumanFirst,
    AllowanceSuccess,
    AllowanceTimeout,
}

#[tokio::test]
async fn budget_remember_active_allowance_excludes_hold_and_expires_after_resume() {
    Box::pin(qualify(Disposition::AllowanceSuccess)).await;
    Box::pin(qualify(Disposition::AllowanceTimeout)).await;
}

async fn qualify(disposition: Disposition) {
    let allowance_test = matches!(
        disposition,
        Disposition::AllowanceSuccess | Disposition::AllowanceTimeout
    );
    let stopped = matches!(
        disposition,
        Disposition::Stopped | Disposition::Replaced | Disposition::HeldStop
    );
    let held = matches!(
        disposition,
        Disposition::HeldRelease
            | Disposition::HeldStop
            | Disposition::BudgetFirst
            | Disposition::HumanFirst
            | Disposition::AllowanceSuccess
            | Disposition::AllowanceTimeout
    );
    let human = matches!(
        disposition,
        Disposition::BudgetFirst | Disposition::HumanFirst
    );
    let Prepared {
        mut service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(if held {
        BudgetLimits {
            input: mez_agent::token_budget::TokenLimit::Limited(0),
            output: mez_agent::token_budget::TokenLimit::Unlimited,
        }
    } else {
        BudgetLimits::default()
    })
    .await;
    drop(dispatch);
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
    service.set_config_root(root.0.as_ref().unwrap().clone());
    if human {
        service
            .session_mut_for_tests()
            .set_pane_live_state("%1", false)
            .unwrap();
    }
    service
        .execute_agent_shell_command(
            &primary,
            "/remember Preserve exact original expense custody.",
        )
        .unwrap();
    let observation = service
        .pending_agent_remember_observation_id("%1")
        .unwrap()
        .to_owned();
    let preparation = service
        .prepare_runtime_budget_auxiliary(&crate::runtime::RuntimeBudgetAuxiliarySource::Remember {
            pane: "%1".into(),
            observation: observation.clone(),
        })
        .unwrap()
        .unwrap();
    let prepared = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(prepared).unwrap();
    let mut dispatch = service
        .claim_agent_remember_task("%1", &observation)
        .unwrap()
        .unwrap();
    assert!(dispatch.budget_owner.is_some());
    assert!(service.agent_remember_observation_matches("%1", &observation));
    // Model a genuine already-claimed worker not yet polled by its executor.
    // Stop retires content authority while the dispatch/journal owns expense.
    if stopped && !held {
        service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        assert!(!service.agent_remember_observation_matches("%1", &observation));
    }
    if disposition == Disposition::Replaced {
        service
            .execute_agent_shell_command(&primary, "/remember A distinct successor.")
            .unwrap();
        let successor = service
            .pending_agent_remember_observation_id("%1")
            .unwrap()
            .to_owned();
        let preparation = service
            .prepare_runtime_budget_auxiliary(
                &crate::runtime::RuntimeBudgetAuxiliarySource::Remember {
                    pane: "%1".into(),
                    observation: successor.clone(),
                },
            )
            .unwrap()
            .unwrap();
        let prepared = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
        service.apply_agent_provider_preparation(prepared).unwrap();
        let successor_dispatch = service
            .claim_agent_remember_task("%1", &successor)
            .unwrap()
            .unwrap();
        assert_ne!(successor, observation);
        assert!(service.agent_remember_observation_matches("%1", &successor));
        // The actor keeps the actual successor; dropping its unissued dispatch
        // cannot give the old weak fence authority over that replacement.
        drop(successor_dispatch);
    }
    let grant_gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    if allowance_test {
        dispatch
            .budget_owner
            .as_mut()
            .unwrap()
            .gate_title_grant_for_tests(grant_gate.clone());
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    dispatch.provider = crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(
        OpenAiCompatibleChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)
            .unwrap()
            .with_provider_id("local-chat")
            .unwrap()
            .with_streaming_options_for_test(disposition == Disposition::AllowanceTimeout)
            .unwrap()
            .with_endpoint(format!("http://{}/v1", listener.local_addr().unwrap())),
    );
    let sends = Arc::new(AtomicUsize::new(0));
    let count = sends.clone();
    let sent = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let sent_by_server = sent.clone();
    let release_server = release.clone();
    let server = tokio::spawn(async move {
        {
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
            sent_by_server.notify_one();
            if disposition == Disposition::IssuedStop {
                release_server.notified().await;
            }
            let content = serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"{\"memories\":[]}"}]}).to_string();
            if disposition == Disposition::AllowanceTimeout {
                // Publish known expense before cancellation, without a final
                // response. The monitor must conserve it in both the native
                // receipts and the original memory failure event.
                let prefix = serde_json::json!({"model":"test-model","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":3}});
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {prefix}\n\n").as_bytes()).await.unwrap();
                socket.flush().await.unwrap();
                tokio::time::sleep(Duration::from_secs(8)).await;
                return;
            }
            if allowance_test {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let message = serde_json::json!({"role":"assistant","content":content});
            let body = serde_json::json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop","message":message}],"usage":{"prompt_tokens":11,"completion_tokens":3}}).to_string();
            let written = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            written.unwrap();
        }
    });
    let drains: ProviderObservationDrains = Default::default();
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let waiter = handle.clone();
    let worker = tokio::spawn(monitor_runtime_agent_remember_dispatch(
        handle.clone(),
        "%1".into(),
        dispatch,
        drains.clone(),
        move |dispatch, observer| {
            execute_runtime_agent_remember_dispatch_with_allowance(
                dispatch,
                observer,
                Some(waiter),
                if allowance_test {
                    Duration::from_secs(10)
                } else {
                    Duration::from_millis(mez_agent::DEFAULT_PROVIDER_TIMEOUT_MS)
                },
            )
        },
    ));
    let control = async {
        let mut worker = worker;
        let retained_original = if held {
            let parked = tokio::time::timeout(
                Duration::from_secs(if allowance_test { 10 } else { 5 }),
                async {
                    loop {
                        if handle
                            .budget_remember_claim_is_quiescent("%1".into())
                            .await
                            .unwrap()
                        {
                            return true;
                        }
                        if worker.is_finished() {
                            return false;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                },
            )
            .await
            .unwrap_or(false);
            let mut retained = parked
                && !worker.is_finished()
                && sends.load(Ordering::SeqCst) == 0
                && rows(&store, "budget_attempts") == 0;
            if allowance_test {
                tokio::time::sleep(Duration::from_secs(11)).await;
                retained &= !worker.is_finished()
                    && handle
                        .budget_remember_claim_is_quiescent("%1".into())
                        .await
                        .unwrap()
                    && sends.load(Ordering::SeqCst) == 0;
            }
            if disposition == Disposition::HeldStop {
                handle
                    .execute_agent_shell_command(primary.clone(), "/stop".into())
                    .await
                    .unwrap();
            }
            if human {
                waiting::browser_input(&handle, &primary, b"/list-agents --all\r").await;
                waiting::browser_input(&handle, &primary, b"p").await;
                waiting::assert_browser_paused(&handle, "%1", true).await;
                if disposition == Disposition::HumanFirst {
                    waiting::browser_input(&handle, &primary, b"p").await;
                    tokio::time::sleep(Duration::from_millis(2100)).await;
                    retained &= handle
                        .budget_remember_claim_is_quiescent("%1".into())
                        .await
                        .unwrap()
                        && sends.load(Ordering::SeqCst) == 0
                        && rows(&store, "budget_attempts") == 0;
                }
            }
            publish_policy(&store, "remember-release", 'e', BudgetLimits::default()).await;
            if allowance_test {
                tokio::time::timeout(Duration::from_secs(5), grant_gate.0.notified())
                    .await
                    .unwrap();
                // Actor resume has ended the inactive span. This genuine
                // pre-authorization barrier spends four seconds of the same
                // original allowance without issuing an HTTP request.
                tokio::time::sleep(Duration::from_secs(4)).await;
                publish_policy(
                    &store,
                    "remember-rehold",
                    'f',
                    BudgetLimits {
                        input: mez_agent::token_budget::TokenLimit::Limited(0),
                        output: mez_agent::token_budget::TokenLimit::Unlimited,
                    },
                )
                .await;
                grant_gate.1.notify_one();
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if handle
                            .budget_remember_claim_is_quiescent("%1".into())
                            .await
                            .unwrap()
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                tokio::time::sleep(Duration::from_secs(11)).await;
                retained &= !worker.is_finished() && sends.load(Ordering::SeqCst) == 0;
                publish_policy(
                    &store,
                    "remember-release-again",
                    'a',
                    BudgetLimits::default(),
                )
                .await;
                tokio::time::timeout(Duration::from_secs(5), grant_gate.0.notified())
                    .await
                    .unwrap();
                grant_gate.1.notify_one();
            }
            if disposition == Disposition::BudgetFirst {
                tokio::time::sleep(Duration::from_millis(2100)).await;
                waiting::assert_browser_paused(&handle, "%1", true).await;
                retained &= handle
                    .budget_remember_claim_is_quiescent("%1".into())
                    .await
                    .unwrap()
                    && sends.load(Ordering::SeqCst) == 0
                    && rows(&store, "budget_attempts") == 0;
                waiting::browser_input(&handle, &primary, b"p").await;
            }
            retained
        } else {
            true
        };
        let stop_result = if disposition == Disposition::IssuedStop {
            let result = tokio::time::timeout(Duration::from_secs(10), sent.notified()).await;
            if result.is_ok() {
                handle
                    .execute_agent_shell_command(primary.clone(), "/stop".into())
                    .await
                    .unwrap();
            }
            release.notify_one();
            Some(result)
        } else {
            None
        };
        let result = tokio::time::timeout(Duration::from_secs(15), &mut worker).await;
        if result.is_err() {
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
        let served = tokio::time::timeout(Duration::from_secs(1), &mut server).await;
        if served.is_err() {
            server.abort();
            let _ = server.await;
        }
        handle
            .execute_agent_shell_command(primary, "/stop".into())
            .await
            .unwrap();
        handle.shutdown().await.unwrap();
        (result, cleanup, served, stop_result, retained_original)
    };
    let ((result, cleanup, served, stop_result, retained_original), mut exit) =
        tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    let known = store.budget_account(&member.root).unwrap().known;
    let attempts = rows(&store, "budget_attempts");
    let receipts = rows(&store, "budget_receipts");
    let outstanding: i64 = rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COUNT(*) FROM (SELECT id FROM budget_attempts WHERE terminal=0 UNION SELECT attempt FROM budget_admissions WHERE retired_receipt IS NULL)",
        [], |row| row.get(0),
    ).unwrap();
    drop((exit, handle, drains));
    if matches!(cleanup, Ok(Ok(()))) {
        root.cleanup();
    }
    cleanup.unwrap().unwrap();
    assert!(
        retained_original,
        "remember original future terminalized instead of parking: result={result:?}, attempts={attempts}, receipts={receipts}, known={known:?}, sends={}",
        sends.load(Ordering::SeqCst)
    );
    if let Some(result) = stop_result {
        result.expect("physical send did not reach the server");
    }
    let completion = result
        .expect("original remember worker timed out")
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(outstanding, 0);
    if allowance_test {
        assert!(
            receipts >= 2,
            "the original exchange retains terminal custody"
        );
        assert_eq!((attempts, sends.load(Ordering::SeqCst)), (1, 1));
        if disposition == Disposition::AllowanceTimeout {
            assert!(
                matches!(completion.0, RuntimeEvent::AgentRemember(AgentRememberEvent::Failed { ref message, usage, .. })
                if message.contains("original active allowance") && usage.input_tokens == 11 && usage.output_tokens == 3),
                "original active allowance or partial failure expense was lost: {:?}",
                completion.0
            );
            assert_eq!(
                known,
                UsageTotals {
                    input: 11,
                    output: 3
                }
            );
        } else {
            assert!(matches!(
                completion.0,
                RuntimeEvent::AgentRemember(AgentRememberEvent::Completed { .. })
            ));
            assert_eq!(
                known,
                UsageTotals {
                    input: 11,
                    output: 3
                }
            );
        }
        return;
    }
    if disposition == Disposition::HeldStop {
        assert_eq!((attempts, receipts), (0, 0));
    } else {
        assert_eq!(attempts, 1);
        assert!(receipts > 0, "unused or issued native grant must settle");
    }
    if stopped {
        assert!(
            served.is_err(),
            "stopped memory request contacted the provider"
        );
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        assert_eq!(known, UsageTotals::default());
        assert!(matches!(
            completion.0,
            RuntimeEvent::AgentRemember(AgentRememberEvent::Failed { .. })
        ));
    } else {
        served
            .expect("current claim did not contact provider")
            .unwrap();
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        assert_eq!(
            known,
            UsageTotals {
                input: 11,
                output: 3
            }
        );
        assert!(matches!(
            completion.0,
            RuntimeEvent::AgentRemember(AgentRememberEvent::Completed { .. })
        ));
    }
}

pub(super) async fn publish_policy(
    store: &TokenUsageStore,
    id: &str,
    digest: char,
    limits: BudgetLimits,
) {
    let writer = store.clone();
    let id = id.to_owned();
    tokio::task::spawn_blocking(move || {
        let projection = writer.budget_policy_projection().unwrap();
        let prior = projection.committed.unwrap();
        let operation = BudgetPolicyPublication {
            namespace: projection.namespace,
            id: BudgetReceiptId::new(id).unwrap(),
            expected_revision: projection.revision,
            previous_digest: prior.digest,
            proposed: BudgetConfigPolicy {
                source: prior.source,
                digest: digest.to_string().repeat(64),
                limits,
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
}
