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

/// One read-only inspected plan; publication acquires ownership and revalidates
/// all captured preimages, including unchanged artifacts and the receipt.
pub(crate) struct Plan {
    publisher: Publisher,
    changes: Vec<Change>,
    observed: Vec<(String, Option<Vec<u8>>)>,
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
    let publisher = Publisher::inspect(root)?;
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
    let mut observed = vec![(receipt_path.clone(), receipt_bytes.clone())];
    for path in paths {
        let before = publisher.read(&path)?;
        observed.push((path.clone(), before.clone()));
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
        observed,
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
    pub(crate) fn apply(mut self) -> Result<()> {
        if !self.changes.is_empty() {
            self.publisher.acquire_lock()?;
        }
        self.publisher.require_no_pending_journal()?;
        for (path, before) in &self.observed {
            if self.publisher.read(path)? != *before {
                return Err(MezError::conflict(
                    "bootstrap inspected preimage changed; no publication",
                ));
            }
        }
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
    recover_with_history(root, manifest, &super::compiled_history(manifest))
}

/// Previews accepted recovery with compiled authority and no root/state writes.
/// None means no pending journal; Some(empty) still means accepted pending work.
pub(crate) fn preview_recovery(root: &Path, manifest: &Manifest) -> Result<Option<Vec<String>>> {
    recover_with_history_mode(root, manifest, &super::compiled_history(manifest), true)
}

/// Authorizes recovery only for the selected compiled release and recognized
/// historical revisions. Journal paths/payloads never supply mutation authority.
pub(crate) fn recover_with_history(
    root: &Path,
    manifest: &Manifest,
    history: &[Manifest],
) -> Result<bool> {
    Ok(recover_with_history_mode(root, manifest, history, false)?.is_some())
}

/// Uses one authorization closure for read-only preview and locked recovery;
/// journal payloads never become new path/manifest authority in either mode.
fn recover_with_history_mode(
    root: &Path,
    manifest: &Manifest,
    history: &[Manifest],
    preview: bool,
) -> Result<Option<Vec<String>>> {
    validate_manifest(manifest)?;
    let publisher = if preview {
        Publisher::inspect(root)?
    } else {
        Publisher::open(root)?
    };
    let authorize = |value: &serde_json::Value, changes: &[Change]| {
        let intent: Intent = serde_json::from_value(value.clone())
            .map_err(|_| MezError::conflict("bootstrap journal intent invalid"))?;
        if intent.manifest.harness != manifest.harness
            || (intent.manifest != *manifest && !history.contains(&intent.manifest))
            || intent.previous.as_ref().is_some_and(|old| {
                old.harness != manifest.harness || (old != manifest && !history.contains(old))
            })
        {
            return Err(MezError::forbidden(
                "bootstrap recovery manifest/release mismatch",
            ));
        }
        // Compiled membership above is authority; the journal only identifies
        // which original immutable target must finish before current refresh.
        let target = &intent.manifest;
        validate_manifest(target)?;
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
            target
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
                    Some(receipt_bytes(target)?)
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
                Some(receipt_bytes(target)?)
            } else {
                None
            };
            if current != expected {
                return Err(MezError::conflict("recovery omitted ownership receipt"));
            }
        }
        Ok(())
    };
    if preview {
        Ok(publisher
            .inspect_recovery_authorized(authorize)?
            .map(|changes| changes.into_iter().map(|change| change.path).collect()))
    } else {
        Ok(publisher.recover_authorized(authorize)?.then(Vec::new))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// Every frozen target remains recoverable for its original install or
    /// uninstall, even when the caller selects the latest compiled adapter.
    /// After settlement, normal current reconciliation remains idempotent and
    /// unrelated authored siblings survive both historical operations.
    #[test]
    fn bootstrap_recovery_all_frozen_targets_keep_original_operation() {
        for harness in ["pi", "opencode"] {
            let current = super::super::compiled_manifest(harness, None).unwrap();
            let history = super::super::compiled_history(&current);
            assert_eq!(
                history.len(),
                5,
                "all frozen targets must remain qualified for {harness}"
            );
            for target in history {
                for operation in [Operation::Install, Operation::Uninstall] {
                    let root = std::env::temp_dir().join(format!(
                        "mez-all-pending-targets-{}",
                        crate::storage::token_usage::new_token_usage_event_id()
                    ));
                    fs::create_dir(&root).unwrap();
                    fs::write(root.join("authored"), b"preserved").unwrap();
                    if matches!(operation, Operation::Uninstall) {
                        plan(&root, &target, Operation::Install)
                            .unwrap()
                            .apply()
                            .unwrap();
                    }
                    let accepted = plan(&root, &target, operation).unwrap();
                    accepted.publisher.stop_after.set(Some(1));
                    assert!(accepted.apply().is_err());
                    let journal = fs::read(root.join(".mez-bootstrap-journal")).unwrap();
                    assert!(preview_recovery(&root, &current).unwrap().is_some());
                    assert_eq!(
                        fs::read(root.join(".mez-bootstrap-journal")).unwrap(),
                        journal
                    );
                    assert!(recover(&root, &current).unwrap());
                    let receipt_path = root.join(format!("mez-bootstrap-ownership-{harness}.json"));
                    if matches!(operation, Operation::Install) {
                        let receipt: Receipt =
                            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
                        assert_eq!(receipt.manifest, target);
                    } else {
                        assert!(!receipt_path.exists());
                    }
                    assert_eq!(fs::read(root.join("authored")).unwrap(), b"preserved");
                    plan(&root, &current, Operation::Install)
                        .unwrap()
                        .apply()
                        .unwrap();
                    assert!(
                        plan(&root, &current, Operation::Install)
                            .unwrap()
                            .changed_paths()
                            .is_empty()
                    );
                    assert!(!recover(&root, &current).unwrap());
                    fs::remove_dir_all(root).unwrap();
                }
            }
        }
    }

    /// A selected current adapter must recognize a genuinely accepted pending
    /// historical target, finish its original immutable bytes/receipt, then
    /// permit a separate current reconciliation. Substituting current bytes
    /// while interpreting the old journal would corrupt forward recovery.
    #[test]
    fn bootstrap_recovery_current_selection_finishes_historical_pending_target() {
        let current = super::super::compiled_manifest("pi", None).unwrap();
        let historical = super::super::compiled_history(&current).remove(0);
        let root = std::env::temp_dir().join(format!(
            "mez-historical-pending-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        fs::create_dir(&root).unwrap();
        let accepted = plan(&root, &historical, Operation::Install).unwrap();
        accepted.publisher.stop_after.set(Some(1));
        assert!(accepted.apply().is_err());
        let journal = fs::read(root.join(".mez-bootstrap-journal")).unwrap();
        let original_value: serde_json::Value = serde_json::from_slice(&journal).unwrap();
        for kind in ["harness", "revision", "artifact", "receipt"] {
            let mut forged = original_value.clone();
            match kind {
                "harness" => forged["intent"]["manifest"]["harness"] = "opencode".into(),
                "revision" => forged["intent"]["manifest"]["revision"] = 999.into(),
                "artifact" => {
                    forged["changes"][0]["after"] = serde_json::json!(b"forged artifact".to_vec())
                }
                "receipt" => {
                    let changes = forged["changes"].as_array_mut().unwrap();
                    let receipt = changes
                        .iter_mut()
                        .find(|change| change["path"] == "mez-bootstrap-ownership-pi.json")
                        .unwrap();
                    receipt["after"] = serde_json::json!(receipt_bytes(&current).unwrap());
                }
                _ => unreachable!(),
            }
            let bytes = serde_json::to_vec(&forged).unwrap();
            fs::write(root.join(".mez-bootstrap-journal"), &bytes).unwrap();
            assert!(preview_recovery(&root, &current).is_err());
            assert!(recover(&root, &current).is_err());
            assert_eq!(
                fs::read(root.join(".mez-bootstrap-journal")).unwrap(),
                bytes
            );
            assert!(!root.join("mez-bootstrap-ownership-pi.json").exists());
        }
        fs::write(root.join(".mez-bootstrap-journal"), &journal).unwrap();
        assert!(
            preview_recovery(&root, &current)
                .expect("known historical target remains recoverable")
                .is_some()
        );
        assert_eq!(
            fs::read(root.join(".mez-bootstrap-journal")).unwrap(),
            journal
        );
        assert!(recover(&root, &current).unwrap());
        let receipt: Receipt = serde_json::from_slice(
            &fs::read(root.join("mez-bootstrap-ownership-pi.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt.manifest, historical);
        plan(&root, &current, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        assert!(
            plan(&root, &current, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Public preview and recovery use the same frozen compiled predecessor
    /// history after a genuine interrupted upgrade. Neither a preview nor the
    /// later settlement requires a caller-supplied historical authority list.
    #[test]
    fn bootstrap_recovery_preview_and_apply_share_compiled_upgrade_history() {
        let current = super::super::compiled_manifest("pi", None).unwrap();
        let previous = super::super::compiled_history(&current).remove(0);
        let root = std::env::temp_dir().join(format!(
            "mez-preview-upgrade-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        fs::create_dir(&root).unwrap();
        plan(&root, &previous, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        let accepted = plan(&root, &current, Operation::Install).unwrap();
        assert!(!accepted.changed_paths().is_empty());
        accepted.publisher.stop_after.set(Some(1));
        assert!(accepted.apply().is_err());
        let journal = fs::read(root.join(".mez-bootstrap-journal")).unwrap();
        assert!(preview_recovery(&root, &current).unwrap().is_some());
        assert_eq!(
            fs::read(root.join(".mez-bootstrap-journal")).unwrap(),
            journal
        );
        assert!(recover(&root, &current).unwrap());
        assert!(
            plan(&root, &current, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        assert!(preview_recovery(&root, &current).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    /// Missing selected roots produce a complete read-only install preview,
    /// retaining a native ancestor/absence witness. Apply may create only that
    /// destination tree; noop uninstall stays absent and recovery cannot invent
    /// a new root merely to discover that no accepted journal exists.
    #[test]
    fn bootstrap_installer_missing_root_preview_materializes_only_on_install() {
        let parent = std::env::temp_dir().join(format!(
            "mez-missing-root-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        fs::create_dir(&parent).unwrap();
        let root = parent.join("config/vendor");
        let manifest = Manifest {
            harness: "fixture".into(),
            revision: 1,
            vendor_version: "test-only".into(),
            entries: vec![Entry {
                path: "nested/owned".into(),
                artifact: super::super::reconciliation::Artifact::File {
                    bytes: b"owned".to_vec(),
                },
            }],
        };
        let accepted = plan(&root, &manifest, Operation::Install)
            .expect("missing root must remain inspectable");
        assert_eq!(
            accepted.changed_paths(),
            ["nested/owned", "mez-bootstrap-ownership-fixture.json"]
        );
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
        let removed = plan(&root, &manifest, Operation::Uninstall).unwrap();
        assert!(removed.changed_paths().is_empty());
        removed.apply().unwrap();
        assert!(!parent.join("config").exists());
        assert!(recover(&root, &manifest).is_err());
        assert!(!parent.join("config").exists());
        accepted.apply().unwrap();
        assert_eq!(fs::read(root.join("nested/owned")).unwrap(), b"owned");
        assert!(root.join("mez-bootstrap-ownership-fixture.json").is_file());
        assert!(
            plan(&root, &manifest, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        fs::remove_dir_all(parent).unwrap();
    }

    /// Inspection must create no lock, journal, receipt, stage or parent and
    /// must not contend with another cooperating publisher. Publication alone
    /// acquires ownership; rejection leaves every planned artifact untouched.
    #[test]
    fn bootstrap_installer_planning_is_read_only_and_publication_owns_lock() {
        let root = std::env::temp_dir().join(format!(
            "mez-readonly-plan-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("authored.json"), b"{\"user\":true}\n").unwrap();
        let manifest = Manifest {
            harness: "fixture".into(),
            revision: 1,
            vendor_version: "test-only".into(),
            entries: vec![Entry {
                path: "nested/owned".into(),
                artifact: super::super::reconciliation::Artifact::File {
                    bytes: b"owned".to_vec(),
                },
            }],
        };
        let first = plan(&root, &manifest, Operation::Install).unwrap();
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            1,
            "inspection mutated vendor root"
        );
        let second = plan(&root, &manifest, Operation::Install).unwrap();
        let holder = Publisher::open(&root).unwrap();
        assert!(
            plan(&root, &manifest, Operation::Install).is_ok(),
            "inspection acquired a cooperating writer lock"
        );
        assert!(first.apply().is_err());
        assert!(!root.join("nested").exists());
        assert!(!root.join(".mez-bootstrap-journal").exists());
        assert!(!root.join("mez-bootstrap-ownership-fixture.json").exists());
        drop(holder);
        second.apply().unwrap();
        assert_eq!(fs::read(root.join("nested/owned")).unwrap(), b"owned");
        assert_eq!(
            fs::read(root.join("authored.json")).unwrap(),
            b"{\"user\":true}\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Apply revalidates all inspected bytes, even artifacts unchanged by an
    /// upgrade and no-op receipt plans. A newly pending journal or replaced root
    /// must reject before artifact publication, not retarget a held descriptor.
    #[test]
    fn bootstrap_installer_inspected_plan_revalidates_unchanged_noop_and_root() {
        let root = std::env::temp_dir().join(format!(
            "mez-plan-preimages-{}",
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
        plan(&root, &manifest, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        let noop = plan(&root, &manifest, Operation::Install).unwrap();
        assert!(noop.changed_paths().is_empty());
        fs::write(root.join("owned"), b"edited").unwrap();
        assert!(noop.apply().is_err());
        fs::write(root.join("owned"), b"owned").unwrap();
        let mut upgraded = manifest.clone();
        upgraded.revision = 2;
        let receipt = fs::read(root.join("mez-bootstrap-ownership-fixture.json")).unwrap();
        let upgrade = plan_with_history(
            &root,
            &upgraded,
            Operation::Install,
            std::slice::from_ref(&manifest),
        )
        .unwrap();
        fs::write(root.join("owned"), b"foreign").unwrap();
        assert!(upgrade.apply().is_err());
        assert_eq!(
            fs::read(root.join("mez-bootstrap-ownership-fixture.json")).unwrap(),
            receipt
        );
        assert!(!root.join(".mez-bootstrap-journal").exists());
        fs::write(root.join("owned"), b"owned").unwrap();
        let pending = plan(&root, &manifest, Operation::Install).unwrap();
        fs::write(root.join(".mez-bootstrap-journal"), b"pending").unwrap();
        assert!(pending.apply().is_err());
        assert_eq!(fs::read(root.join("owned")).unwrap(), b"owned");
        fs::remove_file(root.join(".mez-bootstrap-journal")).unwrap();
        let stale_root = plan(&root, &manifest, Operation::Install).unwrap();
        let moved = root.with_extension("moved");
        fs::rename(&root, &moved).unwrap();
        fs::create_dir(&root).unwrap();
        assert!(stale_root.apply().is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(moved).unwrap();
    }

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
        fs::remove_file(root.join(".mez-bootstrap-lock")).unwrap();
        let preview = preview_recovery(&root, &manifest).unwrap().unwrap();
        assert!(preview.contains(&"owned".to_string()));
        assert_eq!(fs::read(&journal_path).unwrap(), original);
        assert!(!root.join(".mez-bootstrap-lock").exists());
        let holder = Publisher::open(&root).unwrap();
        assert_eq!(
            preview_recovery(&root, &manifest).unwrap().unwrap(),
            preview
        );
        drop(holder);
        let mut wrong = manifest.clone();
        wrong.harness = "other".into();
        assert!(preview_recovery(&root, &wrong).is_err());
        assert!(recover(&root, &wrong).is_err());
        wrong = manifest.clone();
        wrong.vendor_version = "different-release".into();
        assert!(preview_recovery(&root, &wrong).is_err());
        assert!(recover(&root, &wrong).is_err());
        fs::write(copy.join(".mez-bootstrap-journal"), &original).unwrap();
        fs::write(copy.join("owned"), b"owned").unwrap();
        assert!(preview_recovery(&copy, &manifest).is_err());
        assert!(recover(&copy, &manifest).is_err());
        let mut wrong_version: serde_json::Value = serde_json::from_slice(&original).unwrap();
        wrong_version["version"] = 999.into();
        fs::write(&journal_path, serde_json::to_vec(&wrong_version).unwrap()).unwrap();
        assert!(preview_recovery(&root, &manifest).is_err());
        fs::write(&journal_path, &original).unwrap();
        fs::write(root.join("owned"), b"foreign preimage").unwrap();
        assert!(preview_recovery(&root, &manifest).is_err());
        assert_eq!(fs::read(root.join("owned")).unwrap(), b"foreign preimage");
        assert_eq!(fs::read(&journal_path).unwrap(), original);
        fs::write(root.join("owned"), b"owned").unwrap();
        let mut forged: serde_json::Value = serde_json::from_slice(&original).unwrap();
        forged["changes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "path":"unrelated", "before":b"authored".to_vec(), "after":null,
            }));
        fs::write(root.join("unrelated"), b"authored").unwrap();
        fs::write(&journal_path, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert!(preview_recovery(&root, &manifest).is_err());
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
