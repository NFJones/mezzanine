//! Regression coverage for the config tests subsystem.
//!
//! These tests describe the behavior protected by the repository
//! specification and workflow guidance. Keeping the scenarios documented
//! makes failures easier to map back to the user-visible contract.

// Config module tests.

use super::{
    CURRENT_CONFIG_SCHEMA_VERSION, ConfigFormat, ConfigLayer, ConfigMutation,
    ConfigMutationOperation, ConfigMutationValue, ConfigPaths, ConfigScope, DEFAULT_CONFIG_TOML,
    PathBuf, compose_effective_config, extract_config_values, fs, migrate_config_text,
    persist_config_mutation, persist_config_mutation_async, plan_config_mutation,
    validate_config_file, validate_config_file_async, validate_config_text,
};
use mez_agent::permissions::{exact_command_sha256, normalize_exact_command_text};
/// Runs the temp root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("mez-config-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    root
}

/// Runs the set string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn set_string(path: &str, value: &str) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::String(value.to_string())),
    }
}

/// Runs the set integer operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn set_integer(path: &str, value: i64) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::Integer(value)),
    }
}

/// Runs the set boolean operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn set_boolean(path: &str, value: bool) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::Boolean(value)),
    }
}

/// Runs the set string array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn set_string_array(path: &str, values: &[&str]) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(
            values.iter().map(|value| value.to_string()).collect(),
        )),
    }
}

/// Runs the unset operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn unset(path: &str) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Unset,
    }
}

mod command_rules;
mod defaults;
mod layers;
mod migration;
mod mutation;
mod parse;
mod schema_validation;
mod validation;
