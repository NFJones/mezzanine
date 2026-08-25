//! Configuration paths and generated defaults.
//!
//! This module owns the primary user configuration directory, default config
//! material, and selection rules for supported primary config files. The default
//! config string is tested against the checked-in example configuration.

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};
use crate::security::project::{
    ProjectTrustStore, TrustDecision, default_trust_database_path, discover_existing_overlays,
    discover_project_root,
};
use mez_agent::permissions::{exact_command_sha256, normalize_exact_command_text};

/// Exposes the defaults module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod defaults;
/// Exposes the extract module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod extract;
/// Exposes the migration module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod migration;
/// Exposes the mutation module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod mutation;
/// Exposes the parsers module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod parsers;
/// Exposes the paths module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod paths;
/// Exposes the schema module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod schema;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;
/// Exposes the validation module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod validation;

pub use defaults::{DEFAULT_CONFIG_TOML, DEFAULT_PROJECT_CONFIG_TOML};
pub(crate) use defaults::{initial_config_toml, provider_default_config_toml};
#[cfg(test)]
pub use migration::migrate_config_text;
pub use migration::{CURRENT_CONFIG_SCHEMA_VERSION, migrate_config_file};
pub use paths::ConfigPaths;
pub use schema::{
    BASELINE_TOP_LEVEL_KEYS, PRIMARY_CONFIG_FILENAMES, config_change_option_reference_markdown,
    config_change_setting_path_annotations_markdown, config_change_setting_path_description,
};
pub(crate) use schema::{
    config_change_path_is_user_only_host_policy, config_change_path_is_user_only_host_power_policy,
    config_change_path_is_user_only_sandbox_policy,
    config_change_path_is_user_only_transport_policy,
};
pub use types::{
    ConfigBatchMutationPlan, ConfigDiagnostic, ConfigFormat, ConfigLayer, ConfigMutation,
    ConfigMutationOperation, ConfigMutationPlan, ConfigMutationValue, ConfigScope,
    ConfigValidation, ConfigValue, EffectiveConfig,
};
pub use validation::{
    compose_effective_config, persist_config_mutation, persist_config_text, plan_config_mutation,
    plan_config_mutations, validate_config_file, validate_config_text,
};
#[cfg(test)]
pub use validation::{persist_config_mutation_async, validate_config_file_async};

pub(crate) use extract::contains_secret_material;
use extract::{
    clean_key_segment, clean_value, extract_config_values, extract_json_paths, extract_toml_paths,
    extract_yaml_paths, line_indent, validate_command_rule_effects, validate_command_rule_examples,
    validate_known_schema_path, validate_mcp_server_path, validate_permission_value,
    validate_permissions_path,
};
use migration::parse_config_schema_version;
use mutation::{
    mutate_json_text, mutate_toml_text, mutate_yaml_text, parse_mutation_path,
    reject_container_target, reject_unsupported_mutation_path,
};
pub(crate) use parsers::parse_config_json_value;
use parsers::{
    JsonPathParser, JsonValueParser, parse_config_json_object, parse_config_json_value_best_effort,
};
#[cfg(test)]
use paths::write_private_config_file_async;
use paths::{format_diagnostics, write_private_config_file};
use schema::{
    AGENT_AUTO_SIZING_KEYS, AGENT_KEYS, AUDIT_KEYS, AUTH_KEYS, BUBBLEWRAP_PERMISSION_KEYS,
    COMMAND_RULE_EFFECT_KEYS, COMMAND_RULE_KEYS, CONTROL_KEYS, HISTORY_KEYS, HOOK_KEYS, HOST_KEYS,
    HOST_LEASE_KEYS, INSTRUCTION_KEYS, IROH_TRANSPORT_KEYS, ISSUE_KEYS, KEY_BINDING_KEYS,
    KEY_PRESET_KEYS, LAYOUT_KEYS, MCP_SERVER_KEYS, MEMORY_KEYS, MESSAGE_PROTOCOL_KEYS,
    MODEL_PRESET_KEYS, MODEL_PROFILE_KEYS, PANE_FRAME_KEYS, PERMISSION_KEYS,
    PERSONALITY_PROFILE_KEYS, PROVIDER_KEYS, RUNTIME_KEYS, SESSION_KEYS, SHELL_KEYS, SNAPSHOT_KEYS,
    SUBAGENT_PROFILE_KEYS, TERMINAL_KEYS, THEME_KEYS, WINDOW_FRAME_KEYS,
};

