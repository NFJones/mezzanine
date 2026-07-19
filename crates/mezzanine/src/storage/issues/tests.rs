//! Regression coverage for the SQLite issue store.
//!
//! These tests protect project/kind indexing, bounded querying, and scoped
//! deletion so every CLI, slash-command, and MAAP action surface can share the
//! same persistence contract.

use super::{
    IssueBrowserQuery, IssueKind, IssueQuery, IssueState, IssueStore, IssueUpdate, NewIssueRecord,
    fs, issue_database_location,
};

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

/// Verifies issue state defaults to open, resolved issues remain queryable, and
/// default work queries exclude resolved issue history.
#[test]
fn issue_store_defaults_to_open_and_filters_resolved_state() {
    let store = temp_store("state");
    let issue = store
        .add_issue(
            "/repo".to_string(),
            IssueKind::Task,
            "Track state".to_string(),
            None,
            None,
            10,
        )
        .unwrap();
    assert_eq!(issue.state, IssueState::Open);

    let resolved = store
        .update_issue(
            "/repo".to_string(),
            issue.id.clone(),
            IssueUpdate {
                state: Some(IssueState::Resolved),
                ..IssueUpdate::default()
            },
            20,
        )
        .unwrap()
        .record
        .unwrap();
    assert_eq!(resolved.state, IssueState::Resolved);

    let default_results = store
        .query_issues(&IssueQuery::new("/repo".to_string(), None, None, Some(10)).unwrap())
        .unwrap();
    assert!(default_results.is_empty());

    let resolved_results = store
        .query_issues(
            &IssueQuery::new_with_state(
                "/repo".to_string(),
                None,
                Some(IssueState::Resolved),
                None,
                Some(10),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(resolved_results, vec![resolved]);
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

/// Verifies browser queries can filter across matching project globs while
/// keeping open issues as the default work surface.
#[test]
fn issue_store_browser_query_filters_by_project_glob_and_open_state() {
    let store = temp_store("browser-project-glob");
    let current_open = store
        .add_issue(
            "/repo/current".to_string(),
            IssueKind::Task,
            "Current open".to_string(),
            None,
            None,
            10,
        )
        .unwrap();
    let resolved = store
        .add_issue(
            "/repo/current".to_string(),
            IssueKind::Task,
            "Current resolved".to_string(),
            None,
            None,
            11,
        )
        .unwrap();
    store
        .update_issue(
            "/repo/current".to_string(),
            resolved.id,
            IssueUpdate {
                state: Some(IssueState::Resolved),
                ..IssueUpdate::default()
            },
            20,
        )
        .unwrap();
    let sibling_open = store
        .add_issue(
            "/repo/other".to_string(),
            IssueKind::Defect,
            "Sibling open".to_string(),
            None,
            None,
            12,
        )
        .unwrap();

    let results = store
        .query_issue_browser(
            &IssueBrowserQuery::new(
                Some("/repo/*".to_string()),
                None,
                Some(IssueState::Open),
                None,
                Some(100),
            )
            .unwrap(),
        )
        .unwrap();

    assert_eq!(results, vec![sibling_open, current_open]);
}

/// Verifies browser queries search title and body text, then keep bounded
/// results ordered by recent updates.
#[test]
fn issue_store_browser_query_matches_text_and_respects_limit() {
    let store = temp_store("browser-text");
    let body_match = store
        .add_issue(
            "/repo/a".to_string(),
            IssueKind::Task,
            "Unrelated title".to_string(),
            Some("panic happens in renderer".to_string()),
            None,
            10,
        )
        .unwrap();
    let newer_title_match = store
        .add_issue(
            "/repo/b".to_string(),
            IssueKind::Task,
            "Panic follow-up".to_string(),
            None,
            None,
            20,
        )
        .unwrap();
    let _non_match = store
        .add_issue(
            "/repo/c".to_string(),
            IssueKind::Task,
            "Docs only".to_string(),
            None,
            None,
            30,
        )
        .unwrap();

    let results = store
        .query_issue_browser(
            &IssueBrowserQuery::new(
                None,
                None,
                Some(IssueState::Open),
                Some("panic".to_string()),
                Some(1),
            )
            .unwrap(),
        )
        .unwrap();

    assert_eq!(results, vec![newer_title_match.clone()]);

    let expanded = store
        .query_issue_browser(
            &IssueBrowserQuery::new(
                None,
                None,
                Some(IssueState::Open),
                Some("panic".to_string()),
                Some(100),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(expanded, vec![newer_title_match, body_match]);
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

/// Verifies issue dependencies persist, query with records, and reject cycles.
///
/// Native dependency support is used by agents to choose a valid work order for
/// split tasks. The store must reject nonexistent dependencies and dependency
/// cycles so higher-level issue actions can trust the returned graph.
#[test]
fn issue_store_persists_dependencies_and_rejects_cycles() {
    let store = temp_store("dependencies");
    let prerequisite = store
        .add_issue(
            "/repo".to_string(),
            IssueKind::Task,
            "Implement storage".to_string(),
            None,
            None,
            10,
        )
        .unwrap();
    let dependent = store
        .add_issue_with_dependencies(
            NewIssueRecord {
                project: "/repo".to_string(),
                kind: IssueKind::Task,
                title: "Teach skills".to_string(),
                body: None,
                notes: None,
                depends_on: vec![prerequisite.id.clone()],
            },
            20,
        )
        .unwrap();

    assert_eq!(dependent.depends_on, vec![prerequisite.id.clone()]);
    let queried = store
        .query_issues(&IssueQuery::new("/repo".to_string(), None, None, Some(10)).unwrap())
        .unwrap();
    assert!(
        queried.iter().any(|record| record.id == dependent.id
            && record.depends_on == vec![prerequisite.id.clone()])
    );

    let missing = store.add_issue_with_dependencies(
        NewIssueRecord {
            project: "/repo".to_string(),
            kind: IssueKind::Task,
            title: "Blocked by missing issue".to_string(),
            body: None,
            notes: None,
            depends_on: vec!["missing".to_string()],
        },
        30,
    );
    assert!(missing.is_err());

    let cycle = store.update_issue(
        "/repo".to_string(),
        prerequisite.id.clone(),
        IssueUpdate {
            depends_on: Some(vec![dependent.id.clone()]),
            ..IssueUpdate::default()
        },
        40,
    );
    assert!(cycle.is_err());
}

/// Verifies an open dependent protects its prerequisite from deletion while a
/// resolved dependent no longer blocks cleanup of the prerequisite record.
#[test]
fn issue_store_delete_rejects_open_dependents_and_allows_resolved_dependents() {
    let store = temp_store("delete-dependencies");
    let prerequisite = store
        .add_issue(
            "/repo".to_string(),
            IssueKind::Task,
            "Implement storage".to_string(),
            None,
            None,
            10,
        )
        .unwrap();
    let dependent = store
        .add_issue_with_dependencies(
            NewIssueRecord {
                project: "/repo".to_string(),
                kind: IssueKind::Task,
                title: "Teach skills".to_string(),
                body: None,
                notes: None,
                depends_on: vec![prerequisite.id.clone()],
            },
            20,
        )
        .unwrap();

    let blocked = store.delete_issue("/repo".to_string(), prerequisite.id.clone());
    assert!(blocked.is_err());
    assert!(
        blocked
            .unwrap_err()
            .message()
            .contains(dependent.id.as_str())
    );

    store
        .update_issue(
            "/repo".to_string(),
            dependent.id,
            IssueUpdate {
                state: Some(IssueState::Resolved),
                ..IssueUpdate::default()
            },
            30,
        )
        .unwrap();
    assert!(
        store
            .delete_issue("/repo".to_string(), prerequisite.id)
            .unwrap()
            .deleted
    );
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

/// Verifies relative configured issue database paths remain Mezzanine-owned so
/// their parent directories are created and privatized under the config root.
#[test]
fn relative_configured_issue_database_path_manages_private_parent() {
    let root = std::env::temp_dir().join(format!(
        "mez-issue-store-relative-parent-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let database_path = issue_database_location(&root, Some("nested/issues.sqlite"));
    assert!(database_path.manages_private_parent());
    let store = IssueStore::from_database_path(database_path);

    store
        .add_issue(
            "/repo".to_string(),
            IssueKind::Task,
            "Create owned parent".to_string(),
            None,
            None,
            10,
        )
        .unwrap();

    let parent = root.join("nested");
    assert!(parent.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    let _ = fs::remove_dir_all(root);
}

/// Verifies absolute configured issue database paths do not cause Mezzanine to
/// chmod or create caller-owned parent directories outside the config root.
#[test]
fn absolute_configured_issue_database_path_preserves_parent_permissions() {
    let root = std::env::temp_dir().join(format!(
        "mez-issue-store-absolute-parent-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let parent = root.join("external-parent");
    fs::create_dir_all(&parent).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let configured = parent.join("issues.sqlite");
    let configured = configured.to_str().unwrap();
    let database_path = issue_database_location(root.join("config"), Some(configured));
    assert!(!database_path.manages_private_parent());
    let store = IssueStore::from_database_path(database_path);

    store
        .add_issue(
            "/repo".to_string(),
            IssueKind::Defect,
            "Preserve caller parent".to_string(),
            None,
            None,
            10,
        )
        .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    let _ = fs::remove_dir_all(root);
}
