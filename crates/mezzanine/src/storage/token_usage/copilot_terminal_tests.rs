//! Canonical terminal-call storage invariants with synthetic mapped sources.
//!
//! These fixtures qualify transactions and replay, not native process authority
//! or vendor delivery. Runtime ingress must independently supply that proof.

use super::external::copilot_calls::copilot_terminal_call_id;
use super::*;

/// Two foreground scopes intentionally have different native source owners.
pub(super) fn fixture() -> (TokenUsageStore, String, String) {
    let store = TokenUsageStore::new(
        std::env::temp_dir()
            .join(format!("mez-copilot-call-{}", new_token_usage_event_id()))
            .join("usage.sqlite"),
    );
    let a = owner(&store, 42, "foreground-a");
    let b = owner(&store, 42, "foreground-b");
    (store, a, b)
}

/// Creates only storage mapping metadata; this is never native authorization.
fn owner(store: &TokenUsageStore, uid: u32, session: &str) -> String {
    store
        .external_source_owner(
            &ExternalSourceKey::from_native(
                "copilot",
                NativeSourceWitness {
                    boot: "01234567-89ab-cdef-0123-456789abcdef",
                    uid,
                    root: (100, 2),
                    producer: (101, 100, 3),
                    kind: NativeSourceKind::CuratedCreator,
                    session,
                },
            )
            .unwrap(),
        )
        .unwrap()
}

/// Models an already-admitted immutable terminal observation with unknown
/// original project. The UID comes from a fixture, never the wire payload.
pub(super) fn report(owner: &str, uid: u32, call: &str, time: u64) -> ExternalUsageReport {
    let mut report = ExternalUsageReport {
        owner: owner.into(),
        project: None,
        harness: "copilot".into(),
        epoch: "pending".into(),
        event_id: copilot_terminal_call_id(uid, call).unwrap(),
        sequence: 1,
        mode: "observation".into(),
        baseline: false,
        observed_at: time,
        model: mez_agent::ModelTokenUsageKey::new("copilot", "fixture-model"),
        counters: ExternalCounters {
            input_tokens: 10,
            output_tokens: 4,
            reasoning_tokens: Some(1),
            cached_input_tokens: Some(3),
            cache_write_input_tokens: None,
        },
    };
    report.epoch = external_observation_epoch(&report).unwrap();
    report
}

/// Counts durable facts after failures without interpreting a returned envelope
/// as evidence. Native source mappings preexist and are intentionally separate.
pub(super) fn counts(store: &TokenUsageStore) -> Vec<i64> {
    let connection = store.open().unwrap();
    [
        "token_usage_events",
        "external_usage_streams",
        "external_usage_observations",
        "external_usage_receipts",
        "external_copilot_calls",
        "external_usage_requests",
    ]
    .map(|table| {
        connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    })
    .to_vec()
}

/// Removes only this fixture's unique private directory after all connections
/// have left their owning scopes. No vendor or ordinary user tree is involved.
pub(super) fn cleanup(store: TokenUsageStore) {
    let directory = store.path().parent().unwrap().to_path_buf();
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}

/// A lost first ACK, delayed reporter and storage reopen must acknowledge ONE
/// durable call. Another foreground owner cannot project its first reporter's
/// checkpoint, and no arrival clock may backfill the unknown original project.
#[test]
fn copilot_terminal_replay_preserves_first_expense_and_projection_owner() {
    let (store, a, b) = fixture();
    let call = "a".repeat(64);
    let first = store
        .ingest_copilot_terminal(&report(&a, 42, &call, 100), 42, &call, 101)
        .unwrap();
    assert!(first.applied());
    assert_eq!(first.projection().unwrap().totals.input_tokens, 10);
    drop(first); // Deliberately discard the actual committed acknowledgment.
    let reopened = TokenUsageStore::new(store.path());
    let replay = reopened
        .ingest_copilot_terminal(&report(&b, 42, &call, 200), 42, &call, 201)
        .unwrap();
    assert!(!replay.applied());
    assert!(replay.projection().is_none());
    assert_eq!(replay.revision(), 1);
    let own = reopened
        .ingest_copilot_terminal(&report(&a, 42, &call, 300), 42, &call, 301)
        .unwrap();
    assert!(!own.applied());
    assert!(own.projection().is_some());
    assert_eq!(counts(&store), [1, 1, 1, 1, 1, 0]);
    let connection = store.open().unwrap();
    let row: (String, i64, Option<String>, i64) = connection
        .query_row(
            "SELECT c.owner,c.observed_at,e.project_id,e.observed_at
             FROM external_copilot_calls c JOIN token_usage_events e ON 1=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(row, (a, 100, None, 100));
    drop(connection);
    drop(reopened);
    cleanup(store);
}

/// Reporter races use distinct SQLite connections and an actual common start
/// barrier. Exactly one transaction may publish expense and a projectable result.
#[test]
fn copilot_terminal_concurrent_reporters_commit_once() {
    let (store, a, b) = fixture();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let jobs = [a, b].map(|owner| {
        let store = TokenUsageStore::new(store.path());
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            let call = "b".repeat(64);
            let input = report(&owner, 42, &call, 100);
            barrier.wait();
            store.ingest_copilot_terminal(&input, 42, &call, 101)
        })
    });
    barrier.wait();
    let replies = jobs.map(|job| job.join().unwrap().unwrap());
    assert_eq!(replies.iter().filter(|reply| reply.applied()).count(), 1);
    assert_eq!(
        replies
            .iter()
            .filter(|reply| reply.projection().is_some())
            .count(),
        1
    );
    assert_eq!(counts(&store), [1, 1, 1, 1, 1, 0]);
    cleanup(store);
}

