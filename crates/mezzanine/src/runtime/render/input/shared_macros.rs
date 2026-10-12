//! Shared macro selector parity with the existing effective catalog owner.
//!
//! The selector sees the same trusted-root snapshot and per-name winners as
//! listing and invocation; placement never contributes scripts or authority.

use super::*;

/// A shared-only macro is selectable once with project attribution, while
/// native insertion switches that same selector row to the native winner.
/// Catalog step counts/descriptions match and body content stays out of metadata.
#[test]
fn macro_shared_selector_snapshot_uses_same_effective_winner() {
    let root = std::env::temp_dir().join(format!(
        "mez-shared-macro-selector-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    let shared = root.join(".agents/macros/review");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(
        shared.join("MACRO.md"),
        "---\nname: review\ndescription: Shared workflow\n---\n## Steps\n1. BODY_NOT_METADATA\n",
    )
    .unwrap();
    for native in [false, true] {
        if native {
            let directory = root.join(".mezzanine/macros/review");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("MACRO.md"), "---\nname: review\ndescription: Native workflow\n---\n## Steps\n1. NATIVE_BODY\n2. SECOND_BODY\n").unwrap();
        }
        let catalog = crate::integrations::macros::discover_macro_catalog(None, Some(&root));
        let summary = catalog.get("review").unwrap();
        let candidates = runtime_agent_selector_extra_candidates_from_snapshot(
            vec![],
            None,
            Some(root.clone()),
            None,
            None,
            Default::default(),
        );
        let rows = candidates
            .iter()
            .filter(|row| row.command == "#" && row.candidate.value == "#review")
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        let metadata = format!("{:?}", rows[0]);
        assert!(metadata.contains(&format!(
            "{} (project; {} steps)",
            summary.description, summary.step_count
        )));
        assert!(!metadata.contains("BODY_NOT_METADATA"));
        assert!(!metadata.contains("NATIVE_BODY"));
    }
    std::fs::remove_dir_all(root).unwrap();
}
