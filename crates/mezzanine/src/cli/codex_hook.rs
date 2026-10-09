//! Neutral content-free Codex command hook over an inherited observer socket.
//! This helper owns no daemon credentials or client role. Stdin is bounded and
//! normalized before one socket write; no transcripts, subprocesses, arbitrary
//! RPC or retries are used. Missing descriptors/outage lose telemetry only and
//! output remains the documented neutral JSON object.

use crate::error::{MezError, Result};
use crate::integrations::bootstrap::codex::{self, Observation};
use std::io::Write;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::Duration;

/// Projects only main-session inert IDs and states; raw callback data never
/// reaches the observation socket. Output length is independently bounded.
fn frame(bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    let Some(item) = codex::normalize("best-effort", bytes)? else {
        return Ok(None);
    };
    let item = match item {
        Observation::SessionReady { session } => {
            serde_json::json!({"kind":"start","session":session})
        }
        Observation::Turn {
            session,
            turn,
            state,
        } => serde_json::json!({"kind":"turn","session":session,"turn":turn,"state":state}),
        Observation::SessionEnded { session } => {
            serde_json::json!({"kind":"end","session":session})
        }
    };
    let mut bytes =
        serde_json::to_vec(&item).map_err(|_| MezError::invalid_state("Codex hook unavailable"))?;
    if bytes.len() > 1024 {
        return Err(MezError::invalid_state("Codex hook unavailable"));
    }
    bytes.push(b'\n');
    Ok(Some(bytes))
}

/// Makes one finite non-replayed write to an already authorized inherited
/// socket. Kernel peer UID and socket type precede copying bytes. Rust process
/// runtime's ignored SIGPIPE preserves neutral failure on a closed observer.
fn send(fd: &impl std::os::fd::AsFd, bytes: &[u8]) -> Result<()> {
    let stat = rustix::fs::fstat(fd).map_err(std::io::Error::from)?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Socket {
        return Err(MezError::invalid_state("Codex hook unavailable"));
    }
    crate::runtime::authenticated_unix_peer_uid(
        fd.as_fd().as_raw_fd(),
        crate::runtime::current_effective_uid(),
    )?;
    let owned = rustix::io::dup(fd).map_err(std::io::Error::from)?;
    let mut stream = std::os::unix::net::UnixStream::from(owned);
    stream.set_write_timeout(Some(Duration::from_millis(50)))?;
    if stream.write(bytes)? != bytes.len() {
        return Err(MezError::invalid_state("Codex hook unavailable"));
    }
    Ok(())
}

/// Vendor callbacks always receive neutral JSON. Parent-selected routing hints
/// cannot grant daemon authority; only the observation socket is inherited.
pub(super) fn run<W: Write>(stdout: &mut W) -> Result<()> {
    if std::env::var("MEZ_CODEX_OBSERVER_FD").as_deref() == Ok("3")
        // SAFETY: F_GETFD validates descriptor existence without modifying it.
        && unsafe { libc::fcntl(3, libc::F_GETFD) } >= 0
    {
        // SAFETY: fd3 was validated above; this helper never closes it, and send
        // validates its socket type/peer before duplicating the observer.
        let fd = unsafe { BorrowedFd::borrow_raw(3) };
        if let Ok(bytes) =
            super::harness_event::read_input_from(&std::io::stdin(), Duration::from_millis(250))
            && let Ok(Some(bytes)) = frame(&bytes)
        {
            let _ = send(&fd, &bytes);
        }
    }
    writeln!(stdout, "{{}}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Raw hook content and child IDs never escape into the inherited stream;
    /// Stop carries an inert ready boundary rather than claimed completion.
    #[test]
    fn codex_hook_frames_are_neutral_and_content_free() {
        let bytes=frame(br#"{"hook_event_name":"Stop","session_id":"session","turn_id":"turn","stop_hook_active":false,"prompt":"PRIVATE","usage":{"input_tokens":99}}"#).unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"kind":"turn","session":"session","turn":"turn","state":"ready"})
        );
        assert!(!String::from_utf8(bytes).unwrap().contains("PRIVATE"));
        assert!(
            frame(br#"{"hook_event_name":"Stop","session_id":"session","agent_id":"child"}"#)
                .unwrap()
                .is_none()
        );
    }
    /// Test-owned socket delivery succeeds once; a closed peer or ordinary file
    /// is rejected without retries, path writes, daemon access or secret output.
    /// A retained peer duplicate models fork-before-exec descriptor lifetime;
    /// closure must be established by the fixture, not assumed from one fd drop.
    #[test]
    fn codex_hook_socket_write_rejects_closed_and_non_socket_nodes() {
        use std::io::Read;
        let (socket, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
        send(&socket, b"{}\n").unwrap();
        let mut bytes = [0; 3];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"{}\n");
        let duplicate = reader.try_clone().unwrap();
        reader.shutdown(std::net::Shutdown::Both).unwrap();
        drop(reader);
        assert!(send(&socket, b"{}\n").is_err());
        drop(duplicate);
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(send(&file, b"{}\n").is_err());
    }
}
