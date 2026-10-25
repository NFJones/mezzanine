//! Generated-store tests for inert, bounded native source activation receipts.
//!
//! Synthetic native metadata exercises storage identity only; none of these
//! tests attests a real producer or grants enrollment/reporting authority.

use super::source_activation::{ExternalSourceActivation, ExternalSourceActivationState};
use super::*;

/// Owns only this test's unique temporary database directory.
struct Fixture {
    root: PathBuf,
    store: TokenUsageStore,
}

impl Fixture {
    /// Creates isolated storage without opening a user store or native process.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mez-source-activation-{}",
            new_token_usage_event_id()
        ));
        let store = TokenUsageStore::new(root.join("usage.sqlite"));
        Self { root, store }
    }
}

impl Drop for Fixture {
    /// Removes only the generated directory after synchronous SQLite users drop.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Builds a storage-only exact source selector, never native admission evidence.
fn key(session: &str) -> ExternalSourceKey {
    ExternalSourceKey::from_native(
        "pi",
        NativeSourceWitness {
            boot: "01234567-89ab-cdef-0123-456789abcdef",
            uid: 42,
            root: (100, 2),
            producer: (101, 100, 3),
            kind: NativeSourceKind::Socket,
            session,
        },
    )
    .unwrap()
}

/// SQLite text length/substr may stop at NUL, while a BLOB of UUID bytes can
/// satisfy length checks too. Neither malformed source mapping may alias the
/// distinct legitimate owner's accepted receipt through ANY activation API.
/// Both sources and the original receipt must remain byte-for-byte unchanged.
#[test]
fn external_source_activation_malformed_owner_cannot_alias_another_source() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let source = key("legitimate-owner");
    let foreign = key("malformed-owner");
    let owner = store.external_source_owner(&source).unwrap();
    let other = store.external_source_owner(&foreign).unwrap();
    let proposal = activation(1, 2, 100, 200);
    store
        .prepare_external_source_activation(&source, &proposal)
        .unwrap();
    let accepted = store
        .accept_external_source_activation(&source, &proposal, 1, 101)
        .unwrap();
    let database = rusqlite::Connection::open(store.path()).unwrap();
    for malformed in [
        rusqlite::types::Value::Text(format!("{owner}\0foreign")),
        rusqlite::types::Value::Blob(owner.as_bytes().to_vec()),
    ] {
        database
            .execute(
                "UPDATE external_usage_sources SET owner=?1 WHERE owner=?2",
                rusqlite::params![malformed, other],
            )
            .unwrap();
        let before = database.query_row("SELECT typeof(owner),CAST(owner AS BLOB) FROM external_usage_sources WHERE owner<>?1",
            [&owner], |row| Ok((row.get::<_, String>(0)?,row.get::<_, Vec<u8>>(1)?))).unwrap();
        assert!(
            store.external_source_activation(&foreign).is_err(),
            "malformed owner exposed another source's receipt"
        );
        assert!(
            store
                .prepare_external_source_activation(&foreign, &proposal)
                .is_err()
        );
        assert!(
            store
                .accept_external_source_activation(&foreign, &proposal, 1, 102)
                .is_err()
        );
        assert!(
            store
                .retire_external_source_activation(&foreign, &proposal, 2)
                .is_err()
        );
        assert_eq!(
            store.external_source_activation(&source).unwrap(),
            Some(accepted.clone())
        );
        assert_eq!(database.query_row("SELECT typeof(owner),CAST(owner AS BLOB) FROM external_usage_sources WHERE owner<>?1",
            [&owner], |row| Ok((row.get::<_, String>(0)?,row.get::<_, Vec<u8>>(1)?))).unwrap(), before);
        assert_eq!(
            database
                .query_row(
                    "SELECT count(*) FROM external_usage_source_activations",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        database
            .execute(
                "UPDATE external_usage_sources SET owner=?1 WHERE owner<>?2",
                rusqlite::params![other, owner],
            )
            .unwrap();
    }
}

/// Supplies an existing expense to verify conservation independently of receipt
/// metadata. It is generated provider accounting, not an external usage claim.
fn existing_expense() -> TokenUsageEvent {
    TokenUsageEvent {
        id: "existing-expense".into(),
        project: None,
        observed_at_unix_seconds: 100,
        model: mez_agent::ModelTokenUsageKey::new("fixture", "model"),
        usage: mez_agent::ModelTokenUsage {
            input_tokens: 7,
            ..Default::default()
        },
    }
}

/// A version-7 source mapping is not evidence that native admission published.
/// Upgrading preserves its namespace, frozen request origin and expense while
/// creating an EMPTY activation ledger. Reopening cannot invent acceptance.
#[test]
fn external_source_activation_migration_keeps_legacy_mappings_inert() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let source = key("legacy");
    let owner = store.external_source_owner(&source).unwrap();
    let request = "a".repeat(64);
    let origin = store
        .begin_external_request(&owner, "pi", &request, None, 100)
        .unwrap();
    let event = existing_expense();
    store.append(&event).unwrap();
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database
        .execute_batch(
            "DROP TABLE IF EXISTS external_usage_source_activations; PRAGMA user_version=7;",
        )
        .unwrap();
    store.initialize(100).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_source_activations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("migration must create the inert activation ledger"),
        0
    );
    assert_eq!(
        database
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        8
    );
    assert_eq!(store.external_source_owner(&source).unwrap(), owner);
    assert_eq!(
        store
            .begin_external_request(&owner, "pi", &request, None, 101)
            .unwrap(),
        origin
    );
    assert!(
        !store.append(&event).unwrap(),
        "existing expense replay changed"
    );
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        7
    );
}

