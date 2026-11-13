//! Atomic terminal-unknown transfer and replay-only grant retention. Storage
//! tests validate proposals; physical proof comes from connected native tests.

use super::admission::{authorized, grant, native, publish};
use super::*;
use crate::storage::token_usage::budget::admission::{BudgetAdmissionHold, BudgetAttemptAdmission};
use crate::storage::token_usage::budget::retirement::BudgetExposureRetirement;
use mez_agent::token_budget::TokenLimit;

#[test]
fn budget_retirement_unknown_preserves_subtotal_and_never_reissues_original_grant() {
    let f = Fixture::new();
    let (member, lease) = native(&f, "root", None);
    publish(&f, TokenLimit::Limited(100), TokenLimit::Unlimited);
    let original = attempt(&member, "issued");
    let granted = authorized(grant(&f, &member, &lease, "issued"));
    let prefix = receipt(&original, "prefix", Some(17), None, false);
    let terminal = receipt(&original, "retired", None, None, true);
    let evidence = BudgetExposureRetirement::new(granted.clone(), prefix.clone());
    assert!(
        f.store
            .commit_budget_retirement_receipt(&terminal, &evidence)
            .is_err()
    );
    f.store.commit_budget_receipt(&prefix).unwrap();
    assert!(matches!(
        grant(&f, &member, &lease, "next"),
        BudgetAttemptAdmission::Held(BudgetAdmissionHold::OutstandingExposure)
    ));
    let outcome = f
        .store
        .commit_budget_retirement_receipt(&terminal, &evidence)
        .unwrap();
    assert_eq!(
        outcome.known,
        UsageTotals {
            input: 17,
            output: 0
        }
    );
    let db = f.store.open_budget().unwrap();
    let snapshot: String = db
        .query_row(
            "SELECT snapshot FROM budget_attempts WHERE id='issued'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(snapshot["prefix"]["input"], 17);
    assert!(snapshot["terminal"]["input"].is_null());
    assert!(snapshot["terminal"]["output"].is_null());
    assert_eq!(receipts::outstanding_attempts(&db).unwrap(), 0);
    let reopened = TokenUsageStore::new(f.store.path());
    assert_eq!(
        reopened
            .budget_retirement_receipt_outcome(&terminal, &evidence)
            .unwrap(),
        Some(outcome.clone())
    );
    assert_eq!(
        reopened
            .commit_budget_retirement_receipt(&terminal, &evidence)
            .unwrap(),
        outcome
    );
    assert_eq!(
        reopened.budget_attempt_admission(&original).unwrap(),
        Some(granted.clone())
    );
    assert!(
        matches!(grant(&f, &member, &lease, "issued"), BudgetAttemptAdmission::PreviouslyAuthorized(old) if old == granted)
    );
    authorized(grant(&f, &member, &lease, "next"));
    assert_eq!(
        f.store.budget_account(&member.root).unwrap().known.input,
        17
    );
}

#[test]
fn budget_retirement_refusal_and_transaction_failure_preserve_original_exposure() {
    let f = Fixture::new();
    let (member, lease) = native(&f, "root", None);
    publish(&f, TokenLimit::Limited(100), TokenLimit::Unlimited);
    let original = attempt(&member, "issued");
    let granted = authorized(grant(&f, &member, &lease, "issued"));
    let prefix = receipt(&original, "prefix", Some(17), None, false);
    let terminal = receipt(&original, "retired", None, None, true);
    let evidence = BudgetExposureRetirement::new(granted.clone(), prefix.clone());
    f.store.commit_budget_receipt(&prefix).unwrap();
    let before = f.store.budget_account(&member.root).unwrap();
    let mut foreign = granted;
    foreign.owner = "different-owner".into();
    assert!(
        f.store
            .commit_budget_retirement_receipt(
                &terminal,
                &BudgetExposureRetirement::new(foreign, prefix)
            )
            .is_err()
    );
    let mut false_coverage = terminal.clone();
    false_coverage.snapshot.input = Some(17);
    assert!(
        f.store
            .commit_budget_retirement_receipt(&false_coverage, &evidence)
            .is_err()
    );
    let db = f.store.open_budget().unwrap();
    db.execute_batch("CREATE TRIGGER refuse_retirement BEFORE UPDATE OF retired_receipt ON budget_admissions BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        f.store
            .commit_budget_retirement_receipt(&terminal, &evidence)
            .is_err()
    );
    assert_eq!(f.store.budget_account(&member.root).unwrap(), before);
    assert_eq!(
        f.store
            .budget_retirement_receipt_outcome(&terminal, &evidence)
            .unwrap(),
        None
    );
    assert_eq!(receipts::outstanding_attempts(&db).unwrap(), 1);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM budget_receipts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    db.execute_batch("DROP TRIGGER refuse_retirement;").unwrap();
    // A standalone terminal receipt is not upgradeable into qualified release.
    f.store.commit_budget_receipt(&terminal).unwrap();
    assert!(
        f.store
            .commit_budget_retirement_receipt(&terminal, &evidence)
            .is_err()
    );
    assert!(
        f.store
            .budget_retirement_receipt_outcome(&terminal, &evidence)
            .is_err()
    );
    assert_eq!(receipts::outstanding_attempts(&db).unwrap(), 1);
}

#[test]
fn budget_retirement_drains_entire_unlimited_issued_set_before_serial_admission() {
    let f = Fixture::new();
    let (member, lease) = native(&f, "root", None);
    publish(&f, TokenLimit::Unlimited, TokenLimit::Unlimited);
    let issued: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|id| {
            let granted = authorized(grant(&f, &member, &lease, id));
            let original = attempt(&member, id);
            let prefix = receipt(&original, &format!("prefix-{id}"), Some(7), None, false);
            f.store.commit_budget_receipt(&prefix).unwrap();
            (
                receipt(&original, &format!("retired-{id}"), None, None, true),
                BudgetExposureRetirement::new(granted, prefix),
            )
        })
        .collect();
    publish(&f, TokenLimit::Limited(100), TokenLimit::Unlimited);
    for (index, (terminal, evidence)) in issued.iter().enumerate() {
        assert!(matches!(
            grant(&f, &member, &lease, "next"),
            BudgetAttemptAdmission::Held(BudgetAdmissionHold::OutstandingExposure)
        ));
        f.store
            .commit_budget_retirement_receipt(terminal, evidence)
            .unwrap();
        assert_eq!(
            receipts::outstanding_attempts(&f.store.open_budget().unwrap()).unwrap(),
            1 - index as i64
        );
    }
    publish(&f, TokenLimit::Limited(14), TokenLimit::Unlimited);
    assert!(matches!(
        grant(&f, &member, &lease, "next"),
        BudgetAttemptAdmission::Held(BudgetAdmissionHold::Exhausted {
            input: true,
            output: false
        })
    ));
    publish(&f, TokenLimit::Limited(15), TokenLimit::Unlimited);
    authorized(grant(&f, &member, &lease, "next"));
    let (other, other_lease) = native(&f, "other", None);
    authorized(grant(&f, &other, &other_lease, "unaffected"));
}

