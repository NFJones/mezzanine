//! Tests for scheduler queue fairness, concurrency limits, and pane policy.

use super::{
    AgentScheduler, DEFAULT_MAX_CONCURRENT_AGENTS, DEFAULT_MAX_QUEUED_BYTES,
    DEFAULT_MAX_QUEUED_TURNS, ScheduledWork, ScheduledWorkKind, SchedulerCancellation,
    SchedulerErrorKind,
};

/// Runs the work operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn work(turn: &str, agent: &str, pane: &str) -> ScheduledWork {
    ScheduledWork {
        turn_id: turn.to_string(),
        conversation_id: "conversation-1".to_string(),
        agent_id: agent.to_string(),
        pane_id: Some(pane.to_string()),
        kind: ScheduledWorkKind::ShellCapable,
    }
}

/// Verifies that the default scheduler constructor uses the repository-wide
/// agent concurrency value.
#[test]
fn default_agent_concurrency_is_four() {
    let scheduler = AgentScheduler::with_default_limit();

    assert_eq!(
        scheduler.snapshot().max_concurrent_agents,
        DEFAULT_MAX_CONCURRENT_AGENTS
    );
    assert_eq!(
        scheduler.snapshot().max_queued_turns,
        DEFAULT_MAX_QUEUED_TURNS
    );
    assert_eq!(
        scheduler.snapshot().max_queued_bytes,
        DEFAULT_MAX_QUEUED_BYTES
    );
}

/// Verifies queue admission accepts the exact configured count limit, rejects
/// the next turn without retaining it, and frees capacity after cancellation.
#[test]
fn scheduler_enforces_queued_turn_count_budget() {
    let mut scheduler = AgentScheduler::with_limits(1, 2, usize::MAX).unwrap();
    scheduler.enqueue(work("t1", "a1", "%1")).unwrap();
    scheduler.enqueue(work("t2", "a2", "%2")).unwrap();

    let error = scheduler.enqueue(work("t3", "a3", "%3")).unwrap_err();

    assert_eq!(error.kind(), SchedulerErrorKind::InvalidState);
    assert_eq!(scheduler.snapshot().queued, 2);
    assert_eq!(scheduler.snapshot().admission_rejections, 1);
    scheduler.cancel("t1").unwrap();
    scheduler.enqueue(work("t3", "a3", "%3")).unwrap();
    assert_eq!(scheduler.snapshot().queued, 2);
}

/// Verifies estimated queued bytes are charged exactly, an oversized next
/// request is rejected, and cancellation removes the retained-byte charge.
#[test]
fn scheduler_enforces_estimated_queued_byte_budget() {
    let mut scheduler = AgentScheduler::with_limits(1, 4, usize::MAX).unwrap();
    scheduler.enqueue(work("t1", "a1", "%1")).unwrap();
    let exact_bytes = scheduler.snapshot().queued_bytes;
    scheduler.cancel("t1").unwrap();
    scheduler.set_queue_limits(4, exact_bytes).unwrap();

    scheduler.enqueue(work("t1", "a1", "%1")).unwrap();
    let error = scheduler.enqueue(work("t2", "a2", "%2")).unwrap_err();

    assert_eq!(error.kind(), SchedulerErrorKind::InvalidState);
    assert_eq!(scheduler.snapshot().queued_bytes, exact_bytes);
    assert_eq!(scheduler.snapshot().admission_rejections, 1);
    scheduler.cancel("t1").unwrap();
    assert_eq!(scheduler.snapshot().queued_bytes, 0);
}

/// Verifies indexed turn-id state is removed across queued and running
/// cancellation so the same stable identifier may be admitted again safely.
#[test]
fn scheduler_cancellation_cleans_turn_and_claim_indexes() {
    let mut scheduler = AgentScheduler::new(1).unwrap();
    scheduler.enqueue(work("reused", "a1", "%1")).unwrap();
    scheduler.cancel("reused").unwrap();
    scheduler.enqueue(work("reused", "a1", "%1")).unwrap();
    scheduler.start_ready().unwrap();
    scheduler.cancel("reused").unwrap();

    scheduler.enqueue(work("reused", "a1", "%1")).unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "reused");
}

