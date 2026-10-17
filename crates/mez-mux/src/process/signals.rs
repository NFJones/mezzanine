//! Signal translation and process-group signaling for pane teardown.
//!
//! Callers establish immutable birth-root authority and poll the exact owned
//! child before group sends. Foreground PTY observations are not authority.
//! A child can exit between that poll and a send: Darwin's zombie-only groups
//! report EPERM even to signal-zero probes. Reap only the retained child before
//! probing absence, never resend a cleanup signal or accept denial as absence.

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group};

use crate::{MuxError as MezError, Result};

/// Signals a retained exact-child group without the legacy Darwin EPERM
/// exception. Only typed ESRCH, or EPERM followed by authoritative group
/// absence, is benign; permission/I/O failures remain unproven errors. On EPERM
/// the caller's nonblocking exact-root poll runs before the absence probe and
/// must cache any real wait result for later capacity/accounting settlement.
pub(super) fn send_signal_to_retained_pane_group(
    group: i32,
    signal: Signal,
    poll_owned_root: impl FnOnce() -> std::io::Result<()>,
) -> Result<()> {
    // Rustix asserts on negative values inside from_raw; filtering afterward
    // is too late, especially when this path runs during owner destruction.
    if group <= 0 {
        return Err(MezError::invalid_state("pane process group id is invalid"));
    }
    let pid = Pid::from_raw(group)
        .ok_or_else(|| MezError::invalid_state("pane process group id is invalid"))?;
    reconcile_retained_group_signal(
        group,
        signal,
        kill_process_group(pid, signal),
        poll_owned_root,
        || rustix::process::test_kill_process_group(pid),
    )
}

/// Classifies typed signal/probe outcomes without widening numeric authority.
/// The code-owned wait callback observes only the retained exact child; injected
/// operations let tests model exit between the caller's wait and group send.
fn reconcile_retained_group_signal(
    group: i32,
    signal: Signal,
    sent: rustix::io::Result<()>,
    poll_owned_root: impl FnOnce() -> std::io::Result<()>,
    probe_group: impl FnOnce() -> rustix::io::Result<()>,
) -> Result<()> {
    match sent {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(error) => {
            let diagnostic = format!(
                "native group signal {} failed for owned group {group}: {error}",
                signal.as_raw()
            );
            if error == Errno::PERM {
                // Only a real cached native wait can remove a zombie root.
                // Ok(None) is not completion; the subsequent group probe must
                // still prove absence, including any surviving group members.
                poll_owned_root().map_err(|wait| {
                    MezError::io(format!("{diagnostic}; exact root wait failed: {wait}"))
                })?;
                if matches!(probe_group(), Err(Errno::SRCH)) {
                    return Ok(());
                }
            }
            Err(MezError::io(diagnostic))
        }
    }
}

