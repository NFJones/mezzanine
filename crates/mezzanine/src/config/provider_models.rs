//! Provider-model configuration helpers.
//!
//! This module owns deterministic local entry-key generation for provider
//! model records. Provider-facing model ids remain opaque; only the generated
//! table key is normalized for safe use in configuration paths.

use std::collections::BTreeSet;

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
