//! Fixed read-only daemon peer verification for persistent producer clients.
//!
//! The producer opens its own socket and lends a descriptor only for a native
//! peer-UID check. This helper never reads/writes socket data, forwards callbacks,
//! initializes a control role, launches a vendor or consumes credentials. Its
//! fixed private acknowledgment is captured by the client, not vendor stdout.
//! Passing a descriptor here supplies no telemetry authority: the producer must
//! still send its own framed observations through process-qualified admission.

use std::os::fd::RawFd;

use super::{Result, Write};

/// Handles only the exact code-owned helper argv before config/runtime startup.
/// Extra arguments cannot enter this mode; errors emit no payload/diagnostics.
pub(crate) fn run_internal_process(arguments: &[std::ffi::OsString]) -> Option<u8> {
    if arguments.len() != 2 || arguments[1] != "harness-peer" {
        return None;
    }
    Some(if run(&mut std::io::stdout()).is_ok() {
        0
    } else {
        1
    })
}

/// Verifies only the fixed private descriptor supplied by the installed client.
/// No stdin, argv token, daemon discovery or user configuration is consulted.
pub(super) fn run<W: Write>(stdout: &mut W) -> Result<()> {
    verify(3)?;
    stdout.write_all(b"{\"protocol\":\"external-peer/1\",\"verified\":true}\n")?;
    Ok(())
}

/// Requires kernel Unix peer ownership to match the current effective user.
/// Native lookup/type errors fail without any socket I/O or fallback identity.
fn verify(fd: RawFd) -> Result<()> {
    if fd < 0 {
        return Err(std::io::Error::from_raw_os_error(libc::EBADF).into());
    }
    crate::runtime::authenticated_unix_peer_uid(fd, crate::runtime::current_effective_uid())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    /// Native verification leaves queued bytes intact and rejects non-sockets
    /// and invalid descriptors. It cannot fabricate a successful peer witness.
    #[test]
    fn harness_peer_native_check_is_read_only_and_fail_closed() {
        use std::io::{Read, Write};
        let (mut first, mut second) = std::os::unix::net::UnixStream::pair().unwrap();
        second.write_all(b"queued").unwrap();
        verify(first.as_raw_fd()).unwrap();
        let mut bytes = [0; 6];
        first.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"queued");
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(verify(file.as_raw_fd()).is_err());
        assert!(verify(-1).is_err());
    }
}
