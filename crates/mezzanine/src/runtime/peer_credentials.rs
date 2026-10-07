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

/// Kernel-qualified connection-origin credentials, not pane or role authority.
/// Socket inheritance/descriptor passing does not attest the current writer;
/// callers still need fresh process-start/ancestry evidence and actor fencing.
#[allow(
    dead_code,
    reason = "ordinary enrollment consumes the process witness in a later phase"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UnixPeerProcess {
    pub(super) uid: u32,
    pub(super) pid: u32,
}

/// Requires an exact native socket-option result and a positive kernel PID.
/// A malformed or absent PID cannot be replaced with a caller/payload hint.
#[allow(
    dead_code,
    reason = "shared peer-process lookup is not admission by itself"
)]
fn process_witness(
    uid: u32,
    pid: libc::pid_t,
    actual: usize,
    expected: usize,
) -> io::Result<UnixPeerProcess> {
    if actual != expected || pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unix peer process evidence unavailable",
        ));
    }
    Ok(UnixPeerProcess {
        uid,
        pid: pid as u32,
    })
}

/// Captures native Linux connection-origin UID/PID in one kernel lookup. No
/// files, subprocesses, inferred environment or credential changes are involved.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[allow(
    dead_code,
    reason = "ordinary enrollment provenance boundary under construction"
)]
pub(super) fn peer_process(raw_fd: RawFd) -> io::Result<UnixPeerProcess> {
    let mut value = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let expected = std::mem::size_of::<libc::ucred>();
    let mut length = expected as libc::socklen_t;
    // SAFETY: initialized exact ucred/length outputs remain live through the
    // syscall. Invalid descriptors are returned as ordinary operating-system errors.
    let result = unsafe {
        libc::getsockopt(
            raw_fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut value as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    process_witness(value.uid, value.pid, length as usize, expected)
}

/// Captures macOS kernel peer UID and LOCAL_PEERPID without external helpers.
/// Peer PID is connection evidence only, not a durable process incarnation.
#[cfg(target_os = "macos")]
#[allow(
    dead_code,
    reason = "ordinary enrollment provenance boundary under construction"
)]
pub(super) fn peer_process(raw_fd: RawFd) -> io::Result<UnixPeerProcess> {
    let uid = peer_effective_uid(raw_fd)?;
    let mut pid: libc::pid_t = 0;
    let expected = std::mem::size_of::<libc::pid_t>();
    let mut length = expected as libc::socklen_t;
    // SAFETY: initialized pid/length outputs are sized for the native local
    // socket option and remain valid until getsockopt returns.
    let result = unsafe {
        libc::getsockopt(
            raw_fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    process_witness(uid, pid, length as usize, expected)
}

/// Other supported UID-only Unix hosts retain their current authentication;
/// process-qualified enrollment fails closed until a native PID API is provided.
#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
#[allow(
    dead_code,
    reason = "ordinary enrollment provenance boundary under construction"
)]
pub(super) fn peer_process(_raw_fd: RawFd) -> io::Result<UnixPeerProcess> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix peer process lookup unsupported",
    ))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A child can write on an inherited endpoint whose kernel peer identity
    /// still names its creator. This regression prevents treating peer PID alone
    /// as current-writer or vendor/pane authority in future automatic admission.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[tokio::test(flavor = "current_thread")]
    async fn unix_peer_process_inherited_writer_does_not_change_origin() {
        use crate::integrations::bootstrap::pi_launch::{self, LaunchSpec};
        use std::os::fd::AsRawFd;
        use tokio::io::AsyncReadExt;
        let launched = pi_launch::spawn(LaunchSpec {
            executable: "/bin/sh".into(),
            directory: std::env::temp_dir(),
            arguments: vec!["-c".into(), "printf inherited-writer >&3".into()],
            environment: vec![],
            stdin: std::process::Stdio::null(),
            stdout: std::process::Stdio::null(),
            stderr: std::process::Stdio::null(),
        })
        .unwrap();
        let mut child = launched.child;
        let child_pid = child.id().unwrap();
        let mut stream = launched.observer;
        let peer = peer_process(stream.as_raw_fd()).unwrap();
        assert_eq!(peer.pid, std::process::id());
        assert_ne!(peer.pid, child_pid);
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"inherited-writer");
        assert!(child.wait().await.unwrap().success());
    }
    /// Exact socket-option size and positive PID are mandatory; zero, negative
    /// and shortened/oversized evidence cannot become a process witness.
    #[test]
    fn unix_peer_process_rejects_incomplete_native_evidence() {
        assert_eq!(
            process_witness(1000, 12, 4, 4).unwrap(),
            UnixPeerProcess { uid: 1000, pid: 12 }
        );
        for (pid, size) in [(0, 4), (-1, 4), (12, 3), (12, 5)] {
            assert!(process_witness(1000, pid, size, 4).is_err());
        }
    }
    /// A live socket pair reports kernel connection-origin identity while an
    /// ordinary file/closed descriptor cannot produce that evidence. No process
    /// is spawned or credentials/environment changed to obtain this witness.
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    #[test]
    fn unix_peer_process_socket_pair_reports_kernel_origin() {
        use std::os::fd::AsRawFd;
        let (first, second) = std::os::unix::net::UnixStream::pair().unwrap();
        for socket in [&first, &second] {
            let peer = peer_process(socket.as_raw_fd()).unwrap();
            assert_eq!(peer.uid, crate::runtime::current_effective_uid());
            assert_eq!(peer.pid, std::process::id());
        }
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(peer_process(file.as_raw_fd()).is_err());
        assert!(peer_process(-1).is_err());
    }
}
