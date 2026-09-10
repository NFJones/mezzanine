//! Provider-model configuration helpers.
//!
//! This module owns deterministic local entry-key generation for provider
//! model records. Provider-facing model ids remain opaque; only the generated
//! table key is normalized for safe use in configuration paths.

use std::collections::{BTreeMap, BTreeSet};

use mez_agent::ProviderModelInfo;
use serde::Serialize;

use crate::error::{MezError, Result};

/// Model metadata fields that live synchronization may fill when absent.
const SYNC_METADATA_FIELDS: [&str; 6] = [
    "display_name",
    "reasoning_levels",
    "context_window_tokens",
    "max_input_tokens",
    "max_output_tokens",
    "capabilities",
];

/// Reports whether a reasoning level belongs to the canonical user-facing set.
fn canonical_reasoning_level(level: &str) -> bool {
    matches!(level, "low" | "medium" | "high" | "xhigh" | "max")
}

/// Reports whether a capability tag belongs to the provider-neutral vocabulary.
fn canonical_capability_tag(tag: &str) -> bool {
    matches!(
        tag.trim(),
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
    )
}

/// Filters one live metadata list through the canonical vocabulary and records
/// dropped values as conflicts so sync plans stay valid by construction.
fn sanitize_live_model_metadata(
    field: &str,
    observed: &serde_json::Value,
    entry_key: &str,
    id: &str,
    conflicts: &mut Vec<ProviderModelSyncConflict>,
) -> serde_json::Value {
    let Some(items) = observed.as_array() else {
        return observed.clone();
    };
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for value in items.iter().filter_map(serde_json::Value::as_str) {
        let supported = match field {
            "reasoning_levels" => canonical_reasoning_level(value),
            _ => canonical_capability_tag(value),
        };
        if supported {
            kept.push(value.to_string());
        } else {
            dropped.push(value.to_string());
        }
    }
    if !dropped.is_empty() {
        conflicts.push(ProviderModelSyncConflict {
            entry_key: entry_key.to_string(),
            id: id.to_string(),
            field: field.to_string(),
            configured: serde_json::Value::Array(
                kept.iter()
                    .map(|value| serde_json::Value::String(value.clone()))
                    .collect(),
            ),
            observed: observed.clone(),
        });
    }
    serde_json::Value::Array(
        kept.iter()
            .map(|value| serde_json::Value::String(value.clone()))
            .collect(),
    )
}

/// One model record proposed for addition by provider synchronization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProviderModelSyncAddition {
    /// Deterministic path-safe local table key.
    pub entry_key: String,
    /// Opaque canonical provider-facing model identifier.
    pub id: String,
}

/// One locally absent metadata field filled from live provider data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProviderModelSyncUpdate {
    /// Existing local table key.
    pub entry_key: String,
    /// Opaque canonical provider-facing model identifier.
    pub id: String,
    /// Metadata field populated by this update.
    pub field: String,
}

/// One live observation that conflicts with explicit local metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProviderModelSyncConflict {
    /// Existing local table key.
    pub entry_key: String,
    /// Opaque canonical provider-facing model identifier.
    pub id: String,
    /// Metadata field whose explicit local value remains authoritative.
    pub field: String,
    /// Explicit configured value retained by the plan.
    pub configured: serde_json::Value,
    /// Differing provider-observed value that was not applied.
    pub observed: serde_json::Value,
}

/// One reference that prevents a configured-only model from being pruned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProviderModelSyncBlocker {
    /// Canonical configured model identifier proposed for removal.
    pub id: String,
    /// Configuration path selecting the model by canonical id or alias.
    pub reference: String,
}

/// Pure deterministic plan for synchronizing one provider's model records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderModelSyncPlan {
    /// Complete resulting provider model table when the plan is applied.
    pub result_models: serde_json::Map<String, serde_json::Value>,
    /// Newly discovered records, sorted by provider-facing id.
    pub additions: Vec<ProviderModelSyncAddition>,
    /// Fill-only metadata updates, sorted by id and field.
    pub updates: Vec<ProviderModelSyncUpdate>,
    /// Explicit local/live conflicts preserved in favor of local values.
    pub conflicts: Vec<ProviderModelSyncConflict>,
    /// Configured-only model ids retained because pruning was not requested.
    pub retained: Vec<String>,
    /// Configured-only model ids proposed for removal.
    pub removals: Vec<String>,
    /// Complete reference blockers collected before any prune is applied.
    pub blockers: Vec<ProviderModelSyncBlocker>,
}