/// Model/category/count conflicts cannot turn an existing call into new expense.
/// Alias possession also cannot authorize an unmapped or project-selected report.
#[test]
fn copilot_terminal_conflicts_and_unqualified_reporters_do_not_mutate() {
    let (store, a, b) = fixture();
    let call = "c".repeat(64);
    let first = report(&a, 42, &call, 100);
    store
        .ingest_copilot_terminal(&first, 42, &call, 101)
        .unwrap();
    let mut candidates = vec![report(&b, 42, &call, 200); 6];
    candidates[0].counters.input_tokens += 1;
    candidates[1].counters.cache_write_input_tokens = Some(0);
    candidates[2].model.model = "another-model".into();
    candidates[3].owner = new_token_usage_event_id();
    candidates[4].project =
        Some(AccountingProjectId::from_stored(new_token_usage_event_id()).unwrap());
    candidates[5].counters.cached_input_tokens = Some(11);
    for candidate in candidates {
        assert!(
            store
                .ingest_copilot_terminal(&candidate, 42, &call, 201)
                .is_err()
        );
        assert_eq!(counts(&store), [1, 1, 1, 1, 1, 0]);
    }
    let next = "d".repeat(64);
    let accepted = store
        .ingest_copilot_terminal(&report(&a, 42, &next, 200), 42, &next, 201)
        .unwrap();
    assert_eq!(accepted.revision(), 2);
    assert_eq!(accepted.projection().unwrap().totals.input_tokens, 20);
    let replay = store
        .ingest_copilot_terminal(&first, 42, &call, 301)
        .unwrap();
    assert_eq!(replay.revision(), 2);
    assert_eq!(replay.projection().unwrap().totals.input_tokens, 20);
    assert_eq!(counts(&store), [2, 1, 2, 2, 2, 0]);
    cleanup(store);
}

/// A statement refusal after the expense SQL must roll back alias, receipt and
/// expense together. Removing only the fixture fault permits ONE later commit.
#[test]
fn copilot_terminal_alias_failure_rolls_back_every_effect() {
    let (store, a, _) = fixture();
    let connection = store.open().unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fixture_call_failure BEFORE INSERT ON external_copilot_calls
         BEGIN SELECT RAISE(ABORT,'fixture alias refusal'); END;",
        )
        .unwrap();
    let call = "e".repeat(64);
    let input = report(&a, 42, &call, 100);
    assert!(
        store
            .ingest_copilot_terminal(&input, 42, &call, 101)
            .is_err()
    );
    assert_eq!(counts(&store), [0, 0, 0, 0, 0, 0]);
    connection
        .execute_batch("DROP TRIGGER fixture_call_failure;")
        .unwrap();
    assert!(
        store
            .ingest_copilot_terminal(&input, 42, &call, 101)
            .unwrap()
            .applied()
    );
    assert_eq!(counts(&store), [1, 1, 1, 1, 1, 0]);
    drop(connection);
    cleanup(store);
}

/// Zero known input/output is a real observation while omitted optional fields
/// remain unknown. A different native UID is an independent idempotency domain;
/// neither case fabricates a pre-request marker or a currency amount.
#[test]
fn copilot_terminal_explicit_zero_unknown_categories_and_uid_domains() {
    let (store, a, _) = fixture();
    let call = "f".repeat(64);
    let mut zero = report(&a, 42, &call, 100);
    zero.counters = ExternalCounters::default();
    let first = store
        .ingest_copilot_terminal(&zero, 42, &call, 101)
        .unwrap();
    assert!(first.applied());
    assert_eq!(first.projection().unwrap().totals.reasoning_tokens, None);
    assert_eq!(counts(&store), [0, 1, 1, 1, 1, 0]);
    let b = owner(&store, 99, "another-uid");
    let other = report(&b, 99, &call, 100);
    assert_ne!(other.event_id, zero.event_id);
    assert!(
        store
            .ingest_copilot_terminal(&other, 99, &call, 101)
            .unwrap()
            .applied()
    );
    assert_eq!(counts(&store), [1, 2, 2, 2, 2, 0]);
    cleanup(store);
}

