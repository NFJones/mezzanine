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
    after_first_native_stage: bool,
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
            let native = self.backend.terminate(force);
            tokio::pin!(native);
            if self.after_first_native_stage {
                let first = futures_util::future::poll_fn(|cx| {
                    std::task::Poll::Ready(std::future::Future::poll(native.as_mut(), cx))
                })
                .await;
                if let std::task::Poll::Ready(result) = first {
                    return result;
                }
            }
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            if let Some(release) = self.release.take() {
                release
                    .await
                    .map_err(|_| MezError::invalid_state("native fixture gate closed"))?;
            }
            native.await
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
        after_first_native_stage: false,
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
            after_first_native_stage: false,
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
    let fixture_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::path::PathBuf::from("/tmp").join(format!(
        "mez-native-grace-{}-{fixture_id}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    let ready = root.join("ready");
    let marker = root.join("graceful");
    let hup = root.join("hup");
    let handler_release = root.join("release");
    let command = format!(
        "/bin/sh -c 'trap \"printf H >> {}; while [ ! -e {} ]; do sleep 0.01; done; printf graceful > {}; exit 0\" HUP; printf ready > {}; while :; do sleep 0.02; done'",
        hup.display(),
        handler_release.display(),
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
    // This exact duplicated master stays alive through native reap, so backend
    // cancellation cannot generate the HUP that the marker test observes.
    let held_master = process.duplicate_master_fd().unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    native.request_shutdown(false);
    let original_deadline = native.cleanup_deadline();
    let backend = GatedNativeRetirement {
        backend: AsyncPtyPaneProcessIo::new(start.pane_id, process).unwrap(),
        entered: Some(entered),
        release: Some(release_rx),
        after_first_native_stage: true,
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
        while !hup.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&hup).unwrap(), "H");
    assert!(!marker.exists(), "native HUP handler must still be gated");
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    native.seal();
    let (early, completed) = {
        let drain = crate::host::async_runtime::drain_native_pane_retirement(
            &native,
            tokio::time::Instant::from_std(original_deadline),
        );
        tokio::pin!(drain);
        let early = tokio::time::timeout(Duration::from_millis(30), &mut drain).await;
        std::fs::write(&handler_release, b"release").unwrap();
        let completed = if early.is_err() {
            Some(drain.await)
        } else {
            None
        };
        (early, completed)
    };
    // On regression failure, settle only this exact fixture before asserting
    // the saved original verdict; a later retry cannot turn it into success.
    if native.outstanding() != 0 {
        crate::host::async_runtime::drain_native_pane_retirement(
            &native,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap();
    }
    assert!(
        early.is_err(),
        "cleanup returned before the issued HUP handler completed: {early:?}"
    );
    assert!(matches!(completed, Some(Ok(()))), "{completed:?}");
    assert_native_retired(witness);
    assert_eq!(native.cleanup_deadline(), original_deadline);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "graceful");
    assert_eq!(std::fs::read_to_string(&hup).unwrap(), "H");
    drop(held_master);
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

/// An expired retained backend plan may observe status, but must not send a
/// late signal. A fresh input/response from the original unreaped root proves
/// it survived that failed operation. A fixture-only input marker then lets the
/// original root exit naturally; exact backend polling/wait proves reaping
/// without renewing its deadline or issuing forbidden late cleanup signals.
#[tokio::test]
async fn expired_native_backend_plan_does_not_signal_original_live_root() {
    let mut service = test_service();
    let start = service.start_initial_pane_process(Some(
        r#"/bin/sh -c 'stty -echo; while IFS= read -r line; do if [ "$line" = native-fixture-finish ]; then exit 0; fi; printf "native-read:%s\n" "$line"; done'"#)).unwrap();
    let witness = native_retirement_witness(start.primary_pid);
    let native = service.native_pane_retirement_owner();
    let mut process = service
        .take_running_pane_process_for_adapter(&start.pane_id)
        .unwrap();
    let deadline = process.begin_native_termination(true).unwrap();
    let mut backend = AsyncPtyPaneProcessIo::new(start.pane_id, process).unwrap();
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline) + Duration::from_millis(10))
        .await;
    let expired = backend.terminate(false).await.unwrap_err();
    assert!(expired.message().contains("deadline expired"));
    backend.write_input(b"survived-deadline\n").await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(chunk) = backend.read_output(4096).await.unwrap() {
                bytes.extend(chunk);
            }
            if String::from_utf8_lossy(&bytes).contains("native-read:survived-deadline") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(rustix::process::test_kill_process(witness.0), Ok(()));
    backend
        .write_input(b"native-fixture-finish\n")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while backend.poll_exit().await.unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_native_retired(witness);
    assert_eq!(native.outstanding(), 0);
    drop(backend);
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

/// Forwards one fake backend while holding its first output read until the
/// controller releases it, so the real worker has already passed its Running
/// admission check when the actor's lifecycle changes.
struct GatedFirstOutput {
    backend: AsyncFakePaneProcessIo,
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    release: Option<tokio::sync::oneshot::Receiver<()>>,
    terminations: Option<StdArc<std::sync::Mutex<Vec<bool>>>>,
}

impl AsyncPaneProcessIo for GatedFirstOutput {
    /// Retains the same in-progress read across the lifecycle change.
    fn read_output<'a>(
        &'a mut self,
        max_bytes: usize,
    ) -> crate::host::async_runtime::pane_io::AsyncPaneIoFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            if let Some(release) = self.release.take() {
                release
                    .await
                    .map_err(|_| MezError::invalid_state("output gate closed"))?;
            }
            self.backend.read_output(max_bytes).await
        })
    }

    /// Forwards writes to the existing fake backend.
    fn write_input<'a>(
        &'a mut self,
        bytes: &'a [u8],
    ) -> crate::host::async_runtime::pane_io::AsyncPaneIoFuture<'a, usize> {
        self.backend.write_input(bytes)
    }

    /// Forwards resizes to the existing fake backend.
    fn resize<'a>(
        &'a mut self,
        size: Size,
    ) -> crate::host::async_runtime::pane_io::AsyncPaneIoFuture<'a, ()> {
        self.backend.resize(size)
    }

    /// Records termination requests and forwards them to the fake backend.
    fn terminate<'a>(
        &'a mut self,
        force: bool,
    ) -> crate::host::async_runtime::pane_io::AsyncPaneIoFuture<'a, ProcessEvent> {
        if let Some(terminations) = self.terminations.as_ref() {
            terminations.lock().unwrap().push(force);
        }
        self.backend.terminate(force)
    }
}

