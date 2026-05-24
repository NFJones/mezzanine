//! Config schema migration implementation.
//!
//! This module owns durable primary-config upgrades. Runtime config loading
//! calls this before normal validation so user config files can move forward
//! through schema versions while project overlays remain validated against the
//! current schema.

use super::{
    ConfigFormat, DEFAULT_CONFIG_TOML, MezError, Path, Result, extract_config_values, fs,
    write_private_config_file,
};

/// The newest configuration schema version understood by this binary.
pub const CURRENT_CONFIG_SCHEMA_VERSION: u64 = 3;

/// Describes the result of migrating one configuration document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigMigrationPlan {
    /// The schema version detected before migration.
    pub from_version: u64,
    /// The schema version after applying all known migrations.
    pub to_version: u64,
    /// Whether the migration produced different config text.
    pub changed: bool,
    /// The migrated configuration text.
    pub text: String,
}

/// Migrates a primary configuration file to the current schema version.
///
/// # Parameters
/// - `path`: The primary config file to inspect and update if needed.
pub fn migrate_config_file(path: &Path) -> Result<ConfigMigrationPlan> {
    let format = ConfigFormat::from_path(path)?;
    let text = fs::read_to_string(path)?;
    let plan = migrate_config_text(format, &text)?;
    if plan.changed {
        write_private_config_file(path, &plan.text)?;
    }
    Ok(plan)
}

/// Migrates one configuration document to the current schema version.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub fn migrate_config_text(format: ConfigFormat, text: &str) -> Result<ConfigMigrationPlan> {
    let from_version = config_schema_version(format, text)?;
    if from_version > CURRENT_CONFIG_SCHEMA_VERSION {
        return Err(MezError::config(format!(
            "configuration schema version {from_version} is newer than this mez binary supports ({CURRENT_CONFIG_SCHEMA_VERSION})"
        )));
    }

    let mut current_version = from_version;
    let mut current_text = text.to_string();
    while current_version < CURRENT_CONFIG_SCHEMA_VERSION {
        match current_version {
            1 => {
                current_text = migrate_v1_to_v2(format, &current_text)?;
                current_version = 2;
            }
            2 => {
                current_text = migrate_v2_to_v3(format, &current_text)?;
                current_version = 3;
            }
            unsupported => {
                return Err(MezError::config(format!(
                    "no migration path is available from configuration schema version {unsupported}"
                )));
            }
        }
    }

    Ok(ConfigMigrationPlan {
        from_version,
        to_version: CURRENT_CONFIG_SCHEMA_VERSION,
        changed: current_text != text,
        text: current_text,
    })
}

/// Reads the schema version recorded in one config document.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to inspect.
fn config_schema_version(format: ConfigFormat, text: &str) -> Result<u64> {
    let values = extract_config_values(format, text);
    parse_config_schema_version(values.get("version").map(String::as_str))
}

/// Parses an optional config schema version value.
///
/// # Parameters
/// - `value`: The raw extracted version value, if present.
pub(super) fn parse_config_schema_version(value: Option<&str>) -> Result<u64> {
    let Some(value) = value else {
        return Ok(1);
    };
    match value.parse::<u64>() {
        Ok(version) if version > 0 => Ok(version),
        _ => Err(MezError::config(
            "configuration schema version must be a positive integer",
        )),
    }
}

