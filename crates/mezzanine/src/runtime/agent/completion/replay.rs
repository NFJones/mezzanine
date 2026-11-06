//! Original canonical source fences for existing completion retirement owners.
//!
//! Fences are copied at actual append/admission boundaries, never refreshed by a
//! completion callback. Existing group identity and required publication gates
//! still authorize retirement. This metadata owns no body, effect queue or disk
//! acknowledgment. Missing, replaced or reopened source keeps its protection.

use super::*;

impl RuntimeSessionService {
    /// Exercises the actual retained completion owner after its real gates pass;
    /// this creates neither a source fence nor an admission/publication flag.
    #[cfg(test)]
    pub(crate) fn retry_completion_replay_retirement_for_tests(
        &mut self,
        turn_id: &str,
    ) -> Result<()> {
        self.ensure_completion_required_gates_ready(turn_id)?;
        if !self.completion_group_publication_ready(turn_id) {
            return Err(MezError::conflict("completion publication remains pending"));
        }
        self.finish_completion_replay_group(turn_id)
    }

    /// Qualifies all candidate append fences before adopting any live body or
    /// metadata. A multi-group result commit cannot leave a partially advanced
    /// retirement owner when another original source conflicts.
    pub(in crate::runtime::agent) fn ensure_replay_execution_append_source(
        &self,
        turn_id: &str,
        group_id: &mez_agent::ContextExecutionGroupId,
        before: Option<&mez_agent::ReplayExecutionOwner>,
    ) -> Result<()> {
        if self
            .agent
            .completion_settlements
            .groups
            .get(turn_id)
            .filter(|group| &group.group == group_id)
            .is_some_and(|group| group.replay_owner.as_ref() != before)
        {
            return Err(MezError::conflict(
                "completion replay append source changed",
            ));
        }
        if let Some(source) = self
            .agent
            .completion_settlements
            .joined_sources
            .get(&(turn_id.to_string(), group_id.clone()))
        {
            source.ensure_replay_append_source(before)?;
        }
        Ok(())
    }

    /// Observes the actual retained fence without installing a claim or phase.
    #[cfg(test)]
    pub(crate) fn completion_replay_owner_for_tests(
        &self,
        turn_id: &str,
    ) -> Option<&mez_agent::ReplayExecutionOwner> {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)?
            .replay_owner
            .as_ref()
    }

    /// Borrows the original selected backing; captured failed absence never
    /// falls back to a replacement live body. This cannot grant retirement.
    #[cfg(test)]
    pub(crate) fn completion_replay_context_for_tests(
        &self,
        turn_id: &str,
    ) -> Option<&mez_agent::AgentContext> {
        if let Some(backing) = self
            .agent
            .completion_settlements
            .groups
            .get(turn_id)
            .and_then(|group| group.failed_backing.as_ref())
        {
            return backing.context.as_ref();
        }
        self.agent_turn_contexts().get(turn_id)
    }

    /// Retains a fence only when this synchronous context mutation actually
    /// changed its producer-captured append receipt. Exact no-op result retries
    /// cannot adopt a newer unrelated frontier. Existing source ownership stays
    /// in the active or suspended original completion group.
    pub(in crate::runtime::agent) fn record_replay_execution_append(
        &mut self,
        turn_id: &str,
        group_id: &mez_agent::ContextExecutionGroupId,
        before: Option<mez_agent::ReplayExecutionOwner>,
        after: Option<mez_agent::ReplayExecutionOwner>,
    ) -> Result<()> {
        if before == after {
            return Ok(());
        }
        let Some(after) = after else {
            return Ok(());
        };
        self.ensure_replay_execution_append_source(turn_id, group_id, before.as_ref())?;
        if let Some(group) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(turn_id)
            .filter(|group| &group.group == group_id)
        {
            group.replay_owner = Some(after.clone());
        }
        if let Some(source) = self
            .agent
            .completion_settlements
            .joined_sources
            .get_mut(&(turn_id.to_string(), group_id.clone()))
        {
            source.record_replay_append(before.as_ref(), after)?;
        }
        Ok(())
    }

    /// Closes only the actual original group after its existing retirement
    /// gates pass. The retained source fence is never refreshed here. Failed
    /// backing is selected explicitly and cannot borrow a replacement live body.
    pub(super) fn finish_completion_replay_group(&mut self, turn_id: &str) -> Result<()> {
        let Some(group) = self.agent.completion_settlements.groups.get(turn_id) else {
            return Ok(());
        };
        let action_ids = group
            .actions
            .iter()
            .map(|(id, _, _)| id)
            .collect::<BTreeSet<_>>();
        if self
            .agent
            .joined_subagent_dependencies
            .values()
            .any(|dependency| {
                dependency.parent_turn_id == turn_id
                    && action_ids.contains(&dependency.parent_action_id)
            })
        {
            return Ok(());
        }
        let receipt = group.replay_owner.clone();
        let failed = group.failed_backing.is_some();
        let context = if failed {
            self.agent
                .completion_settlements
                .groups
                .get_mut(turn_id)
                .and_then(|group| group.failed_backing.as_mut())
                .and_then(|backing| match &mut backing.context {
                    retirement::CompletionCapture::Captured(context) => context.as_mut(),
                    retirement::CompletionCapture::NotCaptured => None,
                })
        } else {
            self.agent_turn_contexts_mut().get_mut(turn_id)
        };
        let Some(context) = context else {
            return if receipt.is_some() {
                Err(MezError::conflict(
                    "completion original replay context is unavailable",
                ))
            } else {
                Ok(())
            };
        };
        if !context.has_replay_owner() {
            return if receipt.is_some() {
                Err(MezError::conflict(
                    "completion original replay owner was replaced",
                ))
            } else {
                Ok(())
            };
        }
        let receipt = receipt
            .ok_or_else(|| MezError::conflict("completion original replay fence is unavailable"))?;
        context
            .update_replay_owner(|owner| {
                if owner.execution_group_finished(&receipt) {
                    Ok(())
                } else {
                    owner.finish_execution_group(&receipt)
                }
            })
            .map_err(|_| {
                MezError::conflict("completion original replay source changed before retirement")
            })
    }
}
