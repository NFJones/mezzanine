//! Native monitor retirement through genuine actor admission and real execution.
//!
//! A synthetic accepted provider plan queues one native action. The actor mints
//! and claims its actual dispatch; the production executor owns the child,
//! readers and output. A bounded gate holds publication after execution so
//! monitor cancellation cannot be mistaken for blocking-worker completion.
//! Causal assertions run after the worker and original pane root are settled.

use super::*;

/// Owns one abort-capable harness until its actual exit is observed.
struct NativeProfileChild(std::process::Child);

impl Drop for NativeProfileChild {
    /// Reaps this exact harness on errors; unknown native fate remains unknown.
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Actual native blocking panic, result adoption and in-flight cancellation
/// must settle genuine actor claims on normal multithread worker profiles1/2/4.
/// A fresh exact-profile receipt after all checks and successful owned child
/// exit are both required; zero-selected tests or early exit cannot pass.
#[test]
fn native_monitor_faults_retire_claims_on_default_runtime_workers() {
    const CHILD: &str = "MEZ_NATIVE_MONITOR_FAULT_PROFILE_CHILD";
    const RECEIPT: &str = "MEZ_NATIVE_MONITOR_FAULT_PROFILE_RECEIPT";
    if let Some(workers) = std::env::var_os(CHILD) {
        let workers = workers.into_string().unwrap().parse::<usize>().unwrap();
        assert!([1, 2, 4].contains(&workers));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::spawn(Box::pin(async {
                qualify_native_blocking_panic().await;
                qualify_completed_native_retirement().await;
                qualify_native_cancellation().await;
                qualify_supervised_native_cancellation().await;
                qualify_supervised_cleanup_cancellation().await;
                qualify_provider_driver_panic().await;
            }))
            .await
            .unwrap();
        });
        drop(runtime);
        let receipt = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(std::env::var_os(RECEIPT).unwrap())
            .unwrap();
        serde_json::to_writer(
            receipt,
            &serde_json::json!({
                "schema": 1, "workers": workers, "cases": 6,
                "completed": true, "debug_assertions": cfg!(debug_assertions),
            }),
        )
        .unwrap();
        return;
    }
    let receipts = std::env::temp_dir().join(format!(
        "mez-native-monitor-fault-profiles-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&receipts).unwrap();
    for workers in [1, 2, 4] {
        let receipt = receipts.join(format!("{workers}.json"));
        let mut child = NativeProfileChild(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "host::async_runtime::client::provider_service::tests::incomplete::native_retirement::native_monitor_faults_retire_claims_on_default_runtime_workers", "--nocapture"])
                .env_remove("RUST_MIN_STACK").env_remove("RUST_TEST_THREADS")
                .env_remove("TOKIO_WORKER_THREADS")
                .env(CHILD, workers.to_string()).env(RECEIPT, &receipt)
                .spawn().unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(45);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "native monitor workers={workers} watchdog expired; native fate unproven"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "native monitor workers={workers} exit={status}"
        );
        let observed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
        assert_eq!(
            observed,
            serde_json::json!({
            "schema": 1, "workers": workers, "cases": 6,
                "completed": true, "debug_assertions": cfg!(debug_assertions),
            })
        );
        eprintln!("NATIVE_MONITOR_FAULT_PROFILE workers={workers} exit={status} receipt=verified");
    }
    std::fs::remove_dir_all(receipts).unwrap();
}

