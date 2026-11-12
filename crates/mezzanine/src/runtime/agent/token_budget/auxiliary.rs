//! Original native ownership for turn-less auxiliary inference. Queue capture
//! is descriptive and I/O-free; the existing provider preparation worker must
//! publish membership before claim. These records grant no execution authority.

use super::*;

/// Exact queued operation selected by the existing auxiliary scheduler.
#[derive(Debug, Clone)]
pub(crate) enum RuntimeBudgetAuxiliarySource {
    Compaction {
        pane: String,
        generation: u64,
    },
    Remember {
        pane: String,
        observation: String,
    },
    Title {
        conversation: String,
        dispatch: String,
    },
}

/// Native capture made before queuing, never reconstructed from later lineage.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetAuxiliaryCapture {
    pane: String,
    conversation: String,
    native: mez_agent::messaging::MessageNativeIncarnation,
    started_at: u64,
    store: Option<TokenUsageStore>,
    transcripts: Option<AgentTranscriptStore>,
}

/// Issued request retains its original immutable membership and actual holder
/// until the provider future retires, independently of pane/actor replacement.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetAuxiliaryOwner {
    pub(crate) member: BudgetMembership,
    pub(crate) lease: BudgetBindingLease,
    binding_owner: String,
}

impl RuntimeBudgetAuxiliaryOwner {
    /// Pure execution-boundary subject check; this is not a policy permit or a
    /// disk revalidation. The held descriptor remains owned across the request.
    pub(crate) fn validate_request(&self, conversation: &str) -> Result<()> {
        if self.member.conversation != conversation
            || !self.lease.matches(&self.member, &self.binding_owner)
        {
            return Err(MezError::conflict(
                "auxiliary budget request subject changed",
            ));
        }
        Ok(())
    }
}

impl RuntimeSessionService {
    /// Borrows the queued memory capture inside its owning component boundary.
    pub(crate) fn claim_runtime_budget_remember(
        &self,
        pane: &str,
    ) -> Result<Option<RuntimeBudgetAuxiliaryOwner>> {
        self.claim_runtime_budget_auxiliary(
            self.agent
                .pending_agent_remember_tasks
                .get(pane)
                .and_then(|task| task.budget_capture.as_ref()),
        )
    }

    /// Missing capture stays unavailable under enabled assembly. Queue owners
    /// retain existing captures on retry rather than recapturing a replacement.
    pub(crate) fn capture_runtime_budget_auxiliary(
        &self,
        pane: &str,
        conversation: &str,
    ) -> Option<RuntimeBudgetAuxiliaryCapture> {
        if !self.agent.budget_memberships.enabled {
            return None;
        }
        let session = self.agent_shell_store().get(pane)?;
        if session.session_id != conversation {
            return None;
        }
        Some(RuntimeBudgetAuxiliaryCapture {
            pane: pane.to_owned(),
            conversation: conversation.to_owned(),
            native: session.native_message_incarnation.clone(),
            started_at: crate::runtime::current_unix_seconds(),
            store: self.persistence.cloned_token_usage_store(),
            transcripts: self.persistence.cloned_transcript_store(),
        })
    }

    fn validate_auxiliary_capture(&self, capture: &RuntimeBudgetAuxiliaryCapture) -> Result<()> {
        if capture.store.is_none()
            || capture.transcripts.is_none()
            || self.persistence.token_usage_store() != capture.store.as_ref()
            || self.persistence.transcript_store() != capture.transcripts.as_ref()
            || !self
                .agent_shell_store()
                .get(&capture.pane)
                .is_some_and(|session| {
                    session.session_id == capture.conversation
                        && session.native_message_incarnation == capture.native
                })
        {
            return Err(MezError::conflict(
                "original auxiliary budget binding changed or unavailable",
            ));
        }
        Ok(())
    }

    /// Selects pending work by its exact existing scheduler descriptor. Raw-tail
    /// publication has no inference and therefore needs no auxiliary admission.
    pub(crate) fn prepare_runtime_budget_auxiliary(
        &mut self,
        source: &RuntimeBudgetAuxiliarySource,
    ) -> Result<Option<crate::runtime::RuntimeAgentProviderPreparationWork>> {
        if !self.agent.budget_memberships.enabled {
            return Ok(None);
        }
        let capture = match source {
            RuntimeBudgetAuxiliarySource::Compaction { pane, generation } => {
                if self.pending_agent_compaction_task_generation(pane) != Some(*generation)
                    || self.agent_is_human_paused(pane)
                {
                    return Ok(None);
                }
                let task = self
                    .retained_agent_compaction_task(pane, *generation)
                    .ok_or_else(|| MezError::conflict("auxiliary compaction source changed"))?;
                if task.request.is_none() {
                    return Ok(None);
                }
                task.budget_capture.clone()
            }
            RuntimeBudgetAuxiliarySource::Remember { pane, observation } => {
                if self.pending_agent_remember_observation_id(pane) != Some(observation.as_str())
                    || self.agent_is_human_paused(pane)
                {
                    return Ok(None);
                }
                self.agent
                    .pending_agent_remember_tasks
                    .get(pane)
                    .and_then(|task| task.budget_capture.clone())
            }
            RuntimeBudgetAuxiliarySource::Title {
                conversation,
                dispatch,
            } => {
                if self.pending_agent_session_title_dispatch_id(conversation)
                    != Some(dispatch.as_str())
                {
                    return Ok(None);
                }
                self.agent
                    .pending_agent_session_title_tasks
                    .get(conversation)
                    .and_then(|task| task.budget_capture.clone())
            }
        }
        .ok_or_else(|| MezError::invalid_state("auxiliary budget capture unavailable"))?;
        self.validate_auxiliary_capture(&capture)?;
        if self.agent_is_human_paused(&capture.pane) {
            return Ok(None);
        }
        let membership = self.prepare_runtime_budget_conversation(
            &capture.pane,
            &capture.conversation,
            true,
            capture.started_at,
        )?;
        Ok(membership.map(crate::runtime::RuntimeAgentProviderPreparationWork::budget_only))
    }

    /// Claim-time revalidation follows the existing task's authority checks.
    /// A prepared outcome or descriptor alone is never a durable readiness ACK.
    pub(crate) fn claim_runtime_budget_auxiliary(
        &self,
        capture: Option<&RuntimeBudgetAuxiliaryCapture>,
    ) -> Result<Option<RuntimeBudgetAuxiliaryOwner>> {
        if !self.agent.budget_memberships.enabled {
            return Ok(None);
        }
        let capture = capture
            .ok_or_else(|| MezError::invalid_state("auxiliary budget capture unavailable"))?;
        self.validate_auxiliary_capture(capture)?;
        let entry = self
            .agent
            .budget_memberships
            .entries
            .get(&capture.conversation)
            .filter(|entry| {
                entry.ready
                    && entry.pane == capture.pane
                    && entry.native == capture.native
                    && Some(&entry.store) == capture.store.as_ref()
                    && Some(&entry.transcripts) == capture.transcripts.as_ref()
            })
            .ok_or_else(|| MezError::invalid_state("auxiliary budget membership is not ready"))?;
        let member = entry
            .member
            .clone()
            .ok_or_else(|| MezError::invalid_state("auxiliary budget membership absent"))?;
        let lease = entry
            .lease
            .clone()
            .filter(|lease| lease.matches(&member, &entry.owner))
            .ok_or_else(|| MezError::invalid_state("auxiliary budget custody absent"))?;
        Ok(Some(RuntimeBudgetAuxiliaryOwner {
            member,
            lease,
            binding_owner: entry.owner.clone(),
        }))
    }
}
