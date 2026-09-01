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
use std::sync::{Arc, Mutex};

use rand::Rng;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;

use crate::error::{MezError, Result};
use crate::runtime::RuntimeIrohX11Policy;

use super::authority::{
    ensure_private_directory, write_empty_private_xauthority, write_private_xauthority,
};
use super::contracts::{
    X11_FORWARDING_VERSION, X11AuthProtocol, X11Cookie, X11ForwardingMode, X11ForwardingOffer,
    X11ForwardingResult, X11RouteToken, X11StreamPreface,
};
use super::protocol::{
    X11_MAX_SETUP_BYTES, X11SetupProgress, parse_x11_setup, validate_x11_setup_cookie,
};

/// First display number considered for a session-local proxy.
const X11_PROXY_MIN_DISPLAY: u16 = 10;
/// Last display number considered for a session-local proxy.
const X11_PROXY_MAX_DISPLAY: u16 = 99;
/// X11 TCP base port assigned to display zero.
const X11_TCP_BASE_PORT: u16 = 6000;
/// Attempts used to allocate a collision-resistant private directory.
const X11_PROXY_DIRECTORY_ATTEMPTS: usize = 16;

/// Exact application and transport identity allowed to own one X11 route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeX11RouteOwner {
    /// Session whose stable proxy is being activated.
    pub(crate) session_id: String,
    /// Attached primary client created by this control connection.
    pub(crate) client_id: String,
    /// Authenticated Iroh endpoint identity.
    pub(crate) endpoint_id: String,
    /// Durable remote trust identity for host-routed connections.
    pub(crate) principal_id: Option<String>,
    /// Random connection-local identity assigned by the concrete Iroh adapter.
    pub(crate) connection_id: String,
}

/// Clone-safe ownership lease for one reserved or active route generation.
#[derive(Clone)]
pub(crate) struct RuntimeX11RouteLease {
    inner: Arc<RuntimeX11RouteLeaseInner>,
}

impl fmt::Debug for RuntimeX11RouteLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeX11RouteLease")
            .field("owner", &self.inner.owner)
            .field("generation", &self.inner.generation)
            .finish_non_exhaustive()
    }
}

impl PartialEq for RuntimeX11RouteLease {
    fn eq(&self, other: &Self) -> bool {
        self.inner.generation == other.inner.generation && self.inner.owner == other.inner.owner
    }
}

impl Eq for RuntimeX11RouteLease {}

impl RuntimeX11RouteLease {
    /// Publishes the reserved fake cookie and exact transport only after initialization flushed.
    pub(crate) fn activate(&self, connection: iroh::endpoint::Connection) -> Result<()> {
        let connection_id = format!("iroh-{}", connection.stable_id());
        if connection_id != self.inner.owner.connection_id {
            return Err(MezError::invalid_state(
                "X11 route activation used a different Iroh connection",
            ));
        }
        self.inner
            .proxy
            .activate_route(&self.inner.owner, self.inner.generation, Some(connection))
    }

    /// Publishes authority state without a transport for registry-only tests.
    #[cfg(test)]
    fn activate_without_transport(&self) -> Result<()> {
        self.inner
            .proxy
            .activate_route(&self.inner.owner, self.inner.generation, None)
    }

    /// Explicitly invalidates this exact generation. Repeated or stale calls are harmless.
    pub(crate) fn deactivate(&self) -> Result<bool> {
        self.inner
            .proxy
            .deactivate_route(&self.inner.owner, self.inner.generation)
    }

    /// Returns the generation represented by this lease.
    pub(crate) fn generation(&self) -> u64 {
        self.inner.generation
    }
}

/// Shared lease data whose final drop performs stale-safe cleanup once.
struct RuntimeX11RouteLeaseInner {
    proxy: RuntimeX11ProxyHandle,
    owner: RuntimeX11RouteOwner,
    generation: u64,
}

impl Drop for RuntimeX11RouteLeaseInner {
    fn drop(&mut self) {
        let _ = self.proxy.deactivate_route(&self.owner, self.generation);
    }
}

