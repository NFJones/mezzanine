//! Registered identity and objective publication on the single message owner.
//!
//! Reconciliation validates a complete candidate before replacing registration
//! and presence. Lifecycle project rebinding never changes queued audiences.

use super::*;

/// Same-runtime observation of one actual sender registration allocation.
/// This opaque token supplies no acceptance, permission or reconnect authority;
/// objective/presence telemetry preserve it, authority changes and restore do not.
#[derive(Clone)]
pub struct MessageSenderRegistration {
    agent_id: AgentId,
    identity: Arc<()>,
}
impl std::fmt::Debug for MessageSenderRegistration {
    /// Reveals only public attribution, never allocation pointers or credentials.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageSenderRegistration")
            .field("agent_id", &self.agent_id)
            .finish_non_exhaustive()
    }
}

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
    /// Captures the actual live allocation without accepting, offering or reserving
    /// a message. Missing registrations return None; the token is never serialized.
    pub fn capture_sender_registration(
        &self,
        agent_id: &AgentId,
    ) -> Option<MessageSenderRegistration> {
        self.registration_tokens
            .get(agent_id)
            .map(|identity| MessageSenderRegistration {
                agent_id: agent_id.clone(),
                identity: Arc::clone(identity),
            })
    }

    /// Tests exact live allocation independently of mutable telemetry, public ID,
    /// allocator floor or equal restored data. This is observation, not permission.
    pub fn sender_registration_is_current(&self, token: &MessageSenderRegistration) -> bool {
        self.registered.contains_key(&token.agent_id)
            && self
                .registration_tokens
                .get(&token.agent_id)
                .is_some_and(|identity| Arc::ptr_eq(identity, &token.identity))
    }

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

    /// Inserts one identity into registered-agent and presence state. The
    /// allocation high-water fences unpublished older originals as well as
    /// accepted ones; a replacement cannot acquire an in-flight old binding.
    fn insert_registered_identity(
        &mut self,
        identity: SenderIdentity,
        updated_at_ms: u64,
    ) -> SenderIdentity {
        self.registration_tokens
            .insert(identity.agent_id.clone(), Arc::new(()));
        self.registration_floors.insert(
            identity.agent_id.clone(),
            self.next_sequence.saturating_sub(1),
        );
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

            if !same_immutable_sender(existing, &reconciled) {
                self.registration_tokens
                    .insert(identity.agent_id.clone(), Arc::new(()));
            }
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
        if identity.project_scope != project_scope {
            self.registration_tokens
                .insert(agent_id.clone(), Arc::new(()));
        }
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

#[cfg(test)]
mod registration_tests {
    use super::*;

    /// Mutable sender telemetry preserves the live registration observation,
    /// while a foreign or restored equal-looking service cannot inherit it.
    /// No occurrence is accepted and the observation supplies no delivery proof.
    #[test]
    fn sender_registration_observation_requires_actual_live_allocation() {
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "sender", vec![]);
        let token = service
            .capture_sender_registration(&sender.agent_id)
            .unwrap();
        service
            .update_agent_objective(&sender.agent_id, Some("changed objective"), 1)
            .unwrap();
        assert!(service.sender_registration_is_current(&token));
        let restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
        assert!(!restored.sender_registration_is_current(&token));
        assert!(!MessageService::default().sender_registration_is_current(&token));
        assert!(
            restored.sender_registration_is_current(
                &restored
                    .capture_sender_registration(&sender.agent_id)
                    .unwrap()
            )
        );
    }

    /// Retirement and same-ID replacement without any intervening sequence
    /// allocation can have equal numeric floors. Only the actual allocation token
    /// distinguishes that replacement; stale source authority must not transfer.
    #[test]
    fn sender_registration_replacement_without_mail_revokes_original_observation() {
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "sender", vec![]);
        let token = service
            .capture_sender_registration(&sender.agent_id)
            .unwrap();
        let floor = service.registration_floors[&sender.agent_id];
        assert!(service.retire_observational_identity(&sender.agent_id));
        assert!(!service.sender_registration_is_current(&token));
        service.ensure_agent_identity(sender.clone(), 2).unwrap();
        assert_eq!(service.registration_floors[&sender.agent_id], floor);
        assert!(!service.sender_registration_is_current(&token));
        assert!(
            service.sender_registration_is_current(
                &service
                    .capture_sender_registration(&sender.agent_id)
                    .unwrap()
            )
        );
    }
    /// Trusted immutable metadata enrichment and project rebind must invalidate
    /// queued sender authority even without retirement. Repeating the identical
    /// binding is inert, and a new token belongs only to the resulting identity.
    #[test]
    fn sender_registration_observation_rejects_authority_rebinding() {
        let mut service = MessageService::default();
        let mut sender = service.register_agent(None, None, "sender", vec![]);
        let token = service
            .capture_sender_registration(&sender.agent_id)
            .unwrap();
        service.ensure_agent_identity(sender.clone(), 1).unwrap();
        assert!(service.sender_registration_is_current(&token));
        sender.capabilities.push("new-capability".into());
        service.ensure_agent_identity(sender.clone(), 2).unwrap();
        assert!(!service.sender_registration_is_current(&token));
        let token = service
            .capture_sender_registration(&sender.agent_id)
            .unwrap();
        let project = ProjectScopeId::from_canonical_root_bytes(b"/fixture/new-project");
        service
            .rebind_agent_project_scope(&sender.agent_id, Some(project.clone()))
            .unwrap();
        assert!(!service.sender_registration_is_current(&token));
        let token = service
            .capture_sender_registration(&sender.agent_id)
            .unwrap();
        service
            .rebind_agent_project_scope(&sender.agent_id, Some(project))
            .unwrap();
        assert!(service.sender_registration_is_current(&token));
    }
}
