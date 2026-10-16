//! Reader-owned lifecycle completion regressions using real, exclusively owned
//! pipes. Synchronization controls notification timing, never invents EOF or
//! promotes child exit/output to trusted status. No process-global hooks are used.

use super::*;
use crate::security::sandbox::SandboxLifecycleFailureClass as Class;

/// Real pipe adapter that pauses only after the underlying read returned zero.
/// Bounded channel waits keep a broken synchronization path from hanging tests.
struct PausedEofReader {
    /// Owned read descriptor, configured nonblocking by the production drain.
    pipe: std::fs::File,
    /// Announces actual read(0), before the drain can publish its result.
    observed: mpsc::Sender<()>,
    /// Allows the test to queue cancellation before the reader returns zero.
    release: mpsc::Receiver<()>,
}

impl AsFd for PausedEofReader {
    /// Borrows the real pipe used by the production nonblocking setup.
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.pipe.as_fd()
    }
}

impl Read for PausedEofReader {
    /// Returns only underlying read results; the barrier cannot fabricate EOF.
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.pipe.read(buffer)?;
        if count == 0 {
            let _ = self.observed.send(());
            self.release
                .recv_timeout(Duration::from_secs(5))
                .map_err(std::io::Error::other)?;
        }
        Ok(count)
    }
}

/// Actual EOF must survive cancellation queued after read(0), even when the
/// collector has not consumed the reader's done notification. The same private
/// join/validator used by collect must parse the closed child-only status as
/// MissingExit, preserving uncertain effects rather than misclassifying it as
/// an unclosed transport. This is a deterministic owner-boundary regression,
/// not a timing reproduction of any historical integration failure.
#[test]
fn actual_eof_survives_late_cancel_and_undrained_notification() {
    let (pipe, writer) = status_pipe().unwrap();
    let mut writer = std::fs::File::from(writer);
    writer.write_all(b"{\"child-pid\":1}\n").unwrap();
    drop(writer);
    let (observed_tx, observed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, _done_rx) = mpsc::channel();
    let reader = spawn_output_reader(
        SpawnedChildPipe::Status,
        PausedEofReader {
            pipe: std::fs::File::from(pipe),
            observed: observed_tx,
            release: release_rx,
        },
        SPAWNED_CHILD_STATUS_CAPTURE_LIMIT,
        done_tx,
        None,
    );
    let observed = observed_rx.recv_timeout(Duration::from_secs(5));
    cancel_pending_reader(&reader);
    // Release before any assertions so even failed synchronization can join.
    let _ = release_tx.send(());
    let output = ShellExecutionOutput::new(
        Some(7),
        "untrusted stdout".to_string(),
        "launcher diagnostic".to_string(),
        false,
        false,
    );
    let result = join_and_validate_spawned_child_status(
        reader,
        crate::runtime::SandboxBackend::Bubblewrap,
        &output,
        0,
    );
    assert!(observed.is_ok(), "reader never observed actual EOF");
    let error = result.unwrap_err();
    let evidence = error.sandbox_lifecycle_failure().unwrap();
    assert_eq!(evidence.class, Class::MissingExit);
    assert_eq!(evidence.child_record_present, Some(true));
    assert_eq!(evidence.exit_record_present, Some(false));
    assert_eq!(evidence.outer_exit_code, Some(7));
    assert_eq!(evidence.stderr, "launcher diagnostic");
}
