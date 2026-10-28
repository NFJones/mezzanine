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
    assert_eq!(current.revision, 18);
    assert_eq!(previous.entries.len(), 10);
    let revision10 = historical_usage_manifest();
    assert_eq!(revision10.revision, 10);
    assert_eq!(revision10.entries.len(), 11);
    assert!(super::super::compiled_history(&current).contains(&revision10));
    let revision11 = historical_loss_manifest();
    assert_eq!(revision11.revision, 11);
    let revision12 = historical_resume_manifest();
    assert_eq!(revision12.revision, 12);
    assert!(super::super::compiled_history(&current).contains(&revision12));
    assert!(super::super::compiled_history(&revision12).contains(&revision11));
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
            include_bytes!("../history/pi_usage_v12.mjs").as_slice(),
            "9ea319f42d99b8e1211b9707ac7e433082a160c85d2762f2941bf99f8a8b10fb",
        ),
        (
            include_bytes!("../history/pi_persistent_v12.mjs").as_slice(),
            "6b6ad7e56150865660d20705a35f373ee51ecc8e0f29bf53f0f9d941353bba00",
        ),
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
    plan(&root, &revision12, Operation::Install)
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

/// Revision13 changes only the two independently frozen Pi producer slots.
/// Revision11 must resolve those exact revision12 bytes rather than rebuilding
/// them from active source; the shared transport differs only at its previously
/// qualified resume boundary. Every compiled predecessor/file slot rejects byte
/// tampering as historical authority, including receipt-selected revision labels.
#[test]
fn pi_revision13_history_preserves_frozen_slots_and_rejects_tampered_authority() {
    let current = candidate_manifest();
    let revision13 = historical_end_manifest();
    let before_auxiliary = historical_auxiliary_manifest();
    let revision12 = historical_resume_manifest();
    let revision11 = historical_loss_manifest();
    assert_eq!(current.entries.len(), revision12.entries.len());
    // The previously shipped transport/accounting boundary remains frozen;
    // revision18's separate two-producer delta has its own ownership test.
    let end_changes = before_auxiliary
        .entries
        .iter()
        .zip(&revision13.entries)
        .filter_map(|(active, frozen)| (active != frozen).then_some(active.path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        end_changes,
        [
            "extensions/mezzanine/persistent_client.mjs",
            "extensions/mezzanine/request_accounting.mjs",
        ]
    );
    let changed = revision13
        .entries
        .iter()
        .zip(&revision12.entries)
        .filter_map(|(active, frozen)| (active != frozen).then_some(active.path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        changed,
        [
            "extensions/mezzanine/pi_persistent.mjs",
            "extensions/mezzanine/pi_usage.mjs"
        ]
    );
    for (old, frozen) in revision11.entries.iter().zip(&revision12.entries) {
        assert_eq!(old.path, frozen.path);
        if !old.path.ends_with("/persistent_client.mjs") {
            assert_eq!(old.artifact, frozen.artifact);
        }
    }
    let history = super::super::compiled_history(&current);
    assert_eq!(history.len(), 16);
    assert_eq!(
        history.iter().map(|item| item.revision).collect::<Vec<_>>(),
        (2..=17).rev().collect::<Vec<_>>()
    );
    for manifest in std::iter::once(current).chain(history) {
        for index in 0..manifest.entries.len() {
            let mut altered = manifest.clone();
            if let Artifact::File { bytes } = &mut altered.entries[index].artifact {
                bytes.push(0);
                assert!(
                    super::super::compiled_history(&altered).is_empty(),
                    "changed artifact bytes granted compiled historical authority"
                );
            }
        }
    }
}

/// Revision17 remains byte-exact installer history after auxiliary-disclosure
/// wiring changes the two Pi-owned modules. Shared transport and every other
/// installed file remain identical; altered historical bytes grant no ownership.
#[test]
fn pi_auxiliary_revision18_preserves_frozen_revision17_ownership() {
    use sha2::Digest;
    let old = historical_auxiliary_manifest();
    let current = candidate_manifest();
    assert_eq!(old.revision, 17);
    assert_eq!(current.revision, 18);
    assert!(super::super::compiled_history(&current).contains(&old));
    for (bytes, expected) in [
        (
            include_bytes!("../history/pi_usage_v17.mjs").as_slice(),
            "9e781100f974659f87acb29789cf47c57a4c00f3deb6d9f3c40da58d647e3b4e",
        ),
        (
            include_bytes!("../history/pi_persistent_v17.mjs").as_slice(),
            "322a7ed1771098e54d2d387bd0d3de28326675ca8ecd725a977bc683566afdef",
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
    let changed = current
        .entries
        .iter()
        .filter(|entry| !old.entries.contains(entry))
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        changed,
        [
            "extensions/mezzanine/pi_persistent.mjs",
            "extensions/mezzanine/pi_usage.mjs"
        ]
    );
    let mut forged = old;
    if let Artifact::File { bytes } = &mut forged.entries[0].artifact {
        bytes.push(0);
    }
    assert!(super::super::compiled_history(&forged).is_empty());
}