/// Cloneable runtime view of one stable session X11 proxy.
#[derive(Clone)]
pub(crate) struct RuntimeX11ProxyHandle {
    inner: Arc<RuntimeX11ProxyState>,
}

impl PartialEq for RuntimeX11ProxyHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for RuntimeX11ProxyHandle {}

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

    /// Reserves one route generation without publishing its credential yet.
    pub(crate) fn reserve_route(
        &self,
        owner: RuntimeX11RouteOwner,
        offer: X11ForwardingOffer,
    ) -> Result<(X11ForwardingResult, RuntimeX11RouteLease)> {
        if !self.inner.policy.enabled {
            return Err(MezError::forbidden(
                "X11 forwarding is disabled by host policy",
            ));
        }
        if offer.version != X11_FORWARDING_VERSION {
            return Err(MezError::invalid_args("unsupported X11 forwarding version"));
        }
        if offer.auth_protocol != X11AuthProtocol::MitMagicCookie1 {
            return Err(MezError::invalid_args(
                "unsupported X11 authorization protocol",
            ));
        }
        if offer.mode == X11ForwardingMode::Trusted && !self.inner.policy.allow_trusted {
            return Err(MezError::forbidden(
                "trusted X11 forwarding is disabled by host policy",
            ));
        }

        let mut routes =
            self.inner.routes.lock().map_err(|_| {
                MezError::invalid_state("session X11 route registry is unavailable")
            })?;
        if routes.current.is_some() && !offer.takeover {
            return Err(MezError::conflict(
                "the session already has an active X11 route owner",
            ));
        }
        if let Some(current) = routes.current.as_ref() {
            write_empty_private_xauthority(&self.inner.authority_path)?;
            current.cancellation.send_replace(true);
        }
        routes.next_generation = routes
            .next_generation
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("session X11 route generation exhausted"))?;
        let generation = routes.next_generation;
        let route_token = X11RouteToken::random();
        routes.current = Some(RuntimeX11RouteState {
            owner: owner.clone(),
            generation,
            mode: offer.mode,
            fake_cookie: offer.fake_cookie,
            route_token: route_token.clone(),
            active: false,
            connection: None,
            permits: Arc::new(Semaphore::new(self.inner.policy.max_connections_per_route)),
            cancellation: watch::channel(false).0,
        });
        drop(routes);

        let result = X11ForwardingResult {
            version: X11_FORWARDING_VERSION,
            mode: offer.mode,
            generation,
            route_token,
        };
        let lease = RuntimeX11RouteLease {
            inner: Arc::new(RuntimeX11RouteLeaseInner {
                proxy: self.clone(),
                owner,
                generation,
            }),
        };
        Ok((result, lease))
    }

    /// Atomically publishes the fake cookie for one still-current reservation.
    fn activate_route(
        &self,
        owner: &RuntimeX11RouteOwner,
        generation: u64,
        connection: Option<iroh::endpoint::Connection>,
    ) -> Result<()> {
        let mut routes =
            self.inner.routes.lock().map_err(|_| {
                MezError::invalid_state("session X11 route registry is unavailable")
            })?;
        let route = routes.current.as_mut().ok_or_else(|| {
            MezError::invalid_state("session X11 route reservation is no longer current")
        })?;
        if route.generation != generation || &route.owner != owner {
            return Err(MezError::conflict(
                "session X11 route ownership changed before activation",
            ));
        }
        if route.active {
            return Ok(());
        }
        write_private_xauthority(
            &self.inner.authority_path,
            self.inner.display_number,
            &route.fake_cookie,
        )?;
        route.connection = connection;
        route.active = true;
        Ok(())
    }

    /// Invalidates only the exact current route generation.
    fn deactivate_route(&self, owner: &RuntimeX11RouteOwner, generation: u64) -> Result<bool> {
        let mut routes =
            self.inner.routes.lock().map_err(|_| {
                MezError::invalid_state("session X11 route registry is unavailable")
            })?;
        let matches = routes
            .current
            .as_ref()
            .is_some_and(|route| route.generation == generation && &route.owner == owner);
        if !matches {
            return Ok(false);
        }
        write_empty_private_xauthority(&self.inner.authority_path)?;
        if let Some(route) = routes.current.take() {
            route.cancellation.send_replace(true);
        }
        Ok(true)
    }

    /// Snapshots one currently published route for a single accepted socket.
    fn active_route(&self) -> Option<RuntimeX11ActiveRoute> {
        let routes = self.inner.routes.lock().ok()?;
        let route = routes.current.as_ref()?;
        if !route.active {
            return None;
        }
        Some(RuntimeX11ActiveRoute {
            generation: route.generation,
            fake_cookie: route.fake_cookie.clone(),
            route_token: route.route_token.clone(),
            connection: route.connection.as_ref()?.clone(),
            setup_timeout: self.inner.policy.setup_timeout,
            permits: route.permits.clone(),
            cancellation: route.cancellation.subscribe(),
        })
    }

    /// Returns whether one exact route generation is currently published.
    #[cfg(test)]
    fn route_is_active(&self, owner: &RuntimeX11RouteOwner, generation: u64) -> bool {
        self.inner.routes.lock().is_ok_and(|routes| {
            routes.current.as_ref().is_some_and(|route| {
                route.active && route.generation == generation && &route.owner == owner
            })
        })
    }
}

