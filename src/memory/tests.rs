//! Unit tests for session and persistent memory behavior.

use super::{
    MemoryKind, MemoryRecord, MemoryRetentionPolicy, MemoryRetrievalRequest, MemoryScope,
    MemorySearchRequest, MemorySource, MemoryState, PersistentMemoryStore, SessionMemoryStore,
    decode_scope, encode_scope, fs, retrieve_persistent_memory,
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
fn session_memory_clears_session_and_persistent_cache_records_for_deleted_session() {
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

    assert_eq!(store.clear_session("$1"), 3);

    assert!(store.inspect("m1").is_none());
    assert!(store.inspect("m2").is_none());
    assert!(store.inspect("m3").is_none());
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
    assert_eq!(superseded.supersedes_id, None);
    assert_eq!(
        store.inspect("new").unwrap().supersedes_id.as_deref(),
        Some("old")
    );

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

/// Verifies TSV export preserves every extended memory metadata field.
///
/// This regression scenario covers the 16-field export format produced by
/// `MemoryRecord::encode()` so imports do not silently reset kind, lifecycle,
/// reinforcement, supersession, expiry, or retention-duration metadata.
#[test]
fn memory_record_tsv_round_trip_preserves_extended_metadata() {
    let mut original = record(
        "metadata",
        MemoryScope::Project {
            root: "/work/repo".to_string(),
        },
        "remember the full metadata contract",
    );
    original.kind = MemoryKind::Warning;
    original.state = MemoryState::Stale;
    original.last_used_at_unix_seconds = Some(20);
    original.use_count = 3;
    original.confirmed_count = 2;
    original.last_confirmed_at_unix_seconds = Some(21);
    original.supersedes_id = Some("older".to_string());
    original.expires_at_unix_seconds = Some(1_000);
    original.expiration_duration_seconds = Some(600);

    let decoded = MemoryRecord::decode(&original.encode().unwrap()).unwrap();

    assert_eq!(decoded, original);
}

/// Verifies persistent memory applies metadata filters before final limits.
///
/// This regression scenario seeds a higher-ranked record outside the requested
/// scope ahead of a lower-ranked in-scope record. A small result limit must not
/// let an early SQL limit drop the only valid match before metadata filtering.
#[test]
fn persistent_memory_search_filters_before_limiting_results() {
    let root = std::env::temp_dir().join(format!(
        "mez-memory-search-filter-limit-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = PersistentMemoryStore::under_config_root(&root);
    store
        .upsert(record(
            "other-project",
            MemoryScope::Project {
                root: "/work/other".to_string(),
            },
            "prefer fast tests",
        ))
        .unwrap();
    store
        .upsert(record(
            "target-project",
            MemoryScope::Project {
                root: "/work/target".to_string(),
            },
            "prefer focused tests",
        ))
        .unwrap();

    let matches = store
        .search(&MemorySearchRequest {
            scope: Some(MemoryScope::Project {
                root: "/work/target".to_string(),
            }),
            limit: 1,
            ..MemorySearchRequest::default()
        })
        .unwrap();

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].record.id, "target-project");

    let _ = fs::remove_dir_all(root);
}

/// Verifies punctuation-only FTS queries never reach SQLite as empty MATCH text.
///
/// This regression scenario exercises untrusted free-text normalization with a
/// query that contains no searchable tokens. Search may use deterministic
/// fallback ordering, but it must not surface a SQLite FTS syntax error.
#[test]
fn persistent_memory_search_accepts_punctuation_only_queries() {
    let root = std::env::temp_dir().join(format!(
        "mez-memory-punctuation-query-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = PersistentMemoryStore::under_config_root(&root);
    store
        .upsert(record("punctuation", MemoryScope::Global, "ordinary note"))
        .unwrap();

    let matches = store
        .search(&MemorySearchRequest {
            query: Some("!!!".to_string()),
            limit: 10,
            ..MemorySearchRequest::default()
        })
        .unwrap();

    assert_eq!(matches[0].record.id, "punctuation");

    let _ = fs::remove_dir_all(root);
}

/// Verifies archive-before-prune can still make record-count caps effective.
///
/// This regression scenario runs retention twice with a one-record cap. The
/// first pass archives the over-limit record for operator visibility, and the
/// second pass deletes the already-archived record so it stops holding the cap
/// open forever.
#[test]
fn archive_before_prune_deletes_already_archived_records_on_later_passes() {
    let root = std::env::temp_dir().join(format!(
        "mez-memory-archive-before-prune-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = PersistentMemoryStore::under_config_root(&root);
    store
        .upsert(record("older", MemoryScope::Global, "older workflow"))
        .unwrap();
    let mut newer = record("newer", MemoryScope::Global, "newer workflow");
    newer.priority = 20;
    store.upsert(newer).unwrap();

    let policy = MemoryRetentionPolicy {
        now_unix_seconds: 50,
        max_records: Some(1),
        max_bytes: None,
        archive_before_prune: true,
    };
    let first_pass = store.enforce_retention(policy, false).unwrap();
    assert_eq!(first_pass[0].id, "older");
    assert_eq!(store.inspect("older").unwrap().state, MemoryState::Archived);

    let second_pass = store.enforce_retention(policy, false).unwrap();

    assert_eq!(second_pass[0].id, "older");
    assert!(store.inspect("older").is_err());
    assert_eq!(store.list().unwrap().len(), 1);

    let _ = fs::remove_dir_all(root);
}

/// Verifies expiry refresh overflow is rejected instead of clearing expiry.
///
/// This regression scenario protects retention metadata from becoming
/// non-expiring when a use timestamp plus refresh duration exceeds `u64::MAX`.
#[test]
fn record_use_rejects_expiry_refresh_overflow() {
    let root = std::env::temp_dir().join(format!(
        "mez-memory-record-use-overflow-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = PersistentMemoryStore::under_config_root(&root);
    let mut expiring = record("overflow", MemoryScope::Global, "overflow note");
    expiring.expires_at_unix_seconds = Some(100);
    expiring.expiration_duration_seconds = Some(10);
    store.upsert(expiring).unwrap();

    assert!(store.record_use("overflow", u64::MAX - 5).is_err());
    assert_eq!(
        store.inspect("overflow").unwrap().expires_at_unix_seconds,
        Some(100)
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies a zero injection limit disables persistent memory injection.
///
/// This regression scenario distinguishes candidate search defaults from the
/// model-facing injection cap. A configured injection limit of zero must return
/// no injectable candidates rather than acting like an unlimited setting.
#[test]
fn persistent_memory_retrieval_zero_injection_limit_returns_no_candidates() {
    let root = std::env::temp_dir().join(format!(
        "mez-memory-zero-injection-limit-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = PersistentMemoryStore::under_config_root(&root);
    store
        .upsert(record("candidate", MemoryScope::Global, "candidate note"))
        .unwrap();

    let result = retrieve_persistent_memory(
        &store,
        &MemoryRetrievalRequest {
            candidate_limit: 10,
            injection_limit: 0,
            ..MemoryRetrievalRequest::default()
        },
    )
    .unwrap();

    assert!(result.candidates.is_empty());

    let _ = fs::remove_dir_all(root);
}

/// Verifies disabling FTS still permits deterministic queried search.
///
/// This regression scenario protects the `memory.fts_enabled = false` contract:
/// opening the store must not create the FTS table or triggers, and queries must
/// still return deterministic metadata-ranked results instead of using MATCH.
#[test]
fn persistent_memory_search_uses_fallback_when_fts_is_disabled() {
    let root = std::env::temp_dir().join(format!("mez-memory-fts-disabled-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = PersistentMemoryStore::under_config_root(&root).with_fts_enabled(false);
    store
        .upsert(record("candidate", MemoryScope::Global, "candidate note"))
        .unwrap();

    let matches = store
        .search(&MemorySearchRequest {
            query: Some("candidate".to_string()),
            limit: 10,
            ..MemorySearchRequest::default()
        })
        .unwrap();

    assert_eq!(matches[0].record.id, "candidate");
    assert_eq!(matches[0].fts_rank, None);
    let connection = rusqlite::Connection::open(store.path()).unwrap();
    let fts_table_count = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'memory_records_fts'",
            [],
            |row| row.get::<_, u64>(0),
        )
        .unwrap();
    assert_eq!(fts_table_count, 0);

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
