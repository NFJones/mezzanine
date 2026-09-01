//! Typed runtime policy for the optional Iroh control transport.

use std::time::Duration;

use serde_json::Value;

use crate::error::{MezError, Result};

use super::{runtime_json_bool, runtime_json_object, runtime_json_string_array, runtime_json_u64};

/// Compression algorithms supported by the versioned Iroh application framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeIrohCompressionCodec {
    /// Stateful Zstandard compression on the version 3 Iroh ALPN.
    ZstdStream,
    /// Stateful linked-history LZ4 compression on the version 3 Iroh ALPN.
    Lz4Stream,
    /// Zstandard compression on the version 2 Iroh ALPN.
    Zstd,
    /// LZ4 block compression on the version 2 Iroh ALPN.
    Lz4,
    /// The unchanged version 1 Iroh framing contract.
    None,
}

impl RuntimeIrohCompressionCodec {
    /// Returns the stable configuration name for this codec.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ZstdStream => "zstd-stream",
            Self::Lz4Stream => "lz4-stream",
            Self::Zstd => "zstd",
            Self::Lz4 => "lz4",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeIrohIdentityPolicy {
    PerSession,
    Host,
}

impl RuntimeIrohIdentityPolicy {
    /// Returns the stable configuration name for this identity policy.
    const fn as_str(self) -> &'static str {
        match self {
            Self::PerSession => "per_session",
            Self::Host => "host",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeIrohAddressLookupPolicy {
    Disabled,
    N0Dns,
    CustomDns { domain: String },
    Local,
}

impl RuntimeIrohAddressLookupPolicy {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Disabled => "disabled",
            Self::N0Dns => "n0_dns",
            Self::CustomDns { .. } => "custom_dns",
            Self::Local => "local",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeIrohRelayPolicy {
    Disabled,
    Public,
    Custom { urls: Vec<String> },
}

impl RuntimeIrohRelayPolicy {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Disabled => "disabled",
            Self::Public => "public",
            Self::Custom { .. } => "custom",
        }
    }
}

/// Runtime policy for session-local X11 proxying over authenticated Iroh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeIrohX11Policy {
    /// Whether sessions prepare the forwarding proxy and protected environment.
    pub(crate) enabled: bool,
    /// Whether a client may explicitly request trusted X11 credentials.
    pub(crate) allow_trusted: bool,
    /// Maximum setup-pending and active X11 connections owned by one route.
    pub(crate) max_connections_per_route: usize,
    /// Deadline for the bounded X11 setup packet exchange.
    pub(crate) setup_timeout: Duration,
}

impl Default for RuntimeIrohX11Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_trusted: false,
            max_connections_per_route: 16,
            setup_timeout: Duration::from_millis(5_000),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeIrohTransportPolicy {
    pub(crate) enabled: bool,
    pub(crate) outbound_enabled: bool,
    pub(crate) bind_port: u16,
    pub(crate) identity: RuntimeIrohIdentityPolicy,
    pub(crate) address_lookup: RuntimeIrohAddressLookupPolicy,
    pub(crate) relay: RuntimeIrohRelayPolicy,
    pub(crate) direct_connections: bool,
    pub(crate) port_mapping: bool,
    pub(crate) proxy_from_env: bool,
    pub(crate) system_ca_store: bool,
    pub(crate) invitation_ttl: Duration,
    pub(crate) max_connections: usize,
    pub(crate) max_streams_per_connection: usize,
    pub(crate) setup_timeout: Duration,
    pub(crate) idle_timeout: Duration,
    pub(crate) compression_codecs: Vec<RuntimeIrohCompressionCodec>,
    pub(crate) compression_min_bytes: usize,
    pub(crate) compression_zstd_level: i32,
    pub(crate) x11: RuntimeIrohX11Policy,
}

impl Default for RuntimeIrohTransportPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            outbound_enabled: true,
            bind_port: 0,
            identity: RuntimeIrohIdentityPolicy::Host,
            address_lookup: RuntimeIrohAddressLookupPolicy::Disabled,
            relay: RuntimeIrohRelayPolicy::Disabled,
            direct_connections: true,
            port_mapping: false,
            proxy_from_env: false,
            system_ca_store: false,
            invitation_ttl: Duration::from_secs(600),
            max_connections: 16,
            max_streams_per_connection: 1,
            setup_timeout: Duration::from_millis(10_000),
            idle_timeout: Duration::from_millis(300_000),
            compression_codecs: vec![
                RuntimeIrohCompressionCodec::Zstd,
                RuntimeIrohCompressionCodec::Lz4,
                RuntimeIrohCompressionCodec::None,
            ],
            compression_min_bytes: 512,
            compression_zstd_level: 3,
            x11: RuntimeIrohX11Policy::default(),
        }
    }
}

