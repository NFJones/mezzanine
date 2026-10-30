//! Exact foreground-child custody and explicit retirement for native CLI tests.
//!
//! Setup establishes directory identity and fallible PTY plumbing before spawning
//! a child. Retirement independently observes child, reader and writer settlement;
//! no request or destructor is treated as a successful reap. Drop retains storage
//! when explicit verification was skipped or a cleanup obligation is unsettled.

use super::{output_excerpt, reader, storage, test_root};
use portable_pty::{Child as PtyChild, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

mod child_faults;
mod startup_faults;

/// Owns one foreground fixture from preparation through explicit retirement.
///
/// A missing child is valid only during guarded startup before spawning. Live
/// children are installed immediately on successful spawn, before any later
/// fallible step. Output, writer retirement and root identity remain independent
/// obligations, so one failure cannot authorize deletion before the others settle.
pub(super) struct ForegroundProcess {
    /// Exact child handle, absent only before guarded startup creates a child.
    child: Option<Box<dyn PtyChild + Send + Sync>>,
    /// Stores the master pty value for this data structure.
    ///
    /// The field remains owned by the fixture so tests can resize the
    /// interactive terminal while retaining the cloned reader and writer.
    master: Box<dyn MasterPty + Send>,
    /// Stores the writer value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    writer: Option<Box<dyn Write + Send>>,
    /// Stores the output rx value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) output_rx: Receiver<Vec<u8>>,
    /// Stores the reader thread value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    reader_thread: Option<reader::ReaderTask>,
    /// Open directory generation that must match before removing fixture storage.
    root: storage::OwnedRoot,
    /// Detached worker that owns this fixture's writer destructor, when handed off.
    writer_worker: Option<JoinHandle<()>>,
    /// Sticky writer-retirement obligation this fixture can never claim to have cleared.
    writer_obligation: Option<String>,
    /// Instance-scoped injection: makes this fixture's writer handoff fail.
    fail_writer_handoff: bool,
    /// Instance-scoped injection: parks the next writer worker until released.
    park_writer_worker: bool,
    /// Release handle for a parked writer worker, when one is parked.
    writer_park_release: Option<mpsc::Sender<()>>,
}

/// Polling window for the child, reader and writer worker of one retirement.
///
/// This is not a strict end-to-end deadline: storage removal after the poll loop is
/// ordinary filesystem work, and portable-pty's kill performs its own internally
/// bounded HUP grace loop before SIGKILL.
pub(super) const FIXTURE_RETIREMENT_BOUND: Duration = Duration::from_secs(2);

/// Typed outcome of one explicit foreground-fixture retirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FixtureCleanup {
    /// The exact child was reaped, the reader settled and fixture storage was removed.
    Retired,
    /// Ownership remains unsettled; the message names each retained obligation.
    Unsettled(String),
}

/// One per-instance partial-startup failure; normal constructors supply none.
#[derive(Clone, Copy)]
enum SetupFault {
    /// Fail native reader acquisition before a thread or child exists.
    ReaderClone,
    /// Fail writer acquisition while the fixture already owns its master/root.
    Writer,
    /// Fail worker creation after cloning the reader descriptors.
    ReaderThread,
}

impl ForegroundProcess {
    /// Owns root and PTY custody before acquiring plumbing or spawning a child.
    /// Any failed step attempts the same explicit retirement as normal completion
    /// and returns the primary failure together with its cleanup outcome.
    pub(super) fn spawn(
        pair: portable_pty::PtyPair,
        command: CommandBuilder,
        root: storage::OwnedRoot,
        name: &str,
    ) -> Result<Self, String> {
        Self::spawn_with_fault(pair, command, root, name, None)
    }

