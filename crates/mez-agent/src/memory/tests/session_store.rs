//! Process-local session memory tests.

use super::record;
use crate::memory::{MemoryScope, SessionMemoryStore};

/// Verifies session memory clears records for a deleted session.
///
/// This regression scenario covers every session-owned scope and confirms the
/// process-local cache is cleared together with direct session records.
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
