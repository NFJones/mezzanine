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
    build_manifest(helper, 5)
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

/// Builds only current or guarded historical compiled private artifacts.
fn build_manifest(helper: &std::path::Path, revision: u32) -> Result<Manifest> {
    let prefix = "skills/mezzanine-telemetry";
    let module = match revision {
        1 => super::curated_client::historical_claude_module_v1(helper)?,
        2 => super::curated_client::historical_claude_module_v2(helper)?,
        3 => super::curated_client::historical_claude_module_v3(helper)?,
        4 => super::curated_client::historical_claude_module_v4(helper)?,
        5 => super::curated_client::module_source(helper, 10_000)?,
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
        ("hooks/claude_request.mjs", include_bytes!("claude_request.mjs").to_vec()),
    ];
    for (name, source) in super::curated_client::request_accounting_sources() {
        let path = match name {
            "curated_request_accounting.mjs" => "hooks/curated_request_accounting.mjs",
            "request_accounting.mjs" => "hooks/request_accounting.mjs",
            _ => unreachable!("compiled curated source graph has only two enumerated siblings"),
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

    /// The compiled Claude adapter must provide the complete literal mod and
    /// pure siblings at the documented user-root autoload placement, not require
    /// a wrapper, fd3, injected helper environment or authored settings rewrite.
    #[test]
    fn bootstrap_claude_artifact_has_complete_ordinary_autoload_bundle() {
        let manifest = compiled_manifest("claude", None).expect("compiled Claude adapter");
        assert_eq!(manifest.revision, 5);
        assert_eq!(manifest.entries.len(), 8);
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
        std::fs::create_dir(&home).unwrap();
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
        for previous in &history {
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
        std::fs::create_dir(&home).unwrap();
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
                "skills/mezzanine-telemetry/hooks/register.mjs",
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
    /// active closure behavior. Same-helper upgrade to revision5 changes only the
    /// owned module, repeats without edits, and preserves unrelated authors.
    /// Relocated helper bytes cannot select any current compiled history.
    #[test]
    fn bootstrap_claude_artifact_upgrades_guarded_revision3_exactly() {
        let current = compiled_manifest("claude", None).unwrap();
        let history = super::super::compiled_history(&current);
        assert_eq!(
            history.iter().map(|m| m.revision).collect::<Vec<_>>(),
            [4, 3, 2, 1]
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
        std::fs::create_dir(&home).unwrap();
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
                "skills/mezzanine-telemetry/hooks/register.mjs",
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
        std::fs::create_dir(&home).unwrap();
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
        assert_eq!(history.len(), 4);
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
        std::fs::create_dir(&home).unwrap();
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
