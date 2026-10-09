//! Private generated-helper preservation and archive-before-file recovery.

use super::*;

/// Every shipped compiled predecessor can preserve an edited known entry helper
/// during current upgrade. Each currently generated nested helper is qualified
/// separately, not just its loader entry; exact raw bytes survive privately.
#[test]
fn bootstrap_private_archive_covers_historical_upgrades_and_nested_helpers() {
    use crate::integrations::bootstrap::reconciliation::Artifact;
    for harness in ["pi", "opencode"] {
        let current = crate::integrations::bootstrap::compiled_manifest(harness, None).unwrap();
        for old in crate::integrations::bootstrap::compiled_history(&current) {
            let fixture = Fixture::new();
            plan(&fixture.root, &old, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            let entry = old
                .entries
                .iter()
                .find(|entry| {
                    entry.path.ends_with("index.mjs") || entry.path == "plugins/mezzanine.js"
                })
                .unwrap();
            fs::write(fixture.root.join(&entry.path), b"old helper edit").unwrap();
            let accepted = fixture.plan(&current, Operation::Install).unwrap();
            let archive = accepted.intent.archives.get(&entry.path).unwrap().clone();
            accepted.apply().unwrap();
            assert_eq!(
                Publisher::inspect_private(&fixture.root, &fixture.home)
                    .unwrap()
                    .read(&archive)
                    .unwrap()
                    .as_deref(),
                Some(b"old helper edit".as_slice())
            );
            assert!(
                fixture
                    .plan(&current, Operation::Install)
                    .unwrap()
                    .changed_paths()
                    .is_empty()
            );
        }
        for entry in current.entries.iter().filter(|entry| {
            entry.path.ends_with(".mjs") && matches!(entry.artifact, Artifact::File { .. })
        }) {
            let fixture = Fixture::new();
            fixture
                .plan(&current, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            fs::write(fixture.root.join(&entry.path), b"nested helper edit").unwrap();
            let accepted = fixture.plan(&current, Operation::Install).unwrap();
            let archive = accepted.intent.archives.get(&entry.path).unwrap().clone();
            accepted.apply().unwrap();
            assert_eq!(
                Publisher::inspect_private(&fixture.root, &fixture.home)
                    .unwrap()
                    .read(&archive)
                    .unwrap()
                    .as_deref(),
                Some(b"nested helper edit".as_slice())
            );
        }
    }
}

/// A reused archive that disappears after planning cannot silently bypass
/// preservation. The frozen observation rejects before any replacement and
/// no legacy journal can claim a new dependency map without archive effects.
#[test]
fn bootstrap_private_archive_reused_loss_and_legacy_map_are_fenced() {
    let fixture = Fixture::new();
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    fixture
        .plan(&current, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    let file = fixture.root.join("extensions/mezzanine/index.mjs");
    fs::write(&file, b"reused edit").unwrap();
    let first = fixture.plan(&current, Operation::Install).unwrap();
    let key = first
        .intent
        .archives
        .get("extensions/mezzanine/index.mjs")
        .unwrap()
        .clone();
    first.apply().unwrap();
    fs::write(&file, b"reused edit").unwrap();
    let second = fixture.plan(&current, Operation::Install).unwrap();
    let private = fixture
        .receipt("pi")
        .parent()
        .unwrap()
        .join(format!("archive-pi-{}", key.rsplit('/').next().unwrap()));
    fs::remove_file(private).unwrap();
    let before = tree_snapshot(&fixture.workspace);
    assert!(second.apply().is_err());
    assert_eq!(fs::read(&file).unwrap(), b"reused edit");
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    let fixture = Fixture::new();
    let accepted = fixture.plan(&current, Operation::Install).unwrap();
    accepted.fixture_interrupt_after(1);
    assert!(accepted.apply().is_err());
    let mut journal: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.journal()).unwrap()).unwrap();
    journal["version"] = 3.into();
    journal["intent"]["archives"] = serde_json::json!({"extensions/mezzanine/index.mjs":key});
    fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
    let before = tree_snapshot(&fixture.workspace);
    assert!(fixture.plan(&current, Operation::Install).is_err());
    assert!(recover_private(&fixture.root, &fixture.home, &current).is_err());
    assert_eq!(tree_snapshot(&fixture.workspace), before);
}

/// Historical private v3 intent remains strict and recoverable without gaining
/// archive authority. Identical archived edits are reused byte-for-byte on a
/// later install; no duplicate archive effect or silent foreign-slot adoption.
#[test]
fn bootstrap_private_archive_reuses_exact_content_and_keeps_v3_semantics() {
    let fixture = Fixture::new();
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    let accepted = fixture.plan(&current, Operation::Install).unwrap();
    accepted.fixture_interrupt_after(1);
    assert!(accepted.apply().is_err());
    let mut journal: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.journal()).unwrap()).unwrap();
    journal["version"] = 3.into();
    journal["intent"]
        .as_object_mut()
        .unwrap()
        .remove("archives");
    fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
    assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
    let file = fixture.root.join("extensions/mezzanine/index.mjs");
    fs::write(&file, b"same preserved edit").unwrap();
    let first = fixture.plan(&current, Operation::Install).unwrap();
    let key = first.changes[0].path.clone();
    first.apply().unwrap();
    fs::write(&file, b"same preserved edit").unwrap();
    let before = tree_snapshot(&fixture.workspace);
    let second = fixture.plan(&current, Operation::Install).unwrap();
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    assert!(!second.changes.iter().any(|change| change.path == key));
    assert_eq!(
        second.intent.archives.get("extensions/mezzanine/index.mjs"),
        Some(&key)
    );
    second.fixture_interrupt_after(1);
    assert!(second.apply().is_err());
    assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
    assert!(
        fixture
            .plan(&current, Operation::Install)
            .unwrap()
            .changed_paths()
            .is_empty()
    );
}

