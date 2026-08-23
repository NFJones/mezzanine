//! Configuration schema v70 to v71 migration.
//!
//! Schema v71 adds the ordered application-layer compression policy for Iroh.
//! Existing configurations receive deterministic defaults without enabling the
//! inbound listener or changing any Unix transport behavior.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Adds the default Iroh compression policy and advances the schema version.
pub(super) fn migrate_v70_to_v71(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            let mut codecs = toml_edit::Array::new();
            codecs.push("zstd");
            codecs.push("lz4");
            codecs.push("none");
            set_toml_path_item(
                &mut document,
                "transport.iroh.compression_codecs",
                toml_edit::value(codecs),
            )?;
            set_toml_path_item(
                &mut document,
                "transport.iroh.compression_min_bytes",
                toml_edit::value(512),
            )?;
            set_toml_path_item(
                &mut document,
                "transport.iroh.compression_zstd_level",
                toml_edit::value(3),
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(71))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_path_value(
                &mut document,
                "transport.iroh.compression_codecs",
                serde_json::json!(["zstd", "lz4", "none"]),
            )?;
            set_json_path_value(
                &mut document,
                "transport.iroh.compression_min_bytes",
                serde_json::json!(512),
            )?;
            set_json_path_value(
                &mut document,
                "transport.iroh.compression_zstd_level",
                serde_json::json!(3),
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(71))?;
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
