//! Repository-level config projection; no file write/durability, runtime policy
//! adoption or notification success is inferred from these synthetic digests.

use super::*;
use crate::storage::token_usage::budget::policy_projection::*;
use mez_agent::token_budget::{BudgetLimits, TokenLimit};

/// Independent workers contend for the same pending singleton; one operation
/// wins, and the other cannot replace its original source or publication fence.
#[test]
fn budget_policy_projection_competing_workers_have_one_original_winner() {
    let f = Fixture::new();
    let left = publication(&f, "left", 0, 'a', 'b');
    let right = publication(&f, "right", 0, 'a', 'c');
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            f.store.begin_budget_policy_publication(&left)
        });
        let b = scope.spawn(|| {
            barrier.wait();
            TokenUsageStore::new(f.store.path()).begin_budget_policy_publication(&right)
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(results.0.is_ok(), results.1.is_ok());
    let winner = if results.0.is_ok() { left } else { right };
    assert_eq!(
        f.store.budget_policy_projection().unwrap().pending,
        Some(winner.clone())
    );
    f.store
        .reconcile_budget_policy_publication(&winner, &winner.proposed.digest)
        .unwrap();
    assert_eq!(
        f.store.budget_policy_projection().unwrap().committed,
        Some(winner.proposed)
    );
}

/// Failed validation/overflow may not create receipts or alter the current
/// projection. A colliding migration object is not adopted as trusted state.
#[test]
fn budget_policy_projection_validation_overflow_and_schema_collision_are_inert() {
    let f = Fixture::new();
    let original = f.store.budget_policy_projection().unwrap();
    let op = publication(&f, "original", 0, 'a', 'b');
    let mut invalid = op.clone();
    invalid.proposed.digest = "B".repeat(64);
    assert!(f.store.begin_budget_policy_publication(&invalid).is_err());
    invalid = op.clone();
    invalid.namespace = "foreign".into();
    assert!(f.store.begin_budget_policy_publication(&invalid).is_err());
    invalid = op.clone();
    invalid.proposed.limits.input = TokenLimit::Limited(u64::MAX);
    assert!(f.store.begin_budget_policy_publication(&invalid).is_err());
    assert_eq!(f.store.budget_policy_projection().unwrap(), original);
    assert_eq!(
        f.store.budget_policy_publication_outcome(&op.id).unwrap(),
        None
    );
    let db = Connection::open(f.store.path()).unwrap();
    let mut overflow = original.clone();
    overflow.revision = u64::MAX;
    db.execute(
        "UPDATE budget_policy_projection SET body=?1",
        [encode(&overflow).unwrap()],
    )
    .unwrap();
    let mut next = op;
    next.expected_revision = u64::MAX;
    assert!(f.store.begin_budget_policy_publication(&next).is_err());
    assert_eq!(f.store.budget_policy_projection().unwrap(), overflow);
    db.execute_batch("DROP TABLE budget_policy_projection; DROP TABLE budget_policy_publications; PRAGMA user_version=15; CREATE TABLE budget_policy_projection(collision TEXT); INSERT INTO budget_policy_projection VALUES('sentinel');").unwrap();
    assert!(f.store.initialize(100).is_err());
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        15
    );
    assert_eq!(
        db.query_row("SELECT collision FROM budget_policy_projection", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "sentinel"
    );
    assert!(
        db.prepare("SELECT * FROM budget_policy_publications")
            .is_err()
    );
    drop(db);
}

/// An unapplied conflict-resolution write leaves the concurrently edited file
/// present. The older committed projection cannot authorize that third digest.
#[test]
fn budget_policy_projection_unapplied_conflict_recovery_does_not_revive_stale_policy() {
    let f = Fixture::new();
    let initial = publication(&f, "initial", 0, 'a', 'b');
    f.store.begin_budget_policy_publication(&initial).unwrap();
    let committed = f
        .store
        .reconcile_budget_policy_publication(&initial, &initial.proposed.digest)
        .unwrap();
    let changed = publication(&f, "changed", committed.revision, 'b', 'c');
    f.store.begin_budget_policy_publication(&changed).unwrap();
    let conflict = f
        .store
        .reconcile_budget_policy_publication(&changed, &"d".repeat(64))
        .unwrap();
    let mut recovery = publication(&f, "recovery", conflict.revision, 'd', 'e');
    recovery.supersedes = Some(changed.id);
    f.store.begin_budget_policy_publication(&recovery).unwrap();
    f.store
        .reconcile_budget_policy_publication(&recovery, &recovery.previous_digest)
        .unwrap();
    assert_eq!(
        f.store
            .budget_policy_projection()
            .unwrap()
            .admission_policy(),
        None
    );
}

fn publication(
    f: &Fixture,
    id: &str,
    revision: u64,
    old: char,
    new: char,
) -> BudgetPolicyPublication {
    BudgetPolicyPublication {
        namespace: f.store.budget_namespace().unwrap(),
        id: BudgetReceiptId::new(id).unwrap(),
        expected_revision: revision,
        previous_digest: old.to_string().repeat(64),
        proposed: BudgetConfigPolicy {
            source: "primary-config-original".into(),
            digest: new.to_string().repeat(64),
            limits: BudgetLimits {
                input: TokenLimit::Limited(100),
                output: TokenLimit::Unlimited,
            },
        },
        supersedes: None,
    }
}

#[test]
fn budget_policy_projection_pending_reopen_and_original_receipts_are_exact() {
    let f = Fixture::new();
    let peer = TokenUsageStore::new(f.store.path());
    assert_eq!(
        peer.budget_policy_projection().unwrap().admission_policy(),
        None
    );
    let op = publication(&f, "first", 0, 'a', 'b');
    let pending = f.store.begin_budget_policy_publication(&op).unwrap();
    assert_eq!(pending.status, BudgetPolicyPublicationStatus::Updating);
    assert_eq!(
        peer.budget_policy_projection().unwrap().admission_policy(),
        None
    );
    assert_eq!(peer.begin_budget_policy_publication(&op).unwrap(), pending);
    let mut changed = op.clone();
    changed.proposed.limits.output = TokenLimit::Limited(5);
    assert!(peer.begin_budget_policy_publication(&changed).is_err());
    assert!(
        peer.begin_budget_policy_publication(&publication(&f, "competing", 1, 'a', 'c'))
            .is_err()
    );
    let committed = peer
        .reconcile_budget_policy_publication(&op, &op.proposed.digest)
        .unwrap();
    assert_eq!(committed.status, BudgetPolicyPublicationStatus::Committed);
    assert_eq!(
        f.store
            .budget_policy_projection()
            .unwrap()
            .admission_policy(),
        Some(op.proposed.limits)
    );
    let next = publication(&f, "second", committed.revision, 'b', 'c');
    peer.begin_budget_policy_publication(&next).unwrap();
    let unapplied = peer
        .reconcile_budget_policy_publication(&next, &next.previous_digest)
        .unwrap();
    assert_eq!(unapplied.status, BudgetPolicyPublicationStatus::Unapplied);
    assert_eq!(
        f.store.budget_policy_projection().unwrap().committed,
        Some(op.proposed.clone())
    );
    // Original outcome stays immutable after later operations and lost replies.
    assert_eq!(
        f.store.begin_budget_policy_publication(&op).unwrap(),
        committed
    );
    assert_eq!(
        f.store.budget_policy_publication_outcome(&op.id).unwrap(),
        Some(committed)
    );
    assert_eq!(
        peer.reconcile_budget_policy_publication(&next, &next.proposed.digest)
            .unwrap(),
        unapplied
    );
}

#[test]
fn budget_policy_projection_third_digest_requires_explicit_validated_reconciliation() {
    let f = Fixture::new();
    let op = publication(&f, "first", 0, 'a', 'b');
    f.store.begin_budget_policy_publication(&op).unwrap();
    let conflict = f
        .store
        .reconcile_budget_policy_publication(&op, &"c".repeat(64))
        .unwrap();
    assert!(matches!(
        conflict.status,
        BudgetPolicyPublicationStatus::Conflict { .. }
    ));
    assert_eq!(
        f.store
            .budget_policy_projection()
            .unwrap()
            .admission_policy(),
        None
    );
    let mut recovery = publication(&f, "validated-recovery", conflict.revision, 'c', 'd');
    assert!(f.store.begin_budget_policy_publication(&recovery).is_err());
    recovery.supersedes = Some(op.id.clone());
    let mut wrong = recovery.clone();
    wrong.proposed.source = "another-config".into();
    assert!(f.store.begin_budget_policy_publication(&wrong).is_err());
    wrong = recovery.clone();
    wrong.previous_digest = "e".repeat(64);
    assert!(f.store.begin_budget_policy_publication(&wrong).is_err());
    f.store.begin_budget_policy_publication(&recovery).unwrap();
    f.store
        .reconcile_budget_policy_publication(&recovery, &recovery.proposed.digest)
        .unwrap();
    let projection = f.store.budget_policy_projection().unwrap();
    assert_eq!(projection.committed, Some(recovery.proposed));
    assert!(projection.admission_policy().is_some());
    assert_eq!(
        f.store.budget_policy_publication_outcome(&op.id).unwrap(),
        Some(conflict)
    );
}

#[test]
fn budget_policy_projection_migration_preserves_lifetime_and_rejects_missing_future_store() {
    let f = Fixture::new();
    let member = f.member("original", None);
    f.active(&member);
    let original = attempt(&member, "attempt-original");
    f.store.register_budget_attempt(&original).unwrap();
    let usage = receipt(&original, "known-original", Some(77), Some(9), true);
    f.store.commit_budget_receipt(&usage).unwrap();
    let account = f.store.budget_account(&member.root).unwrap();
    let namespace = f.store.budget_namespace().unwrap();
    let db = Connection::open(f.store.path()).unwrap();
    db.execute_batch("DROP TABLE budget_policy_projection; DROP TABLE budget_policy_publications; PRAGMA user_version=15;").unwrap();
    assert!(f.store.budget_policy_projection().is_err());
    f.store.initialize(100).unwrap();
    f.store.initialize(100).unwrap();
    assert_eq!(f.store.budget_namespace().unwrap(), namespace);
    assert_eq!(f.store.budget_account(&member.root).unwrap(), account);
    assert_eq!(
        f.store.commit_budget_receipt(&usage).unwrap().known,
        account.known
    );
    assert_eq!(
        f.store
            .budget_policy_projection()
            .unwrap()
            .admission_policy(),
        None
    );
    db.execute_batch("PRAGMA user_version=999;").unwrap();
    assert!(f.store.budget_policy_projection().is_err());
    let missing = f.root.join("absent.sqlite");
    assert!(
        TokenUsageStore::new(&missing)
            .budget_policy_projection()
            .is_err()
    );
    assert!(!missing.exists());
    drop(db);
}

#[test]
fn budget_policy_projection_transaction_refusal_preserves_fence_and_original_source() {
    let f = Fixture::new();
    let op = publication(&f, "first", 0, 'a', 'b');
    let db = Connection::open(f.store.path()).unwrap();
    db.execute_batch("CREATE TRIGGER fail_projection BEFORE UPDATE ON budget_policy_projection BEGIN SELECT RAISE(ABORT,'fixture write refusal'); END;").unwrap();
    assert!(f.store.begin_budget_policy_publication(&op).is_err());
    assert_eq!(
        f.store.budget_policy_publication_outcome(&op.id).unwrap(),
        None
    );
    assert_eq!(f.store.budget_policy_projection().unwrap().revision, 0);
    db.execute_batch("DROP TRIGGER fail_projection;").unwrap();
    let pending = f.store.begin_budget_policy_publication(&op).unwrap();
    db.execute_batch("CREATE TRIGGER fail_projection BEFORE UPDATE ON budget_policy_projection BEGIN SELECT RAISE(ABORT,'fixture write refusal'); END;").unwrap();
    assert!(
        f.store
            .reconcile_budget_policy_publication(&op, &op.proposed.digest)
            .is_err()
    );
    assert_eq!(
        f.store.budget_policy_publication_outcome(&op.id).unwrap(),
        Some(pending)
    );
    assert_eq!(
        f.store
            .budget_policy_projection()
            .unwrap()
            .admission_policy(),
        None
    );
    db.execute_batch("DROP TRIGGER fail_projection;").unwrap();
    f.store
        .reconcile_budget_policy_publication(&op, &op.proposed.digest)
        .unwrap();
    assert_eq!(
        f.store
            .budget_policy_projection()
            .unwrap()
            .admission_policy(),
        Some(op.proposed.limits)
    );
    drop(db);
}
