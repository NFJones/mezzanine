//! Authenticated local readiness and stream acquisition, without broker startup.
//!
//! Discovery is read-only: a missing root/socket never creates or replaces state.
//! Kernel peer UID is checked before protocol exchange; root/socket identity is
//! revalidated after connecting and after bounded negotiation. This cooperative
//! same-user check/use boundary is not atomic protection against hostile renames.
//! The client retains the exact admitted stream and inert handle, never endpoint
//! keys/device credentials. Election, setup and ordinary CLI routing are separate.

use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags, open, statat};

use super::*;

const SOCKET_NAME: &str = "outbound.sock";

mod killing;
mod listing;
mod session;
#[allow(
    unused_imports,
    reason = "ordinary CLI integration follows client snapshot qualification"
)]
pub(crate) use session::{OutboundSessionClient, SessionSummary};

/// Strict content-free broker reply. Unknown fields cannot carry remote proof.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloResponse {
    protocol: String,
    handle: FrontendHandle,
}

/// One admitted local client; retaining this object retains its exact IPC stream.
/// This is readiness evidence only, not remote authentication or session authority.
pub(crate) struct OutboundFrontendClient {
    stream: Framed<tokio::net::UnixStream, ProtocolFrameCodec>,
    handle: FrontendHandle,
    discovery: Discovery,
}

/// Held root plus socket-name identity, validated without mutating discovery.
struct Discovery {
    path: PathBuf,
    root: std::fs::File,
    device: u64,
    inode: u64,
    uid: u32,
}

impl Discovery {
    /// Opens only an existing private directory, rejecting a final symlink.
    fn capture(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(MezError::invalid_args(
                "outbound discovery root must be absolute",
            ));
        }
        let uid = crate::runtime::current_effective_uid();
        let root: std::fs::File = open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?
        .into();
        let path = std::fs::canonicalize(path)?;
        let socket =
            statat(&root, SOCKET_NAME, AtFlags::SYMLINK_NOFOLLOW).map_err(std::io::Error::from)?;
        let discovery = Self {
            path,
            root,
            device: socket.st_dev,
            inode: socket.st_ino,
            uid,
        };
        discovery.validate()?;
        Ok(discovery)
    }

    /// Checks private policy and exact retained root/socket objects, read-only.
    fn validate(&self) -> Result<()> {
        let held = self.root.metadata()?;
        let current = std::fs::symlink_metadata(&self.path)?;
        if !current.is_dir()
            || current.file_type().is_symlink()
            || current.uid() != self.uid
            || current.mode() & 0o077 != 0
        {
            return Err(MezError::forbidden(
                "outbound discovery root must remain private",
            ));
        }
        if held.dev() != current.dev() || held.ino() != current.ino() {
            return Err(MezError::conflict("outbound discovery root changed"));
        }
        let socket = statat(&self.root, SOCKET_NAME, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(std::io::Error::from)?;
        if FileType::from_raw_mode(socket.st_mode) != FileType::Socket
            || socket.st_uid != self.uid
            || socket.st_mode & 0o077 != 0
        {
            return Err(MezError::forbidden(
                "outbound discovery socket must remain private",
            ));
        }
        if socket.st_dev != self.device || socket.st_ino != self.inode {
            return Err(MezError::conflict("outbound discovery socket changed"));
        }
        Ok(())
    }
}

impl OutboundFrontendClient {
    /// Connects and authenticates one existing broker under a total deadline.
    /// Failure disposes the local stream, without startup, network dial or retries.
    pub(crate) async fn connect(config_root: &Path, deadline: Duration) -> Result<Self> {
        if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&deadline) {
            return Err(MezError::invalid_args(
                "outbound readiness deadline unavailable",
            ));
        }
        let discovery = Discovery::capture(config_root)?;
        let socket = crate::runtime::socket_path_for_name(&discovery.path, SOCKET_NAME)?;
        tokio::time::timeout(deadline, async move {
            let stream = tokio::net::UnixStream::connect(socket).await?;
            crate::runtime::authenticated_unix_peer_uid(stream.as_raw_fd(), discovery.uid)?;
            discovery.validate()?;
            let mut stream = Framed::new(stream, ProtocolFrameCodec::new(HELLO_LIMIT)?);
            stream
                .send(ProtocolFrame::new(
                    CONTENT_TYPE,
                    serde_json::json!({"protocol":PROTOCOL}).to_string(),
                ))
                .await?;
            let response =
                stream.next().await.transpose()?.ok_or_else(|| {
                    MezError::invalid_state("outbound readiness reply unavailable")
                })?;
            if response.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_args(
                    "outbound readiness content type unsupported",
                ));
            }
            let reply: HelloResponse = serde_json::from_str(&response.body)
                .map_err(|_| MezError::invalid_args("outbound readiness reply invalid"))?;
            if reply.protocol != PROTOCOL
                || reply.handle.generation == 0
                || reply.handle.owner.len() != 32
                || !reply
                    .handle
                    .owner
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(MezError::invalid_args(
                    "outbound readiness identity invalid",
                ));
            }
            discovery.validate()?;
            Ok(Self {
                stream,
                handle: reply.handle,
                discovery,
            })
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound readiness timed out"))?
    }

    /// Returns an inert exact handle after revalidating retained discovery.
    pub(crate) fn handle(&self) -> Result<&FrontendHandle> {
        self.discovery.validate()?;
        Ok(&self.handle)
    }
}

#[cfg(test)]
mod tests;
