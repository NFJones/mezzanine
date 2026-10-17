//! Public explicit retry preparation and independent foreground/host progress.
//!
//! Creation-time/stale negative facts are saved before fixture-only natural
//! settlement. Held PTY masters and child replies distinguish surviving native
//! roots from channel state, task completion, or an injected classification.

use super::*;
use std::os::fd::OwnedFd;

/// Bounded fixture-only input, never a raw signal or process lookup.
pub(super) async fn input(master: &OwnedFd, mut bytes: &[u8]) -> bool {
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

/// Reads a child-generated fixture response with echo disabled by its shell.
pub(super) async fn marker(master: &OwnedFd, expected: &str) -> bool {
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

/// Produces a real returned root with no native termination plan yet.
async fn outcome() -> (
    CliExit,
    PaneProcessRetirementOwner,
    OwnedFd,
    rustix::process::Pid,
) {
    let mut native = CliNativeOwnership::default();
    let owner = native.foreground();
    let process = mez_mux::process::spawn_retained_argv_pty_process(std::path::Path::new("/bin/sh"),
        &["-c".into(), "trap '' HUP TERM; stty -echo; printf 'cli-scope-ready\\n'; while IFS= read -r line; do if [ \"$line\" = fixture-finish ]; then exit 0; fi; printf 'cli-scope-child:%s\\n' \"$line\"; done".into()],
        &[], mez_mux::layout::Size::new(80, 24).unwrap(), None, owner.reserve().unwrap()).unwrap();
    let pid =
        rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
    let master = process.duplicate_master_fd().unwrap();
    assert!(marker(&master, "cli-scope-ready").await);
    drop(process);
    owner.seal();
    (CliExit::owned(17, native), owner, master, pid)
}

/// Supplies an admitted graceful root whose real HUP receipt remains visible
/// through the held master. The outer loop survives interrupted read, so HUP
/// does not accidentally make cancellation look like successful retirement.
pub(super) async fn cancellable_outcome() -> (
    CliExit,
    PaneProcessRetirementOwner,
    OwnedFd,
    rustix::process::Pid,
) {
    let mut native = CliNativeOwnership::default();
    let owner = native.foreground();
    let mut process = mez_mux::process::spawn_retained_argv_pty_process(std::path::Path::new("/bin/sh"),
        &["-c".into(), "trap 'printf cancel-hup\\\\n' HUP; trap '' TERM; stty -echo; printf 'cli-scope-ready\\n'; while :; do if IFS= read -r line; then printf 'cli-scope-child:%s\\n' \"$line\"; fi; done".into()],
        &[], mez_mux::layout::Size::new(80, 24).unwrap(), None, owner.reserve().unwrap()).unwrap();
    let pid =
        rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
    let master = process.duplicate_master_fd().unwrap();
    assert!(marker(&master, "cli-scope-ready").await);
    process.begin_native_termination(false).unwrap();
    drop(process);
    owner.seal();
    (CliExit::owned(17, native), owner, master, pid)
}

/// Settles only the same returned native handle after independent fixture input.
async fn finish(owner: &PaneProcessRetirementOwner, master: &OwnedFd) -> bool {
    let sent = input(master, b"fixture-finish\n").await;
    let mut drain = owner.begin_drain().unwrap();
    let Some(mut child) = drain.take_returned() else {
        return owner.outstanding() == 0;
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while tokio::time::Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return sent;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    false
}

/// Public unpolled retry preserves both initial-registration identity and the
/// creation clock. Neither a later registration nor delayed first poll may
/// mint/force; original command status stays17 after independent fixture wait.
#[tokio::test(flavor = "current_thread")]
async fn unpolled_cli_retry_keeps_preparation_and_creation_deadline() {
    for registration_change in [false, true] {
        let (mut exit, owner, master, pid) = outcome().await;
        let result = {
            let old = exit.retry_native_cleanup(if registration_change {
                Duration::from_secs(1)
            } else {
                Duration::from_millis(10)
            });
            if registration_change {
                owner
                    .capture_cleanup_attempt(std::time::Instant::now() + Duration::from_secs(1))
                    .unwrap();
            } else {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            old.await
        };
        let pending = owner.outstanding() == 1;
        let live = rustix::process::test_kill_process(pid) == Ok(());
        let replied = input(&master, b"unpolled-cli-probe\n").await
            && marker(&master, "cli-scope-child:unpolled-cli-probe").await;
        let settled = finish(&owner, &master).await;
        let absent = rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH);
        drop(master);
        assert!(
            settled && absent && owner.outstanding() == 0,
            "independent owned fixture wait failed"
        );
        let error = result.unwrap_err();
        assert!(error.message().contains(if registration_change {
            "stale"
        } else {
            "expired"
        }));
        assert!(
            pending && live && replied,
            "unpolled preparation issued native effects"
        );
        assert_eq!(exit.code(), 17);
    }
}

/// An old public explicit future cannot adopt a distinct retry admitted after
/// its preparation. Capture liveness and a fresh response before the independent
/// new token supplies actual wait/SRCH; the old command status remains17.
#[tokio::test(flavor = "current_thread")]
async fn unpolled_cli_retry_cannot_adopt_a_new_explicit_epoch() {
    let mut native = CliNativeOwnership::default();
    let owner = native.foreground();
    let mut process = mez_mux::process::spawn_retained_argv_pty_process(std::path::Path::new("/bin/sh"),
        &["-c".into(), "trap '' HUP TERM; stty -echo; printf 'cli-scope-ready\\n'; while IFS= read -r line; do printf 'cli-scope-child:%s\\n' \"$line\"; done".into()],
        &[], mez_mux::layout::Size::new(80, 24).unwrap(), None, owner.reserve().unwrap()).unwrap();
    let pid =
        rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
    let master = process.duplicate_master_fd().unwrap();
    assert!(marker(&master, "cli-scope-ready").await);
    let original = process.begin_native_termination(true).unwrap();
    drop(process);
    owner.seal();
    let mut exit = CliExit::owned(17, native);
    let (old_result, token) = {
        let old = exit.retry_native_cleanup(Duration::from_secs(3));
        tokio::time::sleep_until(
            tokio::time::Instant::from_std(original) + Duration::from_millis(10),
        )
        .await;
        let token = owner
            .prepare_explicit_cleanup_retry(std::time::Instant::now() + Duration::from_secs(1))
            .unwrap()
            .admit()
            .unwrap();
        (old.await, token)
    };
    let pending = owner.outstanding() == 1;
    let live = rustix::process::test_kill_process(pid) == Ok(());
    let replied = input(&master, b"new-epoch-probe\n").await
        && marker(&master, "cli-scope-child:new-epoch-probe").await;
    let cleanup = drain_native_pane_retirement_for_attempt(
        &owner,
        &token,
        tokio::time::Instant::from_std(token.deadline()),
    )
    .await;
    let absent = rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH);
    drop(master);
    assert!(
        cleanup.is_ok() && absent && owner.outstanding() == 0,
        "independent authorized root wait failed"
    );
    assert!(old_result.unwrap_err().message().contains("stale") && pending && live && replied);
    assert_eq!(exit.code(), 17);
}

/// A busy foreground reservation cannot starve an eligible real expired host
/// owner. Only the public explicit CLI call admits the host epoch. Its native
/// wait/SRCH precedes dropping the held master; overall Busy/status remains.
#[tokio::test(flavor = "current_thread")]
async fn busy_foreground_does_not_skip_eligible_explicit_host_retry() {
    let (host, master, pid) = SessionSupervisor::expired_native_retry_fixture_for_test().await;
    let mut native = CliNativeOwnership::default();
    let foreground = native.foreground();
    let reservation = foreground.reserve().unwrap();
    foreground.seal();
    native.retain_host(host).unwrap();
    let mut exit = CliExit::owned(17, native);
    let result = exit.retry_native_cleanup(Duration::from_secs(1)).await;
    let host_reaped = exit
        .native
        .host
        .as_ref()
        .unwrap()
        .native_pending_count()
        .unwrap()
        == 0;
    let absent = rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH);
    let busy_preserved = foreground.outstanding() == 1;
    drop(reservation);
    drop(master);
    assert!(
        host_reaped && absent,
        "eligible host native root did not reap independently"
    );
    assert!(result.is_err() && busy_preserved);
    assert_eq!(exit.pending_native_children().unwrap(), 0);
    assert_eq!(exit.code(), 17);
}
