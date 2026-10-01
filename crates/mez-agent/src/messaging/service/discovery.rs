//! Requester-scoped discovery and presence projections of registered identities.
//!
//! Project visibility and explicit session widening use the same registration
//! authority as message acceptance; this module owns no separate registry.

use super::*;

impl MessageService {
    /// Returns administrative session-wide discovery in stable identity order.
    pub fn discover_agents(&self) -> Vec<SenderIdentity> {
        self.discover_agents_filtered_session_wide(None, None, None, None, None, &[])
    }

    /// Discovers the requester and peers admitted by the selected audience.
    #[allow(clippy::too_many_arguments)]
    pub fn discover_agents_filtered_for_requester(
        &self,
        requester: &AgentId,
        scope: MessageScope,
        agent_id: Option<&str>,
        pane_id: Option<&str>,
        window_id: Option<&str>,
        role: Option<&str>,
        status: Option<AgentPresenceStatus>,
        capabilities: &[String],
    ) -> Vec<SenderIdentity> {
        let requester_scope = self
            .registered
            .get(requester)
            .and_then(|identity| identity.project_scope.as_ref());
        self.discover_agents_filtered_session_wide(
            agent_id,
            pane_id,
            window_id,
            role,
            status,
            capabilities,
        )
        .into_iter()
        .filter(|identity| {
            identity.agent_id == *requester
                || matches!(scope, MessageScope::Session)
                || requester_scope
                    .is_some_and(|scope| identity.project_scope.as_ref() == Some(scope))
        })
        .collect()
    }

    /// Filters administrative discovery without implicitly changing audience.
    pub fn discover_agents_filtered_session_wide(
        &self,
        agent_id: Option<&str>,
        pane_id: Option<&str>,
        window_id: Option<&str>,
        role: Option<&str>,
        status: Option<AgentPresenceStatus>,
        capabilities: &[String],
    ) -> Vec<SenderIdentity> {
        let mut agents =
            self.registered
                .values()
                .filter(|identity| {
                    agent_id.is_none_or(|agent_id| identity.agent_id.as_str() == agent_id)
                        && pane_id.is_none_or(|pane_id| {
                            identity.pane_id.as_ref().is_some_and(|identity_pane_id| {
                                identity_pane_id.as_str() == pane_id
                            })
                        })
                        && window_id.is_none_or(|window_id| {
                            identity
                                .window_id
                                .as_ref()
                                .is_some_and(|identity_window_id| {
                                    identity_window_id.as_str() == window_id
                                })
                        })
                        && role.is_none_or(|role| identity.role.as_deref() == Some(role))
                        && status.is_none_or(|status| {
                            self.presence
                                .get(&identity.agent_id)
                                .is_some_and(|presence| presence.status == status)
                        })
                        && capabilities.iter().all(|capability| {
                            identity
                                .capabilities
                                .iter()
                                .any(|registered| registered == capability)
                        })
                })
                .cloned()
                .collect::<Vec<_>>();
        agents.sort_by(|left, right| left.agent_id.as_str().cmp(right.agent_id.as_str()));
        agents
    }

    /// Returns the canonical registration, if the identity remains live.
    pub fn registered_identity(&self, agent_id: &AgentId) -> Option<&SenderIdentity> {
        self.registered.get(agent_id)
    }

    /// Updates an existing presence record, rejecting unknown identities.
    pub fn update_presence(
        &mut self,
        agent_id: &AgentId,
        status: AgentPresenceStatus,
        now_ms: u64,
    ) -> Result<()> {
        let presence = self
            .presence
            .get_mut(agent_id)
            .ok_or_else(|| MessageError::not_found("agent not found"))?;
        presence.status = status;
        presence.updated_at_ms = now_ms;
        Ok(())
    }

    /// Records liveness without changing declared presence status.
    pub fn record_heartbeat(&mut self, agent_id: &AgentId, now_ms: u64) -> Result<()> {
        let presence = self
            .presence
            .get_mut(agent_id)
            .ok_or_else(|| MessageError::not_found("agent not found"))?;
        presence.updated_at_ms = now_ms;
        Ok(())
    }

    /// Projects presence in deterministic identity order.
    pub fn presence(&self) -> Vec<PresenceRecord> {
        let mut records = self.presence.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.identity
                .agent_id
                .as_str()
                .cmp(right.identity.agent_id.as_str())
        });
        records
    }
}
