//! Owned reservation of dedicated local handoff and authenticated X11 relay.
//!
//! The session supplies identity, fake credential, codec and route synchronously.
//! Reservation holds finite capacity and endpoint lifetime, consumes a checked
//! occurrence, then releases the session borrow. Execution remains an owned future,
//! not a detached task. Parent lease disposal closes the cloned connection and
//! cancels local handoff/relay. One absolute deadline covers setup only; established
//! application relay ends on EOF, route stop or caller cancellation. No listener,
//! real credential or ordinary CLI activation is introduced here.

use super::*;
use crate::host::outbound_frontend::client::SessionSummary;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU64, Ordering};

/// Session-scoped reservation evidence that can coexist with consumed control
/// requests. Sources share capacity and occurrence allocation, not independent
/// connection leases; parent retirement remains authoritative for all of them.
pub(in crate::host::outbound_frontend::setup::connection::initialize::session) struct X11RelaySource
{
    endpoint: OutboundEndpointOwner,
    connection: iroh::endpoint::Connection,
    route: X11ForwardingResult,
    cookie: crate::runtime::x11::X11Cookie,
    compression: IrohCompressionPolicy,
    handle: FrontendHandle,
    summary: SessionSummary,
    slots: Arc<Semaphore>,
    occurrence: Arc<AtomicU64>,
}

/// Immutable exact-session evidence plus one nonwaiting channel-capacity permit.
/// No independent connection lease is created: the parent still owns retirement.
pub(in crate::host::outbound_frontend::setup::connection::initialize::session) struct X11RelayReservation
{
    endpoint: OutboundEndpointOwner,
    connection: iroh::endpoint::Connection,
    route: X11ForwardingResult,
    cookie: crate::runtime::x11::X11Cookie,
    compression: IrohCompressionPolicy,
    handle: FrontendHandle,
    summary: SessionSummary,
    occurrence: u64,
    slot: OwnedSemaphorePermit,
    budget: Duration,
    deadline: tokio::time::Instant,
}

impl InitializedSessionFrontend {
    /// Reserves exact evidence and capacity before awaits, releasing the session
    /// borrow on return. Failed capacity/exhaustion checks consume no occurrence;
    /// a returned reservation consumes its occurrence even if never executed.
    pub(in crate::host::outbound_frontend::setup::connection::initialize::session) fn reserve_x11_frontend(
        &mut self,
    ) -> Result<X11RelayReservation> {
        self.x11_relay_source()?.reserve()
    }

    /// Captures immutable admission evidence without reserving a slot or advancing
    /// an occurrence. All sources use the same session-owned allocator and pool.
    pub(in crate::host::outbound_frontend::setup::connection::initialize::session) fn x11_relay_source(
        &self,
    ) -> Result<X11RelaySource> {
        if self.detached {
            return Err(MezError::conflict("outbound X11 session is detached"));
        }
        let route = self
            .x11_route
            .as_ref()
            .ok_or_else(|| MezError::forbidden("outbound X11 route was not negotiated"))?;
        let cookie = self
            .x11_cookie
            .as_ref()
            .ok_or_else(|| MezError::forbidden("outbound X11 offer credential unavailable"))?;
        let endpoint = &self.connected.prepared.frontend._endpoint;
        endpoint.frontend_config_root()?;
        let connection = self.connected.connection.connection();
        if connection.close_reason().is_some() {
            return Err(MezError::invalid_state(
                "outbound X11 parent connection retired",
            ));
        }
        let summary: SessionSummary = serde_json::from_value(self.summary.clone())
            .map_err(|_| MezError::invalid_state("outbound retained X11 session invalid"))?;
        Ok(X11RelaySource {
            endpoint: endpoint.clone(),
            connection: connection.clone(),
            route: route.clone(),
            cookie: cookie.clone(),
            compression: self.connected.compression,
            handle: self.connected.prepared.frontend.handle.clone(),
            summary,
            slots: self.x11_slots.clone(),
            occurrence: self.x11_occurrence.clone(),
        })
    }

