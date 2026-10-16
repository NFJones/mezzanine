//! Exact stop-admission races versus native/callback completion publication.
//!
//! Real runtimes and native waits supply lifecycle and kernel evidence. Private
//! gates pause only publication. Classified negative controls never substitute
//! a channel state, counter, or history record for actual retirement.

use super::*;

/// Captures the exact existing generation and handle, as sealed shutdown does.
fn target(supervisor: &SessionSupervisor, id: &str) -> ExactShutdownTarget {
    let entries = supervisor.inner.entries().unwrap();
    let entry = entries.get(id).unwrap();
    let request = match &entry.state {
        SupervisorEntryState::Running { handle, cancel }
        | SupervisorEntryState::Stopping { handle, cancel } => {
            Some((handle.clone(), cancel.clone()))
        }
        _ => None,
    };
    ExactShutdownTarget {
        generation: entry.generation,
        native: entry.native.clone(),
        completion: entry.completion.subscribe(),
        request,
    }
}

/// Installs an observation-only pause after actual runtime/native completion.
fn pause_completion(
    supervisor: &SessionSupervisor,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (arrived, observed) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    *supervisor.inner.runtime_completion_probe.lock().unwrap() = Some((arrived, released));
    (observed, release)
}

impl SessionSupervisor {
    /// Test-only cross-owner fixture for final CLI completion. Native retirement
    /// uses a real runtime and held-master root wait/SRCH; only the subsequent
    /// callback/publication is gated. No synthetic healthy receipt is installed.
    pub(crate) async fn paused_completion_fixture_for_test(
        callback_failure: bool,
    ) -> (Self, tokio::sync::oneshot::Sender<()>) {
        let handler = callback_failure.then(|| {
            RuntimeCompletionHandler(Arc::new(|_| {
                Err(MezError::new(
                    MezErrorKind::Io,
                    "held callback fixture failure",
                ))
            }))
        });
        let supervisor = Self::new_with_runtime_completion_handler(0, handler);
        let (observed, release) = pause_completion(&supervisor);
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let handle = supervisor
            .start(test_request("cli-held-completion", id))
            .await
            .unwrap();
        let native = handle.native_retirement_owner();
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
        drop(process);
        supervisor.stop(handle.session_id(), true).await.unwrap();
        observed.await.unwrap();
        assert_eq!(native.outstanding(), 0);
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
        supervisor.seal_native_admission().unwrap();
        drop(master);
        (supervisor, release)
    }
}

/// An actor can already be closed while its exact entry awaits callback
/// publication. Repeated stop admission is benign only for that terminal
/// handle; shutdown must remain pending until real completion is published.
/// A held-master native sibling proves actual wait/SRCH before the pause.
#[tokio::test(flavor = "current_thread")]
async fn closed_actor_stop_waits_for_exact_completion_without_history() {
    let supervisor = SessionSupervisor::new(0);
    let (observed, release) = pause_completion(&supervisor);
    let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let handle = supervisor
        .start(test_request("closed-before-completion", id))
        .await
        .unwrap();
    let native = handle.native_retirement_owner();
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
    drop(process);
    supervisor.stop(handle.session_id(), true).await.unwrap();
    observed.await.unwrap();
    assert!(
        handle.actor().lifecycle_state().await.is_err(),
        "actor must actually be closed"
    );
    assert_eq!(
        *handle.actor().lifecycle_state_watcher().borrow(),
        RuntimeLifecycleState::Killed
    );
    assert_eq!(native.outstanding(), 0);
    assert_eq!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    );
    handle
        .force_shutdown("redundant exact stop admission")
        .await
        .unwrap();
    let shutdown = supervisor.shutdown_all(true, Duration::from_secs(1));
    tokio::pin!(shutdown);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut shutdown)
            .await
            .is_err(),
        "actor closure was mistaken for completion"
    );
    release.send(()).unwrap();
    shutdown.await.unwrap();
    assert!(!supervisor.contains(handle.session_id()).unwrap());
    assert!(supervisor.inner.terminal().unwrap().is_empty());
    drop(master);
}

