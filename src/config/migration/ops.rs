//! Shared JSON, TOML, rename, defaulting, and dotted-path migration operations.

use super::*;

/// Parses a JSON or YAML config file into a JSON value tree.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to parse.
pub(super) fn parse_json_compatible_config(
    format: ConfigFormat,
    text: &str,
) -> Result<serde_json::Value> {
    match format {
        ConfigFormat::Json | ConfigFormat::Yaml => parse_config_json_object(format, text),
        ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
    }
}

/// Reports the v2 default paths that should be inserted into older configs.
///
/// # Parameters
/// - `path`: The default scalar path being considered.
/// - `openai_default_profile_compatible`: Whether the existing `default`
///   model profile can safely back OpenAI model presets.
pub(super) fn should_backfill_v2_default_path(
    path: &str,
    openai_default_profile_compatible: bool,
) -> bool {
    if path == "version" {
        return false;
    }
    if path.starts_with("model_profiles.default.") || path.starts_with("model_presets.openai.") {
        return openai_default_profile_compatible;
    }
    true
}

/// Returns config paths removed from the current schema during v2 migration.
pub(super) fn removed_v2_paths() -> &'static [&'static str] {
    &[
        "session.default_command",
        "shell.path",
        "shell.executable",
        "shell.command",
    ]
}

/// Returns config paths removed from the current schema during v12 migration.
pub(super) fn removed_v12_paths() -> &'static [&'static str] {
    &[]
}

/// Returns config paths removed from the current schema during v14 migration.
pub(super) fn removed_v14_paths() -> &'static [&'static str] {
    &[
        "auth.auth_file",
        "auth.credential_store",
        "auth.default_profile",
    ]
}

/// Returns config paths removed from the current schema during v16 migration.
pub(super) fn removed_v16_paths() -> &'static [&'static str] {
    &[
        "session",
        "shell",
        "layout",
        "session.detach_behavior",
        "session.reattach_behavior",
        "session.empty_session_behavior",
        "session.restore_strategy",
        "shell.login",
        "shell.interactive",
        "shell.integration",
        "shell.integration_mode",
        "shell.default_working_directory",
        "shell.env",
        "shell.tool_discovery",
        "shell.tool_cache",
        "shell.fallback_behavior",
        "layout.default",
        "layout.resize_policy",
        "layout.close_policy",
        "layout.min_pane_columns",
        "layout.min_pane_rows",
        "history.search_mode",
        "memory.storage",
        "memory.database_path",
        "memory.max_injected_records",
        "memory.max_injected_bytes",
        "memory.candidate_limit",
        "issues.storage",
        "agents.prompt_profile",
        "agents.default_agent_role",
        "message_protocol",
        "control",
        "snapshots",
        "message_protocol.enabled",
        "message_protocol.endpoint",
        "message_protocol.retention_messages",
        "message_protocol.retention_bytes",
        "message_protocol.allow_remote_bridges",
        "control.endpoint",
        "control.socket_path",
        "control.tcp_bind",
        "control.tcp_enabled",
        "control.auth_token_file",
        "control.observer_policy",
        "snapshots.enabled",
        "snapshots.path",
        "snapshots.on_detach",
        "snapshots.on_interval_seconds",
        "snapshots.on_agent_turn",
        "snapshots.retention_count",
        "audit.redact_secrets",
    ]
}

/// Removes model-profile compatibility aliases deleted in schema v14.
///
/// The aliases were accepted for compatibility but only copied into provider
/// options; they did not participate in runtime fallback compatibility,
/// routing, provider requests, or approval enforcement.
pub(super) fn remove_toml_model_profile_dead_aliases(document: &mut toml_edit::DocumentMut) {
    let Some(profiles) = document
        .as_table_mut()
        .get_mut("model_profiles")
        .and_then(toml_edit::Item::as_table_mut)
    else {
        return;
    };
    for (_name, profile) in profiles.iter_mut() {
        if let Some(profile_table) = profile.as_table_mut() {
            profile_table.remove("privacy");
            profile_table.remove("approval");
        }
    }
}

