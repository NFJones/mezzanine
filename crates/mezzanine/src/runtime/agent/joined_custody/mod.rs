//! Original spawn source retained independently of the parent's latest response.
//!
//! A joined task may outlive a provider response. Its original action, request,
//! result slot and execution group therefore need immutable custody before the
//! scheduler releases the parent. Children from one response share one snapshot.
//! Records are bounded by the existing dependency table and convey no provider,
//! scheduler, permission, child-completion or continuation authority. This module
//! deliberately does not admit dialogue or replace a live execution slot.

use super::{
    AgentAction, AgentTurnExecution, AgentTurnRecord, JoinedSubagentDependency, MezError, Result,
    RuntimeSessionService,
};
use mez_agent::ContextExecutionGroupId;
use std::sync::Arc;

mod results;
pub(crate) use results::PreparedJoinedTaskResult;

/// One immutable provider response whose spawn effects have been accepted.
struct JoinedExecutionSource {
    /// Original parent identity; ledger state may subsequently park or resume.
    turn: AgentTurnRecord,
    /// Original provider execution identity, never sampled from a later response.
    group: ContextExecutionGroupId,
    /// Source action batch and original result slots, shared by sibling joins.
    execution: AgentTurnExecution,
}

impl std::fmt::Debug for JoinedExecutionSource {
    /// Formats owner identity without copying private request/response payloads.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JoinedExecutionSource")
            .field("turn_id", &self.turn.turn_id)
            .field("group", &self.group)
            .finish_non_exhaustive()
    }
}

/// Exact child attempt pointing at its shared original parent execution source.
#[derive(Debug)]
pub(super) struct JoinedActionCustody {
    /// Existing controller dependency, updated only by its explicit redirection.
    dependency: JoinedSubagentDependency,
    /// Immutable original source, rather than the replaceable latest execution.
    source: Arc<JoinedExecutionSource>,
    /// First locally received result value, not a context or wake acknowledgement.
    prepared_result: Option<Arc<PreparedJoinedTaskResult>>,
}

impl JoinedActionCustody {
    /// Returns the parent owner for terminal turn cleanup without decoding source.
    pub(super) fn parent_turn_id(&self) -> &str {
        &self.source.turn.turn_id
    }
}