/// Archive disappearance after confirmed file effects must retain the journal,
/// not falsely certify durable preservation or undo committed artifacts. Exact
/// original intent reconstructs its archive on fresh recovery, then settles once.
#[test]
fn bootstrap_private_archive_post_effect_loss_keeps_original_intent() {
    let fixture = Fixture::new();
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    fixture
        .plan(&current, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    let file = fixture.root.join("extensions/mezzanine/index.mjs");
    fs::write(&file, b"preserve this edit").unwrap();
    let accepted = fixture.plan(&current, Operation::Install).unwrap();
    let logical = accepted.changes[0].path.clone();
    let physical = fixture.receipt("pi").parent().unwrap().join(format!(
        "archive-pi-{}",
        logical.rsplit('/').next().unwrap()
    ));
    *accepted.publisher.before_journal_removal.borrow_mut() = Some(Box::new(move || {
        fs::remove_file(physical).unwrap();
    }));
    let error = accepted.apply().unwrap_err();
    assert!(error.to_string().contains("preservation archive changed"));
    assert!(fixture.journal().exists());
    assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
    let publisher = Publisher::inspect_private(&fixture.root, &fixture.home).unwrap();
    assert_eq!(
        publisher.read(&logical).unwrap().as_deref(),
        Some(b"preserve this edit".as_slice())
    );
    assert!(!fixture.journal().exists());
    assert!(
        fixture
            .plan(&current, Operation::Install)
            .unwrap()
            .changed_paths()
            .is_empty()
    );
}

/// Edited receipt-owned generated helpers preserve exact bytes outside loader
/// paths before replacement/removal. Preview writes nothing; interruption after
/// either archive or file publication retains original intent and converges once.
#[test]
fn bootstrap_private_archive_preserves_helpers_and_recovers_each_edge() {
    for harness in ["pi", "opencode"] {
        let current = crate::integrations::bootstrap::compiled_manifest(harness, None).unwrap();
        let entry = current
            .entries
            .iter()
            .find(|entry| entry.path.ends_with("index.mjs") || entry.path == "plugins/mezzanine.js")
            .unwrap();
        for operation in [Operation::Install, Operation::Uninstall] {
            for boundary in [1, 2] {
                let fixture = Fixture::new();
                fixture
                    .plan(&current, Operation::Install)
                    .unwrap()
                    .apply()
                    .unwrap();
                let file = fixture.root.join(&entry.path);
                fs::write(&file, b"authored generated helper edit").unwrap();
                let before = tree_snapshot(&fixture.workspace);
                let accepted = fixture
                    .plan(&current, operation)
                    .expect("receipt-owned helper edit is preservable");
                assert_eq!(tree_snapshot(&fixture.workspace), before);
                assert!(
                    accepted.changes[0]
                        .path
                        .starts_with("@mez-bootstrap-archive/")
                );
                let archive = accepted.changes[0].path.clone();
                accepted.fixture_interrupt_after(boundary);
                assert!(accepted.apply().is_err());
                let publisher = Publisher::inspect_private(&fixture.root, &fixture.home).unwrap();
                assert_eq!(
                    publisher.read(&archive).unwrap().as_deref(),
                    Some(b"authored generated helper edit".as_slice())
                );
                if boundary == 1 {
                    assert_eq!(fs::read(&file).unwrap(), b"authored generated helper edit");
                }
                fixture.plan(&current, operation).unwrap().apply().unwrap();
                assert!(
                    fixture
                        .plan(&current, operation)
                        .unwrap()
                        .changed_paths()
                        .is_empty()
                );
                assert_eq!(
                    publisher.read(&archive).unwrap().as_deref(),
                    Some(b"authored generated helper edit".as_slice())
                );
                assert_eq!(
                    fs::read(fixture.root.join("authored")).unwrap(),
                    b"preserved"
                );
                if matches!(operation, Operation::Install) {
                    if let crate::integrations::bootstrap::reconciliation::Artifact::File {
                        bytes,
                    } = &entry.artifact
                    {
                        assert_eq!(fs::read(&file).unwrap(), *bytes);
                    }
                } else {
                    assert!(!file.exists());
                }
            }
        }
    }
}

/// A colliding, unsafe or missing required archive cannot permit vendor writes;
/// forged archive bytes/destinations/order or old-version authority are refused.
/// Unowned files and shared Codex whole-file hooks remain strict conflicts.
#[test]
fn bootstrap_private_archive_failure_and_forgery_keep_vendor_bytes() {
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    let path = &current
        .entries
        .iter()
        .find(|entry| entry.path.ends_with("index.mjs"))
        .unwrap()
        .path;
    for fault in [
        "collision",
        "payload",
        "destination",
        "grammar",
        "order",
        "omit",
        "version",
    ] {
        let fixture = Fixture::new();
        fixture
            .plan(&current, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        let file = fixture.root.join(path);
        fs::write(&file, b"edited").unwrap();
        let accepted = fixture.plan(&current, Operation::Install).unwrap();
        let logical = accepted.changes[0].path.clone();
        if fault == "collision" {
            let private = fixture.receipt("pi").parent().unwrap().join(format!(
                "archive-pi-{}",
                logical.rsplit('/').next().unwrap()
            ));
            fs::write(&private, b"foreign archive").unwrap();
            fs::set_permissions(&private, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(accepted.apply().is_err());
            assert_eq!(fs::read(&file).unwrap(), b"edited");
            continue;
        }
        accepted.fixture_interrupt_after(1);
        assert!(accepted.apply().is_err());
        let original = fs::read(fixture.journal()).unwrap();
        let mut journal: serde_json::Value = serde_json::from_slice(&original).unwrap();
        match fault {
            "payload" => journal["changes"][0]["after"] = serde_json::json!([1]),
            "destination" => {
                journal["changes"][0]["path"] =
                    format!("@mez-bootstrap-archive/pi/{}", "0".repeat(64)).into()
            }
            "order" => journal["changes"].as_array_mut().unwrap().swap(0, 1),
            "grammar" => {
                journal["changes"][0]["path"] = "@mez-bootstrap-archive/pi/../escape".into()
            }
            "omit" => {
                let private = fixture.receipt("pi").parent().unwrap().join(format!(
                    "archive-pi-{}",
                    logical.rsplit('/').next().unwrap()
                ));
                fs::remove_file(private).unwrap();
                journal["changes"].as_array_mut().unwrap().remove(0);
            }
            "version" => journal["version"] = 3.into(),
            _ => unreachable!(),
        }
        fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
        let before = tree_snapshot(&fixture.workspace);
        assert!(
            fixture.plan(&current, Operation::Install).is_err(),
            "{fault}"
        );
        assert!(
            recover_private(&fixture.root, &fixture.home, &current).is_err(),
            "{fault}"
        );
        assert_eq!(tree_snapshot(&fixture.workspace), before);
        assert_eq!(fs::read(&file).unwrap(), b"edited");
    }
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(path).parent().unwrap()).unwrap();
    fs::write(fixture.root.join(path), b"foreign unreceipted helper").unwrap();
    assert!(fixture.plan(&current, Operation::Install).is_err());
    let codex = crate::integrations::bootstrap::compiled_manifest("codex", None).unwrap();
    let fixture = Fixture::new();
    fixture
        .plan(&codex, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    fs::write(fixture.root.join("hooks.json"), b"authored shared hooks").unwrap();
    assert!(fixture.plan(&codex, Operation::Install).is_err());
}
