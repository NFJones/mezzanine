//! Publication custody after a response or turn releases execution ownership.
//!
//! Snapshots carry only existing accepted response/ordinal source and canonical
//! eligibility. They never run workers, replay required gates, or restore a
//! provider claim. Failed installation retains the exact conversation-fenced
//! owner; the existing bounded completion pump retries it after terminal cleanup.

use super::*;

/// Exact pane, turn, conversation and accepted response publication owner.
type ProviderLogOwner = (String, String, String, mez_agent::ContextExecutionGroupId);

impl RuntimeSessionService {
    /// Retains a terminal execution's publication before live cleanup removes it.
    /// Missing accepted source is a no-op. Display refusal remains optional and
    /// keeps its snapshot; exact conversation replacement intentionally retires it.
    pub(in crate::runtime::agent) fn retain_terminal_provider_logs_for_turn(
        &mut self,
        turn_id: &str,
        state: AgentTurnState,
    ) -> Result<()> {
        let Some(mut execution) = self.agent_turn_executions().get(turn_id).cloned() else {
            return Ok(());
        };
        execution.terminal_state = state;
        let pane = self
            .agent_turn_ledger()
            .turn(turn_id)
            .map(|turn| turn.pane_id.clone());
        let Some(pane) = pane else {
            return Ok(());
        };
        self.retain_provider_log_execution(&pane, &execution)?;
        Ok(())
    }

    /// Transfers an exact accepted response snapshot to publication-only custody.
    /// The original order owner supplies conversation binding; a rebound pane
    /// cannot relabel old accepted components with its new conversation.
    pub(in crate::runtime::agent) fn release_provider_log_execution(
        &mut self,
        pane: &str,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        if let Some(key) = self.retain_provider_log_execution(pane, execution)? {
            self.drain_provider_log_handoff(&key)?;
        }
        Ok(())
    }

    /// Captures source before finalization can clear transient/live owners.
    /// Installation is separate so terminal presentation retirement cannot
    /// restore an old suffix over a newly acknowledged component.
    fn retain_provider_log_execution(
        &mut self,
        pane: &str,
        execution: &AgentTurnExecution,
    ) -> Result<Option<ProviderLogOwner>> {
        let group = super::super::provider_execution::provider_log_execution_group_id(execution)?;
        let key = self
            .presentation
            .agent_provider_log_orders
            .keys()
            .find(|key| key.0 == pane && key.1 == execution.request.turn_id && key.3 == group)
            .cloned();
        let Some(key) = key else {
            return Ok(None);
        };
        if let Some(owner) = self.presentation.agent_provider_log_handoffs.get_mut(&key) {
            owner.execution = execution.clone();
        } else {
            self.presentation.agent_provider_log_handoffs.insert(
                key.clone(),
                crate::runtime::render::RuntimeProviderLogHandoff {
                    execution: execution.clone(),
                    failure: None,
                },
            );
        }
        Ok(Some(key))
    }

    /// Installs one retained response without changing execution or permission state.
    /// Successful parts keep their existing per-component acknowledgements; any
    /// error leaves the snapshot and immutable queued suffix available to retry.
    fn drain_provider_log_handoff(&mut self, key: &ProviderLogOwner) -> Result<()> {
        let result = self.drain_provider_log_handoff_once(key);
        if let Err(error) = &result
            && let Some(owner) = self.presentation.agent_provider_log_handoffs.get_mut(key)
            && owner.failure.is_none()
        {
            owner.failure = Some(Box::new(error.clone()));
        }
        result
    }

    /// Attempts one accepted-source snapshot without replacing its first error.
    fn drain_provider_log_handoff_once(&mut self, key: &ProviderLogOwner) -> Result<()> {
        if !self
            .agent_shell_store()
            .get(&key.0)
            .is_some_and(|session| session.session_id == key.2)
        {
            self.presentation.agent_provider_log_handoffs.remove(key);
            self.retire_provider_log_groups_matching(|p, t, c, g| {
                p == key.0 && t == key.1 && c == key.2 && g == key.3.as_str()
            });
            return Ok(());
        }
        let Some(execution) = self
            .presentation
            .agent_provider_log_handoffs
            .get(key)
            .map(|owner| owner.execution.clone())
        else {
            return Ok(());
        };
        // Only previously accepted components belong to this owner. Re-entering
        // the outcome producer would demand execution binding after cancellation
        // and could manufacture a new outcome rather than drain retained source.
        self.flush_ordered_provider_headers(&key.0, &execution)?;
        let queued = self
            .presentation
            .agent_queued_provider_headers
            .keys()
            .chain(self.presentation.agent_queued_provider_commands.keys())
            .chain(self.presentation.agent_queued_provider_results.keys())
            .chain(self.presentation.agent_queued_provider_outcomes.keys())
            .any(|(p, t, c, g, _)| p == &key.0 && t == &key.1 && c == &key.2 && g == &key.3);
        if execution.terminal_state != AgentTurnState::Running && queued {
            return Err(MezError::invalid_state(
                "terminal accepted log source remains held",
            ));
        }
        self.presentation.agent_provider_log_handoffs.remove(key);
        Ok(())
    }