/// Output that an already-claimed pane worker produced while the actor was
/// still `Running` must be settled after a nonforced Shutdown instead of
/// becoming a stopping error that escapes the worker as its own failure.
///
/// The worker is the real consuming implementation and the gate sits only in
/// the backend read, so the actor still makes the lifecycle decision. The exact
/// pane generation, applied accounting, stale-generation inertness, and refusal
/// of new work while stopping are all checked separately.
#[tokio::test]
async fn owned_pane_worker_settles_output_after_stopping_admission() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let actor_task = tokio::spawn(actor.run());
    let (instance, mut process) = handle
        .take_running_pane_process_instances_for_adapter(1)
        .await
        .unwrap()
        .pop()
        .unwrap();

    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"owned-before-shutdown\n".to_vec());
    let driver = AsyncPaneProcessDriver::new_for_instance(
        instance.clone(),
        GatedFirstOutput {
            backend,
            entered: Some(entered),
            release: Some(release_rx),
            terminations: None,
        },
        AsyncPaneProcessDriverConfig::default(),
    )
    .unwrap();

    let worker_handle = handle.clone();
    let worker = tokio::spawn(run_owned_pane_process_worker(
        handle.clone(),
        instance.clone(),
        process.primary_pid(),
        driver,
        AsyncPaneProcessServiceConfig {
            max_polls: u64::MAX,
            output_drain_limit: 1,
            drain_limit: 8,
            idle_interval: Duration::from_millis(1),
            foreground_metadata_interval: Duration::from_secs(60),
        },
    ));

    // The worker already passed its Running admission check and owns the
    // in-progress read, so Shutdown is admitted strictly after that check.
    entered_rx.await.unwrap();
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
        reason: "gated owned pane settlement crossing".to_string(),
        force: false,
        failed: false,
    }));
    let admission = handle.submit_runtime_events(batch).await.unwrap();
    assert_eq!(admission.accepted, 1);
    assert_eq!(admission.applied, 1);
    assert_eq!(
        handle.lifecycle_state().await.unwrap(),
        RuntimeLifecycleState::Stopping
    );
    assert!(
        !worker_handle
            .sender
            .closed
            .load(std::sync::atomic::Ordering::Acquire),
        "phase-1 stopping must keep actor ingress open"
    );

    release.send(()).unwrap();
    let (retired_instance, outcome) = worker
        .await
        .unwrap()
        .expect("owned pane settlement must not fail after a stopping admission");
    assert_eq!(retired_instance, instance);
    let AsyncPaneProcessWorkerOutcome::Completed(report) = outcome else {
        panic!("stopping settlement must not be reported as a pane backend failure: {outcome:?}");
    };
    assert_eq!(report.submitted_events, 1);
    assert_eq!(
        report.applied_events, 1,
        "the already-read output must be settled, not discarded"
    );
    assert_eq!(report.terminal_state, RuntimeLifecycleState::Stopping);

    // The actor's own settlement disposition is observed separately, and a
    // stale generation stays inert below the same lifecycle.
    let mut observer = RuntimeEventBatch::new();
    observer.push(RuntimeEvent::PaneProcess {
        instance: instance.clone(),
        event: PaneProcessEvent::Pane(PaneEvent::Output {
            pane_id: instance.pane_id.clone(),
            bytes: b"observer-settlement".to_vec(),
        }),
    });
    let observed = handle.submit_runtime_events(observer).await.unwrap();
    assert_eq!((observed.accepted, observed.applied), (1, 1));

    let stale = PaneProcessInstance {
        pane_id: instance.pane_id.clone(),
        generation: instance.generation.saturating_add(1),
    };
    let mut stale_batch = RuntimeEventBatch::new();
    stale_batch.push(RuntimeEvent::PaneProcess {
        instance: stale,
        event: PaneProcessEvent::Pane(PaneEvent::Output {
            pane_id: instance.pane_id.clone(),
            bytes: b"stale generation".to_vec(),
        }),
    });
    let stale_report = handle.submit_runtime_events(stale_batch).await.unwrap();
    assert_eq!(
        (stale_report.accepted, stale_report.applied),
        (1, 0),
        "a stale generation must stay inert"
    );
    assert_eq!(
        handle.lifecycle_state().await.unwrap(),
        RuntimeLifecycleState::Stopping
    );

    // An unscoped settlement for a pane that does not exist is inert. Unscoped
    // producers carry no generation, so the reducer's own pane lookup decides;
    // the settlement boundary is lifecycle plus pane identity, never a global
    // acceptance of anonymous events.
    let mut missing = RuntimeEventBatch::new();
    missing.push(RuntimeEvent::Pane(PaneEvent::Output {
        pane_id: "%missing-pane".to_string(),
        bytes: b"missing pane".to_vec(),
    }));
    let missing_report = handle.submit_runtime_events(missing).await.unwrap();
    assert_eq!((missing_report.accepted, missing_report.applied), (1, 0));

    // New work is still refused while stopping; only settlement is admitted.
    let refused = handle
        .write_input_to_pane(primary, "%1", b"must not be admitted".to_vec())
        .await
        .unwrap_err();
    assert!(
        refused.message().contains("runtime service is stopping"),
        "new pane input must keep failing while stopping: {refused}"
    );

    let _ = process.terminate(Duration::from_millis(50));
    handle.shutdown().await.unwrap();
    let mut actor_exit = actor_task.await.unwrap();
    actor_exit.service.terminate_all_pane_processes().unwrap();
}

