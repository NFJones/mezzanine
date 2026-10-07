//! Native sender and ancillary rejection tests on real local sockets.

use super::*;
use std::os::fd::AsRawFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Captures exact socket origin or explicitly excludes an unsupported old kernel.
fn origin(socket: &tokio::net::UnixStream) -> Option<Arc<UnixOriginProcess>> {
    match crate::runtime::capture_unix_origin(
        socket.as_raw_fd(),
        crate::runtime::current_effective_uid(),
    ) {
        Ok(origin) => Some(Arc::new(origin)),
        Err(error) if error.raw_os_error() == Some(libc::ENOPROTOOPT) => None,
        Err(error) => panic!("origin capture failed: {error}"),
    }
}

/// Holding connection clones is not observer health. EOF releases the concrete
/// adapter immediately, and its later Drop must not double-release ownership or
/// let a still-live producer manufacture another connected observer.
#[tokio::test(flavor = "current_thread")]
async fn unix_writer_eof_releases_observer_without_producer_death() {
    let (mut reader, mut writer) = tokio::net::UnixStream::pair().unwrap();
    let Some(origin) = origin(&reader) else {
        return;
    };
    let retained = origin.clone();
    let mut qualified = UnixOriginStream::new(&mut reader, Some(origin.clone()));
    writer.write_all(b"x").await.unwrap();
    let mut byte = [0];
    qualified.read_exact(&mut byte).await.unwrap();
    assert!(origin.observer_connected());
    drop(writer);
    assert_eq!(qualified.read(&mut byte).await.unwrap(), 0);
    assert!(!retained.observer_connected());
    assert!(retained.is_live());
    drop(qualified);
    assert!(!retained.observer_connected());
}

/// A native matching sender confirms only bytes actually read, while a child
/// writing the inherited endpoint poisons enrollment forever. Subsequent parent
/// bytes are still delivered unchanged but cannot restore writer authority.
#[tokio::test(flavor = "current_thread")]
async fn unix_writer_inherited_sender_permanently_poisons_enrollment() {
    use std::os::unix::process::CommandExt;

    let (mut reader, mut writer) = tokio::net::UnixStream::pair().unwrap();
    let Some(origin) = origin(&reader) else {
        return;
    };
    assert!(!origin.writer_confirmed());
    let mut qualified = UnixOriginStream::new(&mut reader, Some(origin.clone()));
    writer.write_all(b"p").await.unwrap();
    let mut byte = [0];
    qualified.read_exact(&mut byte).await.unwrap();
    assert_eq!(&byte, b"p");
    assert!(origin.writer_confirmed());
    let fd = writer.as_raw_fd();
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args(["-c", "printf c >&3"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: the pre-exec callback uses only async-signal-safe descriptor APIs.
    // The parent holds writer until spawn completes; no Rust locks/allocation.
    unsafe {
        command.pre_exec(move || {
            if fd != 3 && libc::dup2(fd, 3) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    /// Reaps the intentionally inherited writer even after a failed assertion.
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(command.spawn().unwrap());
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        qualified.read_exact(&mut byte),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(&byte, b"c");
    assert!(!origin.writer_confirmed());
    assert!(child.0.wait().unwrap().success());
    writer.write_all(b"p").await.unwrap();
    qualified.read_exact(&mut byte).await.unwrap();
    assert_eq!(&byte, b"p");
    assert!(!origin.writer_confirmed());
}

/// Sends a finite number of copies of one descriptor using native ancillary I/O.
fn send_rights(socket: &tokio::net::UnixStream, fd: RawFd, copies: usize) {
    let mut byte = *b"x";
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    // SAFETY: requested payload is bounded by the test's <=100 native integers.
    let space =
        unsafe { libc::CMSG_SPACE((copies * std::mem::size_of::<libc::c_int>()) as u32) } as usize;
    let mut control = vec![0_usize; space.div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: zero initializes valid empty ancillary pointers/lengths.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = space;
    // SAFETY: all pointers refer to initialized, aligned, live buffers. Header
    // and integer payload fit exactly within the finite ancillary allocation.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len =
            libc::CMSG_LEN((copies * std::mem::size_of::<libc::c_int>()) as u32) as usize;
        for index in 0..copies {
            libc::CMSG_DATA(header)
                .cast::<libc::c_int>()
                .add(index)
                .write_unaligned(fd);
        }
        assert_eq!(
            libc::sendmsg(socket.as_raw_fd(), &message, libc::MSG_NOSIGNAL),
            1
        );
    }
}

/// Unexpected SCM_RIGHTS and a larger-than-receive-buffer descriptor message
/// both reject enrollment and close every delivered descriptor. Pipe EOF is
/// proof of closure without races from scanning global process descriptor tables.
#[tokio::test(flavor = "current_thread")]
async fn unix_writer_rejects_rights_and_closes_truncated_descriptors() {
    for copies in [1, 100] {
        let (mut reader, writer) = tokio::net::UnixStream::pair().unwrap();
        let Some(origin) = origin(&reader) else {
            return;
        };
        let mut qualified = UnixOriginStream::new(&mut reader, Some(origin.clone()));
        let (pipe_reader, pipe_writer) = rustix::pipe::pipe_with(
            rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK,
        )
        .unwrap();
        send_rights(&writer, pipe_writer.as_raw_fd(), copies);
        drop(pipe_writer);
        let mut byte = [0];
        qualified.read_exact(&mut byte).await.unwrap();
        assert_eq!(&byte, b"x");
        assert!(!origin.writer_confirmed());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match rustix::io::read(&pipe_reader, &mut byte) {
                Ok(0) => break,
                Err(rustix::io::Errno::AGAIN) => {
                    // Parallel fork-to-exec can temporarily inherit CLOEXEC FDs;
                    // it cannot keep them alive forever or excuse a receiver leak.
                    assert!(
                        std::time::Instant::now() < deadline,
                        "received descriptor leaked"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                other => panic!("unexpected pipe outcome: {other:?}"),
            }
        }
    }
}
