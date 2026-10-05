//! Compiled owned Pi extension candidate, independent of certification.
//!
//! The released 1.0.2 loader discovers one package entry, with private sibling
//! modules that are not standalone extension candidates. Exact file ownership
//! reuses the common installer; no settings, vendor dependency, shell profile,
//! trust decision or capability is installed. The public certified registry must
//! remain disabled until private launch/reload and assembled acceptance pass.

use super::installer::Manifest;
use super::reconciliation::{Artifact, Entry};

/// Returns only compiled artifact bytes for an explicit agent-directory root.
/// This candidate is not consulted by `certified_manifest` or public bootstrap.
pub(crate) fn candidate_manifest() -> Manifest {
    let files: &[(&str, &[u8])] = &[
        (
            "package.json",
            b"{\"private\":true,\"type\":\"module\",\"pi\":{\"extensions\":[\"index.mjs\"]}}\n",
        ),
        ("index.mjs", include_bytes!("pi_entry.mjs")),
        ("pi_extension.mjs", include_bytes!("pi_extension.mjs")),
        ("pi_observer.mjs", include_bytes!("pi_observer.mjs")),
        (
            "pi_observer_stream.mjs",
            include_bytes!("pi_observer_stream.mjs"),
        ),
    ];
    Manifest {
        harness: "pi".into(),
        revision: 1,
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
