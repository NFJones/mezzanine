//! Configuration schema v79 to v80 migration.
//!
//! Schema v80 adds a separately opt-in X11 forwarding policy beneath the Iroh
//! transport. Existing remote transport and application behavior remain
//! unchanged because forwarding is disabled by default.

use super::ops::{
    copy_json_default_if_absent, copy_toml_default_if_absent, parse_json_compatible_config,
    set_json_path_value, set_toml_path_item,
};
use super::{ConfigFormat, DEFAULT_CONFIG_TOML, MezError, Result};

/// Defaulted X11 policy paths introduced by schema v80.
const X11_DEFAULT_PATHS: &[&str] = &[
    "transport.iroh.x11.enabled",
    "transport.iroh.x11.allow_trusted",
    "transport.iroh.x11.max_connections_per_route",
    "transport.iroh.x11.setup_timeout_ms",
];

/// Adds the disabled X11 forwarding policy and advances the document to v80.
pub(super) fn migrate_v79_to_v80(format: ConfigFormat, text: &str) -> Result<String> {
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
            for path in X11_DEFAULT_PATHS {
                copy_toml_default_if_absent(&mut document, &defaults, path)?;
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(80))?;
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
            for path in X11_DEFAULT_PATHS {
                copy_json_default_if_absent(&mut document, &defaults, path)?;
            }
            set_json_path_value(&mut document, "version", serde_json::json!(80))?;
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
