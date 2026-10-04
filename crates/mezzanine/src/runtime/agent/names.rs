//! Conversation-owned primary display identity and shared name reservations.
//!
//! Allocation happens at activation, never render. Root identities remain
//! separate from delegation lineage, and hidden/suspended conversations retain
//! reservations. Persisted identity wins over current naming policy on resume.

use super::{Result, RuntimeSessionService};

impl RuntimeSessionService {
    /// Captures reservations for rollback of a failed compound activation.
    pub(crate) fn snapshot_primary_agent_names(
        &self,
    ) -> std::collections::BTreeMap<String, String> {
        self.agent.primary_agent_names.clone()
    }

    /// Restores exact reservations without allocating or renaming any conversation.
    pub(crate) fn restore_primary_agent_names(
        &mut self,
        names: std::collections::BTreeMap<String, String>,
    ) {
        self.agent.primary_agent_names = names;
    }

    /// Reserves restored identities without allocating or changing their spelling.
    pub(crate) fn reserve_primary_agent_name(&mut self, conversation: &str, name: &str) {
        self.agent
            .primary_agent_names
            .insert(conversation.to_string(), name.to_string());
    }

    /// Activates one primary identity without naming children or ephemeral work.
    pub(crate) fn ensure_primary_agent_name(&mut self, pane: &str) -> Result<()> {
        let Some(session) = self.agent_shell_store().get(pane).cloned() else {
            return Ok(());
        };
        if session.ephemeral
            || session.conversation_kind != mez_agent::AgentConversationKind::Root
            || self.subagent_lineage(&format!("agent-{pane}")).is_some()
        {
            return Ok(());
        }
        if let Some(name) = session.display_name.as_deref() {
            self.reserve_primary_agent_name(&session.session_id, name);
            return Ok(());
        }
        let store = self.persistence.cloned_transcript_store();
        let saved = store
            .as_ref()
            .map(|store| store.conversation_primary_display_name(&session.session_id))
            .transpose()?
            .flatten();
        let name = saved
            .or(session.display_name)
            .or_else(|| {
                self.agent
                    .primary_agent_names
                    .get(&session.session_id)
                    .cloned()
            })
            .unwrap_or_else(|| self.resolve_subagent_display_name(&format!("agent-{pane}")));
        self.agent_shell_store_mut()
            .ensure_session(pane)?
            .display_name = Some(name.clone());
        self.agent
            .primary_agent_names
            .insert(session.session_id, name);
        Ok(())
    }

    /// Restores an already-qualified name without storage or allocation.
    pub(crate) fn install_primary_agent_name(
        &mut self,
        pane: &str,
        name: Option<String>,
    ) -> Result<()> {
        let conversation = {
            let session = self.agent_shell_store_mut().ensure_session(pane)?;
            session.display_name = name.clone();
            session.session_id.clone()
        };
        if let Some(name) = name {
            self.agent.primary_agent_names.insert(conversation, name);
        }
        Ok(())
    }

    /// Returns inert primary presentation identity for a currently bound pane.
    pub(crate) fn primary_agent_display_name(&self, pane: &str) -> Option<&str> {
        self.agent_shell_store()
            .get(pane)
            .filter(|session| !session.ephemeral)
            .and_then(|session| session.display_name.as_deref())
    }
}
