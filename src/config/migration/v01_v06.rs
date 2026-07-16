//! Schema migration dispatch and implementations through version 7.

use super::ops::*;
use super::v07_v12::{
    migrate_json_compatible_v7_to_v8, migrate_json_compatible_v8_to_v9,
    migrate_json_compatible_v9_to_v10, migrate_toml_v7_to_v8, migrate_toml_v8_to_v9,
    migrate_toml_v9_to_v10,
};
use super::*;

/// Applies the version 1 to version 2 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v1_to_v2(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v1_to_v2(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v1_to_v2(format, text),
    }
}

/// Applies the version 2 to version 3 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v2_to_v3(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v2_to_v3(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v2_to_v3(format, text),
    }
}

/// Applies the version 3 to version 4 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v3_to_v4(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v3_to_v4(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v3_to_v4(format, text),
    }
}

/// Applies the version 4 to version 5 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v4_to_v5(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v4_to_v5(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v4_to_v5(format, text),
    }
}

/// Applies the version 5 to version 6 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v5_to_v6(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v5_to_v6(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v5_to_v6(format, text),
    }
}

/// Applies the version 6 to version 7 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v6_to_v7(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v6_to_v7(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v6_to_v7(format, text),
    }
}

/// Applies the version 7 to version 8 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v7_to_v8(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v7_to_v8(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v7_to_v8(format, text),
    }
}

/// Applies the version 8 to version 9 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v8_to_v9(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v8_to_v9(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v8_to_v9(format, text),
    }
}

/// Applies the version 9 to version 10 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v9_to_v10(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v9_to_v10(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v9_to_v10(format, text),
    }
}

/// Applies the version 1 to version 2 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v1_to_v2(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
    let has_legacy_routing = toml_item_at(document.as_table(), "agents.auto_reasoning").is_some();
    let default_document = DEFAULT_CONFIG_TOML
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid built-in TOML config: {error}")))?;

    normalize_toml_rename(
        &mut document,
        "terminal.nested_muxxer",
        "terminal.nested_multiplexer",
    )?;
    for path in removed_v2_paths() {
        remove_toml_path(&mut document, path)?;
    }

    let openai_default_profile_compatible =
        toml_string_at(document.as_table(), "model_profiles.default.provider")
            .is_none_or(|provider| provider == "openai");

    for path in extract_config_values(ConfigFormat::Toml, DEFAULT_CONFIG_TOML).keys() {
        if should_backfill_v2_default_path(path, openai_default_profile_compatible)
            && !(has_legacy_routing && path == "agents.routing")
        {
            copy_toml_default_if_absent(&mut document, &default_document, path)?;
        }
    }

    ensure_toml_agent_preset_visible_field(&mut document)?;
    set_toml_path_item(&mut document, "version", toml_edit::value(2))?;

    Ok(document.to_string())
}

/// Applies the version 1 to version 2 migration to JSON and YAML config files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v1_to_v2(format: ConfigFormat, text: &str) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;
    let default_table = toml::from_str::<toml::Table>(DEFAULT_CONFIG_TOML)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;
    let default_document = serde_json::to_value(default_table)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;

    normalize_json_rename(
        &mut document,
        "terminal.nested_muxxer",
        "terminal.nested_multiplexer",
    )?;
    for path in removed_v2_paths() {
        remove_json_path(&mut document, path);
    }

    let openai_default_profile_compatible =
        json_string_at(&document, "model_profiles.default.provider")
            .is_none_or(|provider| provider == "openai");

    for path in extract_config_values(ConfigFormat::Toml, DEFAULT_CONFIG_TOML).keys() {
        if should_backfill_v2_default_path(path, openai_default_profile_compatible) {
            copy_json_default_if_absent(&mut document, &default_document, path)?;
        }
    }

    ensure_json_agent_preset_visible_field(&mut document)?;
    set_json_path_value(&mut document, "version", serde_json::json!(2))?;

    match format {
        ConfigFormat::Json => serde_json::to_string_pretty(&document)
            .map(|mut rendered| {
                rendered.push('\n');
                rendered
            })
            .map_err(|error| MezError::config(format!("failed to render JSON config: {error}"))),
        ConfigFormat::Yaml => serde_norway::to_string(&document)
            .map_err(|error| MezError::config(format!("failed to render YAML config: {error}"))),
        ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
    }
}

