//! Async-runtime tests owned by pane supervision behavior.

use super::super::*;

/// Exact native PID plus optional host start token, never a process scan.
fn native_retirement_witness(pid: u32) -> (rustix::process::Pid, Option<u64>) {
    (
        rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap(),
        mez_mux::process::process_start_token_for_pid(pid),
    )
}

/// Successful retirement must already have reaped the original native child.
fn assert_native_retired(witness: (rustix::process::Pid, Option<u64>)) {
    assert_eq!(
        rustix::process::test_kill_process(witness.0),
        Err(rustix::io::Errno::SRCH),
        "completed operation left original native child present: {witness:?}"
    );
}

/// Real backend adapter gating only terminate/reap, not replacing native I/O.
struct GatedNativeRetirement {
    backend: AsyncPtyPaneProcessIo,
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    release: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl AsyncPaneProcessIo for GatedNativeRetirement {
    fn read_output<'a>(
        &'a mut self,
        max_bytes: usize,
    ) -> crate::host::async_runtime::pane_io::AsyncPaneIoFuture<'a, Option<Vec<u8>>> {
        self.backend.read_output(max_bytes)
    }
    fn output_activity<'a>(
        &'a mut self,
    ) -> Option<crate::host::async_runtime::pane_io::AsyncPaneIoFuture<'a, ()>> {
        self.backend.output_activity()
    }
    fn write_input<'a>(
        &'a mut self,
        bytes: &'a [u8],
    ) -> crate::host::async_runtime::pane_io::AsyncPaneIoFuture<'a, usize> {
        self.backend.write_input(bytes)
    }
    fn poll_exit<'a>(
        &'a mut self,
    ) -> crate::host::async_runtime::pane_io::AsyncPaneIoFuture<'a, Option<ProcessEvent>> {
        self.backend.poll_exit()
    }
    fn foreground_process<'a>(
        &'a mut self,
    ) -> crate::host::async_runtime::pane_io::AsyncPaneIoFuture<
        'a,
        Option<AsyncPaneForegroundProcess>,
    > {
        self.backend.foreground_process()
    }
    fn resize<'a>(
        &'a mut self,
        size: Size,
    ) -> crate::host::async_runtime::pane_io::AsyncPaneIoFuture<'a, ()> {
        self.backend.resize(size)
    }
    fn terminate<'a>(
        &'a mut self,
        force: bool,
    ) -> crate::host::async_runtime::pane_io::AsyncPaneIoFuture<'a, ProcessEvent> {
        Box::pin(async move {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            if let Some(release) = self.release.take() {
                release
                    .await
                    .map_err(|_| MezError::invalid_state("native fixture gate closed"))?;
            }
            self.backend.terminate(force).await
        })
    }
}