/// If real completion publication wins before stop admission, the captured
/// target uses that exact receipt without another actor request or ID lookup.
/// Publication is the natural production path, not an injected watch state.
#[tokio::test(flavor = "current_thread")]
async fn removed_exact_target_waits_for_publication_then_skips_stop_effects() {
    let supervisor = SessionSupervisor::new(0);
    let (observed, release) = pause_completion(&supervisor);
    let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let handle = supervisor
        .start(test_request("removed-before-admission", id))
        .await
        .unwrap();
    supervisor.stop(handle.session_id(), true).await.unwrap();
    observed.await.unwrap();
    supervisor.fence_native_admission(false).unwrap();
    let captured = target(&supervisor, handle.session_id());
    let mut completion = captured.completion.clone();
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while *completion.borrow_and_update() == ExactCompletionState::Pending {
            completion.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(*completion.borrow(), ExactCompletionState::Stopped);
    assert!(!supervisor.contains(handle.session_id()).unwrap());
    let wait = supervisor.stop_exact_shutdown_target(
        handle.session_id(),
        captured,
        true,
        tokio::time::Instant::now() + Duration::from_secs(1),
    );
    wait.await.unwrap();
    assert_eq!(supervisor.native_pending_count().unwrap(), 0);
    assert!(supervisor.inner.terminal().unwrap().is_empty());
    assert_eq!(
        supervisor
            .stop(handle.session_id(), true)
            .await
            .unwrap_err()
            .kind(),
        MezErrorKind::NotFound
    );
}

/// Callback errors remain authoritative failures even with empty terminal
/// history and actual native zero. The real finish path publishes Failed.
#[tokio::test(flavor = "current_thread")]
async fn completion_callback_error_cannot_publish_healthy_stop() {
    let supervisor = SessionSupervisor::new_with_runtime_completion_handler(
        0,
        Some(RuntimeCompletionHandler(Arc::new(|_| {
            Err(MezError::new(
                MezErrorKind::Io,
                "classified callback failure",
            ))
        }))),
    );
    let (observed, release) = pause_completion(&supervisor);
    let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let handle = supervisor
        .start(test_request("callback-failure", id))
        .await
        .unwrap();
    supervisor.stop(handle.session_id(), true).await.unwrap();
    observed.await.unwrap();
    let captured = target(&supervisor, handle.session_id());
    release.send(()).unwrap();
    let error = supervisor
        .await_exact_completion(
            handle.session_id(),
            captured,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap_err();
    assert!(error.message().contains("completion failed"));
    assert_eq!(supervisor.native_pending_count().unwrap(), 0);
    assert!(supervisor.inner.terminal().unwrap().is_empty());
}

/// With no preexisting receipt receiver and zero history, callback failure must
/// survive complete entry removal and make a later shutdown/CLI cleanup fail.
/// The real runtime and production callback/publication path supply evidence.
#[tokio::test(flavor = "current_thread")]
async fn callback_failure_survives_late_snapshot_without_receivers() {
    let supervisor = SessionSupervisor::new_with_runtime_completion_handler(
        0,
        Some(RuntimeCompletionHandler(Arc::new(|_| {
            Err(MezError::new(MezErrorKind::Io, "late callback failure"))
        }))),
    );
    let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let handle = supervisor
        .start(test_request("late-callback-failure", id))
        .await
        .unwrap();
    supervisor.stop(handle.session_id(), true).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while supervisor.contains(handle.session_id()).unwrap() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(supervisor.inner.terminal().unwrap().is_empty());
    assert_eq!(supervisor.native_pending_count().unwrap(), 0);
    assert!(
        supervisor
            .native_cleanup_failure()
            .unwrap()
            .unwrap()
            .contains("late callback failure")
    );
    assert!(
        supervisor
            .shutdown_all(true, Duration::from_secs(1))
            .await
            .is_err()
    );
}

/// Terminal-history publication failure must latch before entry removal even
/// without receipt subscribers. Poison only the fixture's own history mutex;
/// this is a labelled publication failure, not an OS fault or native proof.
#[tokio::test(flavor = "current_thread")]
async fn publication_failure_survives_late_snapshot_without_receivers() {
    let supervisor = SessionSupervisor::new(1);
    let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let handle = supervisor
        .start(test_request("late-publication-failure", id))
        .await
        .unwrap();
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _history = supervisor.inner.terminal.lock().unwrap();
        panic!("labelled fixture history publication failure");
    }));
    assert!(poisoned.is_err());
    supervisor.stop(handle.session_id(), true).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while supervisor.contains(handle.session_id()).unwrap() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(supervisor.native_pending_count().unwrap(), 0);
    assert!(supervisor.native_cleanup_failure().unwrap().is_some());
    assert!(
        supervisor
            .shutdown_all(true, Duration::from_secs(1))
            .await
            .is_err()
    );
}