pub(crate) fn runtime_iroh_transport_policy_from_config(
    root: &Value,
) -> Result<RuntimeIrohTransportPolicy> {
    let Some(transport) = runtime_json_object(root, "transport") else {
        return Ok(RuntimeIrohTransportPolicy::default());
    };
    let Some(iroh) = transport.get("iroh").and_then(Value::as_object) else {
        return Ok(RuntimeIrohTransportPolicy::default());
    };
    let defaults = RuntimeIrohTransportPolicy::default();

    let identity_name = string_value(iroh, "identity", defaults.identity.as_str())?;
    let identity = match identity_name.as_str() {
        "per_session" => RuntimeIrohIdentityPolicy::PerSession,
        "host" => RuntimeIrohIdentityPolicy::Host,
        _ => {
            return Err(MezError::config(
                "transport.iroh.identity must be per_session or host",
            ));
        }
    };
    let lookup_name = string_value(iroh, "address_lookup", "disabled")?;
    let lookup_domain = string_value(iroh, "address_lookup_domain", "")?;
    if lookup_name == "custom_dns"
        && (lookup_domain.is_empty()
            || !lookup_domain
                .chars()
                .all(|character| !character.is_control() && !character.is_whitespace()))
    {
        return Err(MezError::config(
            "transport.iroh custom DNS lookup requires a printable domain",
        ));
    }
    if lookup_name != "custom_dns" && !lookup_domain.is_empty() {
        return Err(MezError::config(
            "transport.iroh.address_lookup_domain is valid only with custom_dns",
        ));
    }
    let address_lookup = match lookup_name.as_str() {
        "disabled" => RuntimeIrohAddressLookupPolicy::Disabled,
        "n0_dns" => RuntimeIrohAddressLookupPolicy::N0Dns,
        "custom_dns" if !lookup_domain.is_empty() => RuntimeIrohAddressLookupPolicy::CustomDns {
            domain: lookup_domain.clone(),
        },
        "custom_dns" => {
            return Err(MezError::config(
                "transport.iroh custom DNS lookup requires a domain",
            ));
        }
        "local" => RuntimeIrohAddressLookupPolicy::Local,
        _ => {
            return Err(MezError::config(
                "transport.iroh.address_lookup has an unsupported value",
            ));
        }
    };

    let relay_name = string_value(iroh, "relay_mode", "disabled")?;
    let relay_urls = runtime_json_string_array(iroh.get("relay_urls"))?.unwrap_or_default();
    if relay_urls.len() > 8 {
        return Err(MezError::config(
            "transport.iroh.relay_urls must contain at most eight URLs",
        ));
    }
    if relay_urls.iter().any(|url| {
        !url.starts_with("https://")
            || url.len() <= "https://".len()
            || url.chars().any(char::is_control)
    }) {
        return Err(MezError::config(
            "transport.iroh custom relay URLs must be printable HTTPS URLs",
        ));
    }
    if relay_name != "custom" && !relay_urls.is_empty() {
        return Err(MezError::config(
            "transport.iroh.relay_urls are valid only with custom relay mode",
        ));
    }
    let relay = match relay_name.as_str() {
        "disabled" => RuntimeIrohRelayPolicy::Disabled,
        "public" => RuntimeIrohRelayPolicy::Public,
        "custom" if !relay_urls.is_empty() => RuntimeIrohRelayPolicy::Custom { urls: relay_urls },
        "custom" => {
            return Err(MezError::config(
                "transport.iroh custom relay mode requires relay URLs",
            ));
        }
        _ => {
            return Err(MezError::config(
                "transport.iroh.relay_mode has an unsupported value",
            ));
        }
    };

    let direct_connections = bool_value(iroh, "direct_connections", defaults.direct_connections)?;
    if !direct_connections && matches!(relay, RuntimeIrohRelayPolicy::Disabled) {
        return Err(MezError::config(
            "transport.iroh disabling direct connections requires a relay mode",
        ));
    }
    let compression_names = runtime_json_string_array(iroh.get("compression_codecs"))?
        .unwrap_or_else(|| {
            defaults
                .compression_codecs
                .iter()
                .map(|codec| codec.as_str().to_string())
                .collect()
        });
    if !(1..=5).contains(&compression_names.len()) {
        return Err(MezError::config(
            "transport.iroh.compression_codecs must contain one through five codecs",
        ));
    }
    let mut compression_codecs = Vec::with_capacity(compression_names.len());
    for name in compression_names {
        let codec = match name.as_str() {
            "zstd-stream" => RuntimeIrohCompressionCodec::ZstdStream,
            "lz4-stream" => RuntimeIrohCompressionCodec::Lz4Stream,
            "zstd" => RuntimeIrohCompressionCodec::Zstd,
            "lz4" => RuntimeIrohCompressionCodec::Lz4,
            "none" => RuntimeIrohCompressionCodec::None,
            _ => {
                return Err(MezError::config(
                    "transport.iroh.compression_codecs supports only zstd-stream, lz4-stream, zstd, lz4, and none",
                ));
            }
        };
        if compression_codecs.contains(&codec) {
            return Err(MezError::config(
                "transport.iroh.compression_codecs must not contain duplicates",
            ));
        }
        compression_codecs.push(codec);
    }
    let x11_defaults = RuntimeIrohX11Policy::default();
    let x11 = match iroh.get("x11") {
        None => x11_defaults,
        Some(value) => {
            let object = value.as_object().ok_or_else(|| {
                MezError::config("transport.iroh.x11 must be a configuration table")
            })?;
            RuntimeIrohX11Policy {
                enabled: x11_bool_value(object, "enabled", x11_defaults.enabled)?,
                allow_trusted: x11_bool_value(object, "allow_trusted", x11_defaults.allow_trusted)?,
                max_connections_per_route: x11_bounded_usize_value(
                    object,
                    "max_connections_per_route",
                    x11_defaults.max_connections_per_route,
                    1,
                    1_024,
                )?,
                setup_timeout: Duration::from_millis(x11_bounded_u64_value(
                    object,
                    "setup_timeout_ms",
                    x11_defaults.setup_timeout.as_millis() as u64,
                    100,
                    120_000,
                )?),
            }
        }
    };
    Ok(RuntimeIrohTransportPolicy {
        enabled: bool_value(iroh, "enabled", defaults.enabled)?,
        outbound_enabled: bool_value(iroh, "outbound_enabled", defaults.outbound_enabled)?,
        bind_port: u16::try_from(bounded_u64_value(
            iroh,
            "bind_port",
            u64::from(defaults.bind_port),
            0,
            u64::from(u16::MAX),
        )?)
        .map_err(|_| MezError::config("transport.iroh.bind_port is too large"))?,
        identity,
        address_lookup,
        relay,
        direct_connections,
        port_mapping: bool_value(iroh, "port_mapping", defaults.port_mapping)?,
        proxy_from_env: bool_value(iroh, "proxy_from_env", defaults.proxy_from_env)?,
        system_ca_store: bool_value(iroh, "system_ca_store", defaults.system_ca_store)?,
        invitation_ttl: Duration::from_secs(bounded_u64_value(
            iroh,
            "invitation_ttl_seconds",
            defaults.invitation_ttl.as_secs(),
            30,
            86_400,
        )?),
        max_connections: bounded_usize_value(
            iroh,
            "max_connections",
            defaults.max_connections,
            1,
            1_024,
        )?,
        max_streams_per_connection: bounded_usize_value(
            iroh,
            "max_streams_per_connection",
            defaults.max_streams_per_connection,
            1,
            1,
        )?,
        setup_timeout: Duration::from_millis(bounded_u64_value(
            iroh,
            "setup_timeout_ms",
            defaults.setup_timeout.as_millis() as u64,
            100,
            120_000,
        )?),
        idle_timeout: Duration::from_millis(bounded_u64_value(
            iroh,
            "idle_timeout_ms",
            defaults.idle_timeout.as_millis() as u64,
            1_000,
            86_400_000,
        )?),
        compression_codecs,
        compression_min_bytes: bounded_usize_value(
            iroh,
            "compression_min_bytes",
            defaults.compression_min_bytes,
            0,
            1024 * 1024,
        )?,
        compression_zstd_level: bounded_i32_value(
            iroh,
            "compression_zstd_level",
            defaults.compression_zstd_level,
            -5,
            22,
        )?,
        x11,
    })
}

