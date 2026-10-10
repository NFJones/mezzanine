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
    build_manifest(helper, false)
}

/// Exact revision-1 ownership for the same fixed installing executable. No
/// receipt-selected helper path or arbitrary source bytes enter this recipe.
pub(super) fn historical_manifest() -> Option<Manifest> {
    build_manifest(&std::env::current_exe().ok()?, true).ok()
}

/// Builds only current or guarded historical compiled private artifacts.
fn build_manifest(helper: &std::path::Path, historical: bool) -> Result<Manifest> {
    let prefix = "skills/mezzanine-telemetry";
    let module = if historical {
        super::curated_client::historical_claude_module_v1(helper)?
    } else {
        super::curated_client::module_source(helper, 10_000)?
    };
    let mut files = vec![
        (".claude-plugin/plugin.json", b"{\"name\":\"mezzanine-telemetry\",\"version\":\"0.1.0\",\"description\":\"Observational Mezzanine lifecycle and available request accounting\"}\n".to_vec()),
        ("hooks/hooks.json", b"{\"modules\":[\"./register.mjs\"]}\n".to_vec()),
        ("hooks/register.mjs", module.into_bytes()),
        ("hooks/curated_lifetime.mjs", include_bytes!("curated_lifetime.mjs").to_vec()),
        ("hooks/claude_observer.mjs", if historical { include_bytes!("history/claude_observer_v1.mjs").to_vec() } else { include_bytes!("claude_observer.mjs").to_vec() }),
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
        revision: if historical { 1 } else { 2 },
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
        assert_eq!(manifest.revision, 2);
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

    /// Shipped revision1 remains exact compiled authority after active lifecycle
    /// changes. Upgrade is idempotent, authored siblings survive, and altered
    /// historical source or fixed-helper identities cannot select other authority.
    #[test]
    fn bootstrap_claude_artifact_upgrades_guarded_revision1_exactly() {
        let current = compiled_manifest("claude", None).unwrap();
        let history = super::super::compiled_history(&current);
        assert_eq!(history.len(), 1);
        let previous = &history[0];
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
