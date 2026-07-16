//! Regression tests for terminal client readiness behavior.

use crate::host::terminal::fd::duration_to_timespec;
use crate::host::terminal::{
    AttachedTerminalFd, AttachedTerminalFdRole, Duration, TerminalFdInterest, TerminalRawModeGuard,
    poll_attached_terminal_fd_readiness,
};
use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

fn pipe_pair() -> std::io::Result<(File, File)> {
    let (read_end, write_end) = rustix::pipe::pipe().map_err(std::io::Error::from)?;
    Ok((File::from(read_end), File::from(write_end)))
}

/// Verifies attached terminal fd rejects negative fd and empty interest.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_fd_rejects_negative_fd_and_empty_interest() {
    assert_eq!(
        AttachedTerminalFd::input(-1, TerminalFdInterest::read())
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert_eq!(
        AttachedTerminalFd::output(1, TerminalFdInterest::default())
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
}

/// Verifies terminal raw mode rejects invalid fd before termios calls.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_raw_mode_rejects_invalid_fd_before_termios_calls() {
    let error = TerminalRawModeGuard::enable(-1).unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies attached terminal readiness reports readable input.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_reports_readable_input() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer.write_all(b"x").unwrap();
    let descriptor =
        AttachedTerminalFd::input(reader.as_raw_fd(), TerminalFdInterest::read()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert_eq!(readiness[0].role, AttachedTerminalFdRole::Input);
    assert_eq!(readiness[0].fd, reader.as_raw_fd());
    assert!(readiness[0].readable);
    assert!(!readiness[0].writable);
    assert!(!readiness[0].hangup);
    assert!(!readiness[0].error);
    assert!(readiness[0].is_ready());
}

/// Verifies attached terminal readiness reports writable output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_reports_writable_output() {
    let (stream, _peer) = UnixStream::pair().unwrap();
    let descriptor =
        AttachedTerminalFd::output(stream.as_raw_fd(), TerminalFdInterest::write()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert_eq!(readiness[0].role, AttachedTerminalFdRole::Output);
    assert!(readiness[0].writable);
    assert!(!readiness[0].readable);
    assert!(!readiness[0].hangup);
    assert!(!readiness[0].error);
}

/// Verifies attached terminal readiness preserves control fd order.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_preserves_control_fd_order() {
    let (mut writer, input) = UnixStream::pair().unwrap();
    let (control, _control_peer) = UnixStream::pair().unwrap();
    writer.write_all(b"x").unwrap();
    let descriptors = [
        AttachedTerminalFd::control(control.as_raw_fd(), TerminalFdInterest::write()).unwrap(),
        AttachedTerminalFd::input(input.as_raw_fd(), TerminalFdInterest::read()).unwrap(),
    ];

    let readiness =
        poll_attached_terminal_fd_readiness(&descriptors, Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 2);
    assert_eq!(readiness[0].role, AttachedTerminalFdRole::Control);
    assert_eq!(readiness[0].interest, TerminalFdInterest::write());
    assert!(readiness[0].writable);
    assert_eq!(readiness[1].role, AttachedTerminalFdRole::Input);
    assert_eq!(readiness[1].interest, TerminalFdInterest::read());
    assert!(readiness[1].readable);
}

/// Verifies attached terminal readiness timeout returns not ready.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_timeout_returns_not_ready() {
    let (stream, _peer) = UnixStream::pair().unwrap();
    let descriptor =
        AttachedTerminalFd::input(stream.as_raw_fd(), TerminalFdInterest::read()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert!(!readiness[0].is_ready());
}

/// Verifies attached terminal readiness reports hangup.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_reports_hangup() {
    let (stream, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let descriptor =
        AttachedTerminalFd::input(stream.as_raw_fd(), TerminalFdInterest::read()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert!(readiness[0].hangup);
    assert!(!readiness[0].error);
}

/// Verifies attached terminal readiness reports pipe error.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_reports_pipe_error() {
    let (read_end, write_end) = pipe_pair().unwrap();
    drop(read_end);
    let descriptor =
        AttachedTerminalFd::output(write_end.as_raw_fd(), TerminalFdInterest::write()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert!(readiness[0].error);
}

/// Verifies attached terminal readiness rejects invalid fd.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_rejects_invalid_fd() {
    let error = AttachedTerminalFd::control(-1, TerminalFdInterest::read()).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies attached terminal readiness timeout conversion preserves precision.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_timeout_conversion_preserves_precision() {
    let zero = duration_to_timespec(Duration::ZERO).unwrap();
    assert_eq!(zero.tv_sec, 0);
    assert_eq!(zero.tv_nsec, 0);

    let one_nano = duration_to_timespec(Duration::from_nanos(1)).unwrap();
    assert_eq!(one_nano.tv_sec, 0);
    assert_eq!(one_nano.tv_nsec, 1);

    let two_millis = duration_to_timespec(Duration::from_millis(2)).unwrap();
    assert_eq!(two_millis.tv_sec, 0);
    assert_eq!(two_millis.tv_nsec, 2_000_000);
}