/// Verifies per-agent ready heads avoid rescanning unrelated queued work when
/// dispatch removes a planning-only turn from a large multi-agent queue.
#[test]
fn scheduler_ready_dispatch_does_not_scan_unrelated_agents() {
    let mut scheduler = AgentScheduler::with_limits(1, 128, usize::MAX).unwrap();
    for index in 0..64 {
        scheduler
            .enqueue(ScheduledWork {
                turn_id: format!("turn-{index}"),
                conversation_id: "conversation-1".to_string(),
                agent_id: format!("agent-{index}"),
                pane_id: None,
                kind: ScheduledWorkKind::PlanningOnly,
            })
            .unwrap();
    }
    let checks_before_dispatch = scheduler.snapshot().readiness_checks;

    assert_eq!(scheduler.start_ready().unwrap().turn_id, "turn-0");

    assert_eq!(
        scheduler.snapshot().readiness_checks,
        checks_before_dispatch
    );
    assert_eq!(scheduler.snapshot().queued, 63);
}

/// Verifies queue diagnostics expose a monotonic age for the oldest retained
/// turn without changing queue order or requiring wall-clock timestamps.
#[test]
fn scheduler_snapshot_reports_oldest_queue_age() {
    let mut scheduler = AgentScheduler::new(1).unwrap();
    scheduler.enqueue(work("t1", "a1", "%1")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));

    assert!(scheduler.snapshot().oldest_queued_age_ms >= 1);
}

/// Verifies that lowering the concurrency limit does not cancel already
/// running work and prevents new starts until capacity is available.
#[test]
fn scheduler_limit_can_be_reconfigured_without_cancelling_running_work() {
    let mut scheduler = AgentScheduler::new(2).unwrap();
    scheduler.enqueue(work("t1", "a1", "%1")).unwrap();
    scheduler.enqueue(work("t2", "a2", "%2")).unwrap();
    scheduler.enqueue(work("t3", "a3", "%3")).unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t1");
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t2");

    scheduler.set_max_concurrent_agents(1).unwrap();

    assert_eq!(scheduler.snapshot().max_concurrent_agents, 1);
    assert_eq!(scheduler.snapshot().running, 2);
    assert!(scheduler.start_ready().is_none());
    scheduler.complete("t1").unwrap();
    assert!(scheduler.start_ready().is_none());
    scheduler.complete("t2").unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t3");
    assert!(scheduler.set_max_concurrent_agents(0).is_err());
}

/// Verifies that shell-capable turns cannot concurrently claim the same pane.
#[test]
fn scheduler_rejects_two_shell_turns_for_same_pane_until_completion() {
    let mut scheduler = AgentScheduler::new(4).unwrap();
    scheduler.enqueue(work("t1", "a1", "%1")).unwrap();
    scheduler.enqueue(work("t2", "a2", "%1")).unwrap();
    scheduler.enqueue(work("t3", "a3", "%2")).unwrap();

    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t1");
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t3");
    assert!(scheduler.start_ready().is_none());

    scheduler.complete("t1").unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t2");
}

