//! Real receipt/reporting transactions. Synthetic normalized counters qualify
//! repository conservation only, not native provider capture or admission.

use super::*;
use crate::storage::token_usage::budget::reporting::{
    BudgetReportingObservation, BudgetReportingOrigin,
};
use crate::storage::token_usage::{AccountingOrigin, AccountingProjectId};
use mez_agent::ModelTokenUsage;

/// A numerically fitting disclosure still cannot establish temporal allocation
/// when the immediately preceding reporting boundary omitted that category.
#[test]
fn budget_reporting_missing_cache_baselines_are_unknown_in_narrow_windows() {
    for writes in [false, true] {
        let f = Fixture::new();
        let (_, attempt) = original(&f);
        for (index, input, cache) in [
            (0, 100, None),
            (1, 150, Some(30)),
            (2, 180, None),
            (3, 200, Some(40)),
        ] {
            let mut receipt = reported(
                &attempt,
                &format!("baseline-{index}"),
                input,
                index + 1,
                index == 3,
            );
            let observation = receipt.reporting.as_mut().unwrap();
            observation.observed_at_unix_seconds = 200 + index;
            if writes {
                observation.usage.cache_write_input_tokens = cache;
            } else {
                observation.usage.cached_input_tokens = cache;
            }
            f.store.commit_budget_receipt(&receipt).unwrap();
            if index == 1 || index == 3 {
                let history = f
                    .store
                    .history_snapshot(200 + index + 86_400, &[1], &Default::default())
                    .unwrap();
                let total = history.windows[&1].values().next().unwrap();
                assert_eq!(total.usage.input_tokens, if index == 1 { 50 } else { 20 });
                if writes {
                    assert_eq!(total.usage.cache_write_input_tokens, None);
                } else {
                    assert_eq!(total.usage.cached_input_tokens, None);
                }
            }
        }
    }
}

/// Repeated exact settlement from independent workers commits the same original
/// unattributed expense once; an absent project is not borrowed from a pane.
#[test]
fn budget_reporting_concurrent_unattributed_receipt_commits_once() {
    let f = Fixture::new();
    let member = f.member("unattributed", None);
    f.active(&member);
    let mut attempt = attempt(&member, "unattributed-send");
    attempt.reporting_origin = Some(BudgetReportingOrigin::capture(
        &AccountingOrigin::Unattributed,
    ));
    f.store.register_budget_attempt(&attempt).unwrap();
    let receipt = reported(&attempt, "concurrent", 7, 3, true);
    let barrier = std::sync::Barrier::new(2);
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            f.store.commit_budget_receipt(&receipt)
        });
        let b = scope.spawn(|| {
            barrier.wait();
            TokenUsageStore::new(f.store.path()).commit_budget_receipt(&receipt)
        });
        (a.join().unwrap().unwrap(), b.join().unwrap().unwrap())
    });
    assert_eq!(a, b);
    assert_eq!(event_count(&f), 1);
    let project: Option<String> = f
        .store
        .open_budget()
        .unwrap()
        .query_row("SELECT project_id FROM token_usage_events", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(project, None);
    assert_eq!(f.store.budget_account(&member.root).unwrap().known.input, 7);
}