/// Fixed generated digests and clock values are representation fixtures, not
/// assertions about native ancestry, authority or the actual platform clock.
fn activation(attempt: u8, ancestry: u8, start: u64, end: u64) -> ExternalSourceActivation {
    ExternalSourceActivation::new([attempt; 32], [ancestry; 32], start, end).unwrap()
}

/// Preparation remains explicitly inert after reopen. Exact acceptance and
/// retirement retries reconcile the prior commit without incrementing again;
/// retirement is absorbing, including for the original valid preparation.
#[test]
fn external_source_activation_exact_lifecycle_and_lost_reply_reconciliation() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let source = key("lifecycle");
    store.external_source_owner(&source).unwrap();
    assert_eq!(store.external_source_activation(&source).unwrap(), None);
    let proposal = activation(1, 2, 100, 60_100);
    let prepared = store
        .prepare_external_source_activation(&source, &proposal)
        .unwrap();
    assert_eq!(prepared.state, ExternalSourceActivationState::Prepared);
    assert_eq!(prepared.revision, 1);
    assert!(!prepared.is_accepted_at(100));
    let reopened = TokenUsageStore::new(store.path());
    assert_eq!(
        reopened.external_source_activation(&source).unwrap(),
        Some(prepared.clone())
    );
    assert_eq!(
        reopened
            .prepare_external_source_activation(&source, &proposal)
            .unwrap(),
        prepared
    );
    let accepted = store
        .accept_external_source_activation(&source, &proposal, 1, 101)
        .unwrap();
    assert_eq!(accepted.state, ExternalSourceActivationState::Accepted);
    assert_eq!(accepted.revision, 2);
    assert!(accepted.is_accepted_at(101));
    assert_eq!(
        reopened
            .accept_external_source_activation(&source, &proposal, 1, 102)
            .unwrap(),
        accepted
    );
    assert_eq!(
        reopened
            .prepare_external_source_activation(&source, &proposal)
            .unwrap(),
        accepted
    );
    let retired = store
        .retire_external_source_activation(&source, &proposal, 2)
        .unwrap();
    assert_eq!(retired.state, ExternalSourceActivationState::Retired);
    assert_eq!(retired.revision, 3);
    assert!(!retired.is_accepted_at(102));
    assert_eq!(
        reopened
            .retire_external_source_activation(&source, &proposal, 2)
            .unwrap(),
        retired
    );
    assert!(
        store
            .prepare_external_source_activation(&source, &proposal)
            .is_err()
    );
    assert!(
        store
            .accept_external_source_activation(&source, &proposal, 1, 103)
            .is_err()
    );
    assert_eq!(
        reopened.external_source_activation(&source).unwrap(),
        Some(retired)
    );
}

