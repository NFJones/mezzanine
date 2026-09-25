//! Async-runtime tests owned by pane supervision behavior.

use super::super::*;

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
