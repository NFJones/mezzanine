//! Unix stream sender evidence for restricted process-qualified enrollment.
//!
//! Linux recvmsg supplies SCM_CREDENTIALS for each received byte segment when
//! SO_PASSCRED is enabled before sending (including on the listening socket).
//! Every read must name the retained live connection origin. Missing, mixed,
//! truncated or foreign ancillary evidence permanently poisons enrollment on
//! that connection, but ordinary UID/role control continues unchanged. Received
//! file descriptors are always closed and never treated as authority. Other
//! platforms retain ordinary byte I/O without manufacturing writer evidence.

use std::io;
use std::os::fd::RawFd;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::UnixOriginProcess;

/// Enables per-segment kernel credentials; listener configuration covers bytes
/// sent immediately after connect. Failure supplies no enrollment authority.
#[cfg(target_os = "linux")]
pub(crate) fn enable_unix_writer_credentials(fd: RawFd) -> io::Result<()> {
    let enabled: libc::c_int = 1;
    // SAFETY: initialized exact-size option input remains live for setsockopt.
    let status = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PASSCRED,
            (&enabled as *const libc::c_int).cast(),
            std::mem::size_of_val(&enabled) as libc::socklen_t,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Unsupported native sender APIs leave ordinary control intact, not attested.
#[cfg(not(target_os = "linux"))]
pub(crate) fn enable_unix_writer_credentials(_fd: RawFd) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix writer credentials unavailable",
    ))
}

/// Borrows a concrete socket and shares its immutable origin's conservative
/// sender-evidence state with actor connection clones. No RPC payload can set it.
pub(crate) struct UnixOriginStream<'a> {
    stream: &'a mut tokio::net::UnixStream,
    origin: Option<Arc<UnixOriginProcess>>,
}

impl<'a> UnixOriginStream<'a> {
    /// Installs optional sender collection without changing transport UID gates.
    pub(crate) fn new(
        stream: &'a mut tokio::net::UnixStream,
        origin: Option<Arc<UnixOriginProcess>>,
    ) -> Self {
        use std::os::fd::AsRawFd;
        if enable_unix_writer_credentials(stream.as_raw_fd()).is_err()
            && let Some(origin) = &origin
        {
            origin.record_writer(false);
        }
        Self { stream, origin }
    }
}

impl AsyncRead for UnixOriginStream<'_> {
    /// Captures native sender metadata with the same read that consumes bytes.
    /// No separate probe or client-supplied PID can attest a received frame.
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        #[cfg(not(target_os = "linux"))]
        {
            if let Some(origin) = &this.origin {
                origin.record_writer(false);
            }
            Pin::new(&mut *this.stream).poll_read(cx, buf)
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            if buf.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
            loop {
                match this.stream.poll_read_ready(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Ready(Ok(_)) => {}
                }
                let received = this.stream.try_io(tokio::io::Interest::READABLE, || {
                    receive_with_credentials(this.stream.as_raw_fd(), buf.initialize_unfilled())
                });
                match received {
                    Ok((count, credentials)) => {
                        if count > 0
                            && let Some(origin) = &this.origin
                        {
                            let matches = credentials.is_some_and(|(uid, pid)| {
                                uid == origin.uid() && pid == origin.identity.process_id
                            });
                            origin.record_writer(matches && origin.is_live());
                        }
                        buf.advance(count);
                        return Poll::Ready(Ok(()));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(error) => return Poll::Ready(Err(error)),
                }
            }
        }
    }
}

impl AsyncWrite for UnixOriginStream<'_> {
    /// Writes ordinary framed replies without changing their bytes or ownership.
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.get_mut().stream).poll_write(cx, bytes)
    }
    /// Flushes the underlying transport using its original asynchronous contract.
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.get_mut().stream).poll_flush(cx)
    }
    /// Shuts down only this connection's write side.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.get_mut().stream).poll_shutdown(cx)
    }
}

/// Reads a finite byte segment and validates exact native ancillary evidence.
/// All SCM_RIGHTS descriptors delivered by the kernel are closed, even on
/// truncation/malformed credential evidence; no descriptor becomes authority.
#[cfg(target_os = "linux")]
fn receive_with_credentials(
    fd: RawFd,
    bytes: &mut [u8],
) -> io::Result<(usize, Option<(u32, u32)>)> {
    use std::os::fd::{FromRawFd, OwnedFd};

    let mut control = [0_usize; 32]; // aligned finite ancillary buffer
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    // SAFETY: zero is valid for all pointer/length fields before initialization.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    // SAFETY: owned socket remains live, and all output buffers are initialized,
    // aligned, sized and live for this nonblocking syscall.
    let count = unsafe {
        libc::recvmsg(
            fd,
            &mut message,
            libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
        )
    };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut valid = message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) == 0;
    let mut credentials = None;
    // SAFETY: kernel-produced ancillary layout is confined to the finite aligned
    // control buffer. CMSG helpers walk only its returned initialized extent.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            let value = &*header;
            let minimum = libc::CMSG_LEN(0) as usize;
            if value.cmsg_len < minimum {
                valid = false;
                break;
            }
            let payload = value.cmsg_len - minimum;
            if value.cmsg_level == libc::SOL_SOCKET && value.cmsg_type == libc::SCM_RIGHTS {
                valid = false;
                for index in 0..payload / std::mem::size_of::<libc::c_int>() {
                    let received = libc::CMSG_DATA(header)
                        .cast::<libc::c_int>()
                        .add(index)
                        .read_unaligned();
                    if received >= 0 {
                        drop(OwnedFd::from_raw_fd(received));
                    }
                }
            } else if value.cmsg_level == libc::SOL_SOCKET
                && value.cmsg_type == libc::SCM_CREDENTIALS
                && value.cmsg_len
                    == libc::CMSG_LEN(std::mem::size_of::<libc::ucred>() as u32) as usize
                && credentials.is_none()
            {
                let value = libc::CMSG_DATA(header)
                    .cast::<libc::ucred>()
                    .read_unaligned();
                if value.pid > 0 {
                    credentials = Some((value.uid, value.pid as u32));
                } else {
                    valid = false;
                }
            } else {
                valid = false;
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    Ok((count as usize, valid.then_some(credentials).flatten()))
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