/// Neither a mapping alone, a foreign source, changed ancestry/attempt/window
/// nor a stale revision can accept or retire another exact receipt. Abandoning
/// a merely prepared attempt may retire it but must never imply acceptance.
#[test]
fn external_source_activation_rejects_unprepared_changed_foreign_and_stale_work() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let source = key("one");
    let foreign = key("two");
    let unmapped = key("unmapped");
    store.external_source_owner(&source).unwrap();
    store.external_source_owner(&foreign).unwrap();
    let proposal = activation(1, 2, 100, 200);
    assert!(
        store
            .prepare_external_source_activation(&unmapped, &proposal)
            .is_err()
    );
    assert!(
        store
            .accept_external_source_activation(&source, &proposal, 1, 101)
            .is_err()
    );
    assert!(
        store
            .retire_external_source_activation(&source, &proposal, 1)
            .is_err()
    );
    let prepared = store
        .prepare_external_source_activation(&source, &proposal)
        .unwrap();
    for changed in [
        activation(3, 2, 100, 200),
        activation(1, 3, 100, 200),
        activation(1, 2, 101, 200),
        activation(1, 2, 100, 201),
    ] {
        assert!(
            store
                .prepare_external_source_activation(&source, &changed)
                .is_err()
        );
        assert!(
            store
                .accept_external_source_activation(&source, &changed, 1, 102)
                .is_err()
        );
        assert!(
            store
                .retire_external_source_activation(&source, &changed, 1)
                .is_err()
        );
    }
    assert!(
        store
            .accept_external_source_activation(&foreign, &proposal, 1, 102)
            .is_err()
    );
    assert!(
        store
            .retire_external_source_activation(&foreign, &proposal, 1)
            .is_err()
    );
    for stale in [0, 2, 3, u64::MAX] {
        assert!(
            store
                .accept_external_source_activation(&source, &proposal, stale, 102)
                .is_err()
        );
        assert!(
            store
                .retire_external_source_activation(&source, &proposal, stale)
                .is_err()
        );
    }
    assert_eq!(
        store.external_source_activation(&source).unwrap(),
        Some(prepared)
    );
    assert_eq!(store.external_source_activation(&foreign).unwrap(), None);
    let abandoned = store
        .retire_external_source_activation(&source, &proposal, 1)
        .unwrap();
    assert_eq!(abandoned.state, ExternalSourceActivationState::Retired);
    assert_eq!(abandoned.revision, 2);
    assert!(!abandoned.is_accepted_at(102));
}

/// A clock value before preparation, at/after expiry, or outside the integer
/// range cannot accept a source. Reopen/lookup never refreshes the original
/// window. Expired accepted facts remain tombstones rather than fresh capacity.
#[test]
fn external_source_activation_boot_window_is_exact_and_never_extended() {
    for (attempt, ancestry, start, end) in [
        ([0; 32], [2; 32], 100, 200),
        ([1; 32], [0; 32], 100, 200),
        ([1; 32], [2; 32], 0, 100),
        ([1; 32], [2; 32], 100, 100),
        ([1; 32], [2; 32], 100, 99),
        ([1; 32], [2; 32], 100, 60_101),
        ([1; 32], [2; 32], i64::MAX as u64, u64::MAX),
    ] {
        assert!(ExternalSourceActivation::new(attempt, ancestry, start, end).is_err());
    }
    let fixture = Fixture::new();
    let store = &fixture.store;
    let source = key("clock");
    store.external_source_owner(&source).unwrap();
    let proposal = activation(1, 2, 100, 60_100);
    store
        .prepare_external_source_activation(&source, &proposal)
        .unwrap();
    for now in [0, 99, 60_100, 60_101, u64::MAX] {
        assert!(
            store
                .accept_external_source_activation(&source, &proposal, 1, now)
                .is_err()
        );
    }
    let accepted = store
        .accept_external_source_activation(&source, &proposal, 1, 100)
        .unwrap();
    assert!(accepted.is_accepted_at(100));
    assert!(accepted.is_accepted_at(60_099));
    assert!(!accepted.is_accepted_at(99));
    assert!(!accepted.is_accepted_at(60_100));
    assert!(
        store
            .accept_external_source_activation(&source, &proposal, 1, 60_100)
            .is_err()
    );
    assert!(
        store
            .prepare_external_source_activation(&source, &activation(3, 2, 60_100, 60_200))
            .is_err()
    );
    assert_eq!(
        store.external_source_activation(&source).unwrap(),
        Some(accepted)
    );
    assert_eq!(
        store
            .retire_external_source_activation(&source, &proposal, 2)
            .unwrap()
            .state,
        ExternalSourceActivationState::Retired
    );
}