    /// Runs the real startup sequence with an optional instance-scoped fault.
    /// A failed fixture never escapes; its native ownership stays in this guard.
    fn spawn_with_fault(
        pair: portable_pty::PtyPair,
        command: CommandBuilder,
        root: storage::OwnedRoot,
        name: &str,
        fault: Option<SetupFault>,
    ) -> Result<Self, String> {
        let (_, output_rx) = mpsc::channel();
        let mut fixture = Self {
            child: None,
            master: pair.master,
            writer: None,
            output_rx,
            reader_thread: None,
            root,
            writer_worker: None,
            writer_obligation: None,
            fail_writer_handoff: false,
            park_writer_worker: false,
            writer_park_release: None,
        };
        let setup = (|| -> Result<(), String> {
            if matches!(fault, Some(SetupFault::Writer)) {
                return Err("fixture writer acquisition failed: injected failure".into());
            }
            fixture.writer = Some(
                fixture
                    .master
                    .take_writer()
                    .map_err(|error| format!("fixture writer acquisition failed: {error}"))?,
            );
            let reader_fault = match fault {
                Some(SetupFault::ReaderClone) => Some(reader::ReaderFault::Clone),
                Some(SetupFault::ReaderThread) => Some(reader::ReaderFault::Thread),
                _ => None,
            };
            let (worker, output) =
                reader::ReaderTask::start_with_fault(fixture.master.as_ref(), name, reader_fault)
                    .map_err(|error| format!("fixture reader setup failed: {error}"))?;
            fixture.reader_thread = Some(worker);
            fixture.output_rx = output;
            fixture.child = Some(
                pair.slave
                    .spawn_command(command)
                    .map_err(|error| format!("fixture child spawn failed: {error}"))?,
            );
            Ok(())
        })();
        drop(pair.slave);
        if let Err(primary) = setup {
            let cleanup = fixture.retire();
            return Err(format!("{primary}; fixture cleanup: {cleanup:?}"));
        }
        Ok(fixture)
    }

    /// Borrows the exact child of a successfully started foreground fixture.
    /// Failed startup fixtures never escape `spawn`, so interactive operations
    /// intentionally require this established invariant.
    pub(super) fn child_mut(&mut self) -> &mut (dyn PtyChild + Send + Sync) {
        self.child
            .as_deref_mut()
            .expect("successfully started fixture owns its child")
    }

    /// Retires this fixture explicitly and reports the exact outcome.
    ///
    /// `Drop` cannot express this contract: it may not block for a bounded time,
    /// it cannot report, and it must not delete storage it cannot prove is
    /// settled. Retirement drops the fixture's own writer handle, polls the exact
    /// child against a deadline and escalates once (a kill request is not a reap),
    /// observes the reader thread against the same deadline, and removes only the
    /// storage this fixture created once both are settled. A second call is inert.
    /// The fixture writer's destructor is handed to its own worker; retirement waits
    /// for that worker inside the same deadline and reports a handoff that never
    /// started or a worker still blocked, so no cleanup obligation is left unobserved.
    pub(super) fn retire(&mut self) -> FixtureCleanup {
        // Retiring clears the owned root, so a second call is inert.
        if self.root.is_retired() {
            return FixtureCleanup::Retired;
        }
        let deadline = Instant::now() + FIXTURE_RETIREMENT_BOUND;
        if let Some(reader) = &self.reader_thread {
            reader.cancel();
        }
        let _ = self.retire_writer_off_thread();
        let mut unsettled = self
            .writer_obligation
            .clone()
            .into_iter()
            .collect::<Vec<_>>();
        let mut reaped = false;
        match self
            .child
            .as_mut()
            .map(|child| child.try_wait())
            .transpose()
        {
            Ok(None | Some(Some(_))) => reaped = true,
            Ok(Some(None)) => {
                if let Err(error) = self.child_mut().kill() {
                    unsettled.push(format!("child termination request failed: {error}"));
                }
                while Instant::now() < deadline {
                    match self.child_mut().try_wait() {
                        Ok(Some(_)) => {
                            reaped = true;
                            break;
                        }
                        Ok(None) => thread::sleep(Duration::from_millis(5)),
                        Err(error) => {
                            unsettled.push(format!("child reap observation failed: {error}"));
                            break;
                        }
                    }
                }
            }
            Err(error) => unsettled.push(format!("child state observation failed: {error}")),
        }
        let reader_settled = match self.reader_thread.as_ref() {
            None => true,
            Some(handle) => {
                while Instant::now() < deadline && !handle.is_finished() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                handle.is_finished()
            }
        };
        if reader_settled
            && let Some(handle) = self.reader_thread.as_mut()
            && let Err(error) = handle.join_finished()
        {
            unsettled.push(error);
        }
        let writer_settled = self.writer_worker_settled_within(deadline);
        if let Some(reason) = &self.writer_obligation
            && !unsettled.contains(reason)
        {
            unsettled.push(reason.clone());
        }
        if reaped && reader_settled && writer_settled && unsettled.is_empty() {
            return self.remove_owned_storage();
        }
        if !reaped {
            unsettled.push("child was not reaped within the retirement bound".to_string());
        }
        if !reader_settled {
            unsettled.push("reader still owned a live PTY clone".to_string());
        }
        // A worker that is merely still running stays retryable: a later retirement
        // that observes its completion discharges the obligation instead of making it
        // permanent, unlike a leaked handoff slot.
        if !writer_settled && self.writer_obligation.is_none() {
            unsettled
                .push("writer retirement worker was still blocked in its destructor".to_string());
        }
        FixtureCleanup::Unsettled(unsettled.join("; "))
    }

