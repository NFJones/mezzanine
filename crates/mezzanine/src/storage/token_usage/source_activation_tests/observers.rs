//! Storage-only public observer identity, CAS, migration and bounded decoding.

use super::*;
use crate::storage::token_usage::ExternalSourceObserverFence;

/// An older repair cannot clear a later pre-admission intent. Exact current
/// full-history repair preserves native lifetime/expense and includes skipped
/// IDs without weakening the codec's bounded counter/seen-set invariants.
#[test]
fn external_source_history_newer_intent_fences_older_repair() {
    let (fixture, source, activation, first) = accepted();
    let before = fixture
        .store
        .external_source_activation(&source)
        .unwrap()
        .unwrap();
    fixture
        .store
        .mark_external_source_history_gap(&source, &activation, &first, [11; 32])
        .unwrap();
    let older = first
        .propose_with_history(
            1,
            "B",
            [12; 32],
            &std::collections::BTreeSet::from([
                "PUBLIC_OBSERVER_CANARY".to_owned(),
                "B".to_owned(),
            ]),
        )
        .unwrap();
    fixture
        .store
        .mark_external_source_history_gap(&source, &activation, &first, [13; 32])
        .unwrap();
    assert!(
        fixture
            .store
            .advance_external_source_observer_history(
                &source,
                &activation,
                2,
                &older,
                1002,
                Some([11; 32])
            )
            .is_err()
    );
    assert!(
        !fixture
            .store
            .external_source_history_complete(&source)
            .unwrap()
    );
    assert_eq!(
        fixture.store.external_source_activation(&source).unwrap(),
        Some(before)
    );
    let all = std::collections::BTreeSet::from([
        "PUBLIC_OBSERVER_CANARY".to_owned(),
        "B".to_owned(),
        "C".to_owned(),
    ]);
    let newer = first.propose_with_history(1, "C", [14; 32], &all).unwrap();
    let repaired = fixture
        .store
        .advance_external_source_observer_history(
            &source,
            &activation,
            2,
            &newer,
            1003,
            Some([13; 32]),
        )
        .unwrap();
    assert!(
        fixture
            .store
            .external_source_history_complete(&source)
            .unwrap()
    );
    assert_eq!(repaired.activation, activation);
    assert_eq!(repaired.revision, 2);
    assert!(repaired.renewal.is_none());
    assert!(
        repaired
            .observer
            .as_ref()
            .unwrap()
            .contains_instance("B")
            .unwrap()
    );
    assert!(
        fixture
            .store
            .advance_external_source_observer_history(
                &source,
                &activation,
                2,
                &older,
                1004,
                Some([11; 32])
            )
            .is_err()
    );
    assert_eq!(
        fixture.store.external_source_activation(&source).unwrap(),
        Some(repaired)
    );
}

/// Old optional-publication rows cannot retroactively prove complete history.
/// Readonly refuses schema10; intentional write migration preserves every
/// original fact while marking legacy completeness unknown, repeatedly inert.
#[test]
fn external_source_history_schema10_migration_never_invents_completeness() {
    let (fixture, source, activation, _) = accepted();
    let before = fixture
        .store
        .external_source_activation(&source)
        .unwrap()
        .unwrap();
    let database = rusqlite::Connection::open(fixture.store.path()).unwrap();
    database.execute_batch("DROP TABLE external_usage_view_resets; ALTER TABLE external_usage_source_activations DROP COLUMN observer_history_gap; PRAGMA user_version=10;").unwrap();
    assert!(
        fixture
            .store
            .external_source_history_complete(&source)
            .is_err()
    );
    assert_eq!(
        database
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        10
    );
    assert_eq!(
        fixture
            .store
            .prepare_external_source_activation(&source, &activation)
            .unwrap(),
        before
    );
    assert!(
        !fixture
            .store
            .external_source_history_complete(&source)
            .unwrap()
    );
    assert_eq!(
        database
            .query_row(
                "SELECT observer_history_gap FROM external_usage_source_activations",
                [],
                |row| row.get::<_, Vec<u8>>(0)
            )
            .unwrap(),
        vec![0; 32]
    );
    assert_eq!(
        fixture
            .store
            .prepare_external_source_activation(&source, &activation)
            .unwrap(),
        before
    );
    assert!(
        !fixture
            .store
            .external_source_history_complete(&source)
            .unwrap()
    );
}

