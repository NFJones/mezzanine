//! Raw descriptor borrowing and disconnected host-output classification.

use std::io::ErrorKind;
use std::os::fd::RawFd;

use crate::error::MezError;
use rustix::fd::BorrowedFd;

pub(in crate::terminal) fn borrow_raw_fd(fd: RawFd) -> BorrowedFd<'static> {
    // SAFETY: callers pass raw descriptors already validated at API boundaries
    // and the returned borrow is consumed by one immediate rustix syscall.
    unsafe { BorrowedFd::borrow_raw(fd) }
}

/// Returns true when a terminal output write failed only because the attached
/// foreground terminal endpoint has already gone away.
pub fn attached_terminal_output_disconnected(error: &MezError) -> bool {
    matches!(
        error.io_kind(),
        Some(
            ErrorKind::BrokenPipe
                | ErrorKind::ConnectionAborted
                | ErrorKind::ConnectionReset
                | ErrorKind::NotConnected
        )
    )
}