/// A stale generation snapshot must fail before dispatch and cannot stop a
/// replacement. The fixture restores its own exact generation for independent
/// cleanup, without searching for or targeting any unrelated process.
#[tokio::test(flavor = "current_thread")]
async fn exact_stop_rejects_replacement_generation_before_admission() {
    let supervisor = SessionSupervisor::new(0);
    let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let handle = supervisor
        .start(test_request("replacement-fence", id))
        .await
        .unwrap();
    supervisor.fence_native_admission(false).unwrap();
    let captured = target(&supervisor, handle.session_id());
    let original = captured.generation;
    supervisor
        .inner
        .entries()
        .unwrap()
        .get_mut(handle.session_id())
        .unwrap()
        .generation = original + 1;
    let result = supervisor
        .stop_exact_shutdown_target(
            handle.session_id(),
            captured,
            true,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await;
    let live = handle.lifecycle_state().await.unwrap();
    supervisor
        .inner
        .entries()
        .unwrap()
        .get_mut(handle.session_id())
        .unwrap()
        .generation = original;
    supervisor
        .shutdown_all(true, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(result.unwrap_err().kind(), MezErrorKind::Conflict);
    assert_eq!(live, RuntimeLifecycleState::Running);
}

/// A lost Pending publisher is unproven even at sealed native zero. This is
/// classification-only evidence, not a synthetic successful runtime or reap.
#[tokio::test(flavor = "current_thread")]
async fn closed_pending_completion_publisher_fails_closed() {
    let supervisor = SessionSupervisor::new(0);
    let native = PaneProcessRetirementOwner::default();
    native.seal();
    let (sender, completion) = watch::channel(ExactCompletionState::Pending);
    drop(sender);
    let target = ExactShutdownTarget {
        generation: 1,
        native,
        completion,
        request: None,
    };
    let error = supervisor
        .await_exact_completion(
            "$missing-publisher",
            target,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap_err();
    assert!(error.message().contains("publisher was lost"));
}

/// A still-admitted reservation blocks native and runtime completion even after
/// actor shutdown. Releasing that empty slot is not a reap receipt: the held
/// master root must independently have a successful actual wait and strict SRCH.
#[tokio::test(flavor = "current_thread")]
async fn pending_native_slot_cannot_be_hidden_by_actor_shutdown() {
    let supervisor = SessionSupervisor::new(0);
    let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let handle = supervisor
        .start(test_request("pending-native-slot", id))
        .await
        .unwrap();
    let native = handle.native_retirement_owner();
    let reservation = native.reserve().unwrap();
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
    drop(process);
    let first = supervisor
        .shutdown_all(true, Duration::from_millis(30))
        .await;
    let still_pending = native.outstanding() >= 1;
    drop(reservation);
    let settled = supervisor.shutdown_all(true, Duration::from_secs(1)).await;
    let absent = rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH);
    drop(master);
    assert!(
        first.is_err() && still_pending,
        "pending reservation fabricated completion"
    );
    assert!(
        settled.is_ok() && native.outstanding() == 0 && absent,
        "independent native waits did not settle"
    );
}

/// Sticky native failure remains a failed overall shutdown even when the real
/// runtime finishes and its own exact completion receipt is healthy.
#[tokio::test(flavor = "current_thread")]
async fn exact_completion_does_not_erase_host_native_failure() {
    let supervisor = SessionSupervisor::new(0);
    let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    supervisor
        .start(test_request("native-failure-sticky", id))
        .await
        .unwrap();
    supervisor.retain_native_failure_for_test();
    let result = supervisor.shutdown_all(true, Duration::from_secs(1)).await;
    assert!(result.is_err());
    assert_eq!(supervisor.native_pending_count().unwrap(), 0);
    assert!(supervisor.native_cleanup_failure().unwrap().is_some());
}
