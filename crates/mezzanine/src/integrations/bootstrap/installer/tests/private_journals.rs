//! Private Publisher/installer integration and exact legacy migration fixtures.

use super::*;
use crate::integrations::bootstrap::state_directory::StateDirectory;
use std::fs::File;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};

/// Explicit recovery freezes its exact pre-lock bytes/location, not merely an
/// admissible decoded operation. Changes made during acquisition must preserve
/// journal evidence and the previously committed first file without finishing
/// any remaining artifact/receipt. Fresh reinspection can subsequently settle.
#[test]
fn bootstrap_private_explicit_recovery_rejects_acquisition_snapshot_drift() {
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    for drift in ["bytes", "location"] {
        let fixture = Fixture::new();
        let accepted = fixture.plan(&current, Operation::Install).unwrap();
        accepted.fixture_interrupt_after(1);
        assert!(accepted.apply().is_err());
        let publisher = Publisher::inspect_private(&fixture.root, &fixture.home).unwrap();
        let private = fixture.journal();
        let legacy = fixture.root.join(".mez-bootstrap-journal");
        let path = private.clone();
        let legacy_path = legacy.clone();
        *publisher.before_private_binding.borrow_mut() = Some(Box::new(move || {
            let original = std::fs::read(&path).unwrap();
            if drift == "bytes" {
                let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
                std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            } else {
                std::fs::write(&legacy_path, &original).unwrap();
            }
        }));
        let vendor_before = tree_snapshot(&fixture.root);
        let error = recover_private_owner(
            publisher,
            &current,
            &crate::integrations::bootstrap::compiled_history(&current),
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("inspected recovery changed"));
        assert!(private.exists());
        assert!(
            !fixture
                .root
                .join("mez-bootstrap-ownership-pi.json")
                .exists()
        );
        if drift == "bytes" {
            assert_eq!(tree_snapshot(&fixture.root), vendor_before);
        } else {
            assert_eq!(
                std::fs::read(&legacy).unwrap(),
                std::fs::read(&private).unwrap()
            );
            std::fs::remove_file(&legacy).unwrap();
            assert_eq!(tree_snapshot(&fixture.root), vendor_before);
            std::fs::write(&legacy, std::fs::read(&private).unwrap()).unwrap();
        }
        assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
        assert!(!private.exists() && !legacy.exists());
        assert!(!recover_private(&fixture.root, &fixture.home, &current).unwrap());
    }
}

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

/// A fresh default OpenCode root shares the initially absent .config ancestor
/// A shared ancestor replaced after native creation cannot be adopted by name
/// even when the actual vendor-root inode survives intact. The held creation
/// receipt must disagree with the replacement's descriptor, preserving partial
/// setup truthfully without publishing artifacts or entering legacy fallback.
#[test]
fn bootstrap_private_publisher_created_shared_ancestor_swap_is_rejected() {
    let fixture = Fixture::new();
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    let root = fixture.home.join(".config/opencode");
    let install = plan_private(&root, &fixture.home, &current, Operation::Install).unwrap();
    let home = fixture.home.clone();
    let saved = fixture.workspace.join("saved-config");
    let target = root.clone();
    *install.publisher.before_private_binding.borrow_mut() = Some(Box::new(move || {
        let original = fs::metadata(&target).unwrap();
        fs::rename(home.join(".config"), &saved).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(home.join(".config"))
            .unwrap();
        fs::rename(saved.join("opencode"), &target).unwrap();
        let current = fs::metadata(&target).unwrap();
        assert_eq!(
            (original.dev(), original.ino()),
            (current.dev(), current.ino())
        );
    }));
    let error = install
        .apply()
        .expect_err("swapped created prefix cannot be adopted");
    assert!(error.to_string().contains("without creation proof"));
    assert!(root.is_dir());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    assert!(!fixture.home.join(".config/mezzanine").exists());
}

