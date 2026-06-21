//! Runtime handle for a spawned pane process.
//!
//! The handle owns the PTY master, tracks process-group termination, and reads
//! or writes pane bytes through the master file descriptor in nonblocking mode.

use std::collections::VecDeque;
use std::io;
#[cfg(unix)]
use std::os::fd::{BorrowedFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use portable_pty::MasterPty;
use rustix::event::{PollFd, PollFlags, Timespec, poll as rustix_poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::{Errno, dup as rustix_dup, read as rustix_read, write as rustix_write};
use rustix::process::Signal;

use crate::error::{MezError, Result};
use crate::layout::Size;

use super::pty::{PTY_IO_CHUNK_BYTES, pty_size};
use super::signals::send_signal_to_pane_process_group;
use super::types::PaneExitStatus;

/// Defines the DEFAULT OUTPUT BACKLOG LIMIT BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const DEFAULT_OUTPUT_BACKLOG_LIMIT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum time a synchronous pane input write may wait for PTY progress.
///
/// The runtime uses pane input for agent-owned shell transactions. If the PTY
/// stops accepting bytes, the caller needs a bounded error instead of blocking
/// before shell-transaction timeout bookkeeping can run.
const PANE_INPUT_WRITE_STALL_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum pane input bytes written in one PTY write attempt.
///
/// PTYs are interactive streams, not bulk pipes. Keeping every physical write
/// comfortably below common line-discipline and remote transport thresholds
/// makes large paste and agent-action payloads observable as a sequence of
/// bounded progress steps instead of one fragile all-or-nothing write.
pub(crate) const PTY_INPUT_WRITE_CHUNK_BYTES: usize = 1024;

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies synchronous pane-input writes allow ten seconds of stalled PTY
    /// progress before surfacing a bounded failure.
    ///
    /// Agent shell transactions can need several seconds to drain large
    /// generated wrappers through slower remotes, but they still must not hang
    /// indefinitely when the PTY stops accepting input.
    #[test]
    fn sync_pane_input_write_timeout_is_ten_seconds() {
        assert_eq!(PANE_INPUT_WRITE_STALL_TIMEOUT, Duration::from_secs(10));
    }

    /// Verifies repeated interrupted PTY polls consume the original timeout
    /// budget instead of restarting it after each signal.
    ///
    /// Pane shutdown and blocking input writes both rely on bounded PTY poll
    /// waits. Simulating several EINTR wakeups through elapsed-time accounting
    /// ensures signal storms cannot extend those waits indefinitely.
    #[test]
    fn pty_poll_timeout_budget_is_preserved_after_repeated_eintr() {
        let timeout = Duration::from_millis(100);
        let mut elapsed = Duration::ZERO;

        for interrupted_after in [10, 30, 40].map(Duration::from_millis) {
            elapsed += interrupted_after;
            assert_eq!(
                remaining_pty_poll_timeout(timeout, elapsed),
                Some(timeout - elapsed)
            );
        }

        elapsed += Duration::from_millis(30);
        assert_eq!(remaining_pty_poll_timeout(timeout, elapsed), None);
    }
}

/// Carries Pane Process state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub struct PaneProcess {
    /// Stores the child value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Stores the master value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) master: Box<dyn MasterPty + Send>,
    /// Stores the output backlog value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) output_backlog: VecDeque<u8>,
    /// Stores the output backlog limit bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) output_backlog_limit_bytes: usize,
    /// Stores the output activity sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) output_activity_sequence: u64,
    /// Stores the output closed value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) output_closed: bool,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) primary_pid: u32,
    /// Stores the process group leader value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) process_group_leader: Option<i32>,
    /// Stores the initial working directory value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) initial_working_directory: Option<PathBuf>,
    /// Stores the exit status value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) exit_status: Option<PaneExitStatus>,
}

impl std::fmt::Debug for PaneProcess {
    /// Runs the fmt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneProcess")
            .field("primary_pid", &self.primary_pid)
            .field("process_group_leader", &self.process_group_leader)
            .field("exit_status", &self.exit_status)
            .finish_non_exhaustive()
    }
}

