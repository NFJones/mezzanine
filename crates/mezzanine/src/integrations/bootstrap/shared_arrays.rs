//! Bounded exact-member ownership across distinct arrays in one strict JSON file.
//!
//! Compiled manifests and receipts authorize only enumerated pointers and complete
//! values. Existing single-array reconciliation owns member equality, order,
//! duplicate policy and finite bounds. This module creates missing object parents
//! only for a new registration, never repairs missing owned state, deletes shared
//! containers or interprets command substrings. Changed strict JSON may reformat;
//! byte-exact noops survive and JSONC/TOML remain unsupported rather than stripped.

use super::reconciliation::{
    Artifact, JsonArrayMember, pointer_parts, reconcile_array, validate_members,
};
use crate::error::{MezError, Result};
use std::collections::{BTreeMap, BTreeSet};

/// Extracts and validates only the compiled shared-array artifact kind.
fn members(artifact: Option<&Artifact>) -> Result<&[JsonArrayMember]> {
    match artifact {
        Some(Artifact::JsonArrayEntries { entries }) => {
            validate_members(entries)?;
            Ok(entries)
        }
        None => Ok(&[]),
        _ => Err(MezError::conflict(
            "bootstrap shared array ownership kind changed",
        )),
    }
}

/// Produces one shared document, never a containing-file deletion. Caller supplies
/// qualified ownership; current private transactions may collapse exact old copies.
pub(super) fn reconcile_arrays(
    current: Option<&[u8]>,
    previous: Option<&Artifact>,
    desired: Option<&Artifact>,
    deduplicate: bool,
) -> Result<Option<Vec<u8>>> {
    if current.is_some_and(|bytes| bytes.len() > 1024 * 1024) {
        return Err(MezError::invalid_args(
            "bootstrap destination exceeds byte limit",
        ));
    }
    let old = members(previous)?
        .iter()
        .map(|entry| (entry.pointer.as_str(), &entry.value))
        .collect::<BTreeMap<_, _>>();
    let new = members(desired)?
        .iter()
        .map(|entry| (entry.pointer.as_str(), &entry.value))
        .collect::<BTreeMap<_, _>>();
    let paths = old
        .keys()
        .chain(new.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    if paths.is_empty() || paths.len() > 16 {
        return Err(MezError::invalid_args(
            "bootstrap shared array reconciliation bounds",
        ));
    }
    let mut bytes = current.map(<[u8]>::to_vec);
    for pointer in paths {
        let previous = old.get(pointer).map(|value| Artifact::JsonArrayEntry {
            pointer: pointer.into(),
            value: (*value).clone(),
        });
        let desired = new.get(pointer).map(|value| Artifact::JsonArrayEntry {
            pointer: pointer.into(),
            value: (*value).clone(),
        });
        if previous.is_none() && desired.is_some() {
            bytes = Some(with_new_parents(bytes.as_deref(), pointer)?);
        }
        bytes = reconcile_array(
            bytes.as_deref(),
            previous.as_ref(),
            desired.as_ref(),
            pointer,
            deduplicate,
        )?;
    }
    Ok(bytes)
}

/// Creates only absent object ancestors for a new pointer; wrong existing types
/// reject. A semantic noop returns original bytes, including whitespace/order.
fn with_new_parents(current: Option<&[u8]>, pointer: &str) -> Result<Vec<u8>> {
    let parts = pointer_parts(pointer)?;
    let mut document = match current {
        Some(bytes) => super::strict_json::decode(bytes)?,
        None => serde_json::json!({}),
    };
    let original = document.clone();
    let mut parent = &mut document;
    for key in &parts[..parts.len() - 1] {
        let object = parent
            .as_object_mut()
            .ok_or_else(|| MezError::conflict("bootstrap shared array parent must be object"))?;
        parent = object
            .entry(key.clone())
            .or_insert_with(|| serde_json::json!({}));
    }
    if original == document
        && let Some(bytes) = current
    {
        return Ok(bytes.to_vec());
    }
    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|_| MezError::invalid_state("bootstrap shared array encoding failed"))?;
    bytes.push(b'\n');
    if bytes.len() > 1024 * 1024 {
        return Err(MezError::invalid_args(
            "bootstrap shared array replacement exceeds byte limit",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds only test-owned exact registrations, never production authority.
    fn artifact(items: &[(&str, &str)]) -> Artifact {
        Artifact::JsonArrayEntries {
            entries: items
                .iter()
                .map(|(pointer, value)| JsonArrayMember {
                    pointer: (*pointer).into(),
                    value: serde_json::json!(value),
                })
                .collect(),
        }
    }

    /// Distinct arrays compose add/upgrade/remove without erasing shared parents
    /// or authored siblings. New parent creation is qualified by new ownership;
    /// a repeat preserves exact original bytes rather than reserializing a noop.
    #[test]
    fn bootstrap_shared_arrays_upgrade_union_and_exact_noop() {
        let old = artifact(&[("/hooks/A", "old-a"), ("/hooks/B", "old-b")]);
        let new = artifact(&[("/hooks/A", "new-a"), ("/other/C", "new-c")]);
        let input =
            br#"{ "hooks": {"A":["left","old-a","right"],"B":["old-b","user-b"]},"policy":false }"#;
        assert_eq!(
            reconcile_arrays(Some(input), Some(&old), Some(&old), false)
                .unwrap()
                .unwrap(),
            input
        );
        let upgraded = reconcile_arrays(Some(input), Some(&old), Some(&new), true)
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&upgraded).unwrap();
        assert_eq!(
            value["hooks"]["A"],
            serde_json::json!(["left", "new-a", "right"])
        );
        assert_eq!(value["hooks"]["B"], serde_json::json!(["user-b"]));
        assert_eq!(value["other"]["C"], serde_json::json!(["new-c"]));
        assert_eq!(value["policy"], false);
        assert!(
            reconcile_arrays(None, None, Some(&new), false)
                .unwrap()
                .is_some()
        );
        let removed = reconcile_arrays(Some(&upgraded), Some(&new), None, true)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&removed).unwrap()["hooks"]["A"],
            serde_json::json!(["left", "right"])
        );
    }

    /// Invalid overlap/duplicate/traversal pointers and excessive members/documents
    /// cannot become mutations. Shared arrays remain bounded independently of the
    /// caller's publisher, and wrong artifact kinds/types are never converted.
    #[test]
    fn bootstrap_shared_arrays_bounds_and_conflicts_are_strict() {
        for items in [
            vec![],
            vec![("/hooks/A", "a"), ("/hooks/A", "b")],
            vec![("/hooks", "a"), ("/hooks/A", "b")],
            vec![("/hooks/~2", "a")],
            vec![("/hooks/", "a")],
        ] {
            assert!(reconcile_arrays(None, None, Some(&artifact(&items)), true).is_err());
        }
        let too_many = Artifact::JsonArrayEntries {
            entries: (0..17)
                .map(|index| JsonArrayMember {
                    pointer: format!("/hooks/{index}"),
                    value: serde_json::json!("owned"),
                })
                .collect(),
        };
        assert!(reconcile_arrays(None, None, Some(&too_many), true).is_err());
        let owned = artifact(&[("/hooks/A", "owned")]);
        for input in [
            br#"{"hooks":{"A":["owned"]}}"#.as_slice(),
            br#"{"hooks":[]}"#,
            br#"{"hooks":{"A":null}}"#,
            br#"{"hooks":{},"hooks":{}}"#,
            b"// comments\n{}",
        ] {
            assert!(reconcile_arrays(Some(input), None, Some(&owned), true).is_err());
        }
        let oversize = vec![b' '; 1024 * 1024 + 1];
        assert!(reconcile_arrays(Some(&oversize), None, Some(&owned), true).is_err());
        let huge =
            serde_json::to_vec(&serde_json::json!({"hooks":{"A":vec!["user";4097]}})).unwrap();
        assert!(reconcile_arrays(Some(&huge), None, Some(&owned), true).is_err());
        let wrong = Artifact::File {
            bytes: b"{}".to_vec(),
        };
        assert!(reconcile_arrays(None, Some(&wrong), Some(&owned), true).is_err());
    }
}
