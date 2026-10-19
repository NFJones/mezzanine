//! Authenticated, bounded local frontend admission for one outbound endpoint.
//!
//! The caller owns private listener publication and startup election. This owner
//! receives already accepted Unix streams, verifies kernel peer UID before any
//! protocol decoding, and admits only the versioned content-free hello. Handles
//! identify a broker incarnation and occurrence, never endpoint/device authority.
//! Each handshake and admitted frontend consumes one finite slot and retains the
//! endpoint resource. No remote connect, pairing, session creation, terminal input,
//! credential export or detached task occurs here. Those operations require the
//! later broker's independently validated request/stream ownership contracts.

use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::codec::Framed;

use super::outbound_endpoint::OutboundEndpointOwner;
use crate::error::{MezError, MezErrorKind, Result};
use crate::protocol::framing::{ProtocolFrame, ProtocolFrameCodec};

/// Exact epoch for the entire local request/response contract, not a product
/// version. Bump whenever an emitted field or semantic requirement can break an
/// existing strict consumer, including optional fields such as if_view_identity.
/// There is deliberately no older-epoch implementation or feature fallback.
const PROTOCOL: &str = "mez-outbound/2";
const CONTENT_TYPE: &str = "application/vnd.mezzanine.outbound+json";
const HELLO_LIMIT: usize = 4096;
/// Static recovery advice must not reflect peer content or authorize takeover.
const WIRE_RECOVERY: &str = "inspect the broker lifecycle owner and active sibling attachments; upgrade client and broker together and coordinate explicit retirement before retry; do not replay uncertain operations";

pub(crate) mod client;
mod clipboard_wire;
mod events;
mod killing;
mod listener;
mod listing;
pub(crate) mod routes;
mod setup;
mod x11_discovery;
#[allow(
    unused_imports,
    reason = "CLI broker integration follows listener publication qualification"
)]
pub(crate) use listener::OutboundFrontendListener;

/// Content-free protocol negotiation; extra fields cannot smuggle operations.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    protocol: String,
}

/// Validates the complete content-free contract before handle allocation or any
/// setup/profile/application work. Callers authenticate the peer and root first.
fn validate_hello(frame: &ProtocolFrame) -> Result<()> {
    if frame.content_type != CONTENT_TYPE {
        return Err(MezError::invalid_args(
            "outbound frontend content type unsupported",
        ));
    }
    let hello: Hello = serde_json::from_str(&frame.body)
        .map_err(|_| MezError::invalid_args("outbound frontend hello invalid"))?;
    if hello.protocol != PROTOCOL {
        return Err(MezError::invalid_args(format!(
            "outbound frontend wire contract unsupported; {WIRE_RECOVERY}"
        )));
    }
    Ok(())
}

/// Inert identity for one admitted frontend, scoped to this broker incarnation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrontendHandle {
    owner: String,
    generation: u64,
}

/// Finite local admission owner; independent attempts need not wait on siblings.
pub(crate) struct OutboundFrontendAdmission {
    endpoint: OutboundEndpointOwner,
    incarnation: String,
    next_generation: AtomicU64,
    slots: Arc<Semaphore>,
    deadline: Duration,
    owner_uid: u32,
}

/// One authenticated local stream retaining endpoint and capacity ownership.
/// Disposal closes only this local stream and releases its slot, not siblings.
pub(crate) struct AdmittedFrontend {
    handle: FrontendHandle,
    stream: Framed<tokio::net::UnixStream, ProtocolFrameCodec>,
    _endpoint: OutboundEndpointOwner,
    _slot: Arc<OwnedSemaphorePermit>,
}

impl OutboundFrontendAdmission {
    /// Constructs bounded admission for a currently valid configuration root.
    /// Invalid limits reject before any stream is accepted or handshake starts.
    pub(crate) fn new(
        endpoint: OutboundEndpointOwner,
        limit: usize,
        deadline: Duration,
    ) -> Result<Self> {
        if !(1..=1024).contains(&limit)
            || !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&deadline)
        {
            return Err(MezError::invalid_args(
                "outbound frontend budget unavailable",
            ));
        }
        endpoint.frontend_config_root()?;
        Ok(Self {
            endpoint,
            incarnation: format!("{:032x}", rand::random::<u128>()),
            next_generation: AtomicU64::new(0),
            slots: Arc::new(Semaphore::new(limit)),
            deadline,
            owner_uid: crate::runtime::current_effective_uid(),
        })
    }

    /// Authenticates before decoding one strict hello under a total deadline.
    /// Capacity rejection is nonwaiting. Cancellation/failure releases this
    /// attempt's local stream and permit; generation values are never reused.
    pub(crate) async fn admit(&self, stream: tokio::net::UnixStream) -> Result<AdmittedFrontend> {
        crate::runtime::authenticated_unix_peer_uid(stream.as_raw_fd(), self.owner_uid)?;
        self.endpoint.frontend_config_root()?;
        let slot = self.slots.clone().try_acquire_owned().map_err(|_| {
            MezError::new(
                MezErrorKind::RateLimited,
                "outbound frontend capacity unavailable",
            )
        })?;
        let mut stream = Framed::new(stream, ProtocolFrameCodec::new(HELLO_LIMIT)?);
        let handle = tokio::time::timeout(self.deadline, async {
            let frame =
                stream.next().await.transpose()?.ok_or_else(|| {
                    MezError::invalid_state("outbound frontend hello unavailable")
                })?;
            validate_hello(&frame)?;
            // Root may have changed while this peer supplied its hello.
            self.endpoint.frontend_config_root()?;
            // Identity allocation publishes no other state. Retry from the
            // observed value; exhaustion must never wrap or reuse a generation.
            let mut previous = self.next_generation.load(Ordering::Relaxed);
            loop {
                let next = previous.checked_add(1).ok_or_else(|| {
                    MezError::invalid_state("outbound frontend generation exhausted")
                })?;
                match self.next_generation.compare_exchange_weak(
                    previous,
                    next,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(current) => previous = current,
                }
            }
            let handle = FrontendHandle {
                owner: self.incarnation.clone(),
                generation: previous + 1,
            };
            let body = serde_json::json!({"protocol":PROTOCOL,"handle":handle}).to_string();
            stream.send(ProtocolFrame::new(CONTENT_TYPE, body)).await?;
            Ok(handle)
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound frontend admission timed out"))??;
        Ok(AdmittedFrontend {
            handle,
            stream,
            _endpoint: self.endpoint.clone(),
            _slot: Arc::new(slot),
        })
    }

    /// Checks that a handle matches its still-retained local stream owner.
    /// Matching values grant no remote application or lifecycle authority.
    pub(crate) fn validate_handle(
        &self,
        frontend: &AdmittedFrontend,
        handle: &FrontendHandle,
    ) -> Result<()> {
        if frontend.handle != *handle || handle.owner != self.incarnation {
            return Err(MezError::conflict("outbound frontend handle changed"));
        }
        self.endpoint.frontend_config_root()?;
        Ok(())
    }
}

impl AdmittedFrontend {
    /// Returns the inert exact handle, never endpoint/device credentials.
    pub(crate) fn handle(&self) -> &FrontendHandle {
        &self.handle
    }
}

#[cfg(test)]
mod tests;
