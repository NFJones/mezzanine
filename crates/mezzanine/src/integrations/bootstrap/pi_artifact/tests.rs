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
    assert!(crate::integrations::bootstrap::compiled_manifest("pi", Some("any-version")).is_some());
    assert_eq!(manifest.entries.len(), 11);
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

/// Revision-9 producer and shared transport bytes are independent of active
/// revision 10. Upgrade/repair/repeat preserves authored configuration and only
/// replaces recognized owned slots, including the newly installed projector.
#[test]
fn pi_usage_revision9_upgrade_and_repair_preserve_exact_owned_history() {
    use sha2::Digest;
    let root = std::env::temp_dir().join(format!(
        "mez-pi-usage-upgrade-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("settings.json"), b"{\"authored\":true}\n").unwrap();
    let previous = historical_manifest();
    let current = candidate_manifest();
    assert_eq!(previous.revision, 9);
    assert_eq!(current.revision, 12);
    assert_eq!(previous.entries.len(), 10);
    let revision10 = historical_usage_manifest();
    assert_eq!(revision10.revision, 10);
    assert_eq!(revision10.entries.len(), 11);
    assert!(super::super::compiled_history(&current).contains(&revision10));
    let revision11 = historical_loss_manifest();
    assert_eq!(revision11.revision, 11);
    assert!(super::super::compiled_history(&current).contains(&revision11));
    assert!(super::super::compiled_history(&revision11).contains(&revision10));
    let mut altered11 = revision11.clone();
    if let Artifact::File { bytes } = &mut altered11
        .entries
        .iter_mut()
        .find(|entry| entry.path.ends_with("/persistent_client.mjs"))
        .unwrap()
        .artifact
    {
        bytes.push(0);
    }
    assert!(super::super::compiled_history(&altered11).is_empty());
    for (bytes, expected) in [
        (
            include_bytes!("../history/pi_persistent_v10.mjs").as_slice(),
            "eb5e0bd7fc7ac984527b6d68c166375ce61e05a86feff7bea2c1b537d55a04a7",
        ),
        (
            include_bytes!("../history/pi_usage_v10.mjs").as_slice(),
            "26ae8734bd3220201acd598a7e1823917f2209cfdb4113b7e501ec69e7f2a279",
        ),
    ] {
        assert_eq!(
            sha2::Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            expected
        );
    }
    assert!(super::super::compiled_history(&current).contains(&previous));
    assert_eq!(
        sha2::Sha256::digest(include_bytes!("../history/pi_persistent_v9.mjs"))
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "557558488625458ddcf04e198581bda2bd7548d44755b3cc029c50df81c9950d"
    );
    plan(&root, &previous, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    plan(&root, &revision10, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    plan(&root, &revision11, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    plan(&root, &current, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    assert!(root.join("extensions/mezzanine/pi_usage.mjs").is_file());
    assert!(
        plan(&root, &current, Operation::Install)
            .unwrap()
            .changed_paths()
            .is_empty()
    );
    std::fs::remove_file(root.join("extensions/mezzanine/pi_usage.mjs")).unwrap();
    plan(&root, &current, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    assert!(
        plan(&root, &current, Operation::Install)
            .unwrap()
            .changed_paths()
            .is_empty()
    );
    assert_eq!(
        std::fs::read(root.join("settings.json")).unwrap(),
        b"{\"authored\":true}\n"
    );
    std::fs::remove_dir_all(root).unwrap();
}
