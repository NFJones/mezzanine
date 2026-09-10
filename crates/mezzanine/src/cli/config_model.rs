//! Typed provider-model configuration commands.
//!
//! This module manages complete provider model records without exposing their
//! generated local table keys. Provider-facing ids remain opaque, updates are
//! selective, destructive id changes are reference-safe, and all writes pass
//! through the normal validated private-config persistence boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use serde::Serialize;

use super::config::{CliConfigMutationTarget, CliConfigPersistOptions, cli_config_mutation_target};
use super::{
    CliOutputFormat, ConfigFormat, ConfigPaths, MezError, Result, Subcommand, serialize_json,
    write_json_or_plain,
};
use crate::config::{
    ConfigLayer, ConfigScope, ProviderModelSyncAddition, ProviderModelSyncBlocker,
    ProviderModelSyncConflict, ProviderModelSyncUpdate, format_diagnostics,
    load_primary_config_layers, parse_config_json_value, persist_config_text,
    plan_config_mutations, plan_provider_model_sync, plan_provider_model_sync_for_target,
    provider_model_reference_paths, unique_model_entry_key, validate_config_text,
};
use crate::runtime::{
    fetch_raw_provider_model_catalog, runtime_effective_config_value,
    runtime_provider_config_from_config,
};
use crate::security::auth::{AuthPaths, AuthStore};

/// Typed arguments for `mez config model`.
#[derive(Debug, Clone, clap::Args)]
pub(super) struct ConfigModelCliArgs {
    /// Provider-model operation.
    #[command(subcommand)]
    command: ConfigModelCliCommand,
}

/// Typed provider-model catalog operations.
#[derive(Debug, Clone, Subcommand)]
enum ConfigModelCliCommand {
    /// Lists configured model records for one provider.
    List(ConfigModelListCliArgs),
    /// Compares raw live models with configured records and optionally persists the plan.
    Sync(ConfigModelSyncCliArgs),
    /// Adds one provider-facing model id.
    Add(ConfigModelAddCliArgs),
    /// Selectively updates one provider-facing model id.
    Update(ConfigModelUpdateCliArgs),
    /// Removes one unreferenced provider-facing model id.
    Remove(ConfigModelRemoveCliArgs),
}

/// Arguments for explicit provider-model synchronization.
#[derive(Debug, Clone, clap::Args)]
struct ConfigModelSyncCliArgs {
    /// Provider configuration name.
    provider: String,
    /// Persists the rendered plan after complete validation.
    #[arg(long)]
    apply: bool,
    /// Proposes removal of configured-only records independently of apply.
    #[arg(long)]
    prune: bool,
    /// Configuration persistence target.
    #[command(flatten)]
    target: CliConfigPersistOptions,
}

/// Arguments shared by provider-model list operations.
#[derive(Debug, Clone, clap::Args)]
struct ConfigModelListCliArgs {
    /// Provider configuration name.
    provider: String,
    /// Configuration persistence target.
    #[command(flatten)]
    target: CliConfigPersistOptions,
}

/// Arguments for adding one provider-model record.
#[derive(Debug, Clone, clap::Args)]
struct ConfigModelAddCliArgs {
    /// Provider configuration name.
    provider: String,
    /// Opaque provider-facing model id.
    #[arg(allow_hyphen_values = true)]
    id: String,
    /// Optional model metadata.
    #[command(flatten)]
    fields: ConfigModelFieldsCliArgs,
    /// Configuration persistence target.
    #[command(flatten)]
    target: CliConfigPersistOptions,
}

/// Arguments for selectively updating one provider-model record.
#[derive(Debug, Clone, clap::Args)]
struct ConfigModelUpdateCliArgs {
    /// Provider configuration name.
    provider: String,
    /// Existing opaque provider-facing model id.
    #[arg(allow_hyphen_values = true)]
    id: String,
    /// Replacement provider-facing id; omitted to retain the current id.
    #[arg(long, allow_hyphen_values = true)]
    new_id: Option<String>,
    /// Optional model metadata updates.
    #[command(flatten)]
    fields: ConfigModelFieldsCliArgs,
    /// Removes display-name metadata.
    #[arg(long, conflicts_with = "display_name")]
    clear_display_name: bool,
    /// Removes context-window metadata.
    #[arg(long, conflicts_with = "context_window_tokens")]
    clear_context_window_tokens: bool,
    /// Removes maximum-input metadata.
    #[arg(long, conflicts_with = "max_input_tokens")]
    clear_max_input_tokens: bool,
    /// Removes maximum-output metadata.
    #[arg(long, conflicts_with = "max_output_tokens")]
    clear_max_output_tokens: bool,
    /// Removes all provider-option defaults before applying supplied options.
    #[arg(long)]
    clear_provider_options: bool,
    /// Provider-option keys to remove.
    #[arg(long = "remove-provider-option", value_name = "KEY")]
    remove_provider_options: Vec<String>,
    /// Configuration persistence target.
    #[command(flatten)]
    target: CliConfigPersistOptions,
}

