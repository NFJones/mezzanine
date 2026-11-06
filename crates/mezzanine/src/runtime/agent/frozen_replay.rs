//! Private NoStore selections retained by actual pane/conversation ownership.
//! These are immutable model views, not a second mutable history or journal.

use super::*;

impl RuntimeSessionService {
    /// Restores only the original parent's persistence policy and immutable
    /// selection after the controller has rebound its exact conversation.
    /// This is continuation of the same NoStore parent, not another source copy:
    /// fresh retrievals must not be revoked a second time on restoration.
    pub(crate) fn restore_agent_loop_parent_replay_binding(
        &mut self,
        pane: &str,
        state: &RuntimeAgentLoopState,
    ) -> Result<()> {
        let frozen = state
            .parent_frozen_replay
            .as_ref()
            .filter(|_| state.parent_no_store_source.is_some())
            .cloned();
        if frozen
            .as_ref()
            .is_some_and(|source| source.conversation().as_str() != state.parent_conversation_id)
        {
            return Err(MezError::conflict(
                "loop parent replay selection changed conversation",
            ));
        }
        let mut current = self
            .agent_shell_store()
            .get(pane)
            .filter(|session| session.session_id == state.parent_conversation_id)
            .cloned()
            .ok_or_else(|| MezError::conflict("loop parent replay binding is unavailable"))?;
        if let Some(source) = &state.parent_no_store_source {
            // Clone the current state, not an old shell snapshot: only these
            // persistence fields change; current task/control settings survive.
            current.ephemeral = true;
            current.ephemeral_transcript_source_conversation_id = source.conversation_id.clone();
            current.ephemeral_transcript_source_entries = source.entries;
            self.agent_shell_store_mut()
                .restore_session(pane, current)?;
        }
        self.set_frozen_replay_binding(pane, frozen);
        Ok(())
    }

    /// Retains the latest exact NoStore body before terminal cleanup. A pane
    /// rebound to another conversation cannot acquire the old turn's source.
    pub(in crate::runtime::agent) fn retain_ephemeral_replay_before_cleanup(
        &mut self,
        turn_id: &str,
    ) -> Result<()> {
        let Some(turn) = self.agent_turn_ledger().turn(turn_id).cloned() else {
            return Ok(());
        };
        if self
            .agent_shell_store()
            .get(&turn.pane_id)
            .is_none_or(|session| !session.ephemeral || session.session_id != turn.conversation_id)
        {
            return Ok(());
        }
        let Some(body) = self
            .completion_replay_context_for_retirement(turn_id)
            .cloned()
        else {
            return Ok(());
        };
        self.retain_ephemeral_replay_body(
            &turn.pane_id,
            &turn.conversation_id,
            &body,
            self.agent_turn_history_source(turn_id).cloned(),
        )
    }

    pub(crate) fn frozen_replay_binding(
        &self,
        pane: &str,
        conversation: &str,
    ) -> Option<&crate::runtime::control::RuntimeFrozenReplayHistory> {
        self.agent
            .frozen_replay_bindings
            .get(pane)
            .filter(|source| source.conversation().as_str() == conversation)
    }

    /// Controller bindings and rollback retain their original immutable value.
    /// A mismatched CID is never returned to prompt composition by the getter.
    pub(crate) fn set_frozen_replay_binding(
        &mut self,
        pane: &str,
        source: Option<crate::runtime::control::RuntimeFrozenReplayHistory>,
    ) {
        if let Some(source) = source {
            self.agent
                .frozen_replay_bindings
                .insert(pane.to_owned(), source);
        } else {
            self.agent.frozen_replay_bindings.remove(pane);
        }
    }
}
