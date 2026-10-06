//! Protected publication and finite admission for dedicated local X11 streams.
//!
//! Publication retains the configuration-root descriptor and endpoint lifetime.
//! A fresh randomized pathname is never authority: kernel UID and the separate
//! exact-session handshake authenticate accepted peers. Cleanup checks the socket
//! object through the held directory and never deliberately unlinks a replacement.
//! Pathname bind/check/unlink are cooperative same-user boundaries, not atomic
//! protection against hostile renames. Pending accepts and admitted streams share
//! finite slots; no detached task or unbounded accepted-stream queue is created.
//! This staged owner does not activate ordinary supervisor or CLI forwarding.

use super::*;
use rustix::fs::{AtFlags, FileType, statat, unlinkat};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// Dedicated listener retaining publication, root and endpoint ownership.
pub(super) struct X11FrontendListener {
    listener: tokio::net::UnixListener,
    publication: Publication,
    endpoint: OutboundEndpointOwner,
    slots: Arc<Semaphore>,
}

/// Held-directory cleanup evidence installed before fallible chmod/conversion.
struct Publication {
    directory: std::fs::File,
    name: String,
    path: PathBuf,
    device: u64,
    inode: u64,
}

/// Accepted local stream retaining its slot and endpoint until actual disposal.
/// Private fields prohibit extracting the stream without its lifetime owner.
pub(super) struct AcceptedX11Frontend {
    stream: tokio::net::UnixStream,
    endpoint: OutboundEndpointOwner,
    slot: OwnedSemaphorePermit,
}

impl X11FrontendListener {
    /// Binds a fresh owner-only socket without probing/removing existing entries.
    /// Random naming prevents routine conflicts but grants no authentication.
    /// Invalid capacity rejects before filesystem publication.
    pub(super) fn bind(endpoint: OutboundEndpointOwner, limit: usize) -> Result<Self> {
        Self::bind_named(
            endpoint,
            limit,
            format!("x{:016x}.sock", rand::random::<u64>()),
        )
    }

    /// Shares exclusive pathname publication with deterministic test names.
    /// Existing entries always win; errors never repair an authored pathname.
    fn bind_named(endpoint: OutboundEndpointOwner, limit: usize, name: String) -> Result<Self> {
        if !(1..=1024).contains(&limit) {
            return Err(MezError::invalid_args(
                "outbound X11 frontend capacity invalid",
            ));
        }
        let directory = endpoint.frontend_root_directory()?;
        let path = crate::runtime::socket_path_for_name(endpoint.frontend_config_root()?, &name)?;
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        let socket = statat(&directory, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(std::io::Error::from)?;
        if FileType::from_raw_mode(socket.st_mode) != FileType::Socket {
            return Err(MezError::conflict("outbound X11 publication changed"));
        }
        let publication = Publication {
            directory,
            name,
            path,
            device: socket.st_dev,
            inode: socket.st_ino,
        };
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&publication.path, std::fs::Permissions::from_mode(0o600))?;
        endpoint.frontend_config_root()?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener: tokio::net::UnixListener::from_std(listener)?,
            publication,
            endpoint,
            slots: Arc::new(Semaphore::new(limit)),
        })
    }

    /// Returns validated discovery spelling, not a directory-relative capability.
    /// Callers announce it only to the exact admitted session frontend.
    pub(super) fn socket_path(&self) -> Result<&Path> {
        self.endpoint.frontend_config_root()?;
        Ok(&self.publication.path)
    }

    /// Reserves capacity nonwaitingly before awaiting an authenticated peer.
    /// Cancellation drops the pending slot; success transfers it with the stream.
    /// The supervisor supplies accept deadlines/cancellation and drives siblings.
    pub(super) async fn accept(&self) -> Result<AcceptedX11Frontend> {
        self.endpoint.frontend_config_root()?;
        let slot = self.slots.clone().try_acquire_owned().map_err(|_| {
            MezError::new(
                MezErrorKind::RateLimited,
                "outbound X11 frontend capacity unavailable",
            )
        })?;
        let (stream, _) = self.listener.accept().await?;
        crate::runtime::authenticated_unix_peer_uid(
            stream.as_raw_fd(),
            crate::runtime::current_effective_uid(),
        )?;
        self.endpoint.frontend_config_root()?;
        Ok(AcceptedX11Frontend {
            stream,
            endpoint: self.endpoint.clone(),
            slot,
        })
    }
}

impl AcceptedX11Frontend {
    /// Retains finite local admission through the session-owned handshake/relay.
    /// Parent-session authority supplies cookie, codec and nonreused occurrence.
    /// Cancellation closes this stream and releases its slot, without replay.
    pub(super) async fn relay(self, session: &mut InitializedSessionFrontend) -> Result<()> {
        let Self {
            stream,
            endpoint,
            slot,
        } = self;
        let _ownership = (endpoint, slot);
        _ownership.0.frontend_config_root()?;
        session.relay_x11_frontend(stream).await
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        if let Ok(socket) = statat(
            &self.directory,
            self.name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) && FileType::from_raw_mode(socket.st_mode) == FileType::Socket
            && socket.st_dev == self.device
            && socket.st_ino == self.inode
        {
            // Same-user check/unlink remains cooperative, not atomic CAS.
            let _ = unlinkat(&self.directory, self.name.as_str(), AtFlags::empty());
        }
    }
}

#[cfg(test)]
mod tests;
