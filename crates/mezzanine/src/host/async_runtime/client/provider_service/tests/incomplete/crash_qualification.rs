//! Subprocess-isolated main-inference unwind/cancellation on default task stacks.
//!
//! Three independent genuine actors admit provider claims concurrently. One
//! fails or is stopped; the other two run the real observed product turn runner
//! only after that failure settles. Synthetic providers replace paid transport,
//! not actor admission, failure completion, persistence or native root cleanup.
//! This qualifies main inference, not auxiliary or native-action panic paths.

use super::*;
use std::sync::Arc;

/// Retains the waitable harness even if an assertion or OS observation fails.
/// Killing/reaping this exact child does not establish native grandchild fate.
struct ProfileChild(std::process::Child);

impl Drop for ProfileChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Owns each abort-capable runtime profile as a waitable process. The child uses
/// Tokio's production multi-thread builder with its default thread stack; only
/// the bounded worker count varies. No serial-workspace or enlarged-stack oracle
/// is used. Timeout reaps the exact harness and explicitly leaves native fate
/// unproven rather than fabricating successful cleanup after a hard abort.
#[test]
fn concurrent_inference_fault_profiles_preserve_actor_siblings() {
    const CHILD: &str = "MEZ_INFERENCE_FAULT_PROFILE_CHILD";
    const RECEIPT: &str = "MEZ_INFERENCE_FAULT_PROFILE_RECEIPT";
    if let Some(profile) = std::env::var_os(CHILD) {
        let profile = profile.into_string().unwrap();
        let (workers, fault) = profile.split_once(':').unwrap();
        let workers = workers.parse::<usize>().unwrap();
        assert!([1, 2, 4].contains(&workers));
        assert!(["panic", "stop"].contains(&fault));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            .build()
            .unwrap();
        let actors = runtime.block_on(Box::pin(qualify_inference_fault(fault == "stop")));
        let receipt = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(std::env::var_os(RECEIPT).unwrap())
            .unwrap();
        serde_json::to_writer(
            receipt,
            &serde_json::json!({
                "schema": 1, "workers": workers, "fault": fault, "actors": actors,
                "completed": true, "debug_assertions": cfg!(debug_assertions),
            }),
        )
        .unwrap();
        eprintln!(
            "INFERENCE_FAULT_PROFILE workers={workers} fault={fault} actors=3 stage=complete debug_assertions={}",
            cfg!(debug_assertions)
        );
        return;
    }
    let receipts = std::env::temp_dir().join(format!(
        "mez-inference-profiles-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&receipts).unwrap();
    for workers in [1, 2, 4] {
        for fault in ["panic", "stop"] {
            let receipt = receipts.join(format!("{workers}-{fault}.json"));
            let mut child = ProfileChild(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        concat!(
                            module_path!(),
                            "::concurrent_inference_fault_profiles_preserve_actor_siblings"
                        )
                        .strip_prefix("mezzanine::")
                        .unwrap(),
                        "--nocapture",
                    ])
                    .env(CHILD, format!("{workers}:{fault}"))
                    .env(RECEIPT, &receipt)
                    .env_remove("RUST_MIN_STACK")
                    .env_remove("RUST_TEST_THREADS")
                    .env_remove("TOKIO_WORKER_THREADS")
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(45);
            let status = loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    let _ = child.0.kill();
                    let status = child.0.wait().unwrap();
                    panic!(
                        "owned inference profile timed out: workers={workers} fault={fault} exit={status}; native fate unproven"
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            assert!(
                status.success(),
                "inference profile workers={workers} fault={fault} exit={status}; no native cleanup inferred on failure"
            );
            let actual: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
            assert_eq!(
                actual,
                serde_json::json!({
                    "schema": 1, "workers": workers, "fault": fault, "actors": 3,
                    "completed": true, "debug_assertions": cfg!(debug_assertions),
                })
            );
            eprintln!(
                "INFERENCE_FAULT_PROFILE workers={workers} fault={fault} exit={status} receipt=verified"
            );
        }
    }
    std::fs::remove_dir_all(receipts).unwrap();
}

/// Executes the actual inference monitor and canonical actor completion paths.
/// The barrier requires all three genuine claims to reach their request future;
/// healthy requests wait until the fault actor has settled before producing a
/// response. Native roots stay manager-owned and are reaped after actor exit.
async fn qualify_inference_fault(stop: bool) -> usize {
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let (fault_settled, _) = watch::channel(false);
    let mut cases = JoinSet::new();
    for index in 0..3 {
        let barrier = barrier.clone();
        let fault_settled = fault_settled.clone();
        cases.spawn(async move {
            let prompt = "offline qualification source ".repeat(512);
            let (mut service, agent, turn_id) = Box::pin(prepared_fixture(&prompt)).await;
            let primary = service.attach_primary("fault-profile", true,
                mez_mux::layout::Size::new(80, 24).unwrap(), 120).unwrap();
            let (handle, actor) = crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
            let actor = tokio::spawn(actor.run());
            let dispatch = handle.claim_configured_agent_provider_task(agent.clone(), turn_id.clone()).await.unwrap().unwrap();
            let generation = dispatch.claim_generation;
            assert!(generation > 0);
            let conversation = dispatch.turn.conversation_id.clone();
            let claim_timers = handle.drain_timer_side_effects(1024).await.unwrap();
            let mut exact = claim_timers.into_iter().filter_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == crate::runtime::RuntimeTimerKind::ProviderClaim
                        && key.owner_id == turn_id && key.generation == generation => Some(key),
                _ => None,
            });
            let claim_timer = exact.next().expect("actual actor claim schedules its lease");
            assert!(exact.next().is_none());
            let drains: ProviderObservationDrains = Default::default();
            let ready = Arc::new(tokio::sync::Notify::new());
            let worker_ready = ready.clone();
            let mut fault_observer = fault_settled.subscribe();
            let monitor = tokio::spawn(monitor_agent_provider_future(
                handle.clone(), agent.clone(), turn_id.clone(), "%1".into(), generation, drains.clone(),
                move |progress, sender, pending| async move {
                    barrier.wait().await;
                    if index == 0 {
                        worker_ready.notify_one();
                        if stop {
                            return std::future::pending::<Result<RuntimeAgentProviderWorkerOutcome>>().await;
                        }
                        panic!("injected private inference payload");
                    }
                    fault_observer.wait_for(|settled| *settled).await.unwrap();
                    let provider = IncompleteProvider {requests: Mutex::new(Vec::new()), failures: 0, stream: false};
                    let observer = ProviderWireRequestObserver::new(&dispatch.turn.conversation_id, "%1", sender)
                        .with_claim_generation(generation).with_pending_observations(pending);
                    let execution = run_incomplete_fixture(&provider, &observer, &dispatch, 0, Some(progress)).await?;
                    assert_eq!(provider.requests.lock().unwrap().len(), 1);
                    Ok(RuntimeAgentProviderWorkerOutcome::Execution(Box::new(execution)))
                },
            ));
            if index == 0 && stop {
                ready.notified().await;
                handle.execute_agent_shell_command(primary, "/stop".into()).await.unwrap();
            }
            let result = tokio::time::timeout(Duration::from_secs(10), monitor).await.unwrap().unwrap().unwrap();
            assert!(drains.lock().unwrap().is_empty());
            if index == 0 && stop {
                assert!(result.is_none());
            } else {
                let (event, completed) = result.as_ref().unwrap();
                assert_eq!(*completed, index != 0);
                if index == 0 {
                    assert!(matches!(event, RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
                        turn_id: exact, claim_generation, message, ..
                    }) if exact == &turn_id && *claim_generation == generation && message == "provider worker join failed: panic"));
                }
                let duplicate_failure = (index == 0).then(|| result.clone());
                let mut report = AsyncAgentProviderPollReport {
                    polls: 0, executions: 0, idle_polls: 0,
                    terminal_state: RuntimeLifecycleState::Running,
                };
                record_joined_agent_provider_worker(Ok(Ok(result)), &handle, &mut report).await.unwrap();
                if let Some(duplicate) = duplicate_failure {
                    record_joined_agent_provider_worker(Ok(Ok(duplicate)), &handle, &mut report).await.unwrap();
                }
                let report = crate::host::async_runtime::run_async_persistence_side_effect_service(
                    &handle, crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
                        max_polls: 16, drain_limit: 64, idle_interval: Duration::from_millis(1),
                    }, |polls, _| polls >= 16,
                ).await.unwrap();
                assert_eq!(report.failed, 0);
            }
            assert!(!handle.agent_turn_is_running(&turn_id).await.unwrap());
            let mut late = RuntimeEventBatch::new();
            late.push(RuntimeEvent::Timer(crate::runtime::TimerEvent {key: claim_timer, now_ms: 0}));
            assert_eq!(handle.submit_runtime_events(late).await.unwrap().applied, 0);
            if index == 0 {
                fault_settled.send_replace(true);
            }
            // A real actor request succeeds after the local fault, not merely a
            // surviving watch sender or a queued sibling task.
            handle.agent_turn_is_running(&turn_id).await.unwrap();
            handle.shutdown().await.unwrap();
            let mut exit = actor.await.unwrap();
            assert!(!exit.service.agent_provider_claim_matches(&agent, &turn_id, generation));
            assert_eq!(exit.service.agent_turn_ledger().turn(&turn_id).unwrap().state,
                if index != 0 { AgentTurnState::Completed } else if stop { AgentTurnState::Interrupted } else { AgentTurnState::Failed });
            if index != 0 {
                let usage = exit.service.agent_token_usage_for_conversation(&conversation);
                assert_eq!(usage[&mez_agent::ModelTokenUsageKey::new("local-chat", "test-model")].input_tokens, 5);
            }
            let roots = exit.service.terminate_all_pane_processes().unwrap();
            assert_eq!(roots.len(), 1);
            assert!(exit.service.tracked_runtime_pane_process_ids().is_empty());
        });
    }
    let mut actors = 0;
    while let Some(result) = cases.join_next().await {
        result.unwrap();
        actors += 1;
    }
    assert_eq!(actors, 3);
    actors
}
