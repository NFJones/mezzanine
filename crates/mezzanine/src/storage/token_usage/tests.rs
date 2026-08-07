//! Regression coverage for durable rolling token accounting.
//!
//! These tests protect exact cutoff semantics, cache-reporting fidelity,
//! idempotent event insertion, retention, schema validation, and private file
//! posture independently of the runtime and `/status` presentation layers.

use std::fs;

use mez_agent::{ModelTokenUsage, ModelTokenUsageKey};
use rusqlite::Connection;

use super::{TokenUsageEvent, TokenUsageStore};

fn temp_store(name: &str) -> TokenUsageStore {
    let root = std::env::temp_dir().join(format!(
        "mez-token-usage-{name}-{}-{}",
        std::process::id(),
        super::new_token_usage_event_id()
    ));
    TokenUsageStore::new(root.join("token-usage.sqlite"))
}

fn event(id: &str, observed_at: u64, input: u64, cached: Option<u64>) -> TokenUsageEvent {
    TokenUsageEvent {
        id: id.to_string(),
        observed_at_unix_seconds: observed_at,
        model: ModelTokenUsageKey::new("openai", "gpt-test"),
        usage: ModelTokenUsage {
            input_tokens: input,
            output_tokens: 3,
            reasoning_tokens: 2,
            cached_input_tokens: cached,
            cache_write_input_tokens: Some(1),
        },
    }
}

/// Events exactly on a lower cutoff are included while older and future rows
/// are excluded from every rolling window.
#[test]
fn aggregate_windows_include_cutoffs_and_exclude_future_events() {
    let store = temp_store("boundaries");
    let now = 100_u64 * 86_400;
    store.initialize(now).unwrap();
    store
        .append(&event("seven", now - 7 * 86_400, 7, Some(2)))
        .unwrap();
    store
        .append(&event("older", now - 7 * 86_400 - 1, 11, Some(2)))
        .unwrap();
    store
        .append(&event("future", now + 1, 13, Some(2)))
        .unwrap();

    let totals = store.aggregate_windows(now, &[7, 30]).unwrap();
    let key = ModelTokenUsageKey::new("openai", "gpt-test");

    assert_eq!(totals[&7][&key].input_tokens, 7);
    assert_eq!(totals[&30][&key].input_tokens, 18);
}

/// Reusing an event id is a no-op and does not double-count a replayed append.
#[test]
fn append_is_idempotent_and_skips_zero_usage() {
    let store = temp_store("idempotent");
    store.initialize(100).unwrap();
    let sample = event("same", 100, 9, Some(4));
    assert!(store.append(&sample).unwrap());
    assert!(!store.append(&sample).unwrap());
    let mut zero = sample.clone();
    zero.id = "zero".to_string();
    zero.usage = ModelTokenUsage::default();
    assert!(!store.append(&zero).unwrap());

    let totals = store.aggregate_windows(100, &[7]).unwrap();
    assert_eq!(totals[&7][&sample.model].input_tokens, 9);
}

/// Aggregation preserves the semantic difference between an explicitly
/// reported zero cache count and an unknown cache count.
#[test]
fn aggregate_windows_preserve_unknown_cache_reporting() {
    let store = temp_store("cache-unknown");
    store.initialize(100).unwrap();
    store.append(&event("known", 90, 5, Some(0))).unwrap();
    store.append(&event("unknown", 91, 7, None)).unwrap();

    let totals = store.aggregate_windows(100, &[7]).unwrap();
    let key = ModelTokenUsageKey::new("openai", "gpt-test");
    assert_eq!(totals[&7][&key].cached_input_tokens, None);
}

/// Initialization prunes rows beyond the 91-day safety retention but keeps
/// future rows for later clock recovery.
#[test]
fn initialize_prunes_expired_rows_without_pruning_future_rows() {
    let store = temp_store("retention");
    let now = 200_u64 * 86_400;
    store.initialize(0).unwrap();
    store
        .append(&event("expired", now - 92 * 86_400, 5, Some(1)))
        .unwrap();
    store.append(&event("future", now + 1, 7, Some(1))).unwrap();
    store.initialize(now).unwrap();

    let connection = Connection::open(store.path()).unwrap();
    let ids = connection
        .prepare("SELECT id FROM token_usage_events ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(ids, vec!["future".to_string()]);
}

/// Values outside SQLite's signed integer range are rejected before insertion.
#[test]
fn append_rejects_values_outside_sqlite_integer_range() {
    let store = temp_store("overflow");
    store.initialize(1).unwrap();
    let error = store
        .append(&event("overflow", 1, u64::MAX, Some(0)))
        .unwrap_err();
    assert!(error.to_string().contains("exceeded SQLite range"));
}

/// A future schema version fails explicitly rather than being silently opened.
#[test]
fn initialize_rejects_future_schema_versions() {
    let store = temp_store("future-schema");
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    let connection = Connection::open(store.path()).unwrap();
    connection.pragma_update(None, "user_version", 2).unwrap();
    drop(connection);

    let error = store.initialize(1).unwrap_err();
    assert!(error.to_string().contains("newer than supported"));
}

/// The standard store creates a private parent directory and database file on
/// Unix so retained usage is not exposed to other local users.
#[cfg(unix)]
#[test]
fn initialize_applies_private_filesystem_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let store = temp_store("permissions");
    store.initialize(1).unwrap();

    let parent_mode = fs::metadata(store.path().parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    let file_mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(parent_mode, 0o700);
    assert_eq!(file_mode, 0o600);
}
