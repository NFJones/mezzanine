//! Command Stores implementation.
//!
//! This module owns the command stores boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AuthStatus, CommandInvocation, ConfigFormat, ConfigMutation, ConfigMutationOperation,
    ConfigMutationPlan, ConfigMutationValue, ConfigPaths, ConfigScope, KeyValueLine, MezError,
    Result, credential_store_kind_name, fs, persist_config_mutation, persist_config_text,
    plan_config_mutation, positional_args, validate_command_identifier,
};
use crate::config::parse_config_json_value;
use crate::terminal::{
    UI_COLOR_SLOT_NAMES, UiThemeDefinition, builtin_ui_theme_definition, resolve_ui_theme,
};
use serde_json::Value;
use std::collections::BTreeMap;

// Store-backed auth, config, and project-trust helpers.

/// Runs the persist command config mutation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn persist_command_config_mutation(
    paths: &ConfigPaths,
    mutation: ConfigMutation,
) -> Result<ConfigMutationPlan> {
    let path = paths.ensure_default_config()?;
    persist_config_mutation(&path, ConfigScope::Primary, mutation)
}

/// Summary of a complete config-store theme update.
pub(super) struct CommandThemeConfigPlan {
    /// Whether the generated theme table changed the primary config text.
    pub(super) changed: bool,
    /// Whether callers should reload config to observe the new theme table.
    pub(super) reload_required: bool,
    /// Number of aliases materialized into `theme.aliases`.
    pub(super) alias_count: usize,
    /// Number of UI color slots materialized into `theme.colors`.
    pub(super) color_slot_count: usize,
}

/// Persists a complete selected theme table into the primary config file.
pub(super) fn persist_command_theme_config(
    paths: &ConfigPaths,
    theme: &str,
) -> Result<CommandThemeConfigPlan> {
    let path = paths.ensure_default_config()?;
    let format = ConfigFormat::from_path(&path)?;
    let text = fs::read_to_string(&path)?;
    let definition = command_theme_definition_from_text(format, &text, theme)?;
    let mutations = command_theme_config_mutations(theme, &definition)?;
    let batch = command_plan_config_mutations(format, &text, ConfigScope::Primary, &mutations)?;
    if batch.changed {
        persist_config_text(&path, ConfigScope::Primary, &batch.text)?;
    }
    Ok(CommandThemeConfigPlan {
        changed: batch.changed,
        reload_required: batch.reload_required,
        alias_count: definition.aliases.len(),
        color_slot_count: UI_COLOR_SLOT_NAMES.len(),
    })
}

/// Returns the full selected theme definition available to config-store
/// commands from the primary config file.
fn command_theme_definition_from_text(
    format: ConfigFormat,
    text: &str,
    theme: &str,
) -> Result<UiThemeDefinition> {
    if let Some(definition) = builtin_ui_theme_definition(theme) {
        resolve_ui_theme(theme, definition.clone())?;
        return Ok(definition);
    }
    let root = command_config_value_from_text(format, text)?;
    let Some(custom_theme) = root
        .get("themes")
        .and_then(Value::as_object)
        .and_then(|themes| themes.get(theme))
    else {
        return Err(MezError::invalid_args(format!(
            "set-theme unknown theme `{theme}`; run list-themes to see available themes"
        )));
    };
    let mut definition = builtin_ui_theme_definition("deepforest")
        .ok_or_else(|| MezError::config("built-in deepforest theme is unavailable"))?;
    definition.merge(command_theme_definition_from_json(
        custom_theme,
        &format!("themes.{theme}"),
    )?);
    resolve_ui_theme(theme, definition.clone())?;
    Ok(definition)
}

/// Parses primary config text into a structured JSON value for theme lookup.
fn command_config_value_from_text(format: ConfigFormat, text: &str) -> Result<Value> {
    parse_config_json_value(format, text)
}

/// Extracts a string-based theme definition from structured config JSON.
fn command_theme_definition_from_json(value: &Value, path: &str) -> Result<UiThemeDefinition> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a table")))?;
    Ok(UiThemeDefinition {
        aliases: command_string_map_from_json(object.get("aliases"), &format!("{path}.aliases"))?,
        colors: command_string_map_from_json(object.get("colors"), &format!("{path}.colors"))?,
    })
}

/// Extracts a string-to-string map from a structured config object.
fn command_string_map_from_json(
    value: Option<&Value>,
    path: &str,
) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a table")))?;
    object
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
                .ok_or_else(|| MezError::config(format!("{path}.{key} must be a string")))
        })
        .collect()
}

