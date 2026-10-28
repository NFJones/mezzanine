//! Compiled ordinary Claude skills-directory mod artifacts and focused acceptance.
//! Exact owned private files reuse the common installer; no credential/settings,
//! vendor executable, trust review, disabled policy or provider work is changed.

use super::{
    installer::Manifest,
    reconciliation::{Artifact, Entry},
};
use crate::error::Result;

/// Pins only the installing executable, never a vendor payload, PATH search or
/// launcher environment. Missing representable evidence leaves support absent.
pub(super) fn manifest() -> Option<Manifest> {
    manifest_for_helper(&std::env::current_exe().ok()?).ok()
}

/// Compiles the exact private mod graph for a trusted code-owned helper path.
/// The public CLI supplies only current_exe; tests qualify a built fixed helper.
pub(crate) fn manifest_for_helper(helper: &std::path::Path) -> Result<Manifest> {
    build_manifest(helper, 9)
}

/// Exact revision-1 ownership for the same fixed installing executable. No
/// receipt-selected helper path or arbitrary source bytes enter this recipe.
pub(super) fn historical_manifest() -> Option<Manifest> {
    build_manifest(&std::env::current_exe().ok()?, 1).ok()
}

/// Exact revision-2 lifecycle bundle for the same installing executable.
/// Independent template/fingerprint guards cannot accept relocated receipt refs.
pub(super) fn historical_lifecycle_manifest() -> Option<Manifest> {
    build_manifest(&std::env::current_exe().ok()?, 2).ok()
}

/// Exact revision-3 paired-clear bundle before owned negative-proof closure.
/// The old client snapshot cannot borrow authority from current helper bytes.
pub(super) fn historical_paired_clear_manifest() -> Option<Manifest> {
    build_manifest(&std::env::current_exe().ok()?, 3).ok()
}

/// Exact revision-4 negative-proof closure before pending-outcome retention.
/// Independent module/client/step snapshots retain the final shipped authority.
pub(super) fn historical_proof_closure_manifest() -> Option<Manifest> {
    build_manifest(&std::env::current_exe().ok()?, 4).ok()
}

/// Exact revision-5 pending-outcome bundle before bounded settled receipts.
/// Its independent accounting snapshot cannot borrow new active source bytes.
pub(super) fn historical_receipt_manifest() -> Option<Manifest> {
    build_manifest(&std::env::current_exe().ok()?, 5).ok()
}

/// Exact revision-6 before current-epoch request coverage diagnostics.
/// The module matches revision5; the bounded receipt client remains current.
pub(super) fn historical_coverage_manifest() -> Option<Manifest> {
    build_manifest(&std::env::current_exe().ok()?, 6).ok()
}

/// Exact revision7 before accepting bounded response-model metadata labels.
pub(super) fn historical_model_manifest() -> Option<Manifest> {
    build_manifest(&std::env::current_exe().ok()?, 7).ok()
}

/// Exact revision8 before original-parent-correlated child request accounting.
/// It retains the metadata-label projector and the unchanged revision7 entry.
pub(super) fn historical_child_manifest() -> Option<Manifest> {
    build_manifest(&std::env::current_exe().ok()?, 8).ok()
}

