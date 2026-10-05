//! Private listener publication for the authenticated outbound admission owner.
//!
//! The retained endpoint lock supplies exclusive configuration-root ownership;
//! this module does not elect or launch another endpoint. Publication revalidates
//! the root before and after pathname-based Unix bind. It is not an atomic
//! directory-relative bind against hostile same-user renames. Cleanup uses a held
//! directory and exact socket dev/inode, preserving replacements. Admission and
//! remote request processing remain separate: accepted streams must be passed to
//! the bounded admission owner, and no tasks are spawned here.

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, statat, unlinkat};

use super::{OutboundEndpointOwner, OutboundFrontendAdmission};
use crate::error::{MezError, Result};

const SOCKET_NAME: &str = "outbound.sock";

#[cfg(test)]
pub(super) mod setup_diagnostics;

/// Published local front door retaining the endpoint and socket cleanup owner.
/// Drop removes only its positively identified socket, never a replacement.
pub(crate) struct OutboundFrontendListener {
    listener: tokio::net::UnixListener,
    admission: OutboundFrontendAdmission,
    publication: SocketPublication,
    pipeline_limit: usize,
}

/// Held-parent cleanup evidence; does not depend on the root pathname surviving.
struct SocketPublication {
    directory: std::fs::File,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl OutboundFrontendListener {
    /// Publishes an owner-private socket at the retained configuration root.
    /// Unsupported path lengths, stale root evidence, and live/unsafe existing
    /// entries reject without changing remote identity or launching an endpoint.
    pub(crate) fn bind(
        endpoint: OutboundEndpointOwner,
        limit: usize,
        deadline: std::time::Duration,
    ) -> Result<Self> {
        let directory = endpoint.frontend_root_directory()?;
        let path =
            crate::runtime::socket_path_for_name(endpoint.frontend_config_root()?, SOCKET_NAME)?;
        let admission = OutboundFrontendAdmission::new(endpoint, limit, deadline)?;
        let listener = crate::runtime::bind_control_socket(&path, admission.owner_uid)?;
        let socket = statat(&directory, SOCKET_NAME, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(std::io::Error::from)?;
        if FileType::from_raw_mode(socket.st_mode) != FileType::Socket {
            return Err(MezError::conflict(
                "outbound socket publication unavailable",
            ));
        }
        let publication = SocketPublication {
            directory,
            path,
            device: socket.st_dev,
            inode: socket.st_ino,
        };
        admission.endpoint.frontend_config_root()?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener: tokio::net::UnixListener::from_std(listener)?,
            admission,
            publication,
            pipeline_limit: limit,
        })
    }

    /// Returns the local discovery path, never endpoint/device credentials.
    pub(crate) fn socket_path(&self) -> Result<&Path> {
        self.admission.endpoint.frontend_config_root()?;
        Ok(&self.publication.path)
    }

    /// Accepts and authenticates a stream without serializing its handshake.
    /// The caller must bound accepted-but-unadmitted work and drive admission
    /// independently. Cancellation of accept owns no accepted stream or task.
    pub(crate) async fn accept(&self) -> Result<tokio::net::UnixStream> {
        self.admission.endpoint.frontend_config_root()?;
        let (stream, _) = self.listener.accept().await?;
        crate::runtime::authenticated_unix_peer_uid(stream.as_raw_fd(), self.admission.owner_uid)?;
        self.admission.endpoint.frontend_config_root()?;
        Ok(stream)
    }

    /// Runs the existing bounded versioned admission on one accepted stream.
    /// Failure or cancellation disposes that stream without affecting siblings.
    pub(crate) async fn admit(
        &self,
        stream: tokio::net::UnixStream,
    ) -> Result<super::AdmittedFrontend> {
        self.admission.admit(stream).await
    }

    /// Drives finite independent session/display pipelines until caller cancellation.
    /// Futures remain owned here, not spawned. Peer failures retire only their
    /// pipeline; cancellation drops local/remote owners without replay. Blocking
    /// profile work retains its separately bounded slot until actual completion.
    /// The caller still owns listener disposal and retained endpoint shutdown.
    pub(crate) async fn serve<C>(&self, cancellation: C) -> Result<u64>
    where
        C: std::future::Future<Output = ()>,
    {
        use futures_util::{StreamExt, stream::FuturesUnordered};
        let mut pipelines = FuturesUnordered::new();
        let mut accepted_count = 0_u64;
        tokio::pin!(cancellation);
        loop {
            tokio::select! {
                biased;
                () = &mut cancellation => return Ok(accepted_count),
                _ = pipelines.next(), if !pipelines.is_empty() => {},
                accepted = self.listener.accept(), if pipelines.len() < self.pipeline_limit => {
                    let (stream, _) = accepted?;
                    self.admission.endpoint.frontend_config_root()?;
                    accepted_count = accepted_count.checked_add(1).ok_or_else(|| {
                        MezError::invalid_state("outbound accepted count exhausted")
                    })?;
                    pipelines.push(self.serve_frontend(stream));
                }
            }
        }
    }

    /// Runs exactly one consumed setup and initialized display connection.
    /// No automatic reconnect, setup retry or generic control forwarding exists.
    async fn serve_frontend(&self, stream: tokio::net::UnixStream) -> Result<()> {
        #[cfg(test)]
        let mut diagnostics = setup_diagnostics::SetupDiagnostics::new();
        let frontend = self.admit(stream).await?;
        #[cfg(test)]
        diagnostics.advance("profile");
        let prepared = frontend.prepare(self.admission.deadline).await?;
        #[cfg(test)]
        diagnostics.advance("connect");
        let connected = prepared.connect_pinned().await?;
        #[cfg(test)]
        diagnostics.advance("initialize");
        if connected.host_only_requested()? {
            let initialized = connected.initialize_host_only().await?;
            #[cfg(test)]
            diagnostics.advance("host-only-delivery");
            initialized.deliver_list().await?;
            #[cfg(test)]
            diagnostics.complete();
            return Ok(());
        }
        let mut session = connected.initialize_session().await?;
        #[cfg(test)]
        diagnostics.advance("first-view");
        loop {
            session = session.deliver_view().await?;
            #[cfg(test)]
            diagnostics.complete();
            if session.is_detached() {
                return Ok(());
            }
        }
    }
}

impl Drop for SocketPublication {
    fn drop(&mut self) {
        let Ok(socket) = statat(&self.directory, SOCKET_NAME, AtFlags::SYMLINK_NOFOLLOW) else {
            return;
        };
        if FileType::from_raw_mode(socket.st_mode) == FileType::Socket
            && socket.st_dev == self.device
            && socket.st_ino == self.inode
        {
            // Same-user entry replacement between check and unlink remains a
            // cooperative cleanup race, not atomic compare-and-delete.
            let _ = unlinkat(&self.directory, SOCKET_NAME, AtFlags::empty());
        }
    }
}

#[cfg(test)]
mod tests;
