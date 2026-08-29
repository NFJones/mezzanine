//! Configuration schema v76 to v77 migration.
//!
//! Schema v77 replaces each provider's model-id array with a keyed table of
//! reusable provider-model metadata records. Migration preserves every model
//! id and profile-local setting while generating deterministic path-safe keys.

use std::collections::BTreeSet;

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Converts provider model-id arrays into keyed records and advances to v77.
pub(super) fn migrate_v76_to_v77(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            migrate_toml_provider_models(&mut document);
            set_toml_path_item(&mut document, "version", toml_edit::value(77))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            migrate_json_provider_models(&mut document);
            set_json_path_value(&mut document, "version", serde_json::json!(77))?;
            match format {
                ConfigFormat::Json => serde_json::to_string_pretty(&document)
                    .map(|mut rendered| {
                        rendered.push('\n');
                        rendered
                    })
                    .map_err(|error| {
                        MezError::config(format!("failed to render JSON config: {error}"))
                    }),
                ConfigFormat::Yaml => serde_norway::to_string(&document).map_err(|error| {
                    MezError::config(format!("failed to render YAML config: {error}"))
                }),
                ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
            }
        }
    }
}

/// Rewrites all well-formed TOML provider model-id arrays in place.
fn migrate_toml_provider_models(document: &mut toml_edit::DocumentMut) {
    let Some(providers) = document
        .as_table_mut()
        .get_mut("providers")
        .and_then(toml_edit::Item::as_table_mut)
    else {
        return;
    };
    for (_provider_id, provider) in providers.iter_mut() {
        let Some(provider) = provider.as_table_mut() else {
            continue;
        };
        let Some(model_ids) = provider
            .get("models")
            .and_then(toml_edit::Item::as_array)
            .and_then(|models| {
                models
                    .iter()
                    .map(|model| model.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()
            })
        else {
            continue;
        };
        let mut used_keys = BTreeSet::new();
        let mut models = toml_edit::Table::new();
        models.set_implicit(true);
        for model_id in model_ids {
            let key = unique_model_entry_key(&model_id, &mut used_keys);
            let mut record = toml_edit::Table::new();
            record.insert("id", toml_edit::value(model_id));
            models.insert(&key, toml_edit::Item::Table(record));
        }
        provider.insert("models", toml_edit::Item::Table(models));
    }
}

/// Rewrites all well-formed JSON-compatible provider model-id arrays in place.
fn migrate_json_provider_models(document: &mut serde_json::Value) {
    let Some(providers) = document
        .get_mut("providers")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    for provider in providers.values_mut() {
        let Some(provider) = provider.as_object_mut() else {
            continue;
        };
        let Some(model_ids) = provider.get("models").and_then(|models| {
            models.as_array().and_then(|models| {
                models
                    .iter()
                    .map(|model| model.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()
            })
        }) else {
            continue;
        };
        let mut used_keys = BTreeSet::new();
        let mut models = serde_json::Map::new();
        for model_id in model_ids {
            let key = unique_model_entry_key(&model_id, &mut used_keys);
            models.insert(key, serde_json::json!({ "id": model_id }));
        }
        provider.insert("models".to_string(), serde_json::Value::Object(models));
    }
}

/// Returns one deterministic path-safe key, adding a numeric collision suffix.
fn unique_model_entry_key(model_id: &str, used_keys: &mut BTreeSet<String>) -> String {
    let base = path_safe_model_entry_key(model_id);
    if used_keys.insert(base.clone()) {
        return base;
    }
    for suffix in 2usize.. {
        let candidate = format!("{base}-{suffix}");
        if used_keys.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("an unbounded numeric suffix always provides a unique model entry key")
}

/// Normalizes a provider-facing model id into an ASCII config-path segment.
fn path_safe_model_entry_key(model_id: &str) -> String {
    let mut key = String::new();
    let mut previous_separator = false;
    for character in model_id.trim().chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
            key.push(character.to_ascii_lowercase());
            previous_separator = false;
        } else if !previous_separator {
            key.push('-');
            previous_separator = true;
        }
    }
    let key = key.trim_matches(['-', '_']);
    if key.is_empty() {
        "model".to_string()
    } else {
        key.to_string()
    }
}
