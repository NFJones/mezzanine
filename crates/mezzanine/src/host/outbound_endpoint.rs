//! One protected outbound endpoint with bounded, independent connection leases.
//!
//! This transport resource owns the persistent client key lock, never returning
//! key material to consumers. It does not resolve profiles, pair principals,
//! initialize application authority, expose IPC, or create sessions. A future
//! frontend broker must perform those operations under their existing owners.
//! Each connection attempt consumes one finite slot, including while connecting.
//! Dropping a lease closes only that connection. Endpoint shutdown is permitted
//! only after all sibling owners and leases have retired. Shutdown retains its
//! original future across timed-out or cancelled waits. Abandoned ownership
//! quarantines the identity lock until process exit, never assuming dependency
//! Drop or a closing flag proves transport teardown. No cleanup task is detached.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error::{MezError, MezErrorKind, Result};
use crate::runtime::{RuntimeIrohTransportPolicy, bind_runtime_iroh_client_endpoint};
use crate::security::remote::RemoteClientIdentity;

/// Shared transport owner; cloning never binds another endpoint or reads a key.
#[derive(Clone)]
pub(crate) struct OutboundEndpointOwner {
    inner: Arc<EndpointResource>,
}

/// Every connection lease retains this complete resource. Only completed
/// shutdown releases identity ownership; all other drops quarantine it.
struct EndpointResource {
    endpoint: iroh::Endpoint,
    identity: Option<RemoteClientIdentity>,
    slots: Arc<Semaphore>,
    setup_timeout: Duration,
}

impl Drop for EndpointResource {
    fn drop(&mut self) {
        if let Some(identity) = self.identity.take() {
            identity.quarantine_until_process_exit();
        }
    }
}

/// Exclusive shutdown ownership, separate from cancellable deadline waits.
/// An incomplete drop withholds the lock until process exit. Retrying finish
/// polls the original future, not another close call on an already-closing endpoint.
pub(crate) struct OutboundEndpointShutdown {
    work: Option<futures_util::future::BoxFuture<'static, ()>>,
    resource: Option<EndpointResource>,
}

impl OutboundEndpointShutdown {
    /// Waits under one bounded deadline without discarding shutdown ownership.
    /// Timeout/cancellation leaves this guard eligible for another wait.
    pub(crate) async fn finish(&mut self) -> Result<()> {
        let Some(resource) = self.resource.as_ref() else {
            return Ok(());
        };
        let work = self.work.as_mut().ok_or_else(|| {
            MezError::invalid_state("outbound endpoint shutdown ownership unavailable")
        })?;
        tokio::time::timeout(resource.setup_timeout, work)
            .await
            .map_err(|_| MezError::invalid_state("outbound endpoint shutdown timed out"))?;
        // Close completion awaits the dependency's tracked runtime shutdown.
        // Dispose both the completed future's clone and the resource endpoint
        // before releasing the lock. Neither field order nor is_closing proves
        // completion, and no owner-held UDP socket may outlive lock release.
        drop(self.work.take());
        if let Some(mut resource) = self.resource.take() {
            let identity = resource.identity.take();
            drop(resource);
            drop(identity);
        }
        Ok(())
    }
}

/// One separately initialized connection, retaining endpoint and slot ownership.
/// Consumers may borrow its transport but cannot close the shared endpoint.
pub(crate) struct OutboundConnectionLease {
    connection: iroh::endpoint::Connection,
    _resource: Arc<EndpointResource>,
    _slot: OwnedSemaphorePermit,
}

impl OutboundEndpointOwner {
    /// Binds one client-only endpoint while holding the existing exclusive lock.
    /// Disabled outbound policy and invalid finite budgets reject before key I/O.
    pub(crate) async fn bind(
        config_root: &Path,
        policy: &RuntimeIrohTransportPolicy,
    ) -> Result<Self> {
        if !policy.outbound_enabled {
            return Err(MezError::forbidden("Iroh outbound transport is disabled"));
        }
        if !(1..=1024).contains(&policy.max_connections)
            || !(Duration::from_millis(100)..=Duration::from_secs(120))
                .contains(&policy.setup_timeout)
        {
            return Err(MezError::invalid_args(
                "outbound endpoint budget unavailable",
            ));
        }
        let identity = RemoteClientIdentity::load_or_create(config_root)?;
        let key = identity.secret_key().clone();
        // Arm fail-closed ownership before entering cancellable transport bind.
        let mut identity = BindIdentityGuard(Some(identity));
        let endpoint = bind_runtime_iroh_client_endpoint(policy, key).await?;
        Ok(Self {
            inner: Arc::new(EndpointResource {
                endpoint,
                identity: identity.0.take(),
                slots: Arc::new(Semaphore::new(policy.max_connections)),
                setup_timeout: policy.setup_timeout,
            }),
        })
    }

    /// Returns public transport identity, never device or endpoint credentials.
    pub(crate) fn endpoint_id(&self) -> iroh::EndpointId {
        self.inner.endpoint.id()
    }

    /// Admits a fresh independent connection without waiting on a full queue.
    /// The caller owns application authentication and exact target selection.
    /// Cancellation releases the attempt slot; errors omit endpoint addresses.
    pub(crate) async fn connect(
        &self,
        target: iroh::EndpointAddr,
        alpn: &[u8],
    ) -> Result<OutboundConnectionLease> {
        let slot = self.inner.slots.clone().try_acquire_owned().map_err(|_| {
            MezError::new(
                MezErrorKind::RateLimited,
                "outbound connection capacity unavailable",
            )
        })?;
        let connection = tokio::time::timeout(
            self.inner.setup_timeout,
            self.inner.endpoint.connect(target, alpn),
        )
        .await
        .map_err(|_| MezError::invalid_state("outbound connection setup timed out"))?
        .map_err(|_| MezError::invalid_state("outbound connection setup unavailable"))?;
        Ok(OutboundConnectionLease {
            connection,
            _resource: self.inner.clone(),
            _slot: slot,
        })
    }

    /// Transfers exclusive, connection-free ownership into a shutdown guard.
    /// Busy rejection does not close sibling connections. This synchronous
    /// transition leaves no cancellation window before the guard owns teardown.
    pub(crate) fn begin_shutdown(self) -> Result<OutboundEndpointShutdown> {
        let resource = Arc::try_unwrap(self.inner)
            .map_err(|_| MezError::conflict("outbound endpoint still has live owners"))?;
        let endpoint = resource.endpoint.clone();
        Ok(OutboundEndpointShutdown {
            work: Some(Box::pin(async move { endpoint.close().await })),
            resource: Some(resource),
        })
    }
}

/// A cancelled or failed bind may have started dependency-owned tasks. Withhold
/// the protected lock unless ownership transfers into the completed resource.
struct BindIdentityGuard(Option<RemoteClientIdentity>);

impl Drop for BindIdentityGuard {
    fn drop(&mut self) {
        if let Some(identity) = self.0.take() {
            identity.quarantine_until_process_exit();
        }
    }
}

impl OutboundConnectionLease {
    /// Borrows the connection for its separately owned control/event workers.
    /// Dropping this lease closes any cloned handles to this same connection.
    pub(crate) fn connection(&self) -> &iroh::endpoint::Connection {
        &self.connection
    }
}

impl Drop for OutboundConnectionLease {
    fn drop(&mut self) {
        self.connection
            .close(iroh::endpoint::VarInt::from_u32(0), b"frontend retired");
    }
}

#[cfg(test)]
mod tests;
