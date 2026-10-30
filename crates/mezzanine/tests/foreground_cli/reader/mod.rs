//! Owns one foreground fixture's PTY output reader and cancellation request.
//!
//! Native readers poll an owned duplicate of the master descriptor before reading.
//! No descriptor flags are changed: the writer shares its open file description.
//! Only this worker consumes output, so another reader cannot drain readiness
//! between poll and read. Cancellation releases a quiet reader even while a slave
//! remains open. Cleanup observes completion before joining; uncertainty retains
//! the obligation rather than claiming that a cancellation request joined it.

use portable_pty::MasterPty;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Instance-local failure at a fallible reader setup boundary.
#[derive(Clone, Copy)]
pub(super) enum ReaderFault {
    /// Reader cloning fails before any worker exists.
    Clone,
    /// Thread creation fails after the cloned descriptors have been acquired.
    Thread,
}

/// Reader ownership, with a sticky completion result across cleanup retries.
pub(super) struct ReaderTask {
    /// Instance-local cancellation; absent only for deliberately opaque test workers.
    stop: Option<Arc<AtomicBool>>,
    /// Exact worker, retained until its completion is observed.
    worker: Option<JoinHandle<()>>,
    /// Joining a panicked worker must never become success on the next attempt.
    completion: Option<Result<(), String>>,
}

impl ReaderTask {
    /// Starts a cancellable native reader before any frontend child is spawned.
    ///
    /// Descriptor cloning and thread creation are fallible. The owned poll
    /// descriptor prevents a closed parent descriptor being reused underneath the
    /// worker. The native master and cloned reader refer to the same PTY.
    pub(super) fn start(
        master: &dyn MasterPty,
        name: &str,
    ) -> io::Result<(Self, Receiver<Vec<u8>>)> {
        Self::start_with_fault(master, name, None)
    }

    /// Exercises the same setup with one deterministic per-instance failure.
    /// No resource exhaustion, global hooks, or unrelated threads are involved.
    pub(super) fn start_with_fault(
        master: &dyn MasterPty,
        name: &str,
        fault: Option<ReaderFault>,
    ) -> io::Result<(Self, Receiver<Vec<u8>>)> {
        if matches!(fault, Some(ReaderFault::Clone)) {
            return Err(io::Error::other("injected reader clone failure"));
        }
        let mut reader = master.try_clone_reader().map_err(io::Error::other)?;
        let raw = master
            .as_raw_fd()
            .ok_or_else(|| io::Error::other("native PTY has no descriptor"))?;
        // SAFETY: master owns this descriptor throughout the immediate clone.
        let readiness = unsafe { BorrowedFd::borrow_raw(raw) }.try_clone_to_owned()?;
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&stop);
        let (tx, rx) = mpsc::channel();
        if matches!(fault, Some(ReaderFault::Thread)) {
            return Err(io::Error::other("injected reader thread creation failure"));
        }
        let worker = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let mut bytes = [0; 4096];
                while !cancelled.load(Ordering::Acquire) {
                    let mut descriptor = libc::pollfd {
                        fd: readiness.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: descriptor is one initialized pollfd and readiness owns
                    // its descriptor for the entire call. The timeout is finite.
                    let ready = unsafe { libc::poll(&mut descriptor, 1, 20) };
                    if ready < 0 {
                        if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        break;
                    }
                    if ready == 0 || cancelled.load(Ordering::Acquire) {
                        continue;
                    }
                    match reader.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(count) if tx.send(bytes[..count].to_vec()).is_err() => break,
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            })?;
        Ok((
            Self {
                stop: Some(stop),
                worker: Some(worker),
                completion: None,
            },
            rx,
        ))
    }

    /// Adopts a deliberately noncooperative synthetic worker for negative controls.
    pub(super) fn opaque_fixture(worker: JoinHandle<()>) -> Self {
        Self {
            stop: None,
            worker: Some(worker),
            completion: None,
        }
    }

    /// Requests cancellation of only this fixture's native output reader.
    pub(super) fn cancel(&self) {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::Release);
        }
    }

    /// Reports observed worker completion, without blocking.
    pub(super) fn is_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Joins only an already finished worker and retains any panic as an error.
    pub(super) fn join_finished(&mut self) -> Result<(), String> {
        if !self.is_finished() {
            return Err("reader still owned a live PTY clone".to_string());
        }
        if let Some(worker) = self.worker.take() {
            self.completion = Some(
                worker
                    .join()
                    .map_err(|_| "reader worker panicked".to_string()),
            );
        }
        self.completion.clone().unwrap_or(Ok(()))
    }

    /// Cancels and observes completion within the caller's polling window.
    pub(super) fn finish(&mut self, deadline: Instant) -> Result<(), String> {
        self.cancel();
        while Instant::now() < deadline && !self.is_finished() {
            thread::sleep(Duration::from_millis(2));
        }
        self.join_finished()
    }
}