/// Simultaneous writers reconcile one preparation. Competing acceptance and
/// abandonment from the SAME revision have exactly one winner; the loser must
/// not mutate or reinterpret the winner. All storage calls own real writers.
#[test]
fn external_source_activation_concurrent_transitions_have_one_exact_winner() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let source = key("concurrent");
    store.external_source_owner(&source).unwrap();
    let proposal = activation(1, 2, 100, 200);
    let barrier = std::sync::Barrier::new(4);
    let prepared = std::thread::scope(|scope| {
        let tasks = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    store
                        .prepare_external_source_activation(&source, &proposal)
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        tasks
            .into_iter()
            .map(|task| task.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(
        prepared
            .iter()
            .all(|receipt| receipt == &prepared[0] && receipt.revision == 1)
    );
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let accept = scope.spawn(|| {
            barrier.wait();
            store.accept_external_source_activation(&source, &proposal, 1, 101)
        });
        let retire = scope.spawn(|| {
            barrier.wait();
            store.retire_external_source_activation(&source, &proposal, 1)
        });
        [accept.join().unwrap(), retire.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let winner = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(winner.revision, 2);
    assert_eq!(
        store.external_source_activation(&source).unwrap(),
        Some(winner)
    );
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_source_activations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

/// A full ledger refuses the next unaccepted source without evicting existing
/// acceptance/retirement facts. Exact retries still resolve at the cap; freeing
/// an accounting receipt elsewhere must not erase a lifetime tombstone.
#[test]
fn external_source_activation_capacity_preserves_existing_facts_and_tombstones() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let source = key("capacity-one");
    let other = key("capacity-two");
    store.external_source_owner(&source).unwrap();
    store.external_source_owner(&other).unwrap();
    let proposal = activation(1, 2, 100, 200);
    let prepared = store
        .prepare_external_source_activation_with_limit_for_tests(&source, &proposal, 1)
        .unwrap();
    assert_eq!(
        store
            .prepare_external_source_activation_with_limit_for_tests(&source, &proposal, 0)
            .unwrap(),
        prepared
    );
    let error = store
        .prepare_external_source_activation_with_limit_for_tests(&other, &proposal, 1)
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::RateLimited);
    assert_eq!(store.external_source_activation(&other).unwrap(), None);
    let retired = store
        .retire_external_source_activation(&source, &proposal, 1)
        .unwrap();
    assert!(
        store
            .prepare_external_source_activation_with_limit_for_tests(&other, &proposal, 1)
            .is_err()
    );
    store.initialize(200 * 86_400).unwrap();
    assert_eq!(
        store.external_source_activation(&source).unwrap(),
        Some(retired)
    );
}

/// Migration collision rolls back version advancement and leaves the old
/// namespace and independently committed expense untouched. The fixture never
/// treats a conflicting table as an empty accepted-source ledger.
#[test]
fn external_source_activation_migration_collision_rolls_back_without_recharge() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let owner = store
        .external_source_owner(&key("migration-collision"))
        .unwrap();
    store.append(&existing_expense()).unwrap();
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database
        .execute_batch(
            "DROP TABLE external_usage_source_activations;
        CREATE TABLE external_usage_source_activations(collision TEXT); PRAGMA user_version=7;",
        )
        .unwrap();
    assert!(store.initialize(100).is_err());
    assert_eq!(
        database
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert_eq!(
        database
            .query_row("SELECT owner FROM external_usage_sources", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        owner
    );
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        7
    );
}

/// SQLite write failure leaves no phantom preparation or acceptance. After the
/// generated fault is removed, the SAME operation can commit once; losing its
/// reply and retrying changes no revision or expense. These are storage faults,
/// not native admission or actor-publication qualification.
#[test]
fn external_source_activation_failed_writes_are_atomic_and_expense_neutral() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let source = key("failed-writes");
    store.external_source_owner(&source).unwrap();
    let proposal = activation(1, 2, 100, 200);
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database.execute_batch("CREATE TRIGGER fail_activation_insert AFTER INSERT ON external_usage_source_activations
        BEGIN SELECT RAISE(ABORT,'test-only activation insert refusal'); END;").unwrap();
    assert!(
        store
            .prepare_external_source_activation(&source, &proposal)
            .is_err()
    );
    assert_eq!(store.external_source_activation(&source).unwrap(), None);
    database
        .execute_batch("DROP TRIGGER fail_activation_insert;")
        .unwrap();
    let prepared = store
        .prepare_external_source_activation(&source, &proposal)
        .unwrap();
    database.execute_batch("CREATE TRIGGER fail_activation_update AFTER UPDATE ON external_usage_source_activations
        BEGIN SELECT RAISE(ABORT,'test-only activation update refusal'); END;").unwrap();
    assert!(
        store
            .accept_external_source_activation(&source, &proposal, 1, 101)
            .is_err()
    );
    assert!(
        store
            .retire_external_source_activation(&source, &proposal, 1)
            .is_err()
    );
    assert_eq!(
        store.external_source_activation(&source).unwrap(),
        Some(prepared)
    );
    database
        .execute_batch("DROP TRIGGER fail_activation_update;")
        .unwrap();
    let accepted = store
        .accept_external_source_activation(&source, &proposal, 1, 101)
        .unwrap();
    assert_eq!(
        store
            .accept_external_source_activation(&source, &proposal, 1, 102)
            .unwrap(),
        accepted
    );
    let retired = store
        .retire_external_source_activation(&source, &proposal, 2)
        .unwrap();
    assert_eq!(retired.revision, 3);
    for table in [
        "token_usage_events",
        "external_usage_requests",
        "external_usage_observations",
        "external_usage_receipts",
        "external_usage_streams",
    ] {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0,
            "activation metadata changed accounting table {table}"
        );
    }
}

/// Future schema prevents both reads and mutations, retaining the exact stored
/// prepared row and version. Refusal must not turn an existing fact into a
/// fabricated accepted/retired receipt or trigger a best-effort migration.
#[test]
fn external_source_activation_future_schema_preserves_original_facts() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let source = key("future");
    store.external_source_owner(&source).unwrap();
    let proposal = activation(1, 2, 100, 200);
    let prepared = store
        .prepare_external_source_activation(&source, &proposal)
        .unwrap();
    let database = rusqlite::Connection::open(store.path()).unwrap();
    let future = super::store::SCHEMA_VERSION + 1;
    database
        .pragma_update(None, "user_version", future)
        .unwrap();
    assert!(store.external_source_activation(&source).is_err());
    assert!(
        store
            .prepare_external_source_activation(&source, &proposal)
            .is_err()
    );
    assert!(
        store
            .accept_external_source_activation(&source, &proposal, 1, 101)
            .is_err()
    );
    assert!(
        store
            .retire_external_source_activation(&source, &proposal, 1)
            .is_err()
    );
    assert_eq!(
        database
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        future
    );
    assert_eq!(
        database
            .query_row(
                "SELECT revision,state FROM external_usage_source_activations",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (1, 0)
    );
    database
        .pragma_update(None, "user_version", super::store::SCHEMA_VERSION)
        .unwrap();
    assert_eq!(
        store.external_source_activation(&source).unwrap(),
        Some(prepared)
    );
}

/// Corrupted rows remain errors even when the generated database deliberately
/// bypasses its own CHECK constraints. Bounded blob reads reject oversize input
/// without materializing it as a receipt; revision exhaustion never wraps.
#[test]
fn external_source_activation_corrupt_rows_and_revision_exhaustion_fail_closed() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let source = key("corrupt");
    store.external_source_owner(&source).unwrap();
    let proposal = activation(1, 2, 100, 200);
    store
        .prepare_external_source_activation(&source, &proposal)
        .unwrap();
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database
        .execute_batch("PRAGMA ignore_check_constraints=ON;")
        .unwrap();
    for corruption in [
        "state=99",
        "revision=0",
        "prepared_at_boot_ms=0",
        "expires_at_boot_ms=60101",
        "attempt=zeroblob(1048576)",
        "ancestry=zeroblob(32)",
    ] {
        database
            .execute_batch(&format!(
                "UPDATE external_usage_source_activations SET {corruption}"
            ))
            .unwrap();
        assert!(
            store.external_source_activation(&source).is_err(),
            "corruption was accepted: {corruption}"
        );
        assert!(
            store
                .accept_external_source_activation(&source, &proposal, 1, 101)
                .is_err()
        );
        database
            .execute(
                "UPDATE external_usage_source_activations SET attempt=?1,ancestry=?2,
            prepared_at_boot_ms=100,expires_at_boot_ms=200,revision=1,state=0",
                rusqlite::params![[1_u8; 32].as_slice(), [2_u8; 32].as_slice()],
            )
            .unwrap();
    }
    database
        .execute(
            "UPDATE external_usage_source_activations SET revision=?1",
            [i64::MAX],
        )
        .unwrap();
    assert!(
        store
            .retire_external_source_activation(&source, &proposal, i64::MAX as u64)
            .is_err()
    );
    assert_eq!(
        store
            .external_source_activation(&source)
            .unwrap()
            .unwrap()
            .revision,
        i64::MAX as u64
    );
}