    /// Waits inside the retirement deadline for the writer worker to finish.
    fn writer_worker_settled_within(&mut self, deadline: Instant) -> bool {
        let Some(handle) = self.writer_worker.as_ref() else {
            return true;
        };
        while Instant::now() < deadline && !handle.is_finished() {
            std::thread::sleep(Duration::from_millis(5));
        }
        if handle.is_finished()
            && let Some(handle) = self.writer_worker.take()
        {
            if handle.join().is_err() {
                self.writer_obligation = Some("writer retirement worker panicked".to_string());
                return false;
            }
            return true;
        }
        false
    }

    /// Retires the fixture writer without letting its destructor block this thread.
    ///
    /// The portable-pty Unix writer destructor performs a VEOF write on the master
    /// descriptor, which can block when the PTY input path is saturated or a
    /// descendant holds the slave without reading. Moving that destructor to a
    /// detached thread keeps the calling thread out of that write; retirement then
    /// observes the worker inside its polling window, and the worker owns only this
    /// one handle.
    /// Returns a retained-obligation reason when the handoff itself could not start.
    ///
    /// Ownership is held in a shared slot that the worker only takes after it starts,
    /// so a failed worker spawn cannot run the blocking destructor on this thread: the
    /// slot still owns the writer and is deliberately leaked (reported as unsettled)
    /// rather than dropped here or silently forgotten.
    fn retire_writer_off_thread(&mut self) -> Option<String> {
        let writer = self.writer.take()?;
        let shared = Arc::new(Mutex::new(Some(writer)));
        if std::mem::take(&mut self.fail_writer_handoff) {
            std::mem::forget(shared);
            let reason = "writer handoff was unavailable; its destructor was retained";
            self.writer_obligation = Some(reason.to_string());
            return Some(reason.to_string());
        }
        let worker = Arc::clone(&shared);
        let park = std::mem::take(&mut self.park_writer_worker).then(|| {
            let (release, parked) = mpsc::channel();
            self.writer_park_release = Some(release);
            parked
        });
        match thread::Builder::new()
            .name("foreground-writer-retire".to_string())
            .spawn(move || {
                if let Some(parked) = park {
                    let _ = parked.recv();
                }
                let writer = worker.lock().ok().and_then(|mut slot| slot.take());
                drop(writer);
            }) {
            Ok(handle) => {
                self.writer_worker = Some(handle);
                None
            }
            Err(_) => {
                // The worker never started, so the slot still owns the writer. Leak it
                // instead of dropping the blocking destructor on the cleanup thread.
                std::mem::forget(shared);
                let reason = "writer handoff could not start; its destructor was retained";
                self.writer_obligation = Some(reason.to_string());
                Some(reason.to_string())
            }
        }
    }

