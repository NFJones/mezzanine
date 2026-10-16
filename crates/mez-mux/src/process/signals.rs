//! Signal translation and process-group signaling for pane teardown.
//!
//! Callers establish immutable birth-root authority and poll the exact owned
//! child before group sends. Foreground PTY observations are not authority.

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group};

use crate::{MuxError as MezError, Result};

/// Signals a retained exact-child group without the legacy Darwin EPERM
/// exception. Only typed ESRCH, or EPERM followed by authoritative group
/// absence, is benign; permission/I/O failures remain unproven errors.
pub(super) fn send_signal_to_retained_pane_group(group: i32, signal: Signal) -> Result<()> {
    // Rustix asserts on negative values inside from_raw; filtering afterward
    // is too late, especially when this path runs during owner destruction.
    if group <= 0 {
        return Err(MezError::invalid_state("pane process group id is invalid"));
    }
    let pid = Pid::from_raw(group)
        .ok_or_else(|| MezError::invalid_state("pane process group id is invalid"))?;
    match kill_process_group(pid, signal) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(Errno::PERM)
            if matches!(
                rustix::process::test_kill_process_group(pid),
                Err(Errno::SRCH)
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(MezError::io(format!(
            "native group signal {} failed for owned group {group}: {error}",
            signal.as_raw()
        ))),
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
        let error = send_signal_to_pane_process_group(0, Signal::TERM).unwrap_err();

        assert!(error.message().contains("process group id is invalid"));
    }

    /// Retained cleanup must reject every nonpositive group before calling the
    /// asserting Rustix constructor or any signal syscall. The same helper is
    /// used during last-owner Drop, where a second panic would abort cleanup.
    #[test]
    fn retained_group_rejects_negative_and_zero_before_pid_construction() {
        for group in [i32::MIN, -1, 0] {
            let error = super::send_signal_to_retained_pane_group(group, Signal::KILL).unwrap_err();
            assert!(error.message().contains("process group id is invalid"));
        }
    }
}