/// Immutable state shared with the serialized runtime service.
struct RuntimeX11ProxyState {
    display: String,
    display_number: u16,
    authority_path: PathBuf,
    policy: RuntimeIrohX11Policy,
    routes: Mutex<RuntimeX11RouteRegistry>,
}

/// Session-local generation counter and optional current owner.
#[derive(Default)]
struct RuntimeX11RouteRegistry {
    next_generation: u64,
    current: Option<RuntimeX11RouteState>,
}

/// Reserved or active route state. Secret fields retain redacted `Debug` behavior.
struct RuntimeX11RouteState {
    owner: RuntimeX11RouteOwner,
    generation: u64,
    mode: X11ForwardingMode,
    fake_cookie: X11Cookie,
    route_token: X11RouteToken,
    active: bool,
    connection: Option<iroh::endpoint::Connection>,
    permits: Arc<Semaphore>,
    cancellation: watch::Sender<bool>,
}

/// Cloneable generation snapshot retained by exactly one proxy worker.
struct RuntimeX11ActiveRoute {
    generation: u64,
    fake_cookie: X11Cookie,
    route_token: X11RouteToken,
    connection: iroh::endpoint::Connection,
    setup_timeout: std::time::Duration,
    permits: Arc<Semaphore>,
    cancellation: watch::Receiver<bool>,
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
    #[cfg(test)]
    pub(crate) fn prepare(config_root: &Path) -> Result<Self> {
        let policy = RuntimeIrohX11Policy {
            enabled: true,
            ..RuntimeIrohX11Policy::default()
        };
        Self::prepare_with_policy(config_root, policy)
    }

    /// Reserves a loopback display under the exact effective host policy.
    pub(crate) fn prepare_with_policy(
        config_root: &Path,
        policy: RuntimeIrohX11Policy,
    ) -> Result<Self> {
        if !policy.enabled {
            return Err(MezError::invalid_args(
                "session X11 proxy preparation requires enabled policy",
            ));
        }
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
                policy,
                routes: Mutex::new(RuntimeX11RouteRegistry::default()),
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

    /// Serves active generation-fenced routes and rejects every other socket.
    pub(crate) async fn serve(self) -> Result<u64> {
        let mut handled = 0u64;
        let mut workers = JoinSet::new();
        loop {
            tokio::select! {
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _peer)) => {
                        handled = handled.saturating_add(1);
                        let Some(route) = self.handle.active_route() else {
                            drop(stream);
                            continue;
                        };
                        let Ok(permit) = route.permits.clone().try_acquire_owned() else {
                            drop(stream);
                            continue;
                        };
                        workers.spawn(async move {
                            let _ = relay_server_x11_socket(stream, route, permit).await;
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        return Err(MezError::invalid_state(format!(
                            "session X11 proxy accept failed after {handled} sockets: {error}"
                        )));
                    }
                },
                joined = workers.join_next(), if !workers.is_empty() => {
                    if let Some(Err(error)) = joined
                        && !error.is_cancelled()
                    {
                        return Err(MezError::invalid_state(format!(
                            "session X11 proxy worker failed: {error}"
                        )));
                    }
                }
            }
        }
    }
}

