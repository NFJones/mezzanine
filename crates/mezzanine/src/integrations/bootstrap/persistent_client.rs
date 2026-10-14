//! Private shared persistent-client artifacts and fixed native helper reference.
//!
//! Adapters include these dependency-free siblings in compiled manifests. The
//! helper path is the installing Mezzanine executable, never a vendor payload,
//! PATH search or launcher environment requirement. Missing executable evidence
//! produces an inert helper reference, not a guessed command or shell fallback.
//! This deployment surface alone does not activate vendor entry points or grant
//! enrollment, and bytes remain governed by exact installer ownership.

use super::reconciliation::{Artifact, Entry};

/// Builds only enumerated private siblings at a compiled adapter prefix.
/// Versioning/upgrade reconciliation remains owned by the surrounding manifest.
pub(super) fn entries(prefix: &str) -> Vec<Entry> {
    let executable = std::env::current_exe().ok();
    entries_for_helper(prefix, executable.as_deref())
}

/// Compiles literal helper references from native installing evidence only.
/// This pure boundary permits isolated path qualification without env mutation.
fn entries_for_helper(prefix: &str, executable: Option<&std::path::Path>) -> Vec<Entry> {
    let helper = executable
        .filter(|path| path.is_absolute())
        .and_then(|path| path.to_str().map(str::to_owned))
        .filter(|path| path.len() <= 4096 && !path.chars().any(char::is_control));
    let config = match helper {
        Some(helper) => format!("export const peerHelper = {};\n", serde_json::json!(helper)),
        None => "export const peerHelper = undefined;\n".to_string(),
    };
    vec![
        Entry {
            path: format!("{prefix}/persistent_client.mjs"),
            artifact: Artifact::File {
                bytes: include_bytes!("persistent_client.mjs").to_vec(),
            },
        },
        Entry {
            path: format!("{prefix}/peer_helper.mjs"),
            artifact: Artifact::File {
                bytes: config.into_bytes(),
            },
        },
        Entry {
            path: format!("{prefix}/request_accounting.mjs"),
            artifact: Artifact::File {
                bytes: include_bytes!("request_accounting.mjs").to_vec(),
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed helper references must reject unrepresentable, relative, oversized
    /// or control-bearing evidence rather than guess PATH or emit a callable
    /// unsafe reference. Literal punctuation/Unicode stay inert JSON data.
    #[test]
    fn persistent_client_helper_reference_is_bounded_and_fail_closed() {
        use std::os::unix::ffi::OsStringExt;
        let oversized = format!("/{}", "x".repeat(4096));
        let oversized_unicode = format!("/{}", "λ".repeat(2048));
        let non_utf8 = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 255]));
        for helper in [
            None,
            Some(std::path::Path::new("relative-mez")),
            Some(std::path::Path::new("/unsafe\nmez")),
            Some(std::path::Path::new("/unsafe\tmez")),
            Some(std::path::Path::new("/unsafe\u{85}mez")),
            Some(std::path::Path::new(&oversized)),
            Some(std::path::Path::new(&oversized_unicode)),
            Some(non_utf8.as_path()),
        ] {
            let entries = entries_for_helper("extensions/mezzanine", helper);
            let Artifact::File { bytes } = &entries[1].artifact else {
                panic!("fixed helper file");
            };
            assert_eq!(bytes, b"export const peerHelper = undefined;\n");
        }
        let exact = format!("/{}", "x".repeat(4095));
        let exact_unicode = format!("/{}x", "λ".repeat(2047));
        for text in ["/literal 'quote' $dollar; λ mez", &exact, &exact_unicode] {
            let path = std::path::Path::new(text);
            let entries = entries_for_helper("extensions/mezzanine", Some(path));
            let Artifact::File { bytes } = &entries[1].artifact else {
                panic!("fixed helper file");
            };
            assert_eq!(
                bytes,
                format!("export const peerHelper = {};\n", serde_json::json!(text)).as_bytes()
            );
        }
    }

    /// Current adapter fixtures use isolated test-owned HOME/root witnesses,
    /// never a v2 journal unable to authorize new format artifacts. Historical
    /// authority still comes from the compiled predecessor table.
    fn plan(
        root: &std::path::Path,
        manifest: &super::super::installer::Manifest,
        operation: super::super::installer::Operation,
    ) -> crate::Result<super::super::installer::Plan> {
        super::super::installer::plan_private(root, root, manifest, operation)
    }

    /// Shared deployment owns only enumerated private files and pins an explicit
    /// installer executable without consulting vendor/launcher environment.
    #[test]
    fn persistent_client_artifacts_pin_fixed_private_helper() {
        let entries = entries("extensions/mezzanine");
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0].path,
            "extensions/mezzanine/persistent_client.mjs"
        );
        assert_eq!(entries[1].path, "extensions/mezzanine/peer_helper.mjs");
        let Artifact::File { bytes } = &entries[1].artifact else {
            panic!("private helper reference must be a file");
        };
        let executable = std::env::current_exe().unwrap();
        assert_eq!(
            bytes,
            format!(
                "export const peerHelper = {};\n",
                serde_json::json!(executable.to_str().unwrap())
            )
            .as_bytes()
        );
    }

    /// Exact predecessor upgrades install current private siblings and entry
    /// wiring; arbitrary receipts cannot masquerade as compiled old authority.
    #[test]
    fn persistent_client_artifact_upgrade_recognizes_exact_previous_revision() {
        use super::super::installer::Operation;
        for harness in ["pi", "opencode"] {
            let current = super::super::compiled_manifest(harness, None).unwrap();
            let previous = super::super::compiled_history(&current).pop().unwrap();
            assert!(super::super::compiled_predecessor_matches(&previous));
            let mut forged = previous.clone();
            let Artifact::File { bytes } = &mut forged.entries[0].artifact else {
                panic!("historical entry must be a file");
            };
            bytes.push(0);
            assert!(!super::super::compiled_predecessor_matches(&forged));
            let root = std::env::temp_dir().join(format!(
                "mez-client-upgrade-{}",
                crate::storage::token_usage::new_token_usage_event_id()
            ));
            std::fs::create_dir(&root).unwrap();
            plan(&root, &previous, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            plan(&root, &current, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            assert!(
                plan(&root, &current, Operation::Install)
                    .unwrap()
                    .changed_paths()
                    .is_empty()
            );
            assert!(
                root.join(if harness == "pi" {
                    "extensions/mezzanine/persistent_client.mjs"
                } else {
                    "plugins/mezzanine/persistent_client.mjs"
                })
                .is_file()
            );
            let mut foreign = current.clone();
            foreign.revision += 99;
            assert!(super::super::compiled_history(&foreign).is_empty());
            std::fs::remove_dir_all(root).unwrap();
            let immediate = super::super::compiled_history(&current).remove(0);
            let root = std::env::temp_dir().join(format!(
                "mez-client-immediate-upgrade-{}",
                crate::storage::token_usage::new_token_usage_event_id()
            ));
            std::fs::create_dir(&root).unwrap();
            plan(&root, &immediate, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            plan(&root, &current, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            assert!(
                plan(&root, &current, Operation::Install)
                    .unwrap()
                    .changed_paths()
                    .is_empty()
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    /// A shared-client byte revision must preserve every compiled historical
    /// upgrade, not just the oldest or immediate predecessor. Historical client
    /// data stays independent of active source and upgrade touches no authored
    /// sibling configuration. Pi13 retains Pi12's unchanged resume transport,
    /// with Pi11 and OpenCode10 independently frozen before resume support.
    /// Revision8 request/transport bytes and every older predecessor remain exact.
    #[test]
    fn persistent_client_artifact_upgrade_retains_all_frozen_predecessors() {
        use super::super::installer::Operation;
        for harness in ["pi", "opencode"] {
            let current = super::super::compiled_manifest(harness, None).unwrap();
            assert_eq!(current.revision, if harness == "pi" { 13 } else { 11 });
            let history = super::super::compiled_history(&current);
            assert_eq!(history.len(), if harness == "pi" { 11 } else { 22 });
            assert!(history.iter().any(|manifest| manifest.revision == 8));
            let immediate = &history[0];
            assert_eq!(immediate.revision + 1, current.revision);
            assert_eq!(immediate.revision, if harness == "pi" { 12 } else { 10 });
            let frozen = immediate
                .entries
                .iter()
                .find(|entry| entry.path.ends_with("/persistent_client.mjs"))
                .unwrap();
            assert_eq!(
                frozen.artifact,
                Artifact::File {
                    bytes: if harness == "pi" {
                        include_bytes!("persistent_client.mjs").to_vec()
                    } else {
                        include_bytes!("history/persistent_client_pre_resume.mjs").to_vec()
                    }
                }
            );
            if harness == "pi" {
                assert_eq!(history[1].revision, 11);
                let pre_resume = history[1].entries.iter()
                    .find(|entry| entry.path.ends_with("/persistent_client.mjs")).unwrap();
                assert_eq!(pre_resume.artifact, Artifact::File {
                    bytes: include_bytes!("history/persistent_client_pre_resume.mjs").to_vec()
                });
                assert!(history.iter().map(|manifest| manifest.revision).eq((2..=12).rev()));
            }
            let request = immediate
                .entries
                .iter()
                .find(|entry| entry.path.ends_with("/request_accounting.mjs"))
                .unwrap();
            assert_eq!(
                request.artifact,
                Artifact::File {
                    bytes: include_bytes!("request_accounting.mjs").to_vec()
                }
            );
            use sha2::Digest;
            assert_eq!(
                sha2::Sha256::digest(include_bytes!("history/persistent_client_pre_resume.mjs"))
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
                "188bb22dcd6c9199807ab6a7930d116f0f0dfd12afa02c2d2700a867fdfe6f15"
            );
            assert_eq!(
                sha2::Sha256::digest(include_bytes!("request_accounting.mjs"))
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
                "43788869b5404f5a098513da7bba66463fd084731b7bd6f76bd9809eff134053"
            );
            for previous in history {
                let root = std::env::temp_dir().join(format!(
                    "mez-client-frozen-upgrade-{}",
                    crate::storage::token_usage::new_token_usage_event_id()
                ));
                std::fs::create_dir(&root).unwrap();
                std::fs::write(root.join("authored.json"), b"{\"authored\":true}\n").unwrap();
                super::super::installer::fixture_seed_nested_layout(&root, &previous);
                plan(&root, &previous, Operation::Install)
                    .unwrap()
                    .apply()
                    .unwrap();
                plan(&root, &current, Operation::Install)
                    .unwrap()
                    .apply()
                    .unwrap();
                assert!(
                    plan(&root, &current, Operation::Install)
                        .unwrap()
                        .changed_paths()
                        .is_empty()
                );
                assert_eq!(
                    std::fs::read(root.join("authored.json")).unwrap(),
                    b"{\"authored\":true}\n"
                );
                let client = current
                    .entries
                    .iter()
                    .find(|entry| entry.path.ends_with("/persistent_client.mjs"))
                    .unwrap();
                let Artifact::File { bytes } = &client.artifact else {
                    panic!("shared client must be a file");
                };
                assert_eq!(std::fs::read(root.join(&client.path)).unwrap(), *bytes);
                let request = current
                    .entries
                    .iter()
                    .find(|entry| entry.path.ends_with("/request_accounting.mjs"))
                    .unwrap();
                let Artifact::File { bytes } = &request.artifact else {
                    panic!("request accounting must be a file");
                };
                assert_eq!(std::fs::read(root.join(&request.path)).unwrap(), *bytes);
                std::fs::remove_dir_all(root).unwrap();
            }
        }
    }
}
