//! Iroh endpoint construction and lifecycle for optional remote control.

use iroh::address_lookup::{DnsAddressLookup, PkarrPublisher};
use iroh::endpoint::{IdleTimeout, PortmapperConfig, QuicTransportConfig, VarInt, presets};
use iroh::tls::CaTlsConfig;
use iroh::{Endpoint, RelayMap, RelayMode, SecretKey, Watcher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use tokio::task::JoinSet;

use crate::control::{AuthenticatedPeer, ControlConnectionState, encode_control_body};
use crate::error::{MezError, Result};
use crate::host::async_runtime::{
    AsyncRuntimeControlConnectionConfig, AsyncRuntimeService, AsyncRuntimeServiceExit,
    AsyncRuntimeSessionHandle,
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots_and_post_flush,
};
use crate::protocol::event::encode_event_notification;
use crate::storage::snapshot::SnapshotRepository;
use mez_core::ids::ClientId;
use tokio::io::AsyncWriteExt;

use super::config::{
    RuntimeIrohAddressLookupPolicy, RuntimeIrohRelayPolicy, RuntimeIrohTransportPolicy,
};

/// ALPN identity for the first Mezzanine Iroh transport contract.
pub(crate) const MEZZANINE_IROH_ALPN: &[u8] = b"mezzanine/transport/1";
pub(crate) const MEZZANINE_IROH_EVENT_STREAM_PREFACE: &[u8] = b"mezzanine/events/1\n";
const IROH_EVENT_BATCH_LIMIT: usize = 64;

/// Privacy-safe aggregate diagnostics for one supervised Iroh listener.
#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeIrohDiagnostics {
    inner: Arc<RuntimeIrohDiagnosticsInner>,
}

#[derive(Debug, Default)]
struct RuntimeIrohDiagnosticsInner {
    listener_active: AtomicBool,
    active_connections: AtomicUsize,
    connections_accepted: AtomicU64,
    connections_rejected: AtomicU64,
    setup_successes: AtomicU64,
    setup_failures: AtomicU64,
    setup_latency_total_millis: AtomicU64,
    setup_latency_max_millis: AtomicU64,
    connections_completed: AtomicU64,
    connections_failed: AtomicU64,
    direct_connections: AtomicU64,
    relay_connections: AtomicU64,
    custom_connections: AtomicU64,
    unknown_connections: AtomicU64,
    shutdown_aborts: AtomicU64,
    last_path: AtomicU8,
}

/// Copyable status projection that contains no endpoint or peer identifiers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RuntimeIrohDiagnosticsSnapshot {
    pub(crate) listener_active: bool,
    pub(crate) active_connections: usize,
    pub(crate) connections_accepted: u64,
    pub(crate) connections_rejected: u64,
    pub(crate) setup_successes: u64,
    pub(crate) setup_failures: u64,
    pub(crate) setup_latency_total_millis: u64,
    pub(crate) setup_latency_max_millis: u64,
    pub(crate) connections_completed: u64,
    pub(crate) connections_failed: u64,
    pub(crate) direct_connections: u64,
    pub(crate) relay_connections: u64,
    pub(crate) custom_connections: u64,
    pub(crate) unknown_connections: u64,
    pub(crate) shutdown_aborts: u64,
    last_path: u8,
}

impl RuntimeIrohDiagnosticsSnapshot {
    pub(crate) fn average_setup_latency_millis(self) -> u64 {
        let attempts = self.setup_successes.saturating_add(self.setup_failures);
        self.setup_latency_total_millis
            .checked_div(attempts)
            .unwrap_or(0)
    }

    pub(crate) const fn last_path_name(self) -> &'static str {
        match self.last_path {
            1 => "direct",
            2 => "relay",
            3 => "custom",
            _ => "unknown",
        }
    }
}

impl RuntimeIrohDiagnostics {
    pub(crate) fn snapshot(&self) -> RuntimeIrohDiagnosticsSnapshot {
        RuntimeIrohDiagnosticsSnapshot {
            listener_active: self.inner.listener_active.load(Ordering::Relaxed),
            active_connections: self.inner.active_connections.load(Ordering::Relaxed),
            connections_accepted: self.inner.connections_accepted.load(Ordering::Relaxed),
            connections_rejected: self.inner.connections_rejected.load(Ordering::Relaxed),
            setup_successes: self.inner.setup_successes.load(Ordering::Relaxed),
            setup_failures: self.inner.setup_failures.load(Ordering::Relaxed),
            setup_latency_total_millis: self
                .inner
                .setup_latency_total_millis
                .load(Ordering::Relaxed),
            setup_latency_max_millis: self.inner.setup_latency_max_millis.load(Ordering::Relaxed),
            connections_completed: self.inner.connections_completed.load(Ordering::Relaxed),
            connections_failed: self.inner.connections_failed.load(Ordering::Relaxed),
            direct_connections: self.inner.direct_connections.load(Ordering::Relaxed),
            relay_connections: self.inner.relay_connections.load(Ordering::Relaxed),
            custom_connections: self.inner.custom_connections.load(Ordering::Relaxed),
            unknown_connections: self.inner.unknown_connections.load(Ordering::Relaxed),
            shutdown_aborts: self.inner.shutdown_aborts.load(Ordering::Relaxed),
            last_path: self.inner.last_path.load(Ordering::Relaxed),
        }
    }

    fn listener_started(&self) {
        self.inner.listener_active.store(true, Ordering::Relaxed);
    }

    fn listener_stopped(&self) {
        self.inner.listener_active.store(false, Ordering::Relaxed);
    }

    fn record_setup_latency(&self, elapsed: std::time::Duration) {
        let millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        self.inner
            .setup_latency_total_millis
            .fetch_add(millis, Ordering::Relaxed);
        self.inner
            .setup_latency_max_millis
            .fetch_max(millis, Ordering::Relaxed);
    }

    fn record_rejected(&self, elapsed: std::time::Duration) {
        self.inner
            .connections_rejected
            .fetch_add(1, Ordering::Relaxed);
        self.inner.setup_failures.fetch_add(1, Ordering::Relaxed);
        self.record_setup_latency(elapsed);
    }

    fn connection_started(
        &self,
        connection: &iroh::endpoint::Connection,
        setup_elapsed: std::time::Duration,
    ) -> RuntimeIrohConnectionGuard {
        self.inner
            .connections_accepted
            .fetch_add(1, Ordering::Relaxed);
        self.inner.setup_successes.fetch_add(1, Ordering::Relaxed);
        self.record_setup_latency(setup_elapsed);
        self.inner
            .active_connections
            .fetch_add(1, Ordering::Relaxed);
        self.record_path(connection);
        RuntimeIrohConnectionGuard {
            diagnostics: self.clone(),
        }
    }

