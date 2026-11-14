//! Descriptive count equality cannot stand in for a fresh native refusal.

use super::*;
use crate::storage::token_usage::budget::admission::{
    BudgetAdmissionHold, BudgetAdmissionRevisions, BudgetAdmissionWaitState,
};

#[test]
fn budget_wait_capacity_count_cycle_is_not_an_unchanged_refusal() {
    let before_retry = BudgetAdmissionWaitState {
        revisions: BudgetAdmissionRevisions {
            projection: 1,
            root_policy: 0,
            accounting: 0,
        },
        limits: Some(Default::default()),
        known: Default::default(),
        root_outstanding: false,
        outstanding_attempts: 4095,
    };
    // A observed 4095, another root claimed the last slot before A's
    // native retry (Held(Capacity)), then retired before A's next read.
    // A's root-local revisions and the final count equal the old snapshot;
    // equality is NOT evidence that the new refusal still applies.
    let after_other_root_retired = before_retry.clone();
    assert!(reconsider_changed_hold(
        &after_other_root_retired,
        Some(&before_retry),
        BudgetAdmissionHold::Capacity
    ));
    assert!(!reconsider_changed_hold(
        &before_retry,
        Some(&before_retry),
        BudgetAdmissionHold::OutstandingExposure
    ));
    let mut full = before_retry.clone();
    full.outstanding_attempts = 4096;
    assert!(!reconsider_changed_hold(
        &full,
        Some(&before_retry),
        BudgetAdmissionHold::Capacity
    ));
    assert!(!reconsider_changed_hold(
        &full,
        None,
        BudgetAdmissionHold::Capacity
    ));
}