/// Unknown and omitted cache categories do not erase the retained known prefix
/// or cause a later disclosure to count the same category twice.
#[test]
fn budget_reporting_cache_unknown_known_omitted_known_preserves_prefix() {
    let f = Fixture::new();
    let (_, attempt) = original(&f);
    for (index, input, cache) in [
        (0, 100, None),
        (1, 150, Some(30)),
        (2, 180, None),
        (3, 200, Some(40)),
    ] {
        let mut receipt = reported(
            &attempt,
            &format!("cache-{index}"),
            input,
            index + 1,
            index == 3,
        );
        receipt
            .reporting
            .as_mut()
            .unwrap()
            .usage
            .cached_input_tokens = cache;
        f.store.commit_budget_receipt(&receipt).unwrap();
    }
    let history = f
        .store
        .history_snapshot(200, &[1], &Default::default())
        .unwrap();
    let total = history.windows[&1].values().next().unwrap();
    assert_eq!(total.usage.total_tokens(), 204);
    assert_eq!(total.usage.cached_input_tokens, None);
    let db = f.store.open_budget().unwrap();
    let cached: Option<i64> = db
        .query_row(
            "SELECT sum(cached_input_tokens) FROM token_usage_events",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // Cumulative knowledge is retained below, but the intervening unknown
    // baselines cannot authorize a temporal category split in rolling rows.
    assert_eq!(cached, None);
    let body: String = db
        .query_row("SELECT snapshot FROM budget_attempts", [], |r| r.get(0))
        .unwrap();
    let state: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(state["reported"]["cached_input_tokens"], 40);
}

/// Cumulative category disclosures need not fit the newest expense interval.
/// Status must not reinterpret late cache counts as additive input when the
/// earlier prefix is outside its window or has already been pruned.
#[test]
fn budget_reporting_late_categories_preserve_rolling_window_inclusive_expense() {
    let f = Fixture::new();
    let (member, attempt) = original(&f);
    let mut prefix = reported(&attempt, "window-prefix", 100, 5, false);
    prefix.reporting.as_mut().unwrap().usage.cached_input_tokens = Some(0);
    f.store.commit_budget_receipt(&prefix).unwrap();
    let mut final_receipt = reported(&attempt, "window-final", 150, 5, true);
    let observation = final_receipt.reporting.as_mut().unwrap();
    observation.observed_at_unix_seconds = 201;
    observation.usage.cached_input_tokens = Some(100);
    observation.usage.reasoning_tokens = 4;
    let outcome = f.store.commit_budget_receipt(&final_receipt).unwrap();
    let projection = outcome.reporting.as_ref().unwrap();
    assert!(projection.event_written);
    assert!(!projection.cache_read_partition_exact);
    assert!(!projection.reasoning_partition_exact);
    assert_eq!(
        f.store.commit_budget_receipt(&final_receipt).unwrap(),
        outcome
    );
    let db = f.store.open_budget().unwrap();
    let body: String = db
        .query_row(
            "SELECT body FROM budget_receipts WHERE id='window-final'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let retained: BudgetReceipt = decode(&body).unwrap();
    assert_eq!(
        retained.reporting.unwrap().usage.cached_input_tokens,
        Some(100)
    );
    drop(db);
    for prune in [false, true] {
        if prune {
            f.store
                .initialize(201 + crate::storage::token_usage::TOKEN_USAGE_RETENTION_DAYS * 86_400)
                .unwrap();
            assert_eq!(event_count(&f), 1);
        }
        let history = f
            .store
            .history_snapshot(201 + 86_400, &[1], &Default::default())
            .unwrap();
        let total = history.windows[&1].values().next().unwrap();
        assert_eq!(total.usage.input_tokens, 50);
        assert_eq!(total.usage.total_tokens(), 50);
        assert_eq!(total.usage.cached_input_tokens, None);
        assert!(!total.reasoning_known);
    }
    assert_eq!(
        f.store.budget_account(&member.root).unwrap().known.input,
        150
    );
}

/// A different native reporting owner cannot preoccupy a receipt's reserved
/// event identity and then have identical bytes adopted as budget provenance.
#[test]
fn budget_reporting_cannot_adopt_an_existing_identical_native_event() {
    let f = Fixture::new();
    let (member, attempt) = original(&f);
    let receipt = reported(&attempt, "collision", 5, 2, true);
    let report = receipt.reporting.as_ref().unwrap();
    let event = crate::storage::token_usage::TokenUsageEvent {
        id: "budget-receipt:collision".into(),
        project: attempt
            .reporting_origin
            .as_ref()
            .unwrap()
            .project
            .clone()
            .map(AccountingProjectId::from_stored)
            .transpose()
            .unwrap(),
        observed_at_unix_seconds: report.observed_at_unix_seconds,
        model: attempt.model.clone(),
        usage: report.usage,
    };
    f.store.append(&event).unwrap();
    let before = f.store.budget_account(&member.root).unwrap();
    assert!(f.store.commit_budget_receipt(&receipt).is_err());
    assert_eq!(f.store.budget_account(&member.root).unwrap(), before);
    assert_eq!(event_count(&f), 1);
}

/// Budget prefixes can settle before full reporting evidence exists. Its later
/// cumulative report projects the original known total once, not just the last
/// budget delta, and permanent receipts prevent re-creating pruned events.
#[test]
fn budget_reporting_delayed_projection_and_pruning_preserve_lifetime_receipts() {
    let f = Fixture::new();
    let (member, attempt) = original(&f);
    let prefix = receipt(&attempt, "early-prefix", Some(8), None, false);
    f.store.commit_budget_receipt(&prefix).unwrap();
    let final_receipt = reported(&attempt, "late-report", 11, 2, true);
    let outcome = f.store.commit_budget_receipt(&final_receipt).unwrap();
    assert_eq!(outcome.known.input, 11);
    let db = f.store.open_budget().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT sum(input_tokens) FROM token_usage_events",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        11
    );
    drop(db);
    f.store
        .initialize(200 + crate::storage::token_usage::TOKEN_USAGE_RETENTION_DAYS * 86_400 + 1)
        .unwrap();
    assert_eq!(event_count(&f), 0);
    assert_eq!(
        f.store.commit_budget_receipt(&final_receipt).unwrap(),
        outcome
    );
    assert_eq!(event_count(&f), 0);
    assert_eq!(
        f.store.budget_account(&member.root).unwrap().known,
        outcome.known
    );
}

/// Unknown dimensions, changed attribution, category regressions and unstoreable
/// timestamps fail without partial root/event mutation. Explicit zero remains
/// valid only with actual known-zero dimensions and valid metadata.
#[test]
fn budget_reporting_invalid_evidence_is_atomic_and_unknown_is_not_zero() {
    let f = Fixture::new();
    let (member, attempt) = original(&f);
    let before = f.store.budget_account(&member.root).unwrap();
    for case in 0..5 {
        let mut invalid = reported(&attempt, "invalid", 0, 0, true);
        match case {
            0 => invalid.snapshot.output = None,
            1 => invalid.reporting.as_mut().unwrap().observed_at_unix_seconds = u64::MAX,
            2 => invalid.reporting.as_mut().unwrap().usage.input_tokens = 1,
            3 => {
                invalid
                    .reporting
                    .as_mut()
                    .unwrap()
                    .usage
                    .cached_input_tokens = Some(1)
            }
            _ => invalid.attempt.reporting_origin = Some(BudgetReportingOrigin { project: None }),
        }
        assert!(f.store.commit_budget_receipt(&invalid).is_err());
        assert_eq!(f.store.budget_account(&member.root).unwrap(), before);
        assert_eq!(event_count(&f), 0);
    }
    let mut first = reported(&attempt, "first-valid", 100, 5, false);
    first.reporting.as_mut().unwrap().usage.cached_input_tokens = Some(30);
    f.store.commit_budget_receipt(&first).unwrap();
    let before = f.store.budget_account(&member.root).unwrap();
    let mut regressed = reported(&attempt, "regressed", 150, 7, true);
    regressed
        .reporting
        .as_mut()
        .unwrap()
        .usage
        .cached_input_tokens = Some(20);
    assert!(f.store.commit_budget_receipt(&regressed).is_err());
    assert_eq!(f.store.budget_account(&member.root).unwrap(), before);
    assert_eq!(event_count(&f), 1);
}

fn original(f: &Fixture) -> (BudgetMembership, BudgetAttempt) {
    let member = f.member("reporting-root", None);
    f.active(&member);
    let mut attempt = attempt(&member, "original-send");
    let origin = AccountingOrigin::Project(
        AccountingProjectId::from_stored("11111111-1111-4111-8111-111111111111".into()).unwrap(),
    );
    attempt.reporting_origin = Some(BudgetReportingOrigin::capture(&origin));
    f.store.register_budget_attempt(&attempt).unwrap();
    (member, attempt)
}

fn reported(
    attempt: &BudgetAttempt,
    id: &str,
    input: u64,
    output: u64,
    terminal: bool,
) -> BudgetReceipt {
    let mut receipt = receipt(attempt, id, Some(input), Some(output), terminal);
    receipt.reporting = Some(BudgetReportingObservation {
        observed_at_unix_seconds: 200,
        usage: ModelTokenUsage {
            input_tokens: input,
            output_tokens: output,
            ..Default::default()
        },
    });
    receipt
}

fn event_count(f: &Fixture) -> i64 {
    f.store
        .open_budget()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM token_usage_events", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn budget_reporting_prefix_final_and_lost_reply_conserve_original_event_partitions() {
    let f = Fixture::new();
    let (member, attempt) = original(&f);
    let mut prefix = reported(&attempt, "prefix", 100, 7, false);
    prefix.reporting.as_mut().unwrap().usage.cached_input_tokens = Some(30);
    prefix
        .reporting
        .as_mut()
        .unwrap()
        .usage
        .cache_write_input_tokens = Some(10);
    prefix.reporting.as_mut().unwrap().usage.reasoning_tokens = 2;
    let first = f.store.commit_budget_receipt(&prefix).unwrap();
    let mut final_receipt = reported(&attempt, "final", 150, 11, true);
    let report = final_receipt.reporting.as_mut().unwrap();
    report.observed_at_unix_seconds = 201;
    report.usage.cached_input_tokens = Some(50);
    report.usage.cache_write_input_tokens = Some(20);
    report.usage.reasoning_tokens = 3;
    let last = f.store.commit_budget_receipt(&final_receipt).unwrap();
    let reopened = TokenUsageStore::new(f.store.path());
    assert_eq!(reopened.commit_budget_receipt(&prefix).unwrap(), first);
    assert_eq!(
        reopened.commit_budget_receipt(&final_receipt).unwrap(),
        last
    );
    assert_eq!(event_count(&f), 2);
    assert_eq!(
        f.store.budget_account(&member.root).unwrap().known.input,
        150
    );
    assert_eq!(last.known.output, 11);
    let db = f.store.open_budget().unwrap();
    let totals: (i64,i64,i64,i64,i64) = db.query_row(
        "SELECT sum(input_tokens),sum(output_tokens),sum(reasoning_tokens),sum(cached_input_tokens),sum(cache_write_input_tokens) FROM token_usage_events",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    ).unwrap();
    assert_eq!(totals, (150, 11, 3, 50, 20));
    let project: String = db
        .query_row(
            "SELECT project_id FROM token_usage_events WHERE id='budget-receipt:final'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        Some(project),
        attempt.reporting_origin.as_ref().unwrap().project
    );
    let time: i64 = db
        .query_row(
            "SELECT observed_at FROM token_usage_events WHERE id='budget-receipt:final'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(time, 201);
    let mut changed = final_receipt.clone();
    changed.reporting.as_mut().unwrap().observed_at_unix_seconds = 202;
    assert!(f.store.commit_budget_receipt(&changed).is_err());
    assert_eq!(event_count(&f), 2);
}

/// Both a reporting INSERT refusal and a later receipt INSERT refusal roll back
/// the entire attempt/root/event transition. Retrying only this immutable
/// receipt after repair commits once, without reissuing the original request.
#[test]
fn budget_reporting_write_failures_roll_back_events_root_and_receipt_together() {
    for table in ["token_usage_events", "budget_receipts"] {
        let f = Fixture::new();
        let (member, attempt) = original(&f);
        let before = f.store.budget_account(&member.root).unwrap();
        let receipt = reported(&attempt, "original-receipt", 17, 5, true);
        let db = f.store.open_budget().unwrap();
        db.execute_batch(&format!("CREATE TRIGGER refuse_reporting BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'fixture refusal'); END;")).unwrap();
        assert!(f.store.commit_budget_receipt(&receipt).is_err());
        assert_eq!(f.store.budget_account(&member.root).unwrap(), before);
        assert_eq!(event_count(&f), 0);
        assert_eq!(
            db.query_row("SELECT count(*) FROM budget_receipts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT terminal FROM budget_attempts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        db.execute_batch("DROP TRIGGER refuse_reporting;").unwrap();
        let outcome = f.store.commit_budget_receipt(&receipt).unwrap();
        assert_eq!(f.store.commit_budget_receipt(&receipt).unwrap(), outcome);
        assert_eq!(outcome.known.input, 17);
        assert_eq!(event_count(&f), 1);
    }
}

#[test]
fn budget_reporting_missing_dimensions_and_explicit_zero_remain_distinct() {
    let f = Fixture::new();
    let (_, attempt) = original(&f);
    let partial = receipt(&attempt, "partial", Some(8), None, false);
    f.store.commit_budget_receipt(&partial).unwrap();
    let unknown = receipt(&attempt, "unknown-final", None, None, true);
    assert_eq!(
        f.store.commit_budget_receipt(&unknown).unwrap().known.input,
        8
    );
    assert_eq!(event_count(&f), 0);
    let db = f.store.open_budget().unwrap();
    let state: String = db
        .query_row("SELECT snapshot FROM budget_attempts", [], |r| r.get(0))
        .unwrap();
    let state: serde_json::Value = serde_json::from_str(&state).unwrap();
    assert_eq!(state["prefix"]["input"], 8);
    assert!(state["terminal"]["input"].is_null());
    assert!(state["terminal"]["output"].is_null());
    drop(db);
    let other = Fixture::new();
    let (_, attempt) = original(&other);
    let zero = reported(&attempt, "explicit-zero", 0, 0, true);
    other.store.commit_budget_receipt(&zero).unwrap();
    assert_eq!(event_count(&other), 0); // existing native reporting omits empty deltas
    assert_eq!(
        other
            .store
            .open_budget()
            .unwrap()
            .query_row("SELECT terminal FROM budget_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn budget_reporting_legacy_migration_and_invalid_provenance_never_invent_expense() {
    let f = Fixture::new();
    let member = f.member("legacy", None);
    f.active(&member);
    let legacy = attempt(&member, "legacy-send");
    f.store.register_budget_attempt(&legacy).unwrap();
    let old = receipt(&legacy, "legacy-receipt", Some(9), Some(2), true);
    f.store.commit_budget_receipt(&old).unwrap();
    let db = f.store.open_budget().unwrap();
    let original_bytes: (String, String) = db
        .query_row("SELECT body,outcome FROM budget_receipts", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    db.execute_batch("PRAGMA user_version=16;").unwrap();
    f.store.initialize(100).unwrap();
    let after_bytes: (String, String) = db
        .query_row("SELECT body,outcome FROM budget_receipts", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(after_bytes, original_bytes);
    assert_eq!(f.store.commit_budget_receipt(&old).unwrap().known.input, 9);
    assert_eq!(event_count(&f), 0);
    let mut changed = legacy.clone();
    changed.reporting_origin = Some(BudgetReportingOrigin { project: None });
    assert!(f.store.register_budget_attempt(&changed).is_err());
    let unqualified = reported(&legacy, "unqualified", 9, 2, true);
    assert!(f.store.commit_budget_receipt(&unqualified).is_err());
    assert_eq!(event_count(&f), 0);
    drop(db);
}
