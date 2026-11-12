//! Exact worker-side reconciliation after uncertain registration/commit ACKs.
//! These reads do not admit inference or repeat a bookkeeping transaction.

use super::*;

/// The recovery connection itself lacks mutation authority, independently of
/// whether the caller happens to issue only SELECT statements today.
#[test]
fn budget_reconciliation_connection_is_read_only() {
    let f = Fixture::new();
    let connection = f.store.open_budget_query().unwrap();
    assert!(
        connection
            .execute("DELETE FROM budget_namespace", [])
            .is_err()
    );
    assert!(f.store.budget_namespace().is_ok());
}

#[test]
fn budget_reconciliation_absence_is_not_acknowledgement_or_store_creation() {
    let f = Fixture::new();
    let member = f.member("root", None);
    f.active(&member);
    let original = attempt(&member, "unregistered");
    let report = receipt(&original, "uncommitted", Some(0), Some(0), true);
    assert!(!f.store.budget_attempt_registration(&original).unwrap());
    assert_eq!(f.store.budget_receipt_outcome(&report).unwrap(), None);
    assert_eq!(
        f.store.budget_account(&member.root).unwrap().known,
        UsageTotals::default()
    );
    let missing = TokenUsageStore::new(f.root.join("missing.sqlite"));
    assert!(missing.budget_attempt_registration(&original).is_err());
    assert!(missing.budget_receipt_outcome(&report).is_err());
    assert!(!missing.path().exists());
}

#[test]
fn budget_reconciliation_returns_original_outcome_after_later_expense() {
    let f = Fixture::new();
    let member = f.member("root", None);
    f.active(&member);
    let original = attempt(&member, "original");
    f.store.register_budget_attempt(&original).unwrap();
    let first = receipt(&original, "first", Some(7), None, false);
    let acknowledged = f.store.commit_budget_receipt(&first).unwrap();
    let final_report = receipt(&original, "final", Some(11), Some(0), true);
    f.store.commit_budget_receipt(&final_report).unwrap();
    let before = f.store.budget_account(&member.root).unwrap();
    let reopened = TokenUsageStore::new(f.store.path());
    assert!(reopened.budget_attempt_registration(&original).unwrap());
    assert_eq!(
        reopened.budget_receipt_outcome(&first).unwrap(),
        Some(acknowledged)
    );
    assert_eq!(reopened.budget_account(&member.root).unwrap(), before);
    let mut changed = first.clone();
    changed.snapshot.output = Some(0);
    assert!(reopened.budget_receipt_outcome(&changed).is_err());
    changed = first.clone();
    changed.attempt.purpose = BudgetAttemptPurpose::Memory;
    assert!(reopened.budget_receipt_outcome(&changed).is_err());
    assert!(
        reopened
            .budget_attempt_registration(&changed.attempt)
            .is_err()
    );
    let other = Fixture::new();
    assert!(other.store.budget_receipt_outcome(&first).is_err());
    assert!(other.store.budget_attempt_registration(&original).is_err());
}

/// Indexed keys are not alternate provenance: unchanged encoded bodies cannot
/// hide corruption of the original attempt/root/conversation relationship.
#[test]
fn budget_reconciliation_and_mutation_replay_refuse_changed_indexed_provenance() {
    for receipt_index in [false, true] {
        let f = Fixture::new();
        let member = f.member("root", None);
        f.active(&member);
        let other = f.member("other", None);
        f.active(&other);
        let original = attempt(&member, "original");
        let foreign = attempt(&other, "foreign");
        f.store.register_budget_attempt(&original).unwrap();
        f.store.register_budget_attempt(&foreign).unwrap();
        let report = receipt(&original, "committed", Some(3), Some(2), true);
        f.store.commit_budget_receipt(&report).unwrap();
        let connection = f.store.open_budget().unwrap();
        if receipt_index {
            connection
                .execute(
                    "UPDATE budget_receipts SET attempt=?1 WHERE id=?2",
                    params![foreign.id.as_str(), report.id.as_str()],
                )
                .unwrap();
            assert!(f.store.budget_receipt_outcome(&report).is_err());
            assert!(f.store.commit_budget_receipt(&report).is_err());
        } else {
            connection
                .execute(
                    "UPDATE budget_attempts SET root=?1,conversation=?2 WHERE id=?3",
                    params![
                        foreign.root.as_str(),
                        foreign.conversation,
                        original.id.as_str()
                    ],
                )
                .unwrap();
            assert!(f.store.budget_attempt_registration(&original).is_err());
            assert!(f.store.register_budget_attempt(&original).is_err());
        }
        assert_eq!(
            f.store.budget_account(&member.root).unwrap().known,
            UsageTotals {
                input: 3,
                output: 2
            }
        );
        assert_eq!(
            f.store.budget_account(&other.root).unwrap().known,
            UsageTotals::default()
        );
    }
}

/// Real abrupt process exit loses only the registration ACK, not the original
/// durable attempt. Recovery reads that identity without registering it again.
#[test]
fn budget_reconciliation_attempt_registration_survives_process_exit() {
    const ENV: &str = "MEZ_TEST_BUDGET_REGISTRATION_EXIT_STORE";
    const TEST: &str = "storage::token_usage::budget::tests::reconciliation::budget_reconciliation_attempt_registration_survives_process_exit";
    if let Some(path) = std::env::var_os(ENV) {
        let store = TokenUsageStore::new(std::path::PathBuf::from(path));
        let member = store.budget_membership("root").unwrap().unwrap().0;
        store
            .register_budget_attempt(&attempt(&member, "lost-registration-ack"))
            .unwrap();
        std::process::exit(76);
    }
    let f = Fixture::new();
    let member = f.member("root", None);
    f.active(&member);
    let original = attempt(&member, "lost-registration-ack");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--quiet"])
        .env(ENV, f.store.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(76));
    let reopened = TokenUsageStore::new(f.store.path());
    assert!(reopened.budget_attempt_registration(&original).unwrap());
    assert_eq!(
        reopened
            .budget_receipt_outcome(&receipt(&original, "not-yet-incurred", None, None, true))
            .unwrap(),
        None
    );
    assert_eq!(
        reopened.budget_account(&member.root).unwrap().known,
        UsageTotals::default()
    );
}
