//! Regression coverage for the SQLite issue store.
//!
//! These tests protect project/kind indexing, bounded querying, and scoped
//! deletion so every CLI, slash-command, and MAAP action surface can share the
//! same persistence contract.

use super::*;

fn temp_store(name: &str) -> IssueStore {
    let root = std::env::temp_dir().join(format!("mez-issue-store-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    IssueStore::new(root.join("issues.sqlite"))
}

/// Verifies issues are stored and filtered by project and kind.
#[test]
fn issue_store_adds_and_queries_by_project_and_kind() {
    let store = temp_store("query");
    let defect = store
        .add_issue(
            "/repo/a".to_string(),
            IssueKind::Defect,
            "Fix panic".to_string(),
            Some("panic in renderer".to_string()),
            10,
        )
        .unwrap();
    let _task = store
        .add_issue(
            "/repo/a".to_string(),
            IssueKind::Task,
            "Write docs".to_string(),
            None,
            11,
        )
        .unwrap();
    let _other = store
        .add_issue(
            "/repo/b".to_string(),
            IssueKind::Defect,
            "Other defect".to_string(),
            None,
            12,
        )
        .unwrap();

    let results = store
        .query_issues(
            &IssueQuery::new(
                "/repo/a".to_string(),
                Some(IssueKind::Defect),
                Some("panic".to_string()),
                Some(10),
            )
            .unwrap(),
        )
        .unwrap();

    assert_eq!(results, vec![defect]);
}

/// Verifies query limits are bounded and ordered by recent updates.
#[test]
fn issue_store_query_limit_bounds_results() {
    let store = temp_store("limit");
    for index in 0..3 {
        store
            .add_issue(
                "/repo".to_string(),
                IssueKind::Task,
                format!("Task {index}"),
                None,
                10 + index,
            )
            .unwrap();
    }

    let results = store
        .query_issues(&IssueQuery::new("/repo".to_string(), None, None, Some(2)).unwrap())
        .unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Task 2");
    assert_eq!(results[1].title, "Task 1");
}

/// Verifies deletion is scoped by project so equal ids cannot remove records
/// from another project key.
#[test]
fn issue_store_delete_requires_matching_project() {
    let store = temp_store("delete");
    let issue = store
        .add_issue(
            "/repo/a".to_string(),
            IssueKind::Defect,
            "Fix leak".to_string(),
            None,
            10,
        )
        .unwrap();

    let miss = store
        .delete_issue("/repo/b".to_string(), issue.id.clone())
        .unwrap();
    assert!(!miss.deleted);

    let hit = store
        .delete_issue("/repo/a".to_string(), issue.id.clone())
        .unwrap();
    assert!(hit.deleted);
    assert!(
        store
            .query_issues(&IssueQuery::new("/repo/a".to_string(), None, None, Some(10)).unwrap())
            .unwrap()
            .is_empty()
    );
}

/// Verifies invalid issue kinds and required text fields fail before SQL is
/// allowed to persist a malformed record.
#[test]
fn issue_validation_rejects_invalid_kind_and_empty_title() {
    assert!(IssueKind::parse("bug").is_err());
    assert!(
        IssueRecord::new(
            "id".to_string(),
            "/repo".to_string(),
            IssueKind::Task,
            "".to_string(),
            None,
            10,
        )
        .is_err()
    );
}
