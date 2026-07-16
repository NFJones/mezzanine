//! Runtime config and theme command helpers.
//!
//! This child module owns live config/theme command support, including option
//! mutation planning, theme materialization and persistence, source-file
//! application, live override storage, and refresh-client diagnostics. The
//! parent command-support module keeps command dispatch while this module
//! isolates config materialization and persistence rules.

use super::super::{
    CommandInvocation, ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation,
    ConfigMutationValue, ConfigPaths, ConfigScope, EventKind, MezError, PathBuf, Result,
    RuntimeSessionService, RuntimeSideEffect, UiThemeDefinition, Value,
    builtin_ui_theme_definition, compose_effective_config, fs, json_escape, persist_config_text,
    plan_config_mutation, resolve_ui_theme, runtime_config_apply_event_payload,
    runtime_effective_config_value, validate_config_text,
};
use super::{
    TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER, runtime_expand_user_path, runtime_positional_args,
};
use mez_mux::theme::{
    BUILTIN_UI_THEME_NAMES, UI_COLOR_SLOT_NAMES, ui_theme_list_table_header,
    ui_theme_list_table_row,
};
use std::collections::BTreeMap;

/// Runs the runtime show options command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_show_options_command(
    service: &RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let effective = compose_effective_config(service.integration.config_layers())?;
    let filter = runtime_positional_args(invocation).first().copied();
    let mut lines = vec![format!(
        "options={}:applied_layers={}:skipped_layers={}:source=runtime-config",
        effective.values().len(),
        effective.applied_layers().len(),
        effective.skipped_layers().len()
    )];
    for (path, value) in effective.values() {
        if let Some(filter) = filter
            && path != filter
        {
            continue;
        }
        lines.push(format!(
            "path={path}:value={}:source={}:live_mutable={}",
            json_escape(&value.value),
            json_escape(&value.source_layer),
            runtime_option_live_mutable(path)
        ));
    }
    Ok(lines.join("\n"))
}

/// Runs the runtime set option command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_set_option_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let args = runtime_positional_args(invocation);
    let path = args
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args("set-option requires an option path"))?;
    let value = args
        .get(1)
        .copied()
        .ok_or_else(|| MezError::invalid_args("set-option requires a value"))?;
    if path == "theme.active" {
        let definition = runtime_theme_definition_for_selection(service, value)?;
        let mutations = runtime_theme_config_mutations(value, &definition)?;
        let plan = runtime_apply_theme_live_override(service, &mutations)?;
        let report = service.apply_runtime_config_layers()?;
        service.append_lifecycle_event(
            EventKind::ConfigChanged,
            runtime_config_apply_event_payload("terminal/command:set-option", &report),
        )?;
        return Ok(format!(
            "path={path}:value={value}:changed={}:reload_required={}:source=runtime-config:layer={TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER}:aliases={}:color_slots={}",
            plan.changed,
            plan.reload_required,
            definition.aliases.len(),
            UI_COLOR_SLOT_NAMES.len()
        ));
    }
    let mutation = ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Set(runtime_config_command_value(value)),
    };
    let plan = runtime_plan_live_override_mutation(service, mutation)?;
    runtime_store_live_override_plan(service, &plan.text);
    let report = service.apply_runtime_config_layers()?;
    service.append_lifecycle_event(
        EventKind::ConfigChanged,
        runtime_config_apply_event_payload("terminal/command:set-option", &report),
    )?;
    Ok(format!(
        "path={path}:value={value}:changed={}:reload_required={}:source=runtime-config:layer={TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER}",
        plan.changed, plan.reload_required
    ))
}

