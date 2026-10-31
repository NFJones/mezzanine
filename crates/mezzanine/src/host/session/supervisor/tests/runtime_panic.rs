//! Whole-runtime panic publication through genuine actor and native ownership.
//!
//! An isolated copy of the test harness owns two independent runtimes. A typed
//! test-only request panics inside one serialized actor; no compromised actor
//! is resumed. An exact retained native child and held PTY master distinguish
//! task loss from process retirement. Only real cleanup permits terminal
//! publication. Historical aborts and unknown native fate are not inferred from
//! this controlled unwind case, and panic payloads never become status text.

use super::*;

/// Retains the exact abort-capable harness until its exit is observed.
struct PanicProfileChild(std::process::Child);

impl Drop for PanicProfileChild {
    /// Kills/reaps this harness on errors without claiming native descendant fate.
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Runs the abort-capable case in a separately waitable owned process. The
/// original current-thread control and normal multi-thread worker profiles
/// 1/2/4 use default stacks. Each requires successful exit and a fresh completion
/// receipt after exact-generation publication, native cleanup and healthy-sibling
/// assertions. A timeout reaps only this harness and cannot prove native fate.
#[test]
fn runtime_actor_panic_preserves_cause_cleanup_and_healthy_sibling() {
    const CHILD_ENV: &str = "MEZ_RUNTIME_PANIC_QUALIFICATION_CHILD";
    const RECEIPT: &str = "MEZ_RUNTIME_PANIC_QUALIFICATION_RECEIPT";
    if let Some(workers) = std::env::var_os(CHILD_ENV) {
        let workers = workers.into_string().unwrap().parse::<usize>().unwrap();
        assert!([0, 1, 2, 4].contains(&workers));
        let mut builder = if workers == 0 {
            tokio::runtime::Builder::new_current_thread()
        } else {
            let mut builder = tokio::runtime::Builder::new_multi_thread();
            builder.worker_threads(workers);
            builder
        };
        let runtime = builder.enable_all().build().unwrap();
        runtime.block_on(async {
            tokio::spawn(Box::pin(actor_panic_case())).await.unwrap();
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
                "schema": 1, "workers": workers, "completed": true,
                "debug_assertions": cfg!(debug_assertions),
            }),
        )
        .unwrap();
        return;
    }
    let receipts = std::env::temp_dir().join(format!(
        "mez-runtime-panic-profiles-{:032x}",
        rand::random::<u128>()
    ));
    fs::create_dir(&receipts).unwrap();
    for workers in [0, 1, 2, 4] {
        let receipt = receipts.join(format!("{workers}.json"));
        let mut child = PanicProfileChild(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "host::session::supervisor::tests::runtime_panic::runtime_actor_panic_preserves_cause_cleanup_and_healthy_sibling",
                    "--nocapture",
                ])
                .env_remove("RUST_MIN_STACK")
                .env_remove("RUST_TEST_THREADS")
                .env_remove("TOKIO_WORKER_THREADS")
                .env(CHILD_ENV, workers.to_string())
                .env(RECEIPT, &receipt)
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "runtime panic workers={workers} watchdog expired; native fate unproven"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "runtime panic workers={workers} exit={status}"
        );
        let observed: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
        assert_eq!(
            observed,
            serde_json::json!({
                "schema": 1, "workers": workers, "completed": true,
                "debug_assertions": cfg!(debug_assertions),
            })
        );
        eprintln!("RUNTIME_PANIC_PROFILE workers={workers} exit={status} receipt=verified");
    }
    fs::remove_dir_all(receipts).unwrap();
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