    /// Removes only the storage this fixture created, reporting failures truthfully.
    fn remove_owned_storage(&mut self) -> FixtureCleanup {
        match self.root.retire() {
            Ok(()) => FixtureCleanup::Retired,
            Err(error) => {
                FixtureCleanup::Unsettled(format!("fixture storage removal failed: {error}"))
            }
        }
    }

    /// Runs the read until operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn read_until<F>(
        &mut self,
        output: &mut Vec<u8>,
        timeout: Duration,
        predicate: F,
    ) -> Result<(), String>
    where
        F: Fn(&str) -> bool,
    {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = self
                .child_mut()
                .try_wait()
                .map_err(|error| format!("failed to poll foreground child: {error}"))?
            {
                return Err(format!(
                    "foreground child exited before expected output: status={status:?} output={}",
                    output_excerpt(output)
                ));
            }

            match self.output_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(chunk) => {
                    output.extend_from_slice(&chunk);
                    let text = String::from_utf8_lossy(output);
                    if predicate(&text) {
                        return Ok(());
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!(
                        "foreground output reader closed before expected output: output={}",
                        output_excerpt(output)
                    ));
                }
            }
        }

        Err(format!(
            "timed out waiting for foreground output: output={}",
            output_excerpt(output)
        ))
    }

    /// Runs the send interrupt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn send_interrupt(&mut self) -> Result<(), String> {
        let process_id = self
            .child_mut()
            .process_id()
            .ok_or_else(|| "foreground child has no process id".to_string())?;
        let raw_pid = i32::try_from(process_id)
            .map_err(|_| format!("foreground child process id {process_id} is too large"))?;
        let pid = rustix::process::Pid::from_raw(raw_pid)
            .ok_or_else(|| format!("foreground child process id {process_id} is invalid"))?;
        rustix::process::kill_process(pid, rustix::process::Signal::INT)
            .map_err(|error| format!("failed to send SIGINT to foreground child: {error}"))
    }

    /// Runs the write input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn write_input(&mut self, input: &[u8]) -> Result<(), String> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| "foreground input writer is closed".to_string())?;
        writer
            .write_all(input)
            .map_err(|error| format!("failed to write foreground input: {error}"))?;
        writer
            .flush()
            .map_err(|error| format!("failed to flush foreground input: {error}"))
    }

    /// Runs the resize pty operation for this subsystem.
    ///
    /// The function keeps terminal geometry changes in the foreground fixture
    /// so end-to-end attach regressions can verify resize-sensitive full-screen
    /// behavior without depending on host terminal state.
    pub(super) fn resize_pty(&mut self, size: PtySize) -> Result<(), String> {
        self.master
            .resize(size)
            .map_err(|error| format!("failed to resize foreground pty: {error}"))
    }

    /// Runs the wait for exit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn wait_for_exit(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self
                .child_mut()
                .try_wait()
                .map_err(|error| format!("failed to poll foreground child: {error}"))?
                .is_some()
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Err("foreground child did not exit after SIGINT".to_string())
    }

    /// Runs the read until exit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn read_until_exit(
        &mut self,
        output: &mut Vec<u8>,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut reader_disconnected = false;
        while Instant::now() < deadline {
            if !reader_disconnected {
                while let Ok(chunk) = self.output_rx.try_recv() {
                    output.extend_from_slice(&chunk);
                }
            }
            if self
                .child_mut()
                .try_wait()
                .map_err(|error| format!("failed to poll foreground child: {error}"))?
                .is_some()
            {
                while let Ok(chunk) = self.output_rx.recv_timeout(Duration::from_millis(20)) {
                    output.extend_from_slice(&chunk);
                }
                return Ok(());
            }
            if reader_disconnected {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            match self.output_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(chunk) => output.extend_from_slice(&chunk),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    if self
                        .child_mut()
                        .try_wait()
                        .map_err(|error| format!("failed to poll foreground child: {error}"))?
                        .is_some()
                    {
                        return Ok(());
                    }
                    reader_disconnected = true;
                }
            }
        }
        Err(format!(
            "foreground child did not exit before timeout: output={}",
            output_excerpt(output)
        ))
    }
}