    fn record_result(&self, result: &Result<u64>) {
        if result.is_ok() {
            self.inner
                .connections_completed
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.inner
                .connections_failed
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_path(&self, connection: &iroh::endpoint::Connection) {
        let paths = connection.paths();
        let path = paths.iter().find(|path| path.is_selected());
        let code = match path {
            Some(path) if path.is_ip() => {
                self.inner
                    .direct_connections
                    .fetch_add(1, Ordering::Relaxed);
                1
            }
            Some(path) if path.is_relay() => {
                self.inner.relay_connections.fetch_add(1, Ordering::Relaxed);
                2
            }
            Some(_) => {
                self.inner
                    .custom_connections
                    .fetch_add(1, Ordering::Relaxed);
                3
            }
            None => {
                self.inner
                    .unknown_connections
                    .fetch_add(1, Ordering::Relaxed);
                0
            }
        };
        self.inner.last_path.store(code, Ordering::Relaxed);
    }
}

struct RuntimeIrohListenerGuard {
    diagnostics: RuntimeIrohDiagnostics,
    endpoint_addr: tokio::sync::watch::Sender<Option<iroh::EndpointAddr>>,
}

impl Drop for RuntimeIrohListenerGuard {
    fn drop(&mut self) {
        self.endpoint_addr.send_replace(None);
        self.diagnostics.listener_stopped();
    }
}

struct RuntimeIrohConnectionGuard {
    diagnostics: RuntimeIrohDiagnostics,
}

impl Drop for RuntimeIrohConnectionGuard {
    fn drop(&mut self) {
        self.diagnostics
            .inner
            .active_connections
            .fetch_sub(1, Ordering::Relaxed);
    }
}

/// Bound endpoint plus the policy governing listener and connection limits.
#[derive(Debug)]
pub(crate) struct RuntimeIrohEndpoint {
    endpoint: Endpoint,
    policy: RuntimeIrohTransportPolicy,
    diagnostics: RuntimeIrohDiagnostics,
    intentional_close: Arc<AtomicBool>,
    endpoint_addr: tokio::sync::watch::Sender<Option<iroh::EndpointAddr>>,
}

/// Cloneable handle for bounded, intentional endpoint shutdown.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeIrohShutdownHandle {
    endpoint: Endpoint,
    timeout: std::time::Duration,
    diagnostics: RuntimeIrohDiagnostics,
    intentional_close: Arc<AtomicBool>,
}

impl RuntimeIrohShutdownHandle {
    /// Notifies peers of endpoint shutdown and bounds QUIC close draining.
    pub(crate) async fn close(&self) -> bool {
        self.intentional_close.store(true, Ordering::Release);
        if tokio::time::timeout(self.timeout, self.endpoint.close())
            .await
            .is_err()
        {
            self.diagnostics
                .inner
                .shutdown_aborts
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }
}

impl RuntimeIrohEndpoint {
    /// Returns the bound Iroh endpoint.
    #[cfg(test)]
    pub(crate) fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Returns the policy applied to this endpoint.
    #[cfg(test)]
    pub(crate) fn policy(&self) -> &RuntimeIrohTransportPolicy {
        &self.policy
    }

    /// Returns a cloneable handle for intentional endpoint shutdown.
    pub(crate) fn shutdown_handle(&self) -> RuntimeIrohShutdownHandle {
        RuntimeIrohShutdownHandle {
            endpoint: self.endpoint.clone(),
            timeout: self.policy.setup_timeout,
            diagnostics: self.diagnostics.clone(),
            intentional_close: self.intentional_close.clone(),
        }
    }

