//! Claude private mod ownership begins only with version-8 compiled semantics.
//! Fixture roots and source bytes are owned; no vendor, credentials or user
//! settings are discovered or executed by these recovery tests.

use super::*;

/// Every original install/uninstall effect boundary remains idempotently
/// recoverable, while relabelled old/future versions cannot acquire Claude mod
/// ownership. Rejection preserves already confirmed effects and journal bytes.
#[test]
fn bootstrap_claude_private_journals_preserve_version8_authority_and_boundaries() {
    let current = crate::integrations::bootstrap::compiled_manifest("claude", None).unwrap();
    for operation in [Operation::Install, Operation::Uninstall] {
        let sample = Fixture::new();
        if matches!(operation, Operation::Uninstall) {
            sample
                .plan(&current, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
        }
        let effects = sample.plan(&current, operation).unwrap().changes.len();
        for boundary in 1..=effects {
            let fixture = Fixture::new();
            if matches!(operation, Operation::Uninstall) {
                fixture
                    .plan(&current, Operation::Install)
                    .unwrap()
                    .apply()
                    .unwrap();
            }
            fs::write(fixture.root.join("authored"), b"preserved").unwrap();
            let accepted = fixture.plan(&current, operation).unwrap();
            accepted.fixture_interrupt_after(boundary);
            assert!(accepted.apply().is_err());
            let original = fs::read(fixture.journal()).unwrap();
            let journal: serde_json::Value = serde_json::from_slice(&original).unwrap();
            assert_eq!(journal["version"], 8);
            for version in [2, 3, 4, 5, 6, 7, 9] {
                let mut forged = journal.clone();
                forged["version"] = version.into();
                fs::write(fixture.journal(), serde_json::to_vec(&forged).unwrap()).unwrap();
                let before = tree_snapshot(&fixture.workspace);
                assert!(recover_private(&fixture.root, &fixture.home, &current).is_err());
                assert_eq!(tree_snapshot(&fixture.workspace), before);
            }
            fs::write(fixture.journal(), &original).unwrap();
            assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
            assert!(!recover_private(&fixture.root, &fixture.home, &current).unwrap());
            assert_eq!(
                fs::read(fixture.root.join("authored")).unwrap(),
                b"preserved"
            );
            assert!(
                fixture
                    .plan(&current, operation)
                    .unwrap()
                    .changed_paths()
                    .is_empty()
            );
        }
    }
}

/// Exact all-file adoption and receipt-owned edited-module preservation reuse
/// the private engine. Adoption grants no edited-file ownership or authority to
/// arbitrary sibling paths; archives survive interruption before repair commits.
#[test]
fn bootstrap_claude_private_adoption_and_edited_repair_remain_exact() {
    let current = crate::integrations::bootstrap::compiled_manifest("claude", None).unwrap();
    let fixture = Fixture::new();
    for entry in &current.entries {
        let crate::integrations::bootstrap::reconciliation::Artifact::File { bytes } =
            &entry.artifact
        else {
            panic!("Claude private file");
        };
        let path = fixture.root.join(&entry.path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let adopted = fixture.plan(&current, Operation::Install).unwrap();
    assert!(adopted.adoption_pending());
    adopted.apply().unwrap();
    let path = fixture
        .root
        .join("skills/mezzanine-telemetry/hooks/register.mjs");
    fs::write(&path, b"// authored edit\n").unwrap();
    let repair = fixture.plan(&current, Operation::Install).unwrap();
    assert_eq!(
        repair.preserved_paths(),
        vec!["skills/mezzanine-telemetry/hooks/register.mjs"]
    );
    repair.fixture_interrupt_after(1);
    assert!(repair.apply().is_err());
    assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
    assert!(
        fs::read(path)
            .unwrap()
            .starts_with(b"/** Literal main-session")
    );
    assert!(
        fixture
            .plan(&current, Operation::Install)
            .unwrap()
            .changed_paths()
            .is_empty()
    );
}