fn string_value(
    object: &serde_json::Map<String, Value>,
    key: &str,
    default: &str,
) -> Result<String> {
    match object.get(key) {
        None => Ok(default.to_string()),
        Some(value) => value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| MezError::config(format!("transport.iroh.{key} must be a string"))),
    }
}

fn bool_value(object: &serde_json::Map<String, Value>, key: &str, default: bool) -> Result<bool> {
    match object.get(key) {
        None => Ok(default),
        value => runtime_json_bool(value)
            .ok_or_else(|| MezError::config(format!("transport.iroh.{key} must be a boolean"))),
    }
}

fn x11_bool_value(
    object: &serde_json::Map<String, Value>,
    key: &str,
    default: bool,
) -> Result<bool> {
    match object.get(key) {
        None => Ok(default),
        value => runtime_json_bool(value)
            .ok_or_else(|| MezError::config(format!("transport.iroh.x11.{key} must be a boolean"))),
    }
}

fn u64_value(object: &serde_json::Map<String, Value>, key: &str, default: u64) -> Result<u64> {
    match object.get(key) {
        None => Ok(default),
        value => runtime_json_u64(value).ok_or_else(|| {
            MezError::config(format!("transport.iroh.{key} must be a positive integer"))
        }),
    }
}

fn bounded_u64_value(
    object: &serde_json::Map<String, Value>,
    key: &str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<u64> {
    let value = u64_value(object, key, default)?;
    if !(minimum..=maximum).contains(&value) {
        return Err(MezError::config(format!(
            "transport.iroh.{key} must be an integer from {minimum} to {maximum}",
        )));
    }
    Ok(value)
}

fn x11_bounded_u64_value(
    object: &serde_json::Map<String, Value>,
    key: &str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<u64> {
    let value = match object.get(key) {
        None => default,
        value => runtime_json_u64(value).ok_or_else(|| {
            MezError::config(format!(
                "transport.iroh.x11.{key} must be a positive integer"
            ))
        })?,
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(MezError::config(format!(
            "transport.iroh.x11.{key} must be an integer from {minimum} to {maximum}",
        )));
    }
    Ok(value)
}

fn x11_bounded_usize_value(
    object: &serde_json::Map<String, Value>,
    key: &str,
    default: usize,
    minimum: u64,
    maximum: u64,
) -> Result<usize> {
    usize::try_from(x11_bounded_u64_value(
        object,
        key,
        default as u64,
        minimum,
        maximum,
    )?)
    .map_err(|_| MezError::config(format!("transport.iroh.x11.{key} is too large")))
}

fn bounded_usize_value(
    object: &serde_json::Map<String, Value>,
    key: &str,
    default: usize,
    minimum: u64,
    maximum: u64,
) -> Result<usize> {
    usize::try_from(bounded_u64_value(
        object,
        key,
        default as u64,
        minimum,
        maximum,
    )?)
    .map_err(|_| MezError::config(format!("transport.iroh.{key} is too large")))
}

fn bounded_i32_value(
    object: &serde_json::Map<String, Value>,
    key: &str,
    default: i32,
    minimum: i32,
    maximum: i32,
) -> Result<i32> {
    let value = match object.get(key) {
        None => i64::from(default),
        Some(value) => value
            .as_i64()
            .ok_or_else(|| MezError::config(format!("transport.iroh.{key} must be an integer")))?,
    };
    let value = i32::try_from(value)
        .map_err(|_| MezError::config(format!("transport.iroh.{key} is out of range")))?;
    if !(minimum..=maximum).contains(&value) {
        return Err(MezError::config(format!(
            "transport.iroh.{key} must be an integer from {minimum} to {maximum}",
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Omitted Iroh policy uses the persistent-host identity declared by the
    /// generated schema while explicit legacy per-session policy remains valid.
    #[test]
    fn iroh_transport_policy_defaults_disabled_and_materializes_explicit_values() {
        let defaults = runtime_iroh_transport_policy_from_config(&serde_json::json!({})).unwrap();
        assert_eq!(defaults, RuntimeIrohTransportPolicy::default());
        assert!(defaults.outbound_enabled);
        assert_eq!(defaults.bind_port, 0);
        assert_eq!(defaults.identity, RuntimeIrohIdentityPolicy::Host);
        assert_eq!(defaults.max_streams_per_connection, 1);
        assert_eq!(
            defaults.compression_codecs,
            vec![
                RuntimeIrohCompressionCodec::Zstd,
                RuntimeIrohCompressionCodec::Lz4,
                RuntimeIrohCompressionCodec::None,
            ]
        );
        assert_eq!(defaults.compression_min_bytes, 512);
        assert_eq!(defaults.compression_zstd_level, 3);
        assert_eq!(defaults.x11, RuntimeIrohX11Policy::default());

        let policy = runtime_iroh_transport_policy_from_config(&serde_json::json!({
            "transport": { "iroh": {
                "enabled": true,
                "outbound_enabled": false,
                "bind_port": 4242,
                "identity": "per_session",
                "address_lookup": "custom_dns",
                "address_lookup_domain": "iroh.example",
                "relay_mode": "custom",
                "relay_urls": ["https://relay.example"],
                "direct_connections": false,
                "port_mapping": true,
                "proxy_from_env": true,
                "system_ca_store": true,
                "invitation_ttl_seconds": 120,
                "max_connections": 8,
                "max_streams_per_connection": 1,
                "setup_timeout_ms": 5000,
                "idle_timeout_ms": 60000,
                "compression_codecs": ["lz4-stream", "zstd-stream", "lz4", "none"],
                "compression_min_bytes": 1024,
                "compression_zstd_level": -2,
                "x11": {
                    "enabled": true,
                    "allow_trusted": true,
                    "max_connections_per_route": 24,
                    "setup_timeout_ms": 7000
                }
            }}
        }))
        .unwrap();
        assert!(policy.enabled);
        assert!(!policy.outbound_enabled);
        assert_eq!(policy.bind_port, 4242);
        assert_eq!(policy.identity, RuntimeIrohIdentityPolicy::PerSession);
        assert_eq!(
            policy.address_lookup,
            RuntimeIrohAddressLookupPolicy::CustomDns {
                domain: "iroh.example".to_string()
            }
        );
        assert_eq!(
            policy.relay,
            RuntimeIrohRelayPolicy::Custom {
                urls: vec!["https://relay.example".to_string()]
            }
        );
        assert!(!policy.direct_connections);
        assert!(policy.port_mapping);
        assert_eq!(policy.invitation_ttl, Duration::from_secs(120));
        assert_eq!(policy.max_connections, 8);
        assert_eq!(policy.max_streams_per_connection, 1);
        assert_eq!(policy.setup_timeout, Duration::from_secs(5));
        assert_eq!(policy.idle_timeout, Duration::from_secs(60));
        assert_eq!(
            policy.compression_codecs,
            vec![
                RuntimeIrohCompressionCodec::Lz4Stream,
                RuntimeIrohCompressionCodec::ZstdStream,
                RuntimeIrohCompressionCodec::Lz4,
                RuntimeIrohCompressionCodec::None,
            ]
        );
        assert_eq!(policy.compression_min_bytes, 1024);
        assert_eq!(policy.compression_zstd_level, -2);
        assert_eq!(
            policy.x11,
            RuntimeIrohX11Policy {
                enabled: true,
                allow_trusted: true,
                max_connections_per_route: 24,
                setup_timeout: Duration::from_secs(7),
            }
        );

        let host = runtime_iroh_transport_policy_from_config(&serde_json::json!({
            "transport": { "iroh": { "enabled": false, "identity": "host" } }
        }))
        .unwrap();
        assert_eq!(host.identity, RuntimeIrohIdentityPolicy::Host);
        assert!(!host.enabled);

        let omitted_identity = runtime_iroh_transport_policy_from_config(&serde_json::json!({
            "transport": { "iroh": { "enabled": true } }
        }))
        .unwrap();
        assert!(omitted_identity.enabled);
        assert_eq!(omitted_identity.identity, RuntimeIrohIdentityPolicy::Host);
    }

    #[test]
    fn iroh_transport_runtime_policy_rejects_schema_invalid_network_combinations() {
        for value in [
            serde_json::json!({"transport":{"iroh":{"identity":7}}}),
            serde_json::json!({"transport":{"iroh":{"address_lookup":"disabled","address_lookup_domain":"unused.example"}}}),
            serde_json::json!({"transport":{"iroh":{"address_lookup":"custom_dns","address_lookup_domain":"bad domain"}}}),
            serde_json::json!({"transport":{"iroh":{"relay_mode":"disabled","relay_urls":["https://relay.example"]}}}),
            serde_json::json!({"transport":{"iroh":{"relay_mode":"custom","relay_urls":["http://relay.example"]}}}),
            serde_json::json!({"transport":{"iroh":{"relay_mode":"disabled","direct_connections":false}}}),
            serde_json::json!({"transport":{"iroh":{"max_connections":0}}}),
            serde_json::json!({"transport":{"iroh":{"max_streams_per_connection":2}}}),
            serde_json::json!({"transport":{"iroh":{"bind_port":65536}}}),
            serde_json::json!({"transport":{"iroh":{"idle_timeout_ms":999}}}),
            serde_json::json!({"transport":{"iroh":{"compression_codecs":[]}}}),
            serde_json::json!({"transport":{"iroh":{"compression_codecs":["zstd","zstd"]}}}),
            serde_json::json!({"transport":{"iroh":{"compression_codecs":["brotli"]}}}),
            serde_json::json!({"transport":{"iroh":{"compression_min_bytes":1048577}}}),
            serde_json::json!({"transport":{"iroh":{"compression_zstd_level":23}}}),
            serde_json::json!({"transport":{"iroh":{"x11":true}}}),
            serde_json::json!({"transport":{"iroh":{"x11":{"enabled":"yes"}}}}),
            serde_json::json!({"transport":{"iroh":{"x11":{"max_connections_per_route":0}}}}),
            serde_json::json!({"transport":{"iroh":{"x11":{"setup_timeout_ms":99}}}}),
        ] {
            assert!(
                runtime_iroh_transport_policy_from_config(&value).is_err(),
                "{value}"
            );
        }
    }
}