/// A fresh default OpenCode root shares the initially absent .config ancestor
/// with private HOME state. Its securely witnessed own creation must permit
/// first-apply installation without treating that ancestor as foreign drift;
/// preview stays write-free and no reinspection/retry is needed for success.
#[test]
fn bootstrap_private_publisher_fresh_opencode_home_installs_on_first_apply() {
    let fixture = Fixture::new();
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    let root = fixture.home.join(".config/opencode");
    let before = tree_snapshot(&fixture.workspace);
    let install = plan_private(&root, &fixture.home, &current, Operation::Install).unwrap();
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    install
        .apply()
        .expect("own shared .config creation must not invalidate private binding");
    assert!(root.join("mez-bootstrap-ownership-opencode.json").is_file());
    assert!(
        plan_private(&root, &fixture.home, &current, Operation::Install)
            .unwrap()
            .changed_paths()
            .is_empty()
    );
    assert!(!root.join(".mez-bootstrap-journal").exists());
}

/// HOME/base/vendor absence witnesses must reject stale preparation before
/// materialization or state creation. This includes a swapped managed ancestor
/// whose final base directory survives intact, and an independently appeared
/// vendor tree whose bytes cannot become implicit installation ownership.
#[test]
fn bootstrap_private_publisher_missing_root_rejects_home_and_absence_drift() {
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    for drift in [
        "home",
        "config-appeared",
        "config-swapped",
        "preserved-base",
        "vendor-appeared",
        "vendor-ancestor",
    ] {
        let fixture = Fixture::new();
        let missing = fixture.root.join("absent/vendor");
        if drift == "config-swapped" {
            fs::DirBuilder::new()
                .mode(0o755)
                .create(fixture.home.join(".config"))
                .unwrap();
        } else if drift == "preserved-base" {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(fixture.home.join(".config/mezzanine/bootstrap"))
                .unwrap();
        }
        let inspected =
            plan_private(&missing, &fixture.home, &current, Operation::Install).unwrap();
        match drift {
            "home" => {
                fs::rename(&fixture.home, fixture.workspace.join("saved-home")).unwrap();
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&fixture.home)
                    .unwrap();
            }
            "config-appeared" => fs::create_dir(fixture.home.join(".config")).unwrap(),
            "config-swapped" => {
                fs::rename(
                    fixture.home.join(".config"),
                    fixture.workspace.join("saved-config"),
                )
                .unwrap();
                fs::DirBuilder::new()
                    .mode(0o755)
                    .create(fixture.home.join(".config"))
                    .unwrap();
            }
            "preserved-base" => {
                fs::rename(
                    fixture.home.join(".config/mezzanine"),
                    fixture.workspace.join("saved-mezzanine"),
                )
                .unwrap();
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(fixture.home.join(".config/mezzanine"))
                    .unwrap();
                fs::rename(
                    fixture.workspace.join("saved-mezzanine/bootstrap"),
                    fixture.home.join(".config/mezzanine/bootstrap"),
                )
                .unwrap();
            }
            "vendor-appeared" => {
                fs::create_dir_all(&missing).unwrap();
                fs::write(missing.join("foreign"), b"not installer-owned").unwrap();
            }
            "vendor-ancestor" => {
                fs::rename(&fixture.root, fixture.workspace.join("saved-vendor")).unwrap();
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&fixture.root)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = tree_snapshot(&fixture.workspace);
        assert!(inspected.apply().is_err(), "{drift}");
        assert_eq!(tree_snapshot(&fixture.workspace), before, "{drift}");
    }
}

