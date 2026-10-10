//! Compiled dependency-free OpenCode artifacts with exact installer ownership.
//! Only a named local plugin entry and private siblings are owned. No settings,
//! package dependencies, credentials, policy or shell profiles are changed.

use super::installer::Manifest;
use super::reconciliation::{Artifact, Entry};

/// Supplies best-effort compiled artifacts for an explicit OpenCode config root.
pub(crate) fn manifest() -> Manifest {
    let mut manifest = Manifest {
        harness: "opencode".into(),
        vendor_version: "best-effort".into(),
        revision: 7,
        entries: vec![
            Entry {
                path: "plugins/mezzanine.js".into(),
                artifact: Artifact::File {
                    bytes: b"export { MezzanineOpenCode } from './mezzanine/opencode_entry.mjs';\n"
                        .to_vec(),
                },
            },
            Entry {
                path: "plugins/mezzanine/opencode_entry.mjs".into(),
                artifact: Artifact::File {
                    bytes: include_bytes!("opencode_entry.mjs").to_vec(),
                },
            },
            Entry {
                path: "plugins/mezzanine/opencode_observer.mjs".into(),
                artifact: Artifact::File {
                    bytes: include_bytes!("opencode_observer.mjs").to_vec(),
                },
            },
            Entry {
                path: "plugins/mezzanine/opencode_tui.mjs".into(),
                artifact: Artifact::File {
                    bytes: include_bytes!("opencode_tui.mjs").to_vec(),
                },
            },
            Entry {
                path: "tui.json".into(),
                artifact: Artifact::JsoncArrayEntry {
                    pointer: "/plugin".into(),
                    value: serde_json::json!("./plugins/mezzanine/opencode_tui.mjs"),
                },
            },
        ],
    };
    manifest
        .entries
        .extend(super::persistent_client::entries("plugins/mezzanine"));
    manifest
}

/// Compiled higher-priority root-local TUI filename, never an arbitrary path.
pub(super) fn jsonc_manifest() -> Manifest {
    let mut candidate = manifest();
    for entry in &mut candidate.entries {
        if entry.path == "tui.json" {
            entry.path = "tui.jsonc".into();
        }
    }
    candidate
}

/// Compiled nested TUI registration, used only when loader normalization makes
/// the existing nested array effective. The containing authored fields are not owned.
pub(super) fn nested_manifest(jsonc: bool) -> Manifest {
    let mut candidate = if jsonc { jsonc_manifest() } else { manifest() };
    for entry in &mut candidate.entries {
        if let Artifact::JsoncArrayEntry { pointer, .. } = &mut entry.artifact {
            *pointer = "/tui/plugin".into();
        }
    }
    candidate
}

/// Exact kind/path identity shipped in revision 6. Module payloads remain the
/// unchanged compiled release bytes; candidate format changes must not redefine
/// historical strict ownership. Fixed helper references retain their existing rules.
pub(super) fn historical_manifest() -> Manifest {
    let mut previous = manifest();
    previous.revision = 6;
    for entry in &mut previous.entries {
        if let Artifact::JsoncArrayEntry { pointer, value } = &entry.artifact {
            entry.artifact = Artifact::JsonArrayEntry {
                pointer: pointer.clone(),
                value: value.clone(),
            };
        }
    }
    previous
}

/// Guards shipped revision-6 payloads not present in the oldest release hash.
/// A later source edit fails closed until its owner supplies reviewed historical
/// snapshots, rather than silently redefining existing receipt/journal authority.
pub(super) fn historical_payloads_match(tui: &[u8], client: &[u8]) -> bool {
    use sha2::Digest;
    [
        (
            tui,
            "9289e29be13f0e1d07102fee76402aaf895c919ecfbda074ca532fa0afbbd536",
        ),
        (
            client,
            "9aad0efad6233466e483bfddaad63b18ea9e45d7468d32a53a651b527cd0556c",
        ),
    ]
    .into_iter()
    .all(|(bytes, expected)| {
        sha2::Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            == expected
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Owned plugin install/repeat/conflict/uninstall never rewrites authored
    /// settings, other plugins, dependencies or credentials in the explicit root.
    #[test]
    fn opencode_artifact_preserves_user_config_and_ownership() {
        use crate::integrations::bootstrap::installer::{Operation, plan_private};
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!(
            "mez-opencode-artifact-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = home.join("vendor");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("opencode.jsonc"),
            b"// authored\n{\"plugin\":[\"unrelated\"]}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tui.json"),
            br#"{"plugin":["user-a",["user-b",{"enabled":true}]],"theme":"authored"}"#,
        )
        .unwrap();
        let manifest = manifest();
        let plan = |operation| plan_private(&root, &home, &manifest, operation);
        plan(Operation::Install).unwrap().apply().unwrap();
        assert!(plan(Operation::Install).unwrap().changed_paths().is_empty());
        let entry = root.join("plugins/mezzanine.js");
        let original = std::fs::read(&entry).unwrap();
        std::fs::write(&entry, b"authored edit").unwrap();
        let removal = plan(Operation::Uninstall).unwrap();
        assert_eq!(removal.preserved_paths(), vec!["plugins/mezzanine.js"]);
        assert_eq!(std::fs::read(&entry).unwrap(), b"authored edit");
        drop(removal);
        std::fs::write(&entry, original).unwrap();
        plan(Operation::Uninstall).unwrap().apply().unwrap();
        assert!(!entry.exists());
        let tui: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("tui.json")).unwrap()).unwrap();
        assert_eq!(
            tui["plugin"],
            serde_json::json!(["user-a",["user-b",{"enabled":true}]])
        );
        assert_eq!(tui["theme"], "authored");
        assert_eq!(
            std::fs::read(root.join("opencode.jsonc")).unwrap(),
            b"// authored\n{\"plugin\":[\"unrelated\"]}\n"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
