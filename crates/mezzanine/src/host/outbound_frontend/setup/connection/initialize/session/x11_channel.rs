//! Bounded server-opened X11 stream admission on an exact retained session.
//!
//! Validates the fixed route preface before exposing a channel. Pending acceptance
//! and active channels share a nonwaiting finite permit pool. Each channel retains
//! endpoint lifetime but no independent connection lease: parent-session disposal
//! closes the exact connection, including its channels. Incomplete Drop resets
//! only this stream pair; graceful completion preserves FIN tails. Neither closes
//! a sibling connection. No local X target, real credential,
//! clipboard/input authority or frontend IPC is introduced here. A consumed relay
//! validates the offered fake setup cookie and adapts direction-local codecs to a
//! caller-owned byte stream. The caller must independently bind that stream to the
//! admitted frontend and perform real-cookie substitution locally; this component
//! never selects a local X destination or owns its real credential.

use super::*;
use crate::runtime::x11::{X11ForwardingResult, X11StreamFailureStage, X11StreamPreface};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use zeroize::Zeroizing;

/// Authenticated stream pair holding its exact endpoint and finite channel slot.
/// Private fields prevent moving a stream out without its reset/lifetime owner.
pub(super) struct AuthenticatedX11Channel {
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    _endpoint: OutboundEndpointOwner,
    _slot: OwnedSemaphorePermit,
    /// Armed until both relay directions finish normally; FIN alone is not
    /// permission to abandon buffered bytes with a transport reset.
    graceful: bool,
}

mod frontend;
mod handoff;
mod listener;
mod relay;

impl InitializedSessionFrontend {
    /// Accepts one exact-route channel under the configured total setup deadline.
    /// Unnegotiated/detached owners reject before waiting. Cancellation releases
    /// pending/active ownership and permits no reconnect or stream replay.
    pub(super) async fn accept_x11_channel(&self) -> Result<AuthenticatedX11Channel> {
        if self.detached {
            return Err(MezError::conflict("outbound X11 session is detached"));
        }
        let route = self
            .x11_route
            .as_ref()
            .ok_or_else(|| MezError::forbidden("outbound X11 route was not negotiated"))?;
        let endpoint = &self.connected.prepared.frontend._endpoint;
        accept_channel(
            endpoint,
            self.connected.connection.connection(),
            route,
            self.x11_slots.clone(),
            endpoint.transport_policy().x11.setup_timeout,
        )
        .await
    }
}

/// Retains admission ownership before awaiting transport and authenticates one
/// raw preface without consuming subsequent setup/application bytes. All errors
/// omit route proof and payloads; failed accepted streams are reset by their guard.
async fn accept_channel(
    endpoint: &OutboundEndpointOwner,
    connection: &iroh::endpoint::Connection,
    route: &X11ForwardingResult,
    slots: Arc<Semaphore>,
    budget: Duration,
) -> Result<AuthenticatedX11Channel> {
    endpoint.frontend_config_root()?;
    if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&budget) {
        return Err(MezError::invalid_args(
            "outbound X11 channel deadline invalid",
        ));
    }
    let slot = slots.try_acquire_owned().map_err(|_| {
        MezError::new(
            MezErrorKind::RateLimited,
            "outbound X11 channel capacity unavailable",
        )
    })?;
    accept_reserved_channel(
        endpoint.clone(),
        connection,
        route,
        slot,
        tokio::time::Instant::now() + budget,
    )
    .await
}

/// Consumes a pre-acquired slot and original setup deadline. Reservation callers
/// must not acquire a second permit or restart the clock while awaiting a stream.
async fn accept_reserved_channel(
    endpoint: OutboundEndpointOwner,
    connection: &iroh::endpoint::Connection,
    route: &X11ForwardingResult,
    slot: OwnedSemaphorePermit,
    deadline: tokio::time::Instant,
) -> Result<AuthenticatedX11Channel> {
    endpoint.frontend_config_root()?;
    if deadline <= tokio::time::Instant::now() {
        return Err(MezError::invalid_state(
            "outbound X11 channel setup timed out",
        ));
    }
    tokio::time::timeout_at(deadline, async move {
        let (send, recv) = connection
            .accept_bi()
            .await
            .map_err(|_| MezError::invalid_state("outbound X11 channel unavailable"))?;
        let mut channel = AuthenticatedX11Channel {
            send,
            recv,
            _endpoint: endpoint,
            _slot: slot,
            graceful: false,
        };
        let mut bytes = Zeroizing::new([0_u8; crate::runtime::x11::X11_STREAM_PREFACE_BYTES]);
        channel
            .recv
            .read_exact(&mut *bytes)
            .await
            .map_err(|_| MezError::invalid_state("outbound X11 channel preface unavailable"))?;
        let preface = X11StreamPreface::decode(&*bytes)
            .map_err(|_| MezError::forbidden("outbound X11 channel preface invalid"))?;
        if preface.generation != route.generation || preface.route_token != route.route_token {
            return Err(MezError::forbidden("outbound X11 channel route mismatch"));
        }
        channel._endpoint.frontend_config_root()?;
        Ok(channel)
    })
    .await
    .map_err(|_| MezError::invalid_state("outbound X11 channel setup timed out"))?
}

impl Drop for AuthenticatedX11Channel {
    fn drop(&mut self) {
        if self.graceful {
            return;
        }
        let code = iroh::endpoint::VarInt::from_u32(
            X11StreamFailureStage::ClientRouteAuthentication.application_code(),
        );
        let _ = self.send.reset(code);
        let _ = self.recv.stop(code);
    }
}

#[cfg(test)]
mod tests;
