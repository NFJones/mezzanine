//! Command Stores implementation.
//!
//! This module owns the command stores boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AuthMethod, AuthStatus, AuthStore, CommandInvocation, ConfigFormat, ConfigMutation,
    ConfigMutationOperation, ConfigMutationPlan, ConfigMutationValue, ConfigPaths, ConfigScope,
    CredentialStorePlan, MezError, Result, credential_store_kind_name, flag_value, fs,
    persist_config_mutation, persist_config_text, plan_config_mutation, positional_args,
    repeated_flag_values, validate_command_identifier,
};
use crate::auth::selected_auth_method_from_flags;
use crate::terminal::{
    UI_COLOR_SLOT_NAMES, UiThemeDefinition, builtin_ui_theme_definition, resolve_ui_theme,
};
use serde_json::Value;
use std::collections::BTreeMap;

// Store-backed auth, MCP, config, and project-trust helpers.

/// Runs the persist mcp add operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn persist_mcp_add(
    paths: &ConfigPaths,
    invocation: &CommandInvocation,
) -> Result<(String, &'static str, String, Vec<ConfigMutationPlan>)> {
    let server_id = mcp_server_id(invocation, "mcp-add requires a server id")?;
    let (transport, target) = mcp_transport_target(invocation)?;
    let args = repeated_flag_values(&invocation.args, "--arg");
    let mut plans = Vec::new();
    plans.push(persist_command_config_mutation(
        paths,
        config_set_bool(format!("mcp_servers.{server_id}.enabled"), true),
    )?);
    match transport {
        "stdio" => {
            plans.push(persist_command_config_mutation(
                paths,
                config_set_string(format!("mcp_servers.{server_id}.command"), target),
            )?);
            plans.push(persist_command_config_mutation(
                paths,
                config_set_string_array(format!("mcp_servers.{server_id}.args"), &args),
            )?);
            plans.push(persist_command_config_mutation(
                paths,
                config_unset(format!("mcp_servers.{server_id}.url")),
            )?);
        }
        "streamable-http" => {
            plans.push(persist_command_config_mutation(
                paths,
                config_set_string(format!("mcp_servers.{server_id}.url"), target),
            )?);
            plans.push(persist_command_config_mutation(
                paths,
                config_unset(format!("mcp_servers.{server_id}.command")),
            )?);
            plans.push(persist_command_config_mutation(
                paths,
                config_set_string_array(format!("mcp_servers.{server_id}.args"), &[]),
            )?);
        }
        _ => unreachable!("MCP transport target validation returned a known transport"),
    }
    Ok((server_id.to_string(), transport, target.to_string(), plans))
}

/// Runs the persist mcp remove operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn persist_mcp_remove(
    paths: &ConfigPaths,
    server_id: &str,
) -> Result<Vec<ConfigMutationPlan>> {
    persist_command_config_mutation(paths, config_unset(format!("mcp_servers.{server_id}")))
        .map(|plan| vec![plan])
}

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
    match format {
        ConfigFormat::Toml => {
            let value = toml::from_str::<toml::Table>(text)
                .map_err(|error| MezError::config(error.to_string()))?;
            serde_json::to_value(value).map_err(|error| MezError::config(error.to_string()))
        }
        ConfigFormat::Yaml => {
            let value = serde_norway::from_str::<serde_norway::Value>(text)
                .map_err(|error| MezError::config(error.to_string()))?;
            serde_json::to_value(value).map_err(|error| MezError::config(error.to_string()))
        }
        ConfigFormat::Json => {
            serde_json::from_str(text).map_err(|error| MezError::config(error.to_string()))
        }
    }
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

/// Runs the config set bool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_set_bool(path: impl Into<String>, value: bool) -> ConfigMutation {
    ConfigMutation {
        path: path.into(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::Boolean(value)),
    }
}

