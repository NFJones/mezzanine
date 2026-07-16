//! Canonical memory line and scope codec tests.

use super::record;
use crate::memory::{
    MemoryKind, MemoryRecord, MemoryScope, MemorySource, MemoryState, decode_scope, encode_scope,
    kind_name, parse_kind, parse_model_writable_kind, parse_state, source_name, state_name,
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

/// Verifies canonical memory labels round trip through the public agent API.
///
/// Product browsers and persistence adapters consume these names directly so
/// they cannot silently grow a second taxonomy when new variants are added.
#[test]
fn memory_taxonomy_names_and_parsers_are_canonical() {
    let kinds = [
        MemoryKind::Preference,
        MemoryKind::Fact,
        MemoryKind::Procedure,
        MemoryKind::Documentation,
        MemoryKind::Research,
        MemoryKind::Episode,
        MemoryKind::Warning,
        MemoryKind::Scratch,
    ];
    for kind in kinds {
        assert_eq!(parse_kind(kind_name(kind)).unwrap(), kind);
    }

    let states = [
        MemoryState::Active,
        MemoryState::Stale,
        MemoryState::Superseded,
        MemoryState::Archived,
        MemoryState::Expired,
    ];
    for state in states {
        assert_eq!(parse_state(state_name(state)).unwrap(), state);
    }

    assert_eq!(source_name(MemorySource::User), "user");
    assert_eq!(source_name(MemorySource::Agent), "agent");
    assert_eq!(source_name(MemorySource::Imported), "imported");
    assert_eq!(source_name(MemorySource::Configuration), "configuration");
}

/// Verifies model-authored stores use one canonical writable-kind policy.
///
/// Durable episodes and scratch records are runtime-managed even though they
/// remain valid storage kinds, while user-facing model labels accept harmless
/// surrounding whitespace and ASCII case differences.
#[test]
fn model_writable_memory_kinds_reject_runtime_managed_taxonomy() {
    assert_eq!(
        parse_model_writable_kind(" Research ").unwrap(),
        MemoryKind::Research
    );
    assert!(parse_model_writable_kind("episode").is_err());
    assert!(parse_model_writable_kind("scratch").is_err());
    assert!(parse_model_writable_kind("unknown").is_err());
}
