//! Exact unreceipted private-bundle adoption through real installer owners.
//!
//! Fixtures materialize only compiled private files, never receipts or runtime
//! capabilities. Current private publication must qualify complete byte bundles,
//! preserve unrelated siblings, revalidate preimages and recover exact intent.

use super::*;
use crate::integrations::bootstrap::reconciliation::Artifact;

/// A genuine v5 transaction without adoption retains its original install
/// authority and effects after the new version is introduced. New optional intent
/// fields are absent, not synthetic receipt provenance; recovery settles once.
#[test]
fn bootstrap_private_adoption_genuine_v5_without_adoption_recovers() {
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    let fixture = Fixture::new();
    let accepted = fixture.plan(&current, Operation::Install).unwrap();
    accepted.fixture_interrupt_after(1);
    assert!(accepted.apply().is_err());
    let mut journal: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.journal()).unwrap()).unwrap();
    assert!(journal["intent"].get("adopted").is_none());
    journal["version"] = 5.into();
    fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
    assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
    assert!(!recover_private(&fixture.root, &fixture.home, &current).unwrap());
    for entry in &current.entries {
        let Artifact::File { bytes } = &entry.artifact else {
            panic!("private only");
        };
        assert_eq!(fs::read(fixture.root.join(&entry.path)).unwrap(), *bytes);
    }
    assert!(
        fixture
            .plan(&current, Operation::Install)
            .unwrap()
            .changed_paths()
            .is_empty()
    );
}

/// A partial or edited generated-looking bundle cannot authorize a replacement.
/// Shared documents never qualify; existing receipt errors are not bypassed.
/// Adoption inspection is frozen like other plans, and late preimage drift must
/// preserve edited bytes rather than implicitly enabling archive repair.
#[test]
fn bootstrap_private_adoption_refuses_unqualified_bundles_and_drift() {
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    for fault in ["partial", "edited", "invalid-receipt"] {
        let fixture = Fixture::new();
        unreceipted_bundle(&fixture, &current);
        let file = fixture.root.join("extensions/mezzanine/index.mjs");
        match fault {
            "partial" => fs::remove_file(&file).unwrap(),
            "edited" => fs::write(&file, b"authored edit").unwrap(),
            "invalid-receipt" => fs::write(
                fixture.root.join("mez-bootstrap-ownership-pi.json"),
                b"invalid receipt",
            )
            .unwrap(),
            _ => unreachable!(),
        }
        let before = tree_snapshot(&fixture.workspace);
        assert!(fixture.plan(&current, Operation::Install).is_err());
        assert_eq!(tree_snapshot(&fixture.workspace), before);
    }
    let fixture = Fixture::new();
    unreceipted_bundle(&fixture, &current);
    let before = tree_snapshot(&fixture.workspace);
    let uninstall = fixture.plan(&current, Operation::Uninstall).unwrap();
    assert!(uninstall.changed_paths().is_empty());
    uninstall.apply().unwrap();
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    // Failed publication may create its cooperating lock/private namespace;
    // prime only that state so the full snapshot proves no artifact/receipt edit.
    let mut owner = Publisher::inspect_private(&fixture.root, &fixture.home).unwrap();
    owner.acquire_lock().unwrap();
    drop(owner);
    let accepted = fixture.plan(&current, Operation::Install).unwrap();
    fs::write(
        fixture.root.join("extensions/mezzanine/index.mjs"),
        b"late edited preimage",
    )
    .unwrap();
    let before = tree_snapshot(&fixture.workspace);
    assert!(accepted.apply().is_err());
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    let codex = crate::integrations::bootstrap::compiled_manifest("codex", None).unwrap();
    let old = crate::integrations::bootstrap::compiled_history(&codex)
        .pop()
        .unwrap();
    let fixture = Fixture::new();
    unreceipted_bundle(&fixture, &old);
    let before = tree_snapshot(&fixture.workspace);
    assert!(fixture.plan(&codex, Operation::Install).is_err());
    assert_eq!(tree_snapshot(&fixture.workspace), before);
}

/// Neither an adoption marker nor a journal is authority: recovery re-proves
/// every compiled original byte, including unchanged files and receipt absence.
/// Forged versions, identities, receipt provenance, preimages, omitted effects or
/// edited unchanged bundle members reject with both trees left exactly intact.
#[test]
fn bootstrap_private_adoption_forged_recovery_preserves_all_state() {
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    let old = crate::integrations::bootstrap::compiled_history(&current)
        .pop()
        .unwrap();
    for fault in [
        "version",
        "identity",
        "harness",
        "path",
        "previous",
        "uninstall",
        "remove-proof",
        "preimage",
        "omit",
        "unchanged",
        "receipt",
    ] {
        let fixture = Fixture::new();
        unreceipted_bundle(&fixture, &old);
        let accepted = fixture.plan(&current, Operation::Install).unwrap();
        accepted.fixture_interrupt_after(1);
        assert!(accepted.apply().is_err());
        let original = fs::read(fixture.journal()).unwrap();
        let mut journal: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let package = fixture.root.join("extensions/mezzanine/package.json");
        let original_package = fs::read(&package).unwrap();
        match fault {
            "version" => journal["version"] = 5.into(),
            "identity" => journal["intent"]["adopted"]["revision"] = 99.into(),
            "harness" => journal["intent"]["adopted"]["harness"] = "opencode".into(),
            "path" => journal["intent"]["adopted"]["entries"][0]["path"] = "settings.json".into(),
            "previous" => journal["intent"]["previous"] = journal["intent"]["adopted"].clone(),
            "uninstall" => journal["intent"]["operation"] = "Uninstall".into(),
            "remove-proof" => {
                journal["intent"].as_object_mut().unwrap().remove("adopted");
            }
            "preimage" => journal["changes"][0]["before"] = serde_json::json!([1]),
            "omit" => {
                journal["changes"].as_array_mut().unwrap().remove(0);
            }
            "unchanged" => fs::write(&package, b"edited unchanged member").unwrap(),
            "receipt" => {
                let last = journal["changes"]
                    .as_array_mut()
                    .unwrap()
                    .last_mut()
                    .unwrap();
                last["before"] = serde_json::json!(b"{}".to_vec());
            }
            _ => unreachable!(),
        }
        fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
        let before = tree_snapshot(&fixture.workspace);
        assert!(
            recover_private(&fixture.root, &fixture.home, &current).is_err(),
            "{fault}"
        );
        assert_eq!(tree_snapshot(&fixture.workspace), before);
        fs::write(&package, original_package).unwrap();
        fs::write(fixture.journal(), original).unwrap();
        assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
    }
}

