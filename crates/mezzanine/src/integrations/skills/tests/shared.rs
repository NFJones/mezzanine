//! Additive shared project skills with native precedence and bounded alias reads.
//!
//! The shared namespace is placement only: it uses the existing parser, project
//! trust and skill source rank, without importing foreign metadata or authority.

use super::*;

/// Shared project skills form a per-name union and override lower-ranked user
/// skills, while native project winners remain unchanged and explicit shared
/// invocation loads the same document. Discovery never creates absent roots.
#[test]
fn shared_project_skill_catalog_preserves_native_precedence_and_union() {
    let base = test_temp_root("shared-union");
    let user = base.join("user");
    let project = base.join("project");
    fs::create_dir(&project).unwrap();
    discover_skill_catalog(Some(&user), Some(&project));
    assert!(!project.join(".agents").exists());
    assert!(!project.join(".mezzanine").exists());
    write_skill(&user.join("skills"), "review", "User", "user");
    write_skill(
        &project.join(".agents/skills"),
        "review",
        "Shared",
        "shared body",
    );
    write_skill(
        &project.join(".agents/skills"),
        "shared-only",
        "Shared only",
        "shared only body",
    );
    write_skill(
        &project.join(".mezzanine/skills"),
        "review",
        "Native",
        "native body",
    );
    let catalog = discover_skill_catalog(Some(&user), Some(&project));
    assert_eq!(catalog.get("review").unwrap().description, "Native");
    let shared = catalog.get("shared-only").unwrap();
    assert_eq!(shared.source, SkillSource::Project);
    assert!(
        load_skill_document(shared)
            .unwrap()
            .text
            .contains("shared only body")
    );
    fs::remove_dir_all(project.join(".mezzanine")).unwrap();
    let catalog = discover_skill_catalog(Some(&user), Some(&project));
    assert_eq!(catalog.get("review").unwrap().description, "Shared");
    fs::remove_dir_all(base).unwrap();
}

/// A namespace symlink cannot turn a trusted project into authority to read an
/// outside tree. Protected shared reads and discovery reject it, but an alias
/// of the explicitly trusted project base remains a supported spelling.
#[test]
fn shared_project_skill_reads_reject_namespace_symlink_escape() {
    use std::os::unix::fs::symlink;
    let base = test_temp_root("shared-namespace");
    let project = base.join("project");
    let outside = base.join("outside");
    fs::create_dir(&project).unwrap();
    write_skill(
        &outside.join("skills"),
        "escaped",
        "Outside",
        "outside body",
    );
    symlink(&outside, project.join(".agents")).unwrap();
    assert!(
        crate::integrations::skills::safe_read::read_shared(
            &project.join(".agents/skills"),
            "escaped"
        )
        .is_err()
    );
    assert!(
        discover_skill_catalog(None, Some(&project))
            .get("escaped")
            .is_none()
    );
    fs::remove_file(project.join(".agents")).unwrap();
    write_skill(&project.join(".agents/skills"), "safe", "Safe", "safe body");
    symlink(&project, base.join("project-alias")).unwrap();
    let catalog = discover_skill_catalog(None, Some(&base.join("project-alias")));
    assert!(
        load_skill_document(catalog.get("safe").unwrap())
            .unwrap()
            .text
            .contains("safe body")
    );
    fs::remove_dir_all(base).unwrap();
}

/// Explicit shared loading revalidates protected descendants rather than using
/// the legacy path reader. A document changed to an outside symlink after
/// discovery cannot leak its content through an already selected summary.
#[test]
fn shared_project_skill_explicit_load_rejects_document_swap() {
    use std::os::unix::fs::symlink;
    let base = test_temp_root("shared-explicit-swap");
    write_skill(
        &base.join(".agents/skills"),
        "review",
        "Shared",
        "safe body",
    );
    let catalog = discover_skill_catalog(None, Some(&base));
    let selected = catalog.get("review").unwrap();
    fs::write(base.join("outside"), "PRIVATE_OUTSIDE").unwrap();
    fs::remove_file(&selected.path).unwrap();
    symlink(base.join("outside"), &selected.path).unwrap();
    assert!(load_skill_document(selected).is_err());
    fs::remove_dir_all(base).unwrap();
}

