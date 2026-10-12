//! Compiled owned best-effort Pi extension, independent of vendor version.
//!
//! The released 1.0.2 loader discovers one package entry, with private sibling
//! modules that are not standalone extension candidates. Exact file ownership
//! reuses the common installer; no settings, vendor dependency, shell profile,
//! trust decision or capability is installed. The ordinary default entry uses
//! genuine callback context plus the native-qualified shared client; it needs no
//! vendor observer descriptor or launcher markers. Finalized assistant requests
//! use the private checked projector and shared durable request client.

use super::installer::Manifest;
use super::reconciliation::{Artifact, Entry};

/// Returns only compiled artifact bytes for an explicit agent-directory root.
/// Public bootstrap consults this manifest without a vendor-version gate.
pub(crate) fn candidate_manifest() -> Manifest {
    let files: &[(&str, &[u8])] = &[
        (
            "package.json",
            b"{\"private\":true,\"type\":\"module\",\"pi\":{\"extensions\":[\"index.mjs\"]}}\n",
        ),
        ("index.mjs", include_bytes!("pi_entry.mjs")),
        ("pi_extension.mjs", include_bytes!("pi_extension.mjs")),
        ("pi_binding.mjs", include_bytes!("pi_binding.mjs")),
        ("pi_observer.mjs", include_bytes!("pi_observer.mjs")),
        ("pi_persistent.mjs", include_bytes!("pi_persistent.mjs")),
        ("pi_usage.mjs", include_bytes!("pi_usage.mjs")),
        (
            "pi_observer_stream.mjs",
            include_bytes!("pi_observer_stream.mjs"),
        ),
    ];
    let mut entries = files
        .iter()
        .map(|(name, bytes)| Entry {
            path: format!("extensions/mezzanine/{name}"),
            artifact: Artifact::File {
                bytes: bytes.to_vec(),
            },
        })
        .collect::<Vec<_>>();
    entries.extend(super::persistent_client::entries("extensions/mezzanine"));
    Manifest {
        harness: "pi".into(),
        revision: 10,
        vendor_version: super::pi::RELEASE.into(),
        entries,
    }
}

/// Reconstructs the exact shipped revision-9 layout, with changed Pi bytes
/// frozen independently of the active producer. Other slots are unchanged by
/// revision 10; fixed helper references retain common native installer policy.
pub(super) fn historical_manifest() -> Manifest {
    let mut manifest = candidate_manifest();
    manifest.revision = 9;
    manifest
        .entries
        .retain(|entry| !entry.path.ends_with("/pi_usage.mjs"));
    for entry in &mut manifest.entries {
        if entry.path.ends_with("/pi_persistent.mjs") {
            entry.artifact = Artifact::File {
                bytes: include_bytes!("history/pi_persistent_v9.mjs").to_vec(),
            };
        }
    }
    super::history::freeze_revision9_transport(&mut manifest);
    manifest
}

#[cfg(test)]
mod tests;
