//! Private Publisher/installer integration and exact legacy migration fixtures.

use super::*;
use crate::integrations::bootstrap::state_directory::StateDirectory;
use std::fs::File;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};

/// A journal can change after every artifact/receipt effect is already committed.
/// Final settlement must preserve the changed bytes or location and report the
/// conflict without undoing confirmed files. Foreign bytes remain rejected;
/// restored accepted bytes or identical dual copies can subsequently settle once.
#[test]
fn bootstrap_private_publisher_final_removal_fences_post_effect_journal_drift() {
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    for drift in ["bytes", "location"] {
        let fixture = Fixture::new();
        let accepted = fixture.plan(&current, Operation::Install).unwrap();
        let journal = fixture.journal();
        let legacy = fixture.root.join(".mez-bootstrap-journal");
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let recorded = captured.clone();
        let private_path = journal.clone();
        let legacy_path = legacy.clone();
        *accepted.publisher.before_journal_removal.borrow_mut() = Some(Box::new(move || {
            let original = fs::read(&private_path).unwrap();
            *recorded.borrow_mut() = Some(original.clone());
            if drift == "bytes" {
                fs::write(&private_path, b"foreign final journal").unwrap();
            } else {
                fs::write(&legacy_path, &original).unwrap();
            }
        }));
        let error = accepted
            .apply()
            .expect_err("post-effect journal drift must reject settlement");
        assert!(error.to_string().contains("accepted journal changed"));
        let receipt: Receipt = serde_json::from_slice(
            &fs::read(fixture.root.join("mez-bootstrap-ownership-pi.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt.manifest, current);
        for entry in &current.entries {
            if let crate::integrations::bootstrap::reconciliation::Artifact::File { bytes } =
                &entry.artifact
            {
                assert_eq!(fs::read(fixture.root.join(&entry.path)).unwrap(), *bytes);
            }
        }
        let original = captured.borrow_mut().take().unwrap();
        if drift == "bytes" {
            assert_eq!(fs::read(&journal).unwrap(), b"foreign final journal");
            let before = tree_snapshot(&fixture.workspace);
            assert!(fixture.plan(&current, Operation::Install).is_err());
            assert_eq!(tree_snapshot(&fixture.workspace), before);
            fs::write(&journal, &original).unwrap();
        } else {
            assert_eq!(fs::read(&journal).unwrap(), original);
            assert_eq!(fs::read(&legacy).unwrap(), original);
        }
        fixture
            .plan(&current, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        assert!(!journal.exists() && !legacy.exists());
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

/// Owns a unique physical temporary HOME and separate vendor root. No ambient
/// HOME/config lookup or vendor process is involved; teardown stays fixture-local.
struct Fixture {
    workspace: std::path::PathBuf,
    root: std::path::PathBuf,
    home: std::path::PathBuf,
}

impl Fixture {
    /// Creates private fixture anchors and an unrelated authored vendor sibling.
    fn new() -> Self {
        let workspace = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "mez-private-journals-{}",
                crate::storage::token_usage::new_token_usage_event_id()
            ));
        let root = workspace.join("vendor");
        let home = workspace.join("home");
        for path in [&workspace, &root, &home] {
            fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        fs::write(root.join("authored"), b"preserved").unwrap();
        Self {
            workspace,
            root,
            home,
        }
    }

    /// Obtains the actual private journal path through the storage owner, never
    /// inventing namespace authority from process hints or arbitrary filenames.
    fn journal(&self) -> std::path::PathBuf {
        StateDirectory::inspect(&self.home, &File::open(&self.root).unwrap())
            .unwrap()
            .fixture_path()
            .join("journal.json")
    }

    /// Captures production deterministic private-journal reconciliation.
    fn plan(&self, manifest: &Manifest, operation: Operation) -> Result<Plan> {
        plan_private(&self.root, &self.home, manifest, operation)
    }

    /// Copies bytes into private state only as explicit controlled fault input.
    /// The real planner must still independently admit compiled journal intent.
    fn private_copy(&self, bytes: &[u8]) {
        let mut state =
            StateDirectory::inspect(&self.home, &File::open(&self.root).unwrap()).unwrap();
        state.acquire().unwrap();
        state
            .publish("journal.json", None, Some(bytes), 32 * 1024 * 1024)
            .unwrap();
    }
}

impl Drop for Fixture {
    /// Fixture-only cleanup avoids double panic after a failed test assertion.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.workspace);
    }
}

/// New private transactions publish no vendor-resident journal. Read-only plan
/// leaves both trees identical, interrupted publication retains bounded private
/// intent, and ordinary same-owner retry restores current receipt once while
/// keeping authored siblings. No-op apply performs no state or vendor changes.
#[test]
fn bootstrap_private_publisher_new_journal_and_retry_are_isolated() {
    let fixture = Fixture::new();
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    let before = tree_snapshot(&fixture.workspace);
    let accepted = fixture.plan(&current, Operation::Install).unwrap();
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    accepted.publisher.stop_after.set(Some(1));
    assert!(accepted.apply().is_err());
    assert!(!fixture.root.join(".mez-bootstrap-journal").exists());
    let journal = fixture.journal();
    assert_eq!(fs::metadata(&journal).unwrap().mode() & 0o777, 0o600);
    assert_eq!(
        fs::metadata(journal.parent().unwrap()).unwrap().mode() & 0o777,
        0o700
    );
    let before = tree_snapshot(&fixture.workspace);
    let retry = fixture.plan(&current, Operation::Install).unwrap();
    assert!(retry.recovery_pending());
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    retry.apply().unwrap();
    assert!(!journal.exists());
    let receipt: Receipt = serde_json::from_slice(
        &fs::read(fixture.root.join("mez-bootstrap-ownership-pi.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt.manifest, current);
    assert_eq!(
        fs::read(fixture.root.join("authored")).unwrap(),
        b"preserved"
    );
    let before = tree_snapshot(&fixture.workspace);
    let repeat = fixture.plan(&current, Operation::Install).unwrap();
    assert!(!repeat.recovery_pending());
    assert!(repeat.changed_paths().is_empty());
    repeat.apply().unwrap();
    assert_eq!(tree_snapshot(&fixture.workspace), before);
}

/// Accepted current and oldest historical install/uninstall intent is previewed
/// without private-state creation, copied only after authorization and retained
/// in both locations if the copy edge is interrupted. A fresh normal operation
/// reauthorizes identical dual copies, finishes original intent and requested
/// current reconciliation, preserving unrelated bytes and repeat idempotence.
#[test]
fn bootstrap_private_publisher_migrates_legacy_and_recovers_copy_interruption() {
    for harness in ["pi", "opencode"] {
        let current = crate::integrations::bootstrap::compiled_manifest(harness, None).unwrap();
        let oldest = crate::integrations::bootstrap::compiled_history(&current)
            .pop()
            .unwrap();
        for target in [&current, &oldest] {
            for original in [Operation::Install, Operation::Uninstall] {
                for requested in [Operation::Install, Operation::Uninstall] {
                    let fixture = Fixture::new();
                    if matches!(original, Operation::Uninstall) {
                        plan(&fixture.root, target, Operation::Install)
                            .unwrap()
                            .apply()
                            .unwrap();
                    }
                    let accepted = plan(&fixture.root, target, original).unwrap();
                    accepted.publisher.stop_after.set(Some(1));
                    assert!(accepted.apply().is_err());
                    let legacy = fixture.root.join(".mez-bootstrap-journal");
                    let bytes = fs::read(&legacy).unwrap();
                    let before = tree_snapshot(&fixture.workspace);
                    let migrate = fixture.plan(&current, requested).unwrap();
                    assert!(migrate.recovery_pending());
                    assert_eq!(tree_snapshot(&fixture.workspace), before);
                    assert!(!fixture.home.join(".config").exists());
                    migrate.publisher.stop_after_migration_copy.set(true);
                    assert!(migrate.apply().is_err());
                    let private = fixture.journal();
                    assert_eq!(fs::read(&private).unwrap(), bytes);
                    assert_eq!(fs::read(&legacy).unwrap(), bytes);
                    let before = tree_snapshot(&fixture.workspace);
                    let retry = fixture.plan(&current, requested).unwrap();
                    assert!(retry.recovery_pending());
                    assert_eq!(tree_snapshot(&fixture.workspace), before);
                    retry.apply().unwrap();
                    assert!(!private.exists() && !legacy.exists());
                    let receipt = fixture
                        .root
                        .join(format!("mez-bootstrap-ownership-{harness}.json"));
                    if matches!(requested, Operation::Install) {
                        let receipt: Receipt =
                            serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
                        assert_eq!(receipt.manifest, current);
                    } else {
                        assert!(!receipt.exists());
                    }
                    assert_eq!(
                        fs::read(fixture.root.join("authored")).unwrap(),
                        b"preserved"
                    );
                    assert!(
                        fixture
                            .plan(&current, requested)
                            .unwrap()
                            .changed_paths()
                            .is_empty()
                    );
                }
            }
        }
    }
}

/// Compiled authority is checked before any copy or state creation. Forged
/// schemas/root/target/effects and disagreements between journal copies remain
/// non-destructive errors. Identical bytes in another location cannot silently
/// change an inspected snapshot's source; fresh inspection can resume safely.
#[test]
fn bootstrap_private_publisher_rejects_forgery_disagreement_and_source_drift() {
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    for kind in ["schema", "root", "target", "effect", "disagree", "source"] {
        let fixture = Fixture::new();
        let accepted = plan(&fixture.root, &current, Operation::Install).unwrap();
        accepted.publisher.stop_after.set(Some(1));
        assert!(accepted.apply().is_err());
        let legacy = fixture.root.join(".mez-bootstrap-journal");
        let original = fs::read(&legacy).unwrap();
        if matches!(kind, "disagree" | "source") {
            fixture.private_copy(&original);
            if kind == "disagree" {
                let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
                fs::write(
                    fixture.journal(),
                    serde_json::to_vec_pretty(&value).unwrap(),
                )
                .unwrap();
                let before = tree_snapshot(&fixture.workspace);
                assert!(fixture.plan(&current, Operation::Install).is_err());
                assert_eq!(tree_snapshot(&fixture.workspace), before);
            } else {
                let inspected = fixture.plan(&current, Operation::Install).unwrap();
                fs::remove_file(&legacy).unwrap();
                let before = tree_snapshot(&fixture.workspace);
                assert!(inspected.apply().is_err());
                assert_eq!(tree_snapshot(&fixture.workspace), before);
                fixture
                    .plan(&current, Operation::Install)
                    .unwrap()
                    .apply()
                    .unwrap();
            }
        } else {
            let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
            match kind {
                "schema" => value["version"] = 999.into(),
                "root" => value["root_inode"] = 0.into(),
                "target" => value["intent"]["manifest"]["revision"] = 999.into(),
                "effect" => value["changes"][0]["after"] = serde_json::json!(b"forged".to_vec()),
                _ => unreachable!(),
            }
            fs::write(&legacy, serde_json::to_vec(&value).unwrap()).unwrap();
            let before = tree_snapshot(&fixture.workspace);
            assert!(
                fixture.plan(&current, Operation::Install).is_err(),
                "{kind}"
            );
            assert_eq!(tree_snapshot(&fixture.workspace), before);
            assert!(!fixture.home.join(".config").exists());
        }
    }
}

/// The transitional owner cooperates with both old vendor-root and new private
/// writers. Contention in either domain prevents recovery/publication; a replaced
/// retained legacy lock cannot continue using its old open description. Private
/// directory creation on rejected acquisition is reported as setup, not effects.
#[test]
fn bootstrap_private_publisher_requires_both_live_lock_domains() {
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    for domain in ["legacy", "private", "replaced-legacy"] {
        let fixture = Fixture::new();
        if domain == "legacy" {
            let inspected = fixture.plan(&current, Operation::Install).unwrap();
            let holder = Publisher::open(&fixture.root).unwrap();
            let before = tree_snapshot(&fixture.root);
            assert!(inspected.apply().is_err());
            assert_eq!(tree_snapshot(&fixture.root), before);
            assert!(!fixture.journal().exists());
            drop(holder);
        } else if domain == "private" {
            let mut holder =
                StateDirectory::inspect(&fixture.home, &File::open(&fixture.root).unwrap())
                    .unwrap();
            holder.acquire().unwrap();
            let inspected = fixture.plan(&current, Operation::Install).unwrap();
            let before = tree_snapshot(&fixture.workspace);
            assert!(inspected.apply().is_err());
            assert_eq!(tree_snapshot(&fixture.workspace), before);
            drop(holder);
        } else {
            let mut publisher = Publisher::inspect_private(&fixture.root, &fixture.home).unwrap();
            publisher.acquire_lock().unwrap();
            let lock = fixture.root.join(".mez-bootstrap-lock");
            fs::rename(&lock, fixture.root.join("old-lock")).unwrap();
            File::create(&lock).unwrap();
            fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
            let holder = Publisher::open(&fixture.root).unwrap();
            assert!(publisher.acquire_lock().is_err());
            assert!(
                publisher
                    .apply(vec![Change {
                        path: "owned".into(),
                        before: None,
                        after: Some(b"wrong".to_vec())
                    }])
                    .is_err()
            );
            assert!(!fixture.root.join("owned").exists());
            assert!(!fixture.journal().exists());
            drop(publisher);
            assert!(Publisher::open(&fixture.root).is_err());
            drop(holder);
        }
        fixture
            .plan(&current, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
    }
}

/// This staged private entry point rejects missing vendor roots before HOME
/// state creation; it does not manufacture a namespace from path/absence hints
/// or silently alter the existing public root-materialization contract.
#[test]
fn bootstrap_private_publisher_requires_actual_existing_root() {
    let fixture = Fixture::new();
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    let before = tree_snapshot(&fixture.workspace);
    assert!(
        plan_private(
            &fixture.root.join("absent"),
            &fixture.home,
            &current,
            Operation::Install
        )
        .is_err()
    );
    assert_eq!(tree_snapshot(&fixture.workspace), before);
}