impl PaneProcess {
    /// Runs the primary pid operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn primary_pid(&self) -> u32 {
        self.primary_pid
    }

    /// Returns the process-group leader recorded when the pane process was
    /// spawned.
    ///
    /// Portable PTY backends can expose a process-group leader that differs
    /// from the child pid returned as `primary_pid`. Foreground-shell checks use
    /// this value to compare PTY foreground process groups against the shell's
    /// process group instead of assuming pid and pgid are identical.
    pub fn process_group_leader(&self) -> Option<i32> {
        self.process_group_leader
    }

    /// Returns the live process name when the host exposes it.
    ///
    /// On Linux this reads `/proc/<pid>/comm`, so the value tracks the current
    /// executable name after shell `exec` replacement. Platforms without an
    /// equivalent non-invasive process query return `None`.
    pub fn process_name(&self) -> Option<String> {
        process_name_for_pid(self.primary_pid)
    }

    /// Returns the foreground process-group id currently owning the pane PTY.
    ///
    /// Interactive shells move foreground jobs into their own process group; this
    /// lets the runtime derive mux-like pane titles from the active program even
    /// when that program does not emit an OSC title sequence.
    pub fn foreground_process_group_id(&self) -> Option<u32> {
        foreground_process_group_id(self.master.as_raw_fd()?)
    }

    /// Returns the host-reported foreground process-group leader name.
    ///
    /// This is best-effort because process metadata can disappear as jobs exit
    /// and not all hosts expose process names through `/proc`-style interfaces.
    pub fn foreground_process_name(&self) -> Option<String> {
        process_name_for_pid(self.foreground_process_group_id()?)
    }

    /// Returns the process current working directory when the host exposes it.
    ///
    /// On Linux this reads `/proc/<pid>/cwd`, so the value tracks directory
    /// changes made by the pane's primary process. If the process exits before
    /// the host metadata can be read, this falls back to the directory used at
    /// spawn time so pane state remains stable for short-lived commands.
    pub fn current_working_directory(&self) -> Option<PathBuf> {
        current_working_directory_for_pid(self.primary_pid)
            .or_else(|| self.initial_working_directory.clone())
    }

    /// Runs the resize operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resize(&self, size: Size) -> Result<()> {
        self.master
            .resize(pty_size(size))
            .map_err(|error| MezError::invalid_state(format!("pane PTY resize failed: {error}")))
    }

    /// Runs the write input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn write_input(&mut self, input: &[u8]) -> Result<()> {
        write_all_to_pty_fd_blocking(self.master_fd()?, input)?;
        Ok(())
    }

    /// Runs the read available output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn read_available_output(&mut self, max_bytes: usize) -> Result<Vec<u8>> {
        if max_bytes == 0 {
            return Err(MezError::invalid_args(
                "pane output read limit must be greater than zero",
            ));
        }

        self.buffer_available_output(max_bytes)?;

        let read_len = max_bytes.min(self.output_backlog.len());
        let mut output = Vec::with_capacity(read_len);
        for _ in 0..read_len {
            if let Some(byte) = self.output_backlog.pop_front() {
                output.push(byte);
            }
        }
        Ok(output)
    }

    /// Returns the monotonic blocking output-activity sequence.
    ///
    /// Synchronous compatibility paths use this sequence before polling output,
    /// then wait for a larger sequence value instead of sleeping blindly.
    pub fn output_activity_sequence(&self) -> u64 {
        self.output_activity_sequence
    }

    /// Blocks until output activity exceeds `sequence` or `timeout` expires.
    ///
    /// Returns `true` when newer PTY activity is already known or the master fd
    /// becomes readable/hung up, and `false` when the timeout elapses.
    pub fn wait_for_output_activity_after(&self, sequence: u64, timeout: Duration) -> bool {
        if self.output_activity_sequence > sequence || self.output_closed {
            return true;
        }
        wait_for_pty_fd_readiness(self.master_fd().ok(), timeout).unwrap_or(false)
    }

    /// Returns whether the PTY master has observed EOF, hangup, or shutdown.
    pub fn output_reader_closed(&self) -> bool {
        self.output_closed
    }

    /// Returns whether output remains buffered or waiting to be drained.
    pub fn has_pending_output(&mut self) -> bool {
        let _ = self.buffer_available_output(1);
        !self.output_backlog.is_empty()
    }

    /// Runs the buffer available output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn buffer_available_output(&mut self, target_bytes: usize) -> Result<()> {
        if self.output_closed || target_bytes == 0 {
            return Ok(());
        }
        while self.output_backlog.len() < target_bytes {
            let available_backlog = self
                .output_backlog_limit_bytes
                .saturating_sub(self.output_backlog.len());
            if available_backlog == 0 {
                break;
            }
            let read_limit = target_bytes
                .saturating_sub(self.output_backlog.len())
                .min(available_backlog)
                .clamp(1, PTY_IO_CHUNK_BYTES);
            match read_pty_fd_nonblocking(self.master_fd()?, read_limit)? {
                PtyRead::Bytes(bytes) => {
                    if append_output_chunk_to_backlog(
                        &mut self.output_backlog,
                        bytes,
                        self.output_backlog_limit_bytes,
                    )
                    .is_some()
                    {
                        break;
                    }
                    self.output_activity_sequence = self.output_activity_sequence.saturating_add(1);
                }
                PtyRead::WouldBlock => break,
                PtyRead::Closed => {
                    self.output_closed = true;
                    self.output_activity_sequence = self.output_activity_sequence.saturating_add(1);
                    break;
                }
            }
        }
        Ok(())
    }

    /// Runs the poll exit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn poll_exit(&mut self) -> Result<Option<PaneExitStatus>> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }

        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        let status = PaneExitStatus::from_portable_exit_status(status);
        self.exit_status = Some(status);
        Ok(Some(status))
    }

    /// Runs the recorded exit status operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn recorded_exit_status(&self) -> Option<PaneExitStatus> {
        self.exit_status
    }

    /// Duplicates the PTY master fd for registration with Tokio readiness.
    ///
    /// The duplicated descriptor refers to the same nonblocking open file
    /// description as the portable-pty master. Async pane workers use it only
    /// for readiness and data transfer while this handle retains resize and
    /// process metadata ownership.
    #[cfg(unix)]
    pub fn duplicate_master_fd(&self) -> Result<OwnedFd> {
        rustix_dup(borrow_raw_pty_fd(self.master_fd()?))
            .map_err(io::Error::from)
            .map_err(Into::into)
    }

    /// Returns the raw PTY master fd exposed by portable-pty on Unix hosts.
    #[cfg(unix)]
    pub(super) fn master_fd(&self) -> Result<RawFd> {
        self.master
            .as_raw_fd()
            .ok_or_else(|| MezError::invalid_state("pane PTY master did not expose a raw fd"))
    }

    /// Runs the wait operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn wait(&mut self) -> Result<PaneExitStatus> {
        let status = self.child.wait()?;
        let status = PaneExitStatus::from_portable_exit_status(status);
        self.exit_status = Some(status);
        Ok(status)
    }

    /// Runs the terminate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn terminate(&mut self, grace: Duration) -> Result<PaneExitStatus> {
        if let Some(status) = self.exit_status {
            return Ok(status);
        }

        self.send_signal_to_process_group(Signal::HUP)?;
        if let Some(status) = self.wait_until_exit(grace)? {
            return Ok(status);
        }

        self.send_signal_to_process_group(Signal::TERM)?;
        if let Some(status) = self.wait_until_exit(grace)? {
            return Ok(status);
        }

        self.send_signal_to_process_group(Signal::KILL)?;
        let status = self.child.wait()?;
        let status = PaneExitStatus::from_portable_exit_status(status);
        self.exit_status = Some(status);
        Ok(status)
    }

    /// Runs the send signal to process group operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn send_signal_to_process_group(&mut self, signal: Signal) -> Result<()> {
        if let Some(process_group_leader) = self.process_group_leader {
            send_signal_to_pane_process_group(process_group_leader, signal)
        } else {
            self.child.kill()?;
            Ok(())
        }
    }

    /// Runs the wait until exit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn wait_until_exit(&mut self, timeout: Duration) -> Result<Option<PaneExitStatus>> {
        let started = Instant::now();
        loop {
            let activity_sequence = self.output_activity_sequence();
            if let Some(status) = self.poll_exit()? {
                return Ok(Some(status));
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Ok(None);
            }
            let _ = self.wait_for_output_activity_after(activity_sequence, timeout - elapsed);
        }
    }
}