/// Acknowledged writes and failure settlements that cross a stopping admission
/// keep their event-specific settlement. The acknowledgement is progress for
/// bytes the PTY already accepted, and write or process failures still reach
/// their diagnostic and transaction-settlement reducers rather than being
/// converted into a stopping error or silently dropped.
#[tokio::test]
async fn stopping_admission_settles_acknowledged_write_and_failure_diagnostics() {
    let mut service = test_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let actor_task = tokio::spawn(actor.run());
    let (instance, mut process) = handle
        .take_running_pane_process_instances_for_adapter(1)
        .await
        .unwrap()
        .pop()
        .unwrap();

    let mut shutdown = RuntimeEventBatch::new();
    shutdown.push(RuntimeEvent::Shutdown(ShutdownEvent {
        reason: "settlement accounting crossing".to_string(),
        force: false,
        failed: false,
    }));
    assert_eq!(
        handle
            .submit_runtime_events(shutdown)
            .await
            .unwrap()
            .applied,
        1
    );
    assert_eq!(
        handle.lifecycle_state().await.unwrap(),
        RuntimeLifecycleState::Stopping
    );

    // An acknowledgement that crosses the stopping admission must not become a
    // stopping error for the worker. Without an owning shell transaction it is
    // the existing no-op disposition; the transaction-backed settlement is
    // proven by the runtime-level stopping admission test.
    for _ in 0..2 {
        let mut acknowledgement = RuntimeEventBatch::new();
        acknowledgement.push(RuntimeEvent::PaneProcess {
            instance: instance.clone(),
            event: PaneProcessEvent::Pane(PaneEvent::InputWritten {
                pane_id: instance.pane_id.clone(),
                bytes: 12,
            }),
        });
        let report = handle.submit_runtime_events(acknowledgement).await.unwrap();
        assert_eq!((report.accepted, report.applied), (1, 0));
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Stopping
        );
    }

    // Failure settlements reach their own reducers: the write failure records a
    // diagnostic and fails affected transactions, and the process failure
    // records its diagnostic without closing the pane.
    for (label, event) in [
        (
            "write-failed",
            PaneProcessEvent::Pane(PaneEvent::WriteFailed {
                pane_id: instance.pane_id.clone(),
                error: "injected write failure".to_string(),
            }),
        ),
        (
            "process-failed",
            PaneProcessEvent::Process(ProcessEvent::Failed {
                pane_id: instance.pane_id.clone(),
                error: "injected process failure".to_string(),
            }),
        ),
    ] {
        let mut failure = RuntimeEventBatch::new();
        failure.push(RuntimeEvent::PaneProcess {
            instance: instance.clone(),
            event,
        });
        let report = handle.submit_runtime_events(failure).await.unwrap();
        assert_eq!(
            (report.accepted, report.applied),
            (1, 1),
            "{label} must settle through its reducer instead of being dropped"
        );
    }

    let _ = process.terminate(Duration::from_millis(50));
    handle.shutdown().await.unwrap();
    let mut actor_exit = actor_task.await.unwrap();
    actor_exit.service.terminate_all_pane_processes().unwrap();
}

