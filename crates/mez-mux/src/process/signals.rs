//! Signal translation and process-group signaling for pane teardown.
//!
//! The process module prefers sending signals to the pane process group when the
//! PTY backend exposes a leader, falling back to child termination otherwise.

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group};

use crate::{MuxError as MezError, Result};

/// Runs the send signal to pane process group operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn send_signal_to_pane_process_group(
    process_group_leader: i32,
    signal: Signal,
) -> Result<()> {
    if process_group_leader < 0 {
        return Err(MezError::invalid_state("pane process group id is invalid"));
    }
    let pid = Pid::from_raw(process_group_leader)
        .ok_or_else(|| MezError::invalid_state("pane process group id is invalid"))?;
    match kill_process_group(pid, signal) {
        Ok(()) => Ok(()),
        Err(Errno::SRCH) => Ok(()),
        Err(error) => Err(MezError::io(format!(
            "failed to send signal {} to pane process group {}: {error}",
            signal.as_raw(),
            process_group_leader
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
    use super::signal_number_from_portable_name;

    /// Verifies resolves standard signal names.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn resolves_standard_signal_names() {
        assert_eq!(signal_number_from_portable_name("sigint"), Some(2));
        assert_eq!(signal_number_from_portable_name("sigquit"), Some(3));
        assert_eq!(signal_number_from_portable_name("sigkill"), Some(9));
        assert_eq!(signal_number_from_portable_name("sigsegv"), Some(11));
        assert_eq!(signal_number_from_portable_name("sigpipe"), Some(13));
        assert_eq!(signal_number_from_portable_name("sigterm"), Some(15));
    }

    /// Verifies resolves descriptive signal names.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn resolves_descriptive_signal_names() {
        assert_eq!(signal_number_from_portable_name("interrupt"), Some(2));
        assert_eq!(signal_number_from_portable_name("killed"), Some(9));
        assert_eq!(
            signal_number_from_portable_name("segmentation fault"),
            Some(11)
        );
        assert_eq!(signal_number_from_portable_name("bus error"), Some(7));
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
}