/// Runs the configure pty master nonblocking operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn configure_pty_master_nonblocking(master: &dyn MasterPty) -> Result<()> {
    let fd = master
        .as_raw_fd()
        .ok_or_else(|| MezError::invalid_state("pane PTY master did not expose a raw fd"))?;
    let borrowed = borrow_raw_pty_fd(fd);
    let flags = fcntl_getfl(borrowed).map_err(io::Error::from)?;
    if !flags.contains(OFlags::NONBLOCK) {
        fcntl_setfl(borrowed, flags | OFlags::NONBLOCK).map_err(io::Error::from)?;
    }
    Ok(())
}

/// Carries Pty Read state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PtyRead {
    /// Represents the Bytes case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Bytes(Vec<u8>),
    /// Represents the Would Block case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    WouldBlock,
    /// Represents the Closed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Closed,
}

/// Runs the read pty fd nonblocking operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn read_pty_fd_nonblocking(fd: RawFd, max_bytes: usize) -> Result<PtyRead> {
    let mut buffer = vec![0u8; max_bytes];
    loop {
        match rustix_read(borrow_raw_pty_fd(fd), buffer.as_mut_slice()) {
            Ok(0) => return Ok(PtyRead::Closed),
            Ok(count) => {
                buffer.truncate(count);
                return Ok(PtyRead::Bytes(buffer));
            }
            Err(Errno::INTR) => continue,
            Err(Errno::IO) => return Ok(PtyRead::Closed),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(PtyRead::WouldBlock);
            }
            Err(error) => return Err(io::Error::from(error).into()),
        }
    }
}