impl ProviderModelSyncPlan {
    /// Reports whether applying this plan would change provider model records.
    pub fn changed(&self) -> bool {
        !self.additions.is_empty() || !self.updates.is_empty() || !self.removals.is_empty()
    }
}

/// Builds one pure provider-model synchronization plan from raw live records.
///
/// Provider-facing ids are matched byte-for-byte. Live metadata fills only
/// absent local fields; explicit values, including empty lists, remain
/// authoritative. Aliases and provider options are never changed. Configured
/// models absent from the live response remain unless `prune` is explicit.
pub(crate) fn plan_provider_model_sync(
    root: &serde_json::Value,
    provider: &str,
    live_models: &[ProviderModelInfo],
    prune: bool,
) -> Result<ProviderModelSyncPlan> {
    plan_provider_model_sync_for_target(root, root, provider, live_models, prune)
}

/// Builds a synchronization plan whose writes are confined to one config layer.
///
/// `target_root` supplies the explicit records that may be changed or pruned,
/// while `effective_root` supplies inherited provider records and references.
/// Missing effective metadata may be represented by a minimal target-layer
/// override, but inherited records are never copied wholesale into the target.
pub(crate) fn plan_provider_model_sync_for_target(
    target_root: &serde_json::Value,
    effective_root: &serde_json::Value,
    provider: &str,
    live_models: &[ProviderModelInfo],
    prune: bool,
) -> Result<ProviderModelSyncPlan> {
    let effective_provider = effective_root
        .get("providers")
        .and_then(serde_json::Value::as_object)
        .and_then(|providers| providers.get(provider))
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::config(format!("provider `{provider}` is not configured")))?;
    let effective_models = match effective_provider.get("models") {
        None => serde_json::Map::new(),
        Some(value) => value
            .as_object()
            .cloned()
            .ok_or_else(|| MezError::config("provider models must be a table of model records"))?,
    };
    let target_provider = target_root
        .get("providers")
        .and_then(serde_json::Value::as_object)
        .and_then(|providers| providers.get(provider));
    let mut result_models = match target_provider {
        None => serde_json::Map::new(),
        Some(value) => match value
            .as_object()
            .ok_or_else(|| MezError::config(format!("provider `{provider}` must be a table")))?
            .get("models")
        {
            None => serde_json::Map::new(),
            Some(value) => value.as_object().cloned().ok_or_else(|| {
                MezError::config("provider models must be a table of model records")
            })?,
        },
    };

    let mut configured_by_id = BTreeMap::new();
    for (entry_key, value) in &result_models {
        let record = value
            .as_object()
            .ok_or_else(|| MezError::config("provider model record must be a table"))?;
        let id = record
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| MezError::config("provider model record id must be a string"))?;
        if configured_by_id
            .insert(id.to_string(), entry_key.clone())
            .is_some()
        {
            return Err(MezError::config(format!(
                "provider `{provider}` configures duplicate model id `{id}`"
            )));
        }
    }

    let mut effective_by_id = BTreeMap::new();
    let mut effective_aliases = BTreeMap::new();
    for (entry_key, value) in &effective_models {
        let record = value
            .as_object()
            .ok_or_else(|| MezError::config("provider model record must be a table"))?;
        let id = record
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| MezError::config("provider model record id must be a string"))?;
        if effective_by_id
            .insert(id.to_string(), entry_key.clone())
            .is_some()
        {
            return Err(MezError::config(format!(
                "provider `{provider}` configures duplicate model id `{id}`"
            )));
        }
        for alias in record_string_list(record, "aliases")? {
            if let Some(previous) = effective_aliases.insert(alias.clone(), id.to_string())
                && previous != id
            {
                return Err(MezError::config(format!(
                    "provider `{provider}` configures duplicate model alias `{alias}`"
                )));
            }
        }
    }

    let mut live_by_id = BTreeMap::new();
    for model in live_models {
        validate_live_model_id(&model.id)?;
        if live_by_id.insert(model.id.clone(), model).is_some() {
            return Err(MezError::invalid_args(format!(
                "provider `{provider}` returned duplicate model id `{}`",
                model.id
            )));
        }
    }
    for id in live_by_id.keys() {
        if let Some(configured_id) = effective_aliases.get(id) {
            return Err(MezError::new(
                crate::error::MezErrorKind::Conflict,
                format!(
                    "provider model id `{id}` collides with alias of configured model `{configured_id}`"
                ),
            ));
        }
    }

    let mut used_keys = effective_models.keys().cloned().collect::<BTreeSet<_>>();
    used_keys.extend(result_models.keys().cloned());
    let mut additions = Vec::new();
    let mut updates = Vec::new();
    let mut conflicts = Vec::new();
    for (id, live) in &live_by_id {
        if let Some(entry_key) = configured_by_id.get(id) {
            let effective_entry_key = effective_by_id.get(id).ok_or_else(|| {
                MezError::config(format!(
                    "provider `{provider}` target model `{id}` is absent from effective configuration"
                ))
            })?;
            let effective_record = effective_models
                .get(effective_entry_key)
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| MezError::config("provider model record must be a table"))?;
            let record = result_models
                .get_mut(entry_key)
                .and_then(serde_json::Value::as_object_mut)
                .ok_or_else(|| MezError::config("provider model record must be a table"))?;
            for field in SYNC_METADATA_FIELDS {
                let Some(observed) = live_model_field(live, field) else {
                    continue;
                };
                let observed = if matches!(field, "reasoning_levels" | "capabilities") {
                    sanitize_live_model_metadata(field, &observed, entry_key, id, &mut conflicts)
                } else {
                    observed
                };
                match effective_record.get(field) {
                    None => {
                        record.insert(field.to_string(), observed);
                        updates.push(ProviderModelSyncUpdate {
                            entry_key: entry_key.clone(),
                            id: id.clone(),
                            field: field.to_string(),
                        });
                    }
                    Some(configured) if configured != &observed => {
                        conflicts.push(ProviderModelSyncConflict {
                            entry_key: entry_key.clone(),
                            id: id.clone(),
                            field: field.to_string(),
                            configured: configured.clone(),
                            observed,
                        });
                    }
                    Some(_) => {}
                }
            }
        } else if let Some(entry_key) = effective_by_id.get(id) {
            let effective_record = effective_models
                .get(entry_key)
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| MezError::config("provider model record must be a table"))?;
            let mut record = serde_json::Map::new();
            record.insert("id".to_string(), serde_json::Value::String(id.clone()));
            for field in SYNC_METADATA_FIELDS {
                let Some(observed) = live_model_field(live, field) else {
                    continue;
                };
                let observed = if matches!(field, "reasoning_levels" | "capabilities") {
                    sanitize_live_model_metadata(field, &observed, entry_key, id, &mut conflicts)
                } else {
                    observed
                };
                match effective_record.get(field) {
                    None => {
                        record.insert(field.to_string(), observed);
                        updates.push(ProviderModelSyncUpdate {
                            entry_key: entry_key.clone(),
                            id: id.clone(),
                            field: field.to_string(),
                        });
                    }
                    Some(configured) if configured != &observed => {
                        conflicts.push(ProviderModelSyncConflict {
                            entry_key: entry_key.clone(),
                            id: id.clone(),
                            field: field.to_string(),
                            configured: configured.clone(),
                            observed,
                        });
                    }
                    Some(_) => {}
                }
            }
            if record.len() > 1 {
                result_models.insert(entry_key.clone(), serde_json::Value::Object(record));
            }
        } else {
            let entry_key = unique_model_entry_key(id, &mut used_keys);
            let mut record = serde_json::Map::new();
            record.insert("id".to_string(), serde_json::Value::String(id.clone()));
            for field in SYNC_METADATA_FIELDS {
                if let Some(value) = live_model_field(live, field) {
                    let value = if matches!(field, "reasoning_levels" | "capabilities") {
                        sanitize_live_model_metadata(field, &value, &entry_key, id, &mut conflicts)
                    } else {
                        value
                    };
                    record.insert(field.to_string(), value);
                }
            }
            result_models.insert(entry_key.clone(), serde_json::Value::Object(record));
            additions.push(ProviderModelSyncAddition {
                entry_key,
                id: id.clone(),
            });
        }
    }

    let mut retained = Vec::new();
    let mut removals = Vec::new();
    let mut blockers = Vec::new();
    for (id, entry_key) in &configured_by_id {
        if live_by_id.contains_key(id) {
            continue;
        }
        if !prune {
            retained.push(id.clone());
            continue;
        }
        removals.push(id.clone());
        let effective_entry_key = effective_by_id.get(id).unwrap_or(entry_key);
        let record = effective_models
            .get(effective_entry_key)
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| MezError::config("provider model record must be a table"))?;
        let mut identities = vec![id.clone()];
        identities.extend(record_string_list(record, "aliases")?);
        for reference in provider_model_reference_paths(effective_root, provider, &identities) {
            blockers.push(ProviderModelSyncBlocker {
                id: id.clone(),
                reference,
            });
        }
    }
    if prune && blockers.is_empty() {
        for id in &removals {
            if let Some(entry_key) = configured_by_id.get(id) {
                result_models.remove(entry_key);
            }
        }
    }

    updates.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.field.cmp(&right.field))
            .then(left.entry_key.cmp(&right.entry_key))
    });
    conflicts.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.field.cmp(&right.field))
            .then(left.entry_key.cmp(&right.entry_key))
    });
    blockers.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.reference.cmp(&right.reference))
    });

    Ok(ProviderModelSyncPlan {
        result_models,
        additions,
        updates,
        conflicts,
        retained,
        removals,
        blockers,
    })
}

