//! Host scope identity, late-owner rejection and CLI-only native retry fixtures.
//!
//! The cleanup-pending entry is labelled fixture metadata, not an actual actor
//! completion. Native roots, held PTYs, reply probes and waits/SRCH are real;
//! they prove only the corresponding process/authority boundary.

use super::*;
use std::os::fd::OwnedFd;

/// Writes only bounded fixture input through its exclusively held master.
async fn input(master: &OwnedFd, mut bytes: &[u8]) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while !bytes.is_empty() && tokio::time::Instant::now() < deadline {
        match rustix::io::write(master, bytes) {
            Ok(0) => return false,
            Ok(count) => bytes = &bytes[count..],
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
            Err(_) => return false,
        }
        if !bytes.is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    bytes.is_empty()
}

/// Requires a child-generated marker rather than PTY echo.
async fn marker(master: &OwnedFd, expected: &str) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    let mut output = Vec::new();
    while tokio::time::Instant::now() < deadline && output.len() < 64 * 1024 {
        let mut bytes = [0u8; 4096];
        match rustix::io::read(master, &mut bytes) {
            Ok(0) => break,
            Ok(count) => output.extend_from_slice(&bytes[..count]),
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
            Err(_) => break,
        }
        if String::from_utf8_lossy(&output).contains(expected) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    false
}

/// Registers one actual returned root under classified cleanup-pending metadata.
async fn install_root(
    supervisor: &SessionSupervisor,
    id: &str,
) -> (
    PaneProcessRetirementOwner,
    OwnedFd,
    rustix::process::Pid,
    std::time::Instant,
) {
    let native = PaneProcessRetirementOwner::default();
    let mut process = mez_mux::process::spawn_retained_argv_pty_process(std::path::Path::new("/bin/sh"),
        &["-c".into(), "trap '' HUP TERM; stty -echo; printf 'host-scope-ready\\n'; while IFS= read -r line; do printf 'host-scope-child:%s\\n' \"$line\"; done".into()],
        &[], Size::new(80, 24).unwrap(), None, native.reserve().unwrap()).unwrap();
    let pid =
        rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
    let master = process.duplicate_master_fd().unwrap();
    assert!(marker(&master, "host-scope-ready").await);
    let deadline = process.begin_native_termination(true).unwrap();
    drop(process);
    supervisor.inner.entries().unwrap().insert(
        id.into(),
        SupervisorEntry {
            generation: 1,
            native: native.clone(),
            completion: watch::channel(ExactCompletionState::Pending).0,
            completion_claimed: false,
            state: SupervisorEntryState::CleanupPending {
                failure: "classified retained native fixture".into(),
            },
        },
    );
    (native, master, pid, deadline)
}

impl SessionSupervisor {
    /// Cross-owner test fixture for CLI sibling progress. It supplies actual
    /// expired native authority, not a fabricated successful runtime receipt.
    pub(crate) async fn expired_native_retry_fixture_for_test()
    -> (Self, OwnedFd, rustix::process::Pid) {
        let supervisor = Self::new(0);
        let (_, master, pid, deadline) =
            install_root(&supervisor, "$cli-native-retry-fixture").await;
        tokio::time::sleep_until(
            tokio::time::Instant::from_std(deadline) + Duration::from_millis(10),
        )
        .await;
        (supervisor, master, pid)
    }
}

/// Single-session stop is also an automatic native continuation: a precreated
/// stop cannot pick up a later explicit epoch from the cleanup-pending entry.
/// Save the original live response before independent authorized wait/SRCH.
#[tokio::test(flavor = "current_thread")]
async fn unpolled_session_stop_does_not_adopt_new_retry_epoch() {
    let supervisor = SessionSupervisor::new(0);
    let id = "$single-stop-attempt";
    let (native, master, pid, original) = install_root(&supervisor, id).await;
    let old = supervisor.stop(id, true);
    native.seal();
    tokio::time::sleep_until(tokio::time::Instant::from_std(original) + Duration::from_millis(10))
        .await;
    let token = native
        .prepare_explicit_cleanup_retry(
            (tokio::time::Instant::now() + Duration::from_secs(1)).into_std(),
        )
        .unwrap()
        .admit()
        .unwrap();
    let result = old.await;
    let live = rustix::process::test_kill_process(pid) == Ok(());
    let replied = input(&master, b"single-stop-probe\n").await
        && marker(&master, "host-scope-child:single-stop-probe").await;
    let pending = native.outstanding() == 1;
    let cleanup = crate::host::async_runtime::drain_native_pane_retirement_for_attempt(
        &native,
        &token,
        tokio::time::Instant::from_std(token.deadline()),
    )
    .await;
    let absent = rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH);
    supervisor.finalize_settled_native_entries().unwrap();
    drop(master);
    assert!(cleanup.is_ok() && absent && native.outstanding() == 0);
    assert!(result.unwrap_err().message().contains("stale") && pending && live && replied);
}

