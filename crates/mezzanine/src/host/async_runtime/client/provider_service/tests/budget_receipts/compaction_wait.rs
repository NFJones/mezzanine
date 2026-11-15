//! Real compaction source, actor lease and monitored original model wave.
//! Only generated stores and loopback HTTP are used; cleanup precedes oracles.

use super::*;
use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
use crate::integrations::http::ReqwestProviderHttpTransport;
use futures_util::FutureExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn budget_compaction_original_wave_resumes_after_policy_release() {
    Box::pin(qualify(false, None)).await;
}

#[tokio::test]
async fn budget_compaction_held_stop_never_revives_original_wave() {
    Box::pin(qualify(true, None)).await;
}

#[tokio::test]
async fn budget_compaction_human_pause_remains_orthogonal_in_both_release_orders() {
    Box::pin(qualify(false, Some(true))).await;
    Box::pin(qualify(false, Some(false))).await;
}

/// Shared by wait and competition owners; the caller retains the exact root.
/// Only competition opts into generated titles. Existing wait config is intact.
pub(super) fn queue_original_wave(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
    generated_title: bool,
) -> u64 {
    let title_policy = if generated_title {
        "session_title_policy=\"generated\"\n"
    } else {
        ""
    };
    service.replace_config_layers(vec![ConfigLayer {
        name: "compaction-original-wait".into(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: format!("[agents]\nrouting=false\n{title_policy}shell_mode=\"native\"\ndefault_provider=\"local-chat\"\ndefault_model_profile=\"default\"\n[permissions]\nsandbox=\"policy-only\"\n[providers.local-chat]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:1/v1\"\nmodels=[\"test-model\"]\ndefault_model=\"test-model\"\n[model_profiles.default]\nprovider=\"local-chat\"\nmodel=\"test-model\"\ncontext_window_tokens=40000\n"),
    }]).unwrap();
    let turn = crate::test_support::compaction_source::complete_task_and_admit_successor(
        service,
        primary,
        &format!("original source {}", "evidence ".repeat(6_000)),
        "completed evidence",
        "continue original",
        mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
    );
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    service
        .schedule_agent_provider_retry_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            &turn,
            ProviderErrorRetryClass::ContextLimit,
            &error,
        )
        .unwrap()
        .unwrap();
    service
        .pending_agent_compaction_task_generation("%1")
        .unwrap()
}