/// Returns every provider default or profile path selecting any identity.
pub(crate) fn provider_model_reference_paths(
    root: &serde_json::Value,
    provider: &str,
    identities: &[String],
) -> Vec<String> {
    let identities = identities
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut references = Vec::new();
    if root
        .get("providers")
        .and_then(serde_json::Value::as_object)
        .and_then(|providers| providers.get(provider))
        .and_then(serde_json::Value::as_object)
        .and_then(|provider| provider.get("default_model"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|selected| identities.contains(selected))
    {
        references.push(format!("providers.{provider}.default_model"));
    }
    if let Some(profiles) = root
        .get("model_profiles")
        .and_then(serde_json::Value::as_object)
    {
        for (name, profile) in profiles {
            let Some(profile) = profile.as_object() else {
                continue;
            };
            if profile.get("provider").and_then(serde_json::Value::as_str) == Some(provider)
                && profile
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|selected| identities.contains(selected))
            {
                references.push(format!("model_profiles.{name}.model"));
            }
        }
    }
    references.sort();
    references
}

/// Rejects missing, padded, or control-bearing provider model identifiers.
fn validate_live_model_id(id: &str) -> Result<()> {
    if id.is_empty() || id.trim() != id || id.chars().any(char::is_control) {
        return Err(MezError::invalid_args(
            "provider model catalog contains an empty or invalid model id",
        ));
    }
    Ok(())
}