/// Arguments for removing one provider-model record.
#[derive(Debug, Clone, clap::Args)]
struct ConfigModelRemoveCliArgs {
    /// Provider configuration name.
    provider: String,
    /// Opaque provider-facing model id.
    #[arg(allow_hyphen_values = true)]
    id: String,
    /// Configuration persistence target.
    #[command(flatten)]
    target: CliConfigPersistOptions,
}

/// Optional model metadata accepted by add and update.
#[derive(Debug, Clone, Default, clap::Args)]
struct ConfigModelFieldsCliArgs {
    /// Human-readable display name.
    #[arg(long)]
    display_name: Option<String>,
    /// Comma-separated aliases; an empty value clears aliases during update.
    #[arg(long, value_name = "ALIAS,...", allow_hyphen_values = true)]
    aliases: Option<String>,
    /// Positive context-window token limit.
    #[arg(long)]
    context_window_tokens: Option<u64>,
    /// Positive maximum-input token limit.
    #[arg(long)]
    max_input_tokens: Option<u64>,
    /// Positive maximum-output token limit.
    #[arg(long)]
    max_output_tokens: Option<u64>,
    /// Comma-separated provider reasoning levels; empty clears the list.
    #[arg(long, value_name = "LEVEL,...", allow_hyphen_values = true)]
    reasoning_levels: Option<String>,
    /// Comma-separated capability tags; empty clears the list.
    #[arg(long, value_name = "CAPABILITY,...", allow_hyphen_values = true)]
    capabilities: Option<String>,
    /// Non-secret string provider option in KEY=VALUE form; repeatable.
    #[arg(long = "provider-option", value_name = "KEY=VALUE")]
    provider_options: Vec<String>,
}

/// One deterministic model record emitted by list and mutation commands.
#[derive(Debug, Clone, Serialize)]
struct ConfigModelRecord {
    /// Generated path-safe local table key.
    entry_key: String,
    /// Canonical provider-facing model id.
    id: String,
    /// Optional display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    /// Configured aliases.
    aliases: Vec<String>,
    /// Optional context-window limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    context_window_tokens: Option<u64>,
    /// Optional maximum-input limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_input_tokens: Option<u64>,
    /// Optional maximum-output limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u64>,
    /// Configured reasoning levels.
    reasoning_levels: Vec<String>,
    /// Configured capability tags.
    capabilities: Vec<String>,
    /// Non-secret provider option defaults.
    provider_options: BTreeMap<String, String>,
}

/// Deterministic list payload.
#[derive(Serialize)]
struct ConfigModelListOutput {
    /// Provider whose local catalog was inspected.
    provider: String,
    /// Selected target scope.
    scope: &'static str,
    /// Selected config path.
    path: String,
    /// Records sorted by canonical id and local key.
    models: Vec<ConfigModelRecord>,
    /// Actionable empty-catalog guidance.
    #[serde(skip_serializing_if = "Option::is_none")]
    guidance: Option<String>,
}

/// Deterministic mutation payload.
#[derive(Serialize)]
struct ConfigModelMutationOutput {
    /// Mutation operation.
    operation: &'static str,
    /// Provider whose local catalog changed.
    provider: String,
    /// Selected target scope.
    scope: &'static str,
    /// Selected config path.
    path: String,
    /// Generated local entry key, absent after removal.
    entry_key: Option<String>,
    /// Canonical provider-facing model id affected by the operation.
    id: String,
    /// Whether the resulting document differed from the original.
    changed: bool,
}

/// Deterministic preview or apply result for one provider-model synchronization.
#[derive(Serialize)]
struct ConfigModelSyncOutput<'a> {
    /// Operation identifier.
    operation: &'static str,
    /// Provider whose raw live catalog was fetched.
    provider: &'a str,
    /// Selected persistence scope.
    scope: &'static str,
    /// Selected config path.
    path: String,
    /// Whether persistence was explicitly requested.
    apply: bool,
    /// Whether configured-only records were considered for removal.
    prune: bool,
    /// Whether the plan differs from the current model table.
    changed: bool,
    /// Whether one validated atomic write was performed.
    persisted: bool,
    /// Secret-free raw provider catalog source.
    source: &'a str,
    /// Number of raw live models used by the planner.
    live_model_count: usize,
    /// Newly discovered model records.
    additions: &'a [ProviderModelSyncAddition],
    /// Fill-only metadata updates.
    updates: &'a [ProviderModelSyncUpdate],
    /// Explicit configured values retained over differing live observations.
    conflicts: &'a [ProviderModelSyncConflict],
    /// Configured-only ids retained because pruning was not requested.
    retained: &'a [String],
    /// Configured-only ids proposed for removal.
    removals: &'a [String],
    /// Complete reference blockers preventing prune application.
    blockers: &'a [ProviderModelSyncBlocker],
    /// Actionable preview or blocker guidance.
    #[serde(skip_serializing_if = "Option::is_none")]
    guidance: Option<String>,
}

