//! Schema migration implementations from version 7 through version 13.

use super::ops::*;
use super::*;

/// Applies the version 7 to version 8 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v7_to_v8(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;

    backfill_toml_provider_api_defaults(&mut document)?;
    set_toml_path_item(&mut document, "version", toml_edit::value(8))?;

    Ok(document.to_string())
}

/// Applies the version 7 to version 8 migration to JSON and YAML config files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v7_to_v8(format: ConfigFormat, text: &str) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;

    backfill_json_provider_api_defaults(&mut document);
    set_json_path_value(&mut document, "version", serde_json::json!(8))?;

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

/// Applies the version 8 to version 9 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v8_to_v9(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;

    set_toml_path_item(
        &mut document,
        "auth.provider_refresh_leeway_seconds",
        toml_edit::value(crate::auth::DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS as i64),
    )?;
    set_toml_path_item(&mut document, "version", toml_edit::value(9))?;

    Ok(document.to_string())
}

/// Applies the version 8 to version 9 migration to JSON and YAML config files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v8_to_v9(format: ConfigFormat, text: &str) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;

    set_json_path_value(
        &mut document,
        "auth.provider_refresh_leeway_seconds",
        serde_json::json!(crate::auth::DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS),
    )?;
    set_json_path_value(&mut document, "version", serde_json::json!(9))?;

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

/// Applies the version 9 to version 10 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v9_to_v10(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
    let default_document = DEFAULT_CONFIG_TOML
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid built-in TOML config: {error}")))?;

    copy_toml_default_if_absent(&mut document, &default_document, "terminal.emoji_width")?;
    set_toml_path_item(&mut document, "version", toml_edit::value(10))?;

    Ok(document.to_string())
}

/// Applies the version 9 to version 10 migration to JSON and YAML config files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v9_to_v10(
    format: ConfigFormat,
    text: &str,
) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;
    let default_table = toml::from_str::<toml::Table>(DEFAULT_CONFIG_TOML)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;
    let default_document = serde_json::to_value(default_table)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;

    copy_json_default_if_absent(&mut document, &default_document, "terminal.emoji_width")?;
    set_json_path_value(&mut document, "version", serde_json::json!(10))?;

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

/// Applies the version 10 to version 11 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v10_to_v11(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v10_to_v11(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v10_to_v11(format, text),
    }
}

/// Applies the version 10 to version 11 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v10_to_v11(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
    let default_document = DEFAULT_CONFIG_TOML
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid built-in TOML config: {error}")))?;

    copy_toml_default_if_absent(&mut document, &default_document, "memory")?;
    set_toml_path_item(&mut document, "version", toml_edit::value(11))?;

    Ok(document.to_string())
}

/// Applies the version 10 to version 11 migration to JSON and YAML config files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v10_to_v11(
    format: ConfigFormat,
    text: &str,
) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;
    let default_table = toml::from_str::<toml::Table>(DEFAULT_CONFIG_TOML)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;
    let default_document = serde_json::to_value(default_table)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;

    copy_json_default_if_absent(&mut document, &default_document, "memory")?;
    set_json_path_value(&mut document, "version", serde_json::json!(11))?;

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

/// Applies the version 11 to version 12 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v11_to_v12(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v11_to_v12(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v11_to_v12(format, text),
    }
}

/// Applies the version 11 to version 12 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v11_to_v12(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;

    for path in removed_v12_paths() {
        remove_toml_path(&mut document, path)?;
    }
    set_toml_path_item(&mut document, "version", toml_edit::value(12))?;

    Ok(document.to_string())
}

/// Applies the version 11 to version 12 migration to JSON and YAML config files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v11_to_v12(
    format: ConfigFormat,
    text: &str,
) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;

    for path in removed_v12_paths() {
        remove_json_path(&mut document, path);
    }
    set_json_path_value(&mut document, "version", serde_json::json!(12))?;

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

/// Applies the version 12 to version 13 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_v12_to_v13(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v12_to_v13(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v12_to_v13(format, text),
    }
}

/// Applies the version 12 to version 13 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
pub(super) fn migrate_toml_v12_to_v13(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
    let default_document = DEFAULT_CONFIG_TOML
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid built-in TOML config: {error}")))?;

    copy_toml_default_if_absent(
        &mut document,
        &default_document,
        "terminal.shell_output_preview_lines",
    )?;
    set_toml_path_item(&mut document, "version", toml_edit::value(13))?;

    Ok(document.to_string())
}

/// Applies the version 12 to version 13 migration to JSON and YAML config
/// files.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub(super) fn migrate_json_compatible_v12_to_v13(
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
        "terminal.shell_output_preview_lines",
    )?;
    set_json_path_value(&mut document, "version", serde_json::json!(13))?;

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
