//! A legacy association adds a coverage gap, not a second expense ledger or a
//! reset. Its account update and immutable membership must commit together.

use super::*;

#[test]
fn budget_storage_legacy_gap_registration_is_atomic_and_reopen_safe() {
    let f = Fixture::new();
    let root = f.member("original-root", None);
    f.active(&root);
    let a = attempt(&root, "original-attempt");
    f.store.register_budget_attempt(&a).unwrap();
    f.store
        .commit_budget_receipt(&receipt(&a, "original-receipt", Some(31), Some(7), true))
        .unwrap();
    f.store
        .commit_budget_policy(&operations::BudgetPolicyOperation {
            id: "original-overrides".into(),
            namespace: root.namespace.clone(),
            root: root.root.clone(),
            input: Some(BudgetOverride::Explicit(
                mez_agent::token_budget::TokenLimit::Unlimited,
            )),
            output: Some(BudgetOverride::Explicit(
                mez_agent::token_budget::TokenLimit::Limited(123),
            )),
        })
        .unwrap();
    let before = f.store.budget_account(&root.root).unwrap();
    let mut child = f.member("legacy-associated", Some(&root));
    child.legacy_coverage_gap = true;
    child.started_at = 200;
    let connection = f.store.open_budget().unwrap();
    connection.execute_batch("CREATE TRIGGER refuse_legacy BEFORE INSERT ON budget_memberships BEGIN SELECT RAISE(ABORT,'fixture refusal'); END;").unwrap();
    assert!(f.store.prepare_budget_membership(&child).is_err());
    assert_eq!(f.store.budget_account(&root.root).unwrap(), before);
    assert!(
        f.store
            .budget_membership(&child.conversation)
            .unwrap()
            .is_none()
    );
    connection
        .execute_batch("DROP TRIGGER refuse_legacy;")
        .unwrap();
    drop(connection);
    assert_eq!(
        f.store.prepare_budget_membership(&child).unwrap(),
        BudgetMembershipState::Pending
    );
    let reopened = TokenUsageStore::new(f.store.path());
    let after = reopened.budget_account(&root.root).unwrap();
    assert_eq!(after.known, before.known);
    assert_eq!(after.started_at, before.started_at);
    assert_eq!(after.input_override, before.input_override);
    assert_eq!(after.output_override, before.output_override);
    assert_eq!(after.policy_revision, before.policy_revision);
    assert_eq!(after.accounting_revision, before.accounting_revision + 1);
    assert!(after.legacy_coverage_gap);
    assert_eq!(
        reopened.prepare_budget_membership(&child).unwrap(),
        BudgetMembershipState::Pending
    );
    assert_eq!(reopened.budget_account(&root.root).unwrap(), after);
    let mut changed = child.clone();
    changed.started_at += 1;
    assert!(reopened.prepare_budget_membership(&changed).is_err());
    assert_eq!(
        reopened
            .budget_membership(&child.conversation)
            .unwrap()
            .unwrap()
            .0,
        child
    );
    assert_eq!(reopened.budget_account(&root.root).unwrap(), after);
}