/// Authenticates one local setup, opens one host-initiated stream, and relays raw bytes.
async fn relay_server_x11_socket(
    mut local: tokio::net::TcpStream,
    mut route: RuntimeX11ActiveRoute,
    _permit: OwnedSemaphorePermit,
) -> Result<()> {
    let setup = tokio::select! {
        biased;
        () = wait_for_route_cancellation(&mut route.cancellation) => {
            return Err(MezError::invalid_state("X11 route was deactivated"));
        }
        result = read_validated_x11_setup(
            &mut local,
            &route.fake_cookie,
            route.setup_timeout,
        ) => result?,
    };
    let open = tokio::time::timeout(route.setup_timeout, route.connection.open_bi());
    let (mut send, mut recv) = tokio::select! {
        biased;
        () = wait_for_route_cancellation(&mut route.cancellation) => {
            return Err(MezError::invalid_state("X11 route was deactivated"));
        }
        result = open => result
            .map_err(|_| MezError::invalid_state("X11 Iroh stream setup timed out"))?
            .map_err(|_| MezError::invalid_state("failed to open X11 Iroh stream"))?,
    };
    let preface = X11StreamPreface {
        generation: route.generation,
        route_token: route.route_token,
    }
    .encode();
    let publish = tokio::time::timeout(route.setup_timeout, async {
        send.write_all(&preface).await?;
        send.write_all(&setup).await?;
        send.flush().await
    });
    tokio::select! {
        biased;
        () = wait_for_route_cancellation(&mut route.cancellation) => {
            return Err(MezError::invalid_state("X11 route was deactivated"));
        }
        result = publish => result
            .map_err(|_| MezError::invalid_state("X11 Iroh stream preface timed out"))??,
    }

    let (mut local_read, mut local_write) = local.into_split();
    let relay = async move {
        let upstream = async {
            tokio::io::copy(&mut local_read, &mut send).await?;
            let _ = send.finish();
            Ok::<(), std::io::Error>(())
        };
        let downstream = async {
            tokio::io::copy(&mut recv, &mut local_write).await?;
            local_write.shutdown().await
        };
        tokio::try_join!(upstream, downstream)?;
        Ok::<(), MezError>(())
    };
    tokio::select! {
        result = relay => result,
        () = wait_for_route_cancellation(&mut route.cancellation) => Ok(()),
    }
}

/// Reads exactly one bounded setup packet and authenticates its fake credential.
async fn read_validated_x11_setup<R>(
    stream: &mut R,
    expected_cookie: &X11Cookie,
    setup_timeout: std::time::Duration,
) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    tokio::time::timeout(setup_timeout, async {
        let mut setup = Vec::new();
        loop {
            match parse_x11_setup(&setup).map_err(|error| MezError::forbidden(error.to_string()))? {
                X11SetupProgress::Complete(_) => {
                    validate_x11_setup_cookie(&setup, expected_cookie)
                        .map_err(|error| MezError::forbidden(error.to_string()))?;
                    return Ok(setup);
                }
                X11SetupProgress::Incomplete { required_len } => {
                    if required_len > X11_MAX_SETUP_BYTES || required_len <= setup.len() {
                        return Err(MezError::forbidden("invalid X11 setup length"));
                    }
                    let start = setup.len();
                    setup.resize(required_len, 0);
                    stream.read_exact(&mut setup[start..]).await?;
                }
            }
        }
    })
    .await
    .map_err(|_| MezError::invalid_state("X11 setup packet timed out"))?
}