async fn qualify(stop: bool, budget_first: Option<bool>) {
    let Prepared {
        mut service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits {
        input: mez_agent::token_budget::TokenLimit::Limited(0),
        output: mez_agent::token_budget::TokenLimit::Unlimited,
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
    if budget_first.is_some() {
        service
            .session_mut_for_tests()
            .set_pane_live_state("%1", false)
            .unwrap();
    }
    service.set_config_root(store.path().parent().unwrap().to_owned());
    let generation = queue_original_wave(&mut service, &primary, false);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
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
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0 && bytes.len() + n <= 1024 * 1024);
            bytes.extend_from_slice(&chunk[..n]);
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
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
        }
        count.fetch_add(1, Ordering::SeqCst);
        let text = serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"done"}]}).to_string();
        let body = serde_json::json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}],"usage":{"prompt_tokens":11,"completion_tokens":3}}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    let drains: ProviderObservationDrains = Default::default();
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let control = async {
        let mut dispatch = handle
            .claim_agent_compaction_task("%1".into(), generation)
            .await
            .unwrap()
            .unwrap();
        let lease_delay = handle
            .drain_timer_side_effects(64)
            .await
            .unwrap()
            .iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == crate::runtime::RuntimeTimerKind::CompactionClaim
                        && key.owner_id == "%1"
                        && key.generation == generation =>
                {
                    Some(*delay_ms)
                }
                _ => None,
            })
            .expect("actual actor-admitted compaction lease");
        dispatch.provider = crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(
            OpenAiCompatibleChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)
                .unwrap()
                .with_provider_id("local-chat")
                .unwrap()
                .with_streaming_options_for_test(false)
                .unwrap()
                .with_endpoint(endpoint),
        );
        let waiter = handle.clone();
        let owner = ProviderWorkerOwner::compaction(&dispatch);
        let mut workers = JoinSet::new();
        let mut owners = Default::default();
        let admitted = spawn_owned_agent_provider_worker(
            &mut workers,
            &mut owners,
            Some(owner),
            monitor_runtime_agent_compaction_dispatch(
                handle.clone(),
                "%1".into(),
                dispatch,
                drains.clone(),
                move |dispatch, observer| {
                    execute_runtime_agent_compaction_dispatch_with_waiter(
                        dispatch,
                        observer,
                        Some(waiter),
                    )
                },
            ),
        );
        assert!(admitted.is_some());
        // Keep the original producer/monitor registered, not a retry or replacement.
        let exercise = std::panic::AssertUnwindSafe(async {
            let parked = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if handle
                        .budget_compaction_claim_is_quiescent("%1".into(), generation)
                        .await
                        .unwrap()
                    {
                        return true;
                    }
                    if workers.is_empty() {
                        return false;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap_or(false);
            let mut retained =
                parked && sends.load(Ordering::SeqCst) == 0 && rows(&store, "budget_attempts") == 0;
            // Deliver the exact original watchdog after its former deadline.
            // This actor boundary must supervise the parked future, not expire
            // it or fabricate another runnable compaction task.
            let mut tick = crate::runtime::RuntimeEventBatch::new();
            tick.push(RuntimeEvent::Timer(crate::runtime::TimerEvent {
                key: crate::runtime::RuntimeTimerKey::new(
                    crate::runtime::RuntimeTimerKind::CompactionClaim,
                    "%1",
                    generation,
                ),
                now_ms: crate::runtime::current_unix_millis().saturating_add(lease_delay + 1),
            }));
            retained &= handle.submit_runtime_events(tick).await.unwrap().applied == 0
                && handle
                    .budget_compaction_claim_is_quiescent("%1".into(), generation)
                    .await
                    .unwrap();
            if let Some(false) = budget_first {
                waiting::browser_input(&handle, &primary, b"/list-agents --all\r").await;
                waiting::browser_input(&handle, &primary, b"p").await;
                waiting::assert_browser_paused(&handle, "%1", true).await;
                waiting::browser_input(&handle, &primary, b"p").await;
            } else if budget_first == Some(true) {
                waiting::browser_input(&handle, &primary, b"/list-agents --all\r").await;
                waiting::browser_input(&handle, &primary, b"p").await;
                waiting::assert_browser_paused(&handle, "%1", true).await;
            }
            if stop {
                handle
                    .execute_agent_shell_command(primary.clone(), "/stop".into())
                    .await
                    .unwrap();
            }
            let writer = store.clone();
            tokio::task::spawn_blocking(move || {
                let projection = writer.budget_policy_projection().unwrap();
                let prior = projection.committed.unwrap();
                let operation = BudgetPolicyPublication {
                    namespace: projection.namespace,
                    id: BudgetReceiptId::new("compaction-release").unwrap(),
                    expected_revision: projection.revision,
                    previous_digest: prior.digest,
                    proposed: BudgetConfigPolicy {
                        source: prior.source,
                        digest: "e".repeat(64),
                        limits: BudgetLimits::default(),
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
            if budget_first == Some(true) {
                tokio::time::sleep(Duration::from_millis(2100)).await;
                waiting::assert_browser_paused(&handle, "%1", true).await;
                retained &= handle
                    .budget_compaction_claim_is_quiescent("%1".into(), generation)
                    .await
                    .unwrap()
                    && sends.load(Ordering::SeqCst) == 0;
                waiting::browser_input(&handle, &primary, b"p").await;
            }
            let result = tokio::time::timeout(Duration::from_secs(15), workers.join_next()).await;
            (retained, result)
        })
        .catch_unwind()
        .await;
        workers.abort_all();
        while workers.join_next().await.is_some() {}
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
        let _ = server.await;
        handle
            .execute_agent_shell_command(primary, "/stop".into())
            .await
            .unwrap();
        handle.shutdown().await.unwrap();
        (exercise, cleanup)
    };
    let ((exercise, cleanup), mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    let known = store.budget_account(&member.root).unwrap().known;
    let outstanding: i64 = rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COUNT(*) FROM (SELECT id FROM budget_attempts WHERE terminal=0 UNION SELECT attempt FROM budget_admissions WHERE retired_receipt IS NULL)", [], |row| row.get(0),
    ).unwrap();
    drop((exit, handle, drains));
    if matches!(cleanup, Ok(Ok(()))) {
        root.cleanup();
    }
    cleanup.unwrap().unwrap();
    let (retained, result) = exercise.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    assert!(
        retained,
        "original compaction wave terminalized instead of parking"
    );
    assert_eq!(outstanding, 0);
    let completion = result
        .expect("original compactor timed out")
        .unwrap()
        .unwrap()
        .unwrap();
    if stop {
        assert!(
            completion.is_none()
                || matches!(
                    completion,
                    Some((
                        RuntimeEvent::AgentCompaction(AgentCompactionEvent::Failed { .. }),
                        _
                    ))
                )
        );
    } else {
        assert!(matches!(
            completion.unwrap().0,
            RuntimeEvent::AgentCompaction(AgentCompactionEvent::Completed { .. })
        ));
    }
    assert_eq!(sends.load(Ordering::SeqCst), usize::from(!stop));
    assert_eq!(
        known,
        if stop {
            UsageTotals::default()
        } else {
            UsageTotals {
                input: 11,
                output: 3,
            }
        }
    );
}