    /// Closes the endpoint and bounds the wait for its I/O tasks to finish.
    pub(crate) async fn close(&self) -> bool {
        self.shutdown_handle().close().await
    }
}

/// Binds a protected endpoint only when remote transport is explicitly enabled.
pub(crate) async fn bind_runtime_iroh_endpoint(
    policy: RuntimeIrohTransportPolicy,
    secret_key: SecretKey,
) -> Result<Option<RuntimeIrohEndpoint>> {
    if !policy.enabled {
        return Ok(None);
    }

    let stream_limit = u32::try_from(policy.max_streams_per_connection)
        .map_err(|_| MezError::config("transport.iroh.max_streams_per_connection is too large"))?;
    let endpoint = bind_policy_iroh_endpoint(
        &policy,
        secret_key,
        stream_limit,
        0,
        vec![MEZZANINE_IROH_ALPN.to_vec()],
        "Iroh endpoint",
    )
    .await?;
    let initial_addr = wait_for_dialable_iroh_addr(&endpoint, &policy).await?;
    let (endpoint_addr, _) = tokio::sync::watch::channel(Some(initial_addr));
    Ok(Some(RuntimeIrohEndpoint {
        endpoint,
        policy,
        diagnostics: RuntimeIrohDiagnostics::default(),
        intentional_close: Arc::new(AtomicBool::new(false)),
        endpoint_addr,
    }))
}

/// Waits until the endpoint publishes at least one concrete transport route.
async fn wait_for_dialable_iroh_addr(
    endpoint: &Endpoint,
    policy: &RuntimeIrohTransportPolicy,
) -> Result<iroh::EndpointAddr> {
    let readiness = async {
        if !matches!(&policy.relay, RuntimeIrohRelayPolicy::Disabled) {
            endpoint.online().await;
        }
        let mut watcher = endpoint.watch_addr();
        loop {
            let addr = watcher.get();
            if !addr.is_empty() {
                return Ok::<iroh::EndpointAddr, MezError>(addr);
            }
            watcher.updated().await.map_err(|_| {
                MezError::invalid_state("Iroh endpoint address watcher disconnected")
            })?;
        }
    };
    tokio::time::timeout(policy.setup_timeout, readiness)
        .await
        .map_err(|_| MezError::invalid_state("Iroh endpoint address readiness timed out"))?
}

fn publish_iroh_endpoint_addr(
    publisher: &tokio::sync::watch::Sender<Option<iroh::EndpointAddr>>,
    addr: iroh::EndpointAddr,
) {
    publisher.send_replace((!addr.is_empty()).then_some(addr));
}

/// Binds a client-only endpoint with no remotely initiated streams.
pub(crate) async fn bind_runtime_iroh_client_endpoint(
    policy: &RuntimeIrohTransportPolicy,
    secret_key: SecretKey,
) -> Result<Endpoint> {
    bind_policy_iroh_endpoint(policy, secret_key, 0, 1, Vec::new(), "Iroh client endpoint").await
}

async fn bind_policy_iroh_endpoint(
    policy: &RuntimeIrohTransportPolicy,
    secret_key: SecretKey,
    incoming_bidi_streams: u32,
    incoming_uni_streams: u32,
    alpns: Vec<Vec<u8>>,
    diagnostic_name: &str,
) -> Result<Endpoint> {
    let idle_timeout = IdleTimeout::try_from(policy.idle_timeout)
        .map_err(|_| MezError::config("transport.iroh.idle_timeout_ms is too large"))?;
    let transport = QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(VarInt::from_u32(incoming_bidi_streams))
        .max_concurrent_uni_streams(VarInt::from_u32(incoming_uni_streams))
        .max_idle_timeout(Some(idle_timeout))
        .build();

    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(secret_key)
        .alpns(alpns)
        .transport_config(transport)
        .relay_mode(relay_mode(&policy.relay)?);

    if !policy.port_mapping {
        builder = builder.portmapper_config(PortmapperConfig::Disabled);
    }
    if !policy.direct_connections {
        builder = builder.clear_ip_transports();
    }
    if policy.proxy_from_env {
        builder = builder.proxy_from_env();
    }
    if policy.system_ca_store {
        builder = builder.ca_tls_config(CaTlsConfig::system());
    }
    builder = match &policy.address_lookup {
        RuntimeIrohAddressLookupPolicy::Disabled | RuntimeIrohAddressLookupPolicy::Local => {
            builder.clear_address_lookup()
        }
        RuntimeIrohAddressLookupPolicy::N0Dns => builder
            .address_lookup(PkarrPublisher::n0_dns())
            .address_lookup(DnsAddressLookup::n0_dns()),
        RuntimeIrohAddressLookupPolicy::CustomDns { domain } => builder
            .clear_address_lookup()
            .address_lookup(DnsAddressLookup::builder(domain.clone())),
    };

    tokio::time::timeout(policy.setup_timeout, builder.bind())
        .await
        .map_err(|_| MezError::invalid_state(format!("{diagnostic_name} setup timed out")))?
        .map_err(|error| {
            MezError::invalid_state(format!("failed to bind {diagnostic_name}: {error}"))
        })
}

impl super::RuntimeSessionService {
    /// Binds the configured endpoint while retaining the protected identity lock.
    pub(crate) async fn bind_configured_iroh_endpoint(
        &mut self,
    ) -> Result<Option<RuntimeIrohEndpoint>> {
        let structured = super::runtime_effective_config_value(self.integration.config_layers())?;
        let policy = super::runtime_iroh_transport_policy_from_config(&structured)?;
        if !policy.enabled {
            self.integration.set_remote_endpoint_addr(None);
            self.integration.set_remote_iroh_diagnostics(None);
            return Ok(None);
        }
        let session_id = self.session.id.to_string();
        let secret_key = self
            .integration
            .ensure_remote_endpoint_identity(&session_id)?
            .secret_key()
            .clone();
        let endpoint = bind_runtime_iroh_endpoint(policy, secret_key).await?;
        if let Some(endpoint) = endpoint.as_ref() {
            self.integration
                .set_remote_endpoint_addr_publisher(endpoint.endpoint_addr.clone());
        }
        self.integration.set_remote_iroh_diagnostics(
            endpoint
                .as_ref()
                .map(|endpoint| endpoint.diagnostics.clone()),
        );
        Ok(endpoint)
    }
}

/// Builds one supervised Iroh control-listener service.
pub(crate) fn build_runtime_iroh_control_service(
    endpoint: RuntimeIrohEndpoint,
    handle: AsyncRuntimeSessionHandle,
    control_config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<SnapshotRepository>,
) -> AsyncRuntimeService {
    AsyncRuntimeService::new("iroh-control", async move {
        let served =
            serve_runtime_iroh_control_listener(&endpoint, &handle, control_config, snapshots)
                .await;
        endpoint.close().await;
        served.map(AsyncRuntimeServiceExit::completed)
    })
}

/// Accepts bounded Iroh connections and delegates one bidirectional control stream.
async fn serve_runtime_iroh_control_listener(
    endpoint: &RuntimeIrohEndpoint,
    handle: &AsyncRuntimeSessionHandle,
    control_config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<SnapshotRepository>,
) -> Result<u64> {
    endpoint.diagnostics.listener_started();
    let _listener_guard = RuntimeIrohListenerGuard {
        diagnostics: endpoint.diagnostics.clone(),
        endpoint_addr: endpoint.endpoint_addr.clone(),
    };
    let mut accepted = 0u64;
    let mut tasks = JoinSet::new();
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut endpoint_addr = endpoint.endpoint.watch_addr();
    publish_iroh_endpoint_addr(&endpoint.endpoint_addr, endpoint_addr.get());
    let mut endpoint_closed_unexpectedly = false;
    loop {
        let state = *lifecycle.borrow();
        if terminal_daemon_state(state) {
            break;
        }
        tokio::select! {
            updated = endpoint_addr.updated() => {
                match updated {
                    Ok(addr) => publish_iroh_endpoint_addr(&endpoint.endpoint_addr, addr),
                    Err(_) => {
                        endpoint.endpoint_addr.send_replace(None);
                        endpoint_closed_unexpectedly =
                            !endpoint.intentional_close.load(Ordering::Acquire)
                                && !terminal_daemon_state(*lifecycle.borrow());
                        break;
                    }
                }
            }
            incoming = endpoint.endpoint.accept(), if tasks.len() < endpoint.policy.max_connections => {
                let Some(incoming) = incoming else {
                    endpoint_closed_unexpectedly =
                        !endpoint.intentional_close.load(Ordering::Acquire)
                            && !terminal_daemon_state(*lifecycle.borrow());
                    break;
                };
                let setup_started = Instant::now();
                let Ok(mut accepting) = incoming.accept() else {
                    endpoint
                        .diagnostics
                        .record_rejected(setup_started.elapsed());
                    continue;
                };
                let alpn = match tokio::time::timeout(endpoint.policy.setup_timeout, accepting.alpn()).await {
                    Ok(Ok(alpn)) => alpn,
                    _ => {
                        endpoint
                            .diagnostics
                            .record_rejected(setup_started.elapsed());
                        continue;
                    }
                };
                if alpn != MEZZANINE_IROH_ALPN {
                    endpoint
                        .diagnostics
                        .record_rejected(setup_started.elapsed());
                    continue;
                }
                let connection = match tokio::time::timeout(endpoint.policy.setup_timeout, accepting).await {
                    Ok(Ok(connection)) => connection,
                    _ => {
                        endpoint
                            .diagnostics
                            .record_rejected(setup_started.elapsed());
                        continue;
                    }
                };
                connection.set_max_concurrent_bi_streams(VarInt::from_u32(1));
                connection.set_max_concurrent_uni_streams(VarInt::from_u32(0));
                let connection_handle = handle.clone();
                let connection_snapshots = snapshots.clone();
                let diagnostics = endpoint.diagnostics.clone();
                let connection_guard =
                    diagnostics.connection_started(&connection, setup_started.elapsed());
                let setup_timeout = endpoint.policy.setup_timeout;
                let idle_timeout = endpoint.policy.idle_timeout;
                tasks.spawn(async move {
                    let _connection_guard = connection_guard;
                    let result = serve_runtime_iroh_control_connection(
                        connection,
                        &connection_handle,
                        control_config,
                        connection_snapshots.as_ref(),
                        setup_timeout,
                        idle_timeout,
                    )
                    .await;
                    diagnostics.record_result(&result);
                    result
                });
                accepted = accepted.saturating_add(1);
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                let Some(joined) = joined else {
                    continue;
                };
                let _connection_result = joined.map_err(|error| {
                    MezError::invalid_state(format!("Iroh control connection task failed: {error}"))
                })?;
            }
        }
    }

    if tokio::time::timeout(
        endpoint.policy.setup_timeout,
        drain_iroh_control_tasks(&mut tasks),
    )
    .await
    .is_err()
    {
        endpoint
            .diagnostics
            .inner
            .shutdown_aborts
            .fetch_add(tasks.len() as u64, Ordering::Relaxed);
        tasks.abort_all();
        while let Some(joined) = tasks.join_next().await {
            if let Err(error) = joined
                && !error.is_cancelled()
            {
                return Err(MezError::invalid_state(format!(
                    "Iroh control connection task failed: {error}"
                )));
            }
        }
    }
    if endpoint_closed_unexpectedly {
        return Err(MezError::invalid_state(
            "Iroh control listener closed unexpectedly while runtime remained active",
        ));
    }
    Ok(accepted)
}

async fn serve_runtime_iroh_control_connection(
    connection: iroh::endpoint::Connection,
    handle: &AsyncRuntimeSessionHandle,
    control_config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<&SnapshotRepository>,
    setup_timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
) -> Result<u64> {
    let endpoint_id = connection.remote_id().to_string();
    let (send, recv) = tokio::time::timeout(setup_timeout, connection.accept_bi())
        .await
        .map_err(|_| MezError::invalid_state("Iroh control stream setup timed out"))?
        .map_err(|error| {
            MezError::invalid_state(format!("failed to accept Iroh control stream: {error}"))
        })?;
    let mut stream = tokio::io::join(recv, send);
    let mut connection_state = ControlConnectionState::new(false, false);
    let (event_start_tx, event_start_rx) = tokio::sync::oneshot::channel::<(ClientId, u32)>();
    let mut event_start_tx = Some(event_start_tx);
    let (event_stop_tx, event_stop_rx) = tokio::sync::watch::channel(false);
    let event_connection = connection.clone();
    let event_handle = handle.clone();
    let mut event_task = tokio::spawn(async move {
        let Ok((client_id, version)) = event_start_rx.await else {
            return Ok(0);
        };
        serve_runtime_iroh_event_stream(
            event_connection,
            event_handle,
            client_id,
            version,
            setup_timeout,
            idle_timeout,
            event_stop_rx,
        )
        .await
    });
    let result =
        serve_authenticated_async_runtime_control_connection_loop_with_snapshots_and_post_flush(
            &mut stream,
            AuthenticatedPeer::iroh_endpoint(endpoint_id),
            handle,
            &mut connection_state,
            control_config,
            snapshots,
            |_, state| terminal_daemon_state(state),
            move |connection_state| {
                if let Some(start) = connection_state.take_event_stream_start()
                    && let Some(sender) = event_start_tx.take()
                {
                    let _ = sender.send(start);
                }
                Ok(())
            },
        )
        .await;
    let _ = event_stop_tx.send(true);
    if tokio::time::timeout(setup_timeout, &mut event_task)
        .await
        .is_err()
    {
        event_task.abort();
        let _ = event_task.await;
    }
    let (_recv, mut send) = stream.into_inner();
    let _ = send.finish();
    let _ = tokio::time::timeout(setup_timeout, send.stopped()).await;
    connection.close(
        VarInt::from_u32(u32::from(result.is_err())),
        if result.is_ok() {
            b"control complete"
        } else {
            b"control failed"
        },
    );
    result
}

async fn serve_runtime_iroh_event_stream(
    connection: iroh::endpoint::Connection,
    handle: AsyncRuntimeSessionHandle,
    caller_client_id: ClientId,
    version: u32,
    setup_timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<u64> {
    if version != 1 {
        return Err(MezError::invalid_args(
            "unsupported Iroh event stream version",
        ));
    }
    let connection_id = format!("iroh-events-{caller_client_id}");
    let mut last_delivered_event_id = 0u64;
    let mut pending = loop {
        if *stop.borrow() {
            return Ok(0);
        }
        match handle
            .event_wakeups_for_client(
                caller_client_id.clone(),
                connection_id.clone(),
                last_delivered_event_id,
                IROH_EVENT_BATCH_LIMIT,
            )
            .await
        {
            Ok(wakeups) => break wakeups,
            Err(error) if error.message().contains("pending observer event streams") => {
                tokio::select! {
                    _ = handle.wait_for_event_delivery() => {}
                    changed = stop.changed() => {
                        if changed.is_err() || *stop.borrow() {
                            return Ok(0);
                        }
                    }
                    _ = connection.closed() => return Ok(0),
                }
            }
            Err(_) => return Ok(0),
        }
    };
    let mut send = tokio::time::timeout(setup_timeout, connection.open_uni())
        .await
        .map_err(|_| MezError::invalid_state("Iroh event stream setup timed out"))?
        .map_err(|_| MezError::invalid_state("failed to open Iroh event stream"))?;
    tokio::time::timeout(
        idle_timeout,
        send.write_all(MEZZANINE_IROH_EVENT_STREAM_PREFACE),
    )
    .await
    .map_err(|_| MezError::invalid_state("Iroh event stream preface timed out"))?
    .map_err(|_| MezError::invalid_state("failed to write Iroh event stream preface"))?;
    tokio::time::timeout(idle_timeout, send.flush())
        .await
        .map_err(|_| MezError::invalid_state("Iroh event stream preface flush timed out"))?
        .map_err(|_| MezError::invalid_state("failed to flush Iroh event stream preface"))?;
    let mut delivered = 0u64;
    loop {
        if *stop.borrow() {
            break;
        }
        if pending.is_empty() {
            pending = match handle
                .event_wakeups_for_client(
                    caller_client_id.clone(),
                    connection_id.clone(),
                    last_delivered_event_id,
                    IROH_EVENT_BATCH_LIMIT,
                )
                .await
            {
                Ok(wakeups) => wakeups,
                Err(_) => break,
            };
        }
        let mut batch_last = None;
        for wakeup in pending.drain(..) {
            for event in wakeup.events {
                let frame = encode_control_body(&encode_event_notification(&event));
                tokio::time::timeout(idle_timeout, send.write_all(&frame))
                    .await
                    .map_err(|_| MezError::invalid_state("Iroh event stream write timed out"))?
                    .map_err(|_| MezError::invalid_state("Iroh event stream write failed"))?;
                batch_last = Some(event.id);
                delivered = delivered.saturating_add(1);
            }
        }
        if let Some(batch_last) = batch_last {
            tokio::time::timeout(idle_timeout, send.flush())
                .await
                .map_err(|_| MezError::invalid_state("Iroh event stream flush timed out"))?
                .map_err(|_| MezError::invalid_state("Iroh event stream flush failed"))?;
            last_delivered_event_id = batch_last;
            continue;
        }
        tokio::select! {
            _ = handle.wait_for_event_delivery() => {}
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            _ = connection.closed() => break,
        }
    }
    let _ = send.finish();
    let _ = tokio::time::timeout(setup_timeout, send.stopped()).await;
    Ok(delivered)
}

async fn drain_iroh_control_tasks(tasks: &mut JoinSet<Result<u64>>) -> Result<()> {
    while let Some(joined) = tasks.join_next().await {
        let _connection_result = joined.map_err(|error| {
            MezError::invalid_state(format!("Iroh control connection task failed: {error}"))
        })?;
    }
    Ok(())
}

fn terminal_daemon_state(state: super::RuntimeLifecycleState) -> bool {
    matches!(
        state,
        super::RuntimeLifecycleState::Stopping
            | super::RuntimeLifecycleState::Killed
            | super::RuntimeLifecycleState::Failed
    )
}

fn relay_mode(policy: &RuntimeIrohRelayPolicy) -> Result<RelayMode> {
    match policy {
        RuntimeIrohRelayPolicy::Disabled => Ok(RelayMode::Disabled),
        RuntimeIrohRelayPolicy::Public => Ok(RelayMode::Default),
        RuntimeIrohRelayPolicy::Custom { urls } => {
            RelayMap::try_from_iter(urls.iter().map(String::as_str))
                .map(RelayMode::Custom)
                .map_err(|error| {
                    MezError::config(format!("invalid transport.iroh.relay_urls: {error}"))
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn iroh_endpoint_construction_is_disabled_by_default() {
        let endpoint = bind_runtime_iroh_endpoint(
            RuntimeIrohTransportPolicy::default(),
            SecretKey::generate(),
        )
        .await
        .unwrap();
        assert!(endpoint.is_none());
    }

    /// Verifies enabled transport startup fails within its setup bound when
    /// neither direct nor relay networking can publish a concrete route.
    ///
    /// An endpoint-id-only address must never become invitation state because
    /// clients without address lookup cannot dial it.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_endpoint_construction_rejects_route_empty_transport() {
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            relay: RuntimeIrohRelayPolicy::Disabled,
            direct_connections: false,
            setup_timeout: std::time::Duration::from_millis(100),
            ..RuntimeIrohTransportPolicy::default()
        };

        let error = bind_runtime_iroh_endpoint(policy, SecretKey::generate())
            .await
            .unwrap_err();
        assert!(
            error.message().contains("failed to bind Iroh endpoint")
                || error.message().contains("address readiness timed out"),
            "{error:?}"
        );
    }

    /// Verifies live address publication exposes concrete routes and removes
    /// stale state when a watcher update becomes route-empty.
    ///
    /// Invitation creation reads this shared channel, so route loss must be
    /// visible immediately rather than retaining a previously dialable value.
    #[test]
    fn iroh_address_publication_tracks_route_presence() {
        let endpoint_id = SecretKey::generate().public();
        let (publisher, receiver) = tokio::sync::watch::channel(None);

        publish_iroh_endpoint_addr(&publisher, iroh::EndpointAddr::new(endpoint_id));
        assert!(receiver.borrow().is_none());

        let dialable =
            iroh::EndpointAddr::new(endpoint_id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        publish_iroh_endpoint_addr(&publisher, dialable.clone());
        assert_eq!(receiver.borrow().as_ref(), Some(&dialable));

        publish_iroh_endpoint_addr(&publisher, iroh::EndpointAddr::new(endpoint_id));
        assert!(receiver.borrow().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn iroh_endpoint_construction_uses_protected_identity_and_limits() {
        let secret_key = SecretKey::generate();
        let endpoint_id = secret_key.public();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            setup_timeout: std::time::Duration::from_secs(10),
            ..RuntimeIrohTransportPolicy::default()
        };
        let endpoint = bind_runtime_iroh_endpoint(policy.clone(), secret_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(endpoint.endpoint().id(), endpoint_id);
        assert_eq!(endpoint.policy(), &policy);
        endpoint.close().await;
    }

    /// Verifies endpoint loss while the daemon remains running is surfaced as
    /// a listener failure rather than a successful service completion.
    ///
    /// An enabled transport must not silently degrade to Unix-only control if
    /// its endpoint closes independently of the runtime lifecycle.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_listener_reports_unexpected_endpoint_closure() {
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let service = RuntimeServiceFixture::new().build();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            setup_timeout: std::time::Duration::from_secs(2),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server = bind_runtime_iroh_endpoint(policy, SecretKey::generate())
            .await
            .unwrap()
            .unwrap();
        let endpoint = server.endpoint().clone();
        let diagnostics = server.diagnostics.clone();
        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await
        });
        let actor_task = tokio::spawn(actor.run());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !diagnostics.snapshot().listener_active {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        endpoint.close().await;

        let error = tokio::time::timeout(std::time::Duration::from_secs(2), listener)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(
            error
                .message()
                .contains("Iroh control listener closed unexpectedly"),
            "{error:?}"
        );
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            crate::runtime::RuntimeLifecycleState::Running
        );
        assert!(!diagnostics.snapshot().listener_active);

        let _ = handle.shutdown().await.unwrap();
        drop(handle);
        actor_task.abort();
        let _ = actor_task.await;
    }

    /// Verifies intentional shutdown notifies an active peer and lets the
    /// listener finish without reporting endpoint loss as a service failure.
    ///
    /// Foreground cancellation awaits this bounded close path before the
    /// supervisor aborts any remaining services.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_shutdown_handle_notifies_active_peer_before_listener_exit() {
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let service = RuntimeServiceFixture::new().build();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 1,
            setup_timeout: std::time::Duration::from_secs(2),
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server = bind_runtime_iroh_endpoint(policy, SecretKey::generate())
            .await
            .unwrap()
            .unwrap();
        let server_addr = server.endpoint().addr();
        let shutdown = server.shutdown_handle();
        let diagnostics = server.diagnostics.clone();
        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await
        });
        let actor_task = tokio::spawn(actor.run());
        let client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let connection = client
            .connect(server_addr, MEZZANINE_IROH_ALPN)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while diagnostics.snapshot().active_connections != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let (closed_cleanly, _peer_close) =
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                tokio::join!(shutdown.close(), connection.closed())
            })
            .await
            .unwrap();
        assert!(closed_cleanly);
        assert_eq!(listener.await.unwrap().unwrap(), 1);