/// Applies the version 1 to version 2 migration.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
fn migrate_v1_to_v2(format: ConfigFormat, text: &str) -> Result<String> {
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
fn migrate_v2_to_v3(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v2_to_v3(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_compatible_v2_to_v3(format, text),
    }
}

/// Applies the version 1 to version 2 migration to TOML while preserving
/// comments and formatting where `toml_edit` can retain them.
///
/// # Parameters
/// - `text`: The TOML document text to migrate.
fn migrate_toml_v1_to_v2(text: &str) -> Result<String> {
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
fn migrate_json_compatible_v1_to_v2(format: ConfigFormat, text: &str) -> Result<String> {
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
fn migrate_toml_v2_to_v3(text: &str) -> Result<String> {
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
fn migrate_json_compatible_v2_to_v3(format: ConfigFormat, text: &str) -> Result<String> {
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

/// Parses a JSON or YAML config file into a JSON value tree.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to parse.
fn parse_json_compatible_config(format: ConfigFormat, text: &str) -> Result<serde_json::Value> {
    let value = match format {
        ConfigFormat::Json => serde_json::from_str(text)
            .map_err(|error| MezError::config(format!("invalid JSON config: {error}")))?,
        ConfigFormat::Yaml => {
            let value = serde_norway::from_str::<serde_norway::Value>(text)
                .map_err(|error| MezError::config(format!("invalid YAML config: {error}")))?;
            serde_json::to_value(value)
                .map_err(|error| MezError::config(format!("invalid YAML config: {error}")))?
        }
        ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
    };
    if value.is_object() {
        Ok(value)
    } else {
        Err(MezError::config(
            "configuration document root must be a mapping",
        ))
    }
}

/// Reports the v2 default paths that should be inserted into older configs.
///
/// # Parameters
/// - `path`: The default scalar path being considered.
/// - `openai_default_profile_compatible`: Whether the existing `default`
///   model profile can safely back OpenAI model presets.
fn should_backfill_v2_default_path(path: &str, openai_default_profile_compatible: bool) -> bool {
    if path == "version" {
        return false;
    }
    if path.starts_with("model_profiles.default.") || path.starts_with("model_presets.openai.") {
        return openai_default_profile_compatible;
    }
    true
}

/// Returns config paths removed from the current schema during v2 migration.
fn removed_v2_paths() -> &'static [&'static str] {
    &[
        "session.default_command",
        "shell.path",
        "shell.executable",
        "shell.command",
    ]
}

/// Copies one default TOML item into the target document if it is absent.
///
/// # Parameters
/// - `target`: The config document being migrated.
/// - `defaults`: The built-in default document.
/// - `path`: The dotted config path to copy.
fn copy_toml_default_if_absent(
    target: &mut toml_edit::DocumentMut,
    defaults: &toml_edit::DocumentMut,
    path: &str,
) -> Result<()> {
    if toml_item_at(target.as_table(), path).is_some() {
        return Ok(());
    }
    let Some(item) = toml_item_at(defaults.as_table(), path).cloned() else {
        return Ok(());
    };
    set_toml_path_item(target, path, item)
}

/// Copies one default JSON-compatible value into the target tree if absent.
///
/// # Parameters
/// - `target`: The config value tree being migrated.
/// - `defaults`: The built-in default value tree.
/// - `path`: The dotted config path to copy.
fn copy_json_default_if_absent(
    target: &mut serde_json::Value,
    defaults: &serde_json::Value,
    path: &str,
) -> Result<()> {
    if json_value_at(target, path).is_some() {
        return Ok(());
    }
    let Some(value) = json_value_at(defaults, path).cloned() else {
        return Ok(());
    };
    set_json_path_value(target, path, value)
}

/// Normalizes one renamed TOML key, preserving canonical-key precedence.
///
/// # Parameters
/// - `document`: The TOML document being migrated.
/// - `old_path`: The historical dotted key.
/// - `new_path`: The canonical dotted key.
fn normalize_toml_rename(
    document: &mut toml_edit::DocumentMut,
    old_path: &str,
    new_path: &str,
) -> Result<()> {
    if toml_item_at(document.as_table(), new_path).is_none()
        && let Some(item) = toml_item_at(document.as_table(), old_path).cloned()
    {
        set_toml_path_item(document, new_path, item)?;
    }
    remove_toml_path(document, old_path)
}

/// Normalizes one renamed JSON-compatible key, preserving canonical-key
/// precedence.
///
/// # Parameters
/// - `document`: The JSON-compatible document being migrated.
/// - `old_path`: The historical dotted key.
/// - `new_path`: The canonical dotted key.
fn normalize_json_rename(
    document: &mut serde_json::Value,
    old_path: &str,
    new_path: &str,
) -> Result<()> {
    if json_value_at(document, new_path).is_none()
        && let Some(value) = json_value_at(document, old_path).cloned()
    {
        set_json_path_value(document, new_path, value)?;
    }
    remove_json_path(document, old_path);
    Ok(())
}

/// Ensures the pane visible-field list exposes the agent preset selector.
///
/// # Parameters
/// - `document`: The TOML document being migrated.
fn ensure_toml_agent_preset_visible_field(document: &mut toml_edit::DocumentMut) -> Result<()> {
    copy_toml_default_if_absent(
        document,
        &DEFAULT_CONFIG_TOML
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| MezError::config(format!("invalid built-in TOML config: {error}")))?,
        "frames.pane.visible_fields",
    )?;
    let segments = split_config_path("frames.pane.visible_fields");
    let Some(parent) = toml_parent_table_mut(document.as_table_mut(), &segments[..2], false)?
    else {
        return Ok(());
    };
    let Some(toml_edit::Item::Value(value)) = parent.get_mut("visible_fields") else {
        return Ok(());
    };
    let Some(array) = value.as_array_mut() else {
        return Ok(());
    };
    if !array
        .iter()
        .any(|item| item.as_str() == Some("agent.preset"))
    {
        array.push("agent.preset");
    }
    Ok(())
}

/// Ensures the pane visible-field list exposes the agent preset selector.
///
/// # Parameters
/// - `document`: The JSON-compatible document being migrated.
fn ensure_json_agent_preset_visible_field(document: &mut serde_json::Value) -> Result<()> {
    copy_json_default_if_absent(
        document,
        &{
            let default_table =
                toml::from_str::<toml::Table>(DEFAULT_CONFIG_TOML).map_err(|error| {
                    MezError::config(format!("invalid built-in default config: {error}"))
                })?;
            serde_json::to_value(default_table).map_err(|error| {
                MezError::config(format!("invalid built-in default config: {error}"))
            })?
        },
        "frames.pane.visible_fields",
    )?;
    let Some(value) = json_value_at_mut(document, "frames.pane.visible_fields") else {
        return Ok(());
    };
    let Some(array) = value.as_array_mut() else {
        return Ok(());
    };
    if !array
        .iter()
        .any(|item| item.as_str() == Some("agent.preset"))
    {
        array.push(serde_json::json!("agent.preset"));
    }
    Ok(())
}

/// Renames one key inside every TOML table stored under a parent table.
///
/// # Parameters
/// - `document`: The TOML document being migrated.
/// - `parent_path`: The parent table containing keyed child tables.
/// - `old_key`: The historical child-table key.
/// - `new_key`: The canonical child-table key.
fn rename_toml_table_key(
    document: &mut toml_edit::DocumentMut,
    parent_path: &str,
    old_key: &str,
    new_key: &str,
) -> Result<()> {
    let segments = split_config_path(parent_path);
    let Some(parent) = toml_parent_table_mut(document.as_table_mut(), &segments, false)? else {
        return Ok(());
    };
    for (_name, item) in parent.iter_mut() {
        let Some(table) = item.as_table_mut() else {
            continue;
        };
        if !table.contains_key(new_key)
            && let Some(value) = table.get(old_key).cloned()
        {
            table.insert(new_key, value);
        }
        table.remove(old_key);
    }
    Ok(())
}

/// Rewrites one string value inside a TOML string array if present.
///
/// # Parameters
/// - `document`: The TOML document being migrated.
/// - `path`: The dotted string-array path.
/// - `old_value`: The historical field name.
/// - `new_value`: The canonical field name.
fn rename_toml_string_array_value(
    document: &mut toml_edit::DocumentMut,
    path: &str,
    old_value: &str,
    new_value: &str,
) -> Result<()> {
    let segments = split_config_path(path);
    let Some(leaf) = segments.last() else {
        return Ok(());
    };
    let Some(parent) = toml_parent_table_mut(
        document.as_table_mut(),
        &segments[..segments.len().saturating_sub(1)],
        false,
    )?
    else {
        return Ok(());
    };
    let Some(toml_edit::Item::Value(value)) = parent.get_mut(leaf) else {
        return Ok(());
    };
    let Some(array) = value.as_array_mut() else {
        return Ok(());
    };
    for item in array.iter_mut() {
        if item.as_str() == Some(old_value) {
            *item = toml_edit::Value::from(new_value);
        }
    }
    Ok(())
}

/// Renames one key inside every JSON-compatible object under a parent object.
///
/// # Parameters
/// - `document`: The JSON-compatible document being migrated.
/// - `parent_path`: The parent object containing keyed child objects.
/// - `old_key`: The historical child-object key.
/// - `new_key`: The canonical child-object key.
fn rename_json_table_key(
    document: &mut serde_json::Value,
    parent_path: &str,
    old_key: &str,
    new_key: &str,
) {
    let Some(parent) = json_value_at_mut(document, parent_path) else {
        return;
    };
    let Some(parent_object) = parent.as_object_mut() else {
        return;
    };
    for value in parent_object.values_mut() {
        let Some(object) = value.as_object_mut() else {
            continue;
        };
        if !object.contains_key(new_key)
            && let Some(old_value) = object.get(old_key).cloned()
        {
            object.insert(new_key.to_string(), old_value);
        }
        object.remove(old_key);
    }
}

/// Rewrites one string value inside a JSON-compatible string array if present.
///
/// # Parameters
/// - `document`: The JSON-compatible document being migrated.
/// - `path`: The dotted string-array path.
/// - `old_value`: The historical field name.
/// - `new_value`: The canonical field name.
fn rename_json_string_array_value(
    document: &mut serde_json::Value,
    path: &str,
    old_value: &str,
    new_value: &str,
) {
    let Some(value) = json_value_at_mut(document, path) else {
        return;
    };
    let Some(array) = value.as_array_mut() else {
        return;
    };
    for item in array.iter_mut() {
        if item.as_str() == Some(old_value) {
            *item = serde_json::Value::String(new_value.to_string());
        }
    }
}

/// Reads a string TOML value at one dotted path.
///
/// # Parameters
/// - `table`: The TOML table to inspect.
/// - `path`: The dotted config path to read.
fn toml_string_at(table: &toml_edit::Table, path: &str) -> Option<String> {
    toml_item_at(table, path)?
        .as_value()
        .and_then(toml_edit::Value::as_str)
        .map(ToString::to_string)
}

/// Reads a string JSON-compatible value at one dotted path.
///
/// # Parameters
/// - `document`: The JSON-compatible value tree to inspect.
/// - `path`: The dotted config path to read.
fn json_string_at(document: &serde_json::Value, path: &str) -> Option<String> {
    json_value_at(document, path)?
        .as_str()
        .map(ToString::to_string)
}

/// Reads a TOML item at one dotted path.
///
/// # Parameters
/// - `table`: The TOML table to inspect.
/// - `path`: The dotted config path to read.
fn toml_item_at<'a>(table: &'a toml_edit::Table, path: &str) -> Option<&'a toml_edit::Item> {
    let mut segments = split_config_path(path).into_iter();
    let first = segments.next()?;
    let mut item = table.get(&first)?;
    for segment in segments {
        item = item.as_table()?.get(&segment)?;
    }
    Some(item)
}

/// Inserts or replaces a TOML item at one dotted path.
///
/// # Parameters
/// - `document`: The TOML document to mutate.
/// - `path`: The dotted config path to write.
/// - `item`: The TOML item to store at the target path.
fn set_toml_path_item(
    document: &mut toml_edit::DocumentMut,
    path: &str,
    item: toml_edit::Item,
) -> Result<()> {
    let segments = split_config_path(path);
    let leaf = segments
        .last()
        .ok_or_else(|| MezError::config("configuration path must not be empty"))?
        .clone();
    let parent_segments = &segments[..segments.len().saturating_sub(1)];
    let parent = toml_parent_table_mut(document.as_table_mut(), parent_segments, true)?
        .expect("create=true returns a parent table");
    parent.insert(&leaf, item);
    Ok(())
}

/// Removes one TOML item if present.
///
/// # Parameters
/// - `document`: The TOML document to mutate.
/// - `path`: The dotted config path to remove.
fn remove_toml_path(document: &mut toml_edit::DocumentMut, path: &str) -> Result<()> {
    let segments = split_config_path(path);
    let Some(leaf) = segments.last() else {
        return Ok(());
    };
    if let Some(parent) = toml_parent_table_mut(
        document.as_table_mut(),
        &segments[..segments.len().saturating_sub(1)],
        false,
    )? {
        parent.remove(leaf);
    }
    Ok(())
}

/// Locates a mutable TOML parent table for a dotted path.
///
/// # Parameters
/// - `table`: The root or parent table to traverse.
/// - `segments`: The parent path segments to walk.
/// - `create`: Whether missing parent tables should be created.
fn toml_parent_table_mut<'a>(
    table: &'a mut toml_edit::Table,
    segments: &[String],
    create: bool,
) -> Result<Option<&'a mut toml_edit::Table>> {
    let Some((segment, rest)) = segments.split_first() else {
        return Ok(Some(table));
    };
    if table.get(segment).is_none() {
        if !create {
            return Ok(None);
        }
        let mut child = toml_edit::Table::new();
        child.set_implicit(true);
        table.insert(segment, toml_edit::Item::Table(child));
    }
    let item = table
        .get_mut(segment)
        .ok_or_else(|| MezError::config("configuration migration parent could not be created"))?;
    match item {
        toml_edit::Item::Table(child) => toml_parent_table_mut(child, rest, create),
        _ => Err(MezError::config(format!(
            "configuration path `{}` is nested below a scalar",
            segments.join(".")
        ))),
    }
}

