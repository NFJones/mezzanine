//! Configuration schema v72 to v73 migration.
//!
//! Schema v73 declares the disabled persistent-host policy and durable-lease
//! retention defaults. It changes only the Iroh identity mode declaration;
//! protected endpoint keys, trust records, and client profiles are not moved or
//! broadened by configuration migration.

use super::ops::{
    copy_json_default_if_absent, copy_toml_default_if_absent, parse_json_compatible_config,
    set_json_path_value, set_toml_path_item,
};
use super::{ConfigFormat, DEFAULT_CONFIG_TOML, MezError, Result};

const HOST_DEFAULT_PATHS: &[&str] = &[
    "host.enabled",
    "host.auto_start_local",
    "host.max_sessions",
    "host.max_live_sessions",
    "host.shutdown_timeout_ms",
    "host.checkpoint_interval_seconds",
    "host.recover_on_start",
    "host.default_session_policy",
    "host.leases.default_ttl_seconds",
    "host.leases.failed_retention_seconds",
    "host.leases.released_retention_seconds",
    "host.leases.max_per_remote_client",
];

/// Adds disabled host defaults and selects host-scoped Iroh identity metadata.
pub(super) fn migrate_v72_to_v73(format: ConfigFormat, text: &str) -> Result<String> {
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
            for path in HOST_DEFAULT_PATHS {
                copy_toml_default_if_absent(&mut document, &defaults, path)?;
            }
            set_toml_path_item(
                &mut document,
                "transport.iroh.identity",
                toml_edit::value("host"),
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(73))?;
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
            for path in HOST_DEFAULT_PATHS {
                copy_json_default_if_absent(&mut document, &defaults, path)?;
            }
            set_json_path_value(
                &mut document,
                "transport.iroh.identity",
                serde_json::json!("host"),
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(73))?;
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