/// Runs one typed provider-model config operation.
pub(super) async fn run_config_model<W: Write>(
    parsed: ConfigModelCliArgs,
    paths: &ConfigPaths,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    match parsed.command {
        ConfigModelCliCommand::List(args) => {
            let target = cli_config_mutation_target(paths, args.target)?;
            let document = load_model_document(&target)?;
            let models = model_records(&document.root, &args.provider)?;
            let guidance = models.is_empty().then(|| {
                format!(
                    "No configured models for provider `{}`. Compatible custom providers may have no built-in catalog; add one with `mez config model add {} MODEL_ID` or configure a supported live /models endpoint.",
                    args.provider, args.provider
                )
            });
            let output = serialize_json(&ConfigModelListOutput {
                provider: args.provider,
                scope: target.scope_name,
                path: target.path.to_string_lossy().into_owned(),
                models,
                guidance,
            })?;
            write_json_or_plain(stdout, output_format, &output)
        }
        ConfigModelCliCommand::Sync(args) => {
            run_model_sync(args, paths, output_format, stdout).await
        }
        ConfigModelCliCommand::Add(args) => run_model_add(args, paths, output_format, stdout),
        ConfigModelCliCommand::Update(args) => run_model_update(args, paths, output_format, stdout),
        ConfigModelCliCommand::Remove(args) => run_model_remove(args, paths, output_format, stdout),
    }
}

/// Fetches, plans, previews, and optionally persists one provider model sync.
async fn run_model_sync<W: Write>(
    args: ConfigModelSyncCliArgs,
    paths: &ConfigPaths,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let provider = validated_text("provider", &args.provider)?;
    let target = cli_config_mutation_target(paths, args.target)?;
    let mut document = load_model_document(&target)?;
    let effective_root = model_sync_effective_root(paths, &target, &document)?;
    let provider_value = effective_root
        .get("providers")
        .and_then(serde_json::Value::as_object)
        .and_then(|providers| providers.get(&provider))
        .ok_or_else(|| MezError::config(format!("provider `{provider}` is not configured")))?;
    let provider_config = runtime_provider_config_from_config(&provider, provider_value)?;
    let auth_store = AuthStore::new(AuthPaths::under_config_root(paths.root()));
    let catalog = fetch_raw_provider_model_catalog(provider_config, auth_store)
        .await
        .map_err(|error| {
            MezError::new(
                error.kind(),
                format!(
                    "provider model sync fetch failed for `{provider}`: {}; configuration was not changed; add a model manually with `mez config model add {provider} MODEL_ID`",
                    error.message()
                ),
            )
        })?;
    if catalog.provider != provider {
        return Err(MezError::invalid_state(format!(
            "provider model sync returned catalog for `{}` while `{provider}` was requested; configuration was not changed; add a model manually with `mez config model add {provider} MODEL_ID`",
            catalog.provider
        )));
    }
    let plan = if target.scope == ConfigScope::Primary {
        plan_provider_model_sync(&document.root, &provider, &catalog.models, args.prune)?
    } else {
        plan_provider_model_sync_for_target(
            &document.root,
            &effective_root,
            &provider,
            &catalog.models,
            args.prune,
        )?
    };
    let changed = plan.changed();
    let blocked = args.prune && !plan.blockers.is_empty();
    let persisted = args.apply && changed && !blocked;
    if persisted {
        *sync_provider_models_mut(&mut document.root, &provider)? = plan.result_models.clone();
        if target.scope != ConfigScope::ProjectOverlay {
            let rendered = render_model_document(&document, &provider)?;
            let validation = validate_config_text(ConfigFormat::Toml, &rendered, target.scope);
            if !validation.valid {
                return Err(MezError::config(format!(
                    "provider model sync rejected; proposed config is invalid: {}",
                    format_diagnostics(&validation.diagnostics)
                )));
            }
        }
        persist_model_document(&target, &document, &provider)?;
    }
    let guidance = if blocked {
        Some(format!(
            "Prune was not applied because configured model references must be updated first; configuration was not changed. To add a model manually, run `mez config model add {provider} MODEL_ID`."
        ))
    } else if !args.apply && changed {
        Some(format!(
            "Preview only; rerun with `mez config model sync {provider} --apply{}` to persist this plan.",
            if args.prune { " --prune" } else { "" }
        ))
    } else {
        None
    };
    let output = serialize_json(&ConfigModelSyncOutput {
        operation: "sync",
        provider: &provider,
        scope: target.scope_name,
        path: target.path.to_string_lossy().into_owned(),
        apply: args.apply,
        prune: args.prune,
        changed,
        persisted,
        source: &catalog.source,
        live_model_count: catalog.models.len(),
        additions: &plan.additions,
        updates: &plan.updates,
        conflicts: &plan.conflicts,
        retained: &plan.retained,
        removals: &plan.removals,
        blockers: &plan.blockers,
        guidance,
    })?;
    write_json_or_plain(stdout, output_format, &output)
}

