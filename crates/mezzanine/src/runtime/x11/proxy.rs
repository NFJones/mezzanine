//! Session-local loopback X11 proxy ownership.
//!
//! The proxy reserves one bounded TCP display, publishes an empty private
//! Xauthority database, and exposes immutable pane environment values before
//! the first process starts. Until route negotiation installs an owner, every
//! accepted socket is closed immediately. The listener task exclusively owns
//! its generated directory so cancellation removes all session-local artifacts.

use std::fmt;
use std::fs;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rand::Rng;

use crate::error::{MezError, Result};

use super::authority::{ensure_private_directory, write_empty_private_xauthority};

/// First display number considered for a session-local proxy.
const X11_PROXY_MIN_DISPLAY: u16 = 10;
/// Last display number considered for a session-local proxy.
const X11_PROXY_MAX_DISPLAY: u16 = 99;
/// X11 TCP base port assigned to display zero.
const X11_TCP_BASE_PORT: u16 = 6000;
/// Attempts used to allocate a collision-resistant private directory.
const X11_PROXY_DIRECTORY_ATTEMPTS: usize = 16;

/// Cloneable runtime view of one stable session X11 proxy.
#[derive(Clone)]
pub(crate) struct RuntimeX11ProxyHandle {
    inner: Arc<RuntimeX11ProxyState>,
}

impl fmt::Debug for RuntimeX11ProxyHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeX11ProxyHandle")
            .field("display", &self.inner.display)
            .field("authority_path", &"[SESSION PATH REDACTED]")
            .finish()
    }
}

impl RuntimeX11ProxyHandle {
    /// Stable loopback DISPLAY exported to every pane in this session.
    pub(crate) fn display(&self) -> &str {
        &self.inner.display
    }

    /// Stable private Xauthority path exported to every pane in this session.
    pub(crate) fn authority_path(&self) -> &Path {
        &self.inner.authority_path
    }

    /// Display number represented by this proxy's TCP listener.
    pub(crate) fn display_number(&self) -> u16 {
        self.inner.display_number
    }
}

/// Immutable state shared with the serialized runtime service.
struct RuntimeX11ProxyState {
    display: String,
    display_number: u16,
    authority_path: PathBuf,
}

/// Listener and generated-artifact owner for one session proxy.
pub(crate) struct RuntimeX11Proxy {
    listener: tokio::net::TcpListener,
    handle: RuntimeX11ProxyHandle,
    directory: PathBuf,
    base_directory: PathBuf,
}

impl fmt::Debug for RuntimeX11Proxy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeX11Proxy")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl RuntimeX11Proxy {
    /// Reserves a loopback display and publishes an empty private authority file.
    pub(crate) fn prepare(config_root: &Path) -> Result<Self> {
        let listener = bind_display_listener()?;
        let display_number = listener_display_number(&listener)?;
        listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(listener)?;

        let base_directory = config_root.join("x11-sessions");
        ensure_private_directory(&base_directory)?;
        let directory = create_unique_private_directory(&base_directory)?;
        let authority_path = directory.join("Xauthority");
        if let Err(error) = write_empty_private_xauthority(&authority_path) {
            let _ = fs::remove_dir_all(&directory);
            return Err(error);
        }
        let handle = RuntimeX11ProxyHandle {
            inner: Arc::new(RuntimeX11ProxyState {
                display: format!("127.0.0.1:{display_number}.0"),
                display_number,
                authority_path,
            }),
        };
        Ok(Self {
            listener,
            handle,
            directory,
            base_directory,
        })
    }

    /// Returns the stable runtime handle retained after the listener is moved.
    pub(crate) fn handle(&self) -> RuntimeX11ProxyHandle {
        self.handle.clone()
    }

    /// Rejects every socket until route negotiation supplies forwarding state.
    pub(crate) async fn serve_no_route(self) -> Result<u64> {
        let mut rejected = 0u64;
        loop {
            match self.listener.accept().await {
                Ok((stream, _peer)) => {
                    drop(stream);
                    rejected = rejected.saturating_add(1);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return Err(MezError::invalid_state(format!(
                        "session X11 proxy accept failed after {rejected} rejected sockets: {error}"
                    )));
                }
            }
        }
    }
}

impl Drop for RuntimeX11Proxy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
        let _ = fs::remove_dir(&self.base_directory);
    }
}

/// Binds the first available display in the fixed session-proxy range.
fn bind_display_listener() -> Result<TcpListener> {
    for display in X11_PROXY_MIN_DISPLAY..=X11_PROXY_MAX_DISPLAY {
        let port = X11_TCP_BASE_PORT
            .checked_add(display)
            .ok_or_else(|| MezError::invalid_state("X11 proxy display port overflowed"))?;
        match TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)) {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(error) => {
                return Err(MezError::invalid_state(format!(
                    "failed to bind the session X11 proxy: {error}"
                )));
            }
        }
    }
    Err(MezError::conflict(
        "no session X11 proxy display is available in the fixed range",
    ))
}

/// Converts the listener's loopback port back to its X display number.
fn listener_display_number(listener: &TcpListener) -> Result<u16> {
    let port = listener.local_addr()?.port();
    port.checked_sub(X11_TCP_BASE_PORT)
        .ok_or_else(|| MezError::invalid_state("session X11 proxy bound an invalid port"))
}

/// Creates one unique owner-private directory beneath the validated base.
fn create_unique_private_directory(base: &Path) -> Result<PathBuf> {
    for _ in 0..X11_PROXY_DIRECTORY_ATTEMPTS {
        let suffix = rand::rng().next_u64();
        let directory = base.join(format!("proxy-{}-{suffix:016x}", std::process::id()));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&directory) {
            Ok(()) => {
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
                return Ok(directory);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(MezError::invalid_state(
        "failed to allocate private session X11 proxy state",
    ))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use tokio::io::AsyncReadExt;

    use super::*;

    /// Preparation must bind loopback only, create private empty authority
    /// state, reject no-route sockets, and remove artifacts when cancelled.
    #[tokio::test]
    async fn prepares_rejects_and_cleans_session_proxy() {
        let root = test_root("lifecycle");
        let proxy = RuntimeX11Proxy::prepare(&root).unwrap();
        let handle = proxy.handle();
        let authority_path = handle.authority_path().to_path_buf();
        let directory = authority_path.parent().unwrap().to_path_buf();
        assert_eq!(fs::read(&authority_path).unwrap(), Vec::<u8>::new());
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&authority_path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let task = tokio::spawn(proxy.serve_no_route());
        let mut stream = tokio::net::TcpStream::connect((
            Ipv4Addr::LOCALHOST,
            X11_TCP_BASE_PORT + handle.display_number(),
        ))
        .await
        .unwrap();
        let mut byte = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read, 0);

        task.abort();
        let _ = task.await;
        assert!(!directory.exists());
        let _ = fs::remove_dir_all(root);
    }

    /// Allocates one owner-private root for proxy tests.
    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mez-runtime-x11-proxy-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
}