/// Runs the runtime set theme command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_set_theme_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let args = runtime_positional_args(invocation);
    let theme = args
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args("set-theme requires a theme name"))?;
    if args.len() > 1 {
        return Err(MezError::invalid_args(
            "set-theme accepts exactly one theme name",
        ));
    }
    if !runtime_theme_available(service, theme)? {
        return Err(MezError::invalid_args(format!(
            "set-theme unknown theme `{theme}`; run list-themes to see available themes"
        )));
    }

    let definition = runtime_theme_definition_for_selection(service, theme)?;
    let mutations = runtime_theme_config_mutations(theme, &definition)?;
    let persist_plan = runtime_plan_theme_persistence(service, &mutations)?;
    let live_plan = runtime_apply_theme_live_override(service, &mutations)?;
    let persist_report = runtime_persist_theme_plan(service, persist_plan)?;
    let report = service.apply_runtime_config_layers()?;
    service.append_lifecycle_event(
        EventKind::ConfigChanged,
        runtime_config_apply_event_payload("terminal/command:set-theme", &report),
    )?;
    let persisted_path = persist_report
        .path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "none".to_string());
    Ok(format!(
        "theme={theme}:changed={}:reload_required={}:source=runtime-config:layer={TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER}:persisted={}:persisted_changed={}:persisted_reload_required={}:persisted_path={}:aliases={}:color_slots={}",
        live_plan.changed,
        live_plan.reload_required,
        persist_report.persisted,
        persist_report.changed,
        persist_report.reload_required,
        json_escape(&persisted_path),
        definition.aliases.len(),
        UI_COLOR_SLOT_NAMES.len()
    ))
}

/// Accumulates a sequence of scalar configuration mutations into one validated
/// replacement document.
struct RuntimeConfigMutationBatch {
    /// Final config text after all mutations are applied.
    text: String,
    /// Whether any mutation changed the input text.
    changed: bool,
    /// Whether applying the mutations requires runtime config reload.
    reload_required: bool,
}

/// Planned persisted theme update for the primary config file.
struct RuntimeThemePersistencePlan {
    /// Primary config file to rewrite.
    path: PathBuf,
    /// Config format inferred from the primary file extension.
    format: ConfigFormat,
    /// Final validated primary config text.
    text: String,
    /// Whether the final text differs from the current primary config.
    changed: bool,
    /// Whether runtime reload is needed after persistence.
    reload_required: bool,
}

/// Result of applying a persisted theme update.
struct RuntimeThemePersistenceReport {
    /// Whether a primary config target was available and updated.
    persisted: bool,
    /// Whether the persisted target changed.
    changed: bool,
    /// Whether the persisted update would require reload.
    reload_required: bool,
    /// Primary config file that received the selected theme, when available.
    path: Option<PathBuf>,
}

/// Result of applying a persisted model-authored config mutation batch.
pub(crate) struct RuntimePersistedConfigMutationBatchReport {
    /// Primary config file that received the batch.
    pub path: PathBuf,
    /// Whether the persisted target changed.
    pub changed: bool,
    /// Whether the batch required a runtime reload.
    pub reload_required: bool,
    /// Number of scalar mutations included in the batch.
    pub mutation_count: usize,
    /// Whether persistence was deferred to the async side-effect writer.
    pub deferred: bool,
}

/// Returns the full theme definition that should be materialized for a selected
/// theme name.
fn runtime_theme_definition_for_selection(
    service: &RuntimeSessionService,
    theme: &str,
) -> Result<UiThemeDefinition> {
    if let Some(definition) = builtin_ui_theme_definition(theme) {
        resolve_ui_theme(theme, definition.clone())?;
        return Ok(definition);
    }

    let structured = runtime_effective_config_value(service.integration.config_layers())?;
    let Some(custom_theme) = structured
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
    definition.merge(runtime_theme_definition_from_json(
        custom_theme,
        &format!("themes.{theme}"),
    )?);
    resolve_ui_theme(theme, definition.clone())?;
    Ok(definition)
}

/// Extracts a string-based theme definition from structured config JSON.
fn runtime_theme_definition_from_json(value: &Value, path: &str) -> Result<UiThemeDefinition> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a table")))?;
    Ok(UiThemeDefinition {
        aliases: runtime_string_map_from_json(object.get("aliases"), &format!("{path}.aliases"))?,
        colors: runtime_string_map_from_json(object.get("colors"), &format!("{path}.colors"))?,
    })
}