/// Queues one native action through the runtime's ordinary accepted-execution
/// path. The offline runner supplies a well-formed request/turn envelope; only
/// its fixture plan is replaced, as in the existing native liveness control.
/// No native outcome, claim generation or transaction marker is fabricated.
async fn native_action_fixture(command: String) -> (RuntimeSessionService, String, String) {
    let (mut service, agent, turn_id) = prepared_fixture("qualify native retirement").await;
    service.permission_policy_mut().set_approval_bypass(true);
    let dispatch = service
        .claim_configured_agent_provider_task(&agent, &turn_id)
        .unwrap()
        .unwrap();
    let provider = IncompleteProvider {
        requests: Mutex::new(Vec::new()),
        failures: 0,
        stream: false,
    };
    let (sender, _receiver) = tokio::sync::mpsc::channel(8);
    let observer = ProviderWireRequestObserver::new(
        &dispatch.turn.conversation_id,
        &dispatch.turn.pane_id,
        sender,
    );
    let mut execution = run_incomplete_fixture(&provider, &observer, &dispatch, 0, None)
        .await
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "native-retirement-action".into(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Return one owned native result".into(),
            command,
            interactive: false,
            stateful: false,
            timeout_ms: Some(5_000),
        },
    };
    execution.response.action_batch = Some(mez_agent::MaapBatch {
        rationale: "native retirement fixture".into(),
        actions: vec![action.clone()],
    });
    execution.action_results = vec![mez_agent::ActionResult::running(
        &dispatch.turn,
        &action,
        vec!["native action accepted".into()],
        Some(r#"{"state":"pending_dispatch"}"#.into()),
    )];
    execution.final_turn = true;
    execution.terminal_state = mez_agent::AgentTurnState::Running;
    service.remove_pending_agent_provider_task(&turn_id);
    service
        .apply_agent_provider_execution(
            &dispatch.turn,
            &dispatch.model_profile,
            &dispatch.model_profile.provider,
            execution,
        )
        .unwrap();
    let pending = service.pending_native_shell_actions();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, turn_id);
    (service, turn_id, action.id)
}

/// Cancellation of an outer monitor must await the real blocking producer and
/// adopt its original result, not strand the genuine native claim or reserved
/// completion slot. A result gate distinguishes producer retirement from async
/// task abortion; normal actor ingress remains responsive while it is held.
#[tokio::test]
async fn native_service_retirement_waits_for_worker_and_adopts_original_result() {
    Box::pin(qualify_completed_native_retirement()).await;
}

/// Shared actual native result-adoption body for current-thread and default
/// multithread subprocess profiles; all original actor/custody checks remain.
async fn qualify_completed_native_retirement() {
    let (service, turn_id, action_id) =
        native_action_fixture("printf 'native-retirement-original-\\162esult'".into()).await;
    let store = service.agent_transcript_store().unwrap().clone();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .conversation_id
        .clone();
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_native_shell_action(turn_id.clone(), action_id)
        .await
        .unwrap()
        .unwrap();
    let (produced, produced_receiver) = tokio::sync::oneshot::channel();
    let (finished, finished_receiver) = tokio::sync::oneshot::channel();
    let (release, release_receiver) = std::sync::mpsc::channel();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    spawn_native_shell_action_worker(
        &mut workers,
        &mut owners,
        handle.clone(),
        dispatch,
        move |dispatch, progress| {
            let outcome =
                crate::runtime::execute_native_shell_dispatch_with_progress(dispatch, progress);
            let _ = produced.send(outcome.clone());
            let released = release_receiver
                .recv_timeout(Duration::from_secs(5))
                .is_ok();
            let _ = finished.send(released);
            outcome
        },
    );
    let original = tokio::time::timeout(Duration::from_secs(5), produced_receiver)
        .await
        .unwrap()
        .unwrap();
    let mut retirement = Box::pin(abort_agent_provider_workers(
        &mut workers,
        &handle,
        &drains,
        &mut owners,
    ));
    let early = tokio::time::timeout(Duration::from_millis(50), &mut retirement).await;
    let returned_early = early.is_ok();
    let ingress = handle.lifecycle_state().await;
    let _ = release.send(());
    let result = match early {
        Ok(result) => result,
        Err(_) => retirement.as_mut().await,
    };
    drop(retirement);
    let gate_released = tokio::time::timeout(Duration::from_secs(5), finished_receiver)
        .await
        .unwrap()
        .unwrap();
    let mut replay = RuntimeEventBatch::new();
    replay.push(RuntimeEvent::NativeShell(original));
    let duplicate = handle.submit_runtime_events(replay).await;
    let persistence = crate::host::async_runtime::run_async_persistence_side_effect_service(
        &handle,
        crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
            max_polls: 16,
            drain_limit: 64,
            idle_interval: Duration::from_millis(1),
        },
        |polls, _| polls >= 16,
    )
    .await;
    let running = handle.agent_turn_is_running(&turn_id).await.unwrap();
    handle.shutdown().await.unwrap();
    let mut exit = actor.await.unwrap();
    exit.service.terminate_all_pane_processes().unwrap();
    let archive = store.inspect(&conversation);
    // The native executor returned before its gated publication, and the root
    // is reaped above. Failed baseline assertions therefore leave no live child.
    result.unwrap();
    assert_eq!(persistence.unwrap().failed, 0);
    assert_eq!(duplicate.unwrap().applied, 0);
    assert_eq!(
        archive
            .unwrap()
            .iter()
            .filter_map(
                |row| mez_agent::TranscriptContextEvent::from_transcript_content(&row.content)
            )
            .filter(|event| matches!(event,
                mez_agent::TranscriptContextEvent::ExecutionBlock {
                    source: ContextSourceKind::ActionResult, content, ..
                } if content.contains("native-retirement-original-result")
            ))
            .count(),
        1
    );
    assert!(ingress.is_ok(), "native retirement blocked actor ingress");
    assert!(
        gate_released,
        "native result gate expired before its actual release"
    );
    assert!(
        !running,
        "native service retirement stranded its genuine claim"
    );
    assert!(exit.service.pending_native_shell_actions().is_empty());
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Completed
    );
    assert!(owners.is_empty());
    assert!(drains.lock().unwrap().is_empty());
    assert!(
        !returned_early,
        "retirement returned before blocking producer exit"
    );
}

