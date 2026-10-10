//! Compiled exact private-bundle recognition without synthesizing receipts.
//!
//! Only complete Claude/Pi/OpenCode all-file bundles at enumerated private destinations
//! qualify. Candidate manifests are supplied by the compiled owner, not observed
//! version text, journals or client JSON. No directory enumeration or substring
//! recognition occurs. Recovery re-proves every original compiled byte preimage;
//! adoption cannot preserve arbitrary edits or grant shared-document ownership.

use super::installer::Manifest;
use super::publication::Change;
use super::reconciliation::{Artifact, publication_path};
use crate::error::{MezError, Result};

/// Bounds the private-only candidate shape independently of byte recognition.
/// Shared package/config/hook registrations and unsupported harnesses never qualify.
pub(super) fn private_bundle(manifest: &Manifest) -> bool {
    !manifest.entries.is_empty()
        && manifest.entries.len() <= 15
        && manifest.entries.iter().all(|entry| {
            let private = match manifest.harness.as_str() {
                "claude" => entry.path.starts_with("skills/mezzanine-telemetry/"),
                "pi" => entry.path.starts_with("extensions/mezzanine/"),
                "opencode" => {
                    entry.path == "plugins/mezzanine.js"
                        || entry.path.starts_with("plugins/mezzanine/")
                }
                _ => false,
            };
            private
                && publication_path(&entry.path).is_ok()
                && matches!(&entry.artifact, Artifact::File { bytes } if bytes.len() <= 1024 * 1024)
        })
}

/// Chooses the most complete exact compiled match, preferring current identity on
/// equal size. A matching old subset never authorizes additional surrounding files;
/// ordinary reconciliation still rejects any populated unowned new destination.
pub(super) fn recognize(
    manifest: &Manifest,
    history: &[Manifest],
    mut read: impl FnMut(&str) -> Result<Option<Vec<u8>>>,
) -> Result<Option<Manifest>> {
    let mut selected: Option<Manifest> = None;
    for candidate in std::iter::once(manifest).chain(history) {
        if candidate.harness != manifest.harness
            || !private_bundle(candidate)
            || selected
                .as_ref()
                .is_some_and(|selected| selected.entries.len() >= candidate.entries.len())
        {
            continue;
        }
        let mut complete = true;
        for entry in &candidate.entries {
            let Artifact::File { bytes } = &entry.artifact else {
                return Err(MezError::invalid_state(
                    "compiled private bundle kind unavailable",
                ));
            };
            if read(&entry.path)?.as_deref() != Some(bytes.as_slice()) {
                complete = false;
                break;
            }
        }
        if complete {
            selected = Some(candidate.clone());
        }
    }
    Ok(selected)
}

/// Re-proves entire adopted authority from original change preimages or unchanged
/// held-root bytes. Missing/edited/omitted/forged bundle evidence refuses recovery;
/// desired after bytes, a receipt or an archive cannot substitute this proof.
pub(super) fn prove_preimages(
    manifest: &Manifest,
    changes: &[Change],
    mut read: impl FnMut(&str) -> Result<Option<Vec<u8>>>,
) -> Result<()> {
    if !private_bundle(manifest) {
        return Err(MezError::forbidden(
            "bootstrap adoption requires a compiled private bundle",
        ));
    }
    for entry in &manifest.entries {
        let Artifact::File { bytes } = &entry.artifact else {
            return Err(MezError::forbidden(
                "bootstrap adoption artifact kind unavailable",
            ));
        };
        let before = if let Some(change) = changes.iter().find(|change| change.path == entry.path) {
            change.before.clone()
        } else {
            read(&entry.path)?
        };
        if before.as_deref() != Some(bytes.as_slice()) {
            return Err(MezError::forbidden(
                "bootstrap adoption compiled preimage unavailable",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::reconciliation::Entry;
    use super::*;
    use std::collections::BTreeMap;

    /// Builds narrow private fixture manifests without production registry input.
    fn fixture_manifest(paths: &[&str]) -> Manifest {
        Manifest {
            harness: "pi".into(),
            revision: 1,
            vendor_version: "fixture".into(),
            entries: paths
                .iter()
                .map(|path| Entry {
                    path: (*path).into(),
                    artifact: Artifact::File {
                        bytes: b"compiled".to_vec(),
                    },
                })
                .collect(),
        }
    }

    /// Complete recognition chooses the most-complete same-harness candidate and
    /// never authorizes additional slots. Empty/oversized/traversal/shared shapes,
    /// mismatched harnesses and non-file artifacts remain unadoptable; edited or
    /// missing preimages cannot be substituted by desired after bytes in recovery.
    #[test]
    fn bootstrap_private_bundle_shape_selection_and_preimage_proof_are_bounded() {
        let short = fixture_manifest(&["extensions/mezzanine/a"]);
        let complete = fixture_manifest(&["extensions/mezzanine/a", "extensions/mezzanine/b"]);
        let data = BTreeMap::from([
            ("extensions/mezzanine/a", b"compiled".to_vec()),
            ("extensions/mezzanine/b", b"compiled".to_vec()),
        ]);
        let read = |path: &str| Ok(data.get(path).cloned());
        assert_eq!(
            recognize(&short, std::slice::from_ref(&complete), read).unwrap(),
            Some(complete.clone())
        );
        assert!(recognize(&complete, &[], |_| Ok(None)).unwrap().is_none());
        let mut foreign = complete.clone();
        foreign.harness = "opencode".into();
        assert_eq!(
            recognize(&short, &[foreign], read).unwrap(),
            Some(short.clone())
        );
        for invalid in [
            fixture_manifest(&[]),
            fixture_manifest(&["package.json"]),
            fixture_manifest(&["extensions/mezzanine/../foreign"]),
        ] {
            assert!(!private_bundle(&invalid));
        }
        let mut shared = complete.clone();
        shared.entries[0].artifact = Artifact::JsonEntry {
            pointer: "/owned".into(),
            value: serde_json::json!(true),
        };
        assert!(!private_bundle(&shared));
        let mut oversized = complete.clone();
        oversized.entries[0].artifact = Artifact::File {
            bytes: vec![0; 1024 * 1024 + 1],
        };
        assert!(!private_bundle(&oversized));
        prove_preimages(&complete, &[], read).unwrap();
        assert!(prove_preimages(&complete, &[], |_| Ok(Some(b"edited".to_vec()))).is_err());
        let fake = Change {
            path: "extensions/mezzanine/a".into(),
            before: None,
            after: Some(b"compiled".to_vec()),
        };
        assert!(prove_preimages(&complete, &[fake], read).is_err());
        let exact = Change {
            path: "extensions/mezzanine/a".into(),
            before: Some(b"compiled".to_vec()),
            after: Some(b"new".to_vec()),
        };
        prove_preimages(&complete, &[exact], read).unwrap();
    }
}
