//! Registered identity and objective publication on the single message owner.
//!
//! Reconciliation validates a complete candidate before replacing registration
//! and presence. Lifecycle project rebinding never changes queued audiences.

use super::*;

/// Reconciles an immutable optional field without overwriting registered values.
fn reconcile_identity_field<T: Clone + PartialEq>(
    existing: &mut Option<T>,
    incoming: &Option<T>,
    conflict_message: &str,
) -> Result<()> {
    match (&*existing, incoming) {
        (None, Some(incoming)) => *existing = Some(incoming.clone()),
        (Some(current), Some(incoming)) if current != incoming => {
            return Err(MessageError::conflict(conflict_message));
        }
        _ => {}
    }
    Ok(())
}

/// Reconciles capabilities as one immutable vector, never as a merged set.
fn reconcile_capabilities(existing: &mut Vec<String>, incoming: &[String]) -> Result<()> {
    if existing.is_empty() {
        if !incoming.is_empty() {
            *existing = incoming.to_vec();
        }
    } else if !incoming.is_empty() && existing.as_slice() != incoming {
        return Err(MessageError::conflict(
            "MMP sender capabilities cannot change after registration",
        ));
    }
    Ok(())
}

impl MessageService {
    /// Registers a new generated identity and its available presence record.
    pub fn register_agent(
        &mut self,
        pane_id: Option<PaneId>,
        window_id: Option<WindowId>,
        role: impl Into<String>,
        capabilities: Vec<String>,
    ) -> SenderIdentity {
        let identity = SenderIdentity {
            agent_id: self.ids.agent(),
            project_scope: None,
            pane_id,
            window_id,
            role: Some(role.into()),
            capabilities,
            objective: None,
        };
        self.insert_registered_identity(identity, 0)
    }

    /// Registers a generated identity after validating its bounded objective.
    pub fn register_agent_with_objective(
        &mut self,
        pane_id: Option<PaneId>,
        window_id: Option<WindowId>,
        role: impl Into<String>,
        capabilities: Vec<String>,
        objective: Option<&str>,
    ) -> Result<SenderIdentity> {
        let identity = SenderIdentity {
            agent_id: self.ids.agent(),
            project_scope: None,
            pane_id,
            window_id,
            role: Some(role.into()),
            capabilities,
            objective: normalize_optional_objective(objective)?,
        };
        validate_sender_identity(&identity)?;
        Ok(self.insert_registered_identity(identity, 0))
    }

    /// Inserts one identity into registered-agent and presence state.
    fn insert_registered_identity(
        &mut self,
        identity: SenderIdentity,
        updated_at_ms: u64,
    ) -> SenderIdentity {
        self.registered
            .insert(identity.agent_id.clone(), identity.clone());
        self.presence.insert(
            identity.agent_id.clone(),
            PresenceRecord {
                identity: identity.clone(),
                status: AgentPresenceStatus::Available,
                updated_at_ms,
            },
        );
        identity
    }

    /// Fills sparse registered metadata atomically, rejecting conflicting values.
    pub fn ensure_agent_identity(
        &mut self,
        identity: SenderIdentity,
        updated_at_ms: u64,
    ) -> Result<SenderIdentity> {
        validate_sender_identity(&identity)?;
        if let Some(existing) = self.registered.get(&identity.agent_id) {
            let mut reconciled = existing.clone();
            reconcile_identity_field(
                &mut reconciled.project_scope,
                &identity.project_scope,
                "MMP sender project scope cannot change after registration",
            )?;
            reconcile_identity_field(
                &mut reconciled.pane_id,
                &identity.pane_id,
                "MMP sender pane id cannot change after registration",
            )?;
            reconcile_identity_field(
                &mut reconciled.window_id,
                &identity.window_id,
                "MMP sender window id cannot change after registration",
            )?;
            reconcile_identity_field(
                &mut reconciled.role,
                &identity.role,
                "MMP sender role cannot change after registration",
            )?;
            reconcile_capabilities(&mut reconciled.capabilities, &identity.capabilities)?;

            self.registered
                .insert(identity.agent_id.clone(), reconciled.clone());
            if let Some(presence) = self.presence.get_mut(&identity.agent_id) {
                presence.identity = reconciled.clone();
            }
            return Ok(reconciled);
        }
        Ok(self.insert_registered_identity(identity, updated_at_ms))
    }

    /// Rebinds trusted lifecycle membership without altering delivery state.
    pub fn rebind_agent_project_scope(
        &mut self,
        agent_id: &AgentId,
        project_scope: Option<ProjectScopeId>,
    ) -> Result<SenderIdentity> {
        let identity = self.registered.get_mut(agent_id).ok_or_else(|| {
            MessageError::not_found("project-scope rebind requires a registered agent")
        })?;
        identity.project_scope = project_scope.clone();
        if let Some(presence) = self.presence.get_mut(agent_id) {
            presence.identity.project_scope = project_scope;
        }
        Ok(identity.clone())
    }

    /// Publishes a bounded objective; absent or unchanged refreshes are no-ops.
    /// Invalid refreshes leave registration and presence unchanged.
    pub fn update_agent_objective(
        &mut self,
        agent_id: &AgentId,
        objective: Option<&str>,
        updated_at_ms: u64,
    ) -> Result<bool> {
        let Some(objective) = normalize_optional_objective(objective)? else {
            return Ok(false);
        };
        let current = self
            .registered
            .get(agent_id)
            .ok_or_else(|| {
                MessageError::not_found("agent objective update requires a registered agent")
            })?
            .objective
            .clone();
        if current.as_deref() == Some(objective.as_str()) {
            return Ok(false);
        }
        if let Some(identity) = self.registered.get_mut(agent_id) {
            identity.objective = Some(objective.clone());
        }
        if let Some(record) = self.presence.get_mut(agent_id) {
            record.identity.objective = Some(objective);
            record.updated_at_ms = updated_at_ms;
        }
        Ok(true)
    }

    /// Intentionally clears an objective, unlike an absent refresh.
    pub fn clear_agent_objective(
        &mut self,
        agent_id: &AgentId,
        updated_at_ms: u64,
    ) -> Result<bool> {
        let current = self
            .registered
            .get(agent_id)
            .ok_or_else(|| {
                MessageError::not_found("agent objective clear requires a registered agent")
            })?
            .objective
            .clone();
        if current.is_none() {
            return Ok(false);
        }
        if let Some(identity) = self.registered.get_mut(agent_id) {
            identity.objective = None;
        }
        if let Some(record) = self.presence.get_mut(agent_id) {
            record.identity.objective = None;
            record.updated_at_ms = updated_at_ms;
        }
        Ok(true)
    }
}
