//! Compiled best-effort Codex hooks with exact shared-array member ownership.
//! Authored callback siblings, config, credentials and trust remain untouched.
//! The historical whole-file manifest is frozen independently for receipt/journal
//! qualification. Normal /hooks review is required; ordinary helper activation
//! remains unfinished and installation alone grants no telemetry authority.

use super::{
    installer::Manifest,
    reconciliation::{Artifact, Entry, JsonArrayMember},
};

/// Owned callback members, never ownership of the surrounding shared document.
/// Public bootstrap supplies this manifest; installation never grants hook trust.
pub(crate) fn manifest() -> Manifest {
    let command = r#"if [ -n "${MEZ_CODEX_HELPER:-}" ]; then "$MEZ_CODEX_HELPER" codex-hook; else printf '{}\n'; fi"#;
    Manifest {
        harness: "codex".into(), revision: 2, vendor_version: "best-effort".into(),
        entries: vec![Entry { path: "hooks.json".into(), artifact: Artifact::JsonArrayEntries {
            entries: ["SessionStart", "UserPromptSubmit", "Stop", "Interrupt", "SessionEnd"]
                .into_iter().map(|name| JsonArrayMember {
                    pointer: format!("/hooks/{name}"),
                    value: serde_json::json!({"hooks":[{"type":"command","command":command,"timeout":1}]}),
                }).collect(),
        }}],
    }
}

/// Immutable revision-1 whole-file bytes. Never derive these from current entries
/// or helper commands: later candidate edits must not redefine shipped authority.
pub(super) fn historical_manifest() -> Manifest {
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

/// Projects only the exact frozen whole-file artifact into narrow shared members.
/// Compiled receipt qualification is still caller-owned. Surrounding description
/// and authored additions are not projected or deleted; lookalike files reject.
pub(super) fn historical_shared_ownership(previous: &Artifact) -> crate::Result<Option<Artifact>> {
    let frozen = historical_manifest();
    let Some(Entry {
        artifact: Artifact::File { bytes },
        ..
    }) = frozen.entries.first()
    else {
        return Err(crate::MezError::invalid_state(
            "compiled Codex history unavailable",
        ));
    };
    if previous
        != &(Artifact::File {
            bytes: bytes.clone(),
        })
    {
        return Ok(None);
    }
    let document = super::strict_json::decode(bytes)?;
    let hooks = document
        .get("hooks")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| crate::MezError::invalid_state("compiled Codex hook history unavailable"))?;
    let entries = hooks
        .iter()
        .map(|(name, values)| {
            let values = values
                .as_array()
                .filter(|values| values.len() == 1)
                .ok_or_else(|| {
                    crate::MezError::invalid_state("compiled Codex hook member unavailable")
                })?;
            Ok(JsonArrayMember {
                pointer: format!("/hooks/{name}"),
                value: values[0].clone(),
            })
        })
        .collect::<crate::Result<Vec<_>>>()?;
    super::reconciliation::validate_members(&entries)?;
    Ok(Some(Artifact::JsonArrayEntries { entries }))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Candidate installation preserves authored config, refuses malformed shared
    /// documents and uses the private versioned owner, never an unrecoverable v2
    /// journal. Uninstall preserves the containing shared hooks file and trust.
    #[test]
    fn codex_artifact_repeat_conflict_and_uninstall_preserve_settings() {
        use super::super::installer::{Operation, plan_private};
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!(
            "mez-codex-artifact-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = home.join("vendor");
        std::fs::create_dir(&root).unwrap();
        let plan = |operation| plan_private(&root, &home, &manifest(), operation);
        std::fs::write(root.join("config.toml"), b"[features]\nhooks = false\n").unwrap();
        std::fs::write(root.join("hooks.json"), b"authored hooks").unwrap();
        assert!(plan(Operation::Install).is_err());
        assert!(super::super::installer::plan(&root, &manifest(), Operation::Install).is_err());
        std::fs::remove_file(root.join("hooks.json")).unwrap();
        plan(Operation::Install).unwrap().apply().unwrap();
        assert!(plan(Operation::Install).unwrap().changed_paths().is_empty());
        plan(Operation::Uninstall).unwrap().apply().unwrap();
        assert_eq!(
            std::fs::read(root.join("config.toml")).unwrap(),
            b"[features]\nhooks = false\n"
        );
        assert!(root.join("hooks.json").is_file());
        std::fs::remove_dir_all(home).unwrap();
    }
}
