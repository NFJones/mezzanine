//! Native admission transactions, not complete provider-service integration.
//! Every fixture owns its private database, transcript inode and cleanup.

use super::*;
use crate::storage::token_usage::budget::admission::*;
use crate::storage::token_usage::budget::policy_projection::*;
use crate::storage::transcript::{AgentTranscriptStore, BudgetBindingLease};
use mez_agent::token_budget::{BudgetLimits, BudgetOverride, TokenLimit};

#[test]
fn budget_admission_legacy_unsettled_registration_alone_fences_finite_root() {
    let f = Fixture::new();
    let (member, lease) = native(&f, "root", None);
    let old = attempt(&member, "legacy-unsettled");
    f.store.register_budget_attempt(&old).unwrap();
    publish(&f, TokenLimit::Limited(100), TokenLimit::Unlimited);
    assert!(f.store.budget_attempt_admission(&old).unwrap().is_none());
    assert!(matches!(
        grant(&f, &member, &lease, "next"),
        BudgetAttemptAdmission::Held(BudgetAdmissionHold::OutstandingExposure)
    ));
    assert!(
        !f.store
            .budget_attempt_registration(&attempt(&member, "next"))
            .unwrap()
    );
    assert_eq!(
        f.store
            .open_budget()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM budget_admissions", [], |r| r
                .get::<_, i64>(0),)
            .unwrap(),
        0
    );
    publish(&f, TokenLimit::Unlimited, TokenLimit::Unlimited);
    authorized(grant(&f, &member, &lease, "next"));
}

#[test]
fn budget_admission_post_grant_inode_replacement_refuses_consume_without_erasing_exposure() {
    use std::os::unix::fs::OpenOptionsExt;
    let f = Fixture::new();
    let (member, lease) = native(&f, "root", None);
    publish(&f, TokenLimit::Limited(100), TokenLimit::Unlimited);
    let BudgetAttemptAdmission::Authorized(permit) = grant(&f, &member, &lease, "issued") else {
        panic!("expected permit");
    };
    let original = attempt(&member, "issued");
    let outcome = f
        .store
        .budget_attempt_admission(&original)
        .unwrap()
        .unwrap();
    let lock = f.root.join("transcripts/root/budget-binding.lock");
    std::fs::rename(&lock, f.root.join("retained-lock")).unwrap();
    let replacement = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(lock)
        .unwrap();
    assert!(permit.consume().is_err());
    assert_eq!(
        f.store.budget_attempt_admission(&original).unwrap(),
        Some(outcome)
    );
    assert!(matches!(
        grant(&f, &member, &lease, "issued"),
        BudgetAttemptAdmission::PreviouslyAuthorized(_)
    ));
    assert!(
        f.store
            .authorize_budget_attempt(&attempt(&member, "new"), &member, "root", &lease)
            .is_err()
    );
    assert!(
        !f.store
            .budget_attempt_registration(&attempt(&member, "new"))
            .unwrap()
    );
    drop(replacement);
}

