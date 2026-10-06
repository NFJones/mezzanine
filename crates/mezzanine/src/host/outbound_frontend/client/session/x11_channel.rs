//! Protected dedicated X11 channel acquisition with exact retained identities.
//!
//! A session-derived opener retains root/control-socket discovery and dedicated
//! socket identity. Kernel UID and a closed version-two ready reply authenticate
//! the local peer before raw bytes are exposed. Read-ahead survives the transition;
//! finite nonwaiting permits cover setup and channel lifetime. No Iroh endpoint,
//! credential, remote request, reconnect or replay is introduced. The attachment
//! supervisor must own/dispose openers and channels when its parent session ends.

use super::*;
use std::os::fd::AsRawFd;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::bytes::BytesMut;

/// Session-scoped immutable discovery evidence and finite channel admission.
/// Capturing it does not grant independent session or remote authority.
pub(crate) struct X11ChannelOpener {
    discovery: Discovery,
    name: String,
    device: u64,
    inode: u64,
    handle: FrontendHandle,
    session: SessionSummary,
    slots: Arc<Semaphore>,
}

/// Raw dedicated stream retaining readiness read-ahead and its capacity permit.
/// It deliberately has no Debug implementation or credential-bearing fields.
pub(crate) struct X11Channel {
    stream: tokio::net::UnixStream,
    pending: BytesMut,
    occurrence: u64,
    _slot: OwnedSemaphorePermit,
}

/// Closed ready evidence excluding remote route proof and arbitrary metadata.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    protocol: String,
    handle: FrontendHandle,
    session: SessionSummary,
    occurrence: u64,
    ready: bool,
}

impl OutboundSessionClient {
    /// Captures the announced basename under this exact retained private root.
    /// Only primary sessions can open channels; callers separately qualify X11
    /// negotiation and own attachment shutdown. No wire traffic occurs here.
    pub(crate) fn x11_channel_opener(&self, name: &str, limit: usize) -> Result<X11ChannelOpener> {
        if self.summary.granted_role != "primary" {
            return Err(MezError::forbidden("outbound X11 channel requires primary"));
        }
        let original = &self.client.discovery;
        original.validate()?;
        let discovery = Discovery {
            path: original.path.clone(),
            root: original.root.try_clone()?,
            device: original.device,
            inode: original.inode,
            uid: original.uid,
        };
        X11ChannelOpener::capture(
            discovery,
            name,
            self.client.handle.clone(),
            self.summary.clone(),
            limit,
        )
    }
}

impl X11ChannelOpener {
    /// Captures exact dedicated socket evidence without repairing discovery.
    fn capture(
        discovery: Discovery,
        name: &str,
        handle: FrontendHandle,
        session: SessionSummary,
        limit: usize,
    ) -> Result<Self> {
        if !(1..=1024).contains(&limit) {
            return Err(MezError::invalid_args(
                "outbound X11 client capacity invalid",
            ));
        }
        crate::host::outbound_frontend::x11_discovery::validate_socket_name(name)?;
        discovery.validate()?;
        let socket = statat(&discovery.root, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(std::io::Error::from)?;
        let opener = Self {
            discovery,
            name: name.to_string(),
            device: socket.st_dev,
            inode: socket.st_ino,
            handle,
            session,
            slots: Arc::new(Semaphore::new(limit)),
        };
        opener.validate()?;
        Ok(opener)
    }

    /// Revalidates private policy and exact root/control/dedicated socket objects.
    /// These cooperative check/use checks are not hostile same-UID rename defense.
    fn validate(&self) -> Result<()> {
        self.discovery.validate()?;
        let socket = statat(
            &self.discovery.root,
            self.name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(std::io::Error::from)?;
        if FileType::from_raw_mode(socket.st_mode) != FileType::Socket
            || socket.st_uid != self.discovery.uid
            || socket.st_mode & 0o077 != 0
        {
            return Err(MezError::forbidden(
                "outbound X11 dedicated socket must remain private",
            ));
        }
        if socket.st_dev != self.device || socket.st_ino != self.inode {
            return Err(MezError::conflict("outbound X11 dedicated socket changed"));
        }
        Ok(())
    }

    /// Opens one dedicated channel under a total setup deadline. Requests do not
    /// choose occurrences. Errors release the slot and stream with no retry;
    /// success retains every raw byte buffered after the validated ready frame.
    pub(crate) async fn open(&self, budget: Duration) -> Result<X11Channel> {
        validate_budget(1, 1, budget)?;
        self.validate()?;
        let slot = self.slots.clone().try_acquire_owned().map_err(|_| {
            MezError::new(
                MezErrorKind::RateLimited,
                "outbound X11 client capacity unavailable",
            )
        })?;
        tokio::time::timeout(budget, async move {
            let stream = tokio::net::UnixStream::connect(self.discovery.path.join(&self.name))
                .await
                .map_err(|_| {
                    MezError::invalid_state("outbound X11 dedicated connection unavailable")
                })?;
            crate::runtime::authenticated_unix_peer_uid(stream.as_raw_fd(), self.discovery.uid)?;
            self.validate()?;
            let mut framed = Framed::new(stream, ProtocolFrameCodec::new(HELLO_LIMIT)?);
            framed
                .send(ProtocolFrame::new(
                    CONTENT_TYPE,
                    serde_json::json!({
                        "protocol":"mez-outbound-x11/2","handle":self.handle,"session":self.session
                    })
                    .to_string(),
                ))
                .await?;
            let frame = framed
                .next()
                .await
                .transpose()?
                .ok_or_else(|| MezError::invalid_state("outbound X11 readiness unavailable"))?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_state(
                    "outbound X11 readiness type invalid",
                ));
            }
            let ready: Ready = serde_json::from_str(&frame.body)
                .map_err(|_| MezError::invalid_state("outbound X11 readiness invalid"))?;
            validate_ready(&ready, &self.handle, &self.session)?;
            self.validate()?;
            let parts = framed.into_parts();
            if !parts.write_buf.is_empty() {
                return Err(MezError::invalid_state("outbound X11 request not flushed"));
            }
            Ok(X11Channel {
                stream: parts.io,
                pending: parts.read_buf,
                occurrence: ready.occurrence,
                _slot: slot,
            })
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound X11 channel opening timed out"))?
    }
}

/// Requires closed, positive broker-assigned settlement for this exact owner.
fn validate_ready(ready: &Ready, handle: &FrontendHandle, session: &SessionSummary) -> Result<()> {
    if ready.protocol != "mez-outbound-x11/2"
        || ready.handle != *handle
        || ready.session != *session
        || ready.occurrence == 0
        || !ready.ready
    {
        return Err(MezError::conflict(
            "outbound X11 readiness ownership changed",
        ));
    }
    Ok(())
}

impl X11Channel {
    /// Reports inert broker-assigned occurrence evidence, never route authority.
    pub(crate) fn occurrence(&self) -> u64 {
        self.occurrence
    }
}

impl AsyncRead for X11Channel {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if !this.pending.is_empty() {
            let count = buf.remaining().min(this.pending.len());
            buf.put_slice(&this.pending.split_to(count));
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for X11Channel {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, bytes)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests;
