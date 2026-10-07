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
        revision: 3,
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
        ],
    };
    manifest
        .entries
        .extend(super::persistent_client::entries("plugins/mezzanine"));
    manifest
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Owned plugin install/repeat/conflict/uninstall never rewrites authored
    /// settings, other plugins, dependencies or credentials in the explicit root.
    #[test]
    fn opencode_artifact_preserves_user_config_and_ownership() {
        use crate::integrations::bootstrap::installer::{Operation, plan};
        let root = std::env::temp_dir().join(format!(
            "mez-opencode-artifact-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("opencode.jsonc"),
            b"// authored\n{\"plugin\":[\"unrelated\"]}\n",
        )
        .unwrap();
        let manifest = manifest();
        plan(&root, &manifest, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        assert!(
            plan(&root, &manifest, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        let entry = root.join("plugins/mezzanine.js");
        let original = std::fs::read(&entry).unwrap();
        std::fs::write(&entry, b"authored edit").unwrap();
        assert!(plan(&root, &manifest, Operation::Uninstall).is_err());
        std::fs::write(&entry, original).unwrap();
        plan(&root, &manifest, Operation::Uninstall)
            .unwrap()
            .apply()
            .unwrap();
        assert!(!entry.exists());
        assert_eq!(
            std::fs::read(root.join("opencode.jsonc")).unwrap(),
            b"// authored\n{\"plugin\":[\"unrelated\"]}\n"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