/// Extracts a string-to-string map from a structured config object.
fn runtime_string_map_from_json(
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

/// Builds the scalar config mutations that make a selected theme self-contained
/// in a root `theme` table.
fn runtime_theme_config_mutations(
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
    mutations.push(ConfigMutation {
        path: "theme.active".to_string(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::String(theme.to_string())),
    });
    for (alias, value) in &definition.aliases {
        mutations.push(ConfigMutation {
            path: format!("theme.aliases.{alias}"),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::String(value.clone())),
        });
    }
    for slot in UI_COLOR_SLOT_NAMES {
        let value = definition.colors.get(*slot).ok_or_else(|| {
            MezError::config(format!("theme `{theme}` is missing color slot `{slot}`"))
        })?;
        mutations.push(ConfigMutation {
            path: format!("theme.colors.{slot}"),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::String(value.clone())),
        });
    }
    Ok(mutations)
}

/// Applies a theme mutation batch to the terminal-command live override layer.
fn runtime_apply_theme_live_override(
    service: &mut RuntimeSessionService,
    mutations: &[ConfigMutation],
) -> Result<RuntimeConfigMutationBatch> {
    let current_text = service
        .integration
        .config_layers()
        .iter()
        .find(|layer| {
            layer.name == TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER
                && layer.scope == ConfigScope::LiveOverride
        })
        .map(|layer| layer.text.as_str())
        .unwrap_or("");
    let batch = runtime_plan_config_mutations(
        ConfigFormat::Toml,
        current_text,
        ConfigScope::LiveOverride,
        mutations,
    )?;
    runtime_store_live_override_plan(service, &batch.text);
    Ok(batch)
}

/// Plans a persisted primary-config update for a selected theme.
fn runtime_plan_theme_persistence(
    service: &RuntimeSessionService,
    mutations: &[ConfigMutation],
) -> Result<Option<RuntimeThemePersistencePlan>> {
    let Some(path) = runtime_primary_config_path(service)? else {
        return Ok(None);
    };
    let format = ConfigFormat::from_path(&path)?;
    let text = fs::read_to_string(&path)?;
    let batch = runtime_plan_config_mutations(format, &text, ConfigScope::Primary, mutations)?;
    Ok(Some(RuntimeThemePersistencePlan {
        path,
        format,
        text: batch.text,
        changed: batch.changed,
        reload_required: batch.reload_required,
    }))
}

/// Persists a planned selected-theme update and mirrors the updated primary
/// layer text into the live runtime service.
fn runtime_persist_theme_plan(
    service: &mut RuntimeSessionService,
    plan: Option<RuntimeThemePersistencePlan>,
) -> Result<RuntimeThemePersistenceReport> {
    let Some(plan) = plan else {
        return Ok(RuntimeThemePersistenceReport {
            persisted: false,
            changed: false,
            reload_required: false,
            path: None,
        });
    };

    if plan.changed {
        persist_config_text(&plan.path, ConfigScope::Primary, &plan.text)?;
    }
    runtime_store_primary_config_text(service, plan.path.clone(), plan.format, plan.text.clone());
    Ok(RuntimeThemePersistenceReport {
        persisted: true,
        changed: plan.changed,
        reload_required: plan.reload_required,
        path: Some(plan.path),
    })
}