/// Parsed config document plus source text used for change detection.
struct ModelDocument {
    /// Source format selected by the target extension.
    format: ConfigFormat,
    /// Original source text.
    original: String,
    /// JSON-compatible mutable representation.
    root: serde_json::Value,
}

/// Loads and parses one selected config target.
fn load_model_document(target: &CliConfigMutationTarget) -> Result<ModelDocument> {
    let format = ConfigFormat::from_path(&target.path)?;
    let original = std::fs::read_to_string(&target.path)?;
    let root = parse_config_json_value(format, &original)?;
    Ok(ModelDocument {
        format,
        original,
        root,
    })
}

/// Builds the read view used by synchronization without changing its target.
///
/// User files are complete primary configurations. Project files are overlays,
/// so their provider connection may be inherited from the primary user layer.
/// Only the selected project file is included; unrelated overlays are neither
/// read nor copied into the synchronization target.
fn model_sync_effective_root(
    paths: &ConfigPaths,
    target: &CliConfigMutationTarget,
    document: &ModelDocument,
) -> Result<serde_json::Value> {
    if target.scope == ConfigScope::Primary {
        return Ok(document.root.clone());
    }
    let normalized = plan_config_mutations(
        document.format,
        &document.original,
        target.scope,
        Vec::new(),
    )?;
    let mut layers = load_primary_config_layers(paths)?;
    layers.push(ConfigLayer {
        name: "model-sync-target".to_string(),
        path: Some(target.path.clone()),
        format: document.format,
        scope: target.scope,
        trusted: true,
        text: normalized.text,
    });
    runtime_effective_config_value(&layers)
}

/// Adds one model record after validating its typed metadata.
fn run_model_add<W: Write>(
    args: ConfigModelAddCliArgs,
    paths: &ConfigPaths,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let target = cli_config_mutation_target(paths, args.target)?;
    let mut document = load_model_document(&target)?;
    let id = validated_text("model id", &args.id)?;
    let record = model_record_value(&id, &args.fields)?;
    let models = provider_models_mut(&mut document.root, &args.provider)?;
    ensure_identity_available(models, &id, None)?;
    let used_keys = models.keys().cloned().collect::<BTreeSet<_>>();
    let mut used_keys = used_keys;
    let entry_key = unique_model_entry_key(&id, &mut used_keys);
    models.insert(entry_key.clone(), record);
    persist_model_document(&target, &document, &args.provider)?;
    write_model_mutation_output(
        stdout,
        output_format,
        &target,
        "add",
        &args.provider,
        Some(entry_key),
        id,
        true,
    )
}

/// Selectively updates one existing model record.
fn run_model_update<W: Write>(
    args: ConfigModelUpdateCliArgs,
    paths: &ConfigPaths,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let target = cli_config_mutation_target(paths, args.target.clone())?;
    let mut document = load_model_document(&target)?;
    let old_id = validated_text("model id", &args.id)?;
    let existing_key = model_entry_key(&document.root, &args.provider, &old_id)?;
    let new_id = args
        .new_id
        .as_deref()
        .map(|value| validated_text("new model id", value))
        .transpose()?
        .unwrap_or_else(|| old_id.clone());
    if new_id != old_id {
        refuse_referenced_model_id(&document.root, &args.provider, &old_id, "rename")?;
    }
    let before = document.root.clone();
    let mut output_key = existing_key.clone();
    {
        let models = provider_models_mut(&mut document.root, &args.provider)?;
        ensure_identity_available(models, &new_id, Some(&existing_key))?;
        let mut record = models
            .remove(&existing_key)
            .and_then(|value| value.as_object().cloned())
            .ok_or_else(|| MezError::config("provider model record must be a table"))?;
        record.insert("id".to_string(), serde_json::Value::String(new_id.clone()));
        apply_field_updates(&mut record, &args)?;
        if new_id != old_id {
            let mut used_keys = models.keys().cloned().collect::<BTreeSet<_>>();
            output_key = unique_model_entry_key(&new_id, &mut used_keys);
        }
        models.insert(output_key.clone(), serde_json::Value::Object(record));
    }
    let changed = document.root != before;
    if changed {
        persist_model_document(&target, &document, &args.provider)?;
    }
    write_model_mutation_output(
        stdout,
        output_format,
        &target,
        "update",
        &args.provider,
        Some(output_key),
        new_id,
        changed,
    )
}