/// Schema13 upgrades append an empty alias table without rewriting existing
/// expense/mappings. A prior generic observation is not silently adopted as a
/// terminal call; missing canonical evidence cannot authorize another charge.
#[test]
fn copilot_terminal_schema13_upgrade_preserves_legacy_observations() {
    let (store, a, _) = fixture();
    let call = "0".repeat(64);
    let legacy = report(&a, 42, &call, 100);
    store.ingest_external(&legacy, 101).unwrap();
    let connection = store.open().unwrap();
    connection
        .execute_batch("DROP TABLE external_copilot_calls; PRAGMA user_version=13;")
        .unwrap();
    drop(connection);
    let reopened = TokenUsageStore::new(store.path());
    assert_eq!(counts(&reopened), [1, 1, 1, 1, 0, 0]);
    assert!(
        reopened
            .ingest_copilot_terminal(&legacy, 42, &call, 101)
            .is_err()
    );
    assert_eq!(counts(&reopened), [1, 1, 1, 1, 0, 0]);
    let connection = reopened.open().unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        14
    );
    drop(connection);
    drop(reopened);
    cleanup(store);
}

/// Full source-local tombstone capacity blocks new calls, but compatible replay
/// is looked up first and cannot be mistaken for a new charge or evicted ID.
#[test]
fn copilot_terminal_replay_survives_source_capacity_without_eviction() {
    let (store, a, _) = fixture();
    let call = "1".repeat(64);
    let input = report(&a, 42, &call, 100);
    let first = store
        .ingest_copilot_terminal(&input, 42, &call, 101)
        .unwrap();
    let connection = store.open().unwrap();
    connection
        .execute(
            "WITH RECURSIVE ids(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM ids WHERE n<65535)
         INSERT INTO external_usage_observations(owner,event_id,fingerprint,stream_id)
         SELECT ?1,printf('%064x',n),?2,?3 FROM ids",
            rusqlite::params![a, "b".repeat(64), first.projection().unwrap().stream_id],
        )
        .unwrap();
    assert!(
        !store
            .ingest_copilot_terminal(&input, 42, &call, 201)
            .unwrap()
            .applied()
    );
    let next = "2".repeat(64);
    assert!(
        store
            .ingest_copilot_terminal(&report(&a, 42, &next, 200), 42, &next, 201)
            .is_err()
    );
    assert_eq!(counts(&store), [1, 1, 65536, 1, 1, 0]);
    drop(connection);
    cleanup(store);
}

/// Ordinary raw-receipt retention may prune old payload receipts and expense
/// events. Retained canonical/observation tombstones must still ACK a fresh
/// reporter without recharging or replacing the first observation timestamp.
#[test]
fn copilot_terminal_retained_alias_replays_after_raw_receipt_pruning() {
    let (store, a, b) = fixture();
    let call = "3".repeat(64);
    store
        .ingest_copilot_terminal(&report(&a, 42, &call, 100), 42, &call, 101)
        .unwrap();
    let now = 100 + 200 * 86400;
    store.initialize(now).unwrap();
    assert_eq!(counts(&store), [0, 1, 1, 0, 1, 0]);
    let replay = store
        .ingest_copilot_terminal(&report(&b, 42, &call, now), 42, &call, now)
        .unwrap();
    assert!(!replay.applied());
    assert!(replay.projection().is_none());
    assert_eq!(counts(&store), [0, 1, 1, 0, 1, 0]);
    cleanup(store);
}

/// A retained alias without its original durable observation is corrupt, not
/// permission to recreate the expense under a new reporter. Raw receipt pruning
/// is separately supported; destroying a tombstone must instead fail closed.
#[test]
fn copilot_terminal_missing_original_observation_never_recharges() {
    let (store, a, b) = fixture();
    let call = "5".repeat(64);
    store
        .ingest_copilot_terminal(&report(&a, 42, &call, 100), 42, &call, 101)
        .unwrap();
    let connection = store.open().unwrap();
    // Deliberately damage this fixture through a connection-local override.
    // Ordinary connections keep SQLite's foreign-key protection enabled.
    connection
        .execute_batch("PRAGMA foreign_keys=OFF;")
        .unwrap();
    connection
        .execute("DELETE FROM external_usage_observations", [])
        .unwrap();
    assert!(
        store
            .ingest_copilot_terminal(&report(&b, 42, &call, 200), 42, &call, 201)
            .is_err()
    );
    assert_eq!(counts(&store), [1, 1, 0, 1, 1, 0]);
    drop(connection);
    cleanup(store);
}
