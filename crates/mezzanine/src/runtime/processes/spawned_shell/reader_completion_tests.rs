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
    )
    .unwrap();
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

/// Failure injected only after a real read confirms the inherited writer is
/// still open and all fixture bytes have already passed through the drain.
#[derive(Clone, Copy, Debug)]
enum UnclosedStop {
    /// Queue the existing collector cancellation request.
    Cancel,
    /// Drop the actual cancellation sender, not the done notification sender.
    Disconnect,
    /// Return a read error instead of the observed WouldBlock.
    ReadError,
    /// Panic in the reader instead of returning the observed WouldBlock.
    Panic,
}

/// Real-pipe adapter that pauses at WouldBlock, after retained fixture bytes.
struct PausedUnclosedReader {
    /// Owned descriptor; the test holds its writer until validation finishes.
    pipe: std::fs::File,
    /// Counts only bytes actually read from the pipe.
    bytes_read: usize,
    /// Announces actual WouldBlock and the number of preceding real bytes.
    observed: mpsc::Sender<usize>,
    /// Releases the read after cancellation/disconnect has been selected.
    release: mpsc::Receiver<()>,
    /// Test-only terminal behavior, never a synthetic successful EOF.
    stop: UnclosedStop,
}

impl AsFd for PausedUnclosedReader {
    /// Borrows the actual pipe for production descriptor configuration.
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.pipe.as_fd()
    }
}

impl Read for PausedUnclosedReader {
    /// Retains real read results until actual WouldBlock; injected failures
    /// cannot generate zero or a favorable status record.
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self.pipe.read(buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let _ = self.observed.send(self.bytes_read);
                self.release
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(std::io::Error::other)?;
                match self.stop {
                    UnclosedStop::Cancel | UnclosedStop::Disconnect => Err(error),
                    UnclosedStop::ReadError => Err(std::io::Error::other("fixture read failure")),
                    UnclosedStop::Panic => panic!("fixture reader panic after actual WouldBlock"),
                }
            }
            result => {
                if let Ok(count) = result {
                    self.bytes_read += count;
                }
                result
            }
        }
    }
}

