//! Focused persisted context-document storage regressions.

use super::*;

fn store(name: &str) -> (ContextDocumentStore, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "mez-context-documents-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    (
        ContextDocumentStore::new(root.join("documents.sqlite")),
        root,
    )
}

#[test]
fn context_documents_select_enabled_global_then_project_deterministically() {
    let (store, root) = store("selection");
    let project = "/repo/a";
    let project_document = store
        .create(
            ContextDocumentScope::Project {
                root: project.to_string(),
            },
            "Project".to_string(),
            "project context".to_string(),
            true,
            10,
        )
        .unwrap();
    let global_document = store
        .create(
            ContextDocumentScope::Global,
            "Global".to_string(),
            "global context".to_string(),
            true,
            10,
        )
        .unwrap();
    store
        .create(
            ContextDocumentScope::Project {
                root: "/repo/other".to_string(),
            },
            "Other".to_string(),
            "other context".to_string(),
            true,
            10,
        )
        .unwrap();
    store
        .create(
            ContextDocumentScope::Global,
            "Disabled".to_string(),
            "disabled context".to_string(),
            false,
            10,
        )
        .unwrap();

    let selected = store.select_enabled_for_project(project).unwrap();
    assert_eq!(selected.omitted, 0);
    assert_eq!(selected.documents.len(), 2);
    assert_eq!(selected.documents[0].id, global_document.id);
    assert_eq!(selected.documents[1].id, project_document.id);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn context_document_cas_rejects_same_timestamp_change_and_deletion() {
    let (store, root) = store("cas");
    let document = store
        .create(
            ContextDocumentScope::Global,
            "Editable".to_string(),
            "before".to_string(),
            true,
            10,
        )
        .unwrap();
    let revision = store.revision(&document).unwrap();
    let changed = store.set_enabled(&document.id, false, 10).unwrap().unwrap();
    let stale = store
        .compare_and_swap_content(&document.id, &revision, "after".to_string(), 10)
        .unwrap();
    assert!(matches!(
        stale,
        CompareAndSwapContextDocumentResult::Stale { .. }
    ));
    assert_eq!(
        store.inspect(&document.id).unwrap().unwrap().content,
        "before"
    );

    let current_revision = store.revision(&changed).unwrap();
    let updated = store
        .compare_and_swap_content(&document.id, &current_revision, "after".to_string(), 11)
        .unwrap();
    let CompareAndSwapContextDocumentResult::Updated(updated) = updated else {
        panic!("current revision should update the document");
    };
    assert_eq!(updated.content, "after");
    assert!(!updated.enabled);

    assert!(store.delete(&document.id).unwrap());
    assert_eq!(
        store
            .compare_and_swap_content(
                &document.id,
                &store.revision(&updated).unwrap(),
                "must not recreate".to_string(),
                12,
            )
            .unwrap(),
        CompareAndSwapContextDocumentResult::Deleted
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn empty_context_document_requires_content_before_enablement() {
    let (store, root) = store("empty-enablement");
    let document = store
        .create(
            ContextDocumentScope::Global,
            "Empty draft".to_string(),
            String::new(),
            false,
            10,
        )
        .unwrap();

    assert!(store.set_enabled(&document.id, true, 11).is_err());
    let revision = store.revision(&document).unwrap();
    let updated = store
        .compare_and_swap_content(
            &document.id,
            &revision,
            "ready for future turns".to_string(),
            11,
        )
        .unwrap();
    let CompareAndSwapContextDocumentResult::Updated(updated) = updated else {
        panic!("content update should succeed");
    };
    assert!(!updated.enabled);
    assert!(
        store
            .set_enabled(&document.id, true, 12)
            .unwrap()
            .unwrap()
            .enabled
    );
    let _ = fs::remove_dir_all(root);
}