/// Unbound HOME inspection cannot read or lock another root's namespace. A
/// foreign pending journal and its live private lock remain unchanged while
/// the new actual root binds a different namespace and installs normally.
#[test]
fn bootstrap_private_publisher_missing_root_ignores_other_namespace() {
    let fixture = Fixture::new();
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    let mut foreign =
        StateDirectory::inspect(&fixture.home, &File::open(&fixture.root).unwrap()).unwrap();
    foreign.acquire().unwrap();
    foreign
        .publish("journal.json", None, Some(b"unknown other-root intent"), 64)
        .unwrap();
    let missing = fixture.root.join("absent/vendor");
    let before = tree_snapshot(&fixture.workspace);
    let install = plan_private(&missing, &fixture.home, &current, Operation::Install).unwrap();
    assert!(!install.recovery_pending());
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    install.apply().unwrap();
    let actual = StateDirectory::inspect(&fixture.home, &File::open(&missing).unwrap()).unwrap();
    assert_ne!(actual.fixture_path(), foreign.fixture_path());
    assert_eq!(
        foreign.read("journal.json", 64).unwrap().as_deref(),
        Some(b"unknown other-root intent".as_slice())
    );
    assert!(!actual.fixture_path().join("journal.json").exists());
}

/// A private HOME binding failure may follow already completed directory
/// creation, but cannot erase its captured witness or fall through to legacy
/// publication on reuse. Repeated failed acquisition changes nothing further;
/// restoring the exact original HOME permits safe binding under actual root ID.
#[test]
fn bootstrap_private_publisher_failed_binding_keeps_private_authority() {
    let fixture = Fixture::new();
    let missing = fixture.root.join("absent/vendor");
    let mut publisher = Publisher::inspect_private(&missing, &fixture.home).unwrap();
    let home = fixture.home.clone();
    let moved = fixture.workspace.join("saved-home");
    let saved = moved.clone();
    *publisher.before_private_binding.borrow_mut() = Some(Box::new(move || {
        fs::rename(&home, &saved).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    }));
    assert!(publisher.acquire_lock().is_err());
    assert!(
        missing.is_dir(),
        "materialized directories remain truthful partial effects"
    );
    assert!(!missing.join(".mez-bootstrap-lock").exists());
    assert!(!fixture.home.join(".config").exists());
    let before = tree_snapshot(&fixture.workspace);
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
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    fs::remove_dir(&fixture.home).unwrap();
    fs::rename(&moved, &fixture.home).unwrap();
    publisher.acquire_lock().unwrap();
    assert!(missing.join(".mez-bootstrap-lock").is_file());
    assert!(!missing.join(".mez-bootstrap-journal").exists());
}

/// A missing vendor root remains previewable without creating either tree.
/// Noop uninstall stays absent; install materializes only its captured suffix
/// and binds private state to the actual resulting root descriptor, retaining
/// interrupted private intent and ordinary retry without a hint-derived key.
#[test]
fn bootstrap_private_publisher_missing_root_binds_only_after_creation() {
    let fixture = Fixture::new();
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    let missing = fixture.root.join("absent/vendor");
    let before = tree_snapshot(&fixture.workspace);
    let uninstall = plan_private(&missing, &fixture.home, &current, Operation::Uninstall).unwrap();
    assert!(uninstall.changed_paths().is_empty());
    uninstall.apply().unwrap();
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    let install = plan_private(&missing, &fixture.home, &current, Operation::Install)
        .expect("missing private root must remain previewable");
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    install.publisher.stop_after.set(Some(1));
    assert!(install.apply().is_err());
    let private = StateDirectory::inspect(&fixture.home, &File::open(&missing).unwrap()).unwrap();
    assert!(private.fixture_path().join("journal.json").is_file());
    assert!(!missing.join(".mez-bootstrap-journal").exists());
    let retry = plan_private(&missing, &fixture.home, &current, Operation::Install).unwrap();
    assert!(retry.recovery_pending());
    retry.apply().unwrap();
    assert!(
        plan_private(&missing, &fixture.home, &current, Operation::Install)
            .unwrap()
            .changed_paths()
            .is_empty()
    );
    assert_eq!(
        fs::read(fixture.root.join("authored")).unwrap(),
        b"preserved"
    );
}