/// Removes model-profile compatibility aliases deleted in schema v14.
pub(super) fn remove_json_model_profile_dead_aliases(document: &mut serde_json::Value) {
    let Some(profiles) = document
        .get_mut("model_profiles")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    for profile in profiles.values_mut() {
        if let Some(profile_object) = profile.as_object_mut() {
            profile_object.remove("privacy");
            profile_object.remove("approval");
        }
    }
}

/// Copies one default TOML item into the target document if it is absent.
///
/// # Parameters
/// - `target`: The config document being migrated.
/// - `defaults`: The built-in default document.
/// - `path`: The dotted config path to copy.
pub(super) fn copy_toml_default_if_absent(
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
pub(super) fn copy_json_default_if_absent(
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

/// Sets a TOML integer default when a path is absent or still has the previous
/// schema's built-in default.
///
/// # Parameters
/// - `document`: The TOML document being migrated.
/// - `path`: The dotted config path to update.
/// - `old_default`: The previous schema default value.
/// - `new_default`: The replacement schema default value.
pub(super) fn set_toml_default_usize_if_absent_or_old_default(
    document: &mut toml_edit::DocumentMut,
    path: &str,
    old_default: i64,
    new_default: i64,
) -> Result<()> {
    let should_set = match toml_item_at(document.as_table(), path) {
        None => true,
        Some(toml_edit::Item::Value(value)) => value.as_integer() == Some(old_default),
        Some(_) => false,
    };
    if should_set {
        set_toml_path_item(document, path, toml_edit::value(new_default))?;
    }
    Ok(())
}

/// Sets a JSON-compatible integer default when a path is absent or still has
/// the previous schema's built-in default.
///
/// # Parameters
/// - `document`: The JSON-compatible document being migrated.
/// - `path`: The dotted config path to update.
/// - `old_default`: The previous schema default value.
/// - `new_default`: The replacement schema default value.
pub(super) fn set_json_default_usize_if_absent_or_old_default(
    document: &mut serde_json::Value,
    path: &str,
    old_default: u64,
    new_default: u64,
) -> Result<()> {
    let should_set = match json_value_at(document, path) {
        None => true,
        Some(value) => value.as_u64() == Some(old_default),
    };
    if should_set {
        set_json_path_value(document, path, serde_json::json!(new_default))?;
    }
    Ok(())
}

/// Backfills the API compatibility selector for every TOML provider that still
/// relies on historical provider-kind defaults.
///
/// # Parameters
/// - `document`: The TOML document being migrated.
pub(super) fn backfill_toml_provider_api_defaults(
    document: &mut toml_edit::DocumentMut,
) -> Result<()> {
    let segments = split_config_path("providers");
    let Some(providers) = toml_parent_table_mut(document.as_table_mut(), &segments, false)? else {
        return Ok(());
    };
    for (provider_id, item) in providers.iter_mut() {
        let Some(table) = item.as_table_mut() else {
            continue;
        };
        if table.contains_key("api") {
            continue;
        }
        let kind = table
            .get("kind")
            .and_then(|item| item.as_value())
            .and_then(|value| value.as_str())
            .unwrap_or(provider_id.get());
        if let Some(api) = provider_default_api_for_kind(kind) {
            table.insert("api", toml_edit::value(api));
        }
    }
    Ok(())
}

/// Backfills the API compatibility selector for every JSON-compatible provider
/// that still relies on historical provider-kind defaults.
///
/// # Parameters
/// - `document`: The JSON-compatible document being migrated.
pub(super) fn backfill_json_provider_api_defaults(document: &mut serde_json::Value) {
    let Some(providers) = json_value_at_mut(document, "providers") else {
        return;
    };
    let Some(providers) = providers.as_object_mut() else {
        return;
    };
    for (provider_id, value) in providers.iter_mut() {
        let Some(object) = value.as_object_mut() else {
            continue;
        };
        if object.contains_key("api") {
            continue;
        }
        let kind = object
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(provider_id);
        if let Some(api) = provider_default_api_for_kind(kind) {
            object.insert(
                "api".to_string(),
                serde_json::Value::String(api.to_string()),
            );
        }
    }
}

/// Returns the API compatibility selector historically implied by one provider
/// kind, when Mezzanine has a built-in default for that kind.
///
/// # Parameters
/// - `kind`: Provider kind string from the legacy provider entry.
pub(super) fn provider_default_api_for_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "openai" => Some("openai-responses"),
        "openai-compatible" => Some("openai-chat-completions"),
        "deepseek" => Some("deepseek-chat-completions"),
        _ => None,
    }
}

