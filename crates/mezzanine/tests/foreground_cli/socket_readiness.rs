//! Detached foreground fixture readiness, independent of attach protocol state.
//!
//! A Unix socket pathname can exist before listening or after its owner exits.
//! The gate must therefore prove connectability while its child remains alive,
//! without sending control frames, requesting a role, or exposing child output.

use super::*;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;

/// Waits for connectability while checking the owned daemon before every probe
/// and after success. Errors describe only stage, status, and OS error codes;
/// child output and path contents are never included. The caller's process guard
/// remains responsible for kill/reap on errors and assertion unwinding.
pub(super) fn wait_for_socket(
    child: &mut ProcessChild,
    path: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut last_error = None;
    loop {
        require_running_child(child)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "timed out waiting for detached socket readiness; child still running; last_connect={last_error:?}"
            ));
        }
        match probe_socket(path, remaining.min(Duration::from_millis(20))) {
            Ok(()) => {
                require_running_child(child)?;
                return Ok(());
            }
            Err(error) => {
                let kind = error.kind();
                let code = error.raw_os_error();
                if !matches!(
                    kind,
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) {
                    return Err(format!(
                        "detached socket probe failed: kind={kind:?} errno={code:?}"
                    ));
                }
                last_error = Some((kind, code));
            }
        }
        // Evidence-driven polling, not a fixed startup delay or attach retry.
        thread::sleep(
            Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

/// Polls/reaps an exited child without collecting potentially private output.
fn require_running_child(child: &mut ProcessChild) -> Result<(), String> {
    match child.try_wait() {
        Ok(None) => Ok(()),
        Ok(Some(status)) => Err(format!(
            "detached serve child exited before socket readiness: status={status:?}"
        )),
        Err(error) => Err(format!(
            "failed to poll detached serve child: kind={:?} errno={:?}",
            error.kind(),
            error.raw_os_error(),
        )),
    }
}

/// Builds a pathname address with Linux/macOS layout and length conventions.
/// Rejects oversized or NUL-bearing paths before any descriptor is allocated.
fn socket_address(path: &Path) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    // SAFETY: all-zero initialization is valid for sockaddr_un.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = length as u8;
    }
    Ok((address, length as libc::socklen_t))
}

/// Opens a uniquely owned nonblocking Unix stream. Nonblocking connect is
/// essential: a full accept backlog must not exceed the fixture's deadline.
fn probe_descriptor() -> std::io::Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    let flags = libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = libc::SOCK_STREAM;
    // SAFETY: socket has no pointer arguments and returns a fresh descriptor.
    let fd = unsafe { libc::socket(libc::AF_UNIX, flags, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is newly allocated and has no other owner.
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: fcntl sets flags on our owned descriptor without pointers.
        // macOS has no atomic SOCK_NONBLOCK/SOCK_CLOEXEC socket flags.
        if unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0
            || unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(socket)
}

/// Connects and immediately drops the stream without sending any bytes. This
/// proves listening only, never control initialization, authentication, client
/// role selection, or application readiness. Each attempt has its own deadline.
fn probe_socket(path: &Path, timeout: Duration) -> std::io::Result<()> {
    let (address, length) = socket_address(path)?;
    let socket = probe_descriptor()?;
    // SAFETY: address and length identify an initialized sockaddr_un and the
    // owned nonblocking descriptor is live for the duration of the call.
    if unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length,
        )
    } == 0
    {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EINPROGRESS) {
        return Err(error);
    }
    let mut poll = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: poll points to one initialized descriptor record and is bounded
    // by the remaining gate allowance (at most 20 ms).
    let ready = unsafe { libc::poll(&mut poll, 1, timeout.as_millis().min(20) as i32) };
    if ready < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if ready == 0 {
        return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
    }
    let mut code: libc::c_int = 0;
    let mut length = std::mem::size_of_val(&code) as libc::socklen_t;
    // SAFETY: code and length are writable output buffers of the required
    // sizes; SO_ERROR checks completion without reading or sending payloads.
    if unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&mut code as *mut libc::c_int).cast(),
            &mut length,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    if code != 0 {
        return Err(std::io::Error::from_raw_os_error(code));
    }
    Ok(())
}

/// Binds a local socket without listening so the regression controls the exact
/// startup boundary instead of relying on daemon timing or successful reruns.
fn bound_socket(path: &Path) -> OwnedFd {
    // SAFETY: socket returns a fresh descriptor, owned exactly once below.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(fd >= 0);
    // SAFETY: fd is newly allocated and has no other owner.
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    let (address, length) = socket_address(path).unwrap();
    // SAFETY: address contains the family and a terminated pathname; length
    // covers those initialized fields and socket remains owned for this call.
    assert_eq!(
        unsafe {
            libc::bind(
                socket.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                length as libc::socklen_t,
            )
        },
        0
    );
    socket
}

