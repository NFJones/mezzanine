//! Common owned-artifact reconciliation, independent of vendor certification.
//!
//! Adapters supply compiled best-effort manifests, never user-provided executable
//! templates. JSON ownership is one exact entry, not its surrounding document;
//! whole-file ownership requires exact equality. Inspection is read-only and publication
//! uses held no-follow directory descriptors, preimage checks and a private journal.
//! No vendor binary, credential, hook trust or daemon authority is installed here.
//! An external writer can still race final check and rename: this is not atomic CAS.

/// Exact private-bundle recognition, never user-document or filename ownership.
mod adoption;
/// Owns the documented ordinary Claude user-root mod bundle, not vendor settings.
pub(crate) mod claude_artifact;
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
/// Owns documented Copilot user hook bytes, separate from runtime association.
mod copilot_artifact;
/// Literal no-Node original-epoch observer source; installation is adapter-owned.
#[allow(dead_code, reason = "curated adapter installation is a separate owner")]
pub(crate) mod curated_client;
/// Owns exact Cursor shared hook members, not authored document/settings fields.
mod cursor_artifact;
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
/// Owns bounded receipt-proven generated-helper preservation, not shared config.
mod preservation;
#[allow(
    dead_code,
    reason = "release-qualified vendor adapters consume the common installer"
)]
mod publication;
#[cfg(test)]
mod publication_tests;

/// Holds exact root/ancestor absence witnesses for read-only native planning.
mod root_directory;
/// Selects documented vendor user roots without I/O or authority inference.
pub(crate) mod roots;
/// Private native storage used by every compiled bootstrap CLI intent.
#[allow(
    dead_code,
    reason = "private publisher journal/archive migration is in progress"
)]
mod state_directory;
/// Rejects ambiguous shared JSON before exact array-member reconciliation.
use crate::protocol::strict_json;

/// Bounded JSONC token editing, never comment-stripping publication.
mod jsonc;
#[allow(
    dead_code,
    reason = "release-qualified vendor adapters consume the common installer"
)]
pub(crate) mod reconciliation;
/// Bounded multi-array planning reuses exact-member ownership, not whole files.
mod shared_arrays;

/// Selects compiled best-effort artifacts. Version text is observation metadata,
/// not an installation gate; artifact identity remains stable across versions.
pub(crate) fn compiled_manifest(
    harness: &str,
    _version: Option<&str>,
) -> Option<installer::Manifest> {
    match harness {
        "claude" => claude_artifact::manifest(),
        "pi" => Some(pi_artifact::candidate_manifest()),
        "opencode" => Some(opencode_artifact::manifest()),
        "codex" => codex_artifact::manifest(),
        "copilot" => copilot_artifact::manifest(),
        "cursor" => cursor_artifact::manifest(),
        _ => None,
    }
}

/// Recognizes exact shipped predecessors using independent frozen source bytes.
/// Receipts cannot supply authority; explicit or automatic recovery may finish
/// these exact historical targets, while unknown fixed-helper references fail
/// closed. The installer owns read-only overlays and locked reconciliation;
/// this directory supplies immutable artifact authority only.
pub(crate) fn compiled_history(manifest: &installer::Manifest) -> Vec<installer::Manifest> {
    if manifest.harness == "claude" {
        return if Some(manifest) == claude_artifact::manifest().as_ref() {
            [
                claude_artifact::historical_lifecycle_manifest(),
                claude_artifact::historical_manifest(),
            ]
            .into_iter()
            .flatten()
            .collect()
        } else {
            Vec::new()
        };
    }
    if Some(manifest) == codex_artifact::manifest().as_ref() {
        let mut history = codex_artifact::historical_fixed_manifest()
            .into_iter()
            .collect::<Vec<_>>();
        history.extend([
            codex_artifact::historical_shared_manifest(),
            codex_artifact::historical_manifest(),
        ]);
        return history;
    }
    let (mut current, revision, paths) = match manifest.harness.as_str() {
        "pi" => (
            pi_artifact::candidate_manifest(),
            2,
            [
                "extensions/mezzanine/persistent_client.mjs",
                "extensions/mezzanine/peer_helper.mjs",
            ],
        ),
        "opencode" => (
            opencode_artifact::historical_manifest(),
            1,
            [
                "plugins/mezzanine/persistent_client.mjs",
                "plugins/mezzanine/peer_helper.mjs",
            ],
        ),
        _ => return Vec::new(),
    };
    let current_opencode = compiled_variant_matches(manifest, &opencode_artifact::manifest());
    if manifest.harness == "opencode"
        && !opencode_artifact::historical_payloads_match(
            include_bytes!("opencode_tui.mjs"),
            &history::persistent_client_v5(),
        )
    {
        return Vec::new();
    }
    if *manifest != current && !current_opencode {
        return Vec::new();
    }
    let revision8_client = if manifest.harness == "pi" || current_opencode {
        let mut snapshot = manifest.clone();
        snapshot.revision = 8;
        history::freeze_revision8_request_client(&mut snapshot);
        Some(snapshot)
    } else {
        None
    };
    let last_current = if manifest.harness == "pi" || current_opencode {
        let mut snapshot = manifest.clone();
        snapshot.revision = 7;
        history::freeze_pre_request_client(&mut snapshot);
        Some(snapshot)
    } else {
        None
    };
    history::freeze_pre_request_client(&mut current);
    let mut latest_client = current.clone();
    latest_client.revision = if manifest.harness == "pi" { 6 } else { 5 };
    for entry in &mut latest_client.entries {
        if entry.path.ends_with("/persistent_client.mjs") {
            entry.artifact = reconciliation::Artifact::File {
                bytes: history::persistent_client_v4(),
            };
        }
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
        let mut history = vec![
            latest_client,
            shipped_client,
            last_shipped,
            immediate,
            previous,
        ];
        if let Some(last_current) = last_current {
            history.insert(0, last_current);
        }
        if current_opencode {
            history.insert(1, opencode_artifact::historical_manifest());
        }
        if let Some(revision8_client) = revision8_client {
            history.insert(0, revision8_client);
        }
        if current_opencode {
            // The newly effective loader layout cannot redefine which shipped
            // predecessors are recognized. Only the four compiled JSONC-era
            // layouts are eligible; receipt-selected paths/pointers and former
            // helper references still supply no authority.
            let mut alternatives = Vec::new();
            for layout in [
                opencode_artifact::manifest(),
                opencode_artifact::jsonc_manifest(),
                opencode_artifact::nested_manifest(false),
                opencode_artifact::nested_manifest(true),
            ] {
                for revision in [7, 8] {
                    let mut snapshot = layout.clone();
                    snapshot.revision = revision;
                    if revision == 8 {
                        history::freeze_revision8_request_client(&mut snapshot);
                    } else {
                        history::freeze_pre_request_client(&mut snapshot);
                    }
                    if !history.contains(&snapshot) {
                        alternatives.push(snapshot);
                    }
                }
            }
            history.splice(2..2, alternatives);
        }
        history
    } else {
        Vec::new()
    }
}

/// Qualifies four fixed current OpenCode filename/effective-pointer variants.
/// No path supplied by receipts, journals or environment creates another variant.
pub(super) fn compiled_variant_matches(
    candidate: &installer::Manifest,
    target: &installer::Manifest,
) -> bool {
    if candidate.harness != "opencode" || target.harness != "opencode" {
        return false;
    }
    let variants = [
        opencode_artifact::manifest(),
        opencode_artifact::jsonc_manifest(),
        opencode_artifact::nested_manifest(false),
        opencode_artifact::nested_manifest(true),
    ];
    variants.contains(candidate) && variants.contains(target)
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
