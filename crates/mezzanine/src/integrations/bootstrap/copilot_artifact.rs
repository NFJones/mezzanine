//! Owned classic Copilot hooks, extension attachment and concrete idle proof.
//!
//! Official hooks-reference/use-hooks document version1 JSON in COPILOT_HOME/hooks
//! and CLI-only exec/args. A fixed installing executable receives one known hook
//! name; no shell, permission decision, trust, exporter or credential is installed.
//! The bundled-SDK extension attaches only to an existing native-qualified run;
//! Startup attachment retries only within a finite window; neither loading policy
//! nor accounting is inferred by waiting for the original classic enrollment.
//! Live per-call metadata is independently bounded and explicitly unattributed.

use super::{
    installer::Manifest,
    reconciliation::{Artifact, Entry},
};

/// Compiles documented user hooks and the bounded native observer entry.
/// Unrepresentable installing evidence cannot guess PATH or another user root.
pub(super) fn manifest() -> Option<Manifest> {
    extension_manifest(
        6,
        include_bytes!("copilot_entry.mjs"),
        include_bytes!("copilot_extension.mjs"),
        Some(include_bytes!("copilot_lifetime.mjs")),
        Some(include_bytes!("copilot_usage.mjs")),
    )
}

/// Independently frozen revision5 graph before live terminal metadata delivery.
pub(super) fn historical_startup_manifest() -> Option<Manifest> {
    extension_manifest(
        5,
        include_bytes!("history/copilot_entry_v5.mjs"),
        include_bytes!("history/copilot_extension_v5.mjs"),
        Some(include_bytes!("history/copilot_lifetime_v5.mjs")),
        None,
    )
}

/// Independently frozen revision4 graph before bounded startup ordering.
pub(super) fn historical_idle_manifest() -> Option<Manifest> {
    extension_manifest(
        4,
        include_bytes!("history/copilot_entry_v4.mjs"),
        include_bytes!("history/copilot_extension_v4.mjs"),
        Some(include_bytes!("history/copilot_lifetime_v4.mjs")),
        None,
    )
}

/// Independently frozen revision3 graph before concrete idle source proof.
pub(super) fn historical_attachment_manifest() -> Option<Manifest> {
    extension_manifest(
        3,
        include_bytes!("history/copilot_entry_v3.mjs"),
        include_bytes!("history/copilot_extension_v3.mjs"),
        None,
        None,
    )
}

/// Compiles only enumerated fixed graph slots; historical bytes never depend on
/// changed active source. The native installing executable supplies peer checks.
fn extension_manifest(
    revision: u32,
    entry: &[u8],
    client: &[u8],
    lifetime: Option<&[u8]>,
    usage: Option<&[u8]>,
) -> Option<Manifest> {
    let mut manifest = historical_lifecycle_manifest()?;
    manifest.revision = revision;
    for (path, bytes) in [
        ("extensions/mezzanine/extension.mjs", entry),
        ("extensions/mezzanine/copilot_extension.mjs", client),
    ] {
        manifest.entries.push(Entry {
            path: path.into(),
            artifact: Artifact::File {
                bytes: bytes.to_vec(),
            },
        });
    }
    if let Some(bytes) = lifetime {
        manifest.entries.push(Entry {
            path: "extensions/mezzanine/copilot_lifetime.mjs".into(),
            artifact: Artifact::File {
                bytes: bytes.to_vec(),
            },
        });
    }
    if let Some(bytes) = usage {
        manifest.entries.push(Entry {
            path: "extensions/mezzanine/copilot_usage.mjs".into(),
            artifact: Artifact::File {
                bytes: bytes.to_vec(),
            },
        });
    }
    manifest.entries.push(Entry {
        path: "extensions/mezzanine/peer_helper.mjs".into(),
        artifact: Artifact::File {
            bytes: format!(
                "export const peerHelper = {};\n",
                serde_json::json!(std::env::current_exe().ok()?.to_str()?)
            )
            .into_bytes(),
        },
    });
    Some(manifest)
}

/// Reconstructs exact revision2 hook bytes without the new extension graph.
pub(super) fn historical_lifecycle_manifest() -> Option<Manifest> {
    build(
        2,
        &[
            "sessionStart",
            "userPromptSubmitted",
            "preToolUse",
            "postToolUse",
            "notification",
            "agentStop",
            "errorOccurred",
            "sessionEnd",
        ],
    )
}

/// Reconstructs exact revision1 bytes independently of new event deployment.
pub(super) fn historical_manifest() -> Option<Manifest> {
    build(1, &["sessionStart", "agentStop", "sessionEnd"])
}

