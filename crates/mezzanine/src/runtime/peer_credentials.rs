//! Operating-system Unix peer credential lookup.
//!
//! This module isolates the platform APIs used to authenticate local control
//! socket peers. Linux-family kernels expose effective credentials through
//! `SO_PEERCRED`, while Apple and BSD systems expose them through
//! `getpeereid`. Every implementation returns the same effective-user-id
//! contract, and unsupported hosts fail closed instead of manufacturing an
//! identity.

use std::io;
use std::os::fd::RawFd;

#[cfg(any(target_os = "android", target_os = "linux"))]
use rustix::fd::BorrowedFd;
#[cfg(any(target_os = "android", target_os = "linux"))]
use rustix::net::sockopt::socket_peercred;

/// Returns the effective user id of the peer connected to a Unix socket.
///
/// `raw_fd` must identify a live, connected Unix-domain socket for the duration
/// of this call. Operating-system lookup failures are returned unchanged as
/// I/O errors so the authorization caller can reject unauthenticated peers.
#[cfg(any(target_os = "android", target_os = "linux"))]
pub(super) fn peer_effective_uid(raw_fd: RawFd) -> io::Result<u32> {
    // SAFETY: callers retain ownership of a live connected Unix-stream
    // descriptor, and the borrow lasts only for the immediate socket option
    // lookup.
    let borrowed_fd = unsafe { BorrowedFd::borrow_raw(raw_fd) };
    socket_peercred(borrowed_fd)
        .map(|credentials| credentials.uid.as_raw())
        .map_err(io::Error::from)
}

/// Returns the effective user id of the peer connected to a Unix socket.
///
/// `raw_fd` must identify a live, connected Unix-domain socket for the duration
/// of this call. Operating-system lookup failures are returned unchanged as
/// I/O errors so the authorization caller can reject unauthenticated peers.
#[cfg(any(
    target_vendor = "apple",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub(super) fn peer_effective_uid(raw_fd: RawFd) -> io::Result<u32> {
    let mut peer_uid: libc::uid_t = 0;
    let mut peer_gid: libc::gid_t = 0;
    // SAFETY: `raw_fd` is a live connected Unix-stream descriptor supplied by
    // the caller, and both output pointers refer to initialized values that
    // remain valid for the duration of the call.
    let status = unsafe { libc::getpeereid(raw_fd, &mut peer_uid, &mut peer_gid) };
    if status == 0 {
        Ok(peer_uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Rejects peer credential lookup on Unix targets without a supported API.
///
/// Returning `Unsupported` preserves the runtime's fail-closed authorization
/// contract while allowing the remainder of the Unix application to compile.
#[cfg(not(any(
    target_os = "android",
    target_os = "linux",
    target_vendor = "apple",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
pub(super) fn peer_effective_uid(_raw_fd: RawFd) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix peer credential lookup is unsupported on this host",
    ))
}
