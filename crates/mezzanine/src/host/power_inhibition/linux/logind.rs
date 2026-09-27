//! logind `Manager.Inhibit` protocol data and owned-FD lease.

use std::os::fd::OwnedFd;

use super::super::PowerInhibitionLease;

pub(super) const DESTINATION: &str = "org.freedesktop.login1";
pub(super) const PATH: &str = "/org/freedesktop/login1";
pub(super) const INTERFACE: &str = "org.freedesktop.login1.Manager";
pub(super) const METHOD: &str = "Inhibit";

/// Exact string tuple passed to logind's inhibitor method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LogindInhibitRequest {
    pub(super) what: &'static str,
    pub(super) who: &'static str,
    pub(super) why: &'static str,
    pub(super) mode: &'static str,
}

impl LogindInhibitRequest {
    pub(super) fn for_active_turn(reason: &'static str) -> Self {
        Self {
            what: "idle",
            who: "Mezzanine",
            why: reason,
            mode: "block",
        }
    }

    pub(super) fn body(self) -> (&'static str, &'static str, &'static str, &'static str) {
        (self.what, self.who, self.why, self.mode)
    }
}

/// Owned logind inhibitor descriptor; closing it releases the inhibitor.
#[derive(Debug)]
pub(super) struct LogindLease {
    fd: Option<OwnedFd>,
}

impl LogindLease {
    pub(super) fn new(fd: OwnedFd) -> Self {
        Self { fd: Some(fd) }
    }
}

impl PowerInhibitionLease for LogindLease {
    fn release(&mut self) -> Result<(), String> {
        drop(self.fd.take());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    use super::*;

    /// Verifies the received logind descriptor remains owned for the complete
    /// lease lifetime and is closed by explicit release. Peer EOF is stable
    /// even when other tests reuse the closed descriptor's numeric value.
    #[test]
    fn logind_descriptor_lifetime_matches_lease_lifetime() {
        let (owned, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let mut lease = LogindLease::new(OwnedFd::from(owned));
        let mut byte = [0_u8; 1];

        assert_eq!(
            peer.read(&mut byte).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        lease.release().unwrap();
        assert_eq!(peer.read(&mut byte).unwrap(), 0);
    }

    /// Verifies dropping an unreleased logind lease closes its owned
    /// descriptor, which is the protocol operation that removes the inhibitor.
    /// Observe EOF rather than the reusable process-wide descriptor number.
    #[test]
    fn logind_descriptor_drop_closes_owned_fd() {
        let (owned, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();

        drop(LogindLease::new(OwnedFd::from(owned)));

        assert_eq!(peer.read(&mut [0_u8; 1]).unwrap(), 0);
    }
}