/// Verifies that blocked queue entries rotate behind runnable entries so a
/// pane conflict does not starve independent work.
#[test]
fn scheduler_fairly_rotates_blocked_work_without_starving_ready_agents() {
    let mut scheduler = AgentScheduler::new(2).unwrap();
    scheduler.enqueue(work("t1", "a1", "%1")).unwrap();
    scheduler.enqueue(work("t2", "a2", "%1")).unwrap();
    scheduler.enqueue(work("t3", "a3", "%2")).unwrap();

    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t1");
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t3");
    assert_eq!(
        scheduler
            .queued_turns()
            .map(|work| work.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["t2"]
    );
}

/// Verifies that fair progress is not merely FIFO: when capacity opens, a
/// runnable turn from a different agent starts before a follow-up turn owned by
/// the most recently started agent.
#[test]
fn scheduler_prefers_other_runnable_agents_after_completion() {
    let mut scheduler = AgentScheduler::new(1).unwrap();
    scheduler.enqueue(work("t1", "a1", "%1")).unwrap();
    scheduler.enqueue(work("t2", "a1", "%1")).unwrap();
    scheduler.enqueue(work("t3", "a2", "%2")).unwrap();

    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t1");
    scheduler.complete("t1").unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t3");
    scheduler.complete("t3").unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t2");
}

/// Verifies that planning-only work does not take exclusive ownership of a
/// pane even when it carries pane context.
#[test]
fn planning_only_work_does_not_claim_a_pane() {
    let mut scheduler = AgentScheduler::new(2).unwrap();
    scheduler.enqueue(work("t1", "a1", "%1")).unwrap();
    scheduler
        .enqueue(ScheduledWork {
            turn_id: "t2".to_string(),
            conversation_id: "conversation-1".to_string(),
            agent_id: "a2".to_string(),
            pane_id: Some("%1".to_string()),
            kind: ScheduledWorkKind::PlanningOnly,
        })
        .unwrap();

    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t1");
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t2");
}

/// Verifies that cancellation handles both queued and running work and reports
/// unknown turn ids as not found.
#[test]
fn scheduler_can_cancel_queued_or_running_turns() {
    let mut scheduler = AgentScheduler::new(1).unwrap();
    scheduler.enqueue(work("t1", "a1", "%1")).unwrap();
    scheduler.enqueue(work("t2", "a2", "%2")).unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t1");

    let queued = scheduler.cancel("t2").unwrap();
    assert!(matches!(queued, SchedulerCancellation::Queued(_)));
    assert_eq!(scheduler.snapshot().queued, 0);

    let running = scheduler.cancel("t1").unwrap();
    assert!(matches!(running, SchedulerCancellation::Running(_)));
    assert_eq!(scheduler.snapshot().running, 0);

    let error = scheduler.cancel("missing").unwrap_err();
    assert_eq!(error.kind(), SchedulerErrorKind::NotFound);
}

/// Verifies that turns blocked on user interaction release provider capacity
/// while retaining exclusive ownership of their agent and shell pane.
#[test]
fn scheduler_blocked_turns_release_capacity_and_keep_pane_exclusive() {
    let mut scheduler = AgentScheduler::new(1).unwrap();
    scheduler.enqueue(work("t1", "a1", "%1")).unwrap();
    scheduler.enqueue(work("t2", "a2", "%1")).unwrap();
    scheduler.enqueue(work("t3", "a3", "%2")).unwrap();

    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t1");
    scheduler.block_running("t1").unwrap();

    assert_eq!(scheduler.snapshot().running, 0);
    assert_eq!(scheduler.snapshot().blocked, 1);
    assert_eq!(scheduler.snapshot().active_capacity_used, 0);
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t3");
    assert!(scheduler.queued_turns().any(|work| work.turn_id == "t2"));

    scheduler.requeue_blocked("t1").unwrap();
    assert_eq!(scheduler.snapshot().reacquiring, 1);
    assert!(scheduler.start_ready().is_none());

    scheduler.complete("t3").unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t1");
    assert_eq!(scheduler.snapshot().reacquiring, 0);
    assert_eq!(scheduler.snapshot().running, 1);
    assert_eq!(scheduler.snapshot().blocked, 0);
    assert!(scheduler.start_ready().is_none());
    scheduler.complete("t1").unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t2");
    scheduler.complete("t2").unwrap();
    assert!(scheduler.start_ready().is_none());
}

/// Verifies cancellation removes an approval-blocked turn before or during
/// capacity reacquisition without leaking its private agent or pane claim.
#[test]
fn scheduler_cancels_blocked_turns_and_reacquisition_claims() {
    let mut scheduler = AgentScheduler::new(1).unwrap();
    scheduler.enqueue(work("blocked", "a1", "%1")).unwrap();
    scheduler.enqueue(work("next", "a2", "%1")).unwrap();
    scheduler.start_ready().unwrap();
    scheduler.block_running("blocked").unwrap();

    let blocked = scheduler.cancel("blocked").unwrap();
    assert!(matches!(blocked, SchedulerCancellation::Blocked(_)));
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "next");
    scheduler.complete("next").unwrap();

    scheduler.enqueue(work("blocked-2", "a1", "%1")).unwrap();
    scheduler.enqueue(work("next-2", "a2", "%1")).unwrap();
    scheduler.start_ready().unwrap();
    scheduler.block_running("blocked-2").unwrap();
    scheduler.requeue_blocked("blocked-2").unwrap();

    let queued = scheduler.cancel("blocked-2").unwrap();
    assert!(matches!(queued, SchedulerCancellation::Queued(_)));
    assert_eq!(scheduler.snapshot().reacquiring, 0);
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "next-2");
}

