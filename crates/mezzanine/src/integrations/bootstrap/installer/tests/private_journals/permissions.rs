//! Ordinary OS-authorized vendor directories versus protected installer state.

use super::*;

/// Permitting a shared routing ancestor never permits an unsafe managed Mez
/// subtree. Private boundaries/leaves must still reject other-write/read grants
/// before journal/receipt reads or artifact effects, preserving complete trees.
#[test]
fn bootstrap_shared_config_keeps_owned_private_subtree_fail_closed() {
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    for unsafe_leaf in ["mezzanine", "bootstrap", "namespace"] {
        let fixture = Fixture::new();
        let config = fixture.home.join(".config");
        fs::DirBuilder::new().mode(0o700).create(&config).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o777)).unwrap();
        let root = config.join("opencode");
        plan_private(&root, &fixture.home, &current, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        let state = StateDirectory::inspect(&fixture.home, &File::open(&root).unwrap()).unwrap();
        let path = match unsafe_leaf {
            "mezzanine" => config.join("mezzanine"),
            "bootstrap" => config.join("mezzanine/bootstrap"),
            _ => state.fixture_path().to_path_buf(),
        };
        fs::set_permissions(
            &path,
            fs::Permissions::from_mode(if unsafe_leaf == "mezzanine" {
                0o777
            } else {
                0o755
            }),
        )
        .unwrap();
        let before = tree_snapshot(&fixture.workspace);
        assert!(
            plan_private(&root, &fixture.home, &current, Operation::Install).is_err(),
            "{unsafe_leaf}"
        );
        assert_eq!(tree_snapshot(&fixture.workspace), before);
    }
}

/// Shared writable vendor JSON is eligible by actual OS access, not its mode.
/// Only the exact compiled entry changes; unrelated settings survive install,
/// repeat and uninstall, while ownership evidence remains in private state.
#[test]
fn bootstrap_shared_vendor_document_mode_does_not_replace_entry_authority() {
    use crate::integrations::bootstrap::reconciliation::Artifact;
    let fixture = Fixture::new();
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o777)).unwrap();
    let path = fixture.root.join("settings.json");
    fs::write(&path, br#"{"hooks":{},"authored":{"keep":true}}"#).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    let manifest = Manifest {
        harness: "fixture".into(),
        revision: 1,
        vendor_version: "compiled-fixture".into(),
        entries: vec![Entry {
            path: "settings.json".into(),
            artifact: Artifact::JsonEntry {
                pointer: "/hooks/mezzanine".into(),
                value: serde_json::json!({"observer":true}),
            },
        }],
    };
    fixture
        .plan(&manifest, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["authored"], serde_json::json!({"keep":true}));
    assert_eq!(
        value["hooks"]["mezzanine"],
        serde_json::json!({"observer":true})
    );
    assert!(
        fixture
            .plan(&manifest, Operation::Install)
            .unwrap()
            .changed_paths()
            .is_empty()
    );
    fixture
        .plan(&manifest, Operation::Uninstall)
        .unwrap()
        .apply()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["authored"], serde_json::json!({"keep":true}));
    assert!(value["hooks"].get("mezzanine").is_none());
    assert_eq!(fs::metadata(&fixture.root).unwrap().mode() & 0o777, 0o777);
}

/// A readonly vendor directory is inspectable but its actual publication fails
/// under ordinary non-root OS credentials. No gate invents writability or falls
/// back to another root, and confirmed setup effects are distinct from artifacts.
#[test]
fn bootstrap_vendor_denied_write_reports_actual_os_failure() {
    if rustix::process::geteuid().is_root() {
        return;
    } // DAC-override environments do not qualify this case.
    let fixture = Fixture::new();
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o500)).unwrap();
    let before = tree_snapshot(&fixture.root);
    let accepted = fixture.plan(&current, Operation::Install).unwrap();
    let error = accepted
        .apply()
        .expect_err("OS must deny the required root write");
    assert!(
        error
            .to_string()
            .to_ascii_lowercase()
            .contains("permission denied"),
        "{error}"
    );
    assert_eq!(tree_snapshot(&fixture.root), before);
    assert!(!fixture.receipt("pi").exists() && !fixture.journal().exists());
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700)).unwrap();
}

