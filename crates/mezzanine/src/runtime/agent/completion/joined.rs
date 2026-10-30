//! Original task execution custody across accepted parent coordination batches.
//!
//! Only quiescent groups whose remaining work consists of exact joined tasks
//! transfer here. Native/external callbacks and required publication finish
//! first. New responses keep the ordinary active slot; terminal child results
//! address this retained original source directly, without swapping global
//! execution state. Original archive targets survive until checked handoff.

use super::retirement::{CompletionTranscriptHandoff, CompletionTranscriptTarget};
use super::*;

/// One pending original group, bounded by its actual joined dependencies.
pub(super) struct RetainedJoinedExecution {
    /// Mutable original result slots, never a later response's action batch.
    execution: AgentTurnExecution,
    /// Existing original-target group, if physical action completion created it.
    group: Option<Box<CompletionGroup>>,
    /// Completed effect receipts travel with their exact group obligations.
    external: BTreeMap<ExternalCompletionKey, Box<ExternalCompletionReceipt>>,
    /// Completed native receipts never remain in the active publication pump.
    native: BTreeMap<NativeCompletionKey, Box<NativeCompletionReceipt>>,
    /// Optional publication diagnostics stay attributed to this original source.
    publication_failure: Option<Box<MezError>>,
    /// Failed terminal cleanup retains original context behind the global fence.
    failure_context: Option<mez_agent::AgentContext>,
}

impl fmt::Debug for RetainedJoinedExecution {
    /// Shows identity/counts only, never task text, provider payloads or output.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetainedJoinedExecution")
            .field("turn_id", &self.execution.request.turn_id)
            .field("has_group", &self.group.is_some())
            .field("external", &self.external.len())
            .field("native", &self.native.len())
            .field("failed_cleanup", &self.failure_context.is_some())
            .finish()
    }
}

impl RuntimeSessionService {
    /// Checks the actual live source before any custody transfer or new claim.
    /// Running joins are pending tasks; every other action must be successful
    /// and terminal, with no physical dispatch, required gate or paint left live.
    pub(in crate::runtime::agent) fn joined_coordination_source_quiescent(
        &self,
        turn_id: &str,
    ) -> bool {
        let Some(execution) = self.agent_turn_executions().get(turn_id) else {
            return false;
        };
        self.ensure_completion_required_gates_ready(turn_id).is_ok()
            && self.completion_group_publication_ready(turn_id)
            && !self.execution_has_pending_shell_dispatch(turn_id, execution)
            && !self
                .agent
                .pending_native_shell_dispatches
                .keys()
                .any(|(owner, _)| owner == turn_id)
            && !self
                .agent
                .claimed_native_shell_dispatches
                .keys()
                .any(|(owner, _)| owner == turn_id)
            && execution.action_results.iter().all(|result| {
                if self.agent_action_has_native_shell_owner(turn_id, &result.action_id)
                    || self.agent_action_has_running_shell_transaction(turn_id, &result.action_id)
                    || self.agent_action_has_pending_pre_shell_hook(turn_id, &result.action_id)
                    || self.agent.pending_apply_patch_phases.contains_key(
                        &Self::apply_patch_batch_state_key(turn_id, &result.action_id),
                    )
                    || self.agent.apply_patch_batch_states.contains_key(
                        &Self::apply_patch_batch_state_key(turn_id, &result.action_id),
                    )
                {
                    return false;
                }
                !result.is_error
                    && (result.status == ActionStatus::Succeeded
                        || (result.status == ActionStatus::Running
                            && result.action_type == "spawn_agent"
                            && self.agent.joined_subagent_dependencies.values().any(
                                |dependency| {
                                    dependency.parent_turn_id == turn_id
                                        && dependency.parent_action_id == result.action_id
                                        && self
                                            .retained_joined_spawn_action(dependency)
                                            .is_ok_and(|action| action.is_some())
                                },
                            )))
            })
    }

