//! Sole-reaper ownership for Linux real-Bubblewrap test commands.
//!
//! Only this owner observes/reaps its freshly spawned root. WNOWAIT observation
//! retains that root (including its zombie) through the last group/root signal
//! decision. Signal authority is consumed BEFORE actual reap, even when signaling
//! fails. No status-reported PID, process scan, detached reader, or fresh cleanup
//! timeout supplies authority. Capture and stdin are cooperatively nonblocking,
//! quota-limited, and share the deadline established before spawn.
//!
//! This is a test helper, not a production sandbox/process API. Synchronous
//! spawn/syscalls cannot be bounded against arbitrary kernel stalls. At deadline
//! cleanup is best effort: a still-unreaped root is explicitly an error, not a
//! cleanup-success claim. Explicit retirement incorporates secondary errors in
//! the returned result. Drop uses that same idempotent nonblocking retirement,
//! never a sink/lock/logger, background reaper, or fresh budget. On panic, the
//! original panic remains visible but secondary cleanup errors retained only in
//! this owner's memory are lost when it is destroyed; no unwind reporting or
//! complete reap is promised.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{
    Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio,
};
use std::time::{Duration, Instant};

const CAPTURE_LIMIT: usize = 1024 * 1024;
const IO_QUOTA: usize = 8192;
const TICK: Duration = Duration::from_millis(1);
const PAYLOAD_DELAY: Duration = Duration::from_millis(50);

/// Operations available only to the owner; the fake implementation journals
/// ordering without creating or signaling any process.
trait Workload {
    /// Configures acquired pipes; failures occur after the owner is installed.
    fn setup(&mut self) -> io::Result<()>;
    /// Returns the monotonic clock used for the one original absolute deadline.
    fn now(&self) -> Instant;
    /// Yields for at most the remaining original budget.
    fn pause(&mut self, duration: Duration);
    /// Observes exit without reaping. ECHILD revokes all signal authority.
    fn exited(&mut self) -> io::Result<bool>;
    /// Signals only the group created by the still-unreaped root.
    fn signal_group(&mut self) -> io::Result<()>;
    /// Signals the exact still-unreaped root, including group-signal failure.
    fn signal_root(&mut self) -> io::Result<()>;
    /// Actually reaps, nonblocking; legal only after the final signal decision.
    fn reap(&mut self) -> io::Result<Option<ExitStatus>>;
    /// Does one bounded round of I/O; true means capture EOF and stdin closed.
    fn pump(&mut self) -> io::Result<bool>;
}

/// Explicitly separates signal authority from a consumed/reaped/lost root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Authority {
    Armed,
    Finalized,
    Reaped,
    Lost,
}

/// Owns every partial-setup and unwind path, as well as ordinary collection.
struct Owner<B: Workload> {
    backend: B,
    deadline: Instant,
    authority: Authority,
    errors: Vec<String>,
    retired: bool,
}

impl<B: Workload> Owner<B> {
    /// Installs ownership before any fallible pipe setup or stdin delivery.
    fn new(backend: B, deadline: Instant) -> Self {
        Self {
            backend,
            deadline,
            authority: Authority::Armed,
            errors: Vec::new(),
            retired: false,
        }
    }

    /// Records context without replacing the primary failure with cleanup noise.
    fn error(&mut self, context: &str, error: impl std::fmt::Display) {
        self.errors.push(format!("{context}: {error}"));
    }