/// Retained child records and a done notice cannot establish closure. Each
/// case reads the same child-only payload as the positive EOF regression, but
/// retains an inherited writer and proves actual WouldBlock before stopping.
/// Cancellation and sender disconnect exercise the real drain branches; read
/// failure and panic exercise the same joined error mapping used by collect.
/// Even consuming a real done notice must leave record presence unknown.
#[test]
fn unclosed_status_remains_transport_after_cancel_disconnect_error_or_panic() {
    for stop in [
        UnclosedStop::Cancel,
        UnclosedStop::Disconnect,
        UnclosedStop::ReadError,
        UnclosedStop::Panic,
    ] {
        let payload = b"{\"child-pid\":1}\n";
        let (pipe, writer) = status_pipe().unwrap();
        let mut writer = std::fs::File::from(writer);
        writer.write_all(payload).unwrap();
        let (observed_tx, observed_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let mut reader = spawn_output_reader(
            SpawnedChildPipe::Status,
            PausedUnclosedReader {
                pipe: std::fs::File::from(pipe),
                bytes_read: 0,
                observed: observed_tx,
                release: release_rx,
                stop,
            },
            SPAWNED_CHILD_STATUS_CAPTURE_LIMIT,
            done_tx,
            None,
        )
        .unwrap();
        let observed = observed_rx.recv_timeout(Duration::from_secs(5));
        // Keep the replacement channel alive, but disconnect the actual
        // cancellation channel held by the production drain in this case.
        let (replacement, _unused_rx) = mpsc::channel();
        match stop {
            UnclosedStop::Cancel => cancel_pending_reader(&reader),
            UnclosedStop::Disconnect => drop(std::mem::replace(&mut reader.cancel, replacement)),
            UnclosedStop::ReadError | UnclosedStop::Panic => {}
        }
        let _ = release_tx.send(());
        let notice = done_rx.recv_timeout(Duration::from_secs(5));
        // This is receipt of a real scheduling notice, not invented EOF proof.
        reader.completed = notice == Ok(SpawnedChildPipe::Status);
        let output = ShellExecutionOutput::new(
            Some(7),
            "{\"exit-code\":7}".to_string(),
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
        // EOF is impossible until after the joined owner has returned.
        drop(writer);
        assert_eq!(observed.unwrap(), payload.len(), "{stop:?}");
        if matches!(stop, UnclosedStop::Panic) {
            assert_eq!(notice, Err(mpsc::RecvTimeoutError::Disconnected));
        } else {
            assert_eq!(notice, Ok(SpawnedChildPipe::Status), "{stop:?}");
        }
        let error = result.unwrap_err();
        let evidence = error.sandbox_lifecycle_failure().unwrap();
        assert_eq!(evidence.class, Class::Transport, "{stop:?}");
        assert_eq!(evidence.child_record_present, None, "{stop:?}");
        assert_eq!(evidence.exit_record_present, None, "{stop:?}");
        assert_eq!(evidence.outer_exit_code, Some(7));
        assert_eq!(evidence.stderr, "launcher diagnostic");
    }
}

/// Cancellation already queued before the first read and loss of its sender
/// must remain distinct non-EOF outcomes. Keeping the actual writer open makes
/// these top-of-loop paths independent of pipe scheduling or child exit.
#[test]
fn pre_read_cancellation_and_disconnect_return_non_eof_reasons() {
    for disconnected in [false, true] {
        let (pipe, writer) = status_pipe().unwrap();
        let (cancel, cancellation) = mpsc::channel();
        if disconnected {
            drop(cancel);
        } else {
            cancel.send(()).unwrap();
        }
        let capture = drain_output_stream(
            SpawnedChildPipe::Status,
            std::fs::File::from(pipe),
            SPAWNED_CHILD_STATUS_CAPTURE_LIMIT,
            cancellation,
            None,
        )
        .unwrap();
        drop(writer);
        assert_eq!(
            capture.termination,
            if disconnected {
                PipeReadTermination::CancellationDisconnected
            } else {
                PipeReadTermination::Cancelled
            }
        );
        assert!(capture.bytes.is_empty());
        assert_eq!(capture.dropped, 0);
    }
}

/// EOF proves closure, not acceptable capture size. Real reads past the 64 KiB
/// status cap must retain the exact cap, count dropped bytes, and still reject
/// even parseable-looking status as Truncated with unknown record presence.
#[test]
fn actual_eof_preserves_status_cap_and_truncation_rejection() {
    let (pipe, writer) = status_pipe().unwrap();
    let (done_tx, _done_rx) = mpsc::channel();
    let reader = spawn_output_reader(
        SpawnedChildPipe::Status,
        std::fs::File::from(pipe),
        SPAWNED_CHILD_STATUS_CAPTURE_LIMIT,
        done_tx,
        None,
    )
    .unwrap();
    let mut writer = std::fs::File::from(writer);
    let payload = vec![b' '; SPAWNED_CHILD_STATUS_CAPTURE_LIMIT + 37];
    writer.write_all(&payload).unwrap();
    drop(writer);
    let capture = join_output_reader(reader).unwrap();
    assert_eq!(capture.termination, PipeReadTermination::Eof);
    assert_eq!(capture.bytes.len(), SPAWNED_CHILD_STATUS_CAPTURE_LIMIT);
    assert_eq!(capture.dropped, 37);
    let output = ShellExecutionOutput::new(Some(0), String::new(), String::new(), false, false);
    let error = validate_spawned_child_status(
        crate::runtime::SandboxBackend::Bubblewrap,
        &capture.bytes,
        capture.dropped,
        capture.termination == PipeReadTermination::Eof,
        &output,
        0,
    )
    .unwrap_err();
    let evidence = error.sandbox_lifecycle_failure().unwrap();
    assert_eq!(evidence.class, Class::Truncated);
    assert_eq!(evidence.child_record_present, None);
}
