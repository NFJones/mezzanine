//! Configuration schema v71 to v72 migration.
//!
//! Schema v72 adds dedicated Iroh connection-status theme colors. Existing
//! frame templates remain user-owned; generated defaults opt into the new
//! `iroh.status` field while migrated custom templates are preserved.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

const IROH_STATUS_COLORS: &[(&str, &str)] = &[
    ("iroh_status_good_fg", "primary_text"),
    ("iroh_status_good_bg", "primary"),
    ("iroh_status_degraded_fg", "tertiary_text"),
    ("iroh_status_degraded_bg", "tertiary"),
    ("iroh_status_poor_fg", "danger_text"),
    ("iroh_status_poor_bg", "danger"),
    ("iroh_status_unknown_fg", "muted_text"),
    ("iroh_status_unknown_bg", "muted"),
];

/// Adds dedicated Iroh status colors and advances the schema version.
pub(super) fn migrate_v71_to_v72(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            for (name, value) in IROH_STATUS_COLORS {
                set_toml_path_item(
                    &mut document,
                    &format!("theme.colors.{name}"),
                    toml_edit::value(*value),
                )?;
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(72))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            for (name, value) in IROH_STATUS_COLORS {
                set_json_path_value(
                    &mut document,
                    &format!("theme.colors.{name}"),
                    serde_json::json!(value),
                )?;
            }
            set_json_path_value(&mut document, "version", serde_json::json!(72))?;
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
