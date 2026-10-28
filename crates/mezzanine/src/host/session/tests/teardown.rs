//! Direct service-plus-native-retirement failure through a real session runtime.
//!
//! One fixture-owned child and its existing native drain claim produce an
//! unproven wait barrier. The controller sends only the authorized exact-root
//! kill before the original deadline, withholds the wait until runtime failure,
//! then observes the real native wait without minting retry authority. This is
//! a controlled retained-wait fault, not an injected kernel signal/wait errno.

use super::*;

/// Observes fixture-owned returned children after their release input is set.
/// This permits only real native waits, with no signal, token or deadline mint.
async fn observe_released_fixture_children(
    native: &PaneProcessRetirementOwner,
) -> crate::Result<()> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut drain = native.begin_drain()?;
        while native.outstanding() > 0 {
            if let Some(mut child) = drain.take_returned() {
                child.try_wait()?;
                drop(child);
            }
            tokio::task::yield_now().await;
        }
        Ok(())
    })
    .await
    .map_err(|_| MezError::invalid_state("fixture native wait remains unproven"))?
}

/// Native retirement failure must not replace the initiating service error or
/// erase its typed observations. A real child remains in exact retained custody
/// until after SessionRuntime returns the combined error; a subsequent actual
/// wait settles the fixture while the original failed outcome remains failed.
#[tokio::test(flavor = "current_thread")]
async fn session_runtime_preserves_service_and_native_retirement_failures() {
    /// The owned script can exit through this fixture input even if a test
    /// assertion or signal operation fails. This sends no numeric signal and
    /// neither renews nor bypasses the native cleanup attempt's authority.
    struct ReleaseInput(PathBuf);
    impl Drop for ReleaseInput {
        /// Releases only the script launched by this fixture.
        fn drop(&mut self) {
            let _ = fs::write(&self.0, b"release");
        }
    }

    let root = test_root("service-native-dual-failure");
    let pid_path = root.join("owned.pid");
    let release_path = root.join("release-input");
    let release = ReleaseInput(release_path.clone());
    let mut request = test_request("service-native", root.clone(), root.join("control.sock"));
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "trap '' HUP TERM; printf '%s' \"$$\" > \"$1\"; while [ ! -f \"$2\" ]; do :; done"
            .to_string(),
        "mez-owned-retirement-fixture".to_string(),
        pid_path.to_string_lossy().into_owned(),
        release_path.to_string_lossy().into_owned(),
    ];
    request.startup = SessionRuntimeStartup::Initial {
        explicit_command: Some(mez_mux::process::shell_command_from_argv(&argv).unwrap()),
        start_directory: None,
        environment: None,
    };
    // The common factory fixture installs preserving custody before kernel spawn
    // and returns a real PID/incarnation witness independently of script output.
    let (mut runtime, native, witnesses) = Box::pin(async move {
        let (runtime, native, witnesses) = Box::pin(create_metadata_runtime(request)).await;
        (Box::new(runtime), native, witnesses)
    })
    .await;
    let Some(&(pid, _)) = witnesses.first() else {
        drop(release);
        drop(runtime);
        retire_metadata_fixture(native, witnesses).await;
        panic!("the initial pane must have an exact native witness");
    };
    let ready = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if fs::read_to_string(&pid_path).is_ok_and(|text| text == pid.to_string()) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if ready.is_err() {
        drop(release);
        drop(runtime);
        retire_metadata_fixture(native, witnesses).await;
        panic!("the owned fixture did not publish readiness after installing its traps");
    }
    let exact = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    let initially_live = rustix::process::test_kill_process(exact);
    // This is initial0 authority, captured once before cleanup, never a retry.
    let attempt = native.initial_shutdown_attempt(true).unwrap().unwrap();
    let mut drain = native
        .try_begin_drain_for_attempt(&attempt)
        .unwrap()
        .unwrap();
    runtime.services = Some(vec![AsyncRuntimeService::new(
        "causal-native-test",
        async {
            Err(MezError::forbidden("INITIATING_SERVICE_NATIVE_FAULT")
                .mark_pane_backend_failure()
                .with_provider_raw_text("fixture-only-origin"))
        },
    )]);
    let (returned, observed) = tokio::sync::oneshot::channel();
    // Keep large factory/runtime states on their owning heap futures. The
    // observer needs only unit success or the original error, not a service copy.
    let running = Box::pin(async move {
        let result = Box::pin((*runtime).run(Vec::new(), std::future::pending()))
            .await
            .map(|_| ());
        let _ = returned.send(());
        result
    });
    let controller = async {
        let child =
            tokio::time::timeout_at(tokio::time::Instant::from_std(attempt.deadline()), async {
                loop {
                    if let Some(child) = drain.take_returned() {
                        break child;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await;
        match child {
            Ok(mut child) => {
                // The retained std Child pins this exact root. Do not poll its
                // wait again until runtime has reported the original deadline.
                let killed = child.kill();
                let notification = observed.await;
                let pending_at_failure = native.outstanding();
                fs::write(&release_path, b"release").unwrap();
                let waited = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        match child.try_wait() {
                            Ok(Some(status)) => break Ok(status),
                            Ok(None) => tokio::task::yield_now().await,
                            Err(error) => break Err(error),
                        }
                    }
                })
                .await;
                drop(child);
                drop(drain);
                Some((killed, notification, pending_at_failure, waited))
            }
            Err(_) => {
                // Let the exact script exit before allowing the runtime's
                // remaining observation path to finish; no replacement signal.
                fs::write(&release_path, b"release").unwrap();
                drop(drain);
                None
            }
        }
    };
    let (outcome, controlled) = tokio::join!(running, controller);
    drop(release);
    let observation = observe_released_fixture_children(&native).await;
    let settled = native.outstanding();
    let kernel_receipt = rustix::process::test_kill_process(exact);
    // Verify actual resource settlement before diagnostic assertions. Leave
    // the release input available if actual wait observation remains unproven.
    observation.unwrap();
    let (killed, notification, pending, waited) =
        controlled.expect("returned exact child must reach its admitted drain");
    killed.unwrap();
    notification.unwrap();
    waited.unwrap().unwrap();
    assert_eq!(initially_live, Ok(()));
    assert_eq!(
        pending, 1,
        "runtime must retain the withheld exact native wait"
    );
    assert_eq!(
        settled, 0,
        "only the real native wait releases the fixture slot"
    );
    assert_eq!(kernel_receipt, Err(rustix::io::Errno::SRCH));
    let _ = fs::remove_dir_all(root);
    let error = outcome.unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(
        error.message().contains("INITIATING_SERVICE_NATIVE_FAULT"),
        "{error}"
    );
    assert!(
        error.message().contains("native cleanup") && error.message().contains("unproven"),
        "{error}"
    );
    assert!(
        error.is_pane_backend_failure(),
        "native failure erased the initiating typed evidence: {error}"
    );
    assert_eq!(error.provider_raw_text(), Some("fixture-only-origin"));
}