/// Removes one unreferenced model record.
fn run_model_remove<W: Write>(
    args: ConfigModelRemoveCliArgs,
    paths: &ConfigPaths,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let target = cli_config_mutation_target(paths, args.target)?;
    let mut document = load_model_document(&target)?;
    let id = validated_text("model id", &args.id)?;
    let entry_key = model_entry_key(&document.root, &args.provider, &id)?;
    refuse_referenced_model_id(&document.root, &args.provider, &id, "remove")?;
    provider_models_mut(&mut document.root, &args.provider)?.remove(&entry_key);
    persist_model_document(&target, &document, &args.provider)?;
    write_model_mutation_output(
        stdout,
        output_format,
        &target,
        "remove",
        &args.provider,
        None,
        id,
        true,
    )
}

/// Applies only fields explicitly supplied by an update invocation.
fn apply_field_updates(
    record: &mut serde_json::Map<String, serde_json::Value>,
    args: &ConfigModelUpdateCliArgs,
) -> Result<()> {
    apply_optional_text(record, "display_name", args.fields.display_name.as_deref())?;
    if args.clear_display_name {
        record.remove("display_name");
    }
    apply_optional_list(record, "aliases", args.fields.aliases.as_deref())?;
    apply_optional_limit(
        record,
        "context_window_tokens",
        args.fields.context_window_tokens,
        args.clear_context_window_tokens,
    )?;
    apply_optional_limit(
        record,
        "max_input_tokens",
        args.fields.max_input_tokens,
        args.clear_max_input_tokens,
    )?;
    apply_optional_limit(
        record,
        "max_output_tokens",
        args.fields.max_output_tokens,
        args.clear_max_output_tokens,
    )?;
    apply_optional_list(
        record,
        "reasoning_levels",
        args.fields.reasoning_levels.as_deref(),
    )?;
    apply_optional_list(record, "capabilities", args.fields.capabilities.as_deref())?;
    apply_provider_option_updates(
        record,
        args.clear_provider_options,
        &args.remove_provider_options,
        &args.fields.provider_options,
    )
}

/// Builds one new model record from typed CLI fields.
fn model_record_value(id: &str, fields: &ConfigModelFieldsCliArgs) -> Result<serde_json::Value> {
    let mut record = serde_json::Map::new();
    record.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    apply_optional_text(&mut record, "display_name", fields.display_name.as_deref())?;
    apply_optional_list(&mut record, "aliases", fields.aliases.as_deref())?;
    apply_optional_limit(
        &mut record,
        "context_window_tokens",
        fields.context_window_tokens,
        false,
    )?;
    apply_optional_limit(
        &mut record,
        "max_input_tokens",
        fields.max_input_tokens,
        false,
    )?;
    apply_optional_limit(
        &mut record,
        "max_output_tokens",
        fields.max_output_tokens,
        false,
    )?;
    apply_optional_list(
        &mut record,
        "reasoning_levels",
        fields.reasoning_levels.as_deref(),
    )?;
    apply_optional_list(&mut record, "capabilities", fields.capabilities.as_deref())?;
    apply_provider_option_updates(&mut record, false, &[], &fields.provider_options)?;
    Ok(serde_json::Value::Object(record))
}

/// Applies one optional printable text field.
fn apply_optional_text(
    record: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<&str>,
) -> Result<()> {
    if let Some(value) = value {
        record.insert(
            key.to_string(),
            serde_json::Value::String(validated_text(key, value)?),
        );
    }
    Ok(())
}

/// Applies one optional comma-separated replacement list.
fn apply_optional_list(
    record: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<&str>,
) -> Result<()> {
    if let Some(value) = value {
        let values = parse_unique_text_list(key, value)?;
        for entry in &values {
            match key {
                "reasoning_levels"
                    if !matches!(entry.as_str(), "low" | "medium" | "high" | "xhigh" | "max") =>
                {
                    return Err(MezError::invalid_args(format!(
                        "--reasoning-levels contains unsupported level `{entry}`"
                    )));
                }
                "capabilities"
                    if !matches!(
                        entry.trim(),
                        "native_thinking"
                            | "function_tools"
                            | "function_calling"
                            | "tool_use"
                            | "tools"
                            | "forced_tool_choice"
                            | "streaming"
                            | "max_output_tokens"
                            | "max_output_token_control"
                            | "vision"
                    ) =>
                {
                    return Err(MezError::invalid_args(format!(
                        "--capabilities contains unrecognized capability tag `{entry}`"
                    )));
                }
                _ => {}
            }
        }
        record.insert(
            key.to_string(),
            serde_json::Value::Array(values.into_iter().map(serde_json::Value::String).collect()),
        );
    }
    Ok(())
}

