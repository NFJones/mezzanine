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
pub(crate) fn manifest() -> Option<Manifest> {
    manifest_for_helper(&std::env::current_exe().ok()?)
}

/// Pins only an absolute bounded UTF-8 installing executable. Shell quoting
/// preserves one inert pathname; unavailable evidence cannot select PATH or a
/// launcher environment fallback. This helper reference grants no admission.
fn manifest_for_helper(helper: &std::path::Path) -> Option<Manifest> {
    let helper_text = helper.to_str()?;
    if !helper.is_absolute()
        || helper_text.len() > 4096
        || helper_text.chars().any(char::is_control)
    {
        return None;
    }
    let quoted = shlex::try_quote(helper_text).ok()?;
    let command = format!("test -x {quoted} && exec {quoted} codex-hook || printf '{{}}\\n'");
    Some(Manifest {
        harness: "codex".into(), revision: 4, vendor_version: "best-effort".into(),
        entries: vec![Entry { path: "hooks.json".into(), artifact: Artifact::JsonArrayEntries {
            entries: ["SessionStart", "UserPromptSubmit", "Stop", "Interrupt", "SessionEnd"]
                .into_iter().map(|name| JsonArrayMember {
                    pointer: format!("/hooks/{name}"),
                    value: serde_json::json!({"hooks":[{"type":"command","command":command,"timeout":1}]}),
                }).collect(),
        }}],
    })
}

/// Immutable revision-3 fixed-helper members. The original command and member
/// layout remain independent of current grammar; only the exact installing
/// executable reference is qualified, never an arbitrary receipt-supplied path.
pub(super) fn historical_fixed_manifest() -> Option<Manifest> {
    let helper = std::env::current_exe().ok()?;
    let text = helper.to_str()?;
    if !helper.is_absolute() || text.len() > 4096 || text.chars().any(char::is_control) {
        return None;
    }
    let quoted = shlex::try_quote(text).ok()?;
    let command =
        format!("if [ -x {quoted} ]; then exec {quoted} codex-hook; else printf '{{}}\\n'; fi");
    Some(Manifest {
        harness: "codex".into(), revision: 3, vendor_version: "best-effort".into(),
        entries: vec![Entry { path: "hooks.json".into(), artifact: Artifact::JsonArrayEntries {
            entries: ["SessionStart", "UserPromptSubmit", "Stop", "Interrupt", "SessionEnd"]
                .into_iter().map(|name| JsonArrayMember {
                    pointer: format!("/hooks/{name}"),
                    value: serde_json::json!({"hooks":[{"type":"command","command":command,"timeout":1}]}),
                }).collect(),
        }}],
    })
}

/// Immutable revision-2 shared members, independent of the current fixed helper
/// path or command builder. These exact bytes qualify historical receipts only.
pub(super) fn historical_shared_manifest() -> Manifest {
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

/// Detects exact historical shared members without adopting them. Installing a
/// new command beside an unreceipted old registration would duplicate callbacks;
/// only receipt-qualified reconciliation may remove or replace those members.
pub(super) fn has_historical_shared_member(bytes: &[u8]) -> crate::Result<bool> {
    if bytes.len() > 1024 * 1024 {
        return Err(crate::MezError::invalid_args(
            "bootstrap destination exceeds byte limit",
        ));
    }
    let document = super::strict_json::decode(bytes)?;
    for frozen in [
        Some(historical_shared_manifest()),
        historical_fixed_manifest(),
    ]
    .into_iter()
    .flatten()
    {
        let Artifact::JsonArrayEntries { entries } = &frozen.entries[0].artifact else {
            return Err(crate::MezError::invalid_state(
                "compiled Codex shared history unavailable",
            ));
        };
        if entries.iter().any(|entry| {
            document
                .pointer(&entry.pointer)
                .and_then(serde_json::Value::as_array)
                .is_some_and(|values| values.contains(&entry.value))
        }) {
            return Ok(true);
        }
    }
    Ok(false)
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
    /// Bootstrap must resolve its own fixed executable without a launcher
    /// helper environment or PATH search. Shell parsing must preserve that
    /// absolute path as one inert argv element before the neutral callback.
    #[test]
    fn codex_artifact_command_pins_installer_without_environment() {
        let manifest = super::super::compiled_manifest("codex", None).unwrap();
        assert_eq!(manifest.revision, 4);
        let Artifact::JsonArrayEntries { entries } = &manifest.entries[0].artifact else {
            panic!("Codex must own shared hook members only");
        };
        for entry in entries {
            let command = entry.value["hooks"][0]["command"].as_str().unwrap();
            assert!(!command.contains("MEZ_CODEX_HELPER"));
            let words = shlex::split(command).unwrap();
            let helper = std::env::current_exe()
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            assert_eq!(words[2], helper);
            assert_eq!(words[4], "exec");
            assert_eq!(words[5], helper);
            assert_eq!(words[6], "codex-hook");
        }
    }

    /// Executable references with spaces, quotes and shell metacharacters stay
    /// one literal path in both test and exec. Relative/control/oversized or
    /// non-UTF-8 evidence is unavailable rather than an interpolated fallback.
    #[test]
    fn codex_artifact_helper_reference_is_literal_and_bounded() {
        use std::os::unix::ffi::OsStringExt;
        let helper = std::path::Path::new("/tmp/owned 'quote' $NOT_A_VARIABLE; helper");
        let manifest = manifest_for_helper(helper).unwrap();
        let Artifact::JsonArrayEntries { entries } = &manifest.entries[0].artifact else {
            unreachable!();
        };
        let command = entries[0].value["hooks"][0]["command"].as_str().unwrap();
        let words = shlex::split(command).unwrap();
        assert_eq!(words[2], helper.to_str().unwrap());
        assert_eq!(words[5], helper.to_str().unwrap());
        for path in [
            std::path::PathBuf::from("relative/mez"),
            std::path::PathBuf::from("/tmp/bad\nhelper"),
            std::path::PathBuf::from(format!("/{}", "x".repeat(4096))),
            std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 255])),
        ] {
            assert!(manifest_for_helper(&path).is_none());
        }
    }

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
        let manifest = manifest().unwrap();
        let plan = |operation| plan_private(&root, &home, &manifest, operation);
        std::fs::write(root.join("config.toml"), b"[features]\nhooks = false\n").unwrap();
        std::fs::write(root.join("hooks.json"), b"authored hooks").unwrap();
        assert!(plan(Operation::Install).is_err());
        assert!(super::super::installer::plan(&root, &manifest, Operation::Install).is_err());
        std::fs::remove_file(root.join("hooks.json")).unwrap();
        plan(Operation::Install).unwrap().apply().unwrap();
        assert!(plan(Operation::Install).unwrap().changed_paths().is_empty());
        let before = std::fs::read(root.join("hooks.json")).unwrap();
        let unknown =
            manifest_for_helper(std::path::Path::new("/tmp/unknown-relocated-mez")).unwrap();
        assert!(plan_private(&root, &home, &unknown, Operation::Install).is_err());
        assert_eq!(std::fs::read(root.join("hooks.json")).unwrap(), before);
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