/// A worker that starts after the lifecycle is already terminal must retire its
/// exact backend and return its own report instead of an error, requesting only
/// graceful termination.
#[tokio::test]
async fn owned_pane_worker_terminal_first_poll_retires_without_failure() {
    let mut service = test_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let actor_task = tokio::spawn(actor.run());
    let (instance, mut process) = handle
        .take_running_pane_process_instances_for_adapter(1)
        .await
        .unwrap()
        .pop()
        .unwrap();

    let mut shutdown = RuntimeEventBatch::new();
    shutdown.push(RuntimeEvent::Shutdown(ShutdownEvent {
        reason: "terminal before first poll".to_string(),
        force: false,
        failed: false,
    }));
    assert_eq!(
        handle
            .submit_runtime_events(shutdown)
            .await
            .unwrap()
            .applied,
        1
    );

    let terminations = StdArc::new(std::sync::Mutex::new(Vec::new()));
    let driver = AsyncPaneProcessDriver::new_for_instance(
        instance.clone(),
        GatedFirstOutput {
            backend: AsyncFakePaneProcessIo::default(),
            entered: None,
            release: None,
            terminations: Some(terminations.clone()),
        },
        AsyncPaneProcessDriverConfig::default(),
    )
    .unwrap();
    let (retired_instance, outcome) = run_owned_pane_process_worker(
        handle.clone(),
        instance.clone(),
        process.primary_pid(),
        driver,
        AsyncPaneProcessServiceConfig::default(),
    )
    .await
    .expect("terminal-first retirement must not fail");
    assert_eq!(retired_instance, instance);
    let AsyncPaneProcessWorkerOutcome::Completed(report) = outcome else {
        panic!("terminal-first retirement must not report a backend failure: {outcome:?}");
    };
    assert_eq!(report.submitted_events, 0);
    assert_eq!(report.applied_events, 0);
    assert_eq!(report.terminal_state, RuntimeLifecycleState::Stopping);
    // The service terminal branch settles termination and the worker re-settles
    // the same backend; neither request may escalate to force.
    let terminations = terminations.lock().unwrap().clone();
    assert!(
        !terminations.is_empty() && terminations.iter().all(|force| !force),
        "terminal retirement must only request graceful termination: {terminations:?}"
    );

    let _ = process.terminate(Duration::from_millis(50));
    handle.shutdown().await.unwrap();
    let mut actor_exit = actor_task.await.unwrap();
    actor_exit.service.terminate_all_pane_processes().unwrap();
}

/// A process exit that arrives while the lifecycle is stopping still settles
/// through the real exit reducer for its exact pane generation instead of
/// becoming a stopping error.
#[tokio::test]
async fn stopping_admission_settles_process_exit_for_current_generation() {
    let mut service = test_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let actor_task = tokio::spawn(actor.run());
    let (instance, mut process) = handle
        .take_running_pane_process_instances_for_adapter(1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let primary_pid = process.primary_pid();

    let mut shutdown = RuntimeEventBatch::new();
    shutdown.push(RuntimeEvent::Shutdown(ShutdownEvent {
        reason: "exit settlement crossing".to_string(),
        force: false,
        failed: false,
    }));
    assert_eq!(
        handle
            .submit_runtime_events(shutdown)
            .await
            .unwrap()
            .applied,
        1
    );

    let mut exit = RuntimeEventBatch::new();
    exit.push(RuntimeEvent::PaneProcess {
        instance: instance.clone(),
        event: PaneProcessEvent::Process(ProcessEvent::Exited {
            pane_id: instance.pane_id.clone(),
            primary_pid: Some(primary_pid),
            exit_code: Some(0),
            signal: None,
        }),
    });
    let report = handle.submit_runtime_events(exit).await.unwrap();
    assert_eq!(
        (report.accepted, report.applied),
        (1, 1),
        "a current-generation exit must settle while stopping"
    );

    let _ = process.terminate(Duration::from_millis(50));
    handle.shutdown().await.unwrap();
    let mut actor_exit = actor_task.await.unwrap();
    let _ = actor_exit.service.terminate_all_pane_processes();
}
