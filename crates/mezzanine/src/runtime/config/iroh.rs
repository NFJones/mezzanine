//! Typed runtime policy for the optional Iroh control transport.

use std::time::Duration;

use serde_json::Value;

use crate::error::{MezError, Result};

use super::{runtime_json_bool, runtime_json_object, runtime_json_string_array, runtime_json_u64};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeIrohIdentityPolicy {
    PerSession,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeIrohTransportPolicy {
    pub(crate) enabled: bool,
    pub(crate) outbound_enabled: bool,
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
}

impl Default for RuntimeIrohTransportPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            outbound_enabled: true,
            identity: RuntimeIrohIdentityPolicy::PerSession,
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

    let identity_name = string_value(iroh, "identity", "per_session")?;
    let identity = match identity_name.as_str() {
        "per_session" => RuntimeIrohIdentityPolicy::PerSession,
        _ => {
            return Err(MezError::config(
                "transport.iroh.identity must be per_session",
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
    Ok(RuntimeIrohTransportPolicy {
        enabled: bool_value(iroh, "enabled", defaults.enabled)?,
        outbound_enabled: bool_value(iroh, "outbound_enabled", defaults.outbound_enabled)?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iroh_transport_policy_defaults_disabled_and_materializes_explicit_values() {
        let defaults = runtime_iroh_transport_policy_from_config(&serde_json::json!({})).unwrap();
        assert_eq!(defaults, RuntimeIrohTransportPolicy::default());
        assert!(defaults.outbound_enabled);
        assert_eq!(defaults.max_streams_per_connection, 1);

        let policy = runtime_iroh_transport_policy_from_config(&serde_json::json!({
            "transport": { "iroh": {
                "enabled": true,
                "outbound_enabled": false,
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
                "idle_timeout_ms": 60000
            }}
        }))
        .unwrap();
        assert!(policy.enabled);
        assert!(!policy.outbound_enabled);
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
            serde_json::json!({"transport":{"iroh":{"idle_timeout_ms":999}}}),
        ] {
            assert!(
                runtime_iroh_transport_policy_from_config(&value).is_err(),
                "{value}"
            );
        }
    }
}
