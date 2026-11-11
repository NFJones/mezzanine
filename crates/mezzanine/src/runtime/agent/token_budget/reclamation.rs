//! Actor metadata reclamation only. Durable membership, usage and replay fences
//! are never pruned here. Releasing an actor descriptor does not attest worker
//! retirement: every issued worker/preparation retains its own original owner.

use super::*;
use std::collections::BTreeSet;

impl RuntimeSessionService {
    /// Reserve finite capture slots, sweeping only when pressure requires it.
    /// Current sessions and saved loop sources remain checkpointable. An
    /// unresolved proposal or a preparation/rollback-owned identity cannot be
    /// reclaimed, even if its pane has changed. There is no filesystem I/O,
    /// mutation replay, eviction of durable obligations, or age-based authority.
    pub(super) fn reserve_runtime_budget_capture_capacity(
        &mut self,
        additional: usize,
    ) -> Result<()> {
        let fits = |len: usize| additional <= RUNTIME_BUDGET_MEMBERSHIP_LIMIT.saturating_sub(len);
        if fits(self.agent.budget_memberships.entries.len()) {
            return Ok(());
        }
        let mut protected = BTreeSet::new();
        for session in self.agent_shell_store().sessions() {
            protected.insert(session.session_id.clone());
            if let Some(source) = &session.ephemeral_transcript_source_conversation_id {
                protected.insert(source.clone());
            }
        }
        for state in self.agent.agent_loops_by_id.values() {
            protected.insert(state.parent_conversation_id.clone());
            if let Some(work) = &state.work_conversation_id {
                protected.insert(work.clone());
            }
            if let Some(source) = state
                .parent_no_store_source
                .as_ref()
                .and_then(|source| source.conversation_id.as_ref())
            {
                protected.insert(source.clone());
            }
        }
        // At most the fixed capture bound is visited. A dispatched provider
        // holds its descriptor independently, so dropping detached actor
        // metadata cannot make its still-live inode available to a successor.
        self.agent
            .budget_memberships
            .entries
            .retain(|conversation, entry| {
                protected.contains(conversation)
                    || entry.member.is_none()
                    || Arc::strong_count(&entry.identity) != 1
                    || Arc::strong_count(&entry.preparation) != 1
            });
        if fits(self.agent.budget_memberships.entries.len()) {
            Ok(())
        } else {
            Err(MezError::invalid_state(
                "runtime budget membership capacity reached",
            ))
        }
    }
}