/// Runs the signal number from portable name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn signal_number_from_portable_name(name: &str) -> Option<i32> {
    let lower = name.to_ascii_lowercase();
    if lower.contains("hangup") || lower.contains("sighup") {
        Some(Signal::HUP.as_raw())
    } else if lower.contains("terminated") || lower.contains("sigterm") {
        Some(Signal::TERM.as_raw())
    } else if lower.contains("killed") || lower.contains("sigkill") {
        Some(Signal::KILL.as_raw())
    } else if lower.contains("interrupt") || lower.contains("sigint") {
        Some(Signal::INT.as_raw())
    } else if lower.contains("quit") || lower.contains("sigquit") {
        Some(Signal::QUIT.as_raw())
    } else if lower.contains("sigill") || lower.contains("illegal") {
        Some(Signal::ILL.as_raw())
    } else if lower.contains("sigtrap") || lower.contains("trace") {
        Some(Signal::TRAP.as_raw())
    } else if lower.contains("sigabrt") || lower.contains("abort") {
        Some(Signal::ABORT.as_raw())
    } else if lower.contains("sigbus") || lower.contains("bus") {
        Some(Signal::BUS.as_raw())
    } else if lower.contains("sigfpe") || lower.contains("floating") {
        Some(Signal::FPE.as_raw())
    } else if lower.contains("sigusr1") || lower.contains("user1") {
        Some(Signal::USR1.as_raw())
    } else if lower.contains("sigsegv") || lower.contains("segmentation") {
        Some(Signal::SEGV.as_raw())
    } else if lower.contains("sigusr2") || lower.contains("user2") {
        Some(Signal::USR2.as_raw())
    } else if lower.contains("sigpipe") || lower.contains("broken pipe") || lower.contains("pipe") {
        Some(Signal::PIPE.as_raw())
    } else if lower.contains("sigalrm") || lower.contains("alarm") {
        Some(Signal::ALARM.as_raw())
    } else {
        None
    }
}

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests {
    use super::{
        send_signal_to_retained_pane_group as send_signal_to_pane_process_group,
        signal_number_from_portable_name,
    };
    use rustix::process::Signal;

    /// Verifies resolves standard signal names.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn resolves_standard_signal_names() {
        assert_eq!(
            signal_number_from_portable_name("sigint"),
            Some(Signal::INT.as_raw())
        );
        assert_eq!(
            signal_number_from_portable_name("sigquit"),
            Some(Signal::QUIT.as_raw())
        );
        assert_eq!(
            signal_number_from_portable_name("sigkill"),
            Some(Signal::KILL.as_raw())
        );
        assert_eq!(
            signal_number_from_portable_name("sigsegv"),
            Some(Signal::SEGV.as_raw())
        );
        assert_eq!(
            signal_number_from_portable_name("sigpipe"),
            Some(Signal::PIPE.as_raw())
        );
        assert_eq!(
            signal_number_from_portable_name("sigterm"),
            Some(Signal::TERM.as_raw())
        );
    }

    /// Verifies resolves descriptive signal names.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn resolves_descriptive_signal_names() {
        assert_eq!(
            signal_number_from_portable_name("interrupt"),
            Some(Signal::INT.as_raw())
        );
        assert_eq!(
            signal_number_from_portable_name("killed"),
            Some(Signal::KILL.as_raw())
        );
        assert_eq!(
            signal_number_from_portable_name("segmentation fault"),
            Some(Signal::SEGV.as_raw())
        );
        assert_eq!(
            signal_number_from_portable_name("bus error"),
            Some(Signal::BUS.as_raw())
        );
    }

    /// Verifies returns none for unrecognized signals.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn returns_none_for_unrecognized_signals() {
        assert!(signal_number_from_portable_name("sigwinch").is_none());
        assert!(signal_number_from_portable_name("sigcont").is_none());
        assert!(signal_number_from_portable_name("").is_none());
        assert!(signal_number_from_portable_name("unknown signal").is_none());
    }

    /// Verifies a zero process-group identifier is rejected before Rustix.
    ///
    /// Darwin PTY metadata can transiently report group zero while a shell is
    /// exiting. Rustix reserves zero and asserts before issuing a syscall, so
    /// teardown must convert the value into an ordinary typed error instead of
    /// panicking during test or production cleanup.
    #[test]
    fn rejects_zero_process_group_without_panicking() {
        let error = send_signal_to_pane_process_group(0, Signal::TERM, || {
            panic!("invalid group must not poll a child")
        })
        .unwrap_err();

        assert!(error.message().contains("process group id is invalid"));
    }

    /// Retained cleanup must reject every nonpositive group before calling the
    /// asserting Rustix constructor or any signal syscall. The same helper is
    /// used during last-owner Drop, where a second panic would abort cleanup.
    #[test]
    fn retained_group_rejects_negative_and_zero_before_pid_construction() {
        for group in [i32::MIN, -1, 0] {
            let error = super::send_signal_to_retained_pane_group(group, Signal::KILL, || {
                panic!("invalid group must not poll a child")
            })
            .unwrap_err();
            assert!(error.message().contains("process group id is invalid"));
        }
    }

    /// Darwin rejects signals and even signal-zero probes for a zombie-only
    /// group. Exact-child wait makes absence observable; probe-before-wait would
    /// incorrectly latch EPERM forever. No second cleanup signal is sent.
    #[test]
    fn permission_race_polls_exact_root_before_group_absence() {
        let reaped = std::cell::Cell::new(false);
        let result = super::reconcile_retained_group_signal(
            42,
            Signal::TERM,
            Err(rustix::io::Errno::PERM),
            || {
                reaped.set(true);
                Ok(())
            },
            || {
                if reaped.get() {
                    Err(rustix::io::Errno::SRCH)
                } else {
                    Err(rustix::io::Errno::PERM)
                }
            },
        );
        assert!(
            result.is_ok(),
            "exact reap must precede absence probe: {result:?}"
        );
        assert!(reaped.get());
    }

    /// A root can exit while another birth-group member survives or access
    /// stays denied. Neither root completion nor unreadable group state alone
    /// proves absence, so permission and other probe failures remain errors.
    #[test]
    fn permission_race_requires_authoritative_group_absence() {
        for probe in [
            Ok(()),
            Err(rustix::io::Errno::PERM),
            Err(rustix::io::Errno::IO),
        ] {
            assert!(
                super::reconcile_retained_group_signal(
                    42,
                    Signal::KILL,
                    Err(rustix::io::Errno::PERM),
                    || Ok(()),
                    || probe,
                )
                .is_err()
            );
        }
    }

    /// A denied send followed by an exact-child wait failure must keep both
    /// causes. A group probe cannot hide failed native ownership observation.
    #[test]
    fn permission_race_preserves_exact_wait_failure() {
        let error = super::reconcile_retained_group_signal(
            42,
            Signal::TERM,
            Err(rustix::io::Errno::PERM),
            || Err(std::io::Error::other("exact wait unavailable")),
            || panic!("failed exact wait must not be bypassed by a probe"),
        )
        .unwrap_err();
        assert!(error.message().contains("native group signal"));
        assert!(error.message().contains("exact wait unavailable"));
    }

    /// Only EPERM has exit-race reconciliation. Success/ESRCH need no callback,
    /// and an unrelated I/O failure cannot be washed away by a later absence.
    #[test]
    fn non_permission_signals_skip_exit_race_reconciliation() {
        for sent in [
            Ok(()),
            Err(rustix::io::Errno::SRCH),
            Err(rustix::io::Errno::IO),
        ] {
            let expected_success = sent.is_ok() || sent == Err(rustix::io::Errno::SRCH);
            let result = super::reconcile_retained_group_signal(
                42,
                Signal::HUP,
                sent,
                || panic!("non-EPERM must not trigger another root poll"),
                || panic!("non-EPERM must not probe another group"),
            );
            assert_eq!(result.is_ok(), expected_success);
        }
    }

    /// Exercises the real Darwin kernel without a PTY: an exclusively owned
    /// child exits in its own birth group and remains unreaped until the signal
    /// helper's callback. Both initial probes must deny; only exact wait makes
    /// ESRCH observable. The fixture always kills/waits before assertions so a
    /// regression cannot leave a running native child behind.
    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_zombie_group_permission_race_reaps_before_absence_probe() {
        use std::os::unix::process::CommandExt;
        use std::time::{Duration, Instant};

        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        let group = i32::try_from(child.id()).unwrap();
        let pid = rustix::process::Pid::from_raw(group).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        // EPERM alone is not proof of zombie state. Observe EXITED without
        // consuming the exact child's wait receipt before invoking the helper.
        let exited = loop {
            let observed = rustix::process::waitid(
                rustix::process::WaitId::Pid(pid),
                rustix::process::WaitIdOptions::EXITED
                    | rustix::process::WaitIdOptions::NOWAIT
                    | rustix::process::WaitIdOptions::NOHANG,
            );
            if !matches!(observed, Ok(None)) || Instant::now() >= deadline {
                break observed;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        let before = rustix::process::test_kill_process_group(pid);
        let mut reaped = false;
        let result = super::send_signal_to_retained_pane_group(group, Signal::TERM, || {
            reaped = child.try_wait()?.is_some();
            Ok(())
        });
        // std Child suppresses kill when its native status is already cached.
        let _ = child.kill();
        let waited = child.wait();
        assert!(
            matches!(exited, Ok(Some(_))),
            "owned child must exit before the probe: {exited:?}"
        );
        assert_eq!(before, Err(rustix::io::Errno::PERM));
        assert!(
            result.is_ok(),
            "zombie-only group must reconcile: {result:?}"
        );
        assert!(reaped, "EPERM reconciliation must reap the exact root");
        assert!(waited.unwrap().success());
    }
}
