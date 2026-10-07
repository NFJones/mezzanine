//! Compiled owned best-effort Pi extension, independent of vendor version.
//!
//! The released 1.0.2 loader discovers one package entry, with private sibling
//! modules that are not standalone extension candidates. Exact file ownership
//! reuses the common installer; no settings, vendor dependency, shell profile,
//! trust decision or capability is installed. Current descriptor activation is
//! legacy fixture plumbing; ordinary-process enrollment migration is unfinished.

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
        (
            "pi_observer_stream.mjs",
            include_bytes!("pi_observer_stream.mjs"),
        ),
    ];
    Manifest {
        harness: "pi".into(),
        revision: 2,
        vendor_version: super::pi::RELEASE.into(),
        entries: files
            .iter()
            .map(|(name, bytes)| Entry {
                path: format!("extensions/mezzanine/{name}"),
                artifact: Artifact::File {
                    bytes: bytes.to_vec(),
                },
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests;
