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