    /// Exposes read-only source gates for real-dispatch regression diagnostics.
    /// No field acknowledges a phase or creates admission authority.
    #[cfg(test)]
    pub(crate) fn joined_source_gates_for_tests(&self, turn_id: &str) -> String {
        format!(
            "quiescent={} required={:?} publication={} native={:?} external={:?}",
            self.joined_coordination_source_quiescent(turn_id),
            self.ensure_completion_required_gates_ready(turn_id),
            self.completion_group_publication_ready(turn_id),
            self.agent.completion_settlements.native,
            self.agent.completion_settlements.external
        )
    }

    /// Observes retained original slots and whether failed cleanup owns context.
    /// This read-only test projection cannot acknowledge custody or publication.
    #[cfg(test)]
    pub(crate) fn retained_joined_sources_for_tests(
        &self,
        turn_id: &str,
    ) -> Vec<(AgentTurnExecution, bool)> {
        self.agent
            .completion_settlements
            .joined_sources
            .iter()
            .filter(|((owner, _), _)| owner == turn_id)
            .map(|(_, source)| (source.execution.clone(), source.failure_context.is_some()))
            .collect()
    }

    /// Moves original execution/group and settled receipts at fresh response
    /// admission. Validation precedes every mutation; no effect is replayed.
    pub(in crate::runtime::agent) fn suspend_joined_coordination_source(
        &mut self,
        turn_id: &str,
    ) -> Result<()> {
        let Some(execution) = self.agent_turn_executions().get(turn_id) else {
            return Ok(());
        };
        if !self.execution_waiting_for_live_joined_subagents(turn_id, execution) {
            return Ok(());
        }
        if !self.joined_coordination_source_quiescent(turn_id) {
            return Err(MezError::conflict("joined source still owns non-task work"));
        }
        let group_id =
            super::super::provider_execution::provider_log_execution_group_id(execution)?;
        let key = (turn_id.to_string(), group_id.clone());
        if self
            .agent
            .completion_settlements
            .joined_sources
            .contains_key(&key)
        {
            return Err(MezError::conflict(
                "joined original group is already retained",
            ));
        }
        if self
            .agent
            .completion_settlements
            .groups
            .get(turn_id)
            .is_some_and(|group| {
                group.group != group_id
                    || group.primary_failure.is_some()
                    || group.paint_revoked
                    || group.scope.is_some()
                    || group.failed_backing.is_some()
                    || group.provider_handoff.is_some()
            })
        {
            return Err(MezError::conflict(
                "joined completion group is not transferable",
            ));
        }
        let external_keys = self
            .agent
            .completion_settlements
            .external
            .iter()
            .filter(|(_, receipt)| {
                receipt.owner.turn_id == turn_id && receipt.owner.group == group_id
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let native_keys = self
            .agent
            .completion_settlements
            .native
            .iter()
            .filter(|(_, receipt)| {
                receipt.owner.turn_id == turn_id && receipt.owner.group == group_id
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let execution = self
            .agent_turn_executions_mut()
            .remove(turn_id)
            .ok_or_else(|| MezError::invalid_state("joined source disappeared before transfer"))?;
        let group = self.agent.completion_settlements.groups.remove(turn_id);
        let mut external = BTreeMap::new();
        for key in external_keys {
            if let Some(receipt) = self.agent.completion_settlements.external.remove(&key) {
                external.insert(key, receipt);
            }
        }
        let mut native = BTreeMap::new();
        for key in native_keys {
            if let Some(receipt) = self.agent.completion_settlements.native.remove(&key) {
                native.insert(key, receipt);
            }
        }
        self.agent.completion_settlements.joined_sources.insert(
            key,
            Box::new(RetainedJoinedExecution {
                execution,
                group,
                external,
                native,
                publication_failure: None,
                failure_context: None,
            }),
        );
        Ok(())
    }

    /// Reports actual independently retained task custody, not a caller label.
    pub(in crate::runtime::agent) fn has_retained_joined_source(&self, turn_id: &str) -> bool {
        self.agent
            .completion_settlements
            .joined_sources
            .keys()
            .any(|(owner, _)| owner == turn_id)
    }

    /// Enumerates current and retained mutable source for controller recovery.
    /// Immutable capture snapshots are deliberately excluded: their old Running
    /// slots cannot resurrect an already settled task or replay its result.
    pub(in crate::runtime::agent) fn joined_execution_sources_for_parent(
        &self,
        turn_id: &str,
    ) -> Vec<&AgentTurnExecution> {
        self.agent_turn_executions()
            .get(turn_id)
            .into_iter()
            .chain(
                self.agent
                    .completion_settlements
                    .joined_sources
                    .iter()
                    .filter(|((owner, _), _)| owner == turn_id)
                    .map(|(_, source)| &source.execution),
            )
            .collect()
    }

    /// Attributes optional original-result publication failure to its retained
    /// group rather than the unrelated latest coordination response.
    pub(in crate::runtime::agent) fn note_retained_joined_publication(
        &mut self,
        turn_id: &str,
        execution: &AgentTurnExecution,
        publication: Result<()>,
    ) {
        if let Err(error) = publication
            && let Ok(group) =
                super::super::provider_execution::provider_log_execution_group_id(execution)
            && let Some(source) = self
                .agent
                .completion_settlements
                .joined_sources
                .get_mut(&(turn_id.to_string(), group))
        {
            source
                .publication_failure
                .get_or_insert_with(|| Box::new(error));
        }
    }

    /// Applies only an exact prepared original result to retained source.
    /// Returns None for the ordinary current-slot path; preparation is not an
    /// archive/context/wake acknowledgement and never selects a foreign batch.
    pub(in crate::runtime::agent) fn apply_retained_joined_result(
        &mut self,
        dependency: &JoinedSubagentDependency,
        prepared: &crate::runtime::agent::joined_custody::PreparedJoinedTaskResult,
    ) -> Result<Option<AgentTurnExecution>> {
        let Some(group) = self.joined_original_group(dependency)? else {
            return Ok(None);
        };
        let Some(source) = self
            .agent
            .completion_settlements
            .joined_sources
            .get_mut(&(dependency.parent_turn_id.clone(), group))
        else {
            return Ok(None);
        };
        let index = prepared.matching_result_slot(&source.execution)?;
        source.execution.action_results[index] = prepared.result().clone();
        source.execution.final_turn = false;
        source.execution.terminal_state =
            runtime_agent_turn_state_from_action_results(&source.execution.action_results, false);
        if let Some(group) = source.group.as_mut() {
            group.source_changed();
        }
        Ok(Some(source.execution.clone()))
    }

    /// Hands a complete retained source to its original checked archive owner.
    /// The source remains installed until candidate ownership is accepted; this
    /// never records parent task completion or ordinary provider continuation.
    fn transfer_retained_joined_archive(
        &mut self,
        key: &(String, mez_agent::ContextExecutionGroupId),
    ) -> Result<()> {
        let source = self
            .agent
            .completion_settlements
            .joined_sources
            .get(key)
            .ok_or_else(|| MezError::invalid_state("retained joined archive source is absent"))?;
        let Some(group) = source.group.as_ref() else {
            return Ok(());
        };
        let CompletionTranscriptTarget::Original { read, directory } = &group.transcript else {
            return Ok(());
        };
        if group.handoff.is_none() {
            let mut candidate = self.completion_bookkeeping_candidate(
                &group.turn,
                &source.execution,
                read.as_ref().clone(),
                directory.clone(),
            )?;
            let evidence = Self::completion_group_settlement_evidence(group);
            Self::append_completion_settlement_evidence(
                &mut candidate,
                &group.group,
                evidence.clone(),
            )?;
            if let Some(group) = self
                .agent
                .completion_settlements
                .joined_sources
                .get_mut(key)
                .and_then(|source| source.group.as_mut())
            {
                group.handoff = Some(Box::new(CompletionTranscriptHandoff {
                    candidate,
                    witness: None,
                    evidence,
                }));
            }
        }
        let handoff = self
            .agent
            .completion_settlements
            .joined_sources
            .get_mut(key)
            .and_then(|source| source.group.as_mut())
            .and_then(|group| group.handoff.as_mut())
            .ok_or_else(|| {
                MezError::invalid_state("retained joined archive candidate is absent")
            })?;
        if handoff.witness.is_some() {
            return Ok(());
        }
        let accepted = self
            .persistence
            .retain_completion_bookkeeping_candidate(handoff.candidate.clone())?;
        handoff.witness = Some(accepted.witness().clone());
        if !self.persistence.transcript_uses_adapter() {
            self.complete_completion_candidate_inline(accepted.witness().generation())?;
        }
        Ok(())
    }

    /// Retires settled original groups only after their final dependency and
    /// exact context result have transferred. Other parents/groups stay owned.
    pub(in crate::runtime::agent) fn retire_settled_joined_sources(
        &mut self,
        turn_id: &str,
    ) -> Result<()> {
        let keys = self
            .agent
            .completion_settlements
            .joined_sources
            .iter()
            .filter(|((owner, _), source)| {
                owner == turn_id
                    && source
                        .execution
                        .action_results
                        .iter()
                        .all(ActionResult::is_terminal)
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            if let Err(error) = self.transfer_retained_joined_archive(&key) {
                self.record_completion_archive_failure(&error);
                return Err(error);
            }
            self.agent
                .completion_settlements
                .joined_sources
                .remove(&key);
        }
        Ok(())
    }

    /// Transfers source whose exact task obligations were explicitly retired.
    /// A failed transfer stays bounded behind the same critical admission fence.
    pub(in crate::runtime::agent) fn retire_unowned_joined_sources(&mut self) {
        let keys = self
            .agent
            .completion_settlements
            .joined_sources
            .iter()
            .filter(|((turn, _), source)| {
                !self
                    .agent
                    .joined_subagent_dependencies
                    .values()
                    .any(|dependency| {
                        &dependency.parent_turn_id == turn
                            && source
                                .execution
                                .action_results
                                .iter()
                                .any(|result| result.action_id == dependency.parent_action_id)
                    })
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            match self.transfer_retained_joined_archive(&key) {
                Ok(()) => {
                    self.agent
                        .completion_settlements
                        .joined_sources
                        .remove(&key);
                }
                Err(error) => {
                    let context = self.agent_turn_contexts().get(&key.0).cloned();
                    if let Some(source) = self
                        .agent
                        .completion_settlements
                        .joined_sources
                        .get_mut(&key)
                        && source.failure_context.is_none()
                    {
                        source.failure_context = context;
                    }
                    self.record_completion_archive_failure(&error);
                }
            }
        }
    }

    /// Transfers every retained original source before terminal context cleanup.
    /// Failure keeps source/context behind the existing critical admission fence;
    /// it never samples a replacement pane or acknowledges unfinished child work.
    pub(in crate::runtime::agent) fn retain_joined_sources_before_cleanup(
        &mut self,
        turn_id: &str,
    ) -> Result<()> {
        let keys = self
            .agent
            .completion_settlements
            .joined_sources
            .keys()
            .filter(|(owner, _)| owner == turn_id)
            .cloned()
            .collect::<Vec<_>>();
        let mut first = None;
        for key in keys {
            match self.transfer_retained_joined_archive(&key) {
                Ok(()) => {
                    self.agent
                        .completion_settlements
                        .joined_sources
                        .remove(&key);
                }
                Err(error) => {
                    let context = self.agent_turn_contexts().get(turn_id).cloned();
                    if let Some(source) = self
                        .agent
                        .completion_settlements
                        .joined_sources
                        .get_mut(&key)
                        && source.failure_context.is_none()
                    {
                        source.failure_context = context;
                    }
                    self.record_completion_archive_failure(&error);
                    if first.is_none() {
                        first = Some(error);
                    }
                }
            }
        }
        first.map_or(Ok(()), Err)
    }
}
