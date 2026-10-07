//! Manifest receipts and exact-entry install/upgrade/uninstall transactions.
//!
//! The adapter registry supplies immutable manifests; process input never supplies
//! executable templates. Receipts record only owned payloads, not surrounding
//! vendor configuration. Receipt publication is last in the same recovery journal.
//! Shared documents may be reformatted on change, but unrelated values survive.

use super::publication::{Change, Publisher};
use super::reconciliation::{Entry, reconcile, validate};
use crate::error::{MezError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Trusted release-qualified adapter artifacts and required operator guidance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub(crate) harness: String,
    pub(crate) revision: u32,
    pub(crate) vendor_version: String,
    pub(crate) entries: Vec<Entry>,
}

/// Accepted ownership; unknown schemas never authorize overwrite or uninstall.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    manifest: Manifest,
}

/// Explicit accepted installation operation, independent of daemon availability.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) enum Operation {
    Install,
    Uninstall,
}

/// Journal intent is evidence only; recovery checks both manifests against
/// compiled authority and recomputes every artifact edit from its preimage.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    manifest: Manifest,
    previous: Option<Manifest>,
    operation: Operation,
}

/// One locked plan; publication verifies captured preimages again at commit.
pub(crate) struct Plan {
    publisher: Publisher,
    changes: Vec<Change>,
    intent: Intent,
}

/// Validates bounded registry metadata and one artifact per destination.
fn validate_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.harness.is_empty()
        || manifest.harness.len() > 32
        || !manifest
            .harness
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
        || manifest.revision == 0
        || manifest.vendor_version.is_empty()
        || manifest.vendor_version.len() > 128
        || manifest.vendor_version.chars().any(char::is_control)
        || manifest.entries.is_empty()
        || manifest.entries.len() > 15
    {
        return Err(MezError::invalid_args("bootstrap manifest bounds"));
    }
    let mut paths = BTreeSet::new();
    for entry in &manifest.entries {
        validate(entry)?;
        if entry.path.starts_with("mez-bootstrap-ownership-") || !paths.insert(&entry.path) {
            return Err(MezError::invalid_args(
                "bootstrap duplicate or reserved destination",
            ));
        }
    }
    receipt_bytes(manifest)?;
    Ok(())
}

/// Bounds the serialized receipt at admission, not after artifact publication.
fn receipt_bytes(manifest: &Manifest) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(&Receipt {
        schema: 1,
        manifest: manifest.clone(),
    })
    .map_err(|error| MezError::invalid_state(format!("bootstrap receipt encoding: {error}")))?;
    if bytes.len() > 1024 * 1024 {
        return Err(MezError::invalid_args(
            "bootstrap serialized ownership receipt exceeds byte limit",
        ));
    }
    Ok(bytes)
}

/// Captures a deterministic ownership-checked plan without changing destinations.
/// An existing journal requires explicit recovery rather than silent replay.
pub(crate) fn plan(root: &Path, manifest: &Manifest, operation: Operation) -> Result<Plan> {
    plan_with_history(
        root,
        manifest,
        operation,
        &super::compiled_history(manifest),
    )
}