/// Expired unpolled host authority cannot cancel a real actor or create stop
/// intent, even with a longer observation budget. Capture cancel/lifecycle and
/// a fresh held-master root reply before independent force/wait cleanup.
#[tokio::test(flavor = "current_thread")]
async fn expired_host_scopes_do_not_cancel_or_stop_live_actors() {
    for longer_outer in [false, true] {
        let supervisor = SessionSupervisor::new(0);
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let handle = supervisor
            .start(test_request("expired-effect-scope", id))
            .await
            .unwrap();
        let native = handle.native_retirement_owner();
        let process = mez_mux::process::spawn_retained_argv_pty_process(std::path::Path::new("/bin/sh"),
            &["-c".into(), "trap '' HUP TERM; stty -echo; printf 'host-scope-ready\\n'; while IFS= read -r line; do printf 'host-scope-child:%s\\n' \"$line\"; done".into()],
            &[], Size::new(80, 24).unwrap(), None, native.reserve().unwrap()).unwrap();
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
        let master = process.duplicate_master_fd().unwrap();
        assert!(marker(&master, "host-scope-ready").await);
        drop(process);
        let cancel = {
            let entries = supervisor.inner.entries().unwrap();
            let SupervisorEntryState::Running { cancel, .. } =
                &entries.get(handle.session_id()).unwrap().state
            else {
                panic!("fixture runtime is not running")
            };
            cancel.clone()
        };
        let deadline = tokio::time::Instant::now() + Duration::from_millis(10);
        let result = if longer_outer {
            let scope = supervisor.capture_native_continuation_scope(deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
            supervisor
                .finish_native_retirement_captured(
                    scope,
                    tokio::time::Instant::now() + Duration::from_millis(30),
                )
                .await
        } else {
            let old = supervisor.finish_native_retirement(deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
            old.await
        };
        let cancellation_sent = *cancel.borrow();
        let running = matches!(
            handle.lifecycle_state().await,
            Ok(RuntimeLifecycleState::Running)
        );
        let live = rustix::process::test_kill_process(pid) == Ok(());
        let replied = input(&master, b"expired-effect-probe\n").await
            && marker(&master, "host-scope-child:expired-effect-probe").await;
        let cleanup = supervisor.shutdown_all(true, Duration::from_secs(1)).await;
        let absent = rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH);
        drop(master);
        assert!(
            cleanup.is_ok() && absent && native.outstanding() == 0,
            "independent fixture shutdown failed: {cleanup:?}"
        );
        assert!(
            result.is_err() && !cancellation_sent && running && live && replied,
            "expired host scope issued effects before independent cleanup"
        );
    }
}

/// A host scope created before polling cannot adopt either a captured owner's
/// new retry epoch or a late owner's retry epoch. Its original root must still
/// respond before independent explicit-token cleanup supplies wait/SRCH proof.
#[tokio::test(flavor = "current_thread")]
async fn old_host_scope_rejects_captured_and_late_retry_epochs() {
    for late in [false, true] {
        let supervisor = SessionSupervisor::new(0);
        let id = "$host-attempt-scope";
        let root = if late {
            None
        } else {
            Some(install_root(&supervisor, id).await)
        };
        let outer = tokio::time::Instant::now() + Duration::from_secs(2);
        let old = supervisor.finish_native_retirement(outer);
        let (native, master, pid, original) = match root {
            Some(root) => root,
            None => install_root(&supervisor, id).await,
        };
        native.seal();
        tokio::time::sleep_until(
            tokio::time::Instant::from_std(original) + Duration::from_millis(10),
        )
        .await;
        let token = native
            .prepare_explicit_cleanup_retry(
                (tokio::time::Instant::now() + Duration::from_secs(2)).into_std(),
            )
            .unwrap()
            .admit()
            .unwrap();
        let old_result = old.await;
        let pending = native.outstanding() == 1;
        let live = rustix::process::test_kill_process(pid) == Ok(());
        let replied = input(&master, b"host-old-scope-probe\n").await
            && marker(&master, "host-scope-child:host-old-scope-probe").await;
        let cleanup = crate::host::async_runtime::drain_native_pane_retirement_for_attempt(
            &native,
            &token,
            tokio::time::Instant::from_std(token.deadline()),
        )
        .await;
        let absent = rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH);
        supervisor.finalize_settled_native_entries().unwrap();
        drop(master);
        assert!(
            cleanup.is_ok() && absent && native.outstanding() == 0,
            "independent token cleanup did not settle"
        );
        let error = old_result.unwrap_err();
        assert!(
            error
                .message()
                .contains(if late { "late" } else { "stale" }),
            "wrong rejection: {error}"
        );
        assert!(
            pending && live && replied,
            "old host scope affected a newly authorized epoch"
        );
    }
}