/// Projects one raw live metadata field without inventing omitted values.
fn live_model_field(model: &ProviderModelInfo, field: &str) -> Option<serde_json::Value> {
    match field {
        "display_name" => model
            .display_name
            .as_ref()
            .map(|value| serde_json::Value::String(value.clone())),
        "reasoning_levels" => model
            .reasoning_levels
            .as_ref()
            .map(|values| string_list_value(values.as_slice())),
        "context_window_tokens" => model.context_window_tokens.map(integer_value),
        "max_input_tokens" => model.max_input_tokens.map(integer_value),
        "max_output_tokens" => model.max_output_tokens.map(integer_value),
        "capabilities" => model
            .capabilities
            .as_ref()
            .map(|values| string_list_value(values.as_slice())),
        _ => None,
    }
}

/// Converts one string list to its JSON-compatible config representation.
fn string_list_value(values: &[String]) -> serde_json::Value {
    serde_json::Value::Array(
        values
            .iter()
            .cloned()
            .map(serde_json::Value::String)
            .collect(),
    )
}

/// Converts a positive platform-sized token count to JSON without truncation.
fn integer_value(value: usize) -> serde_json::Value {
    serde_json::Value::Number(serde_json::Number::from(value as u64))
}

/// Parses a configured string list used for alias-aware reference checks.
fn record_string_list(
    record: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Vec<String>> {
    let Some(value) = record.get(field) else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| MezError::config(format!("provider model {field} must be a string list")))?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                MezError::config(format!("provider model {field} must contain strings"))
            })
        })
        .collect()
}