/// Literal reviewed event lists alone determine artifact authority and revision.
fn build(revision: u32, events: &[&str]) -> Option<Manifest> {
    let helper = std::env::current_exe().ok()?;
    let helper_text = helper.to_str()?;
    if !helper.is_absolute()
        || helper_text.len() > 4096
        || helper_text.chars().any(char::is_control)
    {
        return None;
    }
    let mut hooks = serde_json::Map::new();
    for event in events {
        hooks.insert(
            (*event).into(),
            serde_json::json!([{"type":"command","exec":helper_text,
            "args":["copilot-hook",event],"timeoutSec":1}]),
        );
    }
    Some(Manifest {
        harness: "copilot".into(),
        revision,
        vendor_version: "best-effort".into(),
        entries: vec![Entry {
            path: "hooks/mezzanine.json".into(),
            artifact: Artifact::File {
                bytes: (serde_json::json!({"version":1,"hooks":hooks}).to_string() + "\n")
                    .into_bytes(),
            },
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped revision3 attachment entry/client are independent immutable
    /// inputs. Active pulse edits must never redefine an old receipt's authority;
    /// the historical graph has no lifetime module or pulse activation.
    #[test]
    fn bootstrap_copilot_attachment_history_uses_frozen_revision3_bytes() {
        use sha2::Digest;
        let old = historical_attachment_manifest().unwrap();
        assert_eq!(old.revision, 3);
        assert_eq!(old.entries.len(), 4);
        for (index, expected) in [
            (
                1,
                "7d245415649144f2660e691189b9a253efb8d7b2d94678d1a9e37b9f31f0557a",
            ),
            (
                2,
                "34b0c8ff57d75a4abaf372940ba0bd0830dbce6c376d0c54223cdc058261656a",
            ),
        ] {
            let Artifact::File { bytes } = &old.entries[index].artifact else {
                panic!("frozen source file");
            };
            let actual = sha2::Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(actual, expected);
        }
    }
    /// Revision4's entry, direct client and pulse lifetime are immutable receipt
    /// authority. Startup retry edits must not redefine the historical graph.
    #[test]
    fn bootstrap_copilot_idle_history_uses_frozen_revision4_bytes() {
        use sha2::Digest;
        let old = historical_idle_manifest().unwrap();
        assert_eq!(old.revision, 4);
        assert_eq!(old.entries.len(), 5);
        for (index, expected) in [
            (
                1,
                "ca091ed90bd0ae0024c3342fb1777736f649930164bb65a5407599fc5eaeadd6",
            ),
            (
                2,
                "41329c7e4bd3ab98b7dad26b803af27e991a632087c3b35734173b63eaca0bed",
            ),
            (
                3,
                "daea5e20758aa68296f3fd978ca618ac7bb931a1b0e5e6d9b004ab6cd659a545",
            ),
        ] {
            let Artifact::File { bytes } = &old.entries[index].artifact else {
                panic!("frozen source file");
            };
            let actual = sha2::Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(actual, expected);
        }
    }

    /// Public bootstrap must supply documented version1 direct-exec user hooks,
    /// with no shell interpolation, prompt/permission output or settings owner.
    /// Revision5 startup sources remain independent immutable receipt authority.
    /// Adding terminal delivery must not reinterpret installed old bytes; upgrade
    /// recognizes the old graph and uninstall removes only its owned artifacts.
    #[test]
    fn bootstrap_copilot_startup_history_uses_frozen_revision5_bytes() {
        use sha2::Digest;
        let old = historical_startup_manifest().unwrap();
        assert_eq!(old.revision, 5);
        assert_eq!(old.entries.len(), 5);
        for (index, expected) in [
            (
                1,
                "3277d35851f349f1dff2c5e2b6829fe8a573e48feb536815c8ce3e4601819816",
            ),
            (
                2,
                "41329c7e4bd3ab98b7dad26b803af27e991a632087c3b35734173b63eaca0bed",
            ),
            (
                3,
                "8e9a3a42deb36100de3a7ecfbbc657a5c5032e44d13a7a30b11cea500128e408",
            ),
        ] {
            let Artifact::File { bytes } = &old.entries[index].artifact else {
                panic!("frozen source");
            };
            let actual = sha2::Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(actual, expected);
        }
    }

    /// The metadata extension does not claim default activation or request origin.
    /// Installation is not proof of loaded/enrolled or accounted callbacks.
    #[test]
    fn bootstrap_copilot_manifest_pins_only_metadata_user_hooks() {
        let manifest =
            super::super::compiled_manifest("copilot", None).expect("compiled Copilot hooks");
        assert_eq!(manifest.entries.len(), 6);
        assert_eq!(manifest.revision, 6);
        assert_eq!(manifest.entries[0].path, "hooks/mezzanine.json");
        let Artifact::File { bytes } = &manifest.entries[0].artifact else {
            panic!("owned private hook document");
        };
        let document: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(
            manifest.entries[1].path,
            "extensions/mezzanine/extension.mjs"
        );
        assert_eq!(
            manifest.entries[2].path,
            "extensions/mezzanine/copilot_extension.mjs"
        );
        assert_eq!(
            manifest.entries[5].path,
            "extensions/mezzanine/peer_helper.mjs"
        );
        assert_eq!(document["version"], 1);
        assert_eq!(document["hooks"].as_object().unwrap().len(), 8);
        for event in [
            "sessionStart",
            "userPromptSubmitted",
            "preToolUse",
            "postToolUse",
            "notification",
            "agentStop",
            "errorOccurred",
            "sessionEnd",
        ] {
            let hook = &document["hooks"][event][0];
            assert_eq!(
                hook["exec"],
                std::env::current_exe().unwrap().to_str().unwrap()
            );
            assert_eq!(hook["args"], serde_json::json!(["copilot-hook", event]));
            assert!(hook.get("bash").is_none());
            assert!(hook.get("env").is_none());
        }
    }

    /// Receipt-qualified edits to the generated private hook file are preserved
    /// outside vendor load paths before repair. Foreign collisions never become
    /// ownership, and authored settings/hooks/credentials are not this artifact.
    #[test]
    fn bootstrap_copilot_private_repair_preserves_edits_and_foreign_collision() {
        use super::super::installer::{Operation, plan_private};
        let home = std::env::temp_dir().join(format!(
            "mez-copilot-owned-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        super::super::fixtures::create_private_directory(&home).unwrap();
        let root = home.join("vendor");
        std::fs::create_dir(&root).unwrap();
        let manifest = manifest().unwrap();
        let plan = |operation| plan_private(&root, &home, &manifest, operation);
        plan(Operation::Install).unwrap().apply().unwrap();
        let file = root.join("hooks/mezzanine.json");
        let extension = root.join("extensions/mezzanine/copilot_extension.mjs");
        std::fs::write(&file, b"PRIVATE USER EDIT").unwrap();
        std::fs::write(&extension, b"PRIVATE EXTENSION EDIT").unwrap();
        let preview = plan(Operation::Install).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"PRIVATE USER EDIT");
        assert_eq!(
            std::fs::read(&extension).unwrap(),
            b"PRIVATE EXTENSION EDIT"
        );
        assert_eq!(preview.preserved_paths().len(), 2);
        preview.apply().unwrap();
        assert!(plan(Operation::Install).unwrap().changed_paths().is_empty());
        plan(Operation::Uninstall).unwrap().apply().unwrap();
        std::fs::write(&extension, b"FOREIGN EXTENSION").unwrap();
        assert!(plan(Operation::Install).is_err());
        assert!(!file.exists());
        assert_eq!(std::fs::read(&extension).unwrap(), b"FOREIGN EXTENSION");
        std::fs::remove_file(extension).unwrap();
        std::fs::write(&file, b"FOREIGN USER HOOK").unwrap();
        assert!(plan(Operation::Install).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"FOREIGN USER HOOK");
        std::fs::remove_dir_all(home).unwrap();
    }

    /// Current hooks and extension automatically upgrade independent revisions1/2/3/4
    /// deployment bytes and receipt. Authored siblings/settings remain untouched;
    /// repeat and uninstall use the current manifest without adopting a foreign file.
    #[test]
    fn bootstrap_copilot_revision1_upgrades_and_repeats() {
        use super::super::installer::{Operation, plan_private};
        let home = std::env::temp_dir().join(format!(
            "mez-copilot-upgrade-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        super::super::fixtures::create_private_directory(&home).unwrap();
        for old in [
            historical_manifest().unwrap(),
            historical_lifecycle_manifest().unwrap(),
            historical_attachment_manifest().unwrap(),
            historical_idle_manifest().unwrap(),
            historical_startup_manifest().unwrap(),
        ] {
            let root = home.join(format!("vendor-{}", old.revision));
            std::fs::create_dir(&root).unwrap();
            let current = manifest().unwrap();
            plan_private(&root, &home, &old, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            std::fs::write(root.join("settings.json"), b"AUTHORED EXPORTER").unwrap();
            plan_private(&root, &home, &current, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            assert!(
                plan_private(&root, &home, &current, Operation::Install)
                    .unwrap()
                    .changed_paths()
                    .is_empty()
            );
            assert!(root.join("extensions/mezzanine/peer_helper.mjs").exists());
            let document: serde_json::Value =
                serde_json::from_slice(&std::fs::read(root.join("hooks/mezzanine.json")).unwrap())
                    .unwrap();
            assert_eq!(document["hooks"].as_object().unwrap().len(), 8);
            assert_eq!(
                std::fs::read(root.join("settings.json")).unwrap(),
                b"AUTHORED EXPORTER"
            );
            plan_private(&root, &home, &current, Operation::Uninstall)
                .unwrap()
                .apply()
                .unwrap();
            assert!(!root.join("hooks/mezzanine.json").exists());
            assert!(!root.join("extensions/mezzanine/extension.mjs").exists());
            assert!(
                !root
                    .join("extensions/mezzanine/copilot_lifetime.mjs")
                    .exists()
            );
            assert!(!root.join("extensions/mezzanine/peer_helper.mjs").exists());
            assert!(!root.join("extensions/mezzanine/copilot_usage.mjs").exists());
            assert!(
                !root
                    .join("extensions/mezzanine/copilot_extension.mjs")
                    .exists()
            );
        }
        std::fs::remove_dir_all(home).unwrap();
    }
}
