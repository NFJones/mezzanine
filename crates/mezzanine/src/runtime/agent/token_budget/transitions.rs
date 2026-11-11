//! Independent-account capture at authenticated human conversation transitions.
//! Generic historical binds and delegated context copies acquire no such right.
//! Publication still runs on the existing preparation worker before readiness.

use super::*;

impl RuntimeSessionService {
    /// Called only after the saved-fork claim/publication owner validates the
    /// attached primary and installs its exact fresh target. This is I/O-free:
    /// it does not copy source spend or claim a durable membership ACK. The
    /// enclosing fork transaction retains rollback/uncertain-teardown custody.
    pub(crate) fn capture_runtime_budget_human_fork(
        &mut self,
        primary: &mez_core::ids::ClientId,
        pane: &str,
        conversation: &str,
    ) -> Result<()> {
        if !self.agent.budget_memberships.enabled {
            return Ok(());
        }
        if !self.session.is_attached_primary(primary) {
            return Err(MezError::forbidden("budget fork requires attached primary"));
        }
        let session = self
            .agent_shell_store()
            .get(pane)
            .filter(|session| {
                session.session_id == conversation
                    && session.conversation_kind == AgentConversationKind::Root
                    && !session.ephemeral
                    && session.running_turn_id.is_none()
            })
            .cloned()
            .ok_or_else(|| MezError::conflict("budget fork target changed"))?;
        if self
            .agent
            .budget_memberships
            .entries
            .contains_key(conversation)
        {
            return Err(MezError::conflict("budget fork target already captured"));
        }
        let store = self
            .persistence
            .cloned_token_usage_store()
            .ok_or_else(|| MezError::invalid_state("budget fork accounting store unavailable"))?;
        let transcripts = self
            .persistence
            .cloned_transcript_store()
            .ok_or_else(|| MezError::invalid_state("budget fork transcript store unavailable"))?;
        self.reserve_runtime_budget_capture_capacity(1)?;
        let source = Source::independent(crate::runtime::current_unix_seconds(), false)?;
        self.agent.budget_memberships.entries.insert(
            conversation.to_owned(),
            Entry {
                identity: Arc::new(()),
                preparation: Arc::new(()),
                ready: false,
                store,
                transcripts,
                pane: pane.to_owned(),
                native: session.native_message_incarnation,
                owner: crate::storage::token_usage::new_token_usage_event_id(),
                source,
                member: None,
                lease: None,
            },
        );
        Ok(())
    }
}
