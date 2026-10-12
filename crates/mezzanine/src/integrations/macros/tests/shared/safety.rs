//! Shared macro fallback and descriptor policy, with explicit legacy controls.
//!
//! New placement rejects unsafe nodes without silently hardening accepted native
//! paths. Invalid shared documents never hide a valid lower-priority macro.

use super::*;
use crate::integrations::shared_workflow_documents::{self, SharedWorkflowDocument};
use std::os::unix::fs::symlink;

/// Every existing parser or read failure must diagnose and preserve user
/// fallback: malformed metadata, name mismatch, empty/too-many steps, non-UTF8
/// and oversized bytes. A malformed native entry can likewise fall back to a
/// valid shared entry without weakening any document contract.
#[test]
fn macro_shared_invalid_documents_preserve_valid_fallback() {
    for invalid in [
        "malformed",
        "mismatch",
        "empty",
        "many",
        "utf8",
        "oversized",
    ] {
        let base = test_temp_root(invalid);
        let project = base.join("project");
        let user = base.join("user");
        let root = project.join(".agents/macros");
        write_macro(&root, "review", "Shared", &["SHARED"]);
        write_macro(&user.join("macros"), "review", "User fallback", &["USER"]);
        let bytes = match invalid {
            "malformed" => b"not front matter".to_vec(),
            "mismatch" => {
                b"---\nname: different\ndescription: Wrong\n---\n## Steps\n1. WRONG\n".to_vec()
            }
            "empty" => b"---\nname: review\ndescription: Empty\n---\n## Steps\n".to_vec(),
            "many" => format!(
                "---\nname: review\ndescription: Too many\n---\n## Steps\n{}",
                (1..=129)
                    .map(|index| format!("{index}. step\n"))
                    .collect::<String>()
            )
            .into_bytes(),
            "utf8" => vec![0xff],
            "oversized" => vec![b'x'; mez_agent::MAX_MACRO_FILE_BYTES as usize + 1],
            _ => unreachable!(),
        };
        fs::write(root.join("review/MACRO.md"), bytes).unwrap();
        let catalog = discover_macro_catalog(Some(&user), Some(&project));
        assert_eq!(
            catalog.get("review").unwrap().source,
            MacroSource::User,
            "{invalid}"
        );
        assert!(!catalog.diagnostics.is_empty(), "{invalid}");
        fs::remove_dir_all(base).unwrap();
    }
    let project = test_temp_root("native-invalid-fallback");
    write_macro(
        &project.join(".agents/macros"),
        "review",
        "Shared fallback",
        &["SHARED"],
    );
    let native = project.join(".mezzanine/macros/review");
    fs::create_dir_all(&native).unwrap();
    fs::write(native.join("MACRO.md"), "invalid").unwrap();
    let catalog = discover_macro_catalog(None, Some(&project));
    assert_eq!(
        catalog.get("review").unwrap().description,
        "Shared fallback"
    );
    assert_eq!(catalog.diagnostics.len(), 1);
    fs::remove_dir_all(project).unwrap();
}

/// Namespace/root/entry/document swaps to links must fence a retained summary
/// at loading and make fresh discovery diagnose/skip to user fallback. Legacy
/// native file links remain accepted in the separate compatibility control.
#[test]
fn macro_shared_symlink_swaps_reject_retained_summary_and_keep_fallback() {
    for node in [
        ".agents",
        ".agents/macros",
        ".agents/macros/review",
        ".agents/macros/review/MACRO.md",
    ] {
        let base = test_temp_root("shared-link");
        let project = base.join("project");
        let user = base.join("user");
        write_macro(
            &project.join(".agents/macros"),
            "review",
            "Shared",
            &["SHARED"],
        );
        let old = discover_macro_catalog(None, Some(&project))
            .get("review")
            .unwrap()
            .clone();
        write_macro(&user.join("macros"), "review", "User fallback", &["USER"]);
        let path = project.join(node);
        let moved = base.join("moved");
        fs::rename(&path, &moved).unwrap();
        symlink(&moved, &path).unwrap();
        assert!(load_macro_definition(&old).is_err(), "{node}");
        let catalog = discover_macro_catalog(Some(&user), Some(&project));
        assert_eq!(
            catalog.get("review").unwrap().source,
            MacroSource::User,
            "{node}"
        );
        assert!(!catalog.diagnostics.is_empty(), "{node}");
        fs::remove_dir_all(base).unwrap();
    }
}

