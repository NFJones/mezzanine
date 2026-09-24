//! Effective runtime configuration layer materialization.
//!
//! This module owns validation, decoding, and merge of ordered config layers
//! into the single JSON value consumed by live runtime config application.
//! Keeping this separate leaves domain-specific option readers in sibling
//! modules while preserving the existing public facade export.

use serde_json::Value;

use crate::config::{ConfigLayer, ConfigScope, validate_config_text_with_document};
use crate::error::{MezError, Result};

/// Runs the runtime effective config value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn runtime_effective_config_value(layers: &[ConfigLayer]) -> Result<Value> {
    let mut root = Value::Object(serde_json::Map::new());
    for layer in layers {
        let (validation, document) =
            validate_config_text_with_document(layer.format, &layer.text, layer.scope);
        if !validation.valid {
            return Err(MezError::config(format!(
                "configuration layer `{}` is invalid",
                layer.name
            )));
        }
        if layer.scope == ConfigScope::ProjectOverlay && !layer.trusted {
            continue;
        }
        let value = document
            .ok_or_else(|| {
                MezError::config(format!("configuration layer `{}` is invalid", layer.name))
            })?
            .map_err(|error| {
                // The old runtime decoder reported the native conversion error,
                // without the config parser's format-specific prefix.
                let prefix = match layer.format {
                    crate::config::ConfigFormat::Toml => "invalid TOML config: ",
                    crate::config::ConfigFormat::Yaml => "invalid YAML config: ",
                    crate::config::ConfigFormat::Json => "invalid JSON config: ",
                };
                MezError::config(
                    error
                        .message()
                        .strip_prefix(prefix)
                        .unwrap_or(error.message()),
                )
            })?;
        runtime_merge_json_values(&mut root, value);
    }
    Ok(root)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigFormat;

    /// Validated TOML, JSON, and YAML documents retain recursive map merging;
    /// an untrusted overlay is still validated before it is skipped.
    #[test]
    fn runtime_layers_reuse_validated_documents_across_formats() {
        let layer = |name: &str, format, scope, trusted, text: &str| ConfigLayer {
            name: name.to_string(),
            path: None,
            format,
            scope,
            trusted,
            text: text.to_string(),
        };
        let primary = layer(
            "primary",
            ConfigFormat::Toml,
            ConfigScope::Primary,
            true,
            "[agents]\nturn_timeout_ms = 1000\nprovider_error_retry_limit = 2\n",
        );
        let overlay_text = format!(
            r#"{{"version":{},"agents":{{"turn_timeout_ms":2000}}}}"#,
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        );
        let overlay = layer(
            "overlay",
            ConfigFormat::Json,
            ConfigScope::ProjectOverlay,
            true,
            &overlay_text,
        );
        let yaml = layer(
            "yaml",
            ConfigFormat::Yaml,
            ConfigScope::Primary,
            true,
            "agents:\n  provider_error_retry_limit: 3\n",
        );
        let merged = runtime_effective_config_value(&[primary.clone(), overlay, yaml]).unwrap();
        assert_eq!(merged["agents"]["turn_timeout_ms"], 2000);
        assert_eq!(merged["agents"]["provider_error_retry_limit"], 3);

        let untrusted_text = format!(
            r#"{{"version":{},"agents":{{"turn_timeout_ms":4000}}}}"#,
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        );
        let untrusted = layer(
            "untrusted",
            ConfigFormat::Json,
            ConfigScope::ProjectOverlay,
            false,
            &untrusted_text,
        );
        let skipped = runtime_effective_config_value(&[primary.clone(), untrusted]).unwrap();
        assert_eq!(skipped["agents"]["turn_timeout_ms"], 1000);
        let malformed = layer(
            "malformed",
            ConfigFormat::Json,
            ConfigScope::ProjectOverlay,
            false,
            "{",
        );
        let error = runtime_effective_config_value(&[primary, malformed]).unwrap_err();
        assert!(
            error
                .message()
                .contains("configuration layer `malformed` is invalid")
        );
    }

    /// A syntax-valid YAML mapping with a non-string key must retain the
    /// original JSON-normalization error after semantic validation succeeds.
    #[test]
    fn runtime_layer_reports_yaml_normalization_error() {
        let layer = ConfigLayer {
            name: "non-string-key".to_string(),
            path: None,
            format: ConfigFormat::Yaml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "extensions:\n  ? [x, y]\n  : value\n".to_string(),
        };
        let validation =
            crate::config::validate_config_text(layer.format, &layer.text, layer.scope);
        assert!(validation.valid, "{:?}", validation.diagnostics);
        let error = runtime_effective_config_value(&[layer]).unwrap_err();
        assert!(
            error.message().contains("key must be a string"),
            "{error:?}"
        );
    }
}
