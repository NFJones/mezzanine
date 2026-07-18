//! Synchronous child-process waiting primitives.
//!
//! Product adapters occasionally need a bounded synchronous wait outside the
//! Tokio runtime. This module owns that host-specific polling so callers do not
//! install process-global signal handlers or duplicate timeout behavior.

use std::io;
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

/// Waits for `child` until it exits or `timeout` elapses.
///
/// Returns `Ok(Some(status))` for a completed child and `Ok(None)` at the
/// deadline. The caller remains responsible for killing and reaping a timed-out
/// child. I/O errors from `Child::try_wait` are returned unchanged.
pub(crate) fn wait_for_child_with_timeout(
    child: &mut Child,
    timeout: Duration,
) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(10)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Verifies a child that exits promptly returns its exact status.
    #[test]
    fn bounded_child_wait_returns_completed_status() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();

        let status = wait_for_child_with_timeout(&mut child, Duration::from_secs(1))
            .unwrap()
            .unwrap();

        assert_eq!(status.code(), Some(7));
    }

    /// Verifies the timeout path leaves ownership of child cleanup to the
    /// caller instead of blocking past the requested deadline.
    #[test]
    fn bounded_child_wait_reports_timeout() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 1"])
            .spawn()
            .unwrap();

        let status = wait_for_child_with_timeout(&mut child, Duration::from_millis(1)).unwrap();

        assert!(status.is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
