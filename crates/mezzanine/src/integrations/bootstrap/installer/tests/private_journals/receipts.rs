//! Protected ownership marker migration, version and compiled-authority controls.

use super::*;

/// Fifteen artifact changes plus private receipt publication and legacy removal
/// are the supported 17-effect boundary. Both receipt edges can interrupt safely
/// and recover through the same caller-owned compiled history. An 18th forged
/// destination is rejected without changes, not silently dropped or over-budget.
#[test]
fn bootstrap_private_receipt_migration_accepts_exact_seventeen_effects() {
    use crate::integrations::bootstrap::reconciliation::Artifact;
    let old = Manifest {
        harness: "fixture".into(),
        revision: 1,
        vendor_version: "compiled-fixture".into(),
        entries: (0..15)
            .map(|index| Entry {
                path: format!("owned-{index:02}"),
                artifact: Artifact::File {
                    bytes: b"old".to_vec(),
                },
            })
            .collect(),
    };
    let mut current = old.clone();
    current.revision = 2;
    for entry in &mut current.entries {
        entry.artifact = Artifact::File {
            bytes: b"new".to_vec(),
        };
    }
    for boundary in [16, 17] {
        let fixture = Fixture::new();
        plan(&fixture.root, &old, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        let make_plan = || {
            plan_with_publisher(
                Publisher::inspect_private(&fixture.root, &fixture.home).unwrap(),
                &current,
                Operation::Install,
                std::slice::from_ref(&old),
            )
        };
        let accepted = make_plan().unwrap();
        assert_eq!(accepted.changes.len(), 17);
        accepted.fixture_interrupt_after(boundary);
        assert!(accepted.apply().is_err());
        let original = fs::read(fixture.journal()).unwrap();
        let mut forged: serde_json::Value = serde_json::from_slice(&original).unwrap();
        forged["changes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"path":"unowned","before":null,"after":[1]}));
        fs::write(fixture.journal(), serde_json::to_vec(&forged).unwrap()).unwrap();
        let before = tree_snapshot(&fixture.workspace);
        assert!(make_plan().is_err());
        assert_eq!(tree_snapshot(&fixture.workspace), before);
        fs::write(fixture.journal(), &original).unwrap();
        make_plan().unwrap().apply().unwrap();
        let receipt: Receipt =
            serde_json::from_slice(&fs::read(fixture.receipt("fixture")).unwrap()).unwrap();
        assert_eq!(receipt.manifest, current);
        assert!(
            !fixture
                .root
                .join("mez-bootstrap-ownership-fixture.json")
                .exists()
        );
        assert!(make_plan().unwrap().changed_paths().is_empty());
        for entry in &current.entries {
            assert_eq!(fs::read(fixture.root.join(&entry.path)).unwrap(), b"new");
        }
    }
}

/// Recognized version-2 journals cannot acquire private placement or separate
/// private/vendor source declarations through newly accepted fields. Each such
/// alteration is refused before state copy/publication, while the exact original
/// journal remains recoverable for its original vendor receipt destination.
#[test]
fn bootstrap_private_receipt_v2_source_field_forgery_is_rejected() {
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    for field in ["receipt_location", "previous_private", "previous_vendor"] {
        let fixture = Fixture::new();
        let accepted = plan(&fixture.root, &current, Operation::Install).unwrap();
        accepted.fixture_interrupt_after(1);
        assert!(accepted.apply().is_err());
        let path = fixture.root.join(".mez-bootstrap-journal");
        let original = fs::read(&path).unwrap();
        let mut forged: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(forged["version"], 2);
        forged["intent"][field] = if field == "receipt_location" {
            "Private".into()
        } else {
            serde_json::to_value(&current).unwrap()
        };
        fs::write(&path, serde_json::to_vec(&forged).unwrap()).unwrap();
        let before = tree_snapshot(&fixture.workspace);
        assert!(
            fixture.plan(&current, Operation::Install).is_err(),
            "{field}"
        );
        assert!(
            recover_private(&fixture.root, &fixture.home, &current).is_err(),
            "{field}"
        );
        assert_eq!(tree_snapshot(&fixture.workspace), before);
        assert!(!fixture.home.join(".config").exists());
        fs::write(&path, &original).unwrap();
        assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
        assert!(
            fixture
                .root
                .join("mez-bootstrap-ownership-pi.json")
                .is_file()
        );
        assert!(!fixture.receipt("pi").exists());
    }
}

/// A current legacy installation needs no artifact rewrite, but its ownership
/// marker must become private before exact legacy removal. Every receipt edge
/// remains journaled and idempotent; readonly preview changes neither tree and
/// marker permissions/manifest are qualified after ordinary recovery.
#[test]
fn bootstrap_private_receipt_migration_edges_are_recoverable_and_private() {
    for harness in ["pi", "opencode"] {
        let current = crate::integrations::bootstrap::compiled_manifest(harness, None).unwrap();
        for boundary in [1, 2] {
            let fixture = Fixture::new();
            plan(&fixture.root, &current, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            let legacy = fixture
                .root
                .join(format!("mez-bootstrap-ownership-{harness}.json"));
            let original = fs::read(&legacy).unwrap();
            let before = tree_snapshot(&fixture.workspace);
            let migrate = fixture.plan(&current, Operation::Install).unwrap();
            assert_eq!(migrate.changes.len(), 2);
            assert_eq!(
                migrate.changes[0].path,
                private_receipt_path(harness).unwrap()
            );
            assert_eq!(
                migrate.changes[1].path,
                format!("mez-bootstrap-ownership-{harness}.json")
            );
            assert_eq!(tree_snapshot(&fixture.workspace), before);
            migrate.fixture_interrupt_after(boundary);
            assert!(migrate.apply().is_err());
            let private = fixture.receipt(harness);
            assert_eq!(fs::read(&private).unwrap(), original);
            assert_eq!(legacy.exists(), boundary == 1);
            let journal: serde_json::Value =
                serde_json::from_slice(&fs::read(fixture.journal()).unwrap()).unwrap();
            assert_eq!(journal["version"], 3);
            assert_eq!(journal["intent"]["receipt_location"], "Private");
            assert_eq!(fs::metadata(&private).unwrap().mode() & 0o777, 0o600);
            assert_eq!(
                fs::metadata(private.parent().unwrap()).unwrap().mode() & 0o777,
                0o700
            );
            let before = tree_snapshot(&fixture.workspace);
            let retry = fixture.plan(&current, Operation::Install).unwrap();
            assert!(retry.recovery_pending());
            assert_eq!(tree_snapshot(&fixture.workspace), before);
            retry.apply().unwrap();
            assert!(!legacy.exists() && !fixture.journal().exists());
            assert_eq!(fs::read(&private).unwrap(), original);
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
            fixture
                .plan(&current, Operation::Uninstall)
                .unwrap()
                .apply()
                .unwrap();
            assert!(!private.exists() && !legacy.exists());
        }
    }
}

/// Every frozen predecessor remains exact compiled authority during receipt
/// relocation and current upgrade. Original version-2 explicit recovery must
/// finish its vendor marker unchanged before a separate private current plan;
/// migration does not reinterpret historical target bytes or omit old paths.
#[test]
fn bootstrap_private_receipts_upgrade_all_history_after_original_v2_recovery() {
    for harness in ["pi", "opencode"] {
        let current = crate::integrations::bootstrap::compiled_manifest(harness, None).unwrap();
        let history = crate::integrations::bootstrap::compiled_history(&current);
        assert_eq!(history.len(), 5);
        for old in history {
            let fixture = Fixture::new();
            let pending = plan(&fixture.root, &old, Operation::Install).unwrap();
            pending.fixture_interrupt_after(1);
            assert!(pending.apply().is_err());
            assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
            let legacy = fixture
                .root
                .join(format!("mez-bootstrap-ownership-{harness}.json"));
            let receipt: Receipt = serde_json::from_slice(&fs::read(&legacy).unwrap()).unwrap();
            assert_eq!(receipt.manifest, old);
            assert!(!fixture.receipt(harness).exists());
            fixture
                .plan(&current, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            let private: Receipt =
                serde_json::from_slice(&fs::read(fixture.receipt(harness)).unwrap()).unwrap();
            assert_eq!(private.manifest, current);
            assert!(!legacy.exists());
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

/// Private receipts cannot mint arbitrary artifact identity or silently override
/// a different legacy marker. Unknown schemas/compiled revisions and cross-root
/// journal identity all reject before new publication, preserving both trees.
#[test]
fn bootstrap_private_receipt_and_journal_forgery_are_non_destructive() {
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    let old = crate::integrations::bootstrap::compiled_history(&current)
        .pop()
        .unwrap();
    for forgery in [
        "receipt-schema",
        "receipt-manifest",
        "copies",
        "journal-version",
        "journal-mode",
        "journal-target",
        "journal-order",
        "journal-traversal",
        "journal-root",
        "omitted-private",
        "omitted-legacy",
    ] {
        let fixture = Fixture::new();
        if forgery.starts_with("receipt") || forgery == "copies" {
            fixture
                .plan(&current, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            if forgery == "copies" {
                fs::write(
                    fixture.root.join("mez-bootstrap-ownership-pi.json"),
                    receipt_bytes(&old).unwrap(),
                )
                .unwrap();
            } else {
                let mut receipt: serde_json::Value =
                    serde_json::from_slice(&fs::read(fixture.receipt("pi")).unwrap()).unwrap();
                if forgery == "receipt-schema" {
                    receipt["schema"] = 999.into();
                } else {
                    receipt["manifest"]["revision"] = 999.into();
                }
                fs::write(fixture.receipt("pi"), serde_json::to_vec(&receipt).unwrap()).unwrap();
            }
            let before = tree_snapshot(&fixture.workspace);
            assert!(
                fixture.plan(&current, Operation::Install).is_err(),
                "{forgery}"
            );
            assert_eq!(tree_snapshot(&fixture.workspace), before);
        } else {
            plan(&fixture.root, &current, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            let mut migrate = fixture.plan(&current, Operation::Install).unwrap();
            // Interrupt after private publication, before legacy removal; the
            // omission case explicitly restores private absence when required.
            migrate.publisher.acquire_lock().unwrap();
            migrate.publisher.stop_after.set(Some(1));
            let intent = serde_json::to_value(&migrate.intent).unwrap();
            assert!(
                migrate
                    .publisher
                    .apply_authorized(migrate.changes.clone(), intent)
                    .is_err()
            );
            drop(migrate);
            let mut journal: serde_json::Value =
                serde_json::from_slice(&fs::read(fixture.journal()).unwrap()).unwrap();
            match forgery {
                "journal-version" => journal["version"] = 2.into(),
                "journal-mode" => journal["intent"]["receipt_location"] = "Vendor".into(),
                "journal-target" => {
                    journal["changes"][0]["path"] = "@mez-bootstrap-receipt/opencode".into()
                }
                "journal-root" => journal["root_inode"] = 0.into(),
                "journal-order" => journal["changes"].as_array_mut().unwrap().reverse(),
                "journal-traversal" => {
                    journal["changes"][0]["path"] = "@mez-bootstrap-receipt/../pi".into()
                }
                "omitted-private" => {
                    fs::remove_file(fixture.receipt("pi")).unwrap();
                    journal["changes"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|change| change["path"] != "@mez-bootstrap-receipt/pi");
                }
                "omitted-legacy" => journal["changes"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|change| change["path"] != "mez-bootstrap-ownership-pi.json"),
                _ => unreachable!(),
            }
            fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
            let before = tree_snapshot(&fixture.workspace);
            assert!(
                preview_recovery_private(&fixture.root, &fixture.home, &current).is_err(),
                "{forgery}"
            );
            assert!(
                fixture.plan(&current, Operation::Install).is_err(),
                "{forgery}"
            );
            assert!(
                recover_private(&fixture.root, &fixture.home, &current).is_err(),
                "{forgery}"
            );
            assert_eq!(tree_snapshot(&fixture.workspace), before);
        }
    }
}
