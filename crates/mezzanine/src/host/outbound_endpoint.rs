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

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
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
    root: ConfigRootIdentity,
    slots: Arc<Semaphore>,
    setup_timeout: Duration,
    policy: RuntimeIrohTransportPolicy,
}

/// Retained native directory identity, not a caller-authored routing label.
/// A relocated/replaced root cannot publish a listener for the old key owner.
struct ConfigRootIdentity {
    path: PathBuf,
    directory: std::fs::File,
}

impl ConfigRootIdentity {
    /// Opens a private root without following a final symlink and retains it.
    fn capture(path: &Path) -> Result<Self> {
        crate::runtime::ensure_private_socket_directory(
            path,
            crate::runtime::current_effective_uid(),
        )?;
        let path = std::fs::canonicalize(path)?;
        let descriptor = rustix::fs::open(
            &path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        let root = Self {
            path,
            directory: descriptor.into(),
        };
        root.validate()?;
        Ok(root)
    }

    /// Checks both physical object identity and current private-root policy.
    fn validate(&self) -> Result<()> {
        let retained = self.directory.metadata()?;
        let current = std::fs::symlink_metadata(&self.path)?;
        if !current.is_dir()
            || current.file_type().is_symlink()
            || current.uid() != crate::runtime::current_effective_uid()
            || current.mode() & 0o077 != 0
        {
            return Err(MezError::forbidden(
                "outbound configuration root must remain private",
            ));
        }
        if retained.dev() != current.dev() || retained.ino() != current.ino() {
            return Err(MezError::conflict("outbound configuration root changed"));
        }
        Ok(())
    }
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
        let root = ConfigRootIdentity::capture(config_root)?;
        let identity = RemoteClientIdentity::load_or_create(&root.path)?;
        root.validate()?;
        let key = identity.secret_key().clone();
        // Arm fail-closed ownership before entering cancellable transport bind.
        let mut identity = BindIdentityGuard(Some(identity));
        let endpoint = bind_runtime_iroh_client_endpoint(policy, key).await?;
        Ok(Self {
            inner: Arc::new(EndpointResource {
                endpoint,
                identity: identity.0.take(),
                root,
                slots: Arc::new(Semaphore::new(policy.max_connections)),
                setup_timeout: policy.setup_timeout,
                policy: policy.clone(),
            }),
        })
    }

    /// Returns public transport identity, never device or endpoint credentials.
    pub(crate) fn endpoint_id(&self) -> iroh::EndpointId {
        self.inner.endpoint.id()
    }

    /// Returns the immutable network/framing policy of this exact endpoint.
    /// Consumers cannot silently rebind or broaden it for a different target.
    pub(crate) fn transport_policy(&self) -> &RuntimeIrohTransportPolicy {
        &self.inner.policy
    }

    /// Returns the validated retained root for owner-private frontend discovery.
    /// This is not transport authority and never exposes identity credentials.
    pub(crate) fn frontend_config_root(&self) -> Result<&Path> {
        self.inner.root.validate()?;
        Ok(&self.inner.root.path)
    }

    /// Clones the retained root descriptor for directory-relative publication
    /// inspection and cleanup. This does not grant remote transport authority.
    pub(crate) fn frontend_root_directory(&self) -> Result<std::fs::File> {
        self.inner.root.validate()?;
        Ok(self.inner.root.directory.try_clone()?)
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
