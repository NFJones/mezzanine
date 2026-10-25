//! Agent scheduler ownership operations.
//!
//! This module owns the narrow runtime boundary around queued, running,
//! blocked, cancelled, and concurrency-limited agent work. The scheduler field
//! remains private to `RuntimeAgentComponent`.

use super::{AgentScheduler, ProviderRetryScheduler, Result, RuntimeSessionService, ScheduledWork};
use mez_agent::ProviderRetryPolicy;

impl RuntimeSessionService {
    /// Returns the agent scheduler for read-only diagnostics and prompt context.
    pub(crate) fn agent_scheduler(&self) -> &AgentScheduler {
        &self.agent.agent_scheduler
    }

    /// Reports whether one turn is already represented by any scheduler state.
    pub(crate) fn agent_work_is_scheduled(&self, turn_id: &str) -> bool {
        self.agent
            .agent_scheduler
            .queued_turns()
            .any(|work| work.turn_id == turn_id)
            || self
                .agent
                .agent_scheduler
                .running_turns()
                .any(|work| work.turn_id == turn_id)
            || self
                .agent
                .agent_scheduler
                .blocked_turns()
                .any(|work| work.turn_id == turn_id)
            || self
                .agent
                .agent_scheduler
                .waiting_turns()
                .any(|work| work.turn_id == turn_id)
            || self
                .agent
                .agent_scheduler
                .reacquiring_turns()
                .any(|work| work.turn_id == turn_id)
    }

    /// Returns mutable scheduler access to crate-local regression tests.
    #[cfg(test)]
    pub(crate) fn agent_scheduler_mut(&mut self) -> &mut AgentScheduler {
        &mut self.agent.agent_scheduler
    }

    /// Returns mutable provider-retry reducer access to crate-local tests.
    #[cfg(test)]
    pub(crate) fn provider_retry_scheduler_mut(&mut self) -> &mut ProviderRetryScheduler {
        &mut self.agent.provider_retry_scheduler
    }

    /// Applies the configured global agent concurrency limit.
    pub(crate) fn configure_agent_scheduler_limit(
        &mut self,
        max_concurrent_agents: usize,
    ) -> Result<()> {
        self.agent
            .agent_scheduler
            .set_max_concurrent_agents(max_concurrent_agents)?;
        Ok(())
    }

    /// Applies the configured queued-turn count and estimated-byte budgets.
    pub(crate) fn configure_agent_scheduler_queue_limits(
        &mut self,
        max_queued_turns: usize,
        max_queued_bytes: usize,
    ) -> Result<()> {
        self.agent
            .agent_scheduler
            .set_queue_limits(max_queued_turns, max_queued_bytes)?;
        Ok(())
    }

    /// Applies provider retry policy to subsequent failure observations.
    ///
    /// Active retry generations and already-scheduled timer delays remain
    /// intact; the replacement policy takes effect at the next failure.
    pub(crate) fn configure_provider_retry_policy(&mut self, policy: ProviderRetryPolicy) {
        self.agent.provider_retry_scheduler.set_policy(policy);
    }

    /// Returns the effective provider policy for auxiliary failure observations.
    pub(crate) fn provider_retry_policy(&self) -> ProviderRetryPolicy {
        self.agent.provider_retry_scheduler.policy()
    }

    /// Observes one exact retry attempt and its immutable chosen backoff.
    /// The adapter may retain deadline custody, but cannot advance this source
    /// until the ordinary matching timer event actually reaches the reducer.
    pub(crate) fn pending_agent_provider_retry_timer(&self, turn_id: &str) -> Option<(u64, u64)> {
        self.agent.provider_retry_scheduler.pending_timer(turn_id)
    }

    /// Conserves canonical calls and inline reservations after exact claim
    /// validation. Duplicate or stale worker results must never reach this
    /// method; the existing actor ingress owns that generation fence.
    pub(crate) fn record_agent_provider_invocation(
        &mut self,
        facts: &mez_agent::recovery_attribution::RecoveryFacts,
    ) {
        self.agent.provider_retry_scheduler.record_invocation(
            &facts.turn_id,
            facts.provider_calls_started,
            facts.incomplete_retries_reserved,
        );
    }

    /// Enqueues one validated unit of agent work.
    ///
    /// One accepted inbound prompt turn is exactly one enqueue, so this is also
    /// where the generated-title cadence counts a prompt turn. The tick only
    /// re-arms a due title window: it never queues, fails, or delays the work, and
    /// it runs after the scheduler accepted the work so a rejected enqueue is not
    /// counted as a prompt turn.
    pub(crate) fn enqueue_agent_work(&mut self, work: ScheduledWork) -> Result<()> {
        self.ensure_completion_admission_open()?;
        let conversation_id = work.conversation_id.clone();
        let paused_work = work.clone();
        self.agent.agent_scheduler.enqueue(work)?;
        self.adopt_human_paused_work(&paused_work);
        self.agent
            .session_title_tasks
            .note_prompt_turn(&conversation_id);
        Ok(())
    }

    /// Cancels queued, running, or blocked scheduler work when it exists.
    pub(crate) fn cancel_agent_work(&mut self, turn_id: &str) -> bool {
        self.agent.agent_scheduler.cancel(turn_id).is_ok()
    }

    /// Restores empty work and provider-retry schedulers with default policy.
    pub(crate) fn reset_agent_scheduler(&mut self) {
        self.agent.agent_scheduler = AgentScheduler::with_default_limit();
        self.agent.provider_retry_scheduler = ProviderRetryScheduler::default();
    }
}
