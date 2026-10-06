//! Owned, bounded X11 relay supervision alongside exact-session control delivery.
//!
//! The consumed control future is never cancelled and reused when a channel is
//! accepted or finishes. Every channel future stays in a finite owned collection;
//! no detached tasks, reconnects or mutation replay are introduced. Cancellation,
//! parent closure or control failure drops all owned work and socket publication.
//! The caller must announce the dedicated listener and occurrence contract over
//! authenticated control before invoking this staged supervisor. Ordinary listener
//! dispatch and CLI X11 activation remain gated outside this component.

use super::listener::X11FrontendListener;
use super::*;
use futures_util::stream::FuturesUnordered;
use std::future::Future;
use std::pin::Pin;

impl InitializedSessionFrontend {
    /// Publishes a dedicated socket for this admitted route and owns supervision
    /// until control retirement or failure. The outer frontend pipeline supplies
    /// cancellation by dropping this future; no task or reconnect is introduced.
    pub(crate) async fn serve_x11(self) -> Result<()> {
        self.x11_relay_source()?;
        let endpoint = &self.connected.prepared.frontend._endpoint;
        let listener = X11FrontendListener::bind(
            endpoint.clone(),
            endpoint.transport_policy().x11.max_connections_per_route,
        )?;
        Box::pin(self.supervise_x11(listener, std::future::pending())).await
    }

    /// Supervises an already published dedicated listener while servicing control.
    /// Channel failures retire only that channel. Control failure or cancellation
    /// retires the entire exact session, including pending handshakes and relays.
    /// The owned listener is removed on every return or future abandonment.
    pub(super) async fn supervise_x11<C>(
        mut self,
        listener: X11FrontendListener,
        cancellation: C,
    ) -> Result<()>
    where
        C: Future<Output = ()>,
    {
        let source = self.x11_relay_source()?;
        let parent = self.connected.connection.connection().clone();
        let path = listener.socket_path()?;
        if path.parent()
            != Some(
                self.connected
                    .prepared
                    .frontend
                    ._endpoint
                    .frontend_config_root()?,
            )
        {
            return Err(MezError::conflict("outbound X11 listener root changed"));
        }
        let name = path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| MezError::invalid_state("outbound X11 listener name unavailable"))?;
        crate::host::outbound_frontend::x11_discovery::validate_socket_name(name)?;
        self.x11_socket_name = Some(name.to_string());
        let limit = self
            .connected
            .prepared
            .frontend
            ._endpoint
            .transport_policy()
            .x11
            .max_connections_per_route;
        if !(1..=1024).contains(&limit) {
            return Err(MezError::invalid_args(
                "outbound X11 supervision capacity invalid",
            ));
        }
        let mut channels: FuturesUnordered<Pin<Box<dyn Future<Output = Result<()>>>>> =
            FuturesUnordered::new();
        let mut control = Box::pin(self.deliver_view());
        // Keep accepted streams and capacity waits owned across control replies.
        // Reconstructing this future on every select would discard pending peers.
        let admission = || async {
            let Some(accepted) = listener.accept_waiting().await? else {
                return Ok::<_, MezError>(None);
            };
            let reservation = source.reserve_waiting().await?;
            Ok(Some((accepted, reservation)))
        };
        let mut pending_admission = Box::pin(admission());
        tokio::pin!(cancellation);
        loop {
            tokio::select! {
                biased;
                () = &mut cancellation => return Ok(()),
                _ = parent.closed() => return Err(MezError::invalid_state("outbound X11 parent connection retired")),
                result = &mut control => {
                    let session = result?;
                    if session.is_detached() { return Ok(()); }
                    control = Box::pin(session.deliver_view());
                }
                _ = channels.next(), if !channels.is_empty() => {},
                accepted = &mut pending_admission, if channels.len() < limit => {
                    if let Some((accepted, reservation)) = accepted? {
                        channels.push(Box::pin(accepted.relay_reserved(reservation)));
                    }
                    pending_admission = Box::pin(admission());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