    /// Consumes the last signal decision before any reaping, including errors.
    fn finalize(&mut self) {
        if self.authority != Authority::Armed {
            return;
        }
        // Setup failure/unwind may reach here before the ordinary observation.
        // An explicit ownership-loss receipt forbids even a numeric fallback.
        if let Err(error) = self.backend.exited() {
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.authority = Authority::Lost;
                self.error("exclusive root ownership lost; refusing signals", error);
                return;
            }
            self.error("cleanup nonreaping observation", error);
        }
        self.authority = Authority::Finalized;
        if let Err(error) = self.backend.signal_group() {
            self.error("group SIGKILL", error);
        }
        if let Err(error) = self.backend.signal_root() {
            self.error("root SIGKILL", error);
        }
    }

    /// Attempts actual reap only after all authorized signals have been decided.
    fn reap(&mut self) -> Option<ExitStatus> {
        if self.authority != Authority::Finalized {
            return None;
        }
        match self.backend.reap() {
            Ok(Some(status)) => {
                self.authority = Authority::Reaped;
                Some(status)
            }
            Ok(None) => None,
            Err(error) => {
                // A failed wait never creates permission to signal again.
                self.authority = Authority::Lost;
                self.error("reap failed; cleanup unconfirmed", error);
                None
            }
        }
    }

    /// Collects under the immutable deadline, retaining all secondary failures.
    fn collect(&mut self) -> Result<ExitStatus, String> {
        let mut io_usable = match self.backend.setup() {
            Ok(()) => true,
            Err(error) => {
                self.error("pipe setup", error);
                self.finalize();
                false
            }
        };
        let mut io_done = false;
        let mut status = None;
        loop {
            if self.backend.now() >= self.deadline {
                self.error(
                    "original deadline exhausted",
                    "capture/reap may be incomplete",
                );
                self.finalize();
                if status.is_none() {
                    status = self.reap();
                }
                break;
            }
            if self.authority == Authority::Armed {
                match self.backend.exited() {
                    Ok(true) => self.finalize(),
                    Ok(false) => {}
                    Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                        self.authority = Authority::Lost;
                        self.error("exclusive root ownership lost; refusing signals", error);
                    }
                    Err(error) => {
                        self.error("nonreaping exit observation", error);
                        self.finalize();
                    }
                }
            }
            if io_usable && !io_done && self.backend.now() < self.deadline {
                match self.backend.pump() {
                    Ok(done) => io_done = done,
                    Err(error) => {
                        self.error("bounded I/O", error);
                        io_usable = false;
                        self.finalize();
                    }
                }
            }
            if status.is_none() {
                status = self.reap();
            }
            if self.authority == Authority::Lost || (status.is_some() && (io_done || !io_usable)) {
                break;
            }
            let remaining = self.deadline.saturating_duration_since(self.backend.now());
            if !remaining.is_zero() {
                self.backend.pause(TICK.min(remaining));
            }
        }
        if status.is_none() && self.authority == Authority::Finalized {
            self.error(
                "root still unreaped",
                "nonblocking cleanup incomplete at deadline",
            );
        }
        self.finish(status)
    }

    /// Retires before constructing a result, retaining secondary cleanup errors
    /// and rejecting success at or beyond the original completion deadline.
    fn finish(&mut self, status: Option<ExitStatus>) -> Result<ExitStatus, String> {
        self.retire();
        if self.backend.now() >= self.deadline
            && !self
                .errors
                .iter()
                .any(|error| error.starts_with("original deadline exhausted"))
        {
            self.error(
                "original deadline exhausted",
                "completion was not within budget",
            );
        }
        if self.errors.is_empty() {
            status.ok_or_else(|| "root status unavailable".to_owned())
        } else {
            Err(self.errors.join("; "))
        }
    }

    /// Makes one final nonblocking reap attempt. No external diagnostic I/O is
    /// possible here, and repeated retirement/Drop is inert even on failure.
    fn retire(&mut self) {
        if self.retired {
            return;
        }
        self.retired = true;
        self.finalize();
        if self.authority == Authority::Finalized && self.reap().is_none() {
            self.error("retirement", "root reap unconfirmed; no new cleanup budget");
        }
    }
}

impl<B: Workload> Drop for Owner<B> {
    /// Handles partial setup and unwinding without blocking or post-reap signals.
    fn drop(&mut self) {
        self.retire();
    }
}

/// Bounded bytes and EOF state for one nonblocking test-owned capture pipe.
#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    eof: bool,
}