/// Drives the actual canonical worker, stops the actor and reaps its original
/// pane before returning any causal validation error to the owning regression.
async fn close_native_fixture(
    handle: &AsyncRuntimeSessionHandle,
    actor: tokio::task::JoinHandle<crate::host::async_runtime::AsyncRuntimeActorExit>,
) -> (
    crate::host::async_runtime::AsyncRuntimeActorExit,
    Result<()>,
) {
    let persistence = crate::host::async_runtime::run_async_persistence_side_effect_service(
        handle,
        crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
            max_polls: 16,
            drain_limit: 64,
            idle_interval: Duration::from_millis(1),
        },
        |polls, _| polls >= 16,
    )
    .await;
    let shutdown = handle.shutdown().await;
    let mut exit = actor.await.unwrap();
    let reaped = exit.service.terminate_all_pane_processes();
    let result = persistence
        .and_then(|report| {
            if report.failed == 0 {
                Ok(())
            } else {
                Err(MezError::invalid_state("native fixture persistence failed"))
            }
        })
        .and(shutdown.map(|_| ()))
        .and(reaped.map(|_| ()));
    (exit, result)
}

/// Cancellation before the monitor's first poll must retire the actor's real
/// issued native claim without evaluating the blocking executor at all. Exact
/// native completion validation, not an invented generation, accepts failure.
#[tokio::test(flavor = "current_thread")]
async fn native_retirement_before_first_poll_releases_claim_without_execution() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (service, turn_id, action_id) = native_action_fixture("printf unused".into()).await;
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_native_shell_action(turn_id, action_id)
        .await
        .unwrap()
        .unwrap();
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let monitor = spawn_native_shell_action_worker(
        &mut workers,
        &mut owners,
        handle.clone(),
        dispatch,
        move |dispatch, progress| {
            observed.fetch_add(1, Ordering::SeqCst);
            crate::runtime::execute_native_shell_dispatch_with_progress(dispatch, progress)
        },
    );
    monitor.abort();
    let retirement =
        abort_agent_provider_workers(&mut workers, &handle, &drains, &mut owners).await;
    let ingress = handle.lifecycle_state().await;
    let (exit, cleanup) = close_native_fixture(&handle, actor).await;
    cleanup.unwrap();
    retirement.unwrap();
    assert!(ingress.is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(exit.service.pending_native_shell_actions().is_empty());
    assert!(owners.is_empty());
}

/// A panic inside the actual blocking request boundary must settle the original
/// actor-issued turn/action/marker and preserve bounded failure facts, without
/// stopping actor ingress or publishing the arbitrary panic payload. Replaying
/// that same observed failure must not consume a second completion owner.
#[tokio::test]
async fn native_blocking_panic_settles_original_claim_with_bounded_failure() {
    Box::pin(qualify_native_blocking_panic()).await;
}

