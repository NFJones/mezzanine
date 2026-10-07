//! Compiled best-effort Codex hooks with exact whole-file ownership.
//! An existing authored hooks.json conflicts rather than being replaced; other
//! config, credentials and trust remain untouched. Normal /hooks review is
//! required. Parent chooses helper binary and private fd; no token is installed.

use super::{
    installer::Manifest,
    reconciliation::{Artifact, Entry},
};

/// Owned callback configuration for an explicit unused/receipted Codex root.
/// Public bootstrap supplies this manifest; installation never grants hook trust.
pub(crate) fn manifest() -> Manifest {
    let command = r#"if [ -n "${MEZ_CODEX_HELPER:-}" ]; then "$MEZ_CODEX_HELPER" codex-hook; else printf '{}\n'; fi"#;
    let mut hooks = serde_json::Map::new();
    for name in [
        "SessionStart",
        "UserPromptSubmit",
        "Stop",
        "Interrupt",
        "SessionEnd",
    ] {
        hooks.insert(
            name.into(),
            serde_json::json!([{"hooks":[{"type":"command","command":command,"timeout":1}]}]),
        );
    }
    Manifest {harness:"codex".into(),revision:1,vendor_version:"best-effort".into(),entries:vec![Entry{path:"hooks.json".into(),artifact:Artifact::File{bytes:(serde_json::json!({"description":"Mezzanine observational lifecycle; review with /hooks","hooks":hooks}).to_string()+"\n").into_bytes()}}]}
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Candidate installation preserves authored config and refuses foreign
    /// hooks instead of merging unknown ownership or bypassing trust review.
    #[test]
    fn codex_artifact_repeat_conflict_and_uninstall_preserve_settings() {
        use super::super::installer::{Operation, plan};
        let root = std::env::temp_dir().join(format!(
            "mez-codex-artifact-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("config.toml"), b"[features]\nhooks = false\n").unwrap();
        std::fs::write(root.join("hooks.json"), b"authored hooks").unwrap();
        assert!(plan(&root, &manifest(), Operation::Install).is_err());
        std::fs::remove_file(root.join("hooks.json")).unwrap();
        plan(&root, &manifest(), Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        assert!(
            plan(&root, &manifest(), Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        plan(&root, &manifest(), Operation::Uninstall)
            .unwrap()
            .apply()
            .unwrap();
        assert_eq!(
            std::fs::read(root.join("config.toml")).unwrap(),
            b"[features]\nhooks = false\n"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