impl Capture {
    /// Reads at most one fixed-size chunk, including on Interrupted/WouldBlock.
    fn read(&mut self, reader: &mut impl Read) -> io::Result<()> {
        if self.eof {
            return Ok(());
        }
        let mut chunk = [0; IO_QUOTA];
        match reader.read(&mut chunk) {
            Ok(0) => self.eof = true,
            Ok(count) => {
                let remaining = CAPTURE_LIMIT - self.bytes.len();
                self.bytes.extend_from_slice(&chunk[..count.min(remaining)]);
                if count > remaining {
                    return Err(io::Error::other("capture exceeded 1 MiB per-stream limit"));
                }
            }
            Err(error) if transient(&error) => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }
}

/// Staged wrapper/payload input with bounded writes and the original 50ms gap.
struct Input {
    wrapper: Vec<u8>,
    payload: Vec<u8>,
    offset: usize,
    payload_at: Option<Instant>,
}

impl Input {
    /// Writes one quota and yields on short writes, Interrupted, or WouldBlock.
    fn write(&mut self, writer: &mut impl Write, now: Instant) -> io::Result<bool> {
        let bytes = if let Some(ready) = self.payload_at {
            if now < ready {
                return Ok(false);
            }
            &self.payload
        } else {
            &self.wrapper
        };
        if self.offset < bytes.len() {
            let end = bytes.len().min(self.offset.saturating_add(IO_QUOTA));
            match writer.write(&bytes[self.offset..end]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => self.offset += count,
                Err(error) if transient(&error) => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        if self.offset == bytes.len() {
            if self.payload_at.is_some() {
                return Ok(true);
            }
            self.offset = 0;
            self.payload_at = Some(now + PAYLOAD_DELAY);
        }
        Ok(false)
    }
}

/// Errors that consume this round rather than spinning/restarting the deadline.
fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
    )
}

/// Marks an exclusively owned capture/input descriptor nonblocking.
fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: the borrowed pipe keeps this descriptor alive across both calls.
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: same exclusively owned live descriptor; only O_NONBLOCK is added.
    if unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Kernel adapter. Child ownership is never shared or returned to a caller.
struct LiveWorkload {
    child: Child,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    stdin: Option<ChildStdin>,
    input: Option<Input>,
    out: Capture,
    err: Capture,
}

impl LiveWorkload {
    /// Targets only the birth root/group while its sole reaper retains it.
    fn signal(&self, group: bool) -> io::Result<()> {
        let pid = i32::try_from(self.child.id()).map_err(io::Error::other)?;
        if pid <= 0 {
            return Err(io::Error::other("invalid owned child PID"));
        }
        // SAFETY: Owner calls this only before any reap and only for the exact
        // Child created with process_group(0). No observed status PID is used.
        if unsafe { libc::kill(if group { -pid } else { pid }, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(()) // No live signal recipient; never a reason to retarget.
        } else {
            Err(error)
        }
    }
}

impl Workload for LiveWorkload {
    fn setup(&mut self) -> io::Result<()> {
        self.stdout = self.child.stdout.take();
        self.stderr = self.child.stderr.take();
        self.stdin = self.child.stdin.take();
        nonblocking(
            self.stdout
                .as_ref()
                .ok_or_else(|| io::Error::other("missing stdout"))?,
        )?;
        nonblocking(
            self.stderr
                .as_ref()
                .ok_or_else(|| io::Error::other("missing stderr"))?,
        )?;
        if self.input.is_some() {
            nonblocking(
                self.stdin
                    .as_ref()
                    .ok_or_else(|| io::Error::other("missing stdin"))?,
            )?;
        }
        Ok(())
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn pause(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn exited(&mut self) -> io::Result<bool> {
        // SAFETY: siginfo is an initialized output buffer. P_PID selects only
        // this exclusively owned Child; WNOWAIT explicitly forbids reaping.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            let error = io::Error::last_os_error();
            return if transient(&error) {
                Ok(false)
            } else {
                Err(error)
            };
        }
        // SAFETY: successful waitid initialized the siginfo layout. The PID is
        // used ONLY as a boolean exit observation, never as signal authority.
        Ok(unsafe { info.si_pid() } != 0)
    }

    fn signal_group(&mut self) -> io::Result<()> {
        self.signal(true)
    }

    fn signal_root(&mut self) -> io::Result<()> {
        self.signal(false)
    }

    fn reap(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    fn pump(&mut self) -> io::Result<bool> {
        if let Some(pipe) = &mut self.stdout {
            self.out.read(pipe)?;
        }
        if let Some(pipe) = &mut self.stderr {
            self.err.read(pipe)?;
        }
        if let (Some(input), Some(pipe)) = (&mut self.input, &mut self.stdin)
            && input.write(pipe, Instant::now())?
        {
            self.stdin.take();
            self.input.take();
        }
        Ok(self.out.eof && self.err.eof && self.input.is_none())
    }
}

/// Creates the sole owner with its pre-spawn deadline, before fallible setup.
/// Kernel fixtures reuse this exact path without exposing an unguarded Child.
fn spawn_owner(
    mut command: Command,
    input: Option<(Vec<u8>, Vec<u8>)>,
    timeout: Duration,
) -> Result<Owner<LiveWorkload>, String> {
    let deadline = Instant::now() + timeout;
    command
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let child = command
        .spawn()
        .map_err(|error| format!("spawn workload: {error}"))?;
    Ok(Owner::new(
        LiveWorkload {
            child,
            stdout: None,
            stderr: None,
            stdin: None,
            input: input.map(|(wrapper, payload)| Input {
                wrapper,
                payload,
                offset: 0,
                payload_at: None,
            }),
            out: Capture::default(),
            err: Capture::default(),
        },
        deadline,
    ))
}

/// Spawns and collects one command without exposing an unguarded Child.
pub(super) fn output(
    command: Command,
    input: Option<(Vec<u8>, Vec<u8>)>,
    timeout: Duration,
) -> Result<Output, String> {
    let mut owner = spawn_owner(command, input, timeout)?;
    match owner.collect() {
        Ok(status) => Ok(Output {
            status,
            stdout: std::mem::take(&mut owner.backend.out.bytes),
            stderr: std::mem::take(&mut owner.backend.err.bytes),
        }),
        Err(error) => Err(format!(
            "{error}; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&owner.backend.out.bytes),
            String::from_utf8_lossy(&owner.backend.err.bytes)
        )),
    }
}

#[cfg(test)]
mod journal;

#[cfg(test)]
mod fixture_policy;

#[cfg(test)]
mod kernel;