/// Reads the Tokio worker count from the migrated primary user configuration.
///
/// Project overlays are intentionally excluded because Tokio must be built
/// before asynchronous CLI dispatch can discover and trust a project. Missing
/// primary configuration uses the generated default of two worker threads.
pub fn runtime_cpu_count_from_primary_config(paths: &ConfigPaths) -> Result<usize> {
    let Some(path) = paths.select_primary_file()? else {
        return Ok(2);
    };
    migrate_config_file(&path)?;
    let format = ConfigFormat::from_path(&path)?;
    let text = fs::read_to_string(&path)?;
    let validation = validate_config_text(format, &text, ConfigScope::Primary);
    if !validation.valid {
        return Err(MezError::config(format_diagnostics(
            &validation.diagnostics,
        )));
    }
    let values = extract_config_values(format, &text);
    match values.get("runtime.cpu_count") {
        None => Ok(2),
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| MezError::config("runtime.cpu_count must be a positive integer")),
    }
}

/// Loads primary and caller-directory project configuration using the
/// persisted project-trust database below the supplied config root.
pub(crate) fn load_runtime_config_layers_for_directory(
    paths: &ConfigPaths,
    current_dir: &Path,
) -> Result<Vec<ConfigLayer>> {
    let trust_store =
        ProjectTrustStore::load_from_file(&default_trust_database_path(paths.root()))?;
    load_runtime_config_layers_for_directory_with_trust(paths, &trust_store, current_dir)
}

/// Loads directory-scoped runtime layers against an explicit trust snapshot.
pub(crate) fn load_runtime_config_layers_for_directory_with_trust(
    paths: &ConfigPaths,
    trust_store: &ProjectTrustStore,
    current_dir: &Path,
) -> Result<Vec<ConfigLayer>> {
    let mut layers = load_primary_config_layers(paths)?;
    let project_root = discover_project_root(current_dir);
    let overlay_files = discover_existing_overlays(&project_root, current_dir)?;
    let trusted = trust_store
        .get(&project_root)
        .is_some_and(|record| record.state == TrustDecision::Trusted);
    let overlay_count = overlay_files.len();
    for (index, overlay_path) in overlay_files.into_iter().enumerate() {
        layers.push(ConfigLayer {
            name: if overlay_count == 1 {
                "project".to_string()
            } else {
                format!("project:{}", index + 1)
            },
            format: ConfigFormat::from_path(&overlay_path)?,
            text: fs::read_to_string(&overlay_path)?,
            path: Some(overlay_path),
            scope: ConfigScope::ProjectOverlay,
            trusted,
        });
    }
    Ok(layers)
}

/// Loads the migrated primary user layer or the generated default.
pub(crate) fn load_primary_config_layers(paths: &ConfigPaths) -> Result<Vec<ConfigLayer>> {
    let (path, format, text) = if let Some(path) = paths.select_primary_file()? {
        migrate_config_file(&path)?;
        let format = ConfigFormat::from_path(&path)?;
        let text = fs::read_to_string(&path)?;
        (Some(path), format, text)
    } else {
        (None, ConfigFormat::Toml, DEFAULT_CONFIG_TOML.to_string())
    };
    Ok(vec![ConfigLayer {
        name: "primary".to_string(),
        path,
        format,
        scope: ConfigScope::Primary,
        trusted: true,
        text,
    }])
}

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