        let snapshot = diagnostics.snapshot();
        assert!(!snapshot.listener_active);
        assert_eq!(snapshot.active_connections, 0);
        assert_eq!(snapshot.shutdown_aborts, 0);
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            crate::runtime::RuntimeLifecycleState::Running
        );

        client.close().await;
        let _ = handle.shutdown().await.unwrap();
        drop(handle);
        actor_task.abort();
        let _ = actor_task.await;
    }

    async fn read_test_control_body<R>(stream: &mut R) -> String
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let mut response = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = tokio::io::AsyncReadExt::read(stream, &mut buffer)
                .await
                .unwrap();
            assert!(
                read > 0,
                "framed response must arrive before stream closure"
            );
            response.extend_from_slice(&buffer[..read]);
            if let Ok((body, _)) = crate::control::decode_control_frame(&response, 1024 * 1024) {
                return body;
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn paired_iroh_control_and_events_round_trip_over_direct_listener() {
        use secrecy::ExposeSecret;

        use crate::control::encode_control_body;
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::{RemoteRoleCeiling, RemoteTrustStore};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-direct-listener-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let store = RemoteTrustStore::under_config_root(&root, &session_id).unwrap();
        let invitation = store
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();

        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 1,
            max_streams_per_connection: 1,
            setup_timeout: std::time::Duration::from_secs(10),
            idle_timeout: std::time::Duration::from_secs(30),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server_endpoint = bind_runtime_iroh_endpoint(policy, server_secret)
            .await
            .unwrap()
            .unwrap();
        let server_addr = server_endpoint.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

        let client_endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .transport_config(
                QuicTransportConfig::builder()
                    .max_concurrent_uni_streams(VarInt::from_u32(1))
                    .build(),
            )
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "init",
            "method": "control/initialize",
            "params": {
                "requested_role": "primary",
                "requested_version": 1,
                "client_name": "remote-primary",
                "event_stream_version": 1,
                "client": {
                    "name": "remote-primary",
                    "interactive": true,
                    "terminal": {
                        "columns": 80,
                        "rows": 24,
                        "term": "xterm-256color"
                    }
                },
                "authentication": {
                    "mechanism": "extension:iroh_invitation",
                    "token": invitation.token.expose_secret()
                }
            }
        })
        .to_string();
        let kill = r#"{"jsonrpc":"2.0","id":"kill","method":"session/kill","params":{"force":true,"idempotency_key":"remote-kill"}}"#;

        let listener_handle = handle.clone();
        drop(handle);
        let listener = async move {
            let served = serve_runtime_iroh_control_listener(
                &server_endpoint,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(1024 * 1024, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(served, 1);
            server_endpoint.close().await;
        };
        let client = async {
            let connection = client_endpoint
                .connect(server_addr, MEZZANINE_IROH_ALPN)
                .await
                .unwrap();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    connection.accept_uni(),
                )
                .await
                .is_err(),
                "the server must not open an event stream before initialization",
            );
            send.write_all(&encode_control_body(&initialize))
                .await
                .unwrap();
            send.flush().await.unwrap();
            let initialize_body = read_test_control_body(&mut recv).await;
            assert!(initialize_body.contains(r#""granted_role":"primary""#));
            assert!(initialize_body.contains(r#""device_credential""#));

            let mut events =
                tokio::time::timeout(std::time::Duration::from_secs(3), connection.accept_uni())
                    .await
                    .unwrap()
                    .unwrap();
            let mut preface = vec![0u8; MEZZANINE_IROH_EVENT_STREAM_PREFACE.len()];
            events.read_exact(&mut preface).await.unwrap();
            assert_eq!(preface, MEZZANINE_IROH_EVENT_STREAM_PREFACE);
            let event_body = read_test_control_body(&mut events).await;
            assert!(event_body.contains(r#""method":"event/"#), "{event_body}");
            assert!(!event_body.contains("device_credential"), "{event_body}");
            assert!(
                !event_body.contains(invitation.token.expose_secret()),
                "{event_body}"
            );

            send.write_all(&encode_control_body(kill)).await.unwrap();
            send.finish().unwrap();
            let kill_body = read_test_control_body(&mut recv).await;
            assert!(kill_body.contains(r#""killed":true"#), "{kill_body}");
            connection.close(VarInt::from_u32(0), b"test complete");
            client_endpoint.close().await;
        };

        let actor_task = tokio::spawn(actor.run());
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let ((), ()) = tokio::join!(listener, client);
        })
        .await
        .unwrap();
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn iroh_client_connector_pairs_persists_and_reconnects_without_unix_fallback() {
        use crate::cli::{IrohControlTarget, exchange_iroh_control_request};
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::{
            RemoteClientIdentity, RemoteClientProfileStore, RemoteRoleCeiling, RemoteTrustStore,
        };
        use crate::test_support::runtime::RuntimeServiceFixture;

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-client-connector-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let server_root = root.join("server");
        let client_root = root.join("client");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&server_root).unwrap();
        std::fs::create_dir_all(&client_root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(server_root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = RemoteTrustStore::under_config_root(&server_root, &session_id).unwrap();
        let invitation = trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let server_policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 2,
            max_streams_per_connection: 1,
            setup_timeout: std::time::Duration::from_secs(10),
            idle_timeout: std::time::Duration::from_secs(30),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server_endpoint = bind_runtime_iroh_endpoint(server_policy, server_secret)
            .await
            .unwrap()
            .unwrap();
        let client_policy = RuntimeIrohTransportPolicy {
            setup_timeout: std::time::Duration::from_secs(10),
            idle_timeout: std::time::Duration::from_secs(30),
            ..RuntimeIrohTransportPolicy::default()
        };
        assert!(!client_policy.enabled);
        let server_addr = server_endpoint.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let listener_handle = handle.clone();
        drop(handle);

        let listener = async move {
            let served = serve_runtime_iroh_control_listener(
                &server_endpoint,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(1024 * 1024, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(served, 3);
            server_endpoint.close().await;
        };
        let client = async {
            let invalid_profile_target = IrohControlTarget::Invitation {
                profile_name: "x".repeat(129),
                server_addr: server_addr.clone(),
                token: invitation.token.clone(),
                role: RemoteRoleCeiling::Primary,
                expires_at_unix_seconds: invitation.expires_at_unix_seconds,
            };
            let save_error = exchange_iroh_control_request(
                &client_root,
                &client_policy,
                &invalid_profile_target,
                "window/list",
                "{}",
            )
            .await
            .unwrap_err();
            assert_eq!(save_error.kind(), crate::error::MezErrorKind::InvalidArgs);
            assert_eq!(trust.list_records().unwrap().len(), 1);
            assert!(
                RemoteClientProfileStore::under_config_root(&client_root)
                    .load("workstation")
                    .unwrap()
                    .is_none()
            );

            let invitation_target = IrohControlTarget::Invitation {
                profile_name: "workstation".to_string(),
                server_addr: server_addr.clone(),
                token: invitation.token.clone(),
                role: RemoteRoleCeiling::Primary,
                expires_at_unix_seconds: invitation.expires_at_unix_seconds,
            };
            let first = exchange_iroh_control_request(
                &client_root,
                &client_policy,
                &invitation_target,
                "window/list",
                "{}",
            )
            .await
            .unwrap();
            assert!(first.contains(r#""result""#), "{first}");

            let identity = RemoteClientIdentity::load_or_create(&client_root).unwrap();
            let records = trust.list_records().unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].endpoint_id, identity.endpoint_id().to_string());
            drop(identity);
            let profile = RemoteClientProfileStore::under_config_root(&client_root)
                .load("workstation")
                .unwrap()
                .unwrap();
            assert_eq!(profile.server_addr.id, server_addr.id);

            let second = exchange_iroh_control_request(
                &client_root,
                &client_policy,
                &IrohControlTarget::Profile(profile),
                "session/kill",
                r#"{"force":true,"idempotency_key":"connector-kill"}"#,
            )
            .await
            .unwrap();
            assert!(second.contains(r#""killed":true"#), "{second}");
        };

        let actor_task = tokio::spawn(actor.run());
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let ((), ()) = tokio::join!(listener, client);
        })
        .await
        .unwrap();
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn iroh_listener_rejects_wrong_alpn_without_runtime_authority() {
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let service = RuntimeServiceFixture::new().build();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 1,
            setup_timeout: std::time::Duration::from_secs(2),
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server = bind_runtime_iroh_endpoint(policy, SecretKey::generate())
            .await
            .unwrap()
            .unwrap();
        let server_addr = server.endpoint().addr();
        let diagnostics = server.diagnostics.clone();
        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            let result = serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await;
            server.close().await;
            result
        });
        let actor_task = tokio::spawn(actor.run());
        let client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            client.connect(server_addr, b"mezzanine/wrong/1"),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(!error.to_string().is_empty());
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            crate::runtime::RuntimeLifecycleState::Running
        );
        let _ = handle.shutdown().await.unwrap();
        drop(handle);
        assert_eq!(listener.await.unwrap().unwrap(), 0);
        let snapshot = diagnostics.snapshot();
        assert!(!snapshot.listener_active);
        assert_eq!(snapshot.active_connections, 0);
        assert_eq!(snapshot.connections_accepted, 0);
        assert!(snapshot.connections_rejected >= 1, "{snapshot:?}");
        assert_eq!(snapshot.setup_failures, snapshot.connections_rejected);
        assert_eq!(snapshot.setup_successes, 0);
        assert_eq!(snapshot.shutdown_aborts, 0);
        client.close().await;
        actor_task.abort();
        let _ = actor_task.await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_iroh_client_does_not_stop_later_authorized_control() {
        use crate::cli::{IrohControlTarget, exchange_iroh_control_request};
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::{RemoteRoleCeiling, RemoteTrustStore};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-malformed-isolation-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let server_root = root.join("server");
        let client_root = root.join("client");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&server_root).unwrap();
        std::fs::create_dir_all(&client_root).unwrap();
        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(server_root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = RemoteTrustStore::under_config_root(&server_root, &session_id).unwrap();
        let invitation = trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 2,
            setup_timeout: std::time::Duration::from_secs(3),
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server = bind_runtime_iroh_endpoint(policy.clone(), server_secret)
            .await
            .unwrap()
            .unwrap();
        let server_addr = server.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let listener_handle = handle.clone();
        drop(handle);
        let listener = async move {
            let served = serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(served, 2);
            server.close().await;
        };
        let clients = async {
            let malformed = Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .clear_address_lookup()
                .portmapper_config(PortmapperConfig::Disabled)
                .bind()
                .await
                .unwrap();
            let connection = malformed
                .connect(server_addr.clone(), MEZZANINE_IROH_ALPN)
                .await
                .unwrap();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut send, b"Content-Length: 9999999\r\n\r\n")
                .await
                .unwrap();
            send.finish().unwrap();
            let _ = recv.read_to_end(4096).await;
            connection.close(VarInt::from_u32(1), b"malformed test");
            malformed.close().await;

            let body = exchange_iroh_control_request(
                &client_root,
                &policy,
                &IrohControlTarget::Invitation {
                    profile_name: "authorized-after-malformed".to_string(),
                    server_addr,
                    token: invitation.token,
                    role: RemoteRoleCeiling::Primary,
                    expires_at_unix_seconds: invitation.expires_at_unix_seconds,
                },
                "session/kill",
                r#"{"force":true,"idempotency_key":"malformed-isolation-kill"}"#,
            )
            .await
            .unwrap();
            assert!(body.contains(r#""killed":true"#), "{body}");
        };
        let actor_task = tokio::spawn(actor.run());
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let ((), ()) = tokio::join!(listener, clients);
        })
        .await
        .unwrap();
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn iroh_setup_timeout_isolated_and_revoked_profile_rejected() {
        use crate::cli::{IrohControlTarget, exchange_iroh_control_request};
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::{
            RemoteClientProfileStore, RemoteRoleCeiling, RemoteTrustStore,
        };
        use crate::test_support::runtime::RuntimeServiceFixture;

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-timeout-revocation-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let server_root = root.join("server");
        let client_root = root.join("client");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&server_root).unwrap();
        std::fs::create_dir_all(&client_root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(server_root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = RemoteTrustStore::under_config_root(&server_root, &session_id).unwrap();
        let invitation = trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let mut policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 2,
            max_streams_per_connection: 1,
            setup_timeout: std::time::Duration::from_secs(3),
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let mut server = bind_runtime_iroh_endpoint(policy.clone(), server_secret)
            .await
            .unwrap()
            .unwrap();
        policy.setup_timeout = std::time::Duration::from_millis(500);
        server.policy.setup_timeout = policy.setup_timeout;
        let server_addr = server.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            let served = serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            server.close().await;
            served
        });
        let actor_task = tokio::spawn(actor.run());

        let stalled = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let stalled_connection = stalled
            .connect(server_addr.clone(), MEZZANINE_IROH_ALPN)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        stalled_connection.close(VarInt::from_u32(1), b"setup timeout test");
        stalled.close().await;

        let paired = exchange_iroh_control_request(
            &client_root,
            &policy,
            &IrohControlTarget::Invitation {
                profile_name: "revoked-client".to_string(),
                server_addr,
                token: invitation.token,
                role: RemoteRoleCeiling::Primary,
                expires_at_unix_seconds: invitation.expires_at_unix_seconds,
            },
            "window/list",
            "{}",
        )
        .await
        .unwrap();
        assert!(paired.contains(r#""result""#), "{paired}");

        let profile = RemoteClientProfileStore::under_config_root(&client_root)
            .load("revoked-client")
            .unwrap()
            .unwrap();
        let records = trust.list_records().unwrap();
        assert_eq!(records.len(), 1);
        trust
            .revoke_record(
                &records[0].id,
                Some("revocation regression"),
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let error = exchange_iroh_control_request(
            &client_root,
            &policy,
            &IrohControlTarget::Profile(profile),
            "window/list",
            "{}",
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
        assert!(error.message().contains("trust initialization"), "{error}");

        let _ = handle.shutdown().await.unwrap();
        drop(handle);
        assert_eq!(listener.await.unwrap(), 3);
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn iroh_abrupt_loss_detaches_primary_and_blocks_extra_stream() {
        use secrecy::ExposeSecret;

        use crate::control::{decode_control_frame, encode_control_body};
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::{RemoteRoleCeiling, RemoteTrustStore};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-abrupt-loss-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = RemoteTrustStore::under_config_root(&root, &session_id).unwrap();
        let invitation = trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 1,
            max_streams_per_connection: 1,
            setup_timeout: std::time::Duration::from_secs(2),
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server = bind_runtime_iroh_endpoint(policy, server_secret)
            .await
            .unwrap()
            .unwrap();
        let server_addr = server.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            let served = serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            server.close().await;
            served
        });
        let actor_task = tokio::spawn(actor.run());

        let client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let connection = client
            .connect(server_addr, MEZZANINE_IROH_ALPN)
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let second_stream =
            tokio::time::timeout(std::time::Duration::from_millis(250), connection.open_bi()).await;
        assert!(
            second_stream.is_err(),
            "a second concurrent control stream must be blocked"
        );

        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "init",
            "method": "control/initialize",
            "params": {
                "client_name": "abrupt-primary",
                "requested_version": 1,
                "requested_role": "primary",
                "detach_primary_on_disconnect": true,
                "client": {
                    "name": "abrupt-primary",
                    "interactive": true,
                    "terminal": {
                        "columns": 80,
                        "rows": 24,
                        "term": "xterm-256color"
                    }
                },
                "authentication": {
                    "mechanism": "extension:iroh_invitation",
                    "token": invitation.token.expose_secret()
                }
            }
        })
        .to_string();
        tokio::io::AsyncWriteExt::write_all(&mut send, &encode_control_body(&initialize))
            .await
            .unwrap();
        let initialize_body = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let mut response = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = tokio::io::AsyncReadExt::read(&mut recv, &mut buffer)
                    .await
                    .unwrap();
                assert!(read > 0, "initialize response must precede stream closure");
                response.extend_from_slice(&buffer[..read]);
                if let Ok((body, _)) = decode_control_frame(&response, 1024 * 1024) {
                    break body;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            initialize_body.contains(r#""granted_role":"primary""#),
            "{initialize_body}"
        );
        connection.close(VarInt::from_u32(1), b"abrupt client loss");
        client.close().await;

        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if handle.lifecycle_state().await.unwrap()
                    == crate::runtime::RuntimeLifecycleState::Detached
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let _ = handle.shutdown().await.unwrap();
        drop(handle);
        assert_eq!(listener.await.unwrap(), 1);
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unix_and_iroh_control_remain_simultaneously_available() {
        use crate::cli::{IrohControlTarget, exchange_iroh_control_request};
        use crate::control::{decode_control_frame, encode_control_body};
        use crate::host::async_runtime::{
            AsyncRuntimeActorConfig, AsyncRuntimeSessionActor,
            serve_async_runtime_control_listener_with_snapshots,
        };
        use crate::security::remote::{RemoteRoleCeiling, RemoteTrustStore};
        use crate::test_support::runtime::RuntimeServiceFixture;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-unix-coexistence-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let server_root = root.join("server");
        let client_root = root.join("client");
        let unix_path = root.join("control.sock");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&server_root).unwrap();
        std::fs::create_dir_all(&client_root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(server_root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = RemoteTrustStore::under_config_root(&server_root, &session_id).unwrap();
        let invitation = trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 1,
            max_streams_per_connection: 1,
            setup_timeout: std::time::Duration::from_secs(3),
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let iroh_server = bind_runtime_iroh_endpoint(policy.clone(), server_secret)
            .await
            .unwrap()
            .unwrap();
        let server_addr = iroh_server.endpoint().addr();
        let unix_listener = tokio::net::UnixListener::bind(&unix_path).unwrap();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let actor_task = tokio::spawn(actor.run());

        let unix_handle = handle.clone();
        let unix_server = async {
            let served = serve_async_runtime_control_listener_with_snapshots(
                &unix_listener,
                &unix_handle,
                AsyncRuntimeControlConnectionConfig::new(
                    1024 * 1024,
                    crate::runtime::current_effective_uid(),
                )
                .unwrap(),
                None,
                |accepted, _| accepted >= 1,
            )
            .await
            .unwrap();
            assert_eq!(served, 1);
        };
        let unix_client = async {
            let mut stream = tokio::net::UnixStream::connect(&unix_path).await.unwrap();
            let initialize = encode_control_body(
                r#"{"jsonrpc":"2.0","id":"unix-init","method":"control/initialize","params":{"client_name":"local-recovery","requested_version":1,"requested_role":"primary","detach_primary_on_disconnect":true,"client":{"name":"local-recovery","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
            );
            let status = encode_control_body(
                r#"{"jsonrpc":"2.0","id":"unix-status","method":"remote/status","params":{}}"#,
            );
            stream.write_all(&initialize).await.unwrap();
            stream.write_all(&status).await.unwrap();
            stream.shutdown().await.unwrap();
            let mut output = Vec::new();
            stream.read_to_end(&mut output).await.unwrap();
            let (initialize_body, consumed) = decode_control_frame(&output, 1024 * 1024).unwrap();
            let (status_body, _) = decode_control_frame(&output[consumed..], 1024 * 1024).unwrap();
            assert!(initialize_body.contains(r#""granted_role":"primary""#));
            assert!(status_body.contains(r#""endpoint_id""#), "{status_body}");
        };

        let iroh_handle = handle.clone();
        let iroh_server_task = tokio::spawn(async move {
            let served = serve_runtime_iroh_control_listener(
                &iroh_server,
                &iroh_handle,
                AsyncRuntimeControlConnectionConfig::new(1024 * 1024, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            iroh_server.close().await;
            served
        });

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let ((), ()) = tokio::join!(unix_server, unix_client);
        })
        .await
        .unwrap();
        let remote_body = exchange_iroh_control_request(
            &client_root,
            &policy,
            &IrohControlTarget::Invitation {
                profile_name: "coexisting-remote".to_string(),
                server_addr,
                token: invitation.token,
                role: RemoteRoleCeiling::Primary,
                expires_at_unix_seconds: invitation.expires_at_unix_seconds,
            },
            "session/kill",
            r#"{"force":true,"idempotency_key":"coexistence-kill"}"#,
        )
        .await
        .unwrap();
        assert!(remote_body.contains(r#""killed":true"#), "{remote_body}");

        drop(handle);
        assert_eq!(iroh_server_task.await.unwrap(), 1);
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }
}
