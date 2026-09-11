//! Timer-driven pane certification settlement regressions.
//!
//! Pending agent-subshell certification is owned by a bounded runtime deadline.
//! These tests drive the production timer worker service, which schedules the
//! certification deadline from actor state and re-enters the actor as a typed
//! `TimerEvent`, so a certification that never receives its exact correlated
//! observation settles instead of leaving the pane pending forever.

use super::super::*;

/// Waits until the pane primary shell owns the foreground process group.
fn wait_for_primary_shell_foreground(service: &mut RuntimeSessionService) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if service.pane_foreground_certified_shell_state("%1") == Some(true) {
            return;
        }
        let _ = service.poll_pane_outputs(4096);
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("pane primary shell did not become foreground before the test deadline");
}

/// Builds the production pending-certification state for one pane.
///
/// The returned service has completed its bootstrap transaction and is waiting
/// for the exact correlated foreground observation, together with the
/// observation id that owns the bounded certification deadline.
fn pending_certification_service() -> (RuntimeSessionService, ClientId, String) {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_for_primary_shell_foreground(&mut service);
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    let subshell_group = primary_pid.saturating_add(1);
    service.enter_agent_subshell("%1");
    service.begin_agent_subshell_shell_handoff("%1").unwrap();
    service.dispatch_bootstrap_to_pane("%1").unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", Some(subshell_group));
    let markers = service
        .running_shell_transactions_for_tests()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        markers.len(),
        1,
        "the agent-subshell handoff should own exactly one bootstrap transaction"
    );
    let marker = markers.into_iter().next().unwrap();
    let turn_id = service
        .running_shell_transactions_for_tests()
        .get(&marker)
        .unwrap()
        .turn_id
        .clone();
    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\ttest-host\n\
env\tuser\ttest-user\n\
env\tshell_path\t/bin/sh\n\
env\tshell_class\tposix-sh\n\
env\tpath\t/usr/bin:/bin\n\
env\tcwd\t/tmp\n\
env\tgit_repo\t0\n\
bootstrap\tcomplete\t1714500000\n";
    {
        let transaction = service
            .running_shell_transactions_mut_for_tests()
            .get_mut(&marker)
            .unwrap();
        transaction.observed_output_preview = output.to_string();
        transaction.observed_output_bytes = output.len();
    }
    service
        .observe_agent_shell_transaction_start("%1", &marker, &turn_id, "agent-%1", "%1")
        .unwrap();
    let process = service.take_running_pane_process_for_adapter("%1").unwrap();
    service
        .apply_pane_foreground_process_event("%1", "setsid", subshell_group.saturating_add(1), None)
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, &turn_id, "agent-%1", "%1", 0)
        .unwrap();
    service.maybe_bootstrap_ready_panes().unwrap();
    let observation_id = service
        .drain_pane_io_transition()
        .side_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                effect:
                    crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess {
                        observation_id, ..
                    },
                ..
            } => Some(observation_id),
            _ => None,
        })
        .expect("bootstrap completion should request a correlated foreground observation");
    assert!(
        service.pane_agent_subshell_certification_is_pending("%1"),
        "the pane should be waiting for its correlated certification observation"
    );
    // Keep the adapter-owned process generation alive for the actor run.
    std::mem::forget(process);
    (service, primary, observation_id)
}

/// Verifies the production timer worker settles a pending pane certification at
/// its bounded deadline instead of leaving the pane pending forever.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pending_certification_settles_on_the_runtime_timer_deadline() {
    let (service, primary, observation_id) = pending_certification_service();
    let (declared_deadline, deadline_delay_ms) = service
        .running_shell_transaction_timers()
        .into_iter()
        .find(|timer| timer.marker == observation_id)
        .map(|timer| {
            let delay_ms = timer
                .started_at_unix_ms
                .saturating_add(timer.timeout_ms)
                .saturating_sub(crate::runtime::current_unix_millis());
            (timer.timeout_ms, delay_ms)
        })
        .expect("pending certification should own a bounded deadline");
    assert!(
        declared_deadline > 0 && declared_deadline <= 5_000,
        "the certification deadline must stay inside the bounded observation budget: {declared_deadline}"
    );
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async move {
        handle
            .resize_attached_primary_terminal(primary.clone(), Size::new(80, 24).unwrap())
            .await
            .unwrap();

        // Drive the real timer worker over the certification deadline. Its clock
        // uses the production Unix-millisecond epoch, so the fired event's
        // timestamp is comparable with the certification's start time. The
        // worker drains its own timer queue; the deadline was read from the
        // service-owned certification timer before the actor was built.
        let timer = run_async_runtime_timer_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 8,
                drain_limit: 16,
                idle_interval: Duration::from_millis(1),
            },
            crate::runtime::current_unix_millis(),
            |polls, _| polls >= 8,
        );
        let clock = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(deadline_delay_ms)).await;
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(1)).await;
        };
        let (report, ()) = tokio::join!(timer, clock);
        let report = report.unwrap();
        assert!(
            report.scheduled >= 1,
            "the actor must schedule the pending certification deadline: {report:?}"
        );
        assert!(
            report.fired >= 1,
            "the certification timer must fire: {report:?}"
        );

        let snapshot = handle.pane_certification_snapshot("%1").await.unwrap();
        assert_eq!(
            snapshot.certification_rejection,
            Some("foreground_observation_timed_out"),
            "a no-progress certification must settle with its bounded rejection: {snapshot:?}"
        );
        assert_eq!(
            snapshot.readiness,
            mez_agent::PaneReadinessState::Degraded,
            "{snapshot:?}"
        );
        assert!(!snapshot.bootstrap_pending, "{snapshot:?}");
        assert!(!snapshot.environment_signature_present, "{snapshot:?}");
        assert!(
            !handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .iter()
                .any(|task| task.pane_id == "%1")
        );
        handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
}
