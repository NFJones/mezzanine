//! Shared skill selector snapshot parity with catalog winner precedence.
//!
//! The snapshot builder receives actor-captured trust roots; this test changes
//! only filesystem placement, not the live trust admission or selector grammar.

use super::*;

/// Selector metadata must include one shared-only workflow and switch to the
/// native winner on duplicate insertion, exactly like listing and invocation.
/// Unknown foreign directories remain inert and no skill body is exposed.
#[test]
fn shared_project_skill_selector_snapshot_uses_same_winner() {
    let root = std::env::temp_dir().join(format!(
        "mez-shared-selector-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    let shared = root.join(".agents/skills/review");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(
        shared.join("SKILL.md"),
        "---\nname: review\ndescription: Shared workflow\n---\nBODY_NOT_METADATA\n",
    )
    .unwrap();
    for native in [false, true] {
        if native {
            let directory = root.join(".mezzanine/skills/review");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("SKILL.md"),
                "---\nname: review\ndescription: Native workflow\n---\nNATIVE_BODY\n",
            )
            .unwrap();
        }
        let candidates = runtime_agent_selector_extra_candidates_from_snapshot(
            vec![],
            None,
            Some(root.clone()),
            None,
            None,
            Default::default(),
        );
        let metadata = format!(
            "{:?}",
            candidates
                .iter()
                .filter(|row| row.command == "$")
                .collect::<Vec<_>>()
        );
        assert_eq!(
            candidates
                .iter()
                .filter(|row| row.command == "$" && row.candidate.value == "$review")
                .count(),
            1
        );
        assert!(metadata.contains(if native {
            "Native workflow (project)"
        } else {
            "Shared workflow (project)"
        }));
        assert!(!metadata.contains("BODY_NOT_METADATA"));
        assert!(!metadata.contains("NATIVE_BODY"));
    }
    std::fs::remove_dir_all(root).unwrap();
}
