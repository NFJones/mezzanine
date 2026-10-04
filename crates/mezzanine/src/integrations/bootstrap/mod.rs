//! Common owned-artifact reconciliation, independent of vendor certification.
//!
//! Adapters supply release-qualified manifests, never user-provided executable
//! templates. JSON ownership is one exact entry, not its surrounding document;
//! whole-file ownership requires exact equality. Planning is pure and publication
//! uses held no-follow directory descriptors, preimage checks and a private journal.
//! No vendor binary, credential, hook trust or daemon authority is installed here.
//! An external writer can still race final check and rename: this is not atomic CAS.

#[allow(
    dead_code,
    reason = "pinned projection awaits launch-bound vendor qualification"
)]
pub(crate) mod codex;
#[allow(
    dead_code,
    reason = "release-qualified vendor adapters consume the common installer"
)]
pub(crate) mod installer;
#[allow(
    dead_code,
    reason = "release-qualified vendor adapters consume the common installer"
)]
mod publication;
#[cfg(test)]
mod publication_tests;

#[allow(
    dead_code,
    reason = "release-qualified vendor adapters consume the common installer"
)]
pub(crate) mod reconciliation;

/// Selects a compiled, release-qualified manifest. Candidate research is not a
/// manifest; vendor adapter tasks must add certified releases at this boundary.
pub(crate) fn certified_manifest(
    _harness: &str,
    _version: Option<&str>,
) -> Option<installer::Manifest> {
    None
}
