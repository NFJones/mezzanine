//! Common owned-artifact reconciliation, independent of vendor certification.
//!
//! Adapters supply compiled best-effort manifests, never user-provided executable
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
    reason = "owned hook checkpoint exposes no standalone mutation interface"
)]
pub(crate) mod codex_artifact;
/// Retains exact compiled historical source bytes, not active vendor entries.
mod history;
#[allow(
    dead_code,
    reason = "release-qualified vendor adapters consume the common installer"
)]
pub(crate) mod installer;
#[allow(
    dead_code,
    reason = "pinned projection awaits launch-bound plugin qualification"
)]
pub(crate) mod opencode;
/// Owns compiled dependency-free OpenCode plugin files, not vendor packages.
pub(crate) mod opencode_artifact;
/// Strict private child observations for an exact parent-bound root session.
#[allow(
    dead_code,
    reason = "retained strict observations await ordinary-process enrollment"
)]
pub(crate) mod opencode_stream;
/// Owns private shared persistent-client and fixed helper-reference artifacts.
mod persistent_client;
#[allow(
    dead_code,
    reason = "pinned lifecycle projection awaits private Pi launch qualification"
)]
pub(crate) mod pi;
#[allow(
    dead_code,
    reason = "owned Pi artifacts await private-launch and assembled certification"
)]
pub(crate) mod pi_artifact;
/// Strict content-free child session proposals; credentials remain parent-owned.
#[allow(
    dead_code,
    reason = "retained protocol fixtures await ordinary-process enrollment"
)]
pub(crate) mod pi_binding;
#[allow(
    dead_code,
    reason = "Pi observer IPC awaits private launcher integration"
)]
pub(crate) mod pi_ipc;
/// Test-only descriptor qualification, not a public or hidden vendor launcher.
#[cfg(test)]
pub(crate) mod pi_launch;
#[allow(
    dead_code,
    reason = "Pi launch-owned sequencing awaits privately bound transport"
)]
pub(crate) mod pi_owner;
#[allow(
    dead_code,
    reason = "Pi renewal ownership awaits private launcher integration"
)]
pub(crate) mod pi_renewal;
#[allow(
    dead_code,
    reason = "Pi session worker awaits private launcher and observer IPC integration"
)]
pub(crate) mod pi_session;
#[allow(
    dead_code,
    reason = "Pi capability transport awaits private launcher integration"
)]
pub(crate) mod pi_transport;
#[allow(
    dead_code,
    reason = "release-qualified vendor adapters consume the common installer"
)]
mod publication;
#[cfg(test)]
mod publication_tests;
/// Rejects ambiguous shared JSON before exact array-member reconciliation.
use crate::protocol::strict_json;

#[allow(
    dead_code,
    reason = "release-qualified vendor adapters consume the common installer"
)]
pub(crate) mod reconciliation;

/// Selects compiled best-effort artifacts. Version text is observation metadata,
/// not an installation gate; artifact identity remains stable across versions.
pub(crate) fn compiled_manifest(
    harness: &str,
    _version: Option<&str>,
) -> Option<installer::Manifest> {
    match harness {
        "pi" => Some(pi_artifact::candidate_manifest()),
        "opencode" => Some(opencode_artifact::manifest()),
        "codex" => Some(codex_artifact::manifest()),
        _ => None,
    }
}

/// Recognizes exact shipped predecessors using independent frozen source bytes.
/// Receipts cannot supply authority; older byte variants/old-target recovery
/// remain installer work, and unknown fixed-helper references fail closed.
pub(crate) fn compiled_history(manifest: &installer::Manifest) -> Vec<installer::Manifest> {
    let (current, revision, paths) = match manifest.harness.as_str() {
        "pi" => (
            pi_artifact::candidate_manifest(),
            2,
            [
                "extensions/mezzanine/persistent_client.mjs",
                "extensions/mezzanine/peer_helper.mjs",
            ],
        ),
        "opencode" => (
            opencode_artifact::manifest(),
            1,
            [
                "plugins/mezzanine/persistent_client.mjs",
                "plugins/mezzanine/peer_helper.mjs",
            ],
        ),
        _ => return Vec::new(),
    };
    if *manifest != current {
        return Vec::new();
    }
    let mut shipped_client = current.clone();
    shipped_client.revision = if manifest.harness == "pi" { 5 } else { 4 };
    for entry in &mut shipped_client.entries {
        if entry.path.ends_with("/persistent_client.mjs") {
            entry.artifact = reconciliation::Artifact::File {
                bytes: history::persistent_client_v3(),
            };
        }
    }
    let mut previous = current;
    let mut last_shipped = previous.clone();
    last_shipped.revision = if manifest.harness == "pi" { 4 } else { 3 };
    last_shipped.entries.retain(|entry| {
        !["plugins/mezzanine/opencode_tui.mjs", "tui.json"].contains(&entry.path.as_str())
    });
    for entry in &mut last_shipped.entries {
        if entry.path.ends_with("/persistent_client.mjs") {
            entry.artifact = reconciliation::Artifact::File {
                bytes: history::persistent_client_v2(),
            };
        }
    }
    previous.revision = revision + 1;
    previous.entries.retain(|entry| {
        ![
            "extensions/mezzanine/pi_persistent.mjs",
            "plugins/mezzanine/opencode_tui.mjs",
            "tui.json",
        ]
        .contains(&entry.path.as_str())
    });
    for entry in &mut previous.entries {
        let frozen = if entry.path == "extensions/mezzanine/index.mjs" {
            Some(history::PI_ENTRY_V3)
        } else if entry.path.ends_with("/persistent_client.mjs") {
            Some(history::PERSISTENT_CLIENT_V1)
        } else {
            None
        };
        if let Some(bytes) = frozen {
            entry.artifact = reconciliation::Artifact::File {
                bytes: bytes.to_vec(),
            };
        }
    }
    let immediate = previous.clone();
    previous.revision = revision;
    previous
        .entries
        .retain(|entry| !paths.contains(&entry.path.as_str()));
    if compiled_predecessor_matches(&previous) {
        vec![shipped_client, last_shipped, immediate, previous]
    } else {
        Vec::new()
    }
}

/// Freezes exact historical ownership bytes; edits to current modules cannot
/// silently redefine a prior revision. A changed predecessor fails closed until
/// an explicit reviewed historical snapshot is supplied by its installer owner.
fn compiled_predecessor_matches(manifest: &installer::Manifest) -> bool {
    use sha2::Digest;
    let expected = match (manifest.harness.as_str(), manifest.revision) {
        ("pi", 2) => "daca64626b1d491199e1418607b7ca29ffad0b15de69811f3842a5c030916751",
        ("opencode", 1) => "36a754204d96c6287951d205011a02ca63bfe48b9c6dbffefdaa1fd4f34eea82",
        _ => return false,
    };
    let Ok(bytes) = serde_json::to_vec(manifest) else {
        return false;
    };
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        == expected
}