/// Exercises the real consuming worker with termination deliberately held.
/// It must keep the actor responsive and remain pending while the exact child
/// is unreaped; releasing the same operation must produce immediate ESRCH.
#[tokio::test]
async fn owned_native_worker_waits_for_gated_termination_and_reap() {
    let mut service = test_service();
    let start = service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let witness = native_retirement_witness(start.primary_pid);
    let native = service.native_pane_retirement_owner();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let actor = tokio::spawn(actor.run());
    let (instance, process) = handle
        .take_running_pane_process_instances_for_adapter(1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let backend = GatedNativeRetirement {
        backend: AsyncPtyPaneProcessIo::new(start.pane_id, process).unwrap(),
        entered: Some(entered),
        release: Some(release_rx),
    };
    let driver = AsyncPaneProcessDriver::new_for_instance(
        instance.clone(),
        backend,
        AsyncPaneProcessDriverConfig::default(),
    )
    .unwrap();
    let mut worker = tokio::spawn(run_owned_pane_process_worker(
        handle.clone(),
        instance,
        start.primary_pid,
        driver,
        AsyncPaneProcessServiceConfig {
            max_polls: 1,
            ..Default::default()
        },
    ));
    entered_rx.await.unwrap();
    let early = tokio::time::timeout(Duration::from_millis(30), &mut worker).await;
    let alive = rustix::process::test_kill_process(witness.0);
    handle.lifecycle_state().await.unwrap();
    release.send(()).unwrap();
    let completion = if early.is_err() {
        Some(tokio::time::timeout(Duration::from_secs(2), &mut worker).await)
    } else {
        None
    };
    assert!(
        early.is_err(),
        "consuming worker returned before gated native completion: {early:?}"
    );
    assert_eq!(alive, Ok(()));
    assert!(matches!(completion, Some(Ok(Ok(Ok(_))))), "{completion:?}");
    assert_native_retired(witness);
    assert_eq!(native.outstanding(), 0);
    handle.shutdown().await.unwrap();
    actor.await.unwrap();
}

/// Covers both bounded supervisor exits through actual manager handoff and
/// native workers. Neither should_stop nor max_polls may abort the only waiter
/// and return a healthy report while its original child is still present.
#[tokio::test]
async fn native_supervisor_stop_and_poll_limit_reap_before_return() {
    for predicate_stop in [false, true] {
        let mut service = test_service();
        let start = service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        let witness = native_retirement_witness(start.primary_pid);
        let native = service.native_pane_retirement_owner();
        let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
            .build()
            .unwrap();
        let actor = tokio::spawn(actor.run());
        let report = run_async_pane_process_supervisor_service(
            handle.clone(),
            AsyncPaneProcessSupervisorServiceConfig {
                max_polls: if predicate_stop { u64::MAX } else { 1 },
                ..Default::default()
            },
            |polls, _| predicate_stop && polls >= 1,
        )
        .await
        .unwrap();
        assert_eq!(report.completed_workers, 1);
        assert_native_retired(witness);
        assert_eq!(native.outstanding(), 0);
        handle.shutdown().await.unwrap();
        actor.await.unwrap();
    }
}

/// Cancelling before first poll and while the real worker's native termination
/// is gated must return its exact child to the preallocated session owner.
/// Only the production bounded native drain, not task join or fixture cleanup,
/// is used to prove immediate retirement before the completed operation returns.
#[tokio::test]
async fn cancelled_native_worker_retains_exact_cleanup_before_and_during_wait() {
    for during_wait in [false, true] {
        let mut service = test_service();
        let start = service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        let witness = native_retirement_witness(start.primary_pid);
        let native = service.native_pane_retirement_owner();
        let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
            .build()
            .unwrap();
        let actor = tokio::spawn(actor.run());
        let (instance, process) = handle
            .take_running_pane_process_instances_for_adapter(1)
            .await
            .unwrap()
            .pop()
            .unwrap();
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (_release, release_rx) = tokio::sync::oneshot::channel();
        let backend = GatedNativeRetirement {
            backend: AsyncPtyPaneProcessIo::new(start.pane_id, process).unwrap(),
            entered: Some(entered),
            release: Some(release_rx),
        };
        let driver = AsyncPaneProcessDriver::new_for_instance(
            instance.clone(),
            backend,
            AsyncPaneProcessDriverConfig::default(),
        )
        .unwrap();
        let worker = tokio::spawn(run_owned_pane_process_worker(
            handle.clone(),
            instance,
            start.primary_pid,
            driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 1,
                ..Default::default()
            },
        ));
        if during_wait {
            entered_rx.await.unwrap();
        }
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        assert_eq!(native.outstanding(), 1);
        assert_eq!(native.returned_count(), 1);
        native.seal();
        crate::host::async_runtime::drain_native_pane_retirement(
            &native,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_native_retired(witness);
        handle.shutdown().await.unwrap();
        actor.await.unwrap();
    }
}

/// Graceful intent must survive cancellation of the worker that held the
/// original native child. This real HUP trap writes a fixture-owned marker;
/// force-only Drop behavior would lose it. Reuse the original deadline and
/// require exact native absence before the production drain returns.
#[tokio::test]
async fn graceful_native_cleanup_survives_worker_cancellation_without_kill_race() {
    let root =
        std::path::PathBuf::from("/tmp").join(format!("mez-native-grace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let ready = root.join("ready");
    let marker = root.join("graceful");
    let command = format!(
        "/bin/sh -c 'trap \"printf graceful > {}; exit 0\" HUP; printf ready > {}; while :; do sleep 0.02; done'",
        marker.display(),
        ready.display()
    );
    let mut service = test_service();
    let start = service.start_initial_pane_process(Some(&command)).unwrap();
    let witness = native_retirement_witness(start.primary_pid);
    let native = service.native_pane_retirement_owner();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let actor = tokio::spawn(actor.run());
    let (instance, process) = handle
        .take_running_pane_process_instances_for_adapter(1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (_release, release_rx) = tokio::sync::oneshot::channel();
    let backend = GatedNativeRetirement {
        backend: AsyncPtyPaneProcessIo::new(start.pane_id, process).unwrap(),
        entered: Some(entered),
        release: Some(release_rx),
    };
    let driver = AsyncPaneProcessDriver::new_for_instance(
        instance.clone(),
        backend,
        AsyncPaneProcessDriverConfig::default(),
    )
    .unwrap();
    let worker = tokio::spawn(run_owned_pane_process_worker(
        handle.clone(),
        instance,
        start.primary_pid,
        driver,
        AsyncPaneProcessServiceConfig {
            max_polls: 1,
            ..Default::default()
        },
    ));
    entered_rx.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    native.request_shutdown(false);
    let original_deadline = native.cleanup_deadline();
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    native.seal();
    crate::host::async_runtime::drain_native_pane_retirement(
        &native,
        tokio::time::Instant::from_std(original_deadline),
    )
    .await
    .unwrap();
    assert_native_retired(witness);
    assert_eq!(native.cleanup_deadline(), original_deadline);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "graceful");
    handle.shutdown().await.unwrap();
    actor.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Dropping the native handoff reply receiver must not dispose the sole child
/// waiter. Observe the returned slot through the real actor request handler,
/// then prove strict retirement with the production bounded native operation.
#[tokio::test]
async fn lost_native_handoff_reply_retains_exact_child_for_cleanup() {
    let mut service = test_service();
    let start = service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let witness = native_retirement_witness(start.primary_pid);
    let native = service.native_pane_retirement_owner();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let actor = tokio::spawn(actor.run());
    let (reply, receiver) = tokio::sync::oneshot::channel();
    drop(receiver);
    handle.sender.send(crate::host::async_runtime::AsyncRuntimeRequestEnvelope::new(
        crate::host::async_runtime::actor_types::AsyncRuntimeRequest::TakeRunningPaneProcessesForAdapter { limit: 1, reply }
    )).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while native.returned_count() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(native.outstanding(), 1);
    native.seal();
    crate::host::async_runtime::drain_native_pane_retirement(
        &native,
        tokio::time::Instant::now() + Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_native_retired(witness);
    handle.shutdown().await.unwrap();
    actor.await.unwrap();
}

/// Verifies that the dynamic pane-process supervisor can claim a live
/// manager-owned pane through the actor and start a per-pane worker without a
/// startup-only handoff list. This is the daemon path needed for panes created
/// after the initial session boot.
#[tokio::test]
async fn async_pane_process_supervisor_claims_live_manager_panes() {
    let mut service = test_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let supervisor_handle = handle.clone();
    let supervisor = async move {
        let report = run_async_pane_process_supervisor_service(
            supervisor_handle,
            AsyncPaneProcessSupervisorServiceConfig {
                max_polls: 2,
                take_limit: 8,
                idle_interval: Duration::from_millis(1),
                pane_service: AsyncPaneProcessServiceConfig {
                    max_polls: u64::MAX,
                    output_drain_limit: 1,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                    foreground_metadata_interval: Duration::from_secs(60),
                },
            },
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(report.spawned_workers, 1);
        assert_eq!(
            handle
                .take_running_pane_processes_for_adapter(8)
                .await
                .unwrap()
                .len(),
            0
        );
        let _ = handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(supervisor, actor.run());

    assert_eq!(report.polls, 2);
    assert_eq!(report.spawned_workers, 1);
    assert!(exit.service.terminate_all_pane_processes().is_ok());
}

/// Verifies that the dynamic pane-process supervisor observes child worker
/// completion directly instead of waking on its fallback idle interval. This
/// keeps production supervision responsive to short-lived panes without adding
/// an idle poll while no new handoffs are available.
#[tokio::test]
async fn async_pane_process_supervisor_wakes_on_worker_completion() {
    let mut service = test_service();
    service.start_initial_pane_process(Some("true")).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let supervisor_handle = handle.clone();
    let supervisor = async move {
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            run_async_pane_process_supervisor_service(
                supervisor_handle,
                AsyncPaneProcessSupervisorServiceConfig {
                    max_polls: u64::MAX,
                    take_limit: 8,
                    idle_interval: Duration::from_secs(60),
                    pane_service: AsyncPaneProcessServiceConfig {
                        max_polls: u64::MAX,
                        output_drain_limit: 1,
                        drain_limit: 8,
                        idle_interval: Duration::from_secs(60),
                        foreground_metadata_interval: Duration::from_secs(60),
                    },
                },
                |polls, _| polls >= 3,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(supervisor, actor.run());

    assert_eq!(report.spawned_workers, 1);
    assert_eq!(report.completed_workers, 1);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies a pane-local PTY read failure retires only that process generation
/// while the actor and a sibling pane remain usable.
#[tokio::test]
async fn failed_pane_worker_does_not_stop_sibling_pane_or_runtime() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .split_pane_with_process(
            &primary,
            mez_mux::layout::SplitDirection::Vertical,
            Some("cat >/dev/null"),
        )
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let actor_task = tokio::spawn(actor.run());

    let mut processes = handle
        .take_running_pane_process_instances_for_adapter(8)
        .await
        .unwrap();
    assert_eq!(processes.len(), 2);
    let failed_index = processes
        .iter()
        .position(|(instance, _)| instance.pane_id == "%1")
        .unwrap();
    let (failed_instance, mut failed_process) = processes.swap_remove(failed_index);
    let (sibling_instance, mut sibling_process) = processes.pop().unwrap();

    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output_error("injected pane PTY read failure");
    let driver = AsyncPaneProcessDriver::new_for_instance(
        failed_instance.clone(),
        backend,
        AsyncPaneProcessDriverConfig::default(),
    )
    .unwrap();
    let (retired_instance, outcome) = run_owned_pane_process_worker(
        handle.clone(),
        failed_instance.clone(),
        failed_process.primary_pid(),
        driver,
        AsyncPaneProcessServiceConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(retired_instance, failed_instance);
    assert!(matches!(outcome, AsyncPaneProcessWorkerOutcome::Failed));

    let mut late_failed_event = RuntimeEventBatch::new();
    late_failed_event.push(RuntimeEvent::PaneProcess {
        instance: failed_instance,
        event: PaneProcessEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"late failed-pane output".to_vec(),
        }),
    });
    assert_eq!(
        handle
            .submit_runtime_events(late_failed_event)
            .await
            .unwrap()
            .applied,
        0
    );

    let mut sibling_output = RuntimeEventBatch::new();
    sibling_output.push(RuntimeEvent::PaneProcess {
        instance: sibling_instance,
        event: PaneProcessEvent::Pane(PaneEvent::Output {
            pane_id: "%2".to_string(),
            bytes: b"sibling remains live".to_vec(),
        }),
    });
    assert_eq!(
        handle
            .submit_runtime_events(sibling_output)
            .await
            .unwrap()
            .applied,
        1
    );

    let _ = failed_process.terminate(Duration::from_millis(10));
    let _ = sibling_process.terminate(Duration::from_millis(10));
    handle.shutdown().await.unwrap();
    let mut actor_exit = actor_task.await.unwrap();
    actor_exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies an actor event-ingress failure remains fatal instead of being
/// mistaken for a pane-backend failure and followed by a retirement attempt.
#[tokio::test]
async fn pane_worker_actor_ingress_failure_remains_fatal() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    drop(actor);

    let instance = PaneProcessInstance {
        pane_id: "%1".to_string(),
        generation: 1,
    };
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"actor ingress must remain fatal");
    let driver = AsyncPaneProcessDriver::new_for_instance(
        instance.clone(),
        backend,
        AsyncPaneProcessDriverConfig::default(),
    )
    .unwrap();

    let error = run_owned_pane_process_worker(
        handle,
        instance,
        1,
        driver,
        AsyncPaneProcessServiceConfig::default(),
    )
    .await
    .unwrap_err();
    assert!(
        error.message().contains("actor is closed"),
        "actor ingress error should propagate without pane retirement: {error}"
    );
}
