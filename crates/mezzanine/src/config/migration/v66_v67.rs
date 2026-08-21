//! Configuration schema v66 to v67 migration.
//!
//! Schema v67 adds the primary-user-only Iroh transport policy. Existing
//! configurations migrate to an explicitly disabled remote transport with no
//! public relay, lookup, port mapping, proxy, or system CA behavior enabled.

use super::ops::{
    copy_json_default_if_absent, copy_toml_default_if_absent, parse_json_compatible_config,
    set_json_path_value, set_toml_path_item,
};
use super::{ConfigFormat, DEFAULT_CONFIG_TOML, MezError, Result};

const IROH_DEFAULT_PATHS: &[&str] = &[
    "transport.iroh.enabled",
    "transport.iroh.identity",
    "transport.iroh.address_lookup",
    "transport.iroh.address_lookup_domain",
    "transport.iroh.relay_mode",
    "transport.iroh.relay_urls",
    "transport.iroh.direct_connections",
    "transport.iroh.port_mapping",
    "transport.iroh.proxy_from_env",
    "transport.iroh.system_ca_store",
    "transport.iroh.invitation_ttl_seconds",
    "transport.iroh.max_connections",
    "transport.iroh.max_streams_per_connection",
    "transport.iroh.setup_timeout_ms",
    "transport.iroh.idle_timeout_ms",
];

/// Adds the disabled Iroh transport policy and advances the schema version.
pub(super) fn migrate_v66_to_v67(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            let defaults = DEFAULT_CONFIG_TOML
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| {
                    MezError::config(format!("invalid default TOML config: {error}"))
                })?;
            for path in IROH_DEFAULT_PATHS {
                copy_toml_default_if_absent(&mut document, &defaults, path)?;
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(67))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            let defaults = toml::from_str::<toml::Value>(DEFAULT_CONFIG_TOML).map_err(|error| {
                MezError::config(format!("invalid default TOML config: {error}"))
            })?;
            let defaults = serde_json::to_value(defaults).map_err(|error| {
                MezError::config(format!("failed to convert default config: {error}"))
            })?;
            for path in IROH_DEFAULT_PATHS {
                copy_json_default_if_absent(&mut document, &defaults, path)?;
            }
            set_json_path_value(&mut document, "version", serde_json::json!(67))?;
            match format {
                ConfigFormat::Json => serde_json::to_string_pretty(&document)
                    .map(|mut rendered| {
                        rendered.push('\n');
                        rendered
                    })
                    .map_err(|error| {
                        MezError::config(format!("failed to render JSON config: {error}"))
                    }),
                ConfigFormat::Yaml => serde_norway::to_string(&document).map_err(|error| {
                    MezError::config(format!("failed to render YAML config: {error}"))
                }),
                ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
            }
        }
    }
}