/// Shared genuine-claim blocking-panic body for ordinary and runtime profiles.
async fn qualify_native_blocking_panic() {
    let (service, turn_id, action_id) = native_action_fixture("printf unused".into()).await;
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_native_shell_action(turn_id.clone(), action_id.clone())
        .await
        .unwrap()
        .unwrap();
    let expected_marker = dispatch.marker.clone();
    let expected_started = dispatch.started_at_unix_ms;
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    spawn_native_shell_action_worker(
        &mut workers,
        &mut owners,
        handle.clone(),
        dispatch,
        |_, _| panic!("PRIVATE_NATIVE_BLOCKING_REQUEST_PANIC"),
    );
    let joined = workers.join_next_with_id().await.unwrap();
    let outcome = match &joined {
        Ok((_, Ok(Some((RuntimeEvent::NativeShell(outcome), false))))) => Some(outcome.clone()),
        _ => None,
    };
    let mut report = AsyncAgentProviderPollReport {
        polls: 0,
        executions: 0,
        idle_polls: 0,
        terminal_state: RuntimeLifecycleState::Running,
    };
    let recorded =
        record_owned_agent_provider_worker(joined, &handle, &mut report, &mut owners, &drains)
            .await;
    let duplicate = if let Some(outcome) = &outcome {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::NativeShell(outcome.clone()));
        Some(handle.submit_runtime_events(batch).await)
    } else {
        None
    };
    let ingress = handle.lifecycle_state().await;
    let (exit, cleanup) = close_native_fixture(&handle, actor).await;
    cleanup.unwrap();
    recorded.unwrap();
    let outcome = outcome.unwrap();
    assert_eq!(
        (outcome.turn_id, outcome.action_id, outcome.marker),
        (turn_id, action_id, expected_marker)
    );
    assert_eq!(outcome.started_at_unix_ms, expected_started);
    assert!(outcome.launch_counts.is_none());
    let failure = outcome.result.unwrap_err();
    assert_eq!(failure.message, "native shell worker join failed: panic");
    assert!(failure.lifecycle.is_none());
    assert_eq!(duplicate.unwrap().unwrap().applied, 0);
    assert!(ingress.is_ok());
    assert!(exit.service.pending_native_shell_actions().is_empty());
    assert!(owners.is_empty());
}

/// Service cancellation after a real native readiness barrier must observe the
/// blocking executor's actual interrupted result and settle its original claim.
/// A delayed mutation is observed past its scheduled time after exact worker
/// retirement; no fixture success or replacement command stands in for cleanup.
#[tokio::test]
async fn native_service_cancellation_observes_interruption_without_delayed_effect() {
    Box::pin(qualify_native_cancellation()).await;
}