/// Updates a built-in TOML model profile context default without overriding a
/// custom profile or custom context value.
///
/// # Parameters
/// - `document`: The TOML document being migrated.
/// - `profile_name`: The built-in model profile name to inspect.
/// - `provider`: The provider id expected for the built-in profile.
/// - `model`: The model id expected for the built-in profile.
/// - `old_default`: The previous schema default value.
/// - `new_default`: The replacement schema default value.
pub(super) fn update_toml_model_profile_context_window_default(
    document: &mut toml_edit::DocumentMut,
    profile_name: &str,
    provider: &str,
    model: &str,
    old_default: i64,
    new_default: i64,
) -> Result<()> {
    let profile_path = format!("model_profiles.{profile_name}");
    let provider_path = format!("{profile_path}.provider");
    let model_path = format!("{profile_path}.model");
    if toml_string_at(document.as_table(), &provider_path).as_deref() != Some(provider)
        || toml_string_at(document.as_table(), &model_path).as_deref() != Some(model)
    {
        return Ok(());
    }

    let context_path = format!("{profile_path}.context_window_tokens");
    let should_set = match toml_item_at(document.as_table(), &context_path) {
        None => true,
        Some(toml_edit::Item::Value(value)) => value.as_integer() == Some(old_default),
        Some(_) => false,
    };
    if should_set {
        set_toml_path_item(document, &context_path, toml_edit::value(new_default))?;
    }
    Ok(())
}

/// Updates a built-in JSON-compatible model profile context default without
/// overriding a custom profile or custom context value.
///
/// # Parameters
/// - `document`: The JSON-compatible document being migrated.
/// - `profile_name`: The built-in model profile name to inspect.
/// - `provider`: The provider id expected for the built-in profile.
/// - `model`: The model id expected for the built-in profile.
/// - `old_default`: The previous schema default value.
/// - `new_default`: The replacement schema default value.
pub(super) fn update_json_model_profile_context_window_default(
    document: &mut serde_json::Value,
    profile_name: &str,
    provider: &str,
    model: &str,
    old_default: u64,
    new_default: u64,
) -> Result<()> {
    let profile_path = format!("model_profiles.{profile_name}");
    let provider_path = format!("{profile_path}.provider");
    let model_path = format!("{profile_path}.model");
    if json_string_at(document, &provider_path).as_deref() != Some(provider)
        || json_string_at(document, &model_path).as_deref() != Some(model)
    {
        return Ok(());
    }

    let context_path = format!("{profile_path}.context_window_tokens");
    let should_set = match json_value_at(document, &context_path) {
        None => true,
        Some(value) => value.as_u64() == Some(old_default),
    };
    if should_set {
        set_json_path_value(document, &context_path, serde_json::json!(new_default))?;
    }
    Ok(())
}