impl RuntimeSessionService {
    /// Retains matching live joins before parking a provider response. All
    /// validation precedes insertion; exact repeat capture preserves its first
    /// immutable snapshot, while changed action/group/binding fails closed.
    /// Empty or unrelated responses retain nothing and grant no admission.
    pub(crate) fn retain_joined_parent_execution(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return Ok(());
        };
        let dependencies = self
            .agent
            .joined_subagent_dependencies
            .values()
            .filter(|dependency| {
                dependency.parent_turn_id == turn.turn_id
                    && batch.actions.iter().any(|action| {
                        action.id == dependency.parent_action_id
                            && action.action_type() == "spawn_agent"
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        if dependencies.is_empty() {
            return Ok(());
        }
        if execution.request.turn_id != turn.turn_id || execution.request.agent_id != turn.agent_id
        {
            return Err(MezError::conflict(
                "joined parent execution source identity changed",
            ));
        }
        let group = super::provider_execution::provider_log_execution_group_id(execution)?;
        for dependency in &dependencies {
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == dependency.parent_action_id)
                .ok_or_else(|| {
                    MezError::invalid_state("joined original spawn action is missing")
                })?;
            if action.action_type() != "spawn_agent"
                || !execution.action_results.iter().any(|result| {
                    result.action_id == action.id
                        && result.action_type == "spawn_agent"
                        && result.turn_id == turn.turn_id
                        && result.agent_id == turn.agent_id
                })
            {
                return Err(MezError::conflict(
                    "joined original spawn result identity changed",
                ));
            }
            if let Some(existing) = self
                .agent
                .joined_parent_executions
                .get(&dependency.child_turn_id)
                && (existing.dependency != *dependency
                    || existing.source.group != group
                    || existing.source.turn.conversation_id != turn.conversation_id
                    || existing.source.turn.pane_id != turn.pane_id
                    || existing.source.turn.agent_id != turn.agent_id
                    || existing.source.turn.started_at_unix_seconds != turn.started_at_unix_seconds
                    || existing
                        .source
                        .execution
                        .response
                        .action_batch
                        .as_ref()
                        .and_then(|batch| {
                            batch
                                .actions
                                .iter()
                                .find(|candidate| candidate.id == action.id)
                        })
                        != Some(action))
            {
                return Err(MezError::conflict(
                    "joined original execution custody cannot be replaced",
                ));
            }
        }
        self.agent_turn_contexts_mut()
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("joined parent context is missing"))?
            .retain_pending_execution_group(&group)
            .map_err(|error| MezError::invalid_state(error.to_string()))?;
        if dependencies.iter().all(|dependency| {
            self.agent
                .joined_parent_executions
                .contains_key(&dependency.child_turn_id)
        }) {
            return Ok(());
        }
        let source = Arc::new(JoinedExecutionSource {
            turn: turn.clone(),
            group,
            execution: execution.clone(),
        });
        for dependency in dependencies {
            self.agent
                .joined_parent_executions
                .entry(dependency.child_turn_id.clone())
                .or_insert_with(|| JoinedActionCustody {
                    dependency,
                    source: Arc::clone(&source),
                    prepared_result: None,
                });
        }
        self.prune_joined_parent_execution_custody();
        Ok(())
    }

    /// Reads the original spawn action without consulting the latest execution.
    /// A mismatched dependency, conversation or parent incarnation is a conflict.
    /// No retained source returns None: reconstructed existing dependencies still
    /// use their ordinary checked live-execution path until source is captured.
    pub(crate) fn retained_joined_spawn_action(
        &self,
        dependency: &JoinedSubagentDependency,
    ) -> Result<Option<AgentAction>> {
        let Some(owner) = self
            .agent
            .joined_parent_executions
            .get(&dependency.child_turn_id)
        else {
            return Ok(None);
        };
        let parent = self
            .agent_turn_ledger()
            .turn(&dependency.parent_turn_id)
            .ok_or_else(|| MezError::conflict("joined original parent turn is unavailable"))?;
        if owner.dependency != *dependency
            || self
                .agent
                .joined_subagent_dependencies
                .get(&dependency.child_turn_id)
                != Some(dependency)
            || owner.source.turn.turn_id != parent.turn_id
            || owner.source.turn.agent_id != parent.agent_id
            || owner.source.turn.pane_id != parent.pane_id
            || owner.source.turn.conversation_id != parent.conversation_id
            || owner.source.turn.started_at_unix_seconds != parent.started_at_unix_seconds
            || self
                .agent_shell_store()
                .get(&parent.pane_id)
                .is_none_or(|session| session.session_id != parent.conversation_id)
        {
            return Err(MezError::conflict("joined original parent binding changed"));
        }
        owner
            .source
            .execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| {
                batch
                    .actions
                    .iter()
                    .find(|action| action.id == dependency.parent_action_id)
            })
            .cloned()
            .map(Some)
            .ok_or_else(|| MezError::invalid_state("joined retained spawn action is missing"))
    }

    /// Drops source only after its existing exact dependency no longer owns it.
    pub(super) fn prune_joined_parent_execution_custody(&mut self) {
        let retired = self
            .agent
            .joined_parent_executions
            .iter()
            .filter(|(child, owner)| {
                self.agent.joined_subagent_dependencies.get(*child) != Some(&owner.dependency)
            })
            .map(|(_, owner)| {
                (
                    owner.source.turn.turn_id.clone(),
                    owner.source.group.clone(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>();
        self.agent.joined_parent_executions.retain(|child, owner| {
            self.agent.joined_subagent_dependencies.get(child) == Some(&owner.dependency)
        });
        for (parent, group) in retired {
            if !self
                .agent
                .joined_parent_executions
                .values()
                .any(|owner| owner.source.turn.turn_id == parent && owner.source.group == group)
                && let Some(context) = self.agent_turn_contexts_mut().get_mut(&parent)
            {
                context.release_pending_execution_group(&group);
            }
        }
    }

    /// Retires a parent's source and pending-context protection at its existing
    /// terminal ownership cleanup. Sibling joins of another parent are untouched.
    pub(super) fn release_joined_parent_execution_custody_for_turn(&mut self, turn_id: &str) {
        let groups = self
            .agent
            .joined_parent_executions
            .values()
            .filter(|owner| owner.parent_turn_id() == turn_id)
            .map(|owner| owner.source.group.clone())
            .collect::<std::collections::BTreeSet<_>>();
        self.agent
            .joined_parent_executions
            .retain(|_, owner| owner.parent_turn_id() != turn_id);
        if let Some(context) = self.agent_turn_contexts_mut().get_mut(turn_id) {
            for group in groups {
                context.release_pending_execution_group(&group);
            }
        }
    }

    /// Transfers custody only within the existing validated child redirection.
    /// Original parent source remains immutable; old child attempts lose the key.
    pub(super) fn transfer_joined_parent_execution_custody(&mut self, old: &str, new: &str) {
        if let Some(mut owner) = self.agent.joined_parent_executions.remove(old) {
            owner.dependency.child_turn_id = new.to_string();
            self.agent
                .joined_parent_executions
                .insert(new.to_string(), owner);
        }
    }
}