impl Drop for ForegroundProcess {
    /// Best-effort fallback for a fixture that never retired explicitly.
    ///
    /// This performs no unbounded child or reader wait: it hands the writer
    /// destructor to its own worker, requests termination of a still-running direct
    /// child through portable-pty (whose kill performs an internally bounded HUP grace
    /// loop before SIGKILL), and joins the reader only when it has already finished.
    /// It deletes no storage, because recursive filesystem work has no wall-clock
    /// bound: a fixture that never retires explicitly keeps its exact storage as
    /// visible evidence. Tests are expected to call `retire` so the outcome is
    /// asserted instead of relying on this fallback.
    fn drop(&mut self) {
        let _ = self.retire_writer_off_thread();
        if let Some(child) = self.child.as_mut()
            && child.try_wait().ok().flatten().is_none()
        {
            let _ = child.kill();
        }
        let reader_settled = self
            .reader_thread
            .as_ref()
            .is_none_or(reader::ReaderTask::is_finished);
        if let Some(reader) = &self.reader_thread {
            reader.cancel();
        }
        if reader_settled && let Some(handle) = self.reader_thread.as_mut() {
            let _ = handle.join_finished();
        }
    }
}

/// Builds one privately owned foreground PTY fixture around a shell command.
///
/// The slave handle is returned so a test can deliberately keep the PTY open and
/// exercise the unsettled-reader path; production callers drop it immediately.
fn retirement_fixture(
    root: &Path,
    command: &str,
) -> (ForegroundProcess, Box<dyn portable_pty::SlavePty + Send>) {
    retirement_fixture_with_child(root, command, |child| child)
}

/// Builds the same native fixture with an infallible child-observation adapter.
/// The adapter owns the exact new child; it cannot spawn another process.
fn retirement_fixture_with_child(
    root: &Path,
    command: &str,
    wrap: impl FnOnce(Box<dyn PtyChild + Send + Sync>) -> Box<dyn PtyChild + Send + Sync>,
) -> (ForegroundProcess, Box<dyn portable_pty::SlavePty + Send>) {
    // Own the fixture directory before any live resource exists.
    fs::create_dir_all(root).unwrap();
    let owned_root = storage::OwnedRoot::capture(root).unwrap();
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut builder = CommandBuilder::new("/bin/sh");
    builder.args(["-c", command]);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = pair.master.take_writer().unwrap();
    let (output_tx, output_rx) = mpsc::channel();
    let reader_thread = thread::Builder::new()
        .name("opaque-retirement-control".into())
        .spawn(move || {
            let mut bytes = [0; 256];
            while let Ok(read) = reader.read(&mut bytes) {
                if read == 0 || output_tx.send(bytes[..read].to_vec()).is_err() {
                    break;
                }
            }
        })
        .unwrap();
    let mut reader_thread = reader::ReaderTask::opaque_fixture(reader_thread);
    let child = match pair.slave.spawn_command(builder) {
        Ok(child) => child,
        Err(error) => {
            drop(pair.slave);
            let cleanup = reader_thread.finish(Instant::now() + FIXTURE_RETIREMENT_BOUND);
            panic!(
                "synthetic retirement fixture spawn failed: {error}; reader cleanup: {cleanup:?}"
            );
        }
    };
    (
        ForegroundProcess {
            child: Some(wrap(child)),
            master: pair.master,
            writer: Some(writer),
            output_rx,
            reader_thread: Some(reader_thread),
            root: owned_root,
            writer_worker: None,
            writer_obligation: None,
            fail_writer_handoff: false,
            park_writer_worker: false,
            writer_park_release: None,
        },
        pair.slave,
    )
}

