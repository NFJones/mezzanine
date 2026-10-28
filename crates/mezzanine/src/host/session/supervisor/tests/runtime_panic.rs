//! Whole-runtime panic publication through genuine actor and native ownership.
//!
//! An isolated copy of the test harness owns two independent runtimes. A typed
//! test-only request panics inside one serialized actor; no compromised actor
//! is resumed. An exact retained native child and held PTY master distinguish
//! task loss from process retirement. Only real cleanup permits terminal
//! publication. Historical aborts and unknown native fate are not inferred from
//! this controlled unwind case, and panic payloads never become status text.

use super::*;

/// Runs the abort-capable case in a separately waitable owned process. The
/// ordinary test stack and runtime configuration are unchanged. A timeout kills
/// and reaps only this exact subprocess and does not claim native child fate.
#[test]
fn runtime_actor_panic_preserves_cause_cleanup_and_healthy_sibling() {
    const CHILD_ENV: &str = "MEZ_RUNTIME_PANIC_QUALIFICATION_CHILD";
    if std::env::var_os(CHILD_ENV).as_deref() == Some(std::ffi::OsStr::new("actor-unwind")) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(Box::pin(actor_panic_case()));
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "host::session::supervisor::tests::runtime_panic::runtime_actor_panic_preserves_cause_cleanup_and_healthy_sibling",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "actor-unwind")
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait().unwrap();
            panic!("owned panic qualification timed out ({status}); native fate is unproven");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "owned panic qualification failed: {status}"
    );
}

/// Qualifies the actual supervisor task, actor request, retained native drain,
/// and sibling actor. Observations are saved before native cleanup, but causal
/// assertions run only after both exact owners have settled. No fabricated
/// generation, terminal receipt, native wait, or successful actor reply is used.
async fn actor_panic_case() {
    let publications = Arc::new(Mutex::new(Vec::new()));
    let observed = publications.clone();
    let supervisor = SessionSupervisor::new_with_runtime_completion_handler(
        4,
        Some(RuntimeCompletionHandler(Arc::new(move |snapshot| {
            observed.lock().unwrap().push(snapshot.clone());
            Ok(())
        }))),
    );
    let mut failed_request = native_factory_request("actor-panic");
    let mut healthy_request = native_factory_request("actor-panic-sibling");
    // Keep genuine control listeners alive as in a persistent host session.
    // Use the established factory command rather than adding launch semantics.
    for request in [&mut failed_request, &mut healthy_request] {
        request.sockets.publish_control = true;
    }
    let roots = [
        failed_request.config.root.clone(),
        healthy_request.config.root.clone(),
    ];
    let failed = Box::pin(supervisor.start(failed_request)).await.unwrap();
    let healthy = Box::pin(supervisor.start(healthy_request)).await.unwrap();
    let sibling_initial = healthy.lifecycle_state().await;
    let failed_id = failed.session_id().to_string();
    let generation = supervisor.inner.entries().unwrap()[&failed_id].generation;
    let native = failed.native_retirement_owner();
    let process = mez_mux::process::spawn_retained_argv_pty_process(
        std::path::Path::new("/bin/cat"),
        &[],
        &[],
        Size::new(80, 24).unwrap(),
        None,
        native.reserve().unwrap(),
    )
    .unwrap();
    let pid =
        rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
    let master = process.duplicate_master_fd().unwrap();
    // Hold exact native custody across actor unwind. Task completion alone must
    // leave this generation pending rather than publish a completed retirement.
    let barrier = native.begin_drain().unwrap();
    drop(process);
    let reply = tokio::time::timeout(Duration::from_secs(5), failed.actor().panic_for_tests())
        .await
        .unwrap();
    let pending = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let failure = {
                let entries = supervisor.inner.entries().unwrap();
                entries
                    .get(&failed_id)
                    .and_then(|entry| match &entry.state {
                        SupervisorEntryState::CleanupPending { failure } => Some(failure.clone()),
                        _ => None,
                    })
            };
            if let Some(failure) = failure {
                break failure;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let native_before_cleanup = native.outstanding();
    let live_before_cleanup = rustix::process::test_kill_process(pid);
    let publications_before_cleanup = publications
        .lock()
        .unwrap()
        .iter()
        .filter(|snapshot| snapshot.session_id == failed_id)
        .count();
    let sibling_before_cleanup =
        tokio::time::timeout(Duration::from_secs(5), healthy.lifecycle_state()).await;
    drop(barrier);
    let cleanup = supervisor.stop(&failed_id, true).await;
    let sibling_after_cleanup =
        tokio::time::timeout(Duration::from_secs(5), healthy.lifecycle_state()).await;
    let shutdown = supervisor.shutdown_all(true, Duration::from_secs(5)).await;
    let native_after_cleanup = native.outstanding();
    let dead_after_cleanup = rustix::process::test_kill_process(pid);
    let snapshots = supervisor.snapshots().await.unwrap();
    let publications = publications.lock().unwrap().clone();
    drop(master);
    if native_after_cleanup == 0 && supervisor.native_pending_count().unwrap() == 0 {
        for root in roots {
            fs::remove_dir_all(root).unwrap();
        }
    }

    assert!(
        reply.is_err(),
        "panicked actor cannot return a successful reply"
    );
    assert!(native_before_cleanup > 0);
    assert_eq!(live_before_cleanup, Ok(()));
    assert_eq!(publications_before_cleanup, 0);
    assert_eq!(sibling_initial.unwrap(), RuntimeLifecycleState::Running);
    assert_eq!(
        sibling_before_cleanup.unwrap().unwrap(),
        RuntimeLifecycleState::Running
    );
    assert_eq!(
        sibling_after_cleanup.unwrap().unwrap(),
        RuntimeLifecycleState::Running
    );
    cleanup.unwrap();
    shutdown.unwrap();
    assert_eq!(native_after_cleanup, 0);
    assert_eq!(dead_after_cleanup, Err(rustix::io::Errno::SRCH));
    assert!(!supervisor.contains(&failed_id).unwrap());
    let pending = pending.unwrap();
    assert!(
        pending.contains("panicked"),
        "wrong initiating cause: {pending}"
    );
    assert!(!pending.contains("cancelled"), "{pending}");
    assert!(!pending.contains("PRIVATE_RUNTIME_PANIC_TEST_PAYLOAD"));
    let failed_publications = publications
        .iter()
        .filter(|snapshot| snapshot.session_id == failed_id)
        .collect::<Vec<_>>();
    assert_eq!(failed_publications.len(), 1);
    let snapshot = failed_publications[0];
    assert_eq!(snapshot.generation, generation);
    assert_eq!(snapshot.state, SessionSupervisorState::Failed);
    assert_eq!(snapshot.failure.as_deref(), Some(pending.as_str()));
    assert_eq!(
        snapshots
            .iter()
            .filter(|snapshot| snapshot.session_id == failed_id)
            .count(),
        1
    );
}