/// Verifies dependency-waiting parents release provider capacity while
/// retaining lifecycle and pane ownership.
///
/// With a one-slot limit, independent dependent work must start immediately.
/// Work targeting the waiting parent's pane remains ineligible, and the parent
/// re-enters the ordinary fairness queue once its dependency settles.
#[test]
fn scheduler_dependency_waits_release_capacity_and_reacquire_fairly() {
    let mut scheduler = AgentScheduler::new(1).unwrap();
    scheduler
        .enqueue(work("parent", "parent-agent", "%1"))
        .unwrap();
    scheduler
        .enqueue(work("same-pane", "other-agent", "%1"))
        .unwrap();
    scheduler
        .enqueue(work("child", "child-agent", "%2"))
        .unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "parent");

    scheduler.wait_running("parent").unwrap();

    assert_eq!(scheduler.snapshot().active_capacity_used, 0);
    assert_eq!(scheduler.snapshot().waiting, 1);
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "child");
    assert_eq!(scheduler.snapshot().active_capacity_used, 1);
    scheduler.requeue_waiting("parent").unwrap();
    assert_eq!(scheduler.snapshot().waiting, 0);
    assert_eq!(scheduler.snapshot().reacquiring, 1);
    assert!(scheduler.start_ready().is_none());

    scheduler.complete("child").unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "parent");
    assert_eq!(scheduler.snapshot().reacquiring, 0);
    scheduler.complete("parent").unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "same-pane");
}

/// Verifies cancellation removes both dependency waits and queued
/// reacquisition claims without leaking capacity or pane exclusivity.
#[test]
fn scheduler_cancels_dependency_waits_and_reacquisition_claims() {
    let mut scheduler = AgentScheduler::new(1).unwrap();
    scheduler
        .enqueue(work("parent", "parent-agent", "%1"))
        .unwrap();
    scheduler.enqueue(work("next", "next-agent", "%1")).unwrap();
    scheduler.start_ready().unwrap();
    scheduler.wait_running("parent").unwrap();

    let waiting = scheduler.cancel("parent").unwrap();
    assert!(matches!(waiting, SchedulerCancellation::Waiting(_)));
    assert_eq!(scheduler.snapshot().waiting, 0);
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "next");
    scheduler.complete("next").unwrap();

    scheduler
        .enqueue(work("parent-2", "parent-agent", "%1"))
        .unwrap();
    scheduler
        .enqueue(work("next-2", "next-agent", "%1"))
        .unwrap();
    scheduler.start_ready().unwrap();
    scheduler.wait_running("parent-2").unwrap();
    scheduler.requeue_waiting("parent-2").unwrap();
    let queued = scheduler.cancel("parent-2").unwrap();

    assert!(matches!(queued, SchedulerCancellation::Queued(_)));
    assert_eq!(scheduler.snapshot().reacquiring, 0);
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "next-2");
}

/// Verifies multiple dependency-waiting parents do not reduce newly configured
/// active capacity.
///
/// Both parents retain their wait records and pane claims, while an unrelated
/// child can use the sole provider slot after the limit is lowered.
#[test]
fn scheduler_multiple_dependency_waits_do_not_consume_active_capacity() {
    let mut scheduler = AgentScheduler::new(2).unwrap();
    scheduler
        .enqueue(work("parent-1", "parent-1", "%1"))
        .unwrap();
    scheduler
        .enqueue(work("parent-2", "parent-2", "%2"))
        .unwrap();
    scheduler.enqueue(work("child", "child", "%3")).unwrap();
    scheduler.start_ready().unwrap();
    scheduler.start_ready().unwrap();
    scheduler.wait_running("parent-1").unwrap();
    scheduler.wait_running("parent-2").unwrap();
    scheduler.set_max_concurrent_agents(1).unwrap();

    assert_eq!(scheduler.snapshot().waiting, 2);
    assert_eq!(scheduler.snapshot().active_capacity_used, 0);
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "child");
    assert_eq!(scheduler.snapshot().active_capacity_used, 1);
}

/// Verifies a transient runtime readiness gate leaves rejected work queued
/// while allowing an independent later agent to use available capacity.
#[test]
fn scheduler_runtime_gate_skips_unready_work_without_dequeuing_it() {
    let mut scheduler = AgentScheduler::new(1).unwrap();
    scheduler
        .enqueue(work("gated", "gated-agent", "%1"))
        .unwrap();
    scheduler
        .enqueue(work("ready", "ready-agent", "%2"))
        .unwrap();

    let started = scheduler
        .start_ready_where(|work| work.turn_id != "gated")
        .unwrap();

    assert_eq!(started.turn_id, "ready");
    assert_eq!(
        scheduler
            .queued_turns()
            .map(|work| work.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["gated"]
    );
    scheduler.complete("ready").unwrap();
    assert_eq!(
        scheduler.start_ready_where(|_| true).unwrap().turn_id,
        "gated"
    );
}