/// Shared real native cancellation body with worker join and delayed-effect
/// checks; used unchanged on default multithread runtime workers.
async fn qualify_native_cancellation() {
    let root = std::env::temp_dir().join(format!(
        "mez-native-monitor-cancel-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    let ready = root.join("ready");
    let effect = root.join("forbidden");
    let command = format!(
        "printf ready > '{}'; /bin/sleep 2; : > '{}'",
        ready.display(),
        effect.display()
    );
    let (service, turn_id, action_id) = native_action_fixture(command).await;
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_native_shell_action(turn_id, action_id)
        .await
        .unwrap()
        .unwrap();
    let (finished, receiver) = tokio::sync::oneshot::channel();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    spawn_native_shell_action_worker(
        &mut workers,
        &mut owners,
        handle.clone(),
        dispatch,
        move |dispatch, progress| {
            let outcome =
                crate::runtime::execute_native_shell_dispatch_with_progress(dispatch, progress);
            let _ = finished.send(
                outcome
                    .result
                    .as_ref()
                    .is_ok_and(|output| output.interrupted),
            );
            outcome
        },
    );
    let readiness = tokio::time::timeout(Duration::from_secs(5), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let retirement =
        abort_agent_provider_workers(&mut workers, &handle, &drains, &mut owners).await;
    let interrupted = tokio::time::timeout(Duration::from_secs(5), receiver).await;
    let (exit, cleanup) = close_native_fixture(&handle, actor).await;
    let deadline = Instant::now() + Duration::from_millis(2_200);
    let mut delayed_effect = false;
    while Instant::now() < deadline {
        delayed_effect |= effect.exists();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    delayed_effect |= effect.exists();
    let removed = std::fs::remove_dir_all(root);
    cleanup.unwrap();
    removed.unwrap();
    readiness.unwrap();
    retirement.unwrap();
    assert!(interrupted.unwrap().unwrap());
    assert!(
        !delayed_effect,
        "cancelled native worker performed its delayed mutation"
    );
    assert!(exit.service.pending_native_shell_actions().is_empty());
    assert!(owners.is_empty());
}

/// Destroyed actor ingress must remain a fatal delivery failure even after
/// native resources have joined. Retirement cannot report successful result
/// adoption or discard known output merely because the destination disappeared.
#[tokio::test]
async fn native_retirement_actor_loss_remains_fatal_after_worker_join() {
    let (service, turn_id, action_id) = native_action_fixture("printf owned-result".into()).await;
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_native_shell_action(turn_id, action_id)
        .await
        .unwrap()
        .unwrap();
    let (produced, receiver) = tokio::sync::oneshot::channel();
    let (finished, finished_receiver) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    spawn_native_shell_action_worker(
        &mut workers,
        &mut owners,
        handle.clone(),
        dispatch,
        move |dispatch, progress| {
            let outcome =
                crate::runtime::execute_native_shell_dispatch_with_progress(dispatch, progress);
            let _ = produced.send(());
            let released = gate.recv_timeout(Duration::from_secs(5)).is_ok();
            let _ = finished.send(released);
            outcome
        },
    );
    tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .unwrap()
        .unwrap();
    handle.shutdown().await.unwrap();
    let mut exit = actor.await.unwrap();
    let _ = release.send(());
    let retirement =
        abort_agent_provider_workers(&mut workers, &handle, &drains, &mut owners).await;
    let finished = tokio::time::timeout(Duration::from_secs(5), finished_receiver).await;
    exit.service.terminate_all_pane_processes().unwrap();
    assert!(
        retirement.is_err(),
        "lost actor must not count as successful native adoption"
    );
    assert!(finished.unwrap().unwrap(), "native result gate expired");
    assert!(owners.is_empty());
}

/// The actual service supervisor aborts an auxiliary service future on shutdown.
/// Its retained request custody must outlive that future: supervision cannot
/// complete before the blocking producer joins or strand the genuine claim.
/// A real executor result is gated after child cleanup, and every causal check
/// runs after the blocking producer and original pane root have retired.
#[tokio::test]
async fn native_supervisor_cancellation_retains_service_worker_and_original_outcome() {
    Box::pin(qualify_supervised_native_cancellation()).await;
}

/// Shared production-builder/supervisor takeover for ordinary and default
/// runtime profiles; real request custody and all completion checks remain.
async fn qualify_supervised_native_cancellation() {
    let (service, turn_id, action_id) =
        native_action_fixture("printf supervisor-owned-native".into()).await;
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_native_shell_action(turn_id.clone(), action_id)
        .await
        .unwrap()
        .unwrap();
    let (produced, produced_receiver) = tokio::sync::oneshot::channel();
    let (finished, finished_receiver) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let service = crate::host::async_runtime::provider::build_async_agent_provider_service(
        "native-retirement-fixture",
        handle.clone(),
        AsyncAgentProviderServiceConfig::new(1).unwrap(),
    )
    .unwrap();
    let retirement = service.agent_provider_retirement.as_ref().unwrap().clone();
    {
        let mut state = retirement.work.state.lock().await;
        let ProviderServiceState {
            workers, owners, ..
        } = &mut *state;
        spawn_native_shell_action_worker(
            workers,
            owners,
            handle.clone(),
            dispatch,
            move |dispatch, progress| {
                let outcome =
                    crate::runtime::execute_native_shell_dispatch_with_progress(dispatch, progress);
                let _ = produced.send(());
                let released = gate.recv_timeout(Duration::from_secs(5)).is_ok();
                let _ = finished.send(released);
                outcome
            },
        );
    }
    let primary = crate::host::async_runtime::AsyncRuntimeService::new("primary-fixture", async {
        std::future::pending::<()>().await;
        Ok(crate::host::async_runtime::AsyncRuntimeServiceExit::completed(0))
    });
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let mut supervision = tokio::spawn(
        crate::host::async_runtime::supervise_async_runtime_services(
            vec![service, primary],
            async {
                let _ = cancelled.await;
            },
        ),
    );
    tokio::time::timeout(Duration::from_secs(5), produced_receiver)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !retirement.driver_started_for_tests() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.send(()).unwrap();
    let early = tokio::time::timeout(Duration::from_millis(50), &mut supervision).await;
    let returned_early = early.is_ok();
    let _ = release.send(());
    let result = match early {
        Ok(result) => result,
        Err(_) => supervision.await,
    };
    let finished = tokio::time::timeout(Duration::from_secs(5), finished_receiver).await;
    let running = handle.agent_turn_is_running(&turn_id).await;
    let (exit, cleanup) = close_native_fixture(&handle, actor).await;
    cleanup.unwrap();
    result.unwrap().unwrap();
    assert!(finished.unwrap().unwrap(), "native result gate expired");
    assert!(
        !running.unwrap(),
        "supervisor retirement left the live native turn running"
    );
    assert!(
        exit.service.pending_native_shell_actions().is_empty(),
        "supervisor cancellation stranded the original native claim"
    );
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert!(
        !returned_early,
        "supervision completed before its native producer exited"
    );
}

/// Supervisor cancellation can interrupt the normal service's cleanup after a
/// shared failure. The real native cancellation flag proves cleanup began
/// cancelling work before supervisor abort; it must keep the producer join alive,
/// preserve the first typed error and adopt the original completed native result.
#[tokio::test]
async fn native_supervisor_abort_during_cleanup_preserves_primary_and_worker_custody() {
    Box::pin(qualify_supervised_cleanup_cancellation()).await;
}

/// Shared actual cleanup interruption with the original typed causal error.
async fn qualify_supervised_cleanup_cancellation() {
    use std::sync::atomic::Ordering;
    let (service, turn_id, action_id) =
        native_action_fixture("printf cleanup-owned-native".into()).await;
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_native_shell_action(turn_id.clone(), action_id)
        .await
        .unwrap()
        .unwrap();
    let cancellation = dispatch.cancellation.flag();
    let service = crate::host::async_runtime::provider::build_async_agent_provider_service(
        "cleanup-retirement-fixture",
        handle.clone(),
        AsyncAgentProviderServiceConfig::new(1).unwrap(),
    )
    .unwrap();
    let retirement = service.agent_provider_retirement.as_ref().unwrap().clone();
    let (produced, produced_receiver) = tokio::sync::oneshot::channel();
    let (finished, finished_receiver) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let (fail, failure) = tokio::sync::oneshot::channel();
    {
        let mut state = retirement.work.state.lock().await;
        let ProviderServiceState {
            workers, owners, ..
        } = &mut *state;
        spawn_native_shell_action_worker(
            workers,
            owners,
            handle.clone(),
            dispatch,
            move |dispatch, progress| {
                let outcome =
                    crate::runtime::execute_native_shell_dispatch_with_progress(dispatch, progress);
                let _ = produced.send(());
                let released = gate.recv_timeout(Duration::from_secs(5)).is_ok();
                let _ = finished.send(released);
                outcome
            },
        );
        spawn_owned_agent_provider_worker(workers, owners, None, async move {
            let _ = failure.await;
            Err(MezError::conflict("owned primary service failure")
                .with_provider_failure_json(r#"{"error":{"code":"owned-primary-control"}}"#))
        });
    }
    let primary = crate::host::async_runtime::AsyncRuntimeService::new("primary-fixture", async {
        std::future::pending::<()>().await;
        Ok(crate::host::async_runtime::AsyncRuntimeServiceExit::completed(0))
    });
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let mut supervision = tokio::spawn(
        crate::host::async_runtime::supervise_async_runtime_services(
            vec![service, primary],
            async {
                let _ = cancelled.await;
            },
        ),
    );
    tokio::time::timeout(Duration::from_secs(5), produced_receiver)
        .await
        .unwrap()
        .unwrap();
    fail.send(()).unwrap();
    let draining = tokio::time::timeout(Duration::from_secs(5), async {
        while !cancellation.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    cancel.send(()).unwrap();
    let early = tokio::time::timeout(Duration::from_millis(50), &mut supervision).await;
    let returned_early = early.is_ok();
    let _ = release.send(());
    let result = match early {
        Ok(result) => result,
        Err(_) => supervision.await,
    };
    let finished = tokio::time::timeout(Duration::from_secs(5), finished_receiver).await;
    let running = handle.agent_turn_is_running(&turn_id).await;
    let (exit, cleanup) = close_native_fixture(&handle, actor).await;
    cleanup.unwrap();
    draining.unwrap();
    assert!(finished.unwrap().unwrap(), "native result gate expired");
    let error = result.unwrap().unwrap_err();
    assert_eq!(error.kind(), MezErrorKind::Conflict);
    assert_eq!(
        error.provider_failure_json(),
        Some(r#"{"error":{"code":"owned-primary-control"}}"#)
    );
    assert!(
        !running.unwrap(),
        "cleanup cancellation lost the native result"
    );
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert!(
        !returned_early,
        "cleanup cancellation detached native producer custody"
    );
}

/// Shared driver panic remains fatal to its service, but cannot drop an already
/// admitted native sibling or leak arbitrary panic text through structured
/// service diagnostics. The real result is held until supervision is waiting
/// on retirement, then adopted once before the actor and pane root are closed.
#[tokio::test]
async fn native_provider_driver_panic_remains_fatal_and_retires_native_sibling() {
    Box::pin(qualify_provider_driver_panic()).await;
}

/// Injects only the shared driver future, preserving the genuine actor claim,
/// actual blocking native executor and supervisor-retained cleanup path.
async fn qualify_provider_driver_panic() {
    let (service, turn_id, action_id) =
        native_action_fixture("printf driver-panic-owned-result".into()).await;
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_native_shell_action(turn_id.clone(), action_id)
        .await
        .unwrap()
        .unwrap();
    let retirement = AgentProviderServiceRetirement::new(handle.clone());
    let (produced, ready) = tokio::sync::oneshot::channel();
    let (finished, finished_receiver) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    {
        let mut state = retirement.work.state.lock().await;
        let ProviderServiceState {
            workers, owners, ..
        } = &mut *state;
        spawn_native_shell_action_worker(
            workers,
            owners,
            handle.clone(),
            dispatch,
            move |dispatch, progress| {
                let outcome =
                    crate::runtime::execute_native_shell_dispatch_with_progress(dispatch, progress);
                let _ = produced.send(());
                let released = gate.recv_timeout(Duration::from_secs(5)).is_ok();
                let _ = finished.send(released);
                outcome
            },
        );
    }
    tokio::time::timeout(Duration::from_secs(5), ready)
        .await
        .unwrap()
        .unwrap();
    let worker_retirement = retirement.clone();
    let mut service = crate::host::async_runtime::AsyncRuntimeService::new_auxiliary(
        "owned-driver-panic",
        async move {
            worker_retirement
                .start_driver(async { panic!("PRIVATE_SHARED_PROVIDER_DRIVER_PANIC") })
                .await?;
            let report = worker_retirement.join_driver().await?;
            Ok(crate::host::async_runtime::AsyncRuntimeServiceExit::completed(report.executions))
        },
    );
    service.agent_provider_retirement = Some(retirement);
    let mut supervision = tokio::spawn(
        crate::host::async_runtime::supervise_async_runtime_services(
            vec![service],
            std::future::pending(),
        ),
    );
    let early = tokio::time::timeout(Duration::from_millis(50), &mut supervision).await;
    let returned_early = early.is_ok();
    let _ = release.send(());
    let result = match early {
        Ok(result) => result,
        Err(_) => supervision.await,
    };
    let finished = tokio::time::timeout(Duration::from_secs(5), finished_receiver).await;
    let ingress = handle.lifecycle_state().await;
    let (exit, cleanup) = close_native_fixture(&handle, actor).await;
    cleanup.unwrap();
    assert!(finished.unwrap().unwrap(), "native result gate expired");
    let error = result.unwrap().unwrap_err();
    assert_eq!(error.kind(), MezErrorKind::InvalidState);
    assert!(error.message().contains("driver task failed: panic"));
    assert!(
        !error
            .message()
            .contains("PRIVATE_SHARED_PROVIDER_DRIVER_PANIC")
    );
    assert!(ingress.is_ok());
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert!(
        !returned_early,
        "driver panic detached the admitted native sibling"
    );
}
