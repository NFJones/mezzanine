//! Pure exact-entry ownership planning for install, upgrade and uninstall.
//!
//! Vendor configuration remains opaque except the adapter's exact JSON pointer.
//! Unknown or edited ownership is a conflict; uninstall never restores an old
//! whole-file backup. Shared documents retain unrelated semantic values. Strict
//! JSON is intentionally supported here; JSONC/TOML adapters need their own
//! format-preserving planner rather than silently stripping comments.

use crate::error::{MezError, Result};
use serde::{Deserialize, Serialize};

/// Artifact kind supplied by a trusted, certified adapter manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum Artifact {
    /// Complete file whose bytes are exclusively owned by this integration.
    File { bytes: Vec<u8> },
    /// One exact entry in a shared strict-JSON document, with existing ancestors.
    JsonEntry {
        pointer: String,
        value: serde_json::Value,
    },
    /// One exact array member, never ownership of the containing shared array.
    JsonArrayEntry {
        pointer: String,
        value: serde_json::Value,
    },
}

/// One bounded adapter-owned destination, relative to an explicit root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Relative destination; no absolute, parent or reserved path components.
    pub(crate) path: String,
    /// Exact owned payload, not rendered text or event JSON.
    pub(crate) artifact: Artifact,
}

/// Validates one artifact's admission limits and reserved namespace.
pub(crate) fn validate(entry: &Entry) -> Result<()> {
    publication_path(&entry.path)?;
    match &entry.artifact {
        Artifact::File { bytes } if bytes.len() > 1024 * 1024 => Err(MezError::invalid_args(
            "bootstrap artifact exceeds byte limit",
        )),
        Artifact::JsonEntry { pointer, .. } | Artifact::JsonArrayEntry { pointer, .. } => {
            pointer_parts(pointer)?;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Checks a bounded relative path without filesystem discovery.
pub(super) fn publication_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 1024
        || path.split('/').count() > 16
        || std::path::Path::new(path)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        || path
            .split('/')
            .any(|part| part.is_empty() || part.starts_with(".mez-bootstrap"))
    {
        return Err(MezError::invalid_args("unsafe bootstrap destination"));
    }
    Ok(())
}

/// Resolves object-only JSON pointer components, rejecting ambiguous escapes.
fn pointer_parts(pointer: &str) -> Result<Vec<String>> {
    if !pointer.starts_with('/') || pointer.len() > 1024 {
        return Err(MezError::invalid_args("bootstrap JSON pointer unavailable"));
    }
    let mut parts = Vec::new();
    for part in pointer[1..].split('/') {
        let mut value = String::new();
        let mut chars = part.chars();
        while let Some(ch) = chars.next() {
            value.push(if ch == '~' {
                match chars.next() {
                    Some('0') => '~',
                    Some('1') => '/',
                    _ => {
                        return Err(MezError::invalid_args(
                            "invalid bootstrap JSON pointer escape",
                        ));
                    }
                }
            } else {
                ch
            });
        }
        if value.is_empty() {
            return Err(MezError::invalid_args("empty bootstrap JSON key"));
        }
        parts.push(value);
    }
    if parts.len() > 16 {
        return Err(MezError::invalid_args("bootstrap JSON pointer too deep"));
    }
    Ok(parts)
}

/// Produces replacement bytes, or `None` for an owned-file deletion.
/// `previous` is the accepted manifest receipt, never inferred from matching text.
/// A first install refuses an already populated slot even if it looks identical.
pub(crate) fn reconcile(
    current: Option<&[u8]>,
    previous: Option<&Artifact>,
    desired: Option<&Artifact>,
) -> Result<Option<Vec<u8>>> {
    if current.is_some_and(|bytes| bytes.len() > 1024 * 1024) {
        return Err(MezError::invalid_args(
            "bootstrap destination exceeds byte limit",
        ));
    }
    let basis = desired
        .or(previous)
        .ok_or_else(|| MezError::invalid_args("bootstrap artifact unavailable"))?;
    match basis {
        Artifact::File { .. } => {
            let expected = match previous {
                Some(Artifact::File { bytes }) => Some(bytes.as_slice()),
                None => None,
                _ => {
                    return Err(MezError::conflict(
                        "bootstrap artifact ownership kind changed",
                    ));
                }
            };
            if current != expected {
                return Err(MezError::conflict(
                    "bootstrap owned file changed; no overwrite",
                ));
            }
            match desired {
                Some(Artifact::File { bytes }) if bytes.len() <= 1024 * 1024 => {
                    Ok(Some(bytes.clone()))
                }
                None => Ok(None),
                _ => Err(MezError::invalid_args("bootstrap artifact invalid")),
            }
        }
        Artifact::JsonArrayEntry { pointer, .. } => {
            reconcile_array(current, previous, desired, pointer)
        }
        Artifact::JsonEntry { pointer, .. } => {
            let parts = pointer_parts(pointer)?;
            let mut document: serde_json::Value = match current {
                Some(bytes) => serde_json::from_slice(bytes).map_err(|_| {
                    MezError::invalid_args(
                        "bootstrap requires strict JSON; authored document unchanged",
                    )
                })?,
                None => serde_json::json!({}),
            };
            let mut parent = &mut document;
            for key in &parts[..parts.len() - 1] {
                parent = parent.get_mut(key).ok_or_else(|| {
                    MezError::conflict(
                        "bootstrap JSON parent missing; adapter must provide a qualified edit",
                    )
                })?;
            }
            let object = parent
                .as_object_mut()
                .ok_or_else(|| MezError::invalid_args("bootstrap JSON parent must be object"))?;
            let key = &parts[parts.len() - 1];
            let expected = match previous {
                Some(Artifact::JsonEntry {
                    pointer: old,
                    value,
                }) if old == pointer => Some(value),
                None => None,
                _ => {
                    return Err(MezError::conflict(
                        "bootstrap JSON ownership location changed",
                    ));
                }
            };
            if object.get(key) != expected {
                return Err(MezError::conflict(
                    "bootstrap owned JSON entry changed; no overwrite",
                ));
            }
            match desired {
                Some(Artifact::JsonEntry {
                    pointer: new,
                    value,
                }) if new == pointer => {
                    object.insert(key.clone(), value.clone());
                }
                None => {
                    object.remove(key);
                }
                _ => {
                    return Err(MezError::conflict(
                        "bootstrap JSON ownership location changed",
                    ));
                }
            }
            if let Some(bytes) = current {
                let original: serde_json::Value = serde_json::from_slice(bytes)
                    .map_err(|_| MezError::invalid_args("bootstrap JSON unavailable"))?;
                if original == document {
                    return Ok(Some(bytes.to_vec()));
                }
            }
            let mut bytes = serde_json::to_vec_pretty(&document).map_err(|error| {
                MezError::invalid_state(format!("bootstrap JSON encoding failed: {error}"))
            })?;
            bytes.push(b'\n');
            if bytes.len() > 1024 * 1024 {
                return Err(MezError::invalid_args(
                    "bootstrap replacement exceeds byte limit",
                ));
            }
            Ok(Some(bytes))
        }
    }
}

/// Reconciles a single exact owned array member while preserving sibling order.
/// Missing or ambiguous prior ownership conflicts; unowned matching members are
/// never adopted. Replacement retains its old position and uninstall keeps the
/// shared document/array, not a stale whole-file backup.
fn reconcile_array(
    current: Option<&[u8]>,
    previous: Option<&Artifact>,
    desired: Option<&Artifact>,
    pointer: &str,
) -> Result<Option<Vec<u8>>> {
    let parts = pointer_parts(pointer)?;
    let old = match previous {
        Some(Artifact::JsonArrayEntry {
            pointer: old,
            value,
        }) if old == pointer => Some(value),
        None => None,
        _ => {
            return Err(MezError::conflict(
                "bootstrap array ownership location changed",
            ));
        }
    };
    let new = match desired {
        Some(Artifact::JsonArrayEntry {
            pointer: new,
            value,
        }) if new == pointer => Some(value),
        None => None,
        _ => {
            return Err(MezError::conflict(
                "bootstrap array ownership location changed",
            ));
        }
    };
    let mut document: serde_json::Value = match current {
        Some(bytes) => super::strict_json::decode(bytes)?,
        None => serde_json::json!({}),
    };
    let mut parent = &mut document;
    for key in &parts[..parts.len() - 1] {
        parent = parent
            .get_mut(key)
            .ok_or_else(|| MezError::conflict("bootstrap array parent unavailable"))?;
    }
    let object = parent
        .as_object_mut()
        .ok_or_else(|| MezError::conflict("bootstrap array parent must be object"))?;
    let key = &parts[parts.len() - 1];
    if !object.contains_key(key) {
        if old.is_some() {
            return Err(MezError::conflict(
                "bootstrap owned array entry disappeared",
            ));
        }
        object.insert(key.clone(), serde_json::json!([]));
    }
    let array = object
        .get_mut(key)
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| MezError::conflict("bootstrap target must be an array"))?;
    if array.len() > 4096 {
        return Err(MezError::invalid_args(
            "bootstrap array exceeds entry limit",
        ));
    }
    let indices = old.map(|old| {
        array
            .iter()
            .enumerate()
            .filter_map(|(index, value)| (value == old).then_some(index))
            .collect::<Vec<_>>()
    });
    if indices.as_ref().is_some_and(|indices| indices.len() != 1) {
        return Err(MezError::conflict(
            "bootstrap owned array entry changed or ambiguous",
        ));
    }
    let position = indices
        .as_ref()
        .and_then(|indices| indices.first())
        .copied();
    if new.is_some_and(|new| {
        array
            .iter()
            .enumerate()
            .any(|(index, value)| Some(index) != position && value == new)
    }) {
        return Err(MezError::conflict(
            "bootstrap array target has unowned matching entry",
        ));
    }
    match (position, new) {
        (Some(index), Some(value)) => array[index] = value.clone(),
        (Some(index), None) => {
            array.remove(index);
        }
        (None, Some(value)) => {
            if array.len() == 4096 {
                return Err(MezError::invalid_args("bootstrap array capacity exhausted"));
            }
            array.push(value.clone());
        }
        (None, None) => {}
    }
    if let Some(bytes) = current {
        let original: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|_| MezError::invalid_args("bootstrap JSON unavailable"))?;
        if original == document {
            return Ok(Some(bytes.to_vec()));
        }
    }
    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|_| MezError::invalid_state("bootstrap array encoding failed"))?;
    bytes.push(b'\n');
    if bytes.len() > 1024 * 1024 {
        return Err(MezError::invalid_args(
            "bootstrap array replacement exceeds byte limit",
        ));
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Install, repeated install, upgrade and uninstall own one exact entry and
    /// preserve unrelated user edits. Matching unowned content remains a conflict.
    #[test]
    fn bootstrap_json_reconciliation_preserves_unrelated_edits() {
        let first = Artifact::JsonEntry {
            pointer: "/hooks/mez".into(),
            value: serde_json::json!({"argv":["mez","harness-event"]}),
        };
        let initial = br#"{"hooks":{"user":{"keep":true}},"other":7}"#;
        let installed = reconcile(Some(initial), None, Some(&first))
            .unwrap()
            .unwrap();
        assert!(reconcile(Some(&installed), None, Some(&first)).is_err());
        assert_eq!(
            reconcile(Some(&installed), Some(&first), Some(&first))
                .unwrap()
                .unwrap(),
            installed
        );
        let mut authored: serde_json::Value = serde_json::from_slice(&installed).unwrap();
        authored["other"] = 9.into();
        let authored = serde_json::to_vec(&authored).unwrap();
        let upgraded = Artifact::JsonEntry {
            pointer: "/hooks/mez".into(),
            value: serde_json::json!({"argv":["mez","harness-event"],"version":2}),
        };
        let bytes = reconcile(Some(&authored), Some(&first), Some(&upgraded))
            .unwrap()
            .unwrap();
        let bytes = reconcile(Some(&bytes), Some(&upgraded), None)
            .unwrap()
            .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result["other"], 9);
        assert_eq!(result["hooks"]["user"]["keep"], true);
        assert!(result["hooks"].get("mez").is_none());
    }

    /// Edited ownership, special path syntax, missing parents and JSONC must
    /// fail closed rather than expanding the adapter's edit authority.
    #[test]
    fn bootstrap_reconciliation_refuses_conflicts_and_shaped_paths() {
        let file = Artifact::File {
            bytes: b"owned".to_vec(),
        };
        assert!(reconcile(Some(b"edited"), Some(&file), None).is_err());
        for path in [
            "../config",
            "/config",
            "a/../b",
            ".mez-bootstrap-journal",
            "a//b",
        ] {
            assert!(publication_path(path).is_err(), "{path}");
        }
        let entry = Artifact::JsonEntry {
            pointer: "/hooks/mez".into(),
            value: true.into(),
        };
        assert!(reconcile(Some(b"{}"), None, Some(&entry)).is_err());
        assert!(reconcile(Some(b"{/*comment*/}"), None, Some(&entry)).is_err());
    }

    /// Exact array ownership preserves authored order/settings across install,
    /// repeat, upgrade and uninstall; unreceipted/duplicate/edited matches refuse
    /// mutation instead of adopting arbitrary sibling plugin registrations.
    #[test]
    fn bootstrap_array_reconciliation_preserves_siblings_and_exact_ownership() {
        let first = Artifact::JsonArrayEntry {
            pointer: "/plugin".into(),
            value: serde_json::json!("./mezzanine-tui.mjs"),
        };
        let original = br#"{"plugin":["user-a",["user-b",{"enabled":true}]],"theme":"authored"}"#;
        let installed = reconcile(Some(original), None, Some(&first))
            .unwrap()
            .unwrap();
        assert!(reconcile(Some(&installed), None, Some(&first)).is_err());
        assert_eq!(
            reconcile(Some(&installed), Some(&first), Some(&first))
                .unwrap()
                .unwrap(),
            installed
        );
        let mut authored: serde_json::Value = serde_json::from_slice(&installed).unwrap();
        authored["plugin"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("user-c"));
        let authored = serde_json::to_vec(&authored).unwrap();
        let second = Artifact::JsonArrayEntry {
            pointer: "/plugin".into(),
            value: serde_json::json!("./mezzanine-tui-v2.mjs"),
        };
        let upgraded = reconcile(Some(&authored), Some(&first), Some(&second))
            .unwrap()
            .unwrap();
        let document: serde_json::Value = serde_json::from_slice(&upgraded).unwrap();
        assert_eq!(
            document["plugin"],
            serde_json::json!(["user-a",["user-b",{"enabled":true}],"./mezzanine-tui-v2.mjs","user-c"])
        );
        let removed = reconcile(Some(&upgraded), Some(&second), None)
            .unwrap()
            .unwrap();
        let document: serde_json::Value = serde_json::from_slice(&removed).unwrap();
        assert_eq!(
            document["plugin"],
            serde_json::json!(["user-a",["user-b",{"enabled":true}],"user-c"])
        );
        assert_eq!(document["theme"], "authored");
        for current in [
            br#"{"plugin":{}}"#.as_slice(),
            br#"{"plugin":["./mezzanine-tui.mjs","./mezzanine-tui.mjs"]}"#,
            br#"{"plugin":[]}"#,
        ] {
            assert!(reconcile(Some(current), Some(&first), Some(&second)).is_err());
        }
        let created = reconcile(None, None, Some(&first)).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&created).unwrap()["plugin"],
            serde_json::json!(["./mezzanine-tui.mjs"])
        );
    }

    /// Generic JSON parsing overwrites duplicate keys and could discard authored
    /// plugins while appending ours. Ambiguous root/nested fields must refuse
    /// mutation before producing replacement bytes, including escaped aliases.
    #[test]
    fn bootstrap_array_reconciliation_refuses_duplicate_authored_fields() {
        let entry = Artifact::JsonArrayEntry {
            pointer: "/plugin".into(),
            value: serde_json::json!("./mez-tui.mjs"),
        };
        for input in [
            br#"{"plugin":["user"],"plugin":[]}"#.as_slice(),
            br#"{"plugin":["user"],"\u0070lugin":[]}"#,
            br#"{"plugin":[],"authored":{"value":1,"value":2}}"#,
        ] {
            assert!(reconcile(Some(input), None, Some(&entry)).is_err());
        }
    }
}
