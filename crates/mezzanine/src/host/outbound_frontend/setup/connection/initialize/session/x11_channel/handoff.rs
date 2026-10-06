//! Exact local-owner admission for a dedicated X11 byte stream.
//!
//! Kernel peer UID is authenticated before decoding a bounded closed handshake.
//! The owner supplies the validated frontend/session and a nonreused channel
//! occurrence. Version-two requests cannot choose an occurrence; the ready reply
//! reports the owner-assigned value. These labels grant no remote authority.
//! Control framing ends only after an exact ready reply has flushed. Buffered
//! read-ahead bytes reject; bytes still in the kernel remain intact for raw relay.
//! This helper
//! publishes no listener, starts no task and performs no local X credential work.
//! Ordinary supervisor/CLI forwarding remains separately gated.

use super::*;
use crate::host::outbound_frontend::client::SessionSummary;
use std::os::fd::AsRawFd;

/// Closed handshake excluding route proof, local targets and credentials.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    protocol: String,
    handle: FrontendHandle,
    session: SessionSummary,
}

/// Authenticates an already connected dedicated stream and confirms its binding.
/// Callers bound accepted streams and allocate nonreused occurrences before this
/// operation; frontend requests never select that value. Timeout/cancellation
/// consumes the stream without reconnect/replay.
/// Success permits raw relay only on this stream, never the control connection.
pub(super) async fn authenticate_frontend(
    stream: tokio::net::UnixStream,
    owner_uid: u32,
    handle: &FrontendHandle,
    session: &SessionSummary,
    occurrence: u64,
    budget: Duration,
) -> Result<tokio::net::UnixStream> {
    if occurrence == 0 || !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&budget)
    {
        return Err(MezError::invalid_args(
            "outbound X11 handoff budget or occurrence invalid",
        ));
    }
    crate::runtime::authenticated_unix_peer_uid(stream.as_raw_fd(), owner_uid)?;
    tokio::time::timeout(budget, async move {
        let mut framed = Framed::new(stream, ProtocolFrameCodec::new(HELLO_LIMIT)?);
        let frame = framed
            .next()
            .await
            .transpose()?
            .ok_or_else(|| MezError::invalid_state("outbound X11 handoff unavailable"))?;
        if frame.content_type != CONTENT_TYPE {
            return Err(MezError::invalid_args("outbound X11 handoff type invalid"));
        }
        let request: Request = serde_json::from_str(&frame.body)
            .map_err(|_| MezError::invalid_args("outbound X11 handoff invalid"))?;
        validate_request(&request, handle, session)?;
        if !framed.read_buffer().is_empty() {
            return Err(MezError::invalid_args(
                "outbound X11 handoff contains premature bytes",
            ));
        }
        let body = serde_json::json!({"protocol":"mez-outbound-x11/2","handle":handle,
            "session":session,"occurrence":occurrence,"ready":true})
        .to_string();
        if body.len() > HELLO_LIMIT {
            return Err(MezError::invalid_state(
                "outbound X11 handoff reply exceeds limit",
            ));
        }
        framed.send(ProtocolFrame::new(CONTENT_TYPE, body)).await?;
        // SinkExt::send flushes the complete ready frame. No read happens after
        // admission; the verified empty read buffer cannot lose raw X11 data.
        Ok(framed.into_inner())
    })
    .await
    .map_err(|_| MezError::invalid_state("outbound X11 handoff timed out"))?
}

/// Compares complete retained ownership before emitting readiness or raw data.
fn validate_request(
    request: &Request,
    handle: &FrontendHandle,
    session: &SessionSummary,
) -> Result<()> {
    if request.protocol != "mez-outbound-x11/2"
        || request.handle != *handle
        || request.session != *session
    {
        return Err(MezError::conflict("outbound X11 handoff ownership changed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