/// Applies one validated persisted config mutation batch and reloads once.
///
/// # Parameters
/// - `service`: Runtime service receiving the applied configuration.
/// - `path`: Primary config path to update or queue for persistence.
/// - `mutations`: Ordered scalar mutations to fold into the target document.
/// - `event_source`: Event payload source used for the resulting config-change
///   lifecycle event.
pub(crate) fn runtime_apply_persisted_config_mutation_batch(
    service: &mut RuntimeSessionService,
    path: PathBuf,
    mutations: &[ConfigMutation],
    event_source: &str,
) -> Result<RuntimePersistedConfigMutationBatchReport> {
    if mutations.is_empty() {
        return Err(MezError::invalid_args(
            "persisted config mutation batch requires at least one mutation",
        ));
    }
    let format = ConfigFormat::from_path(&path)?;
    let current_text = service
        .integration
        .config_layers()
        .iter()
        .find(|layer| layer.scope == ConfigScope::Primary && layer.path.as_ref() == Some(&path))
        .map(|layer| Ok(layer.text.clone()))
        .unwrap_or_else(|| fs::read_to_string(&path))?;
    let batch =
        runtime_plan_config_mutations(format, &current_text, ConfigScope::Primary, mutations)?;
    if batch.changed {
        if !service.persistence.config_uses_adapter() {
            persist_config_text(&path, ConfigScope::Primary, &batch.text)?;
        }
        let previous_layers = service.integration.config_layers().to_vec();
        runtime_store_primary_config_text(service, path.clone(), format, batch.text.clone());
        match service.apply_runtime_config_layers() {
            Ok(report) => {
                service.append_lifecycle_event(
                    EventKind::ConfigChanged,
                    runtime_config_apply_event_payload(event_source, &report),
                )?;
                if service.persistence.config_uses_adapter() {
                    service
                        .persistence
                        .queue_config(RuntimeSideEffect::Persist {
                            target: crate::runtime::PersistenceTarget::Config,
                            path: path.clone(),
                            bytes: batch.text.clone().into_bytes(),
                            mode: crate::runtime::PersistenceWriteMode::Replace,
                        });
                }
                service.session.advance_config_generation();
            }
            Err(error) => {
                service.integration.replace_config_layers(previous_layers);
                let _ = service.apply_runtime_config_layers();
                return Err(error);
            }
        }
    }
    Ok(RuntimePersistedConfigMutationBatchReport {
        path,
        changed: batch.changed,
        reload_required: batch.reload_required,
        mutation_count: mutations.len(),
        deferred: service.persistence.config_uses_adapter(),
    })
}

/// Finds or creates the primary config file used for persisted command changes.
fn runtime_primary_config_path(service: &RuntimeSessionService) -> Result<Option<PathBuf>> {
    if let Some(path) = service
        .integration
        .config_layers()
        .iter()
        .find(|layer| layer.scope == ConfigScope::Primary && layer.path.is_some())
        .and_then(|layer| layer.path.clone())
    {
        return Ok(Some(path));
    }
    let Some(root) = service.integration.config_root() else {
        return Ok(None);
    };
    ConfigPaths::from_root(root.to_path_buf())
        .ensure_default_config()
        .map(Some)
}

/// Updates the in-memory primary config layer after persisting a selected theme.
fn runtime_store_primary_config_text(
    service: &mut RuntimeSessionService,
    path: PathBuf,
    format: ConfigFormat,
    text: String,
) {
    if let Some(layer) = service
        .integration
        .config_layers_mut()
        .iter_mut()
        .find(|layer| layer.scope == ConfigScope::Primary && layer.path.as_ref() == Some(&path))
    {
        layer.text = text;
        layer.format = format;
        return;
    }
    service.integration.config_layers_mut().push(ConfigLayer {
        name: "primary".to_string(),
        path: Some(path),
        format,
        scope: ConfigScope::Primary,
        trusted: true,
        text,
    });
}

/// Applies a validated sequence of scalar config mutations to in-memory text.
fn runtime_plan_config_mutations(
    format: ConfigFormat,
    text: &str,
    scope: ConfigScope,
    mutations: &[ConfigMutation],
) -> Result<RuntimeConfigMutationBatch> {
    let mut text = text.to_string();
    let mut changed = false;
    let mut reload_required = false;
    for mutation in mutations {
        let plan = plan_config_mutation(format, &text, scope, mutation.clone())?;
        changed |= plan.changed;
        reload_required |= plan.reload_required;
        text = plan.text;
    }
    Ok(RuntimeConfigMutationBatch {
        text,
        changed,
        reload_required,
    })
}

