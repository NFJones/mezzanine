//! Canonical memory line and scope codec tests.

use super::record;
use crate::memory::{
    MemoryKind, MemoryRecord, MemoryScope, MemoryState, decode_scope, encode_scope,
};

/// Verifies TSV export preserves every extended memory metadata field.
///
/// This regression scenario covers the 16-field export format produced by
/// `MemoryRecord::encode()` so imports do not silently reset metadata.
#[test]
fn memory_record_tsv_round_trip_preserves_extended_metadata() {
    let mut original = record(
        "metadata",
        MemoryScope::Project {
            root: "/work/repo".to_string(),
        },
        "remember the full metadata contract",
    );
    original.kind = MemoryKind::Research;
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

/// Verifies memory scope round trips escaped project paths.
///
/// This regression scenario protects delimiters embedded in project roots
/// from being interpreted as scope-component separators.
#[test]
fn memory_scope_round_trips_escaped_project_paths() {
    let scope = MemoryScope::Project {
        root: "/work/repo:with:colon".to_string(),
    };

    assert_eq!(decode_scope(&encode_scope(&scope)).unwrap(), scope);
}