/// Applies or removes one positive integer token limit.
fn apply_optional_limit(
    record: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<u64>,
    clear: bool,
) -> Result<()> {
    if clear {
        record.remove(key);
    } else if let Some(value) = value {
        if value == 0 {
            return Err(MezError::invalid_args(format!(
                "{key} must be a positive integer"
            )));
        }
        record.insert(key.to_string(), serde_json::Value::Number(value.into()));
    }
    Ok(())
}

/// Applies typed provider-option additions and removals.
fn apply_provider_option_updates(
    record: &mut serde_json::Map<String, serde_json::Value>,
    clear: bool,
    removals: &[String],
    assignments: &[String],
) -> Result<()> {
    if clear {
        record.remove("provider_options");
    }
    if removals.is_empty() && assignments.is_empty() {
        return Ok(());
    }
    let options = record
        .entry("provider_options".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| MezError::config("provider_options must be a table"))?;
    for key in removals {
        let key = validated_option_key(key)?;
        options.remove(&key);
    }
    let mut assigned = BTreeSet::new();
    for assignment in assignments {
        let (key, value) = assignment
            .split_once('=')
            .ok_or_else(|| MezError::invalid_args("provider options must use KEY=VALUE syntax"))?;
        let key = validated_option_key(key)?;
        if !assigned.insert(key.clone()) {
            return Err(MezError::invalid_args(format!(
                "provider option `{key}` was supplied more than once"
            )));
        }
        let value = validated_text("provider option value", value)?;
        options.insert(key, serde_json::Value::String(value));
    }
    if options.is_empty() {
        record.remove("provider_options");
    }
    Ok(())
}

/// Validates a non-secret provider-option key.
fn validated_option_key(value: &str) -> Result<String> {
    let key = validated_text("provider option key", value)?;
    let lower = key.to_ascii_lowercase();
    let secret = matches!(
        lower.as_str(),
        "token" | "api_key" | "secret" | "password" | "access_token" | "refresh_token"
    ) || lower.contains("authorization")
        || lower.contains("credential")
        || lower.ends_with("_token")
        || lower.ends_with("_secret")
        || lower.ends_with("_password");
    if secret {
        return Err(MezError::invalid_args(format!(
            "provider option `{key}` appears to contain credential material"
        )));
    }
    Ok(key)
}

/// Validates and trims one printable non-empty text value.
fn validated_text(label: &str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(MezError::invalid_args(format!(
            "{label} must be non-empty printable text"
        )));
    }
    Ok(value.to_string())
}

/// Parses one deterministic unique comma-separated text list.
fn parse_unique_text_list(label: &str, value: &str) -> Result<Vec<String>> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    for value in value.split(',') {
        let value = validated_text(label, value)?;
        if !seen.insert(value.clone()) {
            return Err(MezError::invalid_args(format!(
                "{label} values must be unique"
            )));
        }
        values.push(value);
    }
    Ok(values)
}

/// Returns the mutable model table for one existing provider.
fn provider_models_mut<'a>(
    root: &'a mut serde_json::Value,
    provider: &str,
) -> Result<&'a mut serde_json::Map<String, serde_json::Value>> {
    let provider = validated_text("provider", provider)?;
    let providers = root
        .get_mut("providers")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| MezError::config("configuration does not define a providers table"))?;
    let provider = providers
        .get_mut(&provider)
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| MezError::config(format!("provider `{provider}` is not configured")))?;
    let models = provider
        .entry("models".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    models
        .as_object_mut()
        .ok_or_else(|| MezError::config("provider models must be a table of model records"))
}

/// Returns the target model table, creating only overlay container tables.
fn sync_provider_models_mut<'a>(
    root: &'a mut serde_json::Value,
    provider: &str,
) -> Result<&'a mut serde_json::Map<String, serde_json::Value>> {
    let root = root
        .as_object_mut()
        .ok_or_else(|| MezError::config("configuration document root must be a mapping"))?;
    let providers = root
        .entry("providers".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| MezError::config("configuration providers must be a table"))?;
    let provider = providers
        .entry(provider.to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| MezError::config("provider configuration must be a table"))?;
    provider
        .entry("models".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| MezError::config("provider models must be a table of model records"))
}