/// Actual system directory descriptors qualify the removal of exact
/// owner admission without changing system data: another UID's root stays a
/// valid readonly directory witness, with the same incarnation on revalidation.
#[test]
fn bootstrap_directory_witness_accepts_actual_other_owner_without_mutation() {
    let path = fs::canonicalize("/usr").unwrap();
    let owner =
        crate::integrations::bootstrap::root_directory::RootDirectory::inspect(&path).unwrap();
    let metadata = owner.file().unwrap().metadata().unwrap();
    owner.validate(&path).unwrap();
    assert_eq!((metadata.dev(), metadata.ino()), {
        let current = fs::metadata(&path).unwrap();
        (current.dev(), current.ino())
    });
    // Normal user runs exercise UID != effective UID; privileged CI still
    // checks type/identity only and must not claim alternate-owner qualification.
}

/// Ordinary writable vendor roots and already existing extension directories
/// remain usable without normalizing owner/mode metadata. Reinstall/readonly
/// planning/uninstall preserve those directory properties and authored siblings;
/// private ownership stays outside loader paths rather than policing the root.
#[test]
fn bootstrap_vendor_writable_directory_modes_are_not_eligibility() {
    let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
    for mode in [0o700, 0o755, 0o775, 0o777] {
        let fixture = Fixture::new();
        let parents = [
            fixture.root.clone(),
            fixture.root.join("extensions"),
            fixture.root.join("extensions/mezzanine"),
        ];
        fs::create_dir_all(&parents[2]).unwrap();
        for path in &parents {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        }
        let metadata = parents
            .iter()
            .map(|path| {
                let stat = fs::metadata(path).unwrap();
                (stat.dev(), stat.ino(), stat.uid(), stat.gid(), stat.mode())
            })
            .collect::<Vec<_>>();
        let before = tree_snapshot(&fixture.workspace);
        let accepted = fixture
            .plan(&current, Operation::Install)
            .expect("OS-writable vendor metadata cannot add bootstrap eligibility policy");
        assert_eq!(tree_snapshot(&fixture.workspace), before);
        accepted.apply().unwrap();
        fixture
            .plan(&current, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        fixture
            .plan(&current, Operation::Uninstall)
            .unwrap()
            .apply()
            .unwrap();
        for (path, expected) in parents.iter().zip(metadata) {
            let stat = fs::metadata(path).unwrap();
            assert_eq!(
                (stat.dev(), stat.ino(), stat.uid(), stat.gid(), stat.mode()),
                expected
            );
        }
        assert_eq!(
            fs::read(fixture.root.join("authored")).unwrap(),
            b"preserved"
        );
    }
}

/// Shared .config is a routing ancestor, not the private-data leaf. Its mode
/// cannot indirectly gate ordinary OpenCode bootstrap when HOME and the owned
/// Mez subtree retain protection, held-incarnation and no-follow fencing. New
/// private journal/receipt nodes remain private and config metadata unchanged.
#[test]
fn bootstrap_shared_config_ancestor_does_not_gate_private_subtree() {
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    for mode in [0o775, 0o777] {
        let fixture = Fixture::new();
        let config = fixture.home.join(".config");
        fs::DirBuilder::new().mode(mode).create(&config).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(mode)).unwrap();
        let root = config.join("opencode");
        let before = tree_snapshot(&fixture.workspace);
        let accepted = plan_private(&root, &fixture.home, &current, Operation::Install)
            .expect("shared config routing ancestor must not impose vendor admission");
        assert_eq!(tree_snapshot(&fixture.workspace), before);
        accepted.apply().unwrap();
        assert_eq!(fs::metadata(&config).unwrap().mode() & 0o777, mode);
        let state = StateDirectory::inspect(&fixture.home, &File::open(&root).unwrap()).unwrap();
        assert_eq!(
            fs::metadata(state.fixture_path()).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(state.fixture_path().join("ownership-opencode.json"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            plan_private(&root, &fixture.home, &current, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
    }
}