/// Runs the write pty fd nonblocking io operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn write_pty_fd_nonblocking_io(fd: RawFd, bytes: &[u8]) -> io::Result<usize> {
    rustix_write(borrow_raw_pty_fd(fd), bytes).map_err(io::Error::from)
}

/// Runs the write all to pty fd blocking operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn write_all_to_pty_fd_blocking(fd: RawFd, input: &[u8]) -> Result<()> {
    let mut written = 0usize;
    let mut last_progress = Instant::now();
    while written < input.len() {
        let chunk_end = written
            .saturating_add(PTY_INPUT_WRITE_CHUNK_BYTES)
            .min(input.len());
        match write_pty_fd_nonblocking_io(fd, &input[written..chunk_end]) {
            Ok(0) => {
                return Err(MezError::invalid_state(
                    "pane PTY write accepted zero bytes",
                ));
            }
            Ok(count) => {
                written = written.saturating_add(count);
                last_progress = Instant::now();
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if last_progress.elapsed() >= PANE_INPUT_WRITE_STALL_TIMEOUT {
                    return Err(MezError::invalid_state(format!(
                        "pane PTY input write timed out after {} ms",
                        PANE_INPUT_WRITE_STALL_TIMEOUT.as_millis()
                    )));
                }
                let _ = wait_for_pty_fd_writability(Some(fd), Duration::from_millis(50))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Runs the wait for pty fd readiness operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wait_for_pty_fd_readiness(fd: Option<RawFd>, timeout: Duration) -> Result<bool> {
    wait_for_pty_fd_events(fd, PollFlags::IN, timeout)
}

/// Runs the wait for pty fd writability operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wait_for_pty_fd_writability(fd: Option<RawFd>, timeout: Duration) -> Result<bool> {
    wait_for_pty_fd_events(fd, PollFlags::OUT, timeout)
}

/// Runs the wait for pty fd events operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wait_for_pty_fd_events(fd: Option<RawFd>, flags: PollFlags, timeout: Duration) -> Result<bool> {
    let Some(fd) = fd else {
        return Ok(false);
    };
    let mut poll_fd = [PollFd::from_borrowed_fd(borrow_raw_pty_fd(fd), flags)];
    let started = Instant::now();
    let mut remaining = timeout;
    loop {
        let timeout_spec = duration_to_timespec(remaining)?;
        match rustix_poll(&mut poll_fd, Some(&timeout_spec)) {
            Ok(_) => {
                let revents = poll_fd[0].revents();
                return Ok(revents.intersects(flags | PollFlags::HUP | PollFlags::ERR));
            }
            Err(Errno::INTR) => {}
            Err(error) => return Err(io::Error::from(error).into()),
        }

        let Some(next_remaining) = remaining_pty_poll_timeout(timeout, started.elapsed()) else {
            return Ok(false);
        };
        remaining = next_remaining;
    }
}

/// Computes the remaining PTY poll timeout after an interrupted wait.
///
/// Returning `None` means the original timeout budget has already elapsed and
/// callers should treat the poll as timed out rather than issuing another
/// blocking wait with a refreshed full timeout.
fn remaining_pty_poll_timeout(timeout: Duration, elapsed: Duration) -> Option<Duration> {
    if elapsed >= timeout {
        None
    } else {
        Some(timeout - elapsed)
    }
}

/// Runs the duration to timespec operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn duration_to_timespec(duration: Duration) -> Result<Timespec> {
    Timespec::try_from(duration)
        .map_err(|_| MezError::invalid_args("pane PTY poll timeout is too large"))
}