/// Finds one model's generated entry key by canonical id.
fn model_entry_key(root: &serde_json::Value, provider: &str, id: &str) -> Result<String> {
    model_entries(root, provider)?
        .iter()
        .find_map(|(key, value)| {
            (value.get("id").and_then(serde_json::Value::as_str) == Some(id)).then(|| key.clone())
        })
        .ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                format!("provider `{provider}` has no configured model id `{id}`"),
            )
        })
}

/// Returns one provider's immutable model map.
fn model_entries<'a>(
    root: &'a serde_json::Value,
    provider: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>> {
    let provider_value = root
        .get("providers")
        .and_then(serde_json::Value::as_object)
        .and_then(|providers| providers.get(provider))
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::config(format!("provider `{provider}` is not configured")))?;
    match provider_value.get("models") {
        None => Ok(empty_json_object()),
        Some(value) => value
            .as_object()
            .ok_or_else(|| MezError::config("provider models must be a table of model records")),
    }
}

/// Shared immutable empty object used only for absent model tables.
fn empty_json_object() -> &'static serde_json::Map<String, serde_json::Value> {
    static EMPTY: std::sync::OnceLock<serde_json::Map<String, serde_json::Value>> =
        std::sync::OnceLock::new();
    EMPTY.get_or_init(serde_json::Map::new)
}

/// Ensures an id does not collide with another canonical id or alias.
fn ensure_identity_available(
    models: &serde_json::Map<String, serde_json::Value>,
    id: &str,
    except_key: Option<&str>,
) -> Result<()> {
    for (key, model) in models {
        if except_key == Some(key.as_str()) {
            continue;
        }
        if model.get("id").and_then(serde_json::Value::as_str) == Some(id)
            || model
                .get("aliases")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|aliases| aliases.iter().any(|alias| alias.as_str() == Some(id)))
        {
            return Err(MezError::new(
                crate::error::MezErrorKind::Conflict,
                format!("provider model id `{id}` collides with an existing id or alias"),
            ));
        }
    }
    Ok(())
}

/// Refuses destructive id changes while provider defaults or profiles refer to it.
fn refuse_referenced_model_id(
    root: &serde_json::Value,
    provider: &str,
    id: &str,
    operation: &str,
) -> Result<()> {
    let record = model_entries(root, provider)?
        .values()
        .find(|record| record.get("id").and_then(serde_json::Value::as_str) == Some(id))
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::config("provider model record must be a table"))?;
    let mut identities = vec![id.to_string()];
    identities.extend(record_string_list(record, "aliases")?);
    let references = provider_model_reference_paths(root, provider, &identities);
    if references.is_empty() {
        return Ok(());
    }
    Err(MezError::new(
        crate::error::MezErrorKind::Conflict,
        format!(
            "cannot {operation} provider model id `{id}` while referenced by {}",
            references.join(", ")
        ),
    ))
}

/// Converts configured records into deterministic typed output.
fn model_records(root: &serde_json::Value, provider: &str) -> Result<Vec<ConfigModelRecord>> {
    let mut records = model_entries(root, provider)?
        .iter()
        .map(|(entry_key, value)| model_record(entry_key, value))
        .collect::<Result<Vec<_>>>()?;
    records.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.entry_key.cmp(&right.entry_key))
    });
    Ok(records)
}

/// Parses one configured model record for output.
fn model_record(entry_key: &str, value: &serde_json::Value) -> Result<ConfigModelRecord> {
    let record = value
        .as_object()
        .ok_or_else(|| MezError::config("provider model record must be a table"))?;
    let id = record
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::config("provider model record id must be a string"))?;
    Ok(ConfigModelRecord {
        entry_key: entry_key.to_string(),
        id: id.to_string(),
        display_name: record
            .get("display_name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        aliases: record_string_list(record, "aliases")?,
        context_window_tokens: record
            .get("context_window_tokens")
            .and_then(serde_json::Value::as_u64),
        max_input_tokens: record
            .get("max_input_tokens")
            .and_then(serde_json::Value::as_u64),
        max_output_tokens: record
            .get("max_output_tokens")
            .and_then(serde_json::Value::as_u64),
        reasoning_levels: record_string_list(record, "reasoning_levels")?,
        capabilities: record_string_list(record, "capabilities")?,
        provider_options: record_string_map(record, "provider_options")?,
    })
}

/// Parses one optional string-list record field.
fn record_string_list(
    record: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Vec<String>> {
    let Some(value) = record.get(key) else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| MezError::config(format!("provider model {key} must be a string list")))?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                MezError::config(format!("provider model {key} must contain strings"))
            })
        })
        .collect()
}

