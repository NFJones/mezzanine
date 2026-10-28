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
        revision: 18,
        vendor_version: super::pi::RELEASE.into(),
        entries,
    }
}

/// Exact revision17 before metadata-only auxiliary attribution disclosure.
/// Frozen producer bytes preserve ownership of every previous shipped layout.
pub(super) fn historical_auxiliary_manifest() -> Manifest {
    let mut manifest = candidate_manifest();
    manifest.revision = 17;
    for entry in &mut manifest.entries {
        let bytes = match entry.path.as_str() {
            "extensions/mezzanine/pi_usage.mjs" => {
                Some(include_bytes!("history/pi_usage_v17.mjs").as_slice())
            }
            "extensions/mezzanine/pi_persistent.mjs" => {
                Some(include_bytes!("history/pi_persistent_v17.mjs").as_slice())
            }
            _ => None,
        };
        if let Some(bytes) = bytes {
            entry.artifact = Artifact::File {
                bytes: bytes.to_vec(),
            };
        }
    }
    manifest
}

/// Exact revision-16 cold-source client before bounded settled receipts.
/// The current transport/producer bytes remain unchanged; accounting is frozen.
pub(super) fn historical_receipt_manifest() -> Manifest {
    let mut manifest = historical_auxiliary_manifest();
    manifest.revision = 16;
    super::history::freeze_pre_receipt_bound(&mut manifest);
    manifest
}

/// Exact revision15 before explicit automatic native cold recovery.
pub(super) fn historical_bounded_end_manifest() -> Manifest {
    let mut manifest = historical_auxiliary_manifest();
    manifest.revision = 15;
    super::history::freeze_bounded_end_transport(&mut manifest);
    manifest
}

/// Exact revision-14 layout before bounded lost-end-ACK recovery.
pub(super) fn historical_one_shot_end_manifest() -> Manifest {
    let mut manifest = historical_auxiliary_manifest();
    manifest.revision = 14;
    super::history::freeze_one_shot_end_transport(&mut manifest);
    manifest
}

/// Exact revision-13 layout before required source-end acknowledgments.
pub(super) fn historical_end_manifest() -> Manifest {
    let mut manifest = historical_auxiliary_manifest();
    manifest.revision = 13;
    super::history::freeze_pre_end_transport(&mut manifest);
    manifest
}

/// Exact revision-12 layout before finite pending-report recovery. Both changed
/// Pi producer slots come from independent pre-edit snapshots; unchanged shared
/// transport retains its original compiled bytes, not receipt-derived authority.
pub(super) fn historical_resume_manifest() -> Manifest {
    let mut manifest = historical_end_manifest();
    manifest.revision = 12;
    for entry in &mut manifest.entries {
        let bytes = match entry.path.as_str() {
            "extensions/mezzanine/pi_usage.mjs" => {
                Some(include_bytes!("history/pi_usage_v12.mjs").as_slice())
            }
            "extensions/mezzanine/pi_persistent.mjs" => {
                Some(include_bytes!("history/pi_persistent_v12.mjs").as_slice())
            }
            _ => None,
        };
        if let Some(bytes) = bytes {
            entry.artifact = Artifact::File {
                bytes: bytes.to_vec(),
            };
        }
    }
    manifest
}

/// Exact revision-11 layout before shared-client resume support. Pi-owned bytes
/// are identical to independently frozen revision12, never active revision13.
pub(super) fn historical_loss_manifest() -> Manifest {
    let mut manifest = historical_resume_manifest();
    manifest.revision = 11;
    super::history::freeze_pre_resume_transport(&mut manifest);
    manifest
}

/// Exact revision-10 changed slots, independent of current loss-ordering code.
/// Shared transport/request bytes are unchanged by revision 11.
pub(super) fn historical_usage_manifest() -> Manifest {
    let mut manifest = historical_auxiliary_manifest();
    manifest.revision = 10;
    for entry in &mut manifest.entries {
        let bytes = match entry.path.as_str() {
            "extensions/mezzanine/pi_persistent.mjs" => {
                Some(include_bytes!("history/pi_persistent_v10.mjs").as_slice())
            }
            "extensions/mezzanine/pi_usage.mjs" => {
                Some(include_bytes!("history/pi_usage_v10.mjs").as_slice())
            }
            _ => None,
        };
        if let Some(bytes) = bytes {
            entry.artifact = Artifact::File {
                bytes: bytes.to_vec(),
            };
        }
    }
    super::history::freeze_pre_resume_transport(&mut manifest);
    manifest
}

/// Reconstructs the exact shipped revision-9 layout, with changed Pi bytes
/// frozen independently of the active producer. Other slots are unchanged by
/// revision 10; fixed helper references retain common native installer policy.
pub(super) fn historical_manifest() -> Manifest {
    let mut manifest = historical_auxiliary_manifest();
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
