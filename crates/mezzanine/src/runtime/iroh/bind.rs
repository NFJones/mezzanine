//! Bounded endpoint construction with explicit optional-transport failure scope.
//!
//! Configuration and cancelled construction are critical by default. Only a
//! completed network bind error without a returned endpoint, or readiness loss
//! followed by observed endpoint closure, permits transport unavailability.
//! Ordinary per-session and outbound callers still propagate every failure.
//! This component never retries, replaces identity ownership or replays work.

use std::future::Future;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use iroh::address_lookup::{DnsAddressLookup, PkarrPublisher};
use iroh::endpoint::{
    BindOpts, IdleTimeout, PortmapperConfig, QuicTransportConfig, VarInt, presets,
};
use iroh::tls::CaTlsConfig;
use iroh::{Endpoint, SecretKey, Watcher};

use crate::error::{MezError, Result};

use super::{
    IrohCompressionPolicy, RuntimeIrohAddressLookupPolicy, RuntimeIrohDiagnostics,
    RuntimeIrohEndpoint, RuntimeIrohRelayPolicy, RuntimeIrohTransportPolicy, relay_mode,
};

/// Construction disposition determined at the actual resource boundary.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeIrohBindError {
    /// No endpoint survives a completed bind error or proven readiness cleanup.
    #[error(transparent)]
    TransportUnavailable(MezError),
    /// Invalid policy or ambiguous construction/cleanup prevents continuation.
    #[error(transparent)]
    Critical(MezError),
}

impl RuntimeIrohBindError {
    /// Returns the complete original error without discarding typed evidence.
    pub(crate) fn into_error(self) -> MezError {
        match self {
            Self::TransportUnavailable(error) | Self::Critical(error) => error,
        }
    }
}

impl From<MezError> for RuntimeIrohBindError {
    /// Ordinary errors are always critical; transport isolation is explicit.
    fn from(error: MezError) -> Self {
        Self::Critical(error)
    }
}

impl From<RuntimeIrohBindError> for MezError {
    /// Non-optional callers propagate every startup failure unchanged.
    fn from(error: RuntimeIrohBindError) -> Self {
        error.into_error()
    }
}

/// Requires actual closure after readiness failure before granting transport-only
/// scope. The readiness future owns its existing setup deadline; close receives
/// only the existing configured close budget. Validation must already be complete;
/// this boundary observes route readiness, not arbitrary configuration or trust.
/// An unused close future is dropped
/// without polling after readiness succeeds, so it cannot close a ready endpoint.
async fn settle_iroh_readiness<R, C>(
    readiness: R,
    close: C,
    close_budget: Duration,
) -> std::result::Result<iroh::EndpointAddr, RuntimeIrohBindError>
where
    R: Future<Output = Result<iroh::EndpointAddr>>,
    C: Future<Output = ()>,
{
    match readiness.await {
        Ok(address) => Ok(address),
        Err(error) => {
            if tokio::time::timeout(close_budget, close).await.is_ok() {
                Err(RuntimeIrohBindError::TransportUnavailable(error))
            } else {
                Err(RuntimeIrohBindError::Critical(error.with_context(
                    "Iroh readiness failed and subsequent endpoint close timed out",
                )))
            }
        }
    }
}

/// Binds a protected endpoint only when remote transport is explicitly enabled.
pub(crate) async fn bind_runtime_iroh_endpoint(
    policy: RuntimeIrohTransportPolicy,
    secret_key: SecretKey,
) -> std::result::Result<Option<RuntimeIrohEndpoint>, RuntimeIrohBindError> {
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
        (policy.bind_port != 0).then_some(policy.bind_port),
        IrohCompressionPolicy::ordered_alpns(&policy.compression_codecs),
        "Iroh endpoint",
    )
    .await?;
    let initial_addr = settle_iroh_readiness(
        wait_for_dialable_iroh_addr(&endpoint, &policy),
        endpoint.close(),
        policy.setup_timeout,
    )
    .await?;
    let (endpoint_addr, _) = tokio::sync::watch::channel(Some(initial_addr));
    Ok(Some(RuntimeIrohEndpoint {
        endpoint,
        policy,
        diagnostics: RuntimeIrohDiagnostics::default(),
        intentional_close: Arc::new(AtomicBool::new(false)),
        endpoint_addr,
        authority: None,
    }))
}

