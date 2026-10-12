//! Existing-format shared macro catalog precedence and safe-read regressions.
//!
//! Shared placement is additive project metadata, not foreign workflow import.
//! These fixtures retain native authoring/legacy reads while qualifying the new
//! namespace and its documents independently of runtime execution authority.

use super::*;

mod safety;

/// Shared-only discovery must reuse the existing parser and load its exact
/// multiline ordered prompts without creating native roots or auxiliary files.
#[test]
fn macro_shared_only_discovers_existing_format_without_mutation() {
    let project = test_temp_root("shared-only");
    let root = project.join(".agents/macros");
    write_macro(
        &root,
        "review",
        "Shared review",
        &["Inspect shared evidence.\n   Preserve exact continuation."],
    );
    fs::create_dir_all(project.join(".agents/commands")).unwrap();
    fs::write(project.join(".agents/commands/ignored.md"), "not a macro").unwrap();
    for path in [
        ".agents/agents/worker.md",
        ".agents/config.toml",
        ".agents/instructions/AGENTS.md",
        ".agents/macros/review/auxiliary.sh",
    ] {
        let path = project.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "AUXILIARY_MUST_STAY_INERT").unwrap();
    }
    let catalog = discover_macro_catalog(None, Some(&project));
    assert_eq!(catalog.names(), ["review"]);
    let summary = catalog.get("review").unwrap();
    assert_eq!(summary.source, MacroSource::Project);
    assert_eq!(summary.path, root.join("review/MACRO.md"));
    let loaded = load_macro_definition(summary).unwrap();
    assert_eq!(loaded.steps.len(), 1);
    assert!(!loaded.text.contains("AUXILIARY_MUST_STAY_INERT"));
    assert!(
        loaded.steps[0]
            .prompt
            .contains("Preserve exact continuation.")
    );
    assert!(!project.join(".mezzanine").exists());
    fs::remove_dir_all(project).unwrap();
}

/// Per-name union is independent of directory creation order. Native project
/// beats shared project, shared beats user, and disjoint names remain sorted;
/// collisions diagnose rather than changing scope or execution policy.
#[test]
fn macro_shared_union_preserves_native_winner_and_user_precedence() {
    let base = test_temp_root("shared-union");
    let user = base.join("user");
    let project = base.join("project");
    let native = project.join(".mezzanine/macros");
    let shared = project.join(".agents/macros");
    write_macro(&shared, "same", "Shared same", &["SHARED SAME"]);
    write_macro(&shared, "shared", "Shared winner", &["SHARED WINNER"]);
    write_macro(
        &user.join("macros"),
        "shared",
        "User shadow",
        &["USER SHADOW"],
    );
    write_macro(&native, "same", "Native winner", &["NATIVE WINNER"]);
    write_macro(&native, "native", "Native only", &["NATIVE ONLY"]);
    let catalog = discover_macro_catalog(Some(&user), Some(&project));
    assert_eq!(catalog.names(), ["native", "same", "shared"]);
    assert_eq!(
        catalog.get("same").unwrap().path,
        native.join("same/MACRO.md")
    );
    assert_eq!(
        catalog.get("shared").unwrap().path,
        shared.join("shared/MACRO.md")
    );
    assert_eq!(catalog.diagnostics.len(), 2);
    assert_eq!(discover_macro_catalog(Some(&user), Some(&project)), catalog);
    fs::remove_dir_all(base).unwrap();
}