/// Captures a plan with caller-owned compiled historical revisions. Neither a
/// receipt nor process input can add a recognized revision to this authority.
pub(crate) fn plan_with_history(
    root: &Path,
    manifest: &Manifest,
    operation: Operation,
    history: &[Manifest],
) -> Result<Plan> {
    validate_manifest(manifest)?;
    let publisher = Publisher::open(root)?;
    publisher.require_no_pending_journal()?;
    let receipt_path = format!("mez-bootstrap-ownership-{}.json", manifest.harness);
    let receipt_bytes = publisher.read(&receipt_path)?;
    let previous: Option<Receipt> = receipt_bytes
        .as_ref()
        .map(|bytes| {
            serde_json::from_slice(bytes)
                .map_err(|_| MezError::conflict("bootstrap receipt invalid; no mutation"))
        })
        .transpose()?;
    if let Some(receipt) = &previous {
        if receipt.schema != 1 || receipt.manifest.harness != manifest.harness {
            return Err(MezError::conflict(
                "bootstrap receipt identity/version mismatch",
            ));
        }
        validate_manifest(&receipt.manifest)?;
        if receipt.manifest != *manifest && !history.contains(&receipt.manifest) {
            return Err(MezError::conflict(
                "bootstrap receipt is not a recognized compiled manifest revision",
            ));
        }
    }
    let old = previous
        .as_ref()
        .map(|receipt| {
            receipt
                .manifest
                .entries
                .iter()
                .map(|entry| (entry.path.clone(), &entry.artifact))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let desired = if matches!(operation, Operation::Install) {
        manifest
            .entries
            .iter()
            .map(|entry| (entry.path.clone(), &entry.artifact))
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    let paths = old
        .keys()
        .chain(desired.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    if paths.len() > 15 {
        return Err(MezError::invalid_args(
            "bootstrap upgrade destination limit",
        ));
    }
    let mut changes = Vec::new();
    for path in paths {
        let before = publisher.read(&path)?;
        let after = reconcile(
            before.as_deref(),
            old.get(&path).copied(),
            desired.get(&path).copied(),
        )?;
        if before != after {
            changes.push(Change {
                path,
                before,
                after,
            });
        }
    }
    let after = if matches!(operation, Operation::Install) {
        Some(
            serde_json::to_vec(&Receipt {
                schema: 1,
                manifest: manifest.clone(),
            })
            .map_err(|error| {
                MezError::invalid_state(format!("bootstrap receipt encoding: {error}"))
            })?,
        )
    } else {
        None
    };
    if after
        .as_ref()
        .is_some_and(|bytes| bytes.len() > 1024 * 1024)
    {
        return Err(MezError::invalid_args(
            "bootstrap serialized ownership receipt exceeds byte limit",
        ));
    }
    if receipt_bytes != after {
        changes.push(Change {
            path: receipt_path,
            before: receipt_bytes,
            after,
        });
    }
    Ok(Plan {
        publisher,
        changes,
        intent: Intent {
            manifest: manifest.clone(),
            previous: previous.map(|receipt| receipt.manifest),
            operation,
        },
    })
}

impl Plan {
    /// Reports planned changes without exposing any configuration payload.
    pub(crate) fn changed_paths(&self) -> Vec<&str> {
        self.changes
            .iter()
            .map(|change| change.path.as_str())
            .collect()
    }

    /// Publishes the accepted plan once; no-op plans do not write any journal.
    pub(crate) fn apply(self) -> Result<()> {
        if self.changes.is_empty() {
            return Ok(());
        }
        let intent = serde_json::to_value(self.intent).map_err(|error| {
            MezError::invalid_state(format!("bootstrap intent encoding: {error}"))
        })?;
        self.publisher.apply_authorized(self.changes, intent)
    }
}

/// Explicitly settles previously accepted intent; conflicts preserve the journal.
pub(crate) fn recover(root: &Path, manifest: &Manifest) -> Result<bool> {
    recover_with_history(root, manifest, &[])
}

/// Authorizes recovery only for the selected compiled release and recognized
/// historical revisions. Journal paths/payloads never supply mutation authority.
pub(crate) fn recover_with_history(
    root: &Path,
    manifest: &Manifest,
    history: &[Manifest],
) -> Result<bool> {
    validate_manifest(manifest)?;
    let publisher = Publisher::open(root)?;
    publisher.recover_authorized(|value, changes| {
        let intent: Intent = serde_json::from_value(value.clone())
            .map_err(|_| MezError::conflict("bootstrap journal intent invalid"))?;
        if intent.manifest != *manifest
            || intent.previous.as_ref().is_some_and(|old| {
                old.harness != manifest.harness || (old != manifest && !history.contains(old))
            })
        {
            return Err(MezError::forbidden(
                "bootstrap recovery manifest/release mismatch",
            ));
        }
        if let Some(old) = &intent.previous {
            validate_manifest(old)?;
        }
        let old = intent
            .previous
            .as_ref()
            .map(|old| {
                old.entries
                    .iter()
                    .map(|entry| (entry.path.as_str(), &entry.artifact))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let desired = if matches!(intent.operation, Operation::Install) {
            manifest
                .entries
                .iter()
                .map(|entry| (entry.path.as_str(), &entry.artifact))
                .collect::<BTreeMap<_, _>>()
        } else {
            BTreeMap::new()
        };
        let receipt_path = format!("mez-bootstrap-ownership-{}.json", manifest.harness);
        let mut paths = BTreeSet::new();
        for change in changes {
            if !paths.insert(change.path.as_str()) {
                return Err(MezError::conflict("duplicate recovery destination"));
            }
            if change.path == receipt_path {
                let previous: Option<Receipt> = change
                    .before
                    .as_ref()
                    .map(|bytes| serde_json::from_slice(bytes))
                    .transpose()
                    .map_err(|_| MezError::conflict("recovery receipt invalid"))?;
                if previous.as_ref().is_some_and(|receipt| receipt.schema != 1)
                    || previous.map(|receipt| receipt.manifest) != intent.previous
                {
                    return Err(MezError::forbidden("recovery prior receipt mismatch"));
                }
                let expected = if matches!(intent.operation, Operation::Install) {
                    Some(receipt_bytes(manifest)?)
                } else {
                    None
                };
                if change.after != expected {
                    return Err(MezError::forbidden("recovery receipt payload mismatch"));
                }
            } else {
                let prior = old.get(change.path.as_str()).copied();
                let next = desired.get(change.path.as_str()).copied();
                if prior.is_none() && next.is_none() {
                    return Err(MezError::forbidden(
                        "recovery destination outside compiled manifest",
                    ));
                }
                if reconcile(change.before.as_deref(), prior, next)? != change.after {
                    return Err(MezError::forbidden("recovery artifact payload mismatch"));
                }
            }
        }
        // Omitted destinations must already satisfy the exact desired state;
        // a forged journal cannot omit a required edit or ownership receipt.
        for path in old.keys().chain(desired.keys()) {
            if paths.contains(path) {
                continue;
            }
            let current = publisher.read(path)?;
            let next = desired.get(path).copied();
            if next.is_some() {
                if reconcile(current.as_deref(), next, next)? != current {
                    return Err(MezError::conflict("recovery omitted artifact changed"));
                }
            } else if let Some(artifact) = old.get(path)
                && reconcile(current.as_deref(), None, Some(artifact)).is_err()
            {
                return Err(MezError::conflict("recovery omitted removal remains owned"));
            }
        }
        if !paths.contains(receipt_path.as_str()) {
            let current = publisher.read(&receipt_path)?;
            let expected = if matches!(intent.operation, Operation::Install) {
                Some(receipt_bytes(manifest)?)
            } else {
                None
            };
            if current != expected {
                return Err(MezError::conflict("recovery omitted ownership receipt"));
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// Recovery must bind the selected compiled release and original directory
    /// object, and recompute every change rather than trusting journal payloads.
    #[test]
    fn bootstrap_installer_recovery_rejects_wrong_authority_and_extra_changes() {
        let root = std::env::temp_dir().join(format!(
            "mez-recovery-authority-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        let copy = root.with_extension("copy");
        for path in [&root, &copy] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let manifest = Manifest {
            harness: "fixture".into(),
            revision: 1,
            vendor_version: "test-only".into(),
            entries: vec![Entry {
                path: "owned".into(),
                artifact: super::super::reconciliation::Artifact::File {
                    bytes: b"owned".to_vec(),
                },
            }],
        };
        let accepted = plan(&root, &manifest, Operation::Install).unwrap();
        accepted.publisher.stop_after.set(Some(1));
        assert!(accepted.apply().is_err());
        let journal_path = root.join(".mez-bootstrap-journal");
        let original = fs::read(&journal_path).unwrap();
        let mut wrong = manifest.clone();
        wrong.harness = "other".into();
        assert!(recover(&root, &wrong).is_err());
        wrong = manifest.clone();
        wrong.vendor_version = "different-release".into();
        assert!(recover(&root, &wrong).is_err());
        fs::write(copy.join(".mez-bootstrap-journal"), &original).unwrap();
        fs::write(copy.join("owned"), b"owned").unwrap();
        assert!(recover(&copy, &manifest).is_err());
        let mut forged: serde_json::Value = serde_json::from_slice(&original).unwrap();
        forged["changes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "path":"unrelated", "before":b"authored".to_vec(), "after":null,
            }));
        fs::write(root.join("unrelated"), b"authored").unwrap();
        fs::write(&journal_path, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert!(recover(&root, &manifest).is_err());
        assert_eq!(fs::read(root.join("unrelated")).unwrap(), b"authored");
        assert!(!root.join("mez-bootstrap-ownership-fixture.json").exists());
        fs::write(&journal_path, original).unwrap();
        assert!(recover(&root, &manifest).unwrap());
        assert!(!recover(&root, &manifest).unwrap());
        assert!(
            plan(&root, &manifest, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(copy).unwrap();
    }

    /// Serialized receipts count against admission bounds: a large byte-array
    /// payload is rejected during planning, while admitted payloads remain valid
    /// through apply, repeat and uninstall, not just the artifact-write phase.
    #[test]
    fn bootstrap_installer_receipt_bounds_are_checked_before_publication() {
        let root = std::env::temp_dir().join(format!(
            "mez-receipt-bounds-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let mut manifest = Manifest {
            harness: "fixture".into(),
            revision: 1,
            vendor_version: "test-only".into(),
            entries: vec![Entry {
                path: "owned".into(),
                artifact: super::super::reconciliation::Artifact::File {
                    bytes: vec![b'x'; 300 * 1024],
                },
            }],
        };
        assert!(plan(&root, &manifest, Operation::Install).is_err());
        assert!(!root.join("owned").exists());
        assert!(!root.join(".mez-bootstrap-journal").exists());
        manifest.entries[0].artifact = super::super::reconciliation::Artifact::File {
            bytes: vec![b'x'; 200 * 1024],
        };
        plan(&root, &manifest, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        assert!(
            plan(&root, &manifest, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        plan(&root, &manifest, Operation::Uninstall)
            .unwrap()
            .apply()
            .unwrap();
        assert!(!root.join("owned").exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// A shaped receipt must never manufacture ownership of an unrelated file,
    /// even if its asserted preimage matches current bytes exactly.
    #[test]
    fn bootstrap_installer_forged_receipt_cannot_delete_unrelated_file() {
        let root = std::env::temp_dir().join(format!(
            "mez-forged-receipt-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let manifest = Manifest {
            harness: "fixture".into(),
            revision: 1,
            vendor_version: "test-only".into(),
            entries: vec![Entry {
                path: "owned".into(),
                artifact: super::super::reconciliation::Artifact::File {
                    bytes: b"owned".to_vec(),
                },
            }],
        };
        let mut forged = manifest.clone();
        forged.entries[0].path = "unrelated".into();
        fs::write(root.join("unrelated"), b"owned").unwrap();
        fs::write(
            root.join("mez-bootstrap-ownership-fixture.json"),
            serde_json::to_vec(&Receipt {
                schema: 1,
                manifest: forged,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(plan(&root, &manifest, Operation::Uninstall).is_err());
        assert_eq!(fs::read(root.join("unrelated")).unwrap(), b"owned");
        fs::remove_dir_all(root).unwrap();
    }

    /// Receipts authorize only previously accepted entries: repeat is byte-stable,
    /// upgrade preserves unrelated values, uninstall removes only owned state.
    #[test]
    fn bootstrap_installer_receipts_preserve_authored_state() {
        let root = std::env::temp_dir().join(format!(
            "mez-installer-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("settings.json"), br#"{"hooks":{},"user":7}"#).unwrap();
        let mut manifest = Manifest {
            harness: "fixture".into(),
            revision: 1,
            vendor_version: "test-only".into(),
            entries: vec![Entry {
                path: "settings.json".into(),
                artifact: super::super::reconciliation::Artifact::JsonEntry {
                    pointer: "/hooks/mez".into(),
                    value: serde_json::json!({"fixed_argv":["mez","harness-event"]}),
                },
            }],
        };
        plan(&root, &manifest, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        let bytes = fs::read(root.join("settings.json")).unwrap();
        let repeated = plan(&root, &manifest, Operation::Install).unwrap();
        assert!(repeated.changed_paths().is_empty());
        repeated.apply().unwrap();
        assert_eq!(fs::read(root.join("settings.json")).unwrap(), bytes);
        let previous = manifest.clone();
        manifest.revision = 2;
        plan_with_history(&root, &manifest, Operation::Install, &[previous])
            .unwrap()
            .apply()
            .unwrap();
        plan(&root, &manifest, Operation::Uninstall)
            .unwrap()
            .apply()
            .unwrap();
        let result: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("settings.json")).unwrap()).unwrap();
        assert_eq!(result["user"], 7);
        assert!(result["hooks"].get("mez").is_none());
        assert!(
            plan(&root, &manifest, Operation::Uninstall)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