/// Special roots and direct children cannot masquerade as shared directories;
/// FIFO/socket leaves must reject without blocking or reading payloads. The
/// regular shared grammar also rejects traversal before opening any leaf.
#[test]
fn macro_shared_special_nodes_and_traversal_are_unavailable() {
    for node in [
        ".agents",
        ".agents/macros",
        ".agents/macros/review",
        ".agents/macros/review/MACRO.md",
    ] {
        let project = test_temp_root("shared-special");
        let path = project.join(node);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: name is a terminated pathname in this test's owned directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let catalog = discover_macro_catalog(None, Some(&project));
        assert!(catalog.macros.is_empty(), "{node}");
        assert!(!catalog.diagnostics.is_empty(), "{node}");
        fs::remove_dir_all(project).unwrap();
    }
    let project = test_temp_root("shared-socket");
    let root = project.join(".agents/macros");
    fs::create_dir_all(root.join("review")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(root.join("review/MACRO.md")).unwrap();
    assert!(
        shared_workflow_documents::read(&root, "review", SharedWorkflowDocument::Macro).is_err()
    );
    for name in ["../review", "review/child", "..", ""] {
        assert!(
            shared_workflow_documents::read(&root, name, SharedWorkflowDocument::Macro).is_err()
        );
    }
    drop(_socket);
    fs::remove_file(root.join("review/MACRO.md")).unwrap();
    write_macro(&root, "review", "Valid", &["VALID"]);
    assert!(
        shared_workflow_documents::read(&root, "review", SharedWorkflowDocument::Macro).is_ok()
    );
    fs::create_dir_all(project.join("nested")).unwrap();
    assert!(
        shared_workflow_documents::entry_names(
            &project.join("nested/../.agents/macros"),
            SharedWorkflowDocument::Macro
        )
        .is_err()
    );
    fs::remove_dir_all(project).unwrap();
}

/// The new alias must not smuggle in legacy loader hardening. Native root,
/// entry and file aliases have been accepted; retain each accepted behavior
/// while shared placement enforces its own stronger descendant qualification.
#[test]
fn macro_shared_change_preserves_legacy_native_symlink_acceptance() {
    for node in [
        ".mezzanine",
        ".mezzanine/macros",
        ".mezzanine/macros/review",
        ".mezzanine/macros/review/MACRO.md",
    ] {
        let base = test_temp_root("legacy-links");
        let project = base.join("project");
        write_macro(
            &project.join(".mezzanine/macros"),
            "review",
            "Legacy",
            &["LEGACY"],
        );
        let path = project.join(node);
        let moved = base.join("moved");
        fs::rename(&path, &moved).unwrap();
        symlink(&moved, &path).unwrap();
        let catalog = discover_macro_catalog(None, Some(&project));
        let definition = load_macro_definition(catalog.get("review").unwrap()).unwrap();
        assert_eq!(definition.steps[0].prompt, "LEGACY");
        fs::remove_dir_all(base).unwrap();
    }
}

/// A shared summary is filesystem metadata, not ongoing project authority.
/// Loading for invocation must require the same current approved root and
/// reject a document whose declared name changed after its earlier catalog.
#[test]
fn macro_shared_load_revalidates_current_project_and_name() {
    let project = test_temp_root("shared-current-root");
    write_macro(
        &project.join(".agents/macros"),
        "review",
        "Shared",
        &["SHARED"],
    );
    let old = discover_macro_catalog(None, Some(&project))
        .get("review")
        .unwrap()
        .clone();
    use crate::integrations::macros::load_macro_definition_for_project;
    assert!(load_macro_definition_for_project(&old, Some(&project)).is_ok());
    assert!(load_macro_definition_for_project(&old, None).is_err());
    assert!(load_macro_definition_for_project(&old, Some(&project.join("different"))).is_err());
    fs::write(
        &old.path,
        "---\nname: different\ndescription: Changed\n---\n## Steps\n1. CHANGED\n",
    )
    .unwrap();
    assert!(load_macro_definition_for_project(&old, Some(&project)).is_err());
    fs::remove_dir_all(project).unwrap();
}