/// An unsettled reader is reported and its storage is retained.
///
/// A retained slave keeps the PTY open after the direct child exits, so the fixture
/// reader cannot observe EOF. Retirement must name that exact obligation and must
/// not remove fixture storage it cannot yet prove settled.
#[test]
fn foreground_fixture_retirement_reports_unsettled_reader() {
    let root = test_root("retirement-unsettled-reader");
    let (mut fixture, held_slave) = retirement_fixture(&root, "exit 0");
    assert_eq!(
        fixture.retire(),
        FixtureCleanup::Unsettled("reader still owned a live PTY clone".to_string())
    );
    assert!(
        root.exists(),
        "unsettled retirement must retain its storage"
    );
    drop(held_slave);
    assert_eq!(fixture.retire(), FixtureCleanup::Retired);
    assert!(!root.exists());
}

/// Retirement is bounded, reaps a live child and never deletes released storage.
#[test]
fn foreground_fixture_retirement_reaps_live_child_and_is_inert_when_repeated() {
    let root = test_root("retirement-live-child");
    // `exec` keeps exactly one owned process so the control does not depend on shell
    // forking behaviour or on how the OS delivers a hangup to descendants.
    let (mut fixture, slave) = retirement_fixture(&root, "exec sleep 30");
    drop(slave);
    // Prove the control entered the live-child path before retirement escalates.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        fixture.child_mut().try_wait().unwrap().is_none(),
        "the control must retire a live child"
    );
    let started = Instant::now();
    assert_eq!(fixture.retire(), FixtureCleanup::Retired);
    assert!(
        started.elapsed() < FIXTURE_RETIREMENT_BOUND + Duration::from_secs(1),
        "live-child retirement exceeded its bounded window"
    );
    assert!(!root.exists(), "retired fixtures remove their own storage");

    // Ownership was released, so a later occupant of the same path survives.
    fs::create_dir_all(&root).unwrap();
    let marker = root.join("later-owner");
    fs::write(&marker, b"not this fixture's storage").unwrap();
    assert_eq!(fixture.retire(), FixtureCleanup::Retired);
    assert!(
        marker.exists(),
        "released retirement must not delete later storage"
    );
    let _ = fs::remove_dir_all(&root);
}

/// A storage-removal failure is reported instead of claimed as clean retirement.
#[test]
fn foreground_fixture_retirement_reports_storage_removal_failure() {
    let root = test_root("retirement-removal-failure");
    let (mut fixture, slave) = retirement_fixture(&root, "exit 0");
    drop(slave);
    // Replace the owned directory with a regular file: removal must fail.
    fs::remove_dir_all(&root).unwrap();
    fs::write(&root, b"occupied by a later non-directory").unwrap();
    let outcome = fixture.retire();
    match &outcome {
        FixtureCleanup::Unsettled(reason) => assert!(
            reason.contains("fixture storage removal failed"),
            "unexpected reason: {reason}"
        ),
        other => panic!("removal failure must not report clean retirement: {other:?}"),
    }
    // Ownership is retained so a caller can retry after fixing the obstruction.
    assert!(root.is_file());
    fs::remove_file(&root).unwrap();
    assert_eq!(fixture.retire(), FixtureCleanup::Retired);
}

