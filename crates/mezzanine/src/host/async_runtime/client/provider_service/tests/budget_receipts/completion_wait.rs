//! Actual HTTP response -> durable terminal receipt -> original monitor hold.
//! The candidate is inspected after release, not executed by this fixture.

use super::*;
use crate::host::async_runtime::AsyncRuntimeSessionActor;
use crate::integrations::http::ReqwestProviderHttpTransport;
use futures_util::FutureExt;
use mez_agent::token_budget::TokenLimit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn budget_completed_response_equality_holds_original_until_sufficient_raise() {
    qualify(false, false).await;
}

#[tokio::test]
async fn budget_completed_response_stop_cannot_publish_after_policy_raise() {
    qualify(true, false).await;
}

#[tokio::test]
async fn budget_completed_response_read_refusal_retains_original_until_recovery() {
    qualify(false, true).await;
}

async fn qualify(stop: bool, read_failure: bool) {
    let Prepared {
        service,
        mut dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits {
        input: TokenLimit::Limited(11),
        output: TokenLimit::Limited(3),
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
    let turn = dispatch.turn.turn_id.clone();
    let agent = AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider =
        OpenAiCompatibleChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)
            .unwrap()
            .with_provider_id("local-chat")
            .unwrap()
            .with_streaming_options_for_test(false)
            .unwrap()
            .with_endpoint(format!("http://{}/v1", listener.local_addr().unwrap()));
    dispatch.provider =
        crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider);
    let sends = Arc::new(AtomicUsize::new(0));
    let count = sends.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 2048];
            let n = socket.read(&mut chunk).await.unwrap();
            assert_ne!(n, 0, "request closed before body");
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
        count.fetch_add(1, Ordering::SeqCst);
        let content = serde_json::json!({"rationale":"fixture", "actions":[{"type":"shell_command", "summary":"Original retained candidate", "command":"printf retained-native-candidate"}]}).to_string();
        let body = serde_json::json!({"id":"original-completion", "choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":3,"total_tokens":14}}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        // Observe any unexpected retry instead of disappearing after reply one.
        while let Ok((mut extra, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            let _ = extra.shutdown().await;
        }
    });
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let drains = ProviderObservationDrains::default();
    let control = async {
        let mut worker = tokio::spawn(monitor_runtime_agent_provider_dispatch(
            handle.clone(),
            agent,
            turn.clone(),
            dispatch,
            drains.clone(),
        ));
        let mut joined = false;
        let exercise = std::panic::AssertUnwindSafe(async {
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    if worker.is_finished() { return Err("completion escaped before equality hold".to_owned()); }
                    let parked = handle.drain_timer_side_effects(64).await.unwrap().iter().any(|effect| matches!(effect,
                        crate::runtime::RuntimeSideEffect::CancelTimer { key }
                        if key.kind == crate::runtime::RuntimeTimerKind::ProviderClaim && key.owner_id == turn && key.generation == 7
                    ));
                    if parked { return Ok(()); }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.map_err(|_| "completed original did not park".to_owned())??;
            if store.budget_account(&member.root).unwrap().known != (UsageTotals { input: 11, output: 3 }) {
                return Err("original expense not settled before completion hold".to_owned());
            }
            if rows(&store, "budget_receipts") != 2 || !handle.agent_turn_is_running(&turn).await.unwrap() {
                return Err("original terminal receipt or claim missing".to_owned());
            }
            // Clearing input alone cannot release the exhausted output side.
            super::remember_retirement::publish_policy(&store, "completion-insufficient", 'c', BudgetLimits {
                input: TokenLimit::Unlimited, output: TokenLimit::Limited(3),
            }).await;
            if tokio::time::timeout(Duration::from_millis(450), &mut worker).await.is_ok() {
                joined = true;
                return Err("insufficient raise published original candidate".to_owned());
            }
            let original = if read_failure {
                let db = rusqlite::Connection::open(store.path()).unwrap();
                let original: (String, String) = db.query_row(
                    "SELECT id,body FROM budget_receipts WHERE id=(SELECT retired_receipt FROM budget_admissions LIMIT 1)",
                    [], |row| Ok((row.get(0)?, row.get(1)?)),
                ).unwrap();
                db.execute("UPDATE budget_receipts SET body='{}' WHERE id=?1", [&original.0]).unwrap();
                Some(original)
            } else { None };
            if stop { handle.execute_agent_shell_command(primary.clone(), "/stop".into()).await.unwrap(); }
            super::remember_retirement::publish_policy(&store, "completion-release", 'd', BudgetLimits::default()).await;
            if let Some((id, body)) = original {
                let early = tokio::time::timeout(Duration::from_millis(2300), &mut worker).await;
                // A ready JoinHandle has already been consumed, even if the
                // following exact restoration fails and cleanup unwinds.
                joined |= early.is_ok();
                // Restore this fixture's exact before-image even if the
                // regression returned an early error/completion.
                rusqlite::Connection::open(store.path()).unwrap().execute(
                    "UPDATE budget_receipts SET body=?1 WHERE id=?2", [&body, &id],
                ).unwrap();
                if early.is_ok() {
                    return Err("read refusal discarded or published original response".to_owned());
                }
            }
            let event = tokio::time::timeout(Duration::from_secs(8), &mut worker).await
                .map_err(|_| "original completion did not settle after policy/Stop".to_owned())?;
            joined = true;
            let event = event
                .map_err(|error| error.to_string())?.map_err(|error| error.to_string())?;
            let published = match event {
                Some((RuntimeEvent::AgentProvider(AgentProviderEvent::Completed { execution, .. }), _)) => {
                    execution.response.action_batch.is_some() && execution.response.raw_text.contains("retained-native-candidate")
                }
                _ => false,
            };
            Ok(published)
        }).catch_unwind().await;
        // Own and join the actual monitor before collecting producer drains.
        if !joined {
            if !worker.is_finished() {
                worker.abort();
            }
            let _ = worker.await;
        }
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
        (exercise, server_result, cleanup)
    };
    let ((exercise, server_result, cleanup), mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    let known = store.budget_account(&member.root).unwrap().known;
    let outstanding: i64 = rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COUNT(*) FROM (SELECT id FROM budget_attempts WHERE terminal=0 UNION SELECT attempt FROM budget_admissions WHERE retired_receipt IS NULL)", [], |row| row.get(0),
    ).unwrap();
    drop((exit, handle, drains));
    root.cleanup();
    cleanup.unwrap();
    assert!(server_result.unwrap_err().is_cancelled());
    let published = match exercise {
        Ok(result) => result.unwrap(),
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert_eq!(published, !stop);
    assert_eq!(
        sends.load(Ordering::SeqCst),
        1,
        "hold/release must not repeat inference"
    );
    assert_eq!(
        known,
        UsageTotals {
            input: 11,
            output: 3
        }
    );
    assert_eq!(outstanding, 0);
}
