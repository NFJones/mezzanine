//! Session-owned composition of dedicated local handoff and authenticated X11 relay.
//!
//! The retained session supplies exact local identity, negotiated fake credential,
//! immutable codec and route. Checked occurrence allocation happens before awaits,
//! so a failed/cancelled attempt cannot reuse its label. Local UID/handshake and
//! remote preface admission complete under one configured deadline; setup delivery
//! uses only its remaining budget. Application relay remains caller-owned until
//! EOF, route stop or cancellation. No listener, task, real credential or ordinary
//! CLI activation is introduced. The mutable borrow retains parent-session lifetime
//! throughout this staged exchange, deliberately serializing its caller.

use super::*;
use crate::host::outbound_frontend::client::SessionSummary;
use std::os::fd::AsRawFd;

impl InitializedSessionFrontend {
    /// Relays one already-connected dedicated frontend stream using this session's
    /// admitted offer/codec only. Callers must bound accepted streams and announce
    /// the next occurrence on their authenticated control path before invoking this
    /// staged operation. Failures consume the occurrence and stream, never replay
    /// bytes; they do not grant another connection or change session authority.
    pub(in crate::host::outbound_frontend::setup::connection::initialize::session) async fn relay_x11_frontend(
        &mut self,
        stream: tokio::net::UnixStream,
    ) -> Result<()> {
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
        let uid = crate::runtime::current_effective_uid();
        crate::runtime::authenticated_unix_peer_uid(stream.as_raw_fd(), uid)?;
        let summary: SessionSummary = serde_json::from_value(self.summary.clone())
            .map_err(|_| MezError::invalid_state("outbound retained X11 session invalid"))?;
        let occurrence = self
            .x11_occurrence
            .checked_add(1)
            .ok_or_else(|| MezError::conflict("outbound X11 occurrence exhausted"))?;
        self.x11_occurrence = occurrence;
        let budget = endpoint.transport_policy().x11.setup_timeout;
        let deadline = tokio::time::Instant::now() + budget;
        let (frontend, channel) = tokio::time::timeout_at(deadline, async {
            let frontend = handoff::authenticate_frontend(
                stream,
                uid,
                &self.connected.prepared.frontend.handle,
                &summary,
                occurrence,
                budget,
            )
            .await?;
            endpoint.frontend_config_root()?;
            let channel = accept_channel(
                endpoint,
                self.connected.connection.connection(),
                route,
                self.x11_slots.clone(),
                budget,
            )
            .await?;
            Ok::<_, MezError>((frontend, channel))
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound X11 frontend admission timed out"))??;
        // Preserve the original deadline, including valid sub-minimum remaining
        // time. Once setup commits, application lifetime remains caller-owned.
        channel
            .relay_until(frontend, self.connected.compression, cookie, deadline)
            .await
    }
}
