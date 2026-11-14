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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Disposition {
    Current,
    Stopped,
    Replaced,
    IssuedStop,
}

async fn qualify(disposition: Disposition) {
    let stopped = matches!(disposition, Disposition::Stopped | Disposition::Replaced);
    let Prepared {
        mut service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits::default()).await;
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
    if stopped {
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
    let count = sends.clone();
    let sent = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let sent_by_server = sent.clone();
    let release_server = release.clone();
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
        sent_by_server.notify_one();
        if disposition == Disposition::IssuedStop {
            release_server.notified().await;
        }
        let content = serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"{\"memories\":[]}"}]}).to_string();
        let body = serde_json::json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":content}}],"usage":{"prompt_tokens":11,"completion_tokens":3}}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let drains: ProviderObservationDrains = Default::default();
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let worker = tokio::spawn(monitor_runtime_agent_remember_dispatch(
        handle.clone(),
        "%1".into(),
        dispatch,
        drains.clone(),
        execute_runtime_agent_remember_dispatch,
    ));
    let control = async {
        let mut worker = worker;
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
        (result, cleanup, served, stop_result)
    };
    let ((result, cleanup, served, stop_result), mut exit) = tokio::join!(control, actor.run());
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
    if let Some(result) = stop_result {
        result.expect("physical send did not reach the server");
    }
    let completion = result
        .expect("original remember worker timed out")
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(outstanding, 0);
    assert_eq!(attempts, 1);
    assert!(receipts > 0, "unused or issued native grant must settle");
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