/// Parses one optional string-map record field.
fn record_string_map(
    record: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<BTreeMap<String, String>> {
    let Some(value) = record.get(key) else {
        return Ok(BTreeMap::new());
    };
    value
        .as_object()
        .ok_or_else(|| MezError::config(format!("provider model {key} must be a table")))?
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
                .ok_or_else(|| MezError::config("provider model options must be strings"))
        })
        .collect()
}

/// Persists a mutated document through the validated private-file writer.
fn persist_model_document(
    target: &CliConfigMutationTarget,
    document: &ModelDocument,
    provider: &str,
) -> Result<()> {
    let text = render_model_document(document, provider)?;
    persist_config_text(&target.path, target.scope, &text)
}

/// Renders the target format while retaining unrelated TOML source structure.
fn render_model_document(document: &ModelDocument, provider: &str) -> Result<String> {
    match document.format {
        ConfigFormat::Json => serde_json::to_string_pretty(&document.root)
            .map(|mut text| {
                text.push('\n');
                text
            })
            .map_err(|error| MezError::config(format!("failed to render JSON config: {error}"))),
        ConfigFormat::Yaml => serde_norway::to_string(&document.root)
            .map_err(|error| MezError::config(format!("failed to render YAML config: {error}"))),
        ConfigFormat::Toml => render_toml_models(&document.original, &document.root, provider),
    }
}

/// Replaces only one provider's TOML model table in the original document.
fn render_toml_models(original: &str, root: &serde_json::Value, provider: &str) -> Result<String> {
    let mut document = original
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
    if document.as_table().get("providers").is_none() {
        let mut providers = toml_edit::Table::new();
        providers.set_implicit(true);
        document
            .as_table_mut()
            .insert("providers", toml_edit::Item::Table(providers));
    }
    let providers = document
        .as_table_mut()
        .get_mut("providers")
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| MezError::config("configuration providers must be a table"))?;
    if providers.get(provider).is_none() {
        let mut provider_table = toml_edit::Table::new();
        provider_table.set_implicit(true);
        providers.insert(provider, toml_edit::Item::Table(provider_table));
    }
    let provider_table = providers
        .get_mut(provider)
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| MezError::config(format!("provider `{provider}` must be a table")))?;
    let mut models_table = toml_edit::Table::new();
    models_table.set_implicit(true);
    for (entry_key, record) in model_entries(root, provider)? {
        models_table.insert(entry_key, toml_model_record_item(record)?);
    }
    provider_table.insert("models", toml_edit::Item::Table(models_table));
    Ok(document.to_string())
}

/// Converts one JSON-compatible model record into a TOML table item.
fn toml_model_record_item(value: &serde_json::Value) -> Result<toml_edit::Item> {
    let record = value
        .as_object()
        .ok_or_else(|| MezError::config("provider model record must be a table"))?;
    let mut table = toml_edit::Table::new();
    for (key, value) in record {
        table.insert(key, toml_model_value_item(key, value)?);
    }
    Ok(toml_edit::Item::Table(table))
}

/// Converts one supported provider-model value to TOML.
fn toml_model_value_item(key: &str, value: &serde_json::Value) -> Result<toml_edit::Item> {
    match value {
        serde_json::Value::String(value) => Ok(toml_edit::value(value.as_str())),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(toml_edit::value)
            .ok_or_else(|| MezError::config(format!("provider model {key} integer is invalid"))),
        serde_json::Value::Array(values) => {
            let mut array = toml_edit::Array::new();
            for value in values {
                let value = value.as_str().ok_or_else(|| {
                    MezError::config(format!("provider model {key} must contain strings"))
                })?;
                array.push(value);
            }
            Ok(toml_edit::value(array))
        }
        serde_json::Value::Object(values) if key == "provider_options" => {
            let mut table = toml_edit::Table::new();
            for (option, value) in values {
                let value = value
                    .as_str()
                    .ok_or_else(|| MezError::config("provider model options must be strings"))?;
                table.insert(option, toml_edit::value(value));
            }
            Ok(toml_edit::Item::Table(table))
        }
        _ => Err(MezError::config(format!(
            "unsupported provider model value for `{key}`"
        ))),
    }
}

/// Writes one deterministic mutation result.
#[allow(clippy::too_many_arguments)]
fn write_model_mutation_output<W: Write>(
    stdout: &mut W,
    output_format: CliOutputFormat,
    target: &CliConfigMutationTarget,
    operation: &'static str,
    provider: &str,
    entry_key: Option<String>,
    id: String,
    changed: bool,
) -> Result<()> {
    let output = serialize_json(&ConfigModelMutationOutput {
        operation,
        provider: provider.to_string(),
        scope: target.scope_name,
        path: target.path.to_string_lossy().into_owned(),
        entry_key,
        id,
        changed,
    })?;
    write_json_or_plain(stdout, output_format, &output)
}
