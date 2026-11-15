//! Original ACKs stay immutable while a coherent read observes newer policy and
//! expense. These observations confer no admission or retirement authority.

use super::*;
use crate::storage::token_usage::budget::policy_projection::{
    BudgetConfigPolicy, BudgetPolicyPublication,
};
use crate::storage::token_usage::budget::receipts::BudgetSettlementState;
use mez_agent::token_budget::{BudgetLimits, TokenLimit};

fn publication(
    f: &Fixture,
    id: &str,
    digest: char,
    limits: BudgetLimits,
) -> BudgetPolicyPublication {
    let prior = f.store.budget_policy_projection().unwrap();
    BudgetPolicyPublication {
        namespace: prior.namespace,
        id: BudgetReceiptId::new(id).unwrap(),
        expected_revision: prior.revision,
        previous_digest: prior.committed.map_or_else(|| "a".repeat(64), |p| p.digest),
        proposed: BudgetConfigPolicy {
            source: "primary".into(),
            digest: digest.to_string().repeat(64),
            limits,
        },
        supersedes: None,
    }
}

#[test]
fn budget_settlement_original_receipt_and_current_policy_are_distinct() {
    let f = Fixture::new();
    let member = f.member("root", None);
    f.active(&member);
    let original = attempt(&member, "original");
    f.store.register_budget_attempt(&original).unwrap();
    let first = receipt(&original, "first", Some(7), None, false);
    assert_eq!(f.store.budget_receipt_settlement(&first).unwrap(), None);
    let ack = f.store.commit_budget_receipt(&first).unwrap();
    assert_eq!(
        f.store
            .budget_receipt_settlement(&first)
            .unwrap()
            .unwrap()
            .state(),
        BudgetSettlementState::PolicyUnavailable
    );
    let limits = BudgetLimits {
        input: TokenLimit::Limited(11),
        output: TokenLimit::Limited(3),
    };
    let op = publication(&f, "finite", 'b', limits);
    f.store.begin_budget_policy_publication(&op).unwrap();
    f.store
        .reconcile_budget_policy_publication(&op, &op.proposed.digest)
        .unwrap();
    let before = f.store.budget_receipt_settlement(&first).unwrap().unwrap();
    assert_eq!(before.receipt, ack);
    assert_eq!(before.state(), BudgetSettlementState::WithinKnownLimits);
    let terminal = receipt(&original, "terminal", Some(11), Some(3), true);
    f.store.commit_budget_receipt(&terminal).unwrap();
    let crossed = f.store.budget_receipt_settlement(&first).unwrap().unwrap();
    assert_eq!(crossed.receipt, ack);
    assert_eq!(
        crossed.known,
        UsageTotals {
            input: 11,
            output: 3
        }
    );
    assert_eq!(
        crossed.state(),
        BudgetSettlementState::Exhausted {
            input: true,
            output: true
        }
    );
    assert!(crossed.revisions.accounting > before.revisions.accounting);
    let updating = publication(&f, "lift", 'c', BudgetLimits::default());
    f.store.begin_budget_policy_publication(&updating).unwrap();
    assert_eq!(
        f.store
            .budget_receipt_settlement(&first)
            .unwrap()
            .unwrap()
            .state(),
        BudgetSettlementState::PolicyUnavailable
    );
    f.store
        .reconcile_budget_policy_publication(&updating, &updating.proposed.digest)
        .unwrap();
    let lifted = f.store.budget_receipt_settlement(&first).unwrap().unwrap();
    assert_eq!(lifted.receipt, ack);
    assert_eq!(lifted.known, crossed.known);
    assert!(lifted.revisions.projection > crossed.revisions.projection);
    assert_eq!(lifted.state(), BudgetSettlementState::WithinKnownLimits);
    let overridden = f
        .store
        .commit_budget_policy(&operations::BudgetPolicyOperation {
            id: "root-input-only".into(),
            namespace: member.namespace.clone(),
            root: member.root.clone(),
            input: Some(BudgetOverride::Explicit(TokenLimit::Limited(11))),
            output: None,
        })
        .unwrap();
    let local = f.store.budget_receipt_settlement(&first).unwrap().unwrap();
    assert_eq!(local.receipt, ack);
    assert_eq!(local.known, crossed.known);
    assert_eq!(local.revisions.root_policy, overridden.policy_revision);
    assert_eq!(local.revisions.projection, lifted.revisions.projection);
    assert_eq!(
        local.state(),
        BudgetSettlementState::Exhausted {
            input: true,
            output: false
        }
    );
    // A current observation cannot turn an old ACK into a new committed result.
    assert_eq!(f.store.commit_budget_receipt(&first).unwrap(), ack);
    let mut changed = first.clone();
    changed.snapshot.input = Some(8);
    assert!(f.store.budget_receipt_settlement(&changed).is_err());
    let missing = TokenUsageStore::new(f.root.join("missing.sqlite"));
    assert!(missing.budget_receipt_settlement(&first).is_err());
    assert!(!missing.path().exists());
    assert_eq!(
        f.store.budget_account(&member.root).unwrap().known,
        crossed.known
    );
}

#[test]
fn budget_settlement_refuses_foreign_namespace_and_regressed_account_without_mutation() {
    let f = Fixture::new();
    let member = f.member("root", None);
    f.active(&member);
    let original = attempt(&member, "original");
    f.store.register_budget_attempt(&original).unwrap();
    let report = receipt(&original, "committed", Some(7), None, true);
    f.store.commit_budget_receipt(&report).unwrap();
    let other = Fixture::new();
    assert!(other.store.budget_receipt_settlement(&report).is_err());
    let db = f.store.open_budget().unwrap();
    let mut regressed = f.store.budget_account(&member.root).unwrap();
    regressed.known.input = 0;
    let body = encode(&regressed).unwrap();
    db.execute(
        "UPDATE budget_roots SET body=?1 WHERE id=?2",
        params![body, member.root.as_str()],
    )
    .unwrap();
    assert!(f.store.budget_receipt_settlement(&report).is_err());
    assert_eq!(f.store.budget_account(&member.root).unwrap(), regressed);
}

#[test]
fn budget_settlement_refuses_changed_indexed_attempt_provenance() {
    let f = Fixture::new();
    let member = f.member("root", None);
    f.active(&member);
    let foreign = f.member("foreign", None);
    f.active(&foreign);
    let original = attempt(&member, "original");
    f.store.register_budget_attempt(&original).unwrap();
    let report = receipt(&original, "committed", Some(7), None, true);
    let acknowledged = f.store.commit_budget_receipt(&report).unwrap();
    f.store
        .open_budget()
        .unwrap()
        .execute(
            "UPDATE budget_attempts SET root=?1,conversation=?2 WHERE id=?3",
            params![
                foreign.root.as_str(),
                foreign.conversation,
                original.id.as_str()
            ],
        )
        .unwrap();
    let refused = f.store.budget_receipt_settlement(&report).is_err();
    // Historical receipt replay is intentionally independent; the new current
    // observation must additionally qualify the original indexed attempt.
    assert_eq!(
        f.store.budget_receipt_outcome(&report).unwrap(),
        Some(acknowledged)
    );
    assert!(
        refused,
        "current settlement observation accepted changed attempt provenance"
    );
}