    /// Compatibility composition for serialized internal callers. Supervision
    /// can instead retain the reservation and drive it alongside control work.
    pub(in crate::host::outbound_frontend::setup::connection::initialize::session) async fn relay_x11_frontend(
        &mut self,
        stream: tokio::net::UnixStream,
    ) -> Result<()> {
        self.reserve_x11_frontend()?.relay(stream).await
    }
}

impl X11RelaySource {
    /// Reserves capacity nonwaitingly and allocates one checked occurrence. No
    /// async work starts here. Failure releases any acquired permit; successful
    /// reservations consume their identity even if cancelled before execution.
    pub(in crate::host::outbound_frontend::setup::connection::initialize::session) fn reserve(
        &self,
    ) -> Result<X11RelayReservation> {
        self.endpoint.frontend_config_root()?;
        if self.connection.close_reason().is_some() {
            return Err(MezError::invalid_state(
                "outbound X11 parent connection retired",
            ));
        }
        let budget = self.endpoint.transport_policy().x11.setup_timeout;
        if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&budget) {
            return Err(MezError::invalid_args(
                "outbound X11 channel deadline invalid",
            ));
        }
        let slot = self.slots.clone().try_acquire_owned().map_err(|_| {
            MezError::new(
                MezErrorKind::RateLimited,
                "outbound X11 channel capacity unavailable",
            )
        })?;
        let occurrence = self
            .occurrence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| MezError::conflict("outbound X11 occurrence exhausted"))?
            + 1;
        let reserved = X11RelayReservation {
            endpoint: self.endpoint.clone(),
            connection: self.connection.clone(),
            route: self.route.clone(),
            cookie: self.cookie.clone(),
            compression: self.compression,
            handle: self.handle.clone(),
            summary: self.summary.clone(),
            occurrence,
            slot,
            budget,
            deadline: tokio::time::Instant::now() + budget,
        };
        Ok(reserved)
    }
}

impl X11RelayReservation {
    /// Executes once on a dedicated stream; cancellation drops the stream and
    /// reservation/accepted-channel permit. Parent closure wins over further
    /// readiness or output. No failure returns reusable partial stream state.
    pub(in crate::host::outbound_frontend::setup::connection::initialize::session) async fn relay(
        self,
        stream: tokio::net::UnixStream,
    ) -> Result<()> {
        let parent = self.connection.clone();
        let operation = async move {
            self.endpoint.frontend_config_root()?;
            if self.deadline <= tokio::time::Instant::now() {
                return Err(MezError::invalid_state(
                    "outbound X11 frontend admission timed out",
                ));
            }
            let uid = crate::runtime::current_effective_uid();
            crate::runtime::authenticated_unix_peer_uid(stream.as_raw_fd(), uid)?;
            let (frontend, channel) = tokio::time::timeout_at(self.deadline, async {
                let frontend = handoff::authenticate_frontend(
                    stream,
                    uid,
                    &self.handle,
                    &self.summary,
                    self.occurrence,
                    self.budget,
                )
                .await?;
                self.endpoint.frontend_config_root()?;
                let channel = accept_reserved_channel(
                    self.endpoint,
                    &self.connection,
                    &self.route,
                    self.slot,
                    self.deadline,
                )
                .await?;
                Ok::<_, MezError>((frontend, channel))
            })
            .await
            .map_err(|_| MezError::invalid_state("outbound X11 frontend admission timed out"))??;
            channel
                .relay_until(frontend, self.compression, &self.cookie, self.deadline)
                .await
        };
        tokio::select! {
            biased;
            _ = parent.closed() => Err(MezError::invalid_state("outbound X11 parent connection retired")),
            result = operation => result,
        }
    }
}

#[cfg(test)]
mod tests;