/// Applies the version 2 to version 3 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v2_to_v3(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;

    normalize_toml_rename(&mut document, "agents.auto_reasoning", "agents.routing")?;
    rename_toml_table_key(
        &mut document,
        "personalities",
        "auto_reasoning_enabled",
        "routing_enabled",
    )?;
    rename_toml_table_key(&mut document, "personalities", "auto_reasoning", "routing")?;
    rename_toml_string_array_value(
        &mut document,
        "frames.pane.visible_fields",
        "agent.auto_reasoning",
        "agent.routing",
    )?;
    rename_toml_string_array_value(
        &mut document,
        "frames.window.visible_fields",
        "agent.auto_reasoning",
        "agent.routing",
    )?;
    set_toml_path_item(&mut document, "version", toml_edit::value(3))?;

    Ok(document.to_string())
}

/// Applies the version 2 to version 3 migration to JSON and YAML config files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v2_to_v3(format: ConfigFormat, text: &str) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;

    normalize_json_rename(&mut document, "agents.auto_reasoning", "agents.routing")?;
    rename_json_table_key(
        &mut document,
        "personalities",
        "auto_reasoning_enabled",
        "routing_enabled",
    );
    rename_json_table_key(&mut document, "personalities", "auto_reasoning", "routing");
    rename_json_string_array_value(
        &mut document,
        "frames.pane.visible_fields",
        "agent.auto_reasoning",
        "agent.routing",
    );
    rename_json_string_array_value(
        &mut document,
        "frames.window.visible_fields",
        "agent.auto_reasoning",
        "agent.routing",
    );
    set_json_path_value(&mut document, "version", serde_json::json!(3))?;

    match format {
        ConfigFormat::Json => serde_json::to_string_pretty(&document)
            .map(|mut rendered| {
                rendered.push('\n');
                rendered
            })
            .map_err(|error| MezError::config(format!("failed to render JSON config: {error}"))),
        ConfigFormat::Yaml => serde_norway::to_string(&document)
            .map_err(|error| MezError::config(format!("failed to render YAML config: {error}"))),
        ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
    }
}

/// Applies the version 3 to version 4 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v3_to_v4(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;

    ensure_toml_agent_thinking_visible_field(&mut document)?;
    set_toml_path_item(&mut document, "version", toml_edit::value(4))?;

    Ok(document.to_string())
}

/// Applies the version 3 to version 4 migration to JSON and YAML config files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v3_to_v4(format: ConfigFormat, text: &str) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;

    ensure_json_agent_thinking_visible_field(&mut document)?;
    set_json_path_value(&mut document, "version", serde_json::json!(4))?;

    match format {
        ConfigFormat::Json => serde_json::to_string_pretty(&document)
            .map(|mut rendered| {
                rendered.push('\n');
                rendered
            })
            .map_err(|error| MezError::config(format!("failed to render JSON config: {error}"))),
        ConfigFormat::Yaml => serde_norway::to_string(&document)
            .map_err(|error| MezError::config(format!("failed to render YAML config: {error}"))),
        ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
    }
}

/// Applies the version 4 to version 5 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v4_to_v5(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;

    copy_toml_default_if_absent(
        &mut document,
        &DEFAULT_CONFIG_TOML
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| MezError::config(format!("invalid built-in TOML config: {error}")))?,
        "agents.implementation_pressure_after_shell_actions",
    )?;
    set_toml_path_item(&mut document, "version", toml_edit::value(5))?;

    Ok(document.to_string())
}

