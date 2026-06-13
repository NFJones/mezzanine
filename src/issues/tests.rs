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
            None,
            10,
        )
        .unwrap();
    let _task = store
        .add_issue(
            "/repo/a".to_string(),
            IssueKind::Task,
            "Write docs".to_string(),
            None,
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
            None,
            10,
        )
        .is_err()
    );
}

/// Verifies notes are persisted, updated, cleared, and used to refresh the
/// issue's update timestamp without changing the original description body.
#[test]
fn issue_store_persists_and_updates_notes() {
    let store = temp_store("notes");
    let issue = store
        .add_issue(
            "/repo".to_string(),
            IssueKind::Task,
            "Track rollout".to_string(),
            Some("Initial scope".to_string()),
            Some("Started investigation".to_string()),
            10,
        )
        .unwrap();
    assert_eq!(issue.notes.as_deref(), Some("Started investigation"));

    let updated = store
        .update_issue(
            "/repo".to_string(),
            issue.id.clone(),
            IssueUpdate {
                notes: Some("Validated storage path".to_string()),
                ..IssueUpdate::default()
            },
            20,
        )
        .unwrap();
    let record = updated.record.unwrap();
    assert!(updated.updated);
    assert_eq!(record.body.as_deref(), Some("Initial scope"));
    assert_eq!(record.notes.as_deref(), Some("Validated storage path"));
    assert_eq!(record.updated_at_unix_seconds, 20);

    let cleared = store
        .update_issue(
            "/repo".to_string(),
            issue.id.clone(),
            IssueUpdate {
                clear_notes: true,
                ..IssueUpdate::default()
            },
            30,
        )
        .unwrap()
        .record
        .unwrap();
    assert_eq!(cleared.notes, None);
    assert_eq!(cleared.updated_at_unix_seconds, 30);
}

/// Verifies existing databases without a notes column migrate in place and
/// preserve older issue rows with empty notes.
#[test]
fn issue_store_migrates_old_schema_to_notes_column() {
    let root =
        std::env::temp_dir().join(format!("mez-issue-store-migration-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let path = root.join("issues.sqlite");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE issues (
                    id TEXT PRIMARY KEY NOT NULL,
                    project TEXT NOT NULL,
                    kind TEXT NOT NULL CHECK (kind IN ('defect', 'task')),
                    title TEXT NOT NULL,
                    body TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                INSERT INTO issues (id, project, kind, title, body, created_at, updated_at)
                VALUES ('old-id', '/repo', 'defect', 'Old issue', 'legacy body', 10, 10);",
            )
            .unwrap();
    }

    let store = IssueStore::new(&path);
    let results = store
        .query_issues(&IssueQuery::new("/repo".to_string(), None, None, Some(10)).unwrap())
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].notes, None);
    let updated = store
        .update_issue(
            "/repo".to_string(),
            "old-id".to_string(),
            IssueUpdate {
                notes: Some("migrated note".to_string()),
                ..IssueUpdate::default()
            },
            20,
        )
        .unwrap()
        .record
        .unwrap();
    assert_eq!(updated.notes.as_deref(), Some("migrated note"));

    let _ = fs::remove_dir_all(root);
}