/// Runs the config set string array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_set_string_array(
    path: impl Into<String>,
    values: &[String],
) -> ConfigMutation {
    ConfigMutation {
        path: path.into(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(values.to_vec())),
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

/// Runs the mutation plans changed operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mutation_plans_changed(plans: &[ConfigMutationPlan]) -> bool {
    plans.iter().any(|plan| plan.changed)
}

/// Runs the mutation plans reload required operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mutation_plans_reload_required(plans: &[ConfigMutationPlan]) -> bool {
    plans.iter().any(|plan| plan.reload_required)
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
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_transport_target(invocation: &CommandInvocation) -> Result<(&'static str, &str)> {
    let command = flag_value(&invocation.args, "--command");
    let url = flag_value(&invocation.args, "--url");
    if command.is_some() == url.is_some() {
        return Err(MezError::invalid_args(
            "mcp-add requires exactly one of --command or --url",
        ));
    }
    Ok(match (command, url) {
        (Some(command), None) => ("stdio", command),
        (None, Some(url)) => ("streamable-http", url),
        _ => unreachable!("validated exactly one transport target"),
    })
}

/// Runs the execute auth login operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn execute_auth_login(
    auth_store: &AuthStore,
    invocation: &CommandInvocation,
) -> Result<String> {
    let method = auth_login_method(&invocation.args)?;
    let provider = flag_value(&invocation.args, "--provider").unwrap_or("openai");
    if method != AuthMethod::ApiKey {
        return Ok(format!(
            "provider={provider} method={} authenticated=false action=interactive-required reason=run-mez-auth-login source=auth-store",
            auth_method_display_name(method)
        ));
    }

    let selected_profile = flag_value(&invocation.args, "--profile").unwrap_or("default");
    let Some(api_key_path) = flag_value(&invocation.args, "--api-key-file") else {
        let plan = auth_store.plan_provider_flow(provider, AuthMethod::ApiKey);
        return Ok(format!(
            "provider={} method=api-key credential_target={} action=prompt-required source=auth-store",
            plan.provider, plan.credential_target
        ));
    };
    let api_key = fs::read_to_string(api_key_path)?;
    let api_key = api_key.trim();

    let metadata = match flag_value(&invocation.args, "--credential-store") {
        Some("file") => {
            let credential_store = auth_store.file_credential_store(provider)?;
            auth_store.login_provider_api_key(
                provider,
                selected_profile,
                api_key,
                &credential_store,
            )?
        }
        Some("os") => auth_store.login_provider_api_key_with_default_os_store(
            provider,
            selected_profile,
            api_key,
        )?,
        Some(other) => {
            return Err(MezError::invalid_args(format!(
                "unknown credential store `{other}`"
            )));
        }
        None => match auth_store.credential_store_plan(provider) {
            CredentialStorePlan::OperatingSystem { .. } => auth_store
                .login_provider_api_key_with_default_os_store(
                    provider,
                    selected_profile,
                    api_key,
                )?,
            CredentialStorePlan::PrivateFileFallback { .. } => {
                let credential_store = auth_store.file_credential_store(provider)?;
                auth_store.login_provider_api_key(
                    provider,
                    selected_profile,
                    api_key,
                    &credential_store,
                )?
            }
        },
    };

    let credential_store = metadata
        .credential_store_ref
        .as_deref()
        .and_then(|reference| reference.split_once(':').map(|(prefix, _)| prefix))
        .unwrap_or("unknown");
    Ok(format!(
        "provider={} method=api-key authenticated=true selected_model_profile={} credential_store={} source=auth-store",
        metadata.provider, metadata.selected_model_profile, credential_store
    ))
}

/// Runs the auth login method operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn auth_login_method(args: &[String]) -> Result<AuthMethod> {
    let api_key = args.iter().any(|arg| arg == "--api-key");
    let browser = args.iter().any(|arg| arg == "--browser");
    let device_code = args
        .iter()
        .any(|arg| arg == "--device-code" || arg == "--device-auth");
    selected_auth_method_from_flags(
        api_key,
        browser,
        device_code,
        "auth-login accepts only one authentication method flag",
    )
}

/// Runs the auth method display name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn auth_method_display_name(method: AuthMethod) -> &'static str {
    method.as_str()
}

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
        crate::auth::AuthCredentialState::Available { store, .. } => format!(
            "authenticated=true provider={provider} profile={profile} credential_store={} source=auth-store",
            credential_store_kind_name(store)
        ),
        crate::auth::AuthCredentialState::LoggedOut => {
            "authenticated=false provider=none profile=none state=logged-out source=auth-store"
                .to_string()
        }
        crate::auth::AuthCredentialState::MissingSecret { reference } => format!(
            "authenticated=false provider={provider} profile={profile} state=missing-secret reference_present={} source=auth-store",
            reference.is_some()
        ),
    }
}

/// Runs the mcp status store display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_status_store_display(status: crate::auth::McpAuthStatus) -> String {
    match status.credential_state {
        crate::auth::AuthCredentialState::Available { store, .. } => format!(
            "server={} authenticated={} metadata_present={} stale_url={} credential_store={} source=auth-store",
            status.server_id,
            status.authenticated,
            status.metadata_present,
            status.stale_url,
            credential_store_kind_name(store)
        ),
        crate::auth::AuthCredentialState::LoggedOut => format!(
            "server={} authenticated=false metadata_present=false state=logged-out source=auth-store",
            status.server_id
        ),
        crate::auth::AuthCredentialState::MissingSecret { reference } => format!(
            "server={} authenticated=false metadata_present={} stale_url={} state=missing-secret reference_present={} source=auth-store",
            status.server_id,
            status.metadata_present,
            status.stale_url,
            reference.is_some()
        ),
    }
}
