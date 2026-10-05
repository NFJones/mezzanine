//! Candidate ownership qualification in temporary explicit roots only.

use super::*;
use crate::integrations::bootstrap::installer::{Operation, plan};

/// Repeat installation is byte-stable; edited artifacts conflict without
/// overwrite, and uninstall preserves unrelated extensions/settings. Candidate
/// ownership does not enable the public certified registry.
#[test]
fn pi_artifact_candidate_owned_install_repeat_conflict_and_uninstall() {
    let root = std::env::temp_dir().join(format!(
        "mez-pi-artifact-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("settings.json"), b"{\"authored\":true}\n").unwrap();
    let manifest = candidate_manifest();
    assert!(crate::integrations::bootstrap::certified_manifest("pi", Some("1.0.2")).is_none());
    assert_eq!(manifest.entries.len(), 5);
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
    let path = root.join("extensions/mezzanine/index.mjs");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"authored edit").unwrap();
    assert!(plan(&root, &manifest, Operation::Install).is_err());
    assert!(plan(&root, &manifest, Operation::Uninstall).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"authored edit");
    std::fs::write(&path, original).unwrap();
    std::fs::write(root.join("extensions/unrelated.js"), b"authored extension").unwrap();
    plan(&root, &manifest, Operation::Uninstall)
        .unwrap()
        .apply()
        .unwrap();
    assert!(!path.exists());
    assert_eq!(
        std::fs::read(root.join("settings.json")).unwrap(),
        b"{\"authored\":true}\n"
    );
    assert_eq!(
        std::fs::read(root.join("extensions/unrelated.js")).unwrap(),
        b"authored extension"
    );
    std::fs::remove_dir_all(root).unwrap();
}