#[test]
fn budget_retirement_migration_never_infers_release_from_old_terminal_receipts() {
    for collision in [false, true] {
        let f = Fixture::new();
        let (member, lease) = native(&f, "root", None);
        publish(&f, TokenLimit::Limited(100), TokenLimit::Unlimited);
        let granted = authorized(grant(&f, &member, &lease, "old"));
        let original = attempt(&member, "old");
        f.store
            .commit_budget_receipt(&receipt(&original, "expense", Some(7), None, true))
            .unwrap();
        let db = f.store.open_budget().unwrap();
        db.execute_batch("DROP INDEX budget_admissions_outstanding;")
            .unwrap();
        if !collision {
            db.execute_batch("ALTER TABLE budget_admissions DROP COLUMN retired_receipt;")
                .unwrap();
        }
        db.execute_batch("PRAGMA user_version=18;").unwrap();
        if collision {
            assert!(f.store.initialize(100).is_err());
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                18
            );
        } else {
            f.store.initialize(100).unwrap();
            f.store.initialize(100).unwrap();
            assert_eq!(
                f.store.budget_attempt_admission(&original).unwrap(),
                Some(granted)
            );
            assert_eq!(receipts::outstanding_attempts(&db).unwrap(), 1);
            assert!(matches!(
                grant(&f, &member, &lease, "next"),
                BudgetAttemptAdmission::Held(BudgetAdmissionHold::OutstandingExposure)
            ));
            assert_eq!(f.store.budget_account(&member.root).unwrap().known.input, 7);
        }
    }
}