/// A socket can be present but refuse connections until listen is called.
/// The readiness gate must reject that state, then pass only after the same
/// descriptor transitions to listening. No RPC or attach retry is involved.
#[test]
fn detached_socket_readiness_requires_listening_not_path() {
    let root = test_root("socket-ready-gate");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("s.sock");
    let socket = bound_socket(&path);
    assert!(path.exists());
    let mut child = live_fixture_child();
    let before = wait_for_socket(&mut child, &path, Duration::from_millis(40));
    // Arrange the positive transition before asserting, so even a RED failure
    // leaves no blocked thread or unowned descriptor behind.
    // SAFETY: socket is an owned, bound stream socket.
    assert_eq!(unsafe { libc::listen(socket.as_raw_fd(), 1) }, 0);
    let after = wait_for_socket(&mut child, &path, Duration::from_millis(40));
    fs::remove_dir_all(&root).unwrap();
    assert!(
        before.is_err(),
        "a bound pathname is not a listening socket"
    );
    assert!(after.is_ok());
}

/// Owns an inert live process so readiness tests exercise real child-health
/// checks without launching another daemon, shell descendants, or RPC clients.
fn live_fixture_child() -> DetachedServeProcess {
    DetachedServeProcess(
        Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

/// A leftover socket after listener destruction cannot pass readiness. Failure
/// must remain bounded and content-free, and dropping the fixture must reap its
/// still-live child rather than leaving it behind after a timeout assertion.
#[test]
fn detached_socket_readiness_rejects_stale_socket_and_reaps_child() {
    let root = test_root("socket-ready-stale");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("s.sock");
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    let mut child = live_fixture_child();
    let pid = child.id();
    let error = wait_for_socket(&mut child, &path, Duration::from_millis(40)).unwrap_err();
    assert!(error.contains("timed out"));
    assert!(error.contains("child still running"));
    assert!(error.contains("ConnectionRefused"));
    assert!(!error.contains(path.to_str().unwrap()));
    drop(child);
    // SAFETY: signal zero probes existence only; the pid was our reaped child.
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    fs::remove_dir_all(&root).unwrap();
}

/// Listener success cannot mask a dead child. Exit diagnostics must identify
/// status, not dump private stdout/stderr, and the exited child is reaped even
/// though the listening descriptor belongs to another fixture component.
#[test]
fn detached_socket_readiness_reports_child_exit_without_output() {
    let root = test_root("socket-ready-dead");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("s.sock");
    let _listener = UnixListener::bind(&path).unwrap();
    let mut child = DetachedServeProcess(
        Command::new("/bin/sh")
            .args([
                "-c",
                "printf PRIVATE_CHILD_OUTPUT; printf PRIVATE_CHILD_OUTPUT >&2; exit 7",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    assert_eq!(child.wait().unwrap().code(), Some(7));
    let error = wait_for_socket(&mut child, &path, Duration::from_secs(1)).unwrap_err();
    assert!(error.contains("child exited before socket readiness"));
    assert!(error.contains("status="));
    assert!(!error.contains("PRIVATE_CHILD_OUTPUT"));
    assert!(!error.contains(path.to_str().unwrap()));
    fs::remove_dir_all(&root).unwrap();
}

/// A successful probe opens and closes one stream without a control frame or
/// even a single byte. Authentication and initial terminal rendering remain
/// exclusively the subsequent real attach client's responsibility.
#[test]
fn detached_socket_readiness_probe_is_protocol_inert() {
    let root = test_root("socket-ready-inert");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("s.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let mut child = live_fixture_child();
    wait_for_socket(&mut child, &path, Duration::from_secs(1)).unwrap();
    let (mut probe, _) = listener.accept().unwrap();
    probe
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(probe.read(&mut [0; 1]).unwrap(), 0);
    fs::remove_dir_all(&root).unwrap();
}

/// Linux reports EAGAIN when a nonblocking Unix connection meets a full accept
/// backlog. Fill that backlog with owned inert streams and prove the gate times
/// out rather than blocking inside connect. No server accept, RPC, or sleep is
/// needed to arrange saturation; every descriptor and child stays guarded.
#[cfg(target_os = "linux")]
#[test]
fn detached_socket_readiness_full_backlog_remains_bounded() {
    let root = test_root("socket-ready-full");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("s.sock");
    let listener = bound_socket(&path);
    // SAFETY: the owned bound descriptor is a stream socket.
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
    let (address, length) = socket_address(&path).unwrap();
    let mut blockers = Vec::new();
    let mut saturated = false;
    for _ in 0..32 {
        let socket = probe_descriptor().unwrap();
        // SAFETY: the initialized address and owned nonblocking descriptor
        // remain live during connect; no payload is exchanged.
        let result = unsafe {
            libc::connect(
                socket.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                length,
            )
        };
        if result == 0 {
            blockers.push(socket);
        } else {
            assert_eq!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::WouldBlock
            );
            saturated = true;
            break;
        }
    }
    assert!(saturated, "controlled backlog did not fill");
    let mut child = live_fixture_child();
    let error = wait_for_socket(&mut child, &path, Duration::from_millis(40)).unwrap_err();
    assert!(error.contains("timed out"));
    assert!(error.contains("WouldBlock"));
    drop(blockers);
    fs::remove_dir_all(&root).unwrap();
}