/// Applies the version 4 to version 5 migration to JSON and YAML config files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v4_to_v5(format: ConfigFormat, text: &str) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;
    let default_table = toml::from_str::<toml::Table>(DEFAULT_CONFIG_TOML)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;
    let default_document = serde_json::to_value(default_table)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;

    copy_json_default_if_absent(
        &mut document,
        &default_document,
        "agents.implementation_pressure_after_shell_actions",
    )?;
    set_json_path_value(&mut document, "version", serde_json::json!(5))?;

    match format {
        ConfigFormat::Json => serde_json::to_string_pretty(&document)
            .map(|mut rendered| {
                rendered.push('\n');
                rendered
            })
            .map_err(|error| MezError::config(format!("failed to render JSON config: {error}"))),
        ConfigFormat::Yaml => serde_norway::to_string(&document)
            .map_err(|error| MezError::config(format!("failed to render YAML config: {error}"))),
        ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
    }
}

/// Applies the version 5 to version 6 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v5_to_v6(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;

    remove_toml_path(&mut document, "agents.auto_compact")?;
    remove_toml_path(&mut document, "agents.auto_compact_threshold")?;
    set_toml_default_usize_if_absent_or_old_default(
        &mut document,
        "agents.implementation_pressure_after_shell_actions",
        8,
        5,
    )?;
    set_toml_path_item(&mut document, "version", toml_edit::value(6))?;

    Ok(document.to_string())
}

/// Applies the version 5 to version 6 migration to JSON and YAML config files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v5_to_v6(format: ConfigFormat, text: &str) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;

    remove_json_path(&mut document, "agents.auto_compact");
    remove_json_path(&mut document, "agents.auto_compact_threshold");
    set_json_default_usize_if_absent_or_old_default(
        &mut document,
        "agents.implementation_pressure_after_shell_actions",
        8,
        5,
    )?;
    set_json_path_value(&mut document, "version", serde_json::json!(6))?;

    match format {
        ConfigFormat::Json => serde_json::to_string_pretty(&document)
            .map(|mut rendered| {
                rendered.push('\n');
                rendered
            })
            .map_err(|error| MezError::config(format!("failed to render JSON config: {error}"))),
        ConfigFormat::Yaml => serde_norway::to_string(&document)
            .map_err(|error| MezError::config(format!("failed to render YAML config: {error}"))),
        ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
    }
}

/// Applies the version 6 to version 7 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v6_to_v7(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;

    set_toml_default_usize_if_absent_or_old_default(
        &mut document,
        "agents.implementation_pressure_after_shell_actions",
        5,
        3,
    )?;
    update_toml_model_profile_context_window_default(
        &mut document,
        "deepseek-default",
        "deepseek",
        "deepseek-v4-pro",
        524_288,
        1_000_000,
    )?;
    update_toml_model_profile_context_window_default(
        &mut document,
        "deepseek-fast",
        "deepseek",
        "deepseek-v4-flash",
        524_288,
        1_000_000,
    )?;
    set_toml_path_item(&mut document, "version", toml_edit::value(7))?;

    Ok(document.to_string())
}

/// Applies the version 6 to version 7 migration to JSON and YAML config files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v6_to_v7(format: ConfigFormat, text: &str) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;

    set_json_default_usize_if_absent_or_old_default(
        &mut document,
        "agents.implementation_pressure_after_shell_actions",
        5,
        3,
    )?;
    update_json_model_profile_context_window_default(
        &mut document,
        "deepseek-default",
        "deepseek",
        "deepseek-v4-pro",
        524_288,
        1_000_000,
    )?;
    update_json_model_profile_context_window_default(
        &mut document,
        "deepseek-fast",
        "deepseek",
        "deepseek-v4-flash",
        524_288,
        1_000_000,
    )?;
    set_json_path_value(&mut document, "version", serde_json::json!(7))?;

    match format {
        ConfigFormat::Json => serde_json::to_string_pretty(&document)
            .map(|mut rendered| {
                rendered.push('\n');
                rendered
            })
            .map_err(|error| MezError::config(format!("failed to render JSON config: {error}"))),
        ConfigFormat::Yaml => serde_norway::to_string(&document)
            .map_err(|error| MezError::config(format!("failed to render YAML config: {error}"))),
        ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
    }
}
