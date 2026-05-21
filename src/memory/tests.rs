//! Unit tests for session and persistent memory behavior.

use super::{
    MemoryRecord, MemoryScope, MemorySource, PersistentMemoryStore, SessionMemoryStore,
    decode_scope, encode_scope, fs,
};
/// Runs the record operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn record(id: &str, scope: MemoryScope, content: &str) -> MemoryRecord {
    MemoryRecord {
        id: id.to_string(),
        scope,
        created_at_unix_seconds: 10,
        updated_at_unix_seconds: 10,
        source: MemorySource::Agent,
        priority: 10,
        content: content.to_string(),
        explicit_sensitive_consent: false,
    }
}

/// Verifies persistent memory rejects secrets without explicit consent.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn persistent_memory_rejects_secrets_without_explicit_consent() {
    let record = record("m1", MemoryScope::Global, "api_key = sk-secret");

    let error = record.validate_for_persistence().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
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
        .edit_content("m1", "prefer cargo test --all-targets", 12, false)
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