/// Runs the borrow raw pty fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn borrow_raw_pty_fd(fd: RawFd) -> BorrowedFd<'static> {
    // SAFETY: pane PTY descriptors are validated by portable-pty and borrowed
    // only for immediate syscalls; ownership remains with the live pane handle
    // or a duplicated async worker descriptor.
    unsafe { BorrowedFd::borrow_raw(fd) }
}

/// Runs the append output chunk to backlog operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn append_output_chunk_to_backlog(
    backlog: &mut VecDeque<u8>,
    bytes: Vec<u8>,
    limit_bytes: usize,
) -> Option<Vec<u8>> {
    if bytes.is_empty() {
        return None;
    }
    if limit_bytes == 0 || bytes.len() > limit_bytes {
        return Some(bytes);
    }
    if backlog.len().saturating_add(bytes.len()) > limit_bytes {
        return Some(bytes);
    }
    backlog.extend(bytes);
    None
}

/// Runs the foreground process group id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(unix)]
fn foreground_process_group_id(fd: std::os::fd::RawFd) -> Option<u32> {
    // SAFETY: `fd` comes from portable-pty's live master handle and is borrowed
    // only for the duration of this immediate tcgetpgrp query.
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let process_group = rustix::termios::tcgetpgrp(fd).ok()?;
    u32::try_from(process_group.as_raw_pid()).ok()
}

/// Runs the foreground process group id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(not(unix))]
fn foreground_process_group_id(_fd: i32) -> Option<u32> {
    None
}

/// Runs the process name for pid operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(target_os = "linux")]
pub(super) fn process_name_for_pid(pid: u32) -> Option<String> {
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = name
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_string();
    (!name.is_empty()).then_some(name)
}

/// Runs the process name for pid operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(not(target_os = "linux"))]
pub(super) fn process_name_for_pid(_pid: u32) -> Option<String> {
    None
}

/// Runs the current working directory for pid operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(target_os = "linux")]
fn current_working_directory_for_pid(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Runs the current working directory for pid operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(not(target_os = "linux"))]
fn current_working_directory_for_pid(_pid: u32) -> Option<PathBuf> {
    None
}
