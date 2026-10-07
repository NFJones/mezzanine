//! Private shared persistent-client artifacts and fixed native helper reference.
//!
//! Adapters include these dependency-free siblings in compiled manifests. The
//! helper path is the installing Mezzanine executable, never a vendor payload,
//! PATH search or launcher environment requirement. Missing executable evidence
//! produces an inert helper reference, not a guessed command or shell fallback.
//! This deployment surface alone does not activate vendor entry points or grant
//! enrollment, and bytes remain governed by exact installer ownership.

use super::reconciliation::{Artifact, Entry};

/// Builds only two enumerated private siblings at a compiled adapter prefix.
/// Versioning/upgrade reconciliation remains owned by the surrounding manifest.
pub(super) fn entries(prefix: &str) -> Vec<Entry> {
    let helper = std::env::current_exe()
        .ok()
        .filter(|path| path.is_absolute())
        .and_then(|path| path.to_str().map(str::to_owned));
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
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shared deployment owns only enumerated private files and pins an explicit
    /// installer executable without consulting vendor/launcher environment.
    #[test]
    fn persistent_client_artifacts_pin_fixed_private_helper() {
        let entries = entries("extensions/mezzanine");
        assert_eq!(entries.len(), 2);
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
        use super::super::installer::{Operation, plan};
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
}