    /// Retries a finite snapshot of accepted display owners, including terminal
    /// turns with no execution receipt. No executor or provider work is replayed.
    pub(in crate::runtime::agent) fn retry_provider_log_handoffs(&mut self) -> usize {
        self.retry_provider_log_handoffs_matching(None)
    }

    /// Completes one terminal cleanup's display after worker delivery retirement.
    /// This bounded final attempt cannot advance unrelated execution owners.
    pub(in crate::runtime::agent) fn retry_provider_log_handoffs_for_turn(
        &mut self,
        turn: &str,
    ) -> usize {
        self.retry_provider_log_handoffs_matching(Some(turn))
    }

    /// Snapshots only selected display keys; each successful key releases custody.
    fn retry_provider_log_handoffs_matching(&mut self, turn: Option<&str>) -> usize {
        let keys = self
            .presentation
            .agent_provider_log_handoffs
            .keys()
            .filter(|key| turn.is_none_or(|turn| key.1 == turn))
            .cloned()
            .collect::<Vec<_>>();
        let mut advanced = 0;
        for key in keys {
            if self.drain_provider_log_handoff(&key).is_ok() {
                advanced += 1;
                if self.agent_turn_ledger().turn(&key.1).is_none_or(|turn| {
                    matches!(
                        turn.state,
                        AgentTurnState::Completed
                            | AgentTurnState::Failed
                            | AgentTurnState::Interrupted
                    )
                }) {
                    self.clear_settled_provider_log_groups_for_turn(&key.1);
                }
            }
        }
        advanced
    }

    /// Retires only acknowledged owners; failed accepted publication retains all
    /// order and entry fences until its publication-only snapshot has drained.
    pub(crate) fn clear_settled_provider_log_groups_for_turn(&mut self, turn_id: &str) {
        let retained = self
            .presentation
            .agent_provider_log_handoffs
            .keys()
            .map(|(p, t, c, g)| (p.clone(), t.clone(), c.clone(), g.as_str().to_string()))
            .collect::<BTreeSet<_>>();
        self.retire_provider_log_groups_matching(|p, t, c, g| {
            t == turn_id
                && !retained.contains(&(p.to_string(), t.to_string(), c.to_string(), g.to_string()))
        });
    }

    /// Removes an explicitly acknowledged or conversation-retired group across
    /// every component map together, preserving unrelated live response owners.
    fn retire_provider_log_groups_matching(
        &mut self,
        retire: impl Fn(&str, &str, &str, &str) -> bool,
    ) {
        self.presentation
            .agent_settled_provider_log_groups
            .retain(|(p, t, c, g)| !retire(p, t, c, g.as_str()));
        self.presentation
            .agent_deferred_provider_progress
            .retain(|(p, t, c, g, _)| !retire(p, t, c, g.as_str()));
        self.presentation
            .agent_retired_provider_says
            .retain(|(p, t, c, g, _)| !retire(p, t, c, g.as_str()));
        self.presentation
            .agent_published_header_thinking
            .retain(|(p, t, c, g, _, _)| !retire(p, t, c, g.as_str()));
        self.presentation
            .agent_queued_provider_outcomes
            .retain(|(p, t, c, g, _), _| !retire(p, t, c, g.as_str()));
        self.presentation
            .agent_published_provider_outcomes
            .retain(|((p, t, c, g, _), _, _)| !retire(p, t, c, g.as_str()));
        self.presentation
            .agent_queued_provider_headers
            .retain(|(p, t, c, g, _), _| !retire(p, t, c, g.as_str()));
        self.presentation
            .agent_queued_provider_commands
            .retain(|(p, t, c, g, _), _| !retire(p, t, c, g.as_str()));
        self.presentation
            .agent_published_provider_commands
            .retain(|((p, t, c, g, _), _)| !retire(p, t, c, g.as_str()));
        self.presentation
            .agent_queued_provider_results
            .retain(|(p, t, c, g, _), _| !retire(p, t, c, g.as_str()));
        self.presentation
            .agent_published_provider_results
            .retain(|((p, t, c, g, _), _)| !retire(p, t, c, g.as_str()));
        self.presentation
            .agent_provider_log_orders
            .retain(|(p, t, c, g), _| !retire(p, t, c, g.as_str()));
        self.presentation
            .agent_published_provider_headers
            .retain(|(p, t, c, g, _)| !retire(p, t, c, g.as_str()));
    }
}