/// Owns one generated accepted row; synthetic metadata is not native authority.
fn accepted() -> (
    Fixture,
    ExternalSourceKey,
    ExternalSourceActivation,
    ExternalSourceObserverFence,
) {
    let fixture = Fixture::new();
    let source = key("public-observer");
    let activation = ExternalSourceActivation::new([1; 32], [2; 32], 1000, 61_000).unwrap();
    let fence = ExternalSourceObserverFence::initial("PUBLIC_OBSERVER_CANARY", [3; 32]).unwrap();
    fixture.store.external_source_owner(&source).unwrap();
    fixture
        .store
        .prepare_external_source_activation(&source, &activation)
        .unwrap();
    fixture
        .store
        .accept_external_source_activation_with_observer(
            &source,
            &activation,
            1,
            1001,
            Some(&fence),
        )
        .unwrap();
    (fixture, source, activation, fence)
}

/// Prepared never carries an accepted observer. Acceptance and first public
/// fence commit atomically, while identical replay makes no second SQL UPDATE.
#[test]
fn external_source_observer_acceptance_is_atomic_and_exact() {
    let fixture = Fixture::new();
    let source = key("initial-public-observer");
    let activation = ExternalSourceActivation::new([1; 32], [2; 32], 1000, 61_000).unwrap();
    fixture.store.external_source_owner(&source).unwrap();
    let prepared = fixture
        .store
        .prepare_external_source_activation(&source, &activation)
        .unwrap();
    assert!(prepared.observer.is_none());
    let database = rusqlite::Connection::open(fixture.store.path()).unwrap();
    database.execute_batch("CREATE TABLE updates(n INTEGER); INSERT INTO updates VALUES(0); CREATE TRIGGER count_updates AFTER UPDATE ON external_usage_source_activations BEGIN UPDATE updates SET n=n+1; END;").unwrap();
    let fence = ExternalSourceObserverFence::initial("PUBLIC_OBSERVER_CANARY", [3; 32]).unwrap();
    let one = fixture
        .store
        .accept_external_source_activation_with_observer(
            &source,
            &activation,
            1,
            1001,
            Some(&fence),
        )
        .unwrap();
    let repeated = fixture
        .store
        .accept_external_source_activation_with_observer(
            &source,
            &activation,
            1,
            1002,
            Some(&fence),
        )
        .unwrap();
    assert_eq!(one, repeated);
    assert_eq!(one.observer, Some(fence));
    assert_eq!(
        database
            .query_row("SELECT n FROM updates", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let bytes: Vec<u8> = database
        .query_row(
            "SELECT observer_fence FROM external_usage_source_activations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bytes.len(), 107);
    assert!(
        !bytes
            .windows(b"PUBLIC_OBSERVER_CANARY".len())
            .any(|part| part == b"PUBLIC_OBSERVER_CANARY")
    );
    assert!(!format!("{:?}", one.observer).contains("PUBLIC_OBSERVER_CANARY"));
    let changed = ExternalSourceObserverFence::initial("different", [4; 32]).unwrap();
    assert!(
        fixture
            .store
            .accept_external_source_activation_with_observer(
                &source,
                &activation,
                1,
                1003,
                Some(&changed)
            )
            .is_err()
    );
}

/// Public generation/operation changes have their own exact CAS; lifetime
/// revision and immutable source/window do not move. Stale contenders and seen
/// old instances cannot supersede a committed successor or replay an UPDATE.
#[test]
fn external_source_observer_cas_replay_and_seen_fences_are_exact() {
    let (fixture, source, activation, first) = accepted();
    let database = rusqlite::Connection::open(fixture.store.path()).unwrap();
    database.execute_batch("CREATE TABLE updates(n INTEGER); INSERT INTO updates VALUES(0); CREATE TRIGGER count_updates AFTER UPDATE ON external_usage_source_activations BEGIN UPDATE updates SET n=n+1; END;").unwrap();
    let proposal = first.propose(1, "second", [4; 32]).unwrap();
    let stale = first.propose(1, "competitor", [5; 32]).unwrap();
    let second = fixture
        .store
        .advance_external_source_observer(&source, &activation, 2, &proposal, 1002)
        .unwrap();
    assert_eq!(second.revision, 2);
    assert_eq!(second.activation, activation);
    assert!(second.renewal.is_none());
    assert_eq!(second.observer.as_ref().unwrap().generation(), 2);
    assert_eq!(
        fixture
            .store
            .advance_external_source_observer(&source, &activation, 2, &proposal, 1003)
            .unwrap(),
        second
    );
    assert!(
        fixture
            .store
            .advance_external_source_observer(&source, &activation, 2, &stale, 1004)
            .is_err()
    );
    let fence = second.observer.unwrap();
    assert!(fence.propose(2, "PUBLIC_OBSERVER_CANARY", [6; 32]).is_err());
    assert!(fence.propose(1, "third", [6; 32]).is_err());
    let same = fence.propose(2, "second", [7; 32]).unwrap();
    assert_eq!(
        fixture
            .store
            .advance_external_source_observer(&source, &activation, 2, &same, 1005)
            .unwrap()
            .observer
            .unwrap()
            .generation(),
        3
    );
    assert_eq!(
        database
            .query_row("SELECT n FROM updates", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
}

/// Native lifetime renewal preserves public fences; old physical revisions
/// refuse until a new caller-owned qualification. Retirement preserves the
/// fence and makes every observer mutation absorbing-unavailable after reopen.
#[test]
fn external_source_observer_renewal_retirement_and_expiry_conserve() {
    let (fixture, source, activation, first) = accepted();
    let proposal = first.propose(1, "second", [4; 32]).unwrap();
    let renewal = ExternalSourceRenewal::new(1100, 61_100).unwrap();
    let renewed = fixture
        .store
        .renew_external_source_activation(&source, &activation, 2, &renewal, 1101)
        .unwrap();
    assert_eq!(renewed.observer, Some(first));
    assert!(
        fixture
            .store
            .advance_external_source_observer(&source, &activation, 2, &proposal, 1102)
            .is_err()
    );
    let second = fixture
        .store
        .advance_external_source_observer(&source, &activation, 3, &proposal, 1103)
        .unwrap();
    assert_eq!(second.renewal, Some(renewal));
    assert!(
        fixture
            .store
            .advance_external_source_observer(&source, &activation, 3, &proposal, 61_100)
            .is_err()
    );
    let retired = fixture
        .store
        .retire_external_source_activation_with_observer(
            &source,
            &activation,
            3,
            second.observer.as_ref(),
        )
        .unwrap();
    assert_eq!(retired.observer, second.observer);
    let reopened = TokenUsageStore::new(fixture.store.path());
    assert_eq!(
        reopened.external_source_activation(&source).unwrap(),
        Some(retired)
    );
    assert!(
        reopened
            .advance_external_source_observer(&source, &activation, 3, &proposal, 1104)
            .is_err()
    );
}

/// Retirement must name the exact public fence even when native revision did
/// not change. None is not a wildcard, and the wrong observer cannot adopt an
/// already-committed same-revision tombstone as its own durable acknowledgment.
#[test]
fn external_source_observer_retirement_binds_fence_and_exact_retry() {
    let (fixture, source, activation, first) = accepted();
    let proposal = first.propose(1, "second", [4; 32]).unwrap();
    let second = fixture
        .store
        .advance_external_source_observer(&source, &activation, 2, &proposal, 1002)
        .unwrap();
    for observer in [None, Some(&first)] {
        assert!(
            fixture
                .store
                .retire_external_source_activation_with_observer(&source, &activation, 2, observer)
                .is_err()
        );
        assert_eq!(
            fixture.store.external_source_activation(&source).unwrap(),
            Some(second.clone())
        );
    }
    let retired = fixture
        .store
        .retire_external_source_activation_with_observer(
            &source,
            &activation,
            2,
            second.observer.as_ref(),
        )
        .unwrap();
    assert_eq!(retired.revision, 3);
    assert_eq!(
        fixture
            .store
            .retire_external_source_activation_with_observer(
                &source,
                &activation,
                2,
                second.observer.as_ref()
            )
            .unwrap(),
        retired
    );
    assert!(
        fixture
            .store
            .retire_external_source_activation_with_observer(&source, &activation, 2, Some(&first))
            .is_err()
    );
    assert!(
        fixture
            .store
            .retire_external_source_activation_with_observer(&source, &activation, 2, None)
            .is_err()
    );
}

/// Competing public handoff and original retirement share one actual SQLite
/// writer boundary. Either the original exact fence retires or the public fence
/// changes; the loser cannot also mutate or retire the winning successor.
#[test]
fn external_source_observer_retirement_races_handoff_without_two_winners() {
    let (fixture, source, activation, first) = accepted();
    let proposal = first.propose(1, "second", [4; 32]).unwrap();
    let gate = std::sync::Barrier::new(2);
    let (retired, changed) = std::thread::scope(|scope| {
        let retire = scope.spawn(|| {
            gate.wait();
            fixture
                .store
                .retire_external_source_activation_with_observer(
                    &source,
                    &activation,
                    2,
                    Some(&first),
                )
        });
        let change = scope.spawn(|| {
            gate.wait();
            fixture
                .store
                .advance_external_source_observer(&source, &activation, 2, &proposal, 1002)
        });
        (retire.join().unwrap(), change.join().unwrap())
    });
    assert_eq!(
        usize::from(retired.is_ok()) + usize::from(changed.is_ok()),
        1
    );
    let winner = retired.or(changed).unwrap();
    assert_eq!(
        fixture.store.external_source_activation(&source).unwrap(),
        Some(winner)
    );
}

/// The128-instance set never evicts tombstones to enable takeover. Same-current
/// requalification may advance its bounded integer without growing that set;
/// generation exhaustion and malformed public IDs fail before any SQL change.
#[test]
fn external_source_observer_capacity_and_generation_are_bounded() {
    let (fixture, source, activation, mut fence) = accepted();
    for n in 1..128u16 {
        let mut operation = [0; 32];
        operation[..2].copy_from_slice(&n.to_be_bytes());
        let proposal = fence
            .propose(fence.generation(), &format!("observer-{n}"), operation)
            .unwrap();
        fence = fixture
            .store
            .advance_external_source_observer(&source, &activation, 2, &proposal, 1002)
            .unwrap()
            .observer
            .unwrap();
    }
    assert!(fence.propose(128, "overflow", [9; 32]).is_err());
    assert!(
        fence
            .propose(128, "PUBLIC_OBSERVER_CANARY", [9; 32])
            .is_err()
    );
    let same = fence.propose(128, "observer-127", [9; 32]).unwrap();
    let final_fence = fixture
        .store
        .advance_external_source_observer(&source, &activation, 2, &same, 1003)
        .unwrap()
        .observer
        .unwrap();
    assert_eq!(final_fence.generation(), 129);
    let database = rusqlite::Connection::open(fixture.store.path()).unwrap();
    let mut bytes: Vec<u8> = database
        .query_row(
            "SELECT observer_fence FROM external_usage_source_activations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bytes.len(), 4171);
    bytes[1..9].copy_from_slice(&(i64::MAX as u64).to_be_bytes());
    database
        .execute(
            "UPDATE external_usage_source_activations SET observer_fence=?1",
            [bytes],
        )
        .unwrap();
    let exhausted = fixture
        .store
        .external_source_activation(&source)
        .unwrap()
        .unwrap()
        .observer
        .unwrap();
    assert!(
        exhausted
            .propose(i64::MAX as u64, "observer-127", [10; 32])
            .is_err()
    );
    for invalid in ["", "bad\0id", "bad\nid"] {
        assert!(ExternalSourceObserverFence::initial(invalid, [1; 32]).is_err());
    }
    assert!(ExternalSourceObserverFence::initial("valid", [0; 32]).is_err());
}

/// Bounded decoding rejects wrong storage types, future/partial headers,
/// duplicate/unsorted/missing-current sets and impossible stages even when
/// generated-fixture SQL checks are bypassed. No corrupt field is echoed.
#[test]
fn external_source_observer_codec_rejects_damaged_facts() {
    let (fixture, source, activation, first) = accepted();
    let proposal = first.propose(1, "second", [4; 32]).unwrap();
    fixture
        .store
        .advance_external_source_observer(&source, &activation, 2, &proposal, 1002)
        .unwrap();
    let database = rusqlite::Connection::open(fixture.store.path()).unwrap();
    let original: Vec<u8> = database
        .query_row(
            "SELECT observer_fence FROM external_usage_source_activations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut cases = vec![vec![], vec![1; 4172], original[..100].to_vec()];
    let mut future = original.clone();
    future[0] = 2;
    cases.push(future);
    let mut zero = original.clone();
    zero[1..9].fill(0);
    cases.push(zero);
    let mut missing = original.clone();
    missing[9..41].fill(9);
    cases.push(missing);
    let mut duplicate = original.clone();
    duplicate[107..139].copy_from_slice(&original[75..107]);
    cases.push(duplicate);
    let mut unordered = original.clone();
    unordered[75..107].copy_from_slice(&original[107..139]);
    unordered[107..139].copy_from_slice(&original[75..107]);
    cases.push(unordered);
    let mut trailing = original.clone();
    trailing.push(0);
    cases.push(trailing);
    database
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    for bytes in cases {
        database
            .execute(
                "UPDATE external_usage_source_activations SET observer_fence=?1",
                [bytes],
            )
            .unwrap();
        assert!(fixture.store.external_source_activation(&source).is_err());
    }
    database
        .execute(
            "UPDATE external_usage_source_activations SET observer_fence=?1",
            ["PUBLIC_CORRUPTION_CANARY"],
        )
        .unwrap();
    let error = fixture
        .store
        .external_source_activation(&source)
        .unwrap_err();
    assert!(!error.message().contains("PUBLIC_CORRUPTION_CANARY"));
    database
        .execute(
            "UPDATE external_usage_source_activations SET observer_fence=?1,state=0,revision=1",
            [original],
        )
        .unwrap();
    assert!(fixture.store.external_source_activation(&source).is_err());
}

/// Schema9 upgrades preserve all native/owner/window/checkpoint bytes but leave
/// public observer NULL unknown. Read-only inspection never silently migrates,
/// and an accepted legacy namespace cannot bootstrap a new public fence.
#[test]
fn external_source_observer_schema9_migration_is_inert() {
    let (fixture, source, activation, _first) = accepted();
    let owner = fixture.store.external_source_owner(&source).unwrap();
    fixture.store.append(&existing_expense()).unwrap();
    let marker = "a".repeat(64);
    let origin = fixture
        .store
        .begin_external_request(&owner, "pi", &marker, None, 100)
        .unwrap();
    let renewal = ExternalSourceRenewal::new(1100, 61_100).unwrap();
    fixture
        .store
        .renew_external_source_activation(&source, &activation, 2, &renewal, 1101)
        .unwrap();
    let database = rusqlite::Connection::open(fixture.store.path()).unwrap();
    database.execute_batch("DROP TABLE external_usage_view_resets; ALTER TABLE external_usage_source_activations DROP COLUMN observer_history_gap; ALTER TABLE external_usage_source_activations DROP COLUMN observer_fence; PRAGMA user_version=9;").unwrap();
    assert!(fixture.store.external_source_activation(&source).is_err());
    let receipt = fixture
        .store
        .prepare_external_source_activation(&source, &activation)
        .unwrap();
    assert_eq!(receipt.activation, activation);
    assert_eq!(receipt.revision, 3);
    assert_eq!(receipt.renewal, Some(renewal));
    assert_eq!(fixture.store.external_source_owner(&source).unwrap(), owner);
    assert_eq!(
        fixture
            .store
            .begin_external_request(&owner, "pi", &marker, None, 101)
            .unwrap(),
        origin
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*),sum(input_tokens) FROM token_usage_events",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (1, 7)
    );
    assert!(receipt.observer.is_none());
    assert_eq!(
        database
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        super::super::store::SCHEMA_VERSION
    );
    let invented = ExternalSourceObserverFence::initial("invented", [9; 32])
        .unwrap()
        .propose(1, "next", [10; 32])
        .unwrap();
    assert!(
        fixture
            .store
            .advance_external_source_observer(&source, &activation, 3, &invented, 1102)
            .is_err()
    );
    assert!(
        fixture
            .store
            .external_source_activation(&source)
            .unwrap()
            .unwrap()
            .observer
            .is_none()
    );
}

/// Two actual independent SQLite writers cannot both replace the same expected
/// public fence, even though native lifetime revision remains unchanged. The
/// winning source/clock record survives and the losing intent is not replayed.
#[test]
fn external_source_observer_competing_writers_have_one_winner() {
    let (fixture, source, activation, first) = accepted();
    let proposals = [
        first.propose(1, "left", [7; 32]).unwrap(),
        first.propose(1, "right", [8; 32]).unwrap(),
    ];
    let gate = std::sync::Arc::new(std::sync::Barrier::new(3));
    let workers = proposals
        .into_iter()
        .map(|proposal| {
            let (gate, store, source, activation) = (
                gate.clone(),
                fixture.store.clone(),
                source.clone(),
                activation.clone(),
            );
            std::thread::spawn(move || {
                gate.wait();
                store.advance_external_source_observer(&source, &activation, 2, &proposal, 1002)
            })
        })
        .collect::<Vec<_>>();
    gate.wait();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let winner = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(winner.revision, 2);
    assert_eq!(winner.observer.as_ref().unwrap().generation(), 2);
    assert_eq!(
        fixture.store.external_source_activation(&source).unwrap(),
        Some(winner)
    );
}

/// A real failed observer UPDATE preserves original facts and existing expense.
/// Removing only the fixture fault allows one explicit storage intent commit;
/// its exact retry returns evidence without a second write or new clock window.
#[test]
fn external_source_observer_sql_failure_preserves_original_expense() {
    let (fixture, source, activation, first) = accepted();
    fixture.store.append(&existing_expense()).unwrap();
    let original = fixture
        .store
        .external_source_activation(&source)
        .unwrap()
        .unwrap();
    let proposal = first.propose(1, "second", [4; 32]).unwrap();
    let database = rusqlite::Connection::open(fixture.store.path()).unwrap();
    database.execute_batch("CREATE TRIGGER reject_observer BEFORE UPDATE ON external_usage_source_activations BEGIN SELECT RAISE(ABORT,'observer fixture refusal'); END;").unwrap();
    let error = fixture
        .store
        .advance_external_source_observer(&source, &activation, 2, &proposal, 1002)
        .unwrap_err();
    assert!(error.message().contains("observer fixture refusal"));
    assert_eq!(
        fixture.store.external_source_activation(&source).unwrap(),
        Some(original)
    );
    database
        .execute_batch("DROP TRIGGER reject_observer")
        .unwrap();
    let committed = fixture
        .store
        .advance_external_source_observer(&source, &activation, 2, &proposal, 1003)
        .unwrap();
    assert_eq!(
        fixture
            .store
            .advance_external_source_observer(&source, &activation, 2, &proposal, 1004)
            .unwrap(),
        committed
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*),sum(input_tokens) FROM token_usage_events",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (1, 7)
    );
}

/// Inconsistent migration/future schema refuses without partial publication or
/// modifying accepted public bytes. No malformed-store repair or fallback runs.
#[test]
fn external_source_observer_migration_failure_preserves_existing_rows() {
    let (fixture, source, activation, first) = accepted();
    let database = rusqlite::Connection::open(fixture.store.path()).unwrap();
    database.execute_batch("PRAGMA user_version=9").unwrap(); // Existing column deliberately conflicts.
    assert!(
        fixture
            .store
            .prepare_external_source_activation(&source, &activation)
            .is_err()
    );
    assert_eq!(
        database
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        9
    );
    database
        .execute_batch(&format!(
            "PRAGMA user_version={}",
            super::super::store::SCHEMA_VERSION + 1
        ))
        .unwrap();
    assert!(fixture.store.external_source_activation(&source).is_err());
    assert!(
        fixture
            .store
            .prepare_external_source_activation(&source, &activation)
            .is_err()
    );
    database
        .execute_batch(&format!(
            "PRAGMA user_version={}",
            super::super::store::SCHEMA_VERSION
        ))
        .unwrap();
    assert_eq!(
        fixture
            .store
            .external_source_activation(&source)
            .unwrap()
            .unwrap()
            .observer,
        Some(first)
    );
}
