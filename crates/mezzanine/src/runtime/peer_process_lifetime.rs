//! Kernel lifetime fences for local socket-origin process observations.
//!
//! A numeric SO_PEERCRED/LOCAL_PEERPID value can outlive its originating process.
//! Opening a fresh pidfd by numeric PID would anchor a replacement, not that
//! origin. Linux SO_PEERPIDFD instead supplies the socket's retained kernel
//! process object. Capture and reobservation require that object to remain live
//! before and after reading the native PID/parent/start record. Unsupported
//! kernels/platforms fail closed, with no numeric-PID fallback. This is groundwork
//! only: no role, producer ownership, pane attribution or current-writer proof
//! follows from retaining a socket-origin anchor. Unix runtime connections retain
//! optional anchors before framing, but admission does not yet consume them;
//! existing UID-only control gates remain unchanged.

use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::fd::{OwnedFd, RawFd};

use mez_mux::process::ProcessParentIdentity;

use super::peer_credentials::UnixPeerProcess;

/// Owns the exact connection-origin lifetime anchor and its first native record.
#[derive(Debug)]
pub(crate) struct UnixOriginProcess {
    /// Kernel connection-origin UID/PID, never a payload-supplied process claim.
    pub(super) peer: UnixPeerProcess,
    /// Native parent/start snapshot captured while the origin anchor was live.
    pub(super) identity: ProcessParentIdentity,
    /// Connection-origin pidfd; never a newly opened pidfd for a numeric PID.
    lifetime: OwnedFd,
    /// Unseen(0), sender-confirmed(1), or permanently unavailable/poisoned(2).
    writer: std::sync::atomic::AtomicU8,
    /// Bounded live concrete observer adapters, distinct from producer lifetime.
    observers: std::sync::atomic::AtomicUsize,
}

impl UnixOriginProcess {
    /// Retains one concrete observer transport owner. Overflow fails closed;
    /// only the adapter that acquired this count may release it on Drop.
    pub(super) fn enter_observer(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.observers
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                (count < 256).then_some(count + 1)
            })
            .is_ok()
    }

    /// Releases one successful adapter acquisition; guards call this exactly
    /// once, so no disconnected clone can keep the observer count elevated.
    pub(super) fn leave_observer(&self) {
        use std::sync::atomic::Ordering;
        let _ = self
            .observers
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                count.checked_sub(1)
            });
    }

    /// Observer transport health does not follow from a live producer PID alone.
    pub(crate) fn observer_connected(&self) -> bool {
        self.observers.load(std::sync::atomic::Ordering::SeqCst) > 0
            && self.writer_confirmed()
            && self.is_live()
    }

    /// Records kernel evidence for consumed bytes. A mismatch never recovers on
    /// this connection, so buffered mixed-origin frames cannot gain admission.
    pub(super) fn record_writer(&self, matches: bool) {
        use std::sync::atomic::Ordering;
        if matches {
            let _ = self
                .writer
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst);
        } else {
            self.writer.store(2, Ordering::SeqCst);
        }
    }

    /// Returns true only after every consumed segment has named the same live
    /// socket origin. Native capture by itself never confirms a current writer.
    pub(crate) fn writer_confirmed(&self) -> bool {
        self.writer.load(std::sync::atomic::Ordering::SeqCst) == 1
    }

    /// Returns the immutable kernel-authenticated UID for connection binding.
    pub(crate) fn uid(&self) -> u32 {
        self.peer.uid
    }

    /// Polls the retained exact lifetime without reading procfs. Used for bounded
    /// registry maintenance; unsupported platforms cannot report a live origin.
    pub(crate) fn is_live(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            require_live_origin(&self.lifetime).is_ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// Rechecks the original lifetime and exact native record before use. Parent
    /// change, PID reuse, exit or unreadable evidence invalidates the observation.
    pub(crate) fn reobserve(&self) -> io::Result<ProcessParentIdentity> {
        let identity = observe_live_origin(&self.lifetime, self.peer.pid)?;
        if identity != self.identity {
            return Err(unavailable());
        }
        Ok(identity)
    }
}

