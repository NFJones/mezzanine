//! Schema migrations from version 13 through the current version.

use super::ops::{
    copy_json_default_if_absent, copy_toml_default_if_absent, parse_json_compatible_config,
    remove_json_model_profile_dead_aliases, remove_json_path,
    remove_toml_model_profile_dead_aliases, remove_toml_path, removed_v14_paths, removed_v16_paths,
    set_json_path_value, set_toml_path_item,
};
use super::{ConfigFormat, DEFAULT_CONFIG_TOML, MezError, Result};

/// Applies the version 13 to version 14 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v13_to_v14(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v13_to_v14(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v13_to_v14(format, text),
    }
}

/// Applies the version 13 to version 14 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v13_to_v14(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;

    for path in removed_v14_paths() {
        remove_toml_path(&mut document, path)?;
    }
    remove_toml_model_profile_dead_aliases(&mut document);
    set_toml_path_item(&mut document, "version", toml_edit::value(14))?;

    Ok(document.to_string())
}

/// Applies the version 13 to version 14 migration to JSON and YAML config
/// files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v13_to_v14(
    format: ConfigFormat,
    text: &str,
) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;

    for path in removed_v14_paths() {
        remove_json_path(&mut document, path);
    }
    remove_json_model_profile_dead_aliases(&mut document);
    set_json_path_value(&mut document, "version", serde_json::json!(14))?;

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

/// Applies the version 14 to version 15 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v14_to_v15(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v14_to_v15(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v14_to_v15(format, text),
    }
}

/// Applies the version 14 to version 15 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v14_to_v15(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
    let default_document = DEFAULT_CONFIG_TOML
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid built-in TOML config: {error}")))?;

    copy_toml_default_if_absent(&mut document, &default_document, "issues")?;
    set_toml_path_item(&mut document, "version", toml_edit::value(15))?;

    Ok(document.to_string())
}

/// Applies the version 14 to version 15 migration to JSON and YAML config
/// files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v14_to_v15(
    format: ConfigFormat,
    text: &str,
) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;
    let default_table = toml::from_str::<toml::Table>(DEFAULT_CONFIG_TOML)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;
    let default_document = serde_json::to_value(default_table)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;

    copy_json_default_if_absent(&mut document, &default_document, "issues")?;
    set_json_path_value(&mut document, "version", serde_json::json!(15))?;

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

/// Applies the version 15 to version 16 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v15_to_v16(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v15_to_v16(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v15_to_v16(format, text),
    }
}

/// Applies the version 15 to version 16 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v15_to_v16(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;

    for path in removed_v16_paths() {
        remove_toml_path(&mut document, path)?;
    }
    set_toml_path_item(&mut document, "version", toml_edit::value(16))?;

    Ok(document.to_string())
}

/// Applies the version 15 to version 16 migration to JSON and YAML config
/// files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v15_to_v16(
    format: ConfigFormat,
    text: &str,
) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;

    for path in removed_v16_paths() {
        remove_json_path(&mut document, path);
    }
    set_json_path_value(&mut document, "version", serde_json::json!(16))?;

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

/// Applies the version 16 to version 17 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v16_to_v17(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v16_to_v17(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v16_to_v17(format, text),
    }
}

/// Applies the version 16 to version 17 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v16_to_v17(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
    let default_document = DEFAULT_CONFIG_TOML
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid built-in TOML config: {error}")))?;

    copy_toml_default_if_absent(
        &mut document,
        &default_document,
        "agents.local_action_executor",
    )?;
    set_toml_path_item(&mut document, "version", toml_edit::value(17))?;

    Ok(document.to_string())
}

/// Applies the version 16 to version 17 migration to JSON and YAML config
/// files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v16_to_v17(
    format: ConfigFormat,
    text: &str,
) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;
    let default_table = toml::from_str::<toml::Table>(DEFAULT_CONFIG_TOML)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;
    let default_document = serde_json::to_value(default_table)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;

    copy_json_default_if_absent(
        &mut document,
        &default_document,
        "agents.local_action_executor",
    )?;
    set_json_path_value(&mut document, "version", serde_json::json!(17))?;

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

