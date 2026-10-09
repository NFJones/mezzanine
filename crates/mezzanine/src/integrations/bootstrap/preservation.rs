//! Narrow generated-file preservation, independent of vendor shared documents.
//!
//! Only compiled receipt-owned Pi/OpenCode helper destinations and the private Pi
//! loader package may preserve edited whole-file preimages. Current private
//! transactions also deduplicate exact receipt-owned shared array registrations.
//! Shared package metadata, hooks/config and unowned slots otherwise retain strict
//! reconciliation. Archive keys are deterministic bounded identities of harness,
//! relative destination and exact preimage; they never supply filesystem authority.

use super::reconciliation::{Artifact, publication_path, reconcile, reconcile_array};
use crate::error::{MezError, Result};
use sha2::{Digest, Sha256};

/// Recomputes current private repairs and required archives for edited artifacts.
/// Caller qualifies compiled receipts and checks vendor/archive observations.
/// Journal versions admit only their frozen private semantics; new Codex shared
/// migration requires v5. No version grants authority outside compiled receipts.
pub(super) fn reconcile_preserving(
    harness: &str,
    path: &str,
    current: Option<&[u8]>,
    previous: Option<&Artifact>,
    desired: Option<&Artifact>,
    journal_version: Option<u32>,
) -> Result<(Option<Vec<u8>>, Option<String>)> {
    let enabled = journal_version.is_some_and(|version| version >= 4);
    if journal_version.is_some_and(|version| version >= 5) {
        if harness == "codex"
            && path == "hooks.json"
            && matches!(desired, Some(Artifact::JsonArrayEntries { .. }) | None)
            && let Some(previous) = previous
            && let Some(shared) = super::codex_artifact::historical_shared_ownership(previous)?
        {
            return Ok((
                super::shared_arrays::reconcile_arrays(current, Some(&shared), desired, true)?,
                None,
            ));
        }
        if matches!(previous, Some(Artifact::JsonArrayEntries { .. })) {
            return Ok((
                super::shared_arrays::reconcile_arrays(current, previous, desired, true)?,
                None,
            ));
        }
    }
    if enabled && let Some(Artifact::JsonArrayEntry { pointer, .. }) = previous {
        return Ok((
            reconcile_array(current, previous, desired, pointer, true)?,
            None,
        ));
    }
    let generated = match harness {
        "pi" => {
            path == "extensions/mezzanine/package.json"
                || (path.starts_with("extensions/mezzanine/") && path.ends_with(".mjs"))
        }
        "opencode" => {
            path == "plugins/mezzanine.js"
                || (path.starts_with("plugins/mezzanine/") && path.ends_with(".mjs"))
        }
        _ => false,
    };
    if enabled
        && generated
        && let (Some(before), Some(Artifact::File { bytes })) = (current, previous)
        && before != bytes.as_slice()
    {
        if before.len() > 1024 * 1024 {
            return Err(MezError::invalid_args(
                "bootstrap preservation preimage exceeds limit",
            ));
        }
        let after = match desired {
            Some(Artifact::File { bytes }) if bytes.len() <= 1024 * 1024 => Some(bytes.clone()),
            None => None,
            _ => {
                return Err(MezError::conflict(
                    "bootstrap preserved artifact ownership kind changed",
                ));
            }
        };
        let archive = if after.as_deref() != Some(before) {
            Some(archive_path(harness, path, before)?)
        } else {
            None
        };
        return Ok((after, archive));
    }
    Ok((reconcile(current, previous, desired)?, None))
}

/// Derives a domain-separated content identity, not an arbitrary private path.
pub(super) fn archive_path(harness: &str, path: &str, bytes: &[u8]) -> Result<String> {
    publication_path(path)?;
    let mut hash = Sha256::new();
    hash.update(b"mez-bootstrap-generated-preservation-v1\0");
    for part in [harness.as_bytes(), path.as_bytes(), bytes] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    let digest = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("@mez-bootstrap-archive/{harness}/{digest}"))
}