impl PartialEq for UnixOriginProcess {
    /// Equality denotes the same retained object, never matching numeric PIDs.
    /// Connection clones share this object through Arc; a recapture is distinct.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for UnixOriginProcess {}

/// Fixed diagnostic deliberately excludes all process metadata and callbacks.
fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "Unix connection origin is unavailable",
    )
}

/// Accepts only a still-live lifetime descriptor. Poll is nonblocking; death,
/// invalid descriptors and unexpected readiness all fail closed.
#[cfg(target_os = "linux")]
fn require_live_origin(lifetime: &OwnedFd) -> io::Result<()> {
    let mut entry = libc::pollfd {
        fd: lifetime.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized poll entry remains valid through this zero-timeout
    // syscall. The descriptor stays owned for the full call.
    let count = unsafe { libc::poll(&mut entry, 1, 0) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    if count != 0 || entry.revents != 0 {
        return Err(unavailable());
    }
    Ok(())
}

/// Observes native parent/start metadata only inside the exact lifetime fence.
#[cfg(target_os = "linux")]
fn observe_live_origin(lifetime: &OwnedFd, pid: u32) -> io::Result<ProcessParentIdentity> {
    observe_with_lifetime(
        pid,
        || require_live_origin(lifetime),
        mez_mux::process::process_parent_identity_for_pid,
    )
}

/// Brackets the native read with exact-origin liveness checks; the injected seam
/// allows deterministic exit-during-read regressions without numeric PID races.
#[cfg(any(target_os = "linux", test))]
fn observe_with_lifetime(
    pid: u32,
    mut live: impl FnMut() -> io::Result<()>,
    mut read: impl FnMut(u32) -> Option<ProcessParentIdentity>,
) -> io::Result<ProcessParentIdentity> {
    live()?;
    let identity = read(pid).ok_or_else(unavailable)?;
    live()?;
    if identity.process_id != pid {
        return Err(unavailable());
    }
    Ok(identity)
}

/// No other platform may substitute an unanchored fresh PID/start observation.
#[cfg(not(target_os = "linux"))]
fn observe_live_origin(_lifetime: &OwnedFd, _pid: u32) -> io::Result<ProcessParentIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix origin lifetime lookup unsupported",
    ))
}

/// Captures same-user socket-origin lifetime plus native incarnation. Older
/// kernels lacking SO_PEERPIDFD return their ordinary OS error without fallback.
/// Call on a bounded connection worker, not the serialized runtime actor.
#[cfg(target_os = "linux")]
pub(crate) fn capture_unix_origin(raw_fd: RawFd, owner_uid: u32) -> io::Result<UnixOriginProcess> {
    use std::os::fd::FromRawFd;

    let peer = super::peer_credentials::peer_process(raw_fd)?;
    if peer.uid != owner_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Unix origin user mismatch",
        ));
    }
    let mut fd: libc::c_int = -1;
    let expected = std::mem::size_of::<libc::c_int>();
    let mut length = expected as libc::socklen_t;
    // SAFETY: exact initialized integer/length outputs remain live through the
    // native socket option. Successful retrieval transfers a new owned pidfd.
    let result = unsafe {
        libc::getsockopt(
            raw_fd,
            libc::SOL_SOCKET,
            libc::SO_PEERPIDFD,
            (&mut fd as *mut libc::c_int).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if fd < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unix origin lifetime result malformed",
        ));
    }
    // SAFETY: the successful SO_PEERPIDFD call transferred this new descriptor.
    // Take ownership before further validation so every error path closes it.
    let lifetime = unsafe { OwnedFd::from_raw_fd(fd) };
    if length as usize != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unix origin lifetime result malformed",
        ));
    }
    // SAFETY: the owned descriptor is live and F_GETFD reads flags only.
    let flags = unsafe { libc::fcntl(lifetime.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::FD_CLOEXEC == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unix origin lifetime not close-on-exec",
        ));
    }
    let identity = observe_live_origin(&lifetime, peer.pid)?;
    Ok(UnixOriginProcess {
        peer,
        identity,
        lifetime,
        writer: std::sync::atomic::AtomicU8::new(0),
        observers: std::sync::atomic::AtomicUsize::new(0),
    })
}

