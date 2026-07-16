//! Effective runtime configuration layer materialization.
//!
//! This module owns validation, decoding, and merge of ordered config layers
//! into the single JSON value consumed by live runtime config application.
//! Keeping this separate leaves domain-specific option readers in sibling
//! modules while preserving the existing public facade export.

use serde_json::Value;

use crate::config::{ConfigFormat, ConfigLayer, ConfigScope, validate_config_text};
use crate::error::{MezError, Result};

/// Runs the runtime effective config value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn runtime_effective_config_value(layers: &[ConfigLayer]) -> Result<Value> {
    let mut root = Value::Object(serde_json::Map::new());
    for layer in layers {
        let validation = validate_config_text(layer.format, &layer.text, layer.scope);
        if !validation.valid {
            return Err(MezError::config(format!(
                "configuration layer `{}` is invalid",
                layer.name
            )));
        }
        if layer.scope == ConfigScope::ProjectOverlay && !layer.trusted {
            continue;
        }
        let value = runtime_config_layer_value(layer)?;
        runtime_merge_json_values(&mut root, value);
    }
    Ok(root)
}

/// Runs the runtime config layer value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_layer_value(layer: &ConfigLayer) -> Result<Value> {
    match layer.format {
        ConfigFormat::Toml => {
            let value = toml::from_str::<toml::Table>(&layer.text)
                .map_err(|error| MezError::config(error.to_string()))?;
            serde_json::to_value(value).map_err(|error| MezError::config(error.to_string()))
        }
        ConfigFormat::Yaml => {
            let value = serde_norway::from_str::<serde_norway::Value>(&layer.text)
                .map_err(|error| MezError::config(error.to_string()))?;
            serde_json::to_value(value).map_err(|error| MezError::config(error.to_string()))
        }
        ConfigFormat::Json => {
            serde_json::from_str(&layer.text).map_err(|error| MezError::config(error.to_string()))
        }
    }
}

/// Runs the runtime merge json values operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_merge_json_values(target: &mut Value, source: Value) {
    match (target, source) {
        (Value::Object(target), Value::Object(source)) => {
            for (key, value) in source {
                if let Some(existing) = target.get_mut(&key) {
                    runtime_merge_json_values(existing, value);
                } else {
                    target.insert(key, value);
                }
            }
        }
        (target, source) => *target = source,
    }
}
