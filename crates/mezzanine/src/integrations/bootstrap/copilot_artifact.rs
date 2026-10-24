//! Owned metadata-only classic Copilot user hooks, not exporter/policy settings.
//!
//! Official hooks-reference/use-hooks document version1 JSON in COPILOT_HOME/hooks
//! and CLI-only exec/args. A fixed installing executable receives one known hook
//! name; no shell, permission decision, trust, exporter or credential is installed.
//! Classic lifecycle session identity is separately native-qualified at ingress.

use super::{
    installer::Manifest,
    reconciliation::{Artifact, Entry},
};

/// Compiles only the documented user hook slot with exact code-owned bytes.
/// Unrepresentable installing evidence cannot guess PATH or another user root.
pub(super) fn manifest() -> Option<Manifest> {
    build(
        2,
        &[
            "sessionStart",
            "userPromptSubmitted",
            "preToolUse",
            "postToolUse",
            "notification",
            "agentStop",
            "errorOccurred",
            "sessionEnd",
        ],
    )
}

/// Reconstructs exact revision1 bytes independently of new event deployment.
pub(super) fn historical_manifest() -> Option<Manifest> {
    build(1, &["sessionStart", "agentStop", "sessionEnd"])
}

/// Literal reviewed event lists alone determine artifact authority and revision.
fn build(revision: u32, events: &[&str]) -> Option<Manifest> {
    let helper = std::env::current_exe().ok()?;
    let helper_text = helper.to_str()?;
    if !helper.is_absolute()
        || helper_text.len() > 4096
        || helper_text.chars().any(char::is_control)
    {
        return None;
    }
    let mut hooks = serde_json::Map::new();
    for event in events {
        hooks.insert(
            (*event).into(),
            serde_json::json!([{"type":"command","exec":helper_text,
            "args":["copilot-hook",event],"timeoutSec":1}]),
        );
    }
    Some(Manifest {
        harness: "copilot".into(),
        revision,
        vendor_version: "best-effort".into(),
        entries: vec![Entry {
            path: "hooks/mezzanine.json".into(),
            artifact: Artifact::File {
                bytes: (serde_json::json!({"version":1,"hooks":hooks}).to_string() + "\n")
                    .into_bytes(),
            },
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Public bootstrap must supply documented version1 direct-exec user hooks,
    /// with no shell interpolation, prompt/permission output or settings owner.
    /// Installation is not proof of loaded/enrolled or accounted callbacks.
    #[test]
    fn bootstrap_copilot_manifest_pins_only_metadata_user_hooks() {
        let manifest =
            super::super::compiled_manifest("copilot", None).expect("compiled Copilot hooks");
        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(manifest.revision, 2);
        assert_eq!(manifest.entries[0].path, "hooks/mezzanine.json");
        let Artifact::File { bytes } = &manifest.entries[0].artifact else {
            panic!("owned private hook document");
        };
        let document: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(document["version"], 1);
        assert_eq!(document["hooks"].as_object().unwrap().len(), 8);
        for event in [
            "sessionStart",
            "userPromptSubmitted",
            "preToolUse",
            "postToolUse",
            "notification",
            "agentStop",
            "errorOccurred",
            "sessionEnd",
        ] {
            let hook = &document["hooks"][event][0];
            assert_eq!(
                hook["exec"],
                std::env::current_exe().unwrap().to_str().unwrap()
            );
            assert_eq!(hook["args"], serde_json::json!(["copilot-hook", event]));
            assert!(hook.get("bash").is_none());
            assert!(hook.get("env").is_none());
        }
    }

    /// Receipt-qualified edits to the generated private hook file are preserved
    /// outside vendor load paths before repair. Foreign collisions never become
    /// ownership, and authored settings/hooks/credentials are not this artifact.
    #[test]
    fn bootstrap_copilot_private_repair_preserves_edits_and_foreign_collision() {
        use super::super::installer::{Operation, plan_private};
        let home = std::env::temp_dir().join(format!(
            "mez-copilot-owned-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        super::super::fixtures::create_private_directory(&home).unwrap();
        let root = home.join("vendor");
        std::fs::create_dir(&root).unwrap();
        let manifest = manifest().unwrap();
        let plan = |operation| plan_private(&root, &home, &manifest, operation);
        plan(Operation::Install).unwrap().apply().unwrap();
        let file = root.join("hooks/mezzanine.json");
        std::fs::write(&file, b"PRIVATE USER EDIT").unwrap();
        let preview = plan(Operation::Install).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"PRIVATE USER EDIT");
        assert!(!preview.preserved_paths().is_empty());
        preview.apply().unwrap();
        assert!(plan(Operation::Install).unwrap().changed_paths().is_empty());
        plan(Operation::Uninstall).unwrap().apply().unwrap();
        std::fs::write(&file, b"FOREIGN USER HOOK").unwrap();
        assert!(plan(Operation::Install).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"FOREIGN USER HOOK");
        std::fs::remove_dir_all(home).unwrap();
    }

    /// Current lifecycle hooks automatically upgrade exact independent revision1
    /// deployment bytes and receipt. Authored siblings/settings remain untouched;
    /// repeat and uninstall use the current manifest without adopting a foreign file.
    #[test]
    fn bootstrap_copilot_revision1_upgrades_and_repeats() {
        use super::super::installer::{Operation, plan_private};
        let home = std::env::temp_dir().join(format!(
            "mez-copilot-upgrade-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        super::super::fixtures::create_private_directory(&home).unwrap();
        let root = home.join("vendor");
        std::fs::create_dir(&root).unwrap();
        let old = historical_manifest().unwrap();
        let current = manifest().unwrap();
        plan_private(&root, &home, &old, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        std::fs::write(root.join("settings.json"), b"AUTHORED EXPORTER").unwrap();
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
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("hooks/mezzanine.json")).unwrap())
                .unwrap();
        assert_eq!(document["hooks"].as_object().unwrap().len(), 8);
        assert_eq!(
            std::fs::read(root.join("settings.json")).unwrap(),
            b"AUTHORED EXPORTER"
        );
        plan_private(&root, &home, &current, Operation::Uninstall)
            .unwrap()
            .apply()
            .unwrap();
        assert!(!root.join("hooks/mezzanine.json").exists());
        std::fs::remove_dir_all(home).unwrap();
    }
}
