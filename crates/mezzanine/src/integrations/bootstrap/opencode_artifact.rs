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
        revision: 14,
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

/// Exact bounded-end transport for every deployed revision13 loader layout.
pub(super) fn historical_bounded_end_manifest(mut candidate: Manifest) -> Manifest {
    candidate.revision = 13;
    super::history::freeze_bounded_end_transport(&mut candidate);
    candidate
}

/// Exact one-shot end transport for every deployed revision-12 loader layout.
pub(super) fn historical_one_shot_end_manifest(mut candidate: Manifest) -> Manifest {
    candidate.revision = 12;
    super::history::freeze_one_shot_end_transport(&mut candidate);
    candidate
}

/// Exact pre-end shared transport for every deployed revision-11 loader layout.
pub(super) fn historical_end_manifest(mut candidate: Manifest) -> Manifest {
    candidate.revision = 11;
    super::history::freeze_pre_end_transport(&mut candidate);
    candidate
}

/// Exact pre-resume shared client for every compiled revision-10 loader layout.
pub(super) fn historical_pre_resume_manifest(mut candidate: Manifest) -> Manifest {
    candidate.revision = 10;
    super::history::freeze_pre_resume_transport(&mut candidate);
    candidate
}

/// Freezes only the deployed revision-9 shared transport for every compiled
/// loader filename/pointer variant. Pi gap support grants no OpenCode behavior.
pub(super) fn historical_request_manifest(mut candidate: Manifest) -> Manifest {
    candidate.revision = 9;
    super::history::freeze_revision9_transport(&mut candidate);
    candidate
}

/// Exact kind/path identity shipped in revision 6. Module payloads remain the
/// unchanged compiled release bytes; candidate format changes must not redefine
/// historical strict ownership. Fixed helper references retain their existing rules.
pub(super) fn historical_manifest() -> Manifest {
    let mut previous = manifest();
    previous.revision = 6;
    super::history::freeze_pre_request_client(&mut previous);
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
    /// Every shipped JSONC-era layout must remain recognized independently of
    /// the newly effective loader layout. Upgrades remove only the old owned
    /// registration, retain authored fields in both documents, and become
    /// byte-exact on repetition. This uses private publication with isolated
    /// HOME/root witnesses, never a vendor process or real user settings.
    #[test]
    fn opencode_artifact_upgrades_across_historical_loader_layouts() {
        use crate::integrations::bootstrap::installer::{Operation, plan_private};
        use std::os::unix::fs::PermissionsExt;
        let layouts = [
            manifest(),
            jsonc_manifest(),
            nested_manifest(false),
            nested_manifest(true),
        ];
        for old_layout in &layouts {
            for revision in [7, 8, 9, 10] {
                let old = super::super::compiled_history(old_layout)
                    .into_iter()
                    .find(|candidate| candidate.revision == revision)
                    .unwrap();
                for wanted in &layouts {
                    assert!(super::super::compiled_history(wanted).contains(&old));
                    let home = std::env::temp_dir().join(format!(
                        "mez-opencode-layout-upgrade-{}",
                        crate::storage::token_usage::new_token_usage_event_id()
                    ));
                    std::fs::create_dir(&home).unwrap();
                    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                    let root = home.join("vendor");
                    std::fs::create_dir(&root).unwrap();
                    let registration = old
                        .entries
                        .iter()
                        .find(|entry| matches!(entry.artifact, Artifact::JsoncArrayEntry { .. }))
                        .unwrap();
                    let document = |entry: &Entry| {
                        let nested = matches!(&entry.artifact, Artifact::JsoncArrayEntry { pointer, .. } if pointer == "/tui/plugin");
                        if nested {
                            serde_json::json!({"tui":{"plugin":["user-nested"]},"theme":"authored"})
                        } else {
                            serde_json::json!({"plugin":["user-root"],"tui":{"plugin":["user-nested"]},"theme":"authored"})
                        }
                    };
                    let mut authored = vec![(registration.path.clone(), document(registration))];
                    std::fs::write(
                        root.join(&registration.path),
                        serde_json::to_vec(&authored[0].1).unwrap(),
                    )
                    .unwrap();
                    plan_private(&root, &home, &old, Operation::Install)
                        .unwrap()
                        .apply()
                        .unwrap();
                    // Creating a higher-priority authored file is a genuine
                    // loader transition. Merely asking for tui.json cannot
                    // override an existing tui.jsonc; that remains authoritative.
                    let requested = wanted
                        .entries
                        .iter()
                        .find(|entry| matches!(entry.artifact, Artifact::JsoncArrayEntry { .. }))
                        .unwrap();
                    if registration.path == "tui.json" && requested.path == "tui.jsonc" {
                        authored.push((requested.path.clone(), document(requested)));
                        std::fs::write(
                            root.join(&requested.path),
                            serde_json::to_vec(&authored[1].1).unwrap(),
                        )
                        .unwrap();
                    }
                    let before =
                        ["tui.json", "tui.jsonc"].map(|name| std::fs::read(root.join(name)).ok());
                    let upgrade = plan_private(&root, &home, wanted, Operation::Install).unwrap();
                    assert_eq!(
                        before,
                        ["tui.json", "tui.jsonc"].map(|name| std::fs::read(root.join(name)).ok())
                    );
                    upgrade.apply().unwrap();
                    assert!(
                        plan_private(&root, &home, wanted, Operation::Install)
                            .unwrap()
                            .changed_paths()
                            .is_empty()
                    );
                    plan_private(&root, &home, wanted, Operation::Uninstall)
                        .unwrap()
                        .apply()
                        .unwrap();
                    for (name, expected) in authored {
                        let actual: serde_json::Value =
                            serde_json::from_slice(&std::fs::read(root.join(name)).unwrap())
                                .unwrap();
                        assert_eq!(actual, expected);
                    }
                    std::fs::remove_dir_all(home).unwrap();
                }
            }
        }
    }

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