/// Fails closed until a native socket-origin lifetime/version interface is
/// reviewed for this platform. Existing ordinary Unix control remains usable.
#[cfg(not(target_os = "linux"))]
pub(crate) fn capture_unix_origin(
    _raw_fd: RawFd,
    _owner_uid: u32,
) -> io::Result<UnixOriginProcess> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix origin lifetime lookup unsupported",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process may exit while its numeric record is being read. Both lifetime
    /// fences are required: dead-before-read performs no lookup, and death
    /// during lookup rejects even a well-shaped replacement-like record.
    #[test]
    fn unix_origin_lifetime_rejects_exit_during_native_read() {
        use std::cell::Cell;

        let identity = ProcessParentIdentity {
            process_id: 12,
            parent_process_id: 1,
            start_token: 99,
        };
        let dead = Cell::new(false);
        let result = observe_with_lifetime(
            12,
            || {
                if dead.get() {
                    Err(unavailable())
                } else {
                    Ok(())
                }
            },
            |_| {
                dead.set(true);
                Some(identity)
            },
        );
        assert!(result.is_err());
        let read = Cell::new(false);
        assert!(
            observe_with_lifetime(
                12,
                || Err(unavailable()),
                |_| {
                    read.set(true);
                    Some(identity)
                }
            )
            .is_err()
        );
        assert!(!read.get());
        assert_eq!(
            observe_with_lifetime(12, || Ok(()), |_| Some(identity)).unwrap(),
            identity
        );
        assert!(observe_with_lifetime(13, || Ok(()), |_| Some(identity)).is_err());
        assert!(observe_with_lifetime(12, || Ok(()), |_| None).is_err());
    }

    /// A socketpair anchors its actual creator. Validity is not writer identity;
    /// the pidfd is private/CLOEXEC and wrong UID/files cannot supply authority.
    /// Older Linux kernels are explicitly unsupported, not numeric-PID fallback.
    #[cfg(target_os = "linux")]
    #[test]
    fn unix_origin_lifetime_socketpair_is_private_and_reobserved() {
        let (socket, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        let uid = crate::runtime::current_effective_uid();
        let captured = capture_unix_origin(socket.as_raw_fd(), uid);
        if let Err(error) = &captured
            && error.raw_os_error() == Some(libc::ENOPROTOOPT)
        {
            eprintln!("SO_PEERPIDFD unavailable; origin capture fails closed");
            return;
        }
        let mut origin = captured.unwrap();
        assert_eq!(origin.peer.pid, std::process::id());
        assert_eq!(origin.reobserve().unwrap(), origin.identity);
        // SAFETY: flag inspection only on a retained owned descriptor.
        let flags = unsafe { libc::fcntl(origin.lifetime.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        origin.identity.start_token = origin.identity.start_token.wrapping_add(1);
        assert!(origin.reobserve().is_err());
        assert_eq!(
            capture_unix_origin(socket.as_raw_fd(), uid.wrapping_add(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(capture_unix_origin(file.as_raw_fd(), uid).is_err());
        assert!(capture_unix_origin(-1, uid).is_err());
    }

    /// Unsupported native lifetime interfaces cannot turn an arbitrary numeric
    /// process hint or descriptor into incarnation evidence on other platforms.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unix_origin_lifetime_unsupported_platform_fails_closed() {
        assert_eq!(
            capture_unix_origin(-1, 0).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
}