/// Every new shared descendant is bounded and no-follow: root and entry links,
/// FIFO documents, oversized and non-UTF-8 files and traversal names fail
/// promptly. A valid user-root alias remains supported by the legacy reader.
#[test]
fn shared_project_skill_reads_reject_unsafe_nodes_and_preserve_user_alias() {
    use crate::integrations::skills::safe_read::{MAX_SKILL_BYTES, read, read_shared};
    use std::os::unix::fs::symlink;
    let base = test_temp_root("shared-nodes");
    let root = base.join(".agents/skills");
    write_skill(&root, "review", "Shared", "safe body");
    assert!(read_shared(&root, "../review").is_err());
    let path = root.join("review/SKILL.md");
    fs::write(&path, vec![b'x'; MAX_SKILL_BYTES as usize + 1]).unwrap();
    assert!(read_shared(&root, "review").is_err());
    fs::write(&path, [0xff]).unwrap();
    assert!(read_shared(&root, "review").is_err());
    fs::remove_file(&path).unwrap();
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &path,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .unwrap();
    assert!(read_shared(&root, "review").is_err());
    fs::remove_file(&path).unwrap();
    fs::write(&path, "safe").unwrap();
    fs::rename(root.join("review"), base.join("entry")).unwrap();
    symlink(base.join("entry"), root.join("review")).unwrap();
    assert!(read_shared(&root, "review").is_err());
    fs::rename(&root, base.join("moved")).unwrap();
    symlink(base.join("moved"), &root).unwrap();
    assert!(read_shared(&root, "review").is_err());
    assert!(
        discover_skill_catalog(None, Some(&base))
            .get("review")
            .is_none()
    );
    let user = base.join("user");
    write_skill(&user.join("skills"), "review", "User", "user body");
    symlink(&user, base.join("user-alias")).unwrap();
    assert!(
        read(&base.join("user-alias/skills"), "review")
            .unwrap()
            .contains("user body")
    );
    // Even a configured user base named .agents retains legacy alias semantics.
    symlink(&user, base.join("custom-user-agents")).unwrap();
    let named = base.join("configured");
    fs::create_dir(&named).unwrap();
    symlink(&user, named.join(".agents")).unwrap();
    assert!(
        read(&named.join(".agents/skills"), "review")
            .unwrap()
            .contains("user body")
    );
    fs::remove_dir_all(base).unwrap();
}

/// Invalid native/shared entries fall back per name, not per root. Unknown
/// foreign .agents folders remain inert and never replace a valid user winner.
#[test]
fn shared_project_skill_catalog_skips_invalid_and_foreign_entries() {
    let base = test_temp_root("shared-invalid");
    let user = base.join("user");
    let project = base.join("project");
    write_skill(&user.join("skills"), "review", "User", "user body");
    write_skill(
        &project.join(".agents/skills"),
        "review",
        "Shared",
        "shared body",
    );
    fs::create_dir_all(project.join(".mezzanine/skills/review")).unwrap();
    fs::write(
        project.join(".mezzanine/skills/review/SKILL.md"),
        "malformed",
    )
    .unwrap();
    write_skill(
        &project.join(".agents/commands"),
        "foreign",
        "Foreign",
        "foreign body",
    );
    let catalog = discover_skill_catalog(Some(&user), Some(&project));
    assert_eq!(catalog.get("review").unwrap().description, "Shared");
    assert!(catalog.get("foreign").is_none());
    assert!(!catalog.diagnostics.is_empty());
    fs::write(project.join(".agents/skills/review/SKILL.md"), "malformed").unwrap();
    assert_eq!(
        discover_skill_catalog(Some(&user), Some(&project))
            .get("review")
            .unwrap()
            .description,
        "User"
    );
    fs::remove_dir_all(base).unwrap();
}

/// Shared enumeration is finite and refuses overflow instead of returning a
/// misleading partial catalog. The declared limit is exact and includes all
/// direct non-dot entries, independent of later document parsing or ordering.
#[test]
fn shared_project_skill_enumeration_enforces_direct_entry_limit() {
    let base = test_temp_root("shared-entry-limit");
    let root = base.join(".agents/skills");
    fs::create_dir_all(&root).unwrap();
    for index in 0..4096 {
        fs::create_dir(root.join(format!("entry-{index}"))).unwrap();
    }
    assert_eq!(
        crate::integrations::skills::safe_read::shared_entry_names(&root)
            .unwrap()
            .len(),
        4096
    );
    fs::create_dir(root.join("overflow")).unwrap();
    assert!(crate::integrations::skills::safe_read::shared_entry_names(&root).is_err());
    fs::remove_dir_all(base).unwrap();
}
