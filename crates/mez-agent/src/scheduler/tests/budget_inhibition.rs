//! Inhibition is orthogonal to queue/claim custody and completion of issued work.

use super::*;

/// Approval-oriented immediate resumption is still a new execution admission.
/// A retained pause must survive it without consuming the blocked owner.
#[test]
fn scheduler_human_pause_refuses_immediate_blocked_resumption() {
    let mut scheduler = AgentScheduler::new(1).unwrap();
    scheduler.enqueue(work("held", "owner", "%1")).unwrap();
    scheduler.start_ready().unwrap();
    scheduler.block_running("held").unwrap();
    scheduler.set_human_paused("held", true);

    let resumed = scheduler.resume_blocked("held");
    assert!(resumed.is_err(), "human pause must fence immediate resume");
    assert_eq!(scheduler.snapshot().blocked, 1);
    assert_eq!(scheduler.snapshot().active_capacity_used, 0);
    scheduler.set_human_paused("held", false);
    assert_eq!(scheduler.resume_blocked("held").unwrap().turn_id, "held");
}

/// Removing either reason first cannot clear the other. A held ready head
/// does not consume capacity or prevent an independent agent from progressing.
#[test]
fn scheduler_budget_and_human_inhibition_are_independent_in_both_release_orders() {
    for budget_first in [true, false] {
        let mut scheduler = AgentScheduler::new(1).unwrap();
        scheduler.enqueue(work("held", "owner", "%1")).unwrap();
        scheduler.enqueue(work("healthy", "other", "%2")).unwrap();
        assert!(scheduler.set_budget_exhausted("held", true));
        scheduler.set_human_paused("held", true);
        assert_eq!(scheduler.start_ready().unwrap().turn_id, "healthy");
        scheduler.complete("healthy").unwrap();
        if budget_first {
            assert!(scheduler.set_budget_exhausted("held", false));
        } else {
            scheduler.set_human_paused("held", false);
        }
        assert!(scheduler.start_ready().is_none());
        if budget_first {
            scheduler.set_human_paused("held", false);
        } else {
            assert!(scheduler.set_budget_exhausted("held", false));
        }
        assert_eq!(scheduler.start_ready().unwrap().turn_id, "held");
        scheduler.complete("held").unwrap();
        assert!(scheduler.start_ready().is_none());
    }
}

/// Both fair and immediate reacquisition retain original claims under a hold;
/// a queued same-pane sibling cannot take over the blocked owner.
#[test]
fn scheduler_budget_inhibition_fences_immediate_and_fair_reacquisition() {
    for waiting in [true, false] {
        let mut scheduler = AgentScheduler::new(1).unwrap();
        scheduler.enqueue(work("held", "owner", "%1")).unwrap();
        scheduler.enqueue(work("sibling", "sibling", "%1")).unwrap();
        scheduler.start_ready().unwrap();
        assert!(scheduler.set_budget_exhausted("held", true));
        if waiting {
            scheduler.wait_running("held").unwrap();
            scheduler.requeue_waiting("held").unwrap();
        } else {
            scheduler.block_running("held").unwrap();
            assert!(scheduler.resume_blocked("held").is_err());
            assert_eq!(scheduler.snapshot().blocked, 1);
            scheduler.requeue_blocked("held").unwrap();
        }
        assert_eq!(scheduler.snapshot().reacquiring, 1);
        assert_eq!(scheduler.snapshot().active_capacity_used, 0);
        assert!(scheduler.start_ready_where(|_| true).is_none());
        assert!(scheduler.set_budget_exhausted("held", false));
        assert_eq!(scheduler.start_ready().unwrap().turn_id, "held");
        scheduler.complete("held").unwrap();
        assert_eq!(scheduler.start_ready().unwrap().turn_id, "sibling");
    }
}

/// Installing the reason cannot invent settlement of already-issued work;
/// removing it cannot invent completion of a dependency or an approval.
#[test]
fn scheduler_budget_inhibition_preserves_running_and_unrequeued_wait_owners() {
    for waiting in [true, false] {
        let mut scheduler = AgentScheduler::new(1).unwrap();
        scheduler.enqueue(work("issued", "owner", "%1")).unwrap();
        scheduler.start_ready().unwrap();
        assert!(scheduler.set_budget_exhausted("issued", true));
        assert_eq!(scheduler.snapshot().active_capacity_used, 1);
        if waiting {
            scheduler.wait_running("issued").unwrap();
        } else {
            scheduler.block_running("issued").unwrap();
        }
        assert!(scheduler.set_budget_exhausted("issued", false));
        assert!(scheduler.start_ready().is_none());
        assert_eq!(scheduler.snapshot().waiting, usize::from(waiting));
        assert_eq!(scheduler.snapshot().blocked, usize::from(!waiting));
    }
}

/// A full queue cannot lose the inhibition or mutate admission charges, and
/// duplicate state notifications do not rotate the original fairness order.
#[test]
fn scheduler_budget_inhibition_survives_full_queue_without_reenqueuing() {
    let mut scheduler = AgentScheduler::with_limits(1, 2, usize::MAX).unwrap();
    scheduler.enqueue(work("first", "a1", "%1")).unwrap();
    scheduler.enqueue(work("second", "a2", "%2")).unwrap();
    let retained_bytes = scheduler.snapshot().queued_bytes;
    for _ in 0..3 {
        assert!(scheduler.set_budget_exhausted("first", true));
        assert!(scheduler.set_budget_exhausted("second", true));
    }
    assert_eq!(
        scheduler
            .enqueue(work("third", "a3", "%3"))
            .unwrap_err()
            .kind(),
        SchedulerErrorKind::QueueFull
    );
    assert!(scheduler.start_ready().is_none());
    assert_eq!(scheduler.snapshot().queued_bytes, retained_bytes);
    for _ in 0..3 {
        assert!(scheduler.set_budget_exhausted("first", false));
        assert!(scheduler.set_budget_exhausted("second", false));
    }
    assert_eq!(scheduler.snapshot().queued, 2);
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "first");
    scheduler.complete("first").unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "second");
}

/// Retired identities cannot retain inhibition/wake debt. Reusing an id in a
/// new admission does not inherit the old owner's reason after cancellation
/// from any lifecycle state, nor after completion of an issued turn.
#[test]
fn scheduler_budget_inhibition_is_removed_on_retirement() {
    for state in [
        "queued",
        "running",
        "blocked",
        "waiting",
        "reacquiring",
        "completed",
    ] {
        let mut scheduler = AgentScheduler::new(1).unwrap();
        assert!(!scheduler.set_budget_exhausted("unknown", true));
        scheduler.enqueue(work("original", "owner", "%1")).unwrap();
        if state != "queued" {
            scheduler.start_ready().unwrap();
        }
        assert!(scheduler.set_budget_exhausted("original", true));
        match state {
            "blocked" => {
                scheduler.block_running("original").unwrap();
            }
            "waiting" => {
                scheduler.wait_running("original").unwrap();
            }
            "reacquiring" => {
                scheduler.wait_running("original").unwrap();
                scheduler.requeue_waiting("original").unwrap();
            }
            _ => {}
        }
        if state == "completed" {
            scheduler.complete("original").unwrap();
        } else {
            scheduler.cancel("original").unwrap();
        }
        assert!(!scheduler.set_budget_exhausted("original", true));
        scheduler.enqueue(work("original", "owner", "%1")).unwrap();
        assert_eq!(
            scheduler.start_ready().unwrap().turn_id,
            "original",
            "{state}"
        );
    }
}