/// Returns one deterministic path-safe key, adding a numeric collision suffix.
pub(crate) fn unique_model_entry_key(model_id: &str, used_keys: &mut BTreeSet<String>) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one raw provider model observation with caller-selected metadata.
    fn live_model(id: &str) -> ProviderModelInfo {
        ProviderModelInfo {
            id: id.to_string(),
            display_name: None,
            reasoning_levels: None,
            context_window_tokens: None,
            max_input_tokens: None,
            max_output_tokens: None,
            capabilities: None,
        }
    }

    /// Verifies synchronization fills only absent fields, preserves explicit
    /// empty/conflicting metadata, and allocates collision-safe opaque ids.
    #[test]
    fn provider_model_sync_planner_is_fill_only_and_collision_safe() {
        let root = serde_json::json!({
            "providers": {"custom": {"models": {
                "existing": {
                    "id": "existing/model",
                    "display_name": "Configured name",
                    "aliases": ["stable"],
                    "reasoning_levels": [],
                    "provider_options": {"tier": "local"}
                },
                "vendor-model-latest": {"id": "configured-only"}
            }}}
        });
        let mut existing = live_model("existing/model");
        existing.display_name = Some("Provider name".to_string());
        existing.reasoning_levels = Some(vec!["high".to_string()]);
        existing.context_window_tokens = Some(32_768);
        existing.capabilities = Some(Vec::new());
        let live = vec![
            existing,
            live_model("vendor/model:latest"),
            live_model("vendor.model/latest"),
        ];

        let plan = plan_provider_model_sync(&root, "custom", &live, false).unwrap();

        assert_eq!(
            plan.additions,
            vec![
                ProviderModelSyncAddition {
                    entry_key: "vendor-model-latest-2".to_string(),
                    id: "vendor.model/latest".to_string(),
                },
                ProviderModelSyncAddition {
                    entry_key: "vendor-model-latest-3".to_string(),
                    id: "vendor/model:latest".to_string(),
                },
            ]
        );
        assert_eq!(plan.retained, vec!["configured-only"]);
        assert!(plan.removals.is_empty());
        let existing = plan.result_models["existing"].as_object().unwrap();
        assert_eq!(existing["display_name"], "Configured name");
        assert_eq!(existing["reasoning_levels"], serde_json::json!([]));
        assert_eq!(existing["context_window_tokens"], 32_768);
        assert_eq!(existing["capabilities"], serde_json::json!([]));
        assert_eq!(existing["aliases"], serde_json::json!(["stable"]));
        assert_eq!(existing["provider_options"]["tier"], "local");
        assert!(
            plan.conflicts
                .iter()
                .any(|conflict| conflict.field == "display_name")
        );
        assert!(
            plan.conflicts
                .iter()
                .any(|conflict| conflict.field == "reasoning_levels")
        );
    }

    /// Verifies prune planning aggregates canonical and alias-selected
    /// provider/profile references before proposing an atomic removal.
    #[test]
    fn provider_model_sync_planner_aggregates_alias_reference_blockers() {
        let root = serde_json::json!({
            "providers": {"custom": {
                "default_model": "default-alias",
                "models": {"old": {
                    "id": "old/model",
                    "aliases": ["default-alias", "profile-alias"]
                }}
            }},
            "model_profiles": {
                "work": {"provider": "custom", "model": "profile-alias"}
            }
        });

        let plan = plan_provider_model_sync(&root, "custom", &[], true).unwrap();

        assert_eq!(plan.removals, vec!["old/model"]);
        assert_eq!(plan.blockers.len(), 2);
        assert_eq!(
            plan.blockers
                .iter()
                .map(|blocker| blocker.reference.as_str())
                .collect::<Vec<_>>(),
            vec![
                "model_profiles.work.model",
                "providers.custom.default_model"
            ]
        );
    }

    /// Verifies raw live catalogs reject ambiguous or unusable identifiers
    /// before any mutation plan is returned.
    #[test]
    fn provider_model_sync_planner_rejects_empty_and_duplicate_live_ids() {
        let root = serde_json::json!({"providers": {"custom": {"models": {
            "configured": {"id": "configured", "aliases": ["alias"]}
        }}}});
        assert!(plan_provider_model_sync(&root, "custom", &[live_model(" ")], false).is_err());
        assert!(
            plan_provider_model_sync(
                &root,
                "custom",
                &[live_model("same"), live_model("same")],
                false,
            )
            .is_err()
        );
        assert!(plan_provider_model_sync(&root, "custom", &[live_model("alias")], false).is_err());
    }
}