/// Applies the version 17 to version 18 migration to all supported config
/// formats.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v17_to_v18(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v17_to_v18(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v17_to_v18(format, text),
    }
}

/// Applies the version 17 to version 18 migration to TOML config files.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v17_to_v18(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
    let default_document = DEFAULT_CONFIG_TOML
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid built-in TOML config: {error}")))?;

    copy_toml_default_if_absent(
        &mut document,
        &default_document,
        "terminal.agent_wrap_column_cap",
    )?;
    set_toml_path_item(&mut document, "version", toml_edit::value(18))?;

    Ok(document.to_string())
}

/// Applies the version 17 to version 18 migration to JSON and YAML config
/// files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v17_to_v18(
    format: ConfigFormat,
    text: &str,
) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;
    let default_table = toml::from_str::<toml::Table>(DEFAULT_CONFIG_TOML)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;
    let default_document = serde_json::to_value(default_table)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;

    copy_json_default_if_absent(
        &mut document,
        &default_document,
        "terminal.agent_wrap_column_cap",
    )?;
    set_json_path_value(&mut document, "version", serde_json::json!(18))?;

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

/// Applies the version 18 to version 19 migration.
///
/// The migration removes the deprecated local action executor setting because
/// pane-shell execution is now the only user-configurable local action mode.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v18_to_v19(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v18_to_v19(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v18_to_v19(format, text),
    }
}

/// Applies the version 18 to version 19 migration to TOML config files.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v18_to_v19(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;

    remove_toml_path(&mut document, "agents.local_action_executor")?;
    set_toml_path_item(&mut document, "version", toml_edit::value(19))?;

    Ok(document.to_string())
}

/// Applies the version 18 to version 19 migration to JSON and YAML config files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v18_to_v19(
    format: ConfigFormat,
    text: &str,
) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;

    remove_json_path(&mut document, "agents.local_action_executor");
    set_json_path_value(&mut document, "version", serde_json::json!(19))?;

    match format {
        ConfigFormat::Json => serde_json::to_string_pretty(&document)
            .map(|mut rendered| {
                rendered.push(char::from(10));
                rendered
            })
            .map_err(|error| MezError::config(format!("failed to render JSON config: {error}"))),
        ConfigFormat::Yaml => serde_norway::to_string(&document)
            .map_err(|error| MezError::config(format!("failed to render YAML config: {error}"))),
        ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
    }
}

/// Applies the version 19 to version 20 migration.
///
/// The migration removes the obsolete model-facing implementation-pressure
/// threshold. Deterministic controller policy now decides whether work should
/// continue without injecting pressure reminders into model context.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v19_to_v20(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v19_to_v20(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v19_to_v20(format, text),
    }
}

/// Applies the version 19 to version 20 migration to TOML config files.
pub(super) fn migrate_toml_v19_to_v20(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;

    remove_toml_path(
        &mut document,
        "agents.implementation_pressure_after_shell_actions",
    )?;
    set_toml_path_item(&mut document, "version", toml_edit::value(20))?;

    Ok(document.to_string())
}

/// Applies the version 19 to version 20 migration to JSON and YAML config files.
pub(super) fn migrate_json_compatible_v19_to_v20(
    format: ConfigFormat,
    text: &str,
) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;

    remove_json_path(
        &mut document,
        "agents.implementation_pressure_after_shell_actions",
    );
    set_json_path_value(&mut document, "version", serde_json::json!(20))?;

    match format {
        ConfigFormat::Json => serde_json::to_string_pretty(&document)
            .map(|mut rendered| {
                rendered.push(char::from(10));
                rendered
            })
            .map_err(|error| MezError::config(format!("failed to render JSON config: {error}"))),
        ConfigFormat::Yaml => serde_norway::to_string(&document)
            .map_err(|error| MezError::config(format!("failed to render YAML config: {error}"))),
        ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
    }
}