/// Waits until the endpoint publishes at least one concrete transport route.
pub(super) async fn wait_for_dialable_iroh_addr(
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

/// Binds a client-only endpoint with no remotely initiated streams.
pub(crate) async fn bind_runtime_iroh_client_endpoint(
    policy: &RuntimeIrohTransportPolicy,
    secret_key: SecretKey,
) -> Result<Endpoint> {
    bind_policy_iroh_endpoint(
        policy,
        secret_key,
        0,
        1,
        None,
        Vec::new(),
        "Iroh client endpoint",
    )
    .await
    .map_err(RuntimeIrohBindError::into_error)
}

/// Constructs network resources once; only a completed SDK error with no
/// returned endpoint permits optional transport unavailability. Timeouts retain
/// critical scope because partial construction settlement was not observed.
async fn bind_policy_iroh_endpoint(
    policy: &RuntimeIrohTransportPolicy,
    secret_key: SecretKey,
    incoming_bidi_streams: u32,
    incoming_uni_streams: u32,
    bind_port: Option<u16>,
    alpns: Vec<Vec<u8>>,
    diagnostic_name: &str,
) -> std::result::Result<Endpoint, RuntimeIrohBindError> {
    let idle_timeout = IdleTimeout::try_from(policy.idle_timeout)
        .map_err(|_| MezError::config("transport.iroh.idle_timeout_ms is too large"))?;
    let keep_alive_interval = policy.idle_timeout / 3;
    let transport = QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(VarInt::from_u32(incoming_bidi_streams))
        .max_concurrent_uni_streams(VarInt::from_u32(incoming_uni_streams))
        .max_idle_timeout(Some(idle_timeout))
        .keep_alive_interval(keep_alive_interval)
        .build();

    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(secret_key)
        .alpns(alpns)
        .transport_config(transport)
        .relay_mode(relay_mode(&policy.relay)?);

    if let Some(bind_port) = bind_port {
        builder = builder
            .bind_addr((Ipv4Addr::UNSPECIFIED, bind_port))
            .map_err(|error| {
                MezError::config(format!("invalid transport.iroh.bind_port: {error}"))
            })?
            .bind_addr_with_opts(
                (Ipv6Addr::UNSPECIFIED, bind_port),
                BindOpts::default().set_is_required(false),
            )
            .map_err(|error| {
                MezError::config(format!("invalid transport.iroh.bind_port: {error}"))
            })?;
    }

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
        .map_err(|_| {
            RuntimeIrohBindError::Critical(MezError::invalid_state(format!(
                "{diagnostic_name} setup timed out before construction settlement"
            )))
        })?
        .map_err(|error| classify_completed_bind_error(error, diagnostic_name))
}

/// Classifies a completed SDK construction error by its typed variant. The SDK
/// also reports TLS/CA, crypto, address-lookup and internal failures here; only
/// socket binding is network-only. Unknown/future variants remain critical.
fn classify_completed_bind_error(
    error: iroh::endpoint::BindError,
    diagnostic_name: &str,
) -> RuntimeIrohBindError {
    let socket_bind_failed = matches!(&error, iroh::endpoint::BindError::Sockets { .. });
    let error = MezError::invalid_state(format!("failed to bind {diagnostic_name}: {error}"));
    if socket_bind_failed {
        RuntimeIrohBindError::TransportUnavailable(error)
    } else {
        RuntimeIrohBindError::Critical(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SDK's empty builder actually fails crypto-provider construction.
    /// This completed error must remain critical even though it returned no
    /// endpoint, preventing security/configuration errors from being mislabelled
    /// as optional socket unavailability. No external service or native child exists.
    #[tokio::test(flavor = "current_thread")]
    async fn sdk_crypto_construction_failure_remains_critical() {
        let error = iroh::endpoint::Builder::empty().bind().await.unwrap_err();
        assert!(error.to_string().contains("crypto provider"), "{error}");
        let result = classify_completed_bind_error(error, "crypto fixture");
        let RuntimeIrohBindError::Critical(error) = result else {
            panic!("SDK security failure was isolated: {result:?}");
        };
        assert!(error.message().contains("crypto provider"), "{error}");
    }

    /// A successfully ready real endpoint must stay open. Constructing its
    /// deferred close future must not execute cleanup on the success branch.
    #[tokio::test(flavor = "current_thread")]
    async fn ready_endpoint_is_not_closed_by_readiness_settlement() {
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            relay: RuntimeIrohRelayPolicy::Disabled,
            setup_timeout: Duration::from_secs(3),
            ..RuntimeIrohTransportPolicy::default()
        };
        let endpoint = bind_policy_iroh_endpoint(
            &policy,
            SecretKey::generate(),
            1,
            0,
            None,
            vec![],
            "readiness fixture",
        )
        .await
        .unwrap();
        let result = settle_iroh_readiness(
            wait_for_dialable_iroh_addr(&endpoint, &policy),
            endpoint.close(),
            policy.setup_timeout,
        )
        .await;
        let remained_open = !endpoint.is_closed();
        endpoint.close().await;
        assert!(!result.unwrap().is_empty());
        assert!(remained_open);
    }

    /// A fixture-owned loopback TCP listener withholds relay TLS readiness.
    /// The actual bound endpoint must close before readiness timeout can be
    /// called transport-only. This uses no external relay, DNS or native child.
    #[tokio::test(flavor = "current_thread")]
    async fn readiness_failure_closes_real_endpoint_before_optional_result() {
        let stalled_relay = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            relay: RuntimeIrohRelayPolicy::Custom {
                urls: vec![format!("https://{}/", stalled_relay.local_addr().unwrap())],
            },
            setup_timeout: Duration::from_secs(2),
            ..RuntimeIrohTransportPolicy::default()
        };
        let endpoint = bind_policy_iroh_endpoint(
            &policy,
            SecretKey::generate(),
            1,
            0,
            None,
            vec![],
            "readiness fixture",
        )
        .await
        .unwrap();
        let result = settle_iroh_readiness(
            wait_for_dialable_iroh_addr(&endpoint, &policy),
            endpoint.close(),
            policy.setup_timeout,
        )
        .await;
        let closed_before_return = endpoint.is_closed();
        drop(stalled_relay);
        let Err(RuntimeIrohBindError::TransportUnavailable(error)) = result else {
            panic!("readiness failure did not finish safe transport cleanup: {result:?}");
        };
        assert!(
            error.message().contains("address readiness timed out"),
            "{error}"
        );
        assert!(closed_before_return);
    }

    /// A generated pending close future proves the policy's unproven-cleanup
    /// branch, not a live SDK close-timeout certification. The original error's
    /// I/O classification and native/provider observations must survive intact.
    #[tokio::test(flavor = "current_thread")]
    async fn readiness_close_timeout_is_critical_with_original_evidence() {
        let original = MezError::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            .mark_pane_backend_failure()
            .with_provider_raw_text("readiness-origin");
        let result = settle_iroh_readiness(
            std::future::ready(Err(original)),
            std::future::pending(),
            Duration::from_millis(10),
        )
        .await;
        let Err(RuntimeIrohBindError::Critical(error)) = result else {
            panic!("unproven close was isolated: {result:?}");
        };
        assert_eq!(error.io_kind(), Some(std::io::ErrorKind::PermissionDenied));
        assert!(error.is_pane_backend_failure());
        assert_eq!(error.provider_raw_text(), Some("readiness-origin"));
        assert!(error.message().contains("endpoint close timed out"));
    }
}