/// Builds only current or guarded historical compiled private artifacts.
fn build_manifest(helper: &std::path::Path, revision: u32) -> Result<Manifest> {
    let prefix = "skills/mezzanine-telemetry";
    let module = match revision {
        1 => super::curated_client::historical_claude_module_v1(helper)?,
        2 => super::curated_client::historical_claude_module_v2(helper)?,
        3 => super::curated_client::historical_claude_module_v3(helper)?,
        4 => super::curated_client::historical_claude_module_v4(helper)?,
        5 | 6 => super::curated_client::historical_claude_module_v5(helper)?,
        7 | 8 => super::curated_client::historical_claude_module_v7(helper)?,
        9 => super::curated_client::module_source(helper, 10_000)?,
        _ => {
            return Err(crate::error::MezError::invalid_state(
                "Claude compiled revision unavailable",
            ));
        }
    };
    let mut files = vec![
        (".claude-plugin/plugin.json", b"{\"name\":\"mezzanine-telemetry\",\"version\":\"0.1.0\",\"description\":\"Observational Mezzanine lifecycle and available request accounting\"}\n".to_vec()),
        ("hooks/hooks.json", b"{\"modules\":[\"./register.mjs\"]}\n".to_vec()),
        ("hooks/register.mjs", module.into_bytes()),
        ("hooks/curated_lifetime.mjs", include_bytes!("curated_lifetime.mjs").to_vec()),
        ("hooks/claude_observer.mjs", if revision == 1 { include_bytes!("history/claude_observer_v1.mjs").to_vec() } else { include_bytes!("claude_observer.mjs").to_vec() }),
        ("hooks/claude_request.mjs", if revision <= 7 { include_bytes!("history/claude_request_v7.mjs").to_vec() } else if revision == 8 { include_bytes!("history/claude_request_v8.mjs").to_vec() } else { include_bytes!("claude_request.mjs").to_vec() }),
    ];
    if revision >= 7 {
        files.push((
            "hooks/claude_usage_gap.mjs",
            include_bytes!("claude_usage_gap.mjs").to_vec(),
        ));
    }
    if revision >= 9 {
        files.push((
            "hooks/claude_child_scopes.mjs",
            include_bytes!("claude_child_scopes.mjs").to_vec(),
        ));
    }
    for (name, source) in super::curated_client::request_accounting_sources() {
        let path = match name {
            "curated_request_accounting.mjs" => "hooks/curated_request_accounting.mjs",
            "request_accounting.mjs" => "hooks/request_accounting.mjs",
            _ => unreachable!("compiled curated source graph has only two enumerated siblings"),
        };
        let source = if revision <= 5 && name == "request_accounting.mjs" {
            include_str!("history/request_accounting_pre_receipt_bound.mjs")
        } else {
            source
        };
        files.push((path, source.as_bytes().to_vec()));
    }
    Ok(Manifest {
        harness: "claude".into(),
        revision,
        vendor_version: "best-effort".into(),
        entries: files
            .into_iter()
            .map(|(path, bytes)| Entry {
                path: format!("{prefix}/{path}"),
                artifact: Artifact::File { bytes },
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::{
        compiled_manifest,
        installer::{Operation, plan_private},
        reconciliation::Artifact,
    };

    /// The prior receipt-cache graph remains independently recognizable. Its
    /// exact accounting snapshot upgrades through the current supported private
    /// installer, including only the enumerated owned current-source changes;
    /// authored files and all remaining prior siblings stay preserved.
    #[test]
    fn bootstrap_claude_settled_receipt_upgrade_preserves_previous_graph() {
        use super::super::installer::{Operation, plan_private};
        let current = compiled_manifest("claude", None).unwrap();
        let previous = super::historical_receipt_manifest().unwrap();
        assert_eq!(previous.revision, 5);
        let old_source = previous
            .entries
            .iter()
            .find(|entry| entry.path.ends_with("/request_accounting.mjs"))
            .unwrap();
        assert_eq!(
            old_source.artifact,
            Artifact::File {
                bytes: include_bytes!("history/request_accounting_pre_receipt_bound.mjs").to_vec(),
            }
        );
        assert!(
            super::super::compiled_history(&compiled_manifest("claude", None).unwrap())
                .contains(&previous)
        );
        let base = std::env::temp_dir().join(format!(
            "mez-claude-receipt-cache-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&base).unwrap();
        let home = base.join("home");
        super::super::fixtures::create_private_directory(&home).unwrap();
        let root = home.join(".claude");
        plan_private(&root, &home, &previous, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        std::fs::write(root.join("authored"), b"preserved").unwrap();
        let upgrade = plan_private(&root, &home, &current, Operation::Install).unwrap();
        assert_eq!(
            upgrade.changed_paths(),
            [
                "skills/mezzanine-telemetry/hooks/claude_child_scopes.mjs",
                "skills/mezzanine-telemetry/hooks/claude_request.mjs",
                "skills/mezzanine-telemetry/hooks/claude_usage_gap.mjs",
                "skills/mezzanine-telemetry/hooks/register.mjs",
                "skills/mezzanine-telemetry/hooks/request_accounting.mjs",
                "@mez-bootstrap-receipt/claude",
            ]
        );
        upgrade.apply().unwrap();
        assert!(
            plan_private(&root, &home, &current, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        assert_eq!(std::fs::read(root.join("authored")).unwrap(), b"preserved");
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Revision7 adds only the fixed gap producer and its literal entry wiring.
    /// Exact revision6 has the old module bytes/graph, shares the current bounded
    /// accounting client and cannot acquire new source authority from a receipt.
    #[test]
    fn bootstrap_claude_usage_gap_revision7_preserves_frozen_revision6() {
        use sha2::Digest;
        let current = super::historical_model_manifest().unwrap();
        let previous = super::historical_coverage_manifest().unwrap();
        assert_eq!((current.revision, previous.revision), (7, 6));
        assert_eq!((current.entries.len(), previous.entries.len()), (9, 8));
        for (bytes, expected) in [
            (
                include_bytes!("history/curated_module_claude_v6.mjs").as_slice(),
                "87db659ad7a77fc35178e6cbdf3840028129f056aa7ac1e12de673b484386ae2",
            ),
            (
                include_bytes!("history/claude_request_step_v6.mjs").as_slice(),
                "8e2b9382cfaba48af30e314fe394558d61e549e92aff96862c4984b6b91bd159",
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
        for old in &previous.entries {
            let new = current
                .entries
                .iter()
                .find(|entry| entry.path == old.path)
                .unwrap();
            if old.path.ends_with("/register.mjs") {
                assert_ne!(new.artifact, old.artifact);
            } else {
                assert_eq!(new.artifact, old.artifact);
            }
        }
        assert!(
            super::super::compiled_history(&compiled_manifest("claude", None).unwrap())
                .contains(&previous)
        );
    }

    /// Metadata-label acceptance changes only the current projector. Prior
    /// revision7 retains its exact independent projector and guarded literal
    /// graph, while native selectors and every shared installed sibling stay exact.
    #[test]
    fn bootstrap_claude_model_revision8_preserves_revision7_projector_and_graph() {
        use sha2::Digest;
        let current = super::historical_child_manifest().unwrap();
        let previous = super::historical_model_manifest().unwrap();
        assert_eq!((current.revision, previous.revision), (8, 7));
        assert_eq!((current.entries.len(), previous.entries.len()), (9, 9));
        assert_eq!(
            sha2::Sha256::digest(include_bytes!("history/claude_request_v7.mjs"))
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "32b4b295e16a9d07afffd604aa4ec7500fa417f6945a4f3f6e2e5f0051ebecf4"
        );
        for old in &previous.entries {
            let new = current
                .entries
                .iter()
                .find(|entry| entry.path == old.path)
                .unwrap();
            if old.path.ends_with("/claude_request.mjs") {
                assert_ne!(new.artifact, old.artifact);
            } else {
                assert_eq!(new.artifact, old.artifact);
            }
        }
        assert!(
            super::super::compiled_history(&compiled_manifest("claude", None).unwrap())
                .contains(&previous)
        );
    }

    /// Child correlation adds one private metadata registry and changes only
    /// entry wiring and request identity projection. Exact revision8 remains
    /// recognized so an ordinary upgrade never mistakes owned files for edits.
    #[test]
    fn bootstrap_claude_child_revision9_preserves_revision8_graph() {
        use sha2::Digest;
        let current = compiled_manifest("claude", None).unwrap();
        let previous = super::historical_child_manifest().unwrap();
        assert_eq!((current.revision, previous.revision), (9, 8));
        assert_eq!((current.entries.len(), previous.entries.len()), (10, 9));
        assert_eq!(
            sha2::Sha256::digest(include_bytes!("history/claude_request_v8.mjs"))
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "b9723048e79bb0f2c409a624714f85b2f144ef084010e9eb6568025353e70cb6"
        );
        for old in &previous.entries {
            let new = current
                .entries
                .iter()
                .find(|entry| entry.path == old.path)
                .unwrap();
            if old.path.ends_with("/register.mjs") || old.path.ends_with("/claude_request.mjs") {
                assert_ne!(new.artifact, old.artifact);
            } else {
                assert_eq!(new.artifact, old.artifact);
            }
        }
        assert!(super::super::compiled_history(&current).contains(&previous));
    }

    /// The compiled Claude adapter must provide the complete literal mod and
    /// pure siblings at the documented user-root autoload placement, not require
    /// a wrapper, fd3, injected helper environment or authored settings rewrite.
    #[test]
    fn bootstrap_claude_artifact_has_complete_ordinary_autoload_bundle() {
        let manifest = compiled_manifest("claude", None).expect("compiled Claude adapter");
        assert_eq!(manifest.revision, 9);
        assert_eq!(manifest.entries.len(), 10);
        assert!(manifest.entries.iter().all(|entry| {
            entry.path.starts_with("skills/mezzanine-telemetry/")
                && matches!(entry.artifact, Artifact::File { .. })
        }));
        let entry = manifest
            .entries
            .iter()
            .find(|entry| entry.path.ends_with("/register.mjs"))
            .unwrap();
        let Artifact::File { bytes } = &entry.artifact else {
            panic!("literal module file");
        };
        let source = std::str::from_utf8(bytes).unwrap();
        assert!(source.contains("on('classic.SessionStart'"));
        assert!(source.contains("on('turn.step'"));
        assert!(source.contains("yield* next(e)"));
        assert!(!source.contains("MEZ_CODEX_HELPER"));
        assert!(!source.contains("__MEZ_HELPER__"));
    }

    /// Preview leaves absent roots and private state untouched. Install/repeat,
    /// receipt-owned edited-file repair/archive and uninstall preserve unrelated
    /// user settings and siblings; generated mod files alone are owned.
    #[test]
    fn bootstrap_claude_artifact_preview_repair_and_uninstall_preserve_authors() {
        let base = std::env::temp_dir().join(format!(
            "mez-claude-artifact-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&base).unwrap();
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _directory = Directory(base.clone());
        let home = base.join("home");
        super::super::fixtures::create_private_directory(&home).unwrap();
        let root = home.join(".claude");
        let manifest = compiled_manifest("claude", None).expect("compiled Claude adapter");
        let preview = plan_private(&root, &home, &manifest, Operation::Install).unwrap();
        assert!(!preview.changed_paths().is_empty());
        assert!(!root.exists());
        assert!(!home.join(".config").exists());
        preview.apply().unwrap();
        std::fs::write(
            root.join("settings.json"),
            b"{\"disableAllHooks\":true,\"authored\":true}\n",
        )
        .unwrap();
        let sibling = root.join("skills/authored");
        std::fs::create_dir(&sibling).unwrap();
        std::fs::write(sibling.join("SKILL.md"), b"authored skill\n").unwrap();
        assert!(
            plan_private(&root, &home, &manifest, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        let edited = root.join("skills/mezzanine-telemetry/hooks/register.mjs");
        std::fs::write(&edited, b"// authored generated-file edit\n").unwrap();
        let repair = plan_private(&root, &home, &manifest, Operation::Install).unwrap();
        assert_eq!(
            repair.preserved_paths(),
            vec!["skills/mezzanine-telemetry/hooks/register.mjs"]
        );
        repair.apply().unwrap();
        assert!(
            std::fs::read(&edited)
                .unwrap()
                .starts_with(b"/** Literal main-session")
        );
        plan_private(&root, &home, &manifest, Operation::Uninstall)
            .unwrap()
            .apply()
            .unwrap();
        assert!(!edited.exists());
        assert_eq!(
            std::fs::read(root.join("settings.json")).unwrap(),
            b"{\"disableAllHooks\":true,\"authored\":true}\n"
        );
        assert_eq!(
            std::fs::read(sibling.join("SKILL.md")).unwrap(),
            b"authored skill\n"
        );
    }

    /// Shipped revisions1–4 retain their original step behavior, and revision4
    /// retains its final negative-proof client. Upgrade changes only the owned
    /// module/receipt, preserves authors, repeats cleanly and refuses relocation.
    #[test]
    fn bootstrap_claude_artifact_upgrades_guarded_revision4_exactly() {
        let current = compiled_manifest("claude", None).unwrap();
        let history = super::super::compiled_history(&current);
        let module = |manifest: &super::Manifest| {
            manifest
                .entries
                .iter()
                .find_map(|entry| match &entry.artifact {
                    Artifact::File { bytes } if entry.path.ends_with("/register.mjs") => {
                        Some(String::from_utf8(bytes.clone()).unwrap())
                    }
                    _ => None,
                })
                .unwrap()
        };
        assert!(module(&current).contains("pendingRequestReports"));
        for previous in history.iter().filter(|previous| previous.revision <= 4) {
            assert!(!module(previous).contains("pendingRequestReports"));
        }
        let previous = history
            .iter()
            .find(|manifest| manifest.revision == 4)
            .unwrap();
        assert!(module(previous).contains("observed.terminal === true"));
        let base = std::env::temp_dir().join(format!(
            "mez-claude-v4-upgrade-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&base).unwrap();
        let home = base.join("home");
        super::super::fixtures::create_private_directory(&home).unwrap();
        let root = home.join(".claude");
        plan_private(&root, &home, previous, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        std::fs::write(root.join("authored"), b"preserved").unwrap();
        let upgrade = plan_private(&root, &home, &current, Operation::Install).unwrap();
        assert_eq!(
            upgrade.changed_paths(),
            [
                "skills/mezzanine-telemetry/hooks/claude_child_scopes.mjs",
                "skills/mezzanine-telemetry/hooks/claude_request.mjs",
                "skills/mezzanine-telemetry/hooks/claude_usage_gap.mjs",
                "skills/mezzanine-telemetry/hooks/register.mjs",
                "skills/mezzanine-telemetry/hooks/request_accounting.mjs",
                "@mez-bootstrap-receipt/claude",
            ]
        );
        upgrade.apply().unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("skills/mezzanine-telemetry/hooks/register.mjs"))
                .unwrap(),
            module(&current)
        );
        assert!(
            plan_private(&root, &home, &current, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        assert_eq!(std::fs::read(root.join("authored")).unwrap(), b"preserved");
        let relocated = super::manifest_for_helper(std::path::Path::new("/relocated/mez")).unwrap();
        assert!(super::super::compiled_history(&relocated).is_empty());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Revision3 retains its exact pre-rejection client rather than borrowing
    /// active closure behavior. Same-helper upgrade changes only owned lifecycle
    /// and accounting sources, repeats cleanly, and preserves unrelated authors.
    /// Relocated helper bytes cannot select any current compiled history.
    #[test]
    fn bootstrap_claude_artifact_upgrades_guarded_revision3_exactly() {
        let current = compiled_manifest("claude", None).unwrap();
        let history = super::super::compiled_history(&current);
        assert_eq!(
            history.iter().map(|m| m.revision).collect::<Vec<_>>(),
            [8, 7, 6, 5, 4, 3, 2, 1]
        );
        let previous = history
            .iter()
            .find(|manifest| manifest.revision == 3)
            .unwrap();
        let module = |manifest: &super::Manifest| {
            manifest
                .entries
                .iter()
                .find_map(|entry| {
                    if !entry.path.ends_with("/register.mjs") {
                        return None;
                    }
                    match &entry.artifact {
                        Artifact::File { bytes } => Some(String::from_utf8(bytes.clone()).unwrap()),
                        _ => None,
                    }
                })
                .unwrap()
        };
        assert!(module(previous).contains("clearHandoff"));
        assert!(!module(previous).contains("observed.observed === false"));
        assert!(module(&current).contains("observed.observed === false"));
        let base = std::env::temp_dir().join(format!(
            "mez-claude-v3-upgrade-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&base).unwrap();
        let home = base.join("home");
        super::super::fixtures::create_private_directory(&home).unwrap();
        let root = home.join(".claude");
        plan_private(&root, &home, previous, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        std::fs::write(root.join("authored"), b"preserved").unwrap();
        let upgrade = plan_private(&root, &home, &current, Operation::Install).unwrap();
        assert_eq!(
            upgrade.changed_paths(),
            [
                "skills/mezzanine-telemetry/hooks/claude_child_scopes.mjs",
                "skills/mezzanine-telemetry/hooks/claude_request.mjs",
                "skills/mezzanine-telemetry/hooks/claude_usage_gap.mjs",
                "skills/mezzanine-telemetry/hooks/register.mjs",
                "skills/mezzanine-telemetry/hooks/request_accounting.mjs",
                "@mez-bootstrap-receipt/claude",
            ]
        );
        upgrade.apply().unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("skills/mezzanine-telemetry/hooks/register.mjs"))
                .unwrap(),
            module(&current)
        );
        assert!(
            plan_private(&root, &home, &current, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        assert_eq!(std::fs::read(root.join("authored")).unwrap(), b"preserved");
        let relocated = super::manifest_for_helper(std::path::Path::new("/relocated/mez")).unwrap();
        assert!(super::super::compiled_history(&relocated).is_empty());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Shipped revision2 keeps independently frozen module bytes when paired
    /// clear changes current registration. Same-helper upgrade repairs only the
    /// owned graph, repeats exactly and preserves authored siblings; helper
    /// relocation and edited history never create new compiled authority.
    #[test]
    fn bootstrap_claude_artifact_upgrades_guarded_revision2_exactly() {
        let current = compiled_manifest("claude", None).unwrap();
        let previous = super::super::compiled_history(&current)
            .into_iter()
            .find(|manifest| manifest.revision == 2)
            .unwrap();
        assert_eq!(previous.revision, 2);
        let entry = previous
            .entries
            .iter()
            .find(|entry| entry.path.ends_with("/register.mjs"))
            .unwrap();
        let Artifact::File { bytes } = &entry.artifact else {
            panic!("compiled module file");
        };
        assert!(!String::from_utf8_lossy(bytes).contains("clearHandoff"));
        let base = std::env::temp_dir().join(format!(
            "mez-claude-v2-upgrade-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&base).unwrap();
        let home = base.join("home");
        super::super::fixtures::create_private_directory(&home).unwrap();
        let root = home.join(".claude");
        plan_private(&root, &home, &previous, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        std::fs::write(root.join("authored"), b"preserved").unwrap();
        plan_private(&root, &home, &current, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        assert!(
            std::fs::read_to_string(root.join(&entry.path))
                .unwrap()
                .contains("clearHandoff")
        );
        assert!(
            plan_private(&root, &home, &current, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        assert_eq!(std::fs::read(root.join("authored")).unwrap(), b"preserved");
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Shipped revision1 remains exact compiled authority after active lifecycle
    /// changes. Upgrade is idempotent, authored siblings survive, and altered
    /// historical source or fixed-helper identities cannot select other authority.
    #[test]
    fn bootstrap_claude_artifact_upgrades_guarded_revision1_exactly() {
        let current = compiled_manifest("claude", None).unwrap();
        let history = super::super::compiled_history(&current);
        assert_eq!(history.len(), 8);
        let previous = history
            .iter()
            .find(|manifest| manifest.revision == 1)
            .unwrap();
        assert_eq!(previous.revision, 1);
        let base = std::env::temp_dir().join(format!(
            "mez-claude-upgrade-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&base).unwrap();
        let home = base.join("home");
        super::super::fixtures::create_private_directory(&home).unwrap();
        let root = home.join(".claude");
        plan_private(&root, &home, previous, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        std::fs::write(root.join("authored"), b"preserved").unwrap();
        plan_private(&root, &home, &current, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        assert!(
            plan_private(&root, &home, &current, Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        assert_eq!(std::fs::read(root.join("authored")).unwrap(), b"preserved");
        let mut forged = current.clone();
        forged.revision += 99;
        assert!(super::super::compiled_history(&forged).is_empty());
        std::fs::remove_dir_all(base).unwrap();
    }
}
