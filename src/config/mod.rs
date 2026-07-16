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
pub use migration::{CURRENT_CONFIG_SCHEMA_VERSION, migrate_config_file};
#[cfg(test)]
pub use migration::{ConfigMigrationPlan, migrate_config_text};
pub use paths::ConfigPaths;
pub use schema::{
    BASELINE_TOP_LEVEL_KEYS, PRIMARY_CONFIG_FILENAMES,
    config_change_setting_path_annotations_markdown, config_change_setting_path_description,
};
pub use types::{
    ConfigDiagnostic, ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation,
    ConfigMutationPlan, ConfigMutationValue, ConfigScope, ConfigValidation, ConfigValue,
    EffectiveConfig,
};
pub use validation::{
    compose_effective_config, persist_config_mutation, persist_config_text, plan_config_mutation,
    validate_config_file, validate_config_text,
};
#[cfg(test)]
pub use validation::{persist_config_mutation_async, validate_config_file_async};

use extract::{
    clean_key_segment, clean_value, contains_secret_material, extract_config_values,
    extract_json_paths, extract_toml_paths, extract_yaml_paths, line_indent,
    validate_command_rule_examples, validate_known_schema_path, validate_mcp_server_path,
    validate_permission_value, validate_permissions_path,
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
use paths::{format_diagnostics, write_private_config_file, write_private_config_file_async};
use schema::{
    AGENT_AUTO_SIZING_KEYS, AGENT_KEYS, AUDIT_KEYS, AUTH_KEYS, COMMAND_RULE_KEYS, CONTROL_KEYS,
    HISTORY_KEYS, HOOK_KEYS, INSTRUCTION_KEYS, ISSUE_KEYS, KEY_BINDING_KEYS, LAYOUT_KEYS,
    MCP_SERVER_KEYS, MEMORY_KEYS, MESSAGE_PROTOCOL_KEYS, MODEL_PRESET_KEYS, MODEL_PROFILE_KEYS,
    PANE_FRAME_KEYS, PERMISSION_KEYS, PERSONALITY_PROFILE_KEYS, PROVIDER_KEYS, SESSION_KEYS,
    SHELL_KEYS, SNAPSHOT_KEYS, SUBAGENT_PROFILE_KEYS, TERMINAL_KEYS, THEME_KEYS, WINDOW_FRAME_KEYS,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