/// Normalizes one renamed TOML key, preserving canonical-key precedence.
///
/// # Parameters
/// - `document`: The TOML document being migrated.
/// - `old_path`: The historical dotted key.
/// - `new_path`: The canonical dotted key.
pub(super) fn normalize_toml_rename(
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
pub(super) fn normalize_json_rename(
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
pub(super) fn ensure_toml_agent_preset_visible_field(
    document: &mut toml_edit::DocumentMut,
) -> Result<()> {
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
pub(super) fn ensure_json_agent_preset_visible_field(
    document: &mut serde_json::Value,
) -> Result<()> {
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

/// Ensures the pane visible-field list exposes the DeepSeek thinking toggle
/// immediately after the reasoning field.
///
/// # Parameters
/// - `document`: The TOML document being migrated.
pub(super) fn ensure_toml_agent_thinking_visible_field(
    document: &mut toml_edit::DocumentMut,
) -> Result<()> {
    copy_toml_default_if_absent(
        document,
        &DEFAULT_CONFIG_TOML
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| MezError::config(format!("invalid built-in TOML config: {error}")))?,
        "frames.pane.visible_fields",
    )?;
    ensure_toml_string_array_value_after(
        document,
        "frames.pane.visible_fields",
        "agent.thinking",
        "agent.reasoning",
    )
}

/// Ensures the pane visible-field list exposes the DeepSeek thinking toggle
/// immediately after the reasoning field.
///
/// # Parameters
/// - `document`: The JSON-compatible document being migrated.
pub(super) fn ensure_json_agent_thinking_visible_field(
    document: &mut serde_json::Value,
) -> Result<()> {
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
    ensure_json_string_array_value_after(
        document,
        "frames.pane.visible_fields",
        "agent.thinking",
        "agent.reasoning",
    );
    Ok(())
}

/// Inserts one string value into a TOML array after an anchor string when it is
/// not already present.
///
/// # Parameters
/// - `document`: The TOML document being migrated.
/// - `path`: The dotted string-array path.
/// - `value`: The value to insert if absent.
/// - `after`: The anchor value after which the new value should appear.
pub(super) fn ensure_toml_string_array_value_after(
    document: &mut toml_edit::DocumentMut,
    path: &str,
    value: &str,
    after: &str,
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
    let Some(toml_edit::Item::Value(array_value)) = parent.get_mut(leaf) else {
        return Ok(());
    };
    let Some(array) = array_value.as_array_mut() else {
        return Ok(());
    };
    if array.iter().any(|item| item.as_str() == Some(value)) {
        return Ok(());
    }
    let index = array
        .iter()
        .position(|item| item.as_str() == Some(after))
        .map(|position| position + 1)
        .unwrap_or_else(|| array.len());
    array.insert(index, value);
    Ok(())
}

/// Inserts one string value into a JSON-compatible array after an anchor string
/// when it is not already present.
///
/// # Parameters
/// - `document`: The JSON-compatible document being migrated.
/// - `path`: The dotted string-array path.
/// - `value`: The value to insert if absent.
/// - `after`: The anchor value after which the new value should appear.
pub(super) fn ensure_json_string_array_value_after(
    document: &mut serde_json::Value,
    path: &str,
    value: &str,
    after: &str,
) {
    let Some(array_value) = json_value_at_mut(document, path) else {
        return;
    };
    let Some(array) = array_value.as_array_mut() else {
        return;
    };
    if array.iter().any(|item| item.as_str() == Some(value)) {
        return;
    }
    let index = array
        .iter()
        .position(|item| item.as_str() == Some(after))
        .map(|position| position + 1)
        .unwrap_or_else(|| array.len());
    array.insert(index, serde_json::Value::String(value.to_string()));
}

/// Renames one key inside every TOML table stored under a parent table.
///
/// # Parameters
/// - `document`: The TOML document being migrated.
/// - `parent_path`: The parent table containing keyed child tables.
/// - `old_key`: The historical child-table key.
/// - `new_key`: The canonical child-table key.
pub(super) fn rename_toml_table_key(
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
pub(super) fn rename_toml_string_array_value(
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
pub(super) fn rename_json_table_key(
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
pub(super) fn rename_json_string_array_value(
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
pub(super) fn toml_string_at(table: &toml_edit::Table, path: &str) -> Option<String> {
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
pub(super) fn json_string_at(document: &serde_json::Value, path: &str) -> Option<String> {
    json_value_at(document, path)?
        .as_str()
        .map(ToString::to_string)
}

/// Reads a TOML item at one dotted path.
///
/// # Parameters
/// - `table`: The TOML table to inspect.
/// - `path`: The dotted config path to read.
pub(super) fn toml_item_at<'a>(
    table: &'a toml_edit::Table,
    path: &str,
) -> Option<&'a toml_edit::Item> {
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
pub(super) fn set_toml_path_item(
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
pub(super) fn remove_toml_path(document: &mut toml_edit::DocumentMut, path: &str) -> Result<()> {
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
pub(super) fn toml_parent_table_mut<'a>(
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
pub(super) fn json_value_at<'a>(
    document: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
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
pub(super) fn json_value_at_mut<'a>(
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
pub(super) fn set_json_path_value(
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
pub(super) fn remove_json_path(document: &mut serde_json::Value, path: &str) {
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
pub(super) fn split_config_path(path: &str) -> Vec<String> {
    path.split('.').map(ToString::to_string).collect()
}