impl Drop for ReaderTask {
    /// Requests cancellation without performing an unbounded join during unwinding.
    fn drop(&mut self) {
        self.cancel();
        if self.is_finished() {
            let _ = self.join_finished();
        }
    }
}

/// A quiet slave must not prevent cancellation, and cancelling one reader must
/// leave an independent PTY usable. Both slaves remain open through cancellation;
/// the second reader proves that no shared stop flag or unrelated descriptor was
/// closed. All descriptors are generated by these fixtures, with no child process.
#[test]
fn quiet_reader_cancels_without_retiring_an_independent_reader() {
    use portable_pty::{PtySize, native_pty_system};
    use std::fs::OpenOptions;
    use std::io::Write;

    let first = native_pty_system().openpty(PtySize::default()).unwrap();
    let second = native_pty_system().openpty(PtySize::default()).unwrap();
    let (mut first_reader, first_output) =
        ReaderTask::start(first.master.as_ref(), "quiet-first").unwrap();
    let (mut second_reader, output) =
        ReaderTask::start(second.master.as_ref(), "quiet-second").unwrap();
    let mut first_writer = OpenOptions::new()
        .write(true)
        .open(first.master.tty_name().unwrap())
        .unwrap();
    first_writer.write_all(b"ready").unwrap();
    let first_deadline = Instant::now() + Duration::from_secs(2);
    let mut first_bytes = Vec::new();
    while first_bytes.len() < b"ready".len() {
        first_bytes.extend(
            first_output
                .recv_timeout(first_deadline.saturating_duration_since(Instant::now()))
                .unwrap(),
        );
    }
    assert_eq!(first_bytes, b"ready");
    first_reader
        .finish(Instant::now() + Duration::from_secs(2))
        .unwrap();
    let mut writer = OpenOptions::new()
        .write(true)
        .open(second.master.tty_name().unwrap())
        .unwrap();
    writer.write_all(b"independent-reader").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bytes = Vec::new();
    while bytes.len() < b"independent-reader".len() {
        bytes.extend(
            output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap(),
        );
    }
    assert_eq!(bytes, b"independent-reader");
    assert!(first.master.get_size().is_ok());
    second_reader
        .finish(Instant::now() + Duration::from_secs(2))
        .unwrap();
    assert!(second.master.get_size().is_ok());
}

/// An observed worker panic remains a cleanup failure on every retry. The panic
/// payload is deliberately omitted from diagnostics, so fixture errors cannot
/// expose arbitrary output or be misreported as successful quiescence later.
#[test]
fn reader_panic_is_a_sticky_cleanup_failure() {
    let mut task = ReaderTask::opaque_fixture(thread::spawn(|| panic!("private fixture payload")));
    assert_eq!(
        task.finish(Instant::now() + Duration::from_secs(2)),
        Err("reader worker panicked".into())
    );
    assert_eq!(task.join_finished(), Err("reader worker panicked".into()));
}
