//! Built-in parallel issue workflow discovery and managed-copy qualification.
//!
//! These tests inspect workflow documents only. They never spawn the workers or
//! reviewers described by the skill, modify real user settings, or fix issues.

use super::*;

const SKILL_NAME: &str = "fix-issues-parallel";

/// The parallel workflow must be discoverable and loadable without external
/// skill files. Its embedded instructions retain isolated medium workers, large
/// independent reviewers, and the reviewed main-branch integration barrier.
#[test]
fn fix_issues_parallel_builtin_is_discoverable_and_loadable() {
    let catalog = discover_skill_catalog(None, None);
    let summary = catalog
        .get(SKILL_NAME)
        .expect("parallel workflow is built in");
    assert_eq!(summary.source, SkillSource::Builtin);
    assert!(summary.path.starts_with(BUILTIN_SKILL_PATH_PREFIX));
    assert!(
        catalog
            .model_catalog_text()
            .contains("- fix-issues-parallel (builtin)")
    );
    let document = load_skill_document(summary).unwrap();
    let (front_matter, body) = split_skill_front_matter(&document.text).unwrap();
    assert!(front_matter.contains("name: fix-issues-parallel"));
    assert!(summary.description.contains("isolated worktrees"));
    for contract in [
        "Act as the coordinator, not the primary implementer.",
        "`role: worker`, `lifetime: persistent`, `size: medium`",
        "`role: explorer`, `lifetime: persistent`, `size: large`",
        "exact base and head commits",
        "main-branch validation",
        "The main agent (coordinator) owns worktree cleanup",
        "without force",
    ] {
        assert!(
            body.contains(contract),
            "missing workflow contract: {contract}"
        );
    }
}

/// Synchronization must create and refresh the new managed workflow while
/// preserving an unmarked user override byte-for-byte. Trusted project overrides
/// retain their existing precedence over both the user copy and built-in source.
#[test]
fn fix_issues_parallel_sync_refreshes_managed_copies_and_preserves_overrides() {
    let root = test_temp_root("parallel-managed-sync");
    let user_root = root.join("user");
    let project_root = root.join("project");
    let path = user_root.join("skills").join(SKILL_NAME).join("SKILL.md");
    let created = sync_managed_builtin_skills(&user_root).unwrap();
    assert_eq!(
        created
            .entries
            .iter()
            .find(|entry| entry.name == SKILL_NAME)
            .unwrap()
            .status,
        ManagedBuiltinSkillSyncStatus::Created
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        managed_builtin_skill_text(SKILL_NAME)
    );
    fs::write(
        &path,
        "---\nmez_managed_version: 0\nname: fix-issues-parallel\ndescription: Stale managed workflow\n---\n\nOld body.\n",
    )
    .unwrap();
    let refreshed = sync_managed_builtin_skills(&user_root).unwrap();
    assert_eq!(
        refreshed
            .entries
            .iter()
            .find(|entry| entry.name == SKILL_NAME)
            .unwrap()
            .status,
        ManagedBuiltinSkillSyncStatus::ReplacedStale
    );
    let catalog = discover_skill_catalog(Some(&user_root), None);
    let document = load_skill_document(catalog.get(SKILL_NAME).unwrap()).unwrap();
    assert_eq!(document.summary.source, SkillSource::User);
    assert_eq!(document.text, managed_builtin_skill_text(SKILL_NAME));
    write_skill(
        &user_root.join("skills"),
        SKILL_NAME,
        "User parallel workflow",
        "Preserve this custom parallel workflow.",
    );
    let original = fs::read(&path).unwrap();
    let preserved = sync_managed_builtin_skills(&user_root).unwrap();
    assert_eq!(
        preserved
            .entries
            .iter()
            .find(|entry| entry.name == SKILL_NAME)
            .unwrap()
            .status,
        ManagedBuiltinSkillSyncStatus::PreservedOverride
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    write_skill(
        &project_root.join(".mezzanine/skills"),
        SKILL_NAME,
        "Project parallel workflow",
        "Use the trusted project's parallel workflow.",
    );
    let catalog = discover_skill_catalog(Some(&user_root), Some(&project_root));
    let document = load_skill_document(catalog.get(SKILL_NAME).unwrap()).unwrap();
    assert_eq!(document.summary.source, SkillSource::Project);
    assert!(
        document
            .text
            .contains("Use the trusted project's parallel workflow.")
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    fs::remove_dir_all(root).unwrap();
}
