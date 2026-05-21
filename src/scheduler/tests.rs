//! Tests for scheduler queue fairness, concurrency limits, and pane policy.

use super::{
    AgentScheduler, DEFAULT_MAX_CONCURRENT_AGENTS, ScheduledWork, ScheduledWorkKind,
    SchedulerCancellation,
};

/// Runs the work operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn work(turn: &str, agent: &str, pane: &str) -> ScheduledWork {
    ScheduledWork {
        turn_id: turn.to_string(),
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
    assert_eq!(error.kind(), crate::error::MezErrorKind::NotFound);
}

/// Verifies that turns blocked on user interaction release global scheduler
/// capacity while still preventing later work for the same pane from starting.
#[test]
fn scheduler_blocked_turns_release_capacity_but_keep_pane_exclusive() {
    let mut scheduler = AgentScheduler::new(1).unwrap();
    scheduler.enqueue(work("t1", "a1", "%1")).unwrap();
    scheduler.enqueue(work("t2", "a2", "%1")).unwrap();
    scheduler.enqueue(work("t3", "a3", "%2")).unwrap();

    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t1");
    scheduler.block_running("t1").unwrap();

    assert_eq!(scheduler.snapshot().running, 0);
    assert_eq!(scheduler.snapshot().blocked, 1);
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t3");
    assert!(scheduler.start_ready().is_none());

    scheduler.complete("t3").unwrap();
    scheduler.resume_blocked("t1").unwrap();
    assert_eq!(scheduler.snapshot().running, 1);
    assert_eq!(scheduler.snapshot().blocked, 0);
    assert!(scheduler.start_ready().is_none());
    scheduler.complete("t1").unwrap();
    assert_eq!(scheduler.start_ready().unwrap().turn_id, "t2");
}