/// Completes when one exact route generation has been invalidated.
async fn wait_for_route_cancellation(cancellation: &mut watch::Receiver<bool>) {
    if *cancellation.borrow() {
        return;
    }
    let _ = cancellation.changed().await;
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

    use iroh::endpoint::{PortmapperConfig, QuicTransportConfig, VarInt, presets};
    use iroh::{Endpoint, RelayMode};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

        let task = tokio::spawn(proxy.serve());
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

    /// Route reservation must remain unpublished until activation, require
    /// explicit takeover, and make stale-generation cleanup harmless.
    #[tokio::test]
    async fn route_ownership_is_generation_fenced_and_takeover_is_explicit() {
        let root = test_root("route-ownership");
        let proxy = RuntimeX11Proxy::prepare(&root).unwrap();
        let handle = proxy.handle();
        let first_owner = route_owner("client-one", "connection-one");
        let second_owner = route_owner("client-two", "connection-two");

        let (_abandoned_result, abandoned_lease) = handle
            .reserve_route(first_owner.clone(), route_offer([0x01; 16], false))
            .unwrap();
        drop(abandoned_lease);
        assert_eq!(fs::read(handle.authority_path()).unwrap(), Vec::<u8>::new());

        let (first_result, first_lease) = handle
            .reserve_route(first_owner.clone(), route_offer([0x11; 16], false))
            .unwrap();
        assert_eq!(fs::read(handle.authority_path()).unwrap(), Vec::<u8>::new());
        first_lease.activate_without_transport().unwrap();
        assert!(handle.route_is_active(&first_owner, first_result.generation));
        assert!(!fs::read(handle.authority_path()).unwrap().is_empty());

        let conflict = handle
            .reserve_route(second_owner.clone(), route_offer([0x22; 16], false))
            .unwrap_err();
        assert_eq!(conflict.kind(), crate::error::MezErrorKind::Conflict);

        let (second_result, second_lease) = handle
            .reserve_route(second_owner.clone(), route_offer([0x22; 16], true))
            .unwrap();
        assert!(second_result.generation > first_result.generation);
        assert_eq!(fs::read(handle.authority_path()).unwrap(), Vec::<u8>::new());
        assert!(!first_lease.deactivate().unwrap());
        second_lease.activate_without_transport().unwrap();
        assert!(handle.route_is_active(&second_owner, second_result.generation));

        let retained = second_lease.clone();
        drop(second_lease);
        assert!(handle.route_is_active(&second_owner, second_result.generation));
        drop(retained);
        assert_eq!(fs::read(handle.authority_path()).unwrap(), Vec::<u8>::new());

        drop(first_lease);
        drop(proxy);
        let _ = fs::remove_dir_all(root);
    }

    /// One authenticated proxy socket must map to one server-opened raw Iroh
    /// stream with the exact generation preface, setup packet, and later data.
    #[tokio::test]
    async fn active_route_relays_one_raw_server_opened_stream() {
        const TEST_ALPN: &[u8] = b"mezzanine/x11-proxy-test/1";
        let server_endpoint = Endpoint::builder(presets::Minimal)
            .alpns(vec![TEST_ALPN.to_vec()])
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let client_endpoint = Endpoint::builder(presets::Minimal)
            .transport_config(
                QuicTransportConfig::builder()
                    .max_concurrent_bidi_streams(VarInt::from_u32(2))
                    .build(),
            )
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let server_addr = server_endpoint.addr();
        let client_side = async {
            client_endpoint
                .connect(server_addr, TEST_ALPN)
                .await
                .unwrap()
        };
        let server_side = async {
            let incoming = server_endpoint.accept().await.unwrap();
            incoming.accept().unwrap().await.unwrap()
        };
        let (client_connection, server_connection) = tokio::join!(client_side, server_side);

        let root = test_root("active-relay");
        let proxy = RuntimeX11Proxy::prepare(&root).unwrap();
        let handle = proxy.handle();
        let owner = route_owner(
            "client-relay",
            &format!("iroh-{}", server_connection.stable_id()),
        );
        let (route, lease) = handle
            .reserve_route(owner, route_offer([0x61; 16], false))
            .unwrap();
        lease.activate(server_connection).unwrap();
        let proxy_task = tokio::spawn(proxy.serve());

        let remote_route = route.clone();
        let relay_connection = client_connection.clone();
        let remote = async move {
            let (mut send, mut recv) =
                tokio::time::timeout(Duration::from_secs(2), relay_connection.accept_bi())
                    .await
                    .unwrap()
                    .unwrap();
            let mut encoded = [0u8; super::super::X11_STREAM_PREFACE_BYTES];
            recv.read_exact(&mut encoded).await.unwrap();
            let preface = X11StreamPreface::decode(&encoded).unwrap();
            assert_eq!(preface.generation, remote_route.generation);
            assert_eq!(preface.route_token, remote_route.route_token);

            let mut setup = [0u8; 48];
            recv.read_exact(&mut setup).await.unwrap();
            validate_x11_setup_cookie(&setup, &X11Cookie::new([0x61; 16])).unwrap();
            let mut payload = [0u8; 4];
            recv.read_exact(&mut payload).await.unwrap();
            assert_eq!(&payload, b"ping");
            send.write_all(b"pong").await.unwrap();
            send.finish().unwrap();
        };
        let local = async {
            let mut stream = tokio::net::TcpStream::connect((
                Ipv4Addr::LOCALHOST,
                X11_TCP_BASE_PORT + handle.display_number(),
            ))
            .await
            .unwrap();
            stream
                .write_all(&setup_packet(b'l', &[0x61; 16]))
                .await
                .unwrap();
            stream.write_all(b"ping").await.unwrap();
            let mut reply = [0u8; 4];
            stream.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"pong");
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(remote, local);
        })
        .await
        .unwrap();

        drop(client_connection);
        lease.deactivate().unwrap();
        proxy_task.abort();
        let _ = proxy_task.await;
        client_endpoint.close().await;
        server_endpoint.close().await;
        let _ = fs::remove_dir_all(root);
    }

    /// Builds one exact route owner for focused registry tests.
    fn route_owner(client_id: &str, connection_id: &str) -> RuntimeX11RouteOwner {
        RuntimeX11RouteOwner {
            session_id: "$x11-test".to_string(),
            client_id: client_id.to_string(),
            endpoint_id: "endpoint-test".to_string(),
            principal_id: Some("principal-test".to_string()),
            connection_id: connection_id.to_string(),
        }
    }

    /// Builds one version-1 untrusted route offer for focused registry tests.
    fn route_offer(cookie: [u8; 16], takeover: bool) -> X11ForwardingOffer {
        X11ForwardingOffer {
            version: X11_FORWARDING_VERSION,
            mode: X11ForwardingMode::Untrusted,
            auth_protocol: X11AuthProtocol::MitMagicCookie1,
            fake_cookie: X11Cookie::new(cookie),
            takeover,
        }
    }

    /// Builds one exact little- or big-endian MIT setup request.
    fn setup_packet(byte_order: u8, cookie: &[u8; 16]) -> Vec<u8> {
        let mut setup = vec![0u8; 48];
        setup[0] = byte_order;
        let encode = |value: u16| {
            if byte_order == b'l' {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            }
        };
        setup[2..4].copy_from_slice(&encode(11));
        setup[4..6].copy_from_slice(&encode(0));
        setup[6..8].copy_from_slice(&encode(18));
        setup[8..10].copy_from_slice(&encode(16));
        setup[12..30].copy_from_slice(b"MIT-MAGIC-COOKIE-1");
        setup[32..48].copy_from_slice(cookie);
        setup
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
