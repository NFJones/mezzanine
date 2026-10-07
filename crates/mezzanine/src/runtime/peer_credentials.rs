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

    /// Keeps a separately exec'd Rust fixture alive on its own connected socket.
    /// Only the parent test supplies the private path; normal suite runs skip it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[ignore = "self-executing fixture; invoked by the connect/accept parent test"]
    fn unix_peer_process_connect_child_fixture() {
        use std::io::{Read, Write};
        let Some(path) = std::env::var_os("MEZ_TEST_PEER_CONNECT_SOCKET") else {
            return;
        };
        let mut socket = std::os::unix::net::UnixStream::connect(path).unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(15)))
            .unwrap();
        socket.write_all(&[1]).unwrap();
        let mut reply = [0];
        socket.read_exact(&mut reply).unwrap();
        assert_eq!(reply, [2]);
    }

    /// A separately exec'd process connects ordinarily (no inherited telemetry
    /// descriptor/token). Kernel origin agrees with its native incarnation and
    /// the exact ancestor chain; stale roots and reversed ancestry fail closed.
    /// This is a transport/provenance fixture, not production pane admission.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(flavor = "current_thread")]
    async fn unix_peer_process_connect_accept_has_exact_native_ancestry() {
        use mez_mux::process::{process_ancestry, process_parent_identity_for_pid};
        use std::os::fd::AsRawFd;
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        /// Kills and reaps the fixture even if a parent assertion fails.
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        /// Removes only the uniquely created fixture directory on every exit.
        struct DirectoryGuard(std::path::PathBuf);
        impl Drop for DirectoryGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = DirectoryGuard(std::path::Path::new("/tmp").join(format!(
            "mez-peer-{}-{:x}",
            std::process::id(),
            rand::random::<u64>()
        )));
        std::fs::create_dir(&directory.0).unwrap();
        let path = directory.0.join("peer.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let root = process_parent_identity_for_pid(std::process::id()).unwrap();
        let mut child = ChildGuard(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runtime::peer_credentials::tests::unix_peer_process_connect_child_fixture",
                    "--ignored",
                    "--quiet",
                ])
                .env("MEZ_TEST_PEER_CONNECT_SOCKET", &path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let peer = peer_process(socket.as_raw_fd()).unwrap();
        assert_eq!(peer.pid, child.0.id());
        assert_eq!(peer.uid, crate::runtime::current_effective_uid());
        let mut ready = [0];
        tokio::time::timeout(Duration::from_secs(10), socket.read_exact(&mut ready))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ready, [1]);
        let origin = process_parent_identity_for_pid(peer.pid).unwrap();
        #[cfg(target_os = "linux")]
        let lifetime = match super::super::peer_process_lifetime::capture_unix_origin(
            socket.as_raw_fd(),
            crate::runtime::current_effective_uid(),
        ) {
            Ok(lifetime) => {
                assert_eq!(lifetime.identity, origin);
                assert_eq!(lifetime.reobserve().unwrap(), origin);
                Some(lifetime)
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOPROTOOPT) => None,
            Err(error) => panic!("origin lifetime capture failed: {error}"),
        };
        let evidence = tokio::task::spawn_blocking(move || process_ancestry(origin, root))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(evidence.chain.first(), Some(&origin));
        assert_eq!(evidence.chain.last(), Some(&root));
        assert_eq!(origin.parent_process_id, root.process_id);
        let mut stale = root;
        stale.start_token += 1;
        assert!(process_ancestry(origin, stale).is_err());
        assert!(process_ancestry(root, origin).is_err());
        socket.write_all(&[2]).await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "peer fixture did not exit"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        #[cfg(target_os = "linux")]
        if let Some(lifetime) = lifetime {
            // The open socket and numeric peer PID cannot keep the dead origin
            // usable. Reobservation must reject its anchored lifetime on exit.
            assert!(lifetime.reobserve().is_err());
            assert!(
                super::super::peer_process_lifetime::capture_unix_origin(
                    socket.as_raw_fd(),
                    crate::runtime::current_effective_uid(),
                )
                .is_err()
            );
        }
    }

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
        #[cfg(target_os = "linux")]
        match super::super::peer_process_lifetime::capture_unix_origin(
            stream.as_raw_fd(),
            crate::runtime::current_effective_uid(),
        ) {
            Ok(origin) => {
                assert_eq!(origin.reobserve().unwrap().process_id, std::process::id());
                assert_ne!(origin.peer.pid, child_pid);
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOPROTOOPT) => {}
            Err(error) => panic!("origin lifetime capture failed: {error}"),
        }
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