/// A worker that was merely still running is discharged by a later retirement.
///
/// A worker timeout is retryable state, not permanent uncertainty: once its
/// obstruction is released, the next retirement observes its completion, joins it
/// and reports clean retirement with the storage removed.
#[test]
fn foreground_fixture_retirement_discharges_a_completed_writer_worker() {
    let root = test_root("retirement-parked-writer");
    let (mut fixture, slave) = retirement_fixture(&root, "exit 0");
    drop(slave);
    fixture.park_writer_worker = true;
    match fixture.retire() {
        FixtureCleanup::Unsettled(reason) => assert!(
            reason.contains("writer retirement worker was still blocked"),
            "unexpected reason: {reason}"
        ),
        other => panic!("a parked worker must be reported: {other:?}"),
    }
    assert!(root.exists(), "pending obligation keeps its storage");
    // Release the parked worker; its observed completion must discharge it.
    fixture.writer_park_release = None;
    let deadline = Instant::now() + FIXTURE_RETIREMENT_BOUND + Duration::from_secs(1);
    loop {
        match fixture.retire() {
            FixtureCleanup::Retired => break,
            FixtureCleanup::Unsettled(reason) => {
                assert!(Instant::now() < deadline, "worker never settled: {reason}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    assert!(
        !root.exists(),
        "a discharged retirement removes its storage"
    );
}

/// A writer handoff that never starts is reported, never dropped inline.
///
/// Thread-creation failure is the case where the blocking portable-pty destructor
/// must not run on the cleanup thread: the slot keeps the writer and the fixture
/// reports the retained obligation instead of claiming clean retirement.
#[test]
fn foreground_fixture_retirement_reports_failed_writer_handoff() {
    let root = test_root("retirement-writer-handoff");
    let (mut fixture, slave) = retirement_fixture(&root, "exit 0");
    drop(slave);
    // The seam is instance-scoped, so parallel fixtures cannot consume it.
    fixture.fail_writer_handoff = true;
    let started = Instant::now();
    let outcome = fixture.retire();
    assert!(
        started.elapsed() < FIXTURE_RETIREMENT_BOUND + Duration::from_secs(1),
        "a failed handoff must not block the cleanup thread"
    );
    match &outcome {
        FixtureCleanup::Unsettled(reason) => assert!(
            reason.contains("handoff was unavailable"),
            "unexpected reason: {reason}"
        ),
        other => panic!("a failed handoff must not report clean retirement: {other:?}"),
    }
    assert!(root.exists(), "retained obligation keeps its storage");
    // The obligation is sticky: a later retirement can never become clean.
    match fixture.retire() {
        FixtureCleanup::Unsettled(reason) => assert!(
            reason.contains("handoff was unavailable"),
            "unexpected second reason: {reason}"
        ),
        other => panic!("a retained handoff obligation must stay visible: {other:?}"),
    }
    assert!(root.exists(), "retained obligation still keeps its storage");
    // The deliberately retained writer obligation also retains its storage.
}

/// Directory replacement cannot turn native quiescence into permission to delete
/// another owner's storage. Restoring the captured generation permits retirement;
/// the replacement's marker must survive both the failed attempt and that retry.
#[test]
fn foreground_fixture_retirement_preserves_replaced_root_generation() {
    let root = test_root("retirement-directory-generation");
    let previous = root.with_extension("previous");
    let replacement = root.with_extension("replacement");
    let (mut fixture, slave) = retirement_fixture(&root, "exit 0");
    drop(slave);
    fs::rename(&root, &previous).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("other-owner"), b"retained").unwrap();
    assert!(
        matches!(fixture.retire(), FixtureCleanup::Unsettled(reason) if reason.contains("generation changed"))
    );
    assert_eq!(fs::read(root.join("other-owner")).unwrap(), b"retained");
    fs::rename(&root, &replacement).unwrap();
    // The original still exists elsewhere: absence is not proof of deletion.
    assert!(matches!(fixture.retire(), FixtureCleanup::Unsettled(_)));
    fs::rename(&previous, &root).unwrap();
    assert_eq!(fixture.retire(), FixtureCleanup::Retired);
    assert_eq!(
        fs::read(replacement.join("other-owner")).unwrap(),
        b"retained"
    );
    fs::remove_dir_all(replacement).unwrap();
}
