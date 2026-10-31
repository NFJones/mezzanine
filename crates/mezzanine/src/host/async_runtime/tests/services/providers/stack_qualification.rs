//! Default-stack native/compaction handoffs and routed future layout witnesses.
//!
//! Owned subprocess profiles run actual native and compaction request paths on
//! normal multithread runtimes. Separate diagnostics infer future types without
//! constructing or polling them.
//! Future storage size is distinct from poll-frame depth and production crash
//! reachability; actual subprocess execution remains the stack-safety oracle.
//! No stack size, scheduler concurrency, native capability or policy is changed.

use super::*;

/// Retains the exact abort-capable harness through assertions and OS failures.
struct QualificationChild(std::process::Child);

impl Drop for QualificationChild {
    /// Reaps only this harness; failed exit does not establish native fate.
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Executes actual native action scheduling and plain/typed compaction workers
/// on production-flavor runtimes with default stacks. Each owned child must
/// exit successfully and write a fresh exact-profile receipt after original
/// liveness, durable publication and native cleanup assertions pass. Existing
/// large-stack responsiveness coverage remains a separate regression control.
#[test]
fn default_runtime_native_and_compaction_handoffs_complete() {
    const CHILD: &str = "MEZ_NATIVE_COMPACTION_PROFILE_CHILD";
    const RECEIPT: &str = "MEZ_NATIVE_COMPACTION_PROFILE_RECEIPT";
    if let Some(workers) = std::env::var_os(CHILD) {
        let workers = workers.into_string().unwrap().parse::<usize>().unwrap();
        assert!([1, 2, 4].contains(&workers));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::spawn(Box::pin(
                async_native_shell_worker_keeps_runtime_actor_responsive_body(),
            ))
            .await
            .unwrap();
            eprintln!("NATIVE_COMPACTION_PROFILE workers={workers} stage=native-settled");
            for typed in [false, true] {
                tokio::spawn(Box::pin(compaction::qualify(false, typed)))
                    .await
                    .unwrap();
                eprintln!(
                    "NATIVE_COMPACTION_PROFILE workers={workers} stage=compaction-settled typed={typed}"
                );
            }
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
                "schema": 1, "workers": workers, "cases": 3,
                "completed": true, "debug_assertions": cfg!(debug_assertions),
            }),
        )
        .unwrap();
        return;
    }
    let receipts = std::env::temp_dir().join(format!(
        "mez-native-compaction-profiles-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&receipts).unwrap();
    for workers in [1, 2, 4] {
        let receipt = receipts.join(format!("{workers}.json"));
        let mut child = QualificationChild(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "host::async_runtime::tests::services::providers::stack_qualification::default_runtime_native_and_compaction_handoffs_complete",
                    "--nocapture",
                ])
                .env_remove("RUST_MIN_STACK")
                .env_remove("RUST_TEST_THREADS")
                .env_remove("TOKIO_WORKER_THREADS")
                .env(CHILD, workers.to_string())
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
                "native/compaction workers={workers} watchdog expired; native fate unproven"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "native/compaction workers={workers} exit={status}"
        );
        let observed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
        assert_eq!(
            observed,
            serde_json::json!({
                "schema": 1, "workers": workers, "cases": 3,
                "completed": true, "debug_assertions": cfg!(debug_assertions),
            })
        );
        eprintln!("NATIVE_COMPACTION_PROFILE workers={workers} exit={status} receipt=verified");
    }
    std::fs::remove_dir_all(receipts).unwrap();
}

/// Infers a future layout without invoking its factory or constructing runtime
/// owners. This performs no provider/native work and cannot establish a bound
/// on the compiler's nested poll frames.
fn future_bytes<A, F: std::future::Future>(_: impl FnOnce(A) -> F) -> usize {
    std::mem::size_of::<F>()
}

/// Reports current debug/release future layouts separately from actual default
/// thread-stack execution. No arbitrary size threshold stands in for the real
/// routing, sandbox, result and healthy-owner acceptance assertions.
#[test]
#[ignore = "diagnostic layout witness; execute explicitly with stack qualification"]
fn routed_native_future_layout_witness() {
    let routed = future_bytes(|(): ()| {
        assert_routed_subagent_settles_after_in_place_selection(
            std::path::Path::new("/not-executed"),
            "not-executed",
            "not-executed",
        )
    });
    let boxed_wrapper = future_bytes(|(): ()| async {
        Box::pin(assert_routed_subagent_settles_after_in_place_selection(
            std::path::Path::new("/not-executed"),
            "not-executed",
            "not-executed",
        ))
        .await;
    });
    let actor =
        future_bytes(|actor: crate::host::async_runtime::AsyncRuntimeSessionActor| actor.run());
    let provider = future_bytes(
        |handle: &'static crate::host::async_runtime::AsyncRuntimeSessionHandle| {
            run_async_agent_provider_service(
                handle,
                AsyncAgentProviderServiceConfig::new(1).unwrap(),
                |_, _| false,
            )
        },
    );
    eprintln!(
        "STACK_LAYOUT routed_future_bytes={routed} boxed_wrapper_future_bytes={boxed_wrapper} actor_future_bytes={actor} provider_future_bytes={provider}"
    );
}