/// Reads a JSON-compatible value at one dotted path.
///
/// # Parameters
/// - `document`: The value tree to inspect.
/// - `path`: The dotted config path to read.
fn json_value_at<'a>(document: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut value = document;
    for segment in split_config_path(path) {
        value = value.as_object()?.get(&segment)?;
    }
    Some(value)
}

/// Reads a mutable JSON-compatible value at one dotted path.
///
/// # Parameters
/// - `document`: The value tree to inspect.
/// - `path`: The dotted config path to read.
fn json_value_at_mut<'a>(
    document: &'a mut serde_json::Value,
    path: &str,
) -> Option<&'a mut serde_json::Value> {
    let mut value = document;
    for segment in split_config_path(path) {
        value = value.as_object_mut()?.get_mut(&segment)?;
    }
    Some(value)
}

/// Inserts or replaces a JSON-compatible value at one dotted path.
///
/// # Parameters
/// - `document`: The value tree to mutate.
/// - `path`: The dotted config path to write.
/// - `value`: The value to store.
fn set_json_path_value(
    document: &mut serde_json::Value,
    path: &str,
    value: serde_json::Value,
) -> Result<()> {
    let segments = split_config_path(path);
    let leaf = segments
        .last()
        .ok_or_else(|| MezError::config("configuration path must not be empty"))?
        .clone();
    let mut current = document;
    for segment in &segments[..segments.len().saturating_sub(1)] {
        if !current.is_object() {
            return Err(MezError::config(format!(
                "configuration path `{}` is nested below a scalar",
                segments.join(".")
            )));
        }
        current
            .as_object_mut()
            .expect("object checked above")
            .entry(segment.clone())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        current = current
            .as_object_mut()
            .expect("object checked above")
            .get_mut(segment)
            .expect("entry inserted above");
    }
    let Some(object) = current.as_object_mut() else {
        return Err(MezError::config(format!(
            "configuration path `{}` is nested below a scalar",
            segments.join(".")
        )));
    };
    object.insert(leaf, value);
    Ok(())
}

/// Removes one JSON-compatible value if present.
///
/// # Parameters
/// - `document`: The value tree to mutate.
/// - `path`: The dotted config path to remove.
fn remove_json_path(document: &mut serde_json::Value, path: &str) {
    let segments = split_config_path(path);
    let Some(leaf) = segments.last() else {
        return;
    };
    let mut current = document;
    for segment in &segments[..segments.len().saturating_sub(1)] {
        let Some(next) = current
            .as_object_mut()
            .and_then(|object| object.get_mut(segment))
        else {
            return;
        };
        current = next;
    }
    if let Some(object) = current.as_object_mut() {
        object.remove(leaf);
    }
}

/// Splits one validated config path into owned segments.
///
/// # Parameters
/// - `path`: The dotted config path to split.
fn split_config_path(path: &str) -> Vec<String> {
    path.split('.').map(ToString::to_string).collect()
}