/// Builds scalar config mutations for a complete selected theme table.
fn command_theme_config_mutations(
    theme: &str,
    definition: &UiThemeDefinition,
) -> Result<Vec<ConfigMutation>> {
    let missing_slots = UI_COLOR_SLOT_NAMES
        .iter()
        .filter(|slot| !definition.colors.contains_key(**slot))
        .copied()
        .collect::<Vec<_>>();
    if !missing_slots.is_empty() {
        return Err(MezError::config(format!(
            "theme `{theme}` is missing color slots: {}",
            missing_slots.join(", ")
        )));
    }

    let mut mutations =
        Vec::with_capacity(1 + definition.aliases.len() + UI_COLOR_SLOT_NAMES.len());
    mutations.push(config_set_string("theme.active", theme));
    for (alias, value) in &definition.aliases {
        mutations.push(config_set_string(format!("theme.aliases.{alias}"), value));
    }
    for slot in UI_COLOR_SLOT_NAMES {
        let value = definition.colors.get(*slot).ok_or_else(|| {
            MezError::config(format!("theme `{theme}` is missing color slot `{slot}`"))
        })?;
        mutations.push(config_set_string(format!("theme.colors.{slot}"), value));
    }
    Ok(mutations)
}

/// Final text and metadata for a batch of config mutations.
struct CommandConfigMutationBatch {
    /// Final config text after all mutations are applied.
    text: String,
    /// Whether any mutation changed the input text.
    changed: bool,
    /// Whether applying the mutations requires runtime config reload.
    reload_required: bool,
}

/// Applies a validated sequence of scalar config mutations to config text.
fn command_plan_config_mutations(
    format: ConfigFormat,
    text: &str,
    scope: ConfigScope,
    mutations: &[ConfigMutation],
) -> Result<CommandConfigMutationBatch> {
    let mut text = text.to_string();
    let mut changed = false;
    let mut reload_required = false;
    for mutation in mutations {
        let plan = plan_config_mutation(format, &text, scope, mutation.clone())?;
        changed |= plan.changed;
        reload_required |= plan.reload_required;
        text = plan.text;
    }
    Ok(CommandConfigMutationBatch {
        text,
        changed,
        reload_required,
    })
}

/// Runs the config set string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_set_string(
    path: impl Into<String>,
    value: impl Into<String>,
) -> ConfigMutation {
    ConfigMutation {
        path: path.into(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::String(value.into())),
    }
}

/// Runs the config unset operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_unset(path: impl Into<String>) -> ConfigMutation {
    ConfigMutation {
        path: path.into(),
        operation: ConfigMutationOperation::Unset,
    }
}

/// Runs the mcp server id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_server_id<'a>(
    invocation: &'a CommandInvocation,
    missing: &str,
) -> Result<&'a str> {
    let server_id = positional_args(invocation)
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args(missing))?;
    validate_command_identifier(server_id, "MCP server id")?;
    Ok(server_id)
}

/// Runs the mcp transport target operation for this subsystem.
///
/// Runs the auth status store display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn auth_status_store_display(status: AuthStatus) -> String {
    let provider = status
        .metadata
        .as_ref()
        .map(|metadata| metadata.provider.as_str())
        .unwrap_or("none");
    let profile = status
        .metadata
        .as_ref()
        .map(|metadata| metadata.selected_model_profile.as_str())
        .unwrap_or("none");
    match status.credential_state {
        crate::auth::AuthCredentialState::Available { store, .. } => KeyValueLine::spaced()
            .push("authenticated", true)
            .push("provider", provider)
            .push("profile", profile)
            .push("credential_store", credential_store_kind_name(store))
            .push("source", "auth-store")
            .finish(),
        crate::auth::AuthCredentialState::LoggedOut => KeyValueLine::spaced()
            .push("authenticated", false)
            .push("provider", "none")
            .push("profile", "none")
            .push("state", "logged-out")
            .push("source", "auth-store")
            .finish(),
        crate::auth::AuthCredentialState::MissingSecret { reference } => KeyValueLine::spaced()
            .push("authenticated", false)
            .push("provider", provider)
            .push("profile", profile)
            .push("state", "missing-secret")
            .push("reference_present", reference.is_some())
            .push("source", "auth-store")
            .finish(),
    }
}

/// Runs the mcp status store display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_status_store_display(status: crate::auth::McpAuthStatus) -> String {
    match status.credential_state {
        crate::auth::AuthCredentialState::Available { store, .. } => KeyValueLine::spaced()
            .push("server", &status.server_id)
            .push("authenticated", status.authenticated)
            .push("metadata_present", status.metadata_present)
            .push("stale_url", status.stale_url)
            .push("credential_store", credential_store_kind_name(store))
            .push("source", "auth-store")
            .finish(),
        crate::auth::AuthCredentialState::LoggedOut => KeyValueLine::spaced()
            .push("server", &status.server_id)
            .push("authenticated", false)
            .push("metadata_present", false)
            .push("state", "logged-out")
            .push("source", "auth-store")
            .finish(),
        crate::auth::AuthCredentialState::MissingSecret { reference } => KeyValueLine::spaced()
            .push("server", &status.server_id)
            .push("authenticated", false)
            .push("metadata_present", status.metadata_present)
            .push("stale_url", status.stale_url)
            .push("state", "missing-secret")
            .push("reference_present", reference.is_some())
            .push("source", "auth-store")
            .finish(),
    }
}