/// Runs the runtime theme available operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_theme_available(service: &RuntimeSessionService, theme: &str) -> Result<bool> {
    if BUILTIN_UI_THEME_NAMES.contains(&theme) {
        return Ok(true);
    }
    let structured = runtime_effective_config_value(service.integration.config_layers())?;
    Ok(structured
        .get("themes")
        .and_then(|value| value.as_object())
        .map(|themes| themes.contains_key(theme))
        .unwrap_or(false))
}

/// Runs the runtime list themes command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_list_themes_command(service: &RuntimeSessionService) -> Result<String> {
    let structured = runtime_effective_config_value(service.integration.config_layers())?;
    let mut custom_theme_names = structured
        .get("themes")
        .and_then(|value| value.as_object())
        .map(|themes| themes.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    custom_theme_names.sort();
    custom_theme_names.dedup();

    let mut lines = vec![ui_theme_list_table_header()];
    for theme in BUILTIN_UI_THEME_NAMES {
        let definition = builtin_ui_theme_definition(theme)
            .ok_or_else(|| MezError::config(format!("built-in theme `{theme}` is unavailable")))?;
        lines.push(ui_theme_list_table_row(
            theme,
            "builtin",
            *theme == service.ui_theme().name,
            &definition,
        ));
    }
    lines.extend(
        custom_theme_names
            .iter()
            .filter(|theme| !BUILTIN_UI_THEME_NAMES.contains(&theme.as_str()))
            .map(|theme| {
                let definition = runtime_theme_definition_for_selection(service, theme)?;
                Ok(ui_theme_list_table_row(
                    theme,
                    "config",
                    theme == &service.ui_theme().name,
                    &definition,
                ))
            })
            .collect::<Result<Vec<_>>>()?,
    );
    Ok(lines.join("\n"))
}

/// Runs the runtime source file command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_source_file_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let path = runtime_positional_args(invocation)
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args("source-file requires a path"))?;
    let path = runtime_expand_user_path(path);
    let format = ConfigFormat::from_path(&path)?;
    let text = fs::read_to_string(&path)?;
    let validation = validate_config_text(format, &text, ConfigScope::LiveOverride);
    if !validation.valid {
        return Err(MezError::config(format!(
            "source-file rejected invalid config: {}",
            validation
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{}: {}", diagnostic.path, diagnostic.message))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    let layer_name = format!("source-file:{}", path.display());
    if let Some(layer) = service
        .integration
        .config_layers_mut()
        .iter_mut()
        .find(|layer| layer.name == layer_name)
    {
        layer.text = text;
        layer.format = format;
        layer.path = Some(path.clone());
        layer.scope = ConfigScope::LiveOverride;
        layer.trusted = true;
    } else {
        service.integration.config_layers_mut().push(ConfigLayer {
            name: layer_name.clone(),
            path: Some(path.clone()),
            format,
            scope: ConfigScope::LiveOverride,
            trusted: true,
            text,
        });
    }
    let report = service.apply_runtime_config_layers()?;
    service.append_lifecycle_event(
        EventKind::ConfigChanged,
        runtime_config_apply_event_payload("terminal/command:source-file", &report),
    )?;
    Ok(format!(
        "path={}:applied=true:changed=true:source=runtime-config:layer={}",
        path.display(),
        json_escape(&layer_name)
    ))
}

/// Runs the runtime refresh client command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_refresh_client_command(
    service: &mut RuntimeSessionService,
) -> Result<String> {
    let primary = service
        .session
        .primary_client_id()
        .cloned()
        .ok_or_else(|| MezError::invalid_state("refresh-client requires an attached primary"))?;
    let size = service.session.authoritative_size;
    service.append_lifecycle_event(
        EventKind::Diagnostic,
        format!(
            r#"{{"client_id":"{}","refresh_client":true,"columns":{},"rows":{}}}"#,
            json_escape(primary.as_str()),
            size.columns,
            size.rows
        ),
    )?;
    Ok(format!(
        "client={primary}:refreshed=true:columns={}:rows={}:source=runtime-client-state",
        size.columns, size.rows
    ))
}

/// Runs the runtime plan live override mutation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_plan_live_override_mutation(
    service: &RuntimeSessionService,
    mutation: ConfigMutation,
) -> Result<crate::config::ConfigMutationPlan> {
    let current_text = service
        .integration
        .config_layers()
        .iter()
        .find(|layer| {
            layer.name == TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER
                && layer.scope == ConfigScope::LiveOverride
        })
        .map(|layer| layer.text.as_str())
        .unwrap_or("");
    plan_config_mutation(
        ConfigFormat::Toml,
        current_text,
        ConfigScope::LiveOverride,
        mutation,
    )
}

/// Runs the runtime store live override plan operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_store_live_override_plan(service: &mut RuntimeSessionService, text: &str) {
    if let Some(layer) = service
        .integration
        .config_layers_mut()
        .iter_mut()
        .find(|layer| {
            layer.name == TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER
                && layer.scope == ConfigScope::LiveOverride
        })
    {
        layer.text = text.to_string();
    } else {
        service.integration.config_layers_mut().push(ConfigLayer {
            name: TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER.to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::LiveOverride,
            trusted: true,
            text: text.to_string(),
        });
    }
}