/// Copies compiled private bytes into the test-owned vendor tree, without creating
/// installer state or receipt evidence. Shared documents are deliberately excluded.
fn unreceipted_bundle(fixture: &Fixture, manifest: &Manifest) {
    for entry in &manifest.entries {
        let Artifact::File { bytes } = &entry.artifact else {
            panic!("private bundle only");
        };
        let path = fixture.root.join(&entry.path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}

/// Current and historical complete Pi/OpenCode private bundles can be recognized
/// by compiled exact bytes, not filename/version text. Read-only inspection writes
/// nothing; install upgrades or establishes ownership, preserves authored siblings,
/// and repeat becomes receipt-qualified byte-exact noop without repeated adoption.
#[test]
fn bootstrap_private_adoption_recognizes_complete_compiled_bundles() {
    for harness in ["pi", "opencode"] {
        let current = crate::integrations::bootstrap::compiled_manifest(harness, None).unwrap();
        for old in std::iter::once(current.clone())
            .chain(crate::integrations::bootstrap::compiled_history(&current))
            .filter(|manifest| {
                manifest
                    .entries
                    .iter()
                    .all(|entry| matches!(entry.artifact, Artifact::File { .. }))
            })
        {
            let fixture = Fixture::new();
            unreceipted_bundle(&fixture, &old);
            fs::write(fixture.root.join("settings.json"), b"authored settings").unwrap();
            let before = tree_snapshot(&fixture.workspace);
            let accepted = fixture
                .plan(&current, Operation::Install)
                .expect("exact complete compiled private bundle is adoptable");
            assert_eq!(tree_snapshot(&fixture.workspace), before);
            let intent = serde_json::to_value(&accepted.intent).unwrap();
            assert_eq!(intent["previous"], serde_json::Value::Null);
            assert_eq!(intent["previous_private"], serde_json::Value::Null);
            assert_eq!(intent["previous_vendor"], serde_json::Value::Null);
            assert_eq!(intent["adopted"], serde_json::to_value(&old).unwrap());
            assert!(accepted.intent.archives.is_empty());
            accepted.apply().unwrap();
            for entry in &current.entries {
                if let Artifact::File { bytes } = &entry.artifact {
                    assert_eq!(fs::read(fixture.root.join(&entry.path)).unwrap(), *bytes);
                }
            }
            assert_eq!(
                fs::read(fixture.root.join("settings.json")).unwrap(),
                b"authored settings"
            );
            let before = tree_snapshot(&fixture.workspace);
            let repeated = fixture.plan(&current, Operation::Install).unwrap();
            assert!(repeated.changed_paths().is_empty());
            assert!(serde_json::to_value(&repeated.intent).unwrap()["adopted"].is_null());
            repeated.apply().unwrap();
            assert_eq!(tree_snapshot(&fixture.workspace), before);
        }
    }
}

/// All adoption publication boundaries retain original explicit provenance, not
/// a synthesized previous receipt. Recovery re-proves compiled preimages, finishes
/// once and leaves current ownership exact; older version labels cannot acquire
/// adoption permission, even after an already confirmed artifact effect.
#[test]
fn bootstrap_private_adoption_recovers_every_effect_boundary() {
    for harness in ["pi", "opencode"] {
        let current = crate::integrations::bootstrap::compiled_manifest(harness, None).unwrap();
        let old = crate::integrations::bootstrap::compiled_history(&current)
            .pop()
            .unwrap();
        let probe = Fixture::new();
        unreceipted_bundle(&probe, &old);
        let effects = probe
            .plan(&current, Operation::Install)
            .unwrap()
            .changes
            .len();
        for boundary in 1..=effects {
            let fixture = Fixture::new();
            unreceipted_bundle(&fixture, &old);
            let accepted = fixture.plan(&current, Operation::Install).unwrap();
            accepted.fixture_interrupt_after(boundary);
            assert!(accepted.apply().is_err());
            let original = fs::read(fixture.journal()).unwrap();
            let mut journal: serde_json::Value = serde_json::from_slice(&original).unwrap();
            assert_eq!(journal["version"], 7);
            journal["version"] = 5.into();
            fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
            let before = tree_snapshot(&fixture.workspace);
            assert!(recover_private(&fixture.root, &fixture.home, &current).is_err());
            assert_eq!(tree_snapshot(&fixture.workspace), before);
            fs::write(fixture.journal(), original).unwrap();
            assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
            assert!(!recover_private(&fixture.root, &fixture.home, &current).unwrap());
            assert!(
                fixture
                    .plan(&current, Operation::Install)
                    .unwrap()
                    .changed_paths()
                    .is_empty()
            );
            assert_eq!(
                fs::read(fixture.root.join("authored")).unwrap(),
                b"preserved"
            );
        }
    }
}