/// Schema17 registrations and receipts remain literal. Migration must not infer
/// authorization, and a colliding future object must leave the old store intact.
#[test]
fn budget_admission_migration_preserves_old_rows_without_granting_and_rolls_back_collision() {
    for collision in [false, true] {
        let f = Fixture::new();
        let (member, lease) = native(&f, "root", None);
        let old = attempt(&member, "old");
        f.store.register_budget_attempt(&old).unwrap();
        let expense = receipt(&old, "receipt", Some(9), None, true);
        let original = f.store.commit_budget_receipt(&expense).unwrap();
        let db = f.store.open_budget().unwrap();
        let bytes: (String, String) = db
            .query_row("SELECT body,snapshot FROM budget_attempts", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        db.execute_batch("DROP TABLE budget_admissions; PRAGMA user_version=17;")
            .unwrap();
        if collision {
            db.execute_batch("CREATE TABLE budget_admissions(sentinel TEXT); INSERT INTO budget_admissions VALUES('keep');").unwrap();
            assert!(f.store.initialize(100).is_err());
            assert_eq!(
                db.query_row("SELECT sentinel FROM budget_admissions", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                "keep"
            );
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                17
            );
        } else {
            f.store.initialize(100).unwrap();
            f.store.initialize(100).unwrap();
            assert!(f.store.budget_attempt_admission(&old).unwrap().is_none());
            assert_eq!(
                f.store.budget_receipt_outcome(&expense).unwrap(),
                Some(original)
            );
            publish(&f, TokenLimit::Unlimited, TokenLimit::Unlimited);
            assert!(matches!(
                grant(&f, &member, &lease, "old"),
                BudgetAttemptAdmission::Held(BudgetAdmissionHold::RegistrationOnly)
            ));
        }
        let after: (String, String) = db
            .query_row("SELECT body,snapshot FROM budget_attempts", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(bytes, after);
    }
}

#[test]
fn budget_admission_capacity_counts_terminal_authorizations_and_old_unsettled_union() {
    let f = Fixture::new();
    let (member, lease) = native(&f, "root", None);
    publish(&f, TokenLimit::Unlimited, TokenLimit::Unlimited);
    authorized(grant(&f, &member, &lease, "authorized"));
    f.store
        .commit_budget_receipt(&receipt(
            &attempt(&member, "authorized"),
            "settled",
            Some(1),
            Some(1),
            true,
        ))
        .unwrap();
    let mut db = f.store.open_budget().unwrap();
    let tx = db.transaction().unwrap();
    for i in 0..4095 {
        receipts::register_attempt(&tx, &attempt(&member, &format!("old-{i}"))).unwrap();
    }
    tx.commit().unwrap();
    let account = f.store.budget_account(&member.root).unwrap();
    assert!(matches!(
        grant(&f, &member, &lease, "full"),
        BudgetAttemptAdmission::Held(BudgetAdmissionHold::Capacity)
    ));
    assert!(
        f.store
            .register_budget_attempt(&attempt(&member, "full"))
            .is_err()
    );
    assert!(
        !f.store
            .budget_attempt_registration(&attempt(&member, "full"))
            .unwrap()
    );
    assert_eq!(receipts::outstanding_attempts(&db).unwrap(), 4096);
    assert_eq!(f.store.budget_account(&member.root).unwrap(), account);
    // Rollback after registration but before grant publication is also atomic.
    db.execute_batch("DELETE FROM budget_attempts WHERE id='old-0'; CREATE TRIGGER refuse_grant BEFORE INSERT ON budget_admissions BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        f.store
            .authorize_budget_attempt(&attempt(&member, "rollback"), &member, "root", &lease)
            .is_err()
    );
    assert!(
        !f.store
            .budget_attempt_registration(&attempt(&member, "rollback"))
            .unwrap()
    );
    assert!(
        f.store
            .budget_attempt_admission(&attempt(&member, "rollback"))
            .unwrap()
            .is_none()
    );
    assert_eq!(f.store.budget_account(&member.root).unwrap(), account);
}

#[test]
fn budget_admission_consumed_permit_retains_native_lifetime_custody() {
    let f = Fixture::new();
    let (member, lease) = native(&f, "root", None);
    publish(&f, TokenLimit::Unlimited, TokenLimit::Unlimited);
    let BudgetAttemptAdmission::Authorized(permit) = grant(&f, &member, &lease, "one") else {
        panic!("expected permit");
    };
    drop(lease);
    let issued = permit.consume().unwrap();
    assert_eq!(issued.outcome().owner, "root");
    let transcripts = AgentTranscriptStore::new(f.root.join("transcripts"));
    assert!(
        f.store
            .publish_native_budget_membership(&transcripts, &member, "successor", None)
            .is_err()
    );
    drop(issued);
    let successor = f
        .store
        .publish_native_budget_membership(&transcripts, &member, "successor", None)
        .unwrap();
    // Rebinding after actual custody destruction does not erase old exposure.
    assert!(
        f.store
            .budget_attempt_admission(&attempt(&member, "one"))
            .unwrap()
            .is_some()
    );
    drop(successor);
}

fn native(
    f: &Fixture,
    conversation: &str,
    parent: Option<&BudgetMembership>,
) -> (BudgetMembership, BudgetBindingLease) {
    let member = f.member(conversation, parent);
    let lease = f
        .store
        .publish_native_budget_membership(
            &AgentTranscriptStore::new(f.root.join("transcripts")),
            &member,
            conversation,
            None,
        )
        .unwrap();
    (member, lease)
}

fn publish(f: &Fixture, input: TokenLimit, output: TokenLimit) {
    let previous = f.store.budget_policy_projection().unwrap();
    let op = BudgetPolicyPublication {
        namespace: previous.namespace,
        id: BudgetReceiptId::new(format!("policy-{}", previous.revision)).unwrap(),
        expected_revision: previous.revision,
        previous_digest: previous
            .committed
            .as_ref()
            .map_or_else(|| "a".repeat(64), |p| p.digest.clone()),
        proposed: BudgetConfigPolicy {
            source: "primary".into(),
            digest: format!("{:064x}", previous.revision + 1),
            limits: BudgetLimits { input, output },
        },
        supersedes: None,
    };
    f.store.begin_budget_policy_publication(&op).unwrap();
    f.store
        .reconcile_budget_policy_publication(&op, &op.proposed.digest)
        .unwrap();
}

fn grant(
    f: &Fixture,
    member: &BudgetMembership,
    lease: &BudgetBindingLease,
    id: &str,
) -> BudgetAttemptAdmission {
    f.store
        .authorize_budget_attempt(&attempt(member, id), member, &member.conversation, lease)
        .unwrap()
}

fn authorized(result: BudgetAttemptAdmission) -> BudgetAdmissionOutcome {
    match result {
        BudgetAttemptAdmission::Authorized(permit) => permit.consume().unwrap().outcome().clone(),
        other => panic!("expected authorization: {other:?}"),
    }
}

#[test]
fn budget_admission_missing_updating_and_exhausted_policy_write_nothing() {
    let f = Fixture::new();
    let (member, lease) = native(&f, "root", None);
    assert!(matches!(
        grant(&f, &member, &lease, "missing"),
        BudgetAttemptAdmission::Held(BudgetAdmissionHold::PolicyUnavailable)
    ));
    for (input, output, input_exhausted, output_exhausted) in [
        (TokenLimit::Limited(0), TokenLimit::Unlimited, true, false),
        (TokenLimit::Unlimited, TokenLimit::Limited(0), false, true),
        (TokenLimit::Limited(0), TokenLimit::Limited(0), true, true),
    ] {
        publish(&f, input, output);
        assert!(
            matches!(grant(&f, &member, &lease, "zero"), BudgetAttemptAdmission::Held(BudgetAdmissionHold::Exhausted { input, output }) if input == input_exhausted && output == output_exhausted)
        );
    }
    let before = f.store.budget_account(&member.root).unwrap();
    let projection = f.store.budget_policy_projection().unwrap();
    let op = BudgetPolicyPublication {
        namespace: member.namespace.clone(),
        id: BudgetReceiptId::new("updating").unwrap(),
        expected_revision: projection.revision,
        previous_digest: projection.committed.unwrap().digest,
        proposed: BudgetConfigPolicy {
            source: "primary".into(),
            digest: "b".repeat(64),
            limits: BudgetLimits::default(),
        },
        supersedes: None,
    };
    f.store.begin_budget_policy_publication(&op).unwrap();
    assert!(matches!(
        grant(&f, &member, &lease, "pending"),
        BudgetAttemptAdmission::Held(BudgetAdmissionHold::PolicyUnavailable)
    ));
    assert_eq!(before, f.store.budget_account(&member.root).unwrap());
    let db = f.store.open_budget().unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM budget_attempts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM budget_admissions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn budget_admission_one_use_original_evidence_survives_policy_and_ack_loss() {
    let f = Fixture::new();
    let (member, lease) = native(&f, "root", None);
    publish(&f, TokenLimit::Limited(10), TokenLimit::Unlimited);
    let original = attempt(&member, "issued");
    let outcome = authorized(grant(&f, &member, &lease, "issued"));
    assert_eq!(
        outcome.revisions,
        BudgetAdmissionRevisions {
            projection: 2,
            root_policy: 0,
            accounting: 0
        }
    );
    assert_eq!(outcome.known, UsageTotals::default());
    publish(&f, TokenLimit::Unlimited, TokenLimit::Unlimited);
    let reopened = TokenUsageStore::new(f.store.path());
    assert_eq!(
        reopened.budget_attempt_admission(&original).unwrap(),
        Some(outcome.clone())
    );
    match reopened
        .authorize_budget_attempt(&original, &member, "root", &lease)
        .unwrap()
    {
        BudgetAttemptAdmission::PreviouslyAuthorized(evidence) => assert_eq!(evidence, outcome),
        other => panic!("a lost reply must never mint another permit: {other:?}"),
    }
    let mut changed = original.clone();
    changed.purpose = BudgetAttemptPurpose::Routing;
    assert!(reopened.budget_attempt_admission(&changed).is_err());
    assert!(
        reopened
            .authorize_budget_attempt(&changed, &member, "root", &lease)
            .is_err()
    );
    assert_eq!(
        reopened.budget_attempt_admission(&original).unwrap(),
        Some(outcome)
    );
}

#[test]
fn budget_admission_root_and_child_compete_but_unrelated_root_proceeds() {
    let f = Fixture::new();
    let (parent, parent_lease) = native(&f, "root", None);
    let (child, child_lease) = native(&f, "child", Some(&parent));
    let (other, other_lease) = native(&f, "other", None);
    publish(&f, TokenLimit::Limited(100), TokenLimit::Unlimited);
    let barrier = std::sync::Barrier::new(2);
    let (left, right) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            grant(&f, &parent, &parent_lease, "parent-send")
        });
        let b = scope.spawn(|| {
            barrier.wait();
            grant(&f, &child, &child_lease, "child-send")
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    let granted = [left, right]
        .into_iter()
        .map(|value| match value {
            BudgetAttemptAdmission::Authorized(permit) => {
                permit.consume().unwrap();
                1
            }
            BudgetAttemptAdmission::Held(BudgetAdmissionHold::OutstandingExposure) => 0,
            other => panic!("unexpected contention outcome: {other:?}"),
        })
        .sum::<usize>();
    assert_eq!(granted, 1);
    authorized(grant(&f, &other, &other_lease, "other-send"));
    assert!(
        !f.store
            .budget_attempt_registration(&attempt(&parent, "parent-send"))
            .unwrap()
            || !f
                .store
                .budget_attempt_registration(&attempt(&child, "child-send"))
                .unwrap()
    );
}

#[test]
fn budget_admission_unlimited_exposure_is_not_retired_by_terminal_expense() {
    let f = Fixture::new();
    let (member, lease) = native(&f, "root", None);
    publish(&f, TokenLimit::Unlimited, TokenLimit::Unlimited);
    authorized(grant(&f, &member, &lease, "one"));
    // Unlimited admission remains concurrent even with earlier unsettled work.
    authorized(grant(&f, &member, &lease, "two"));
    publish(&f, TokenLimit::Limited(100), TokenLimit::Unlimited);
    assert!(matches!(
        grant(&f, &member, &lease, "three"),
        BudgetAttemptAdmission::Held(BudgetAdmissionHold::OutstandingExposure)
    ));
    for id in ["one", "two"] {
        f.store
            .commit_budget_receipt(&receipt(
                &attempt(&member, id),
                &format!("settle-{id}"),
                Some(1),
                Some(1),
                true,
            ))
            .unwrap();
    }
    assert!(matches!(
        grant(&f, &member, &lease, "three"),
        BudgetAttemptAdmission::Held(BudgetAdmissionHold::OutstandingExposure)
    ));
    assert!(
        !f.store
            .budget_attempt_registration(&attempt(&member, "three"))
            .unwrap()
    );
    // Lifting policy restores unlimited concurrency, not usage or exposure reset.
    publish(&f, TokenLimit::Unlimited, TokenLimit::Unlimited);
    let next = authorized(grant(&f, &member, &lease, "three"));
    assert_eq!(
        next.known,
        UsageTotals {
            input: 2,
            output: 2
        }
    );
    assert_eq!(next.revisions.accounting, 2);
}

#[test]
fn budget_admission_current_overrides_equality_and_separate_revisions() {
    let f = Fixture::new();
    let (member, lease) = native(&f, "root", None);
    let old = attempt(&member, "old-bookkeeping");
    f.store.register_budget_attempt(&old).unwrap();
    f.store
        .commit_budget_receipt(&receipt(&old, "old-expense", Some(10), Some(3), true))
        .unwrap();
    publish(&f, TokenLimit::Limited(10), TokenLimit::Unlimited);
    assert!(matches!(
        grant(&f, &member, &lease, "equality"),
        BudgetAttemptAdmission::Held(BudgetAdmissionHold::Exhausted {
            input: true,
            output: false
        })
    ));
    f.store
        .commit_budget_policy(&operations::BudgetPolicyOperation {
            id: "override".into(),
            namespace: member.namespace.clone(),
            root: member.root.clone(),
            input: Some(BudgetOverride::Explicit(TokenLimit::Unlimited)),
            output: Some(BudgetOverride::Explicit(TokenLimit::Limited(4))),
        })
        .unwrap();
    let outcome = authorized(grant(&f, &member, &lease, "override-send"));
    assert_eq!(
        outcome.revisions,
        BudgetAdmissionRevisions {
            projection: 2,
            root_policy: 1,
            accounting: 1
        }
    );
    assert_eq!(
        outcome.limits,
        BudgetLimits {
            input: TokenLimit::Unlimited,
            output: TokenLimit::Limited(4)
        }
    );
    assert_eq!(
        outcome.known,
        UsageTotals {
            input: 10,
            output: 3
        }
    );
    assert!(matches!(
        grant(&f, &member, &lease, "old-bookkeeping"),
        BudgetAttemptAdmission::Held(BudgetAdmissionHold::RegistrationOnly)
    ));
    assert!(f.store.budget_attempt_admission(&old).unwrap().is_none());
}

#[test]
fn budget_admission_native_evidence_refusal_and_dropped_permit_preserve_exposure() {
    for fault in 0..6 {
        let f = Fixture::new();
        let (member, lease) = native(&f, "root", None);
        publish(&f, TokenLimit::Limited(100), TokenLimit::Unlimited);
        let mut a = attempt(&member, "issued");
        let db = f.store.open_budget().unwrap();
        match fault {
            0 => a.namespace = "foreign".into(),
            1 => a.root = BudgetRootId::new("foreign").unwrap(),
            2 => {
                db.execute("UPDATE budget_bindings SET owner='foreign'", [])
                    .unwrap();
            }
            3 => {
                db.execute("UPDATE budget_memberships SET state=1", [])
                    .unwrap();
            }
            4 => {
                std::fs::rename(
                    f.root.join("transcripts/root/budget-binding.lock"),
                    f.root.join("retained-lock"),
                )
                .unwrap();
            }
            5 => {
                std::fs::rename(f.store.path(), f.root.join("retained.sqlite")).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            f.store
                .authorize_budget_attempt(&a, &member, "root", &lease)
                .is_err(),
            "fault {fault}"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM budget_attempts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM budget_admissions", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        if fault == 5 {
            assert!(!f.store.path().exists());
        }
    }
    let f = Fixture::new();
    let (member, lease) = native(&f, "root", None);
    publish(&f, TokenLimit::Limited(100), TokenLimit::Unlimited);
    drop(grant(&f, &member, &lease, "lost"));
    assert!(
        f.store
            .budget_attempt_admission(&attempt(&member, "lost"))
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        grant(&f, &member, &lease, "next"),
        BudgetAttemptAdmission::Held(BudgetAdmissionHold::OutstandingExposure)
    ));
    assert!(matches!(
        grant(&f, &member, &lease, "lost"),
        BudgetAttemptAdmission::PreviouslyAuthorized(_)
    ));
}