/// Runs the runtime config command value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_config_command_value(value: &str) -> ConfigMutationValue {
    match value {
        "true" => ConfigMutationValue::Boolean(true),
        "false" => ConfigMutationValue::Boolean(false),
        _ => value
            .parse::<i64>()
            .map(ConfigMutationValue::Integer)
            .unwrap_or_else(|_| ConfigMutationValue::String(value.to_string())),
    }
}

/// Runs the runtime option live mutable operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_option_live_mutable(path: &str) -> bool {
    path.starts_with("mcp_servers.")
        || path.starts_with("agents.auto_sizing.")
        || path.starts_with("theme.aliases.")
        || path.starts_with("theme.colors.")
        || matches!(
            path,
            "history.lines"
                | "history.rotate_lines"
                | "history.saved_sessions_limit"
                | "agents.max_concurrent_agents"
                | "agents.max_root_subagents"
                | "agents.max_subagents_per_subagent"
                | "agents.max_depth"
                | "agents.compaction_raw_retention_percent"
                | "agents.routing"
                | "agents.action_failure_retry_limit"
                | "agents.implementation_pressure_after_shell_actions"
                | "agents.loop_limit"
                | "agents.shell_only"
                | "agents.subagent_placement"
                | "agents.subagent_wait_policy"
                | "frames.window.enabled"
                | "frames.window.template"
                | "frames.window.right_status"
                | "frames.window.position"
                | "frames.window.style"
                | "frames.window.visible_fields"
                | "frames.pane.enabled"
                | "frames.pane.template"
                | "frames.pane.position"
                | "frames.pane.style"
                | "frames.pane.visible_fields"
                | "terminal.term"
                | "terminal.profile"
                | "terminal.cursor_style"
                | "terminal.cursor_blink"
                | "terminal.cursor_blink_interval_ms"
                | "terminal.emoji_width"
                | "terminal.reduced_motion"
                | "terminal.resize_debounce_ms"
                | "terminal.render_rate_limit_fps"
                | "terminal.shell_output_preview_lines"
                | "terminal.true_color"
                | "terminal.mouse"
                | "terminal.clipboard"
                | "terminal.bracketed_paste"
                | "terminal.focus_events"
                | "terminal.alternate_screen"
                | "theme.active"
                | "permissions.preset"
                | "permissions.approval_policy"
                | "permissions.bypass_mode"
                | "permissions.network_policy"
                | "permissions.destructive_action_policy"
                | "instructions.max_bytes"
                | "instructions.include_hidden_directories"
                | "instructions.on_truncation"
        )
}
