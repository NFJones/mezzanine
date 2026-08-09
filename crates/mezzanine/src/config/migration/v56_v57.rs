//! Configuration schema v56 to v57 migration.
//!
//! Schema v57 makes `xterm-256color` the default pane terminal identity.
//! Existing users who retained the former `screen-256color` default follow the
//! new default, while every other explicit terminal selection is preserved.

use super::ops::{
    parse_json_compatible_config, set_json_default_string_if_absent_or_old_default,
    set_json_path_value, set_toml_default_string_if_absent_or_old_default, set_toml_path_item,
};
use super::{ConfigFormat, MezError, Result};

/// Updates the former pane TERM default without overwriting explicit choices.
pub(super) fn migrate_v56_to_v57(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_default_string_if_absent_or_old_default(
                &mut document,
                "terminal.term",
                "screen-256color",
                "xterm-256color",
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(57))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_default_string_if_absent_or_old_default(
                &mut document,
                "terminal.term",
                "screen-256color",
                "xterm-256color",
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(57))?;
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
