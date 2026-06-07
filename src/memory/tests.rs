//! Unit tests for session and persistent memory behavior.

use super::{
    MemoryKind, MemoryRecord, MemoryRetentionPolicy, MemoryScope, MemorySearchRequest,
    MemorySource, MemoryState, PersistentMemoryStore, SessionMemoryStore, decode_scope,
    encode_scope, fs,
};
/// Runs the record operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn record(id: &str, scope: MemoryScope, content: &str) -> MemoryRecord {
    MemoryRecord::new_with_defaults(id, scope, 10, 10, MemorySource::Agent, 10, content)
}

/// Verifies persistent memory accepts user-managed sensitive content.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn persistent_memory_accepts_sensitive_content_without_heuristic_rejection() {
    let record = record("m1", MemoryScope::Global, "api_key = sk-secret");

    record.validate_for_persistence().unwrap();
}

/// Verifies session memory clears records for deleted session.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn session_memory_clears_records_for_deleted_session() {
    let mut store = SessionMemoryStore::default();
    store
        .upsert(record(
            "m1",
            MemoryScope::Session {
                session_id: "$1".to_string(),
            },
            "session note",
        ))
        .unwrap();
    store
        .upsert(record(
            "m2",
            MemoryScope::Pane {
                session_id: "$1".to_string(),
                pane_id: "%1".to_string(),
            },
            "pane note",
        ))
        .unwrap();
    store
        .upsert(record("m3", MemoryScope::Global, "global note"))
        .unwrap();

    assert_eq!(store.clear_session("$1"), 2);

    assert!(store.inspect("m1").is_none());
    assert!(store.inspect("m2").is_none());
    assert!(store.inspect("m3").is_some());
}

/// Verifies persistent memory can inspect edit export and delete.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn persistent_memory_can_inspect_edit_export_and_delete() {
    let root = std::env::temp_dir().join(format!("mez-memory-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = PersistentMemoryStore::under_config_root(&root);
    store
        .upsert(record(
            "m1",
            MemoryScope::Project {
                root: "/work/repo".to_string(),
            },
            "prefer cargo test",
        ))
        .unwrap();

    assert_eq!(store.inspect("m1").unwrap().content, "prefer cargo test");
    let edited = store
        .edit_content("m1", "prefer cargo test --all-targets", 12)
        .unwrap();
    assert_eq!(edited.updated_at_unix_seconds, 12);
    assert!(
        store
            .export_tsv()
            .unwrap()
            .contains("cargo test --all-targets")
    );
    assert!(store.delete("m1").unwrap());
    assert!(store.inspect("m1").is_err());

    let _ = fs::remove_dir_all(root);
}

/// Verifies persistent memory imports legacy TSV and searches SQLite FTS.
///
/// This regression scenario documents the storage migration and retrieval
/// behavior so failures point at a concrete persistence contract change.
#[test]
fn persistent_memory_imports_legacy_tsv_and_searches_fts() {
    let root = std::env::temp_dir().join(format!("mez-memory-import-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let legacy_record = record(
        "legacy",
        MemoryScope::Global,
        "prefer cargo nextest for suites",
    );
    fs::write(
        root.join("memory.tsv"),
        format!("{}\n", legacy_record.encode().unwrap()),
    )
    .unwrap();

    let store = PersistentMemoryStore::under_config_root(&root);
    let imported = store.inspect("legacy").unwrap();
    assert_eq!(imported.content, "prefer cargo nextest for suites");

    let matches = store
        .search(&MemorySearchRequest {
            query: Some("nextest".to_string()),
            kind: Some(MemoryKind::Fact),
            limit: 10,
            ..MemorySearchRequest::default()
        })
        .unwrap();
    assert_eq!(matches[0].record.id, "legacy");

    let _ = fs::remove_dir_all(root);
}

/// Verifies persistent memory tracks use confirmation supersession and retention.
///
/// This regression scenario documents the lifecycle metadata that keeps memory
/// retrieval auditable while retention can archive or prune lower-value records.
#[test]
fn persistent_memory_tracks_usage_confirmation_supersession_and_retention() {
    let root = std::env::temp_dir().join(format!("mez-memory-lifecycle-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = PersistentMemoryStore::under_config_root(&root);
    store
        .upsert(record("old", MemoryScope::Global, "old workflow"))
        .unwrap();
    store
        .upsert(record("new", MemoryScope::Global, "new workflow"))
        .unwrap();
    store
        .upsert(record("extra", MemoryScope::Global, "extra workflow"))
        .unwrap();

    let used = store.record_use("old", 20).unwrap();
    assert_eq!(used.last_used_at_unix_seconds, Some(20));
    assert_eq!(used.use_count, 1);

    let mut expiring = record("expiring", MemoryScope::Global, "useful workflow");
    expiring.expires_at_unix_seconds = Some(110);
    expiring.expiration_duration_seconds = Some(100);
    store.upsert(expiring).unwrap();
    let refreshed = store.record_use("expiring", 30).unwrap();
    assert_eq!(refreshed.last_used_at_unix_seconds, Some(30));
    assert_eq!(refreshed.expires_at_unix_seconds, Some(130));
    assert_eq!(refreshed.expiration_duration_seconds, Some(100));

    let confirmed = store.confirm("new", 21).unwrap();
    assert_eq!(confirmed.last_confirmed_at_unix_seconds, Some(21));
    assert_eq!(confirmed.confirmed_count, 1);

    let superseded = store.supersede("old", "new", 22).unwrap();
    assert_eq!(superseded.state, MemoryState::Superseded);
    assert_eq!(superseded.supersedes_id.as_deref(), Some("new"));

    let dry_run = store
        .enforce_retention(
            MemoryRetentionPolicy {
                now_unix_seconds: 23,
                max_records: Some(3),
                max_bytes: None,
                archive_before_prune: true,
            },
            true,
        )
        .unwrap();
    assert_eq!(
        dry_run
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        ["old"]
    );
    assert_eq!(store.inspect("old").unwrap().state, MemoryState::Superseded);

    let archived = store
        .enforce_retention(
            MemoryRetentionPolicy {
                now_unix_seconds: 24,
                max_records: Some(3),
                max_bytes: None,
                archive_before_prune: true,
            },
            false,
        )
        .unwrap();
    assert_eq!(archived[0].id, "old");
    assert_eq!(store.inspect("old").unwrap().state, MemoryState::Archived);

    let _ = fs::remove_dir_all(root);
}

/// Verifies memory scope round trips escaped project paths.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn memory_scope_round_trips_escaped_project_paths() {
    let scope = MemoryScope::Project {
        root: "/work/repo:with:colon".to_string(),
    };

    assert_eq!(decode_scope(&encode_scope(&scope)).unwrap(), scope);
}
