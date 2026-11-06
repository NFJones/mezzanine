//! Frozen canonical input and actor admission fences for one logical compactor.
//! This source precedes inference; a completed selective summary cannot create it.

use super::*;
use mez_agent::{ReplayCompactionPlan, ReplayCompactionPreparation, ReplayTailPolicy};

/// Canonical input retained through request chunks, corrections and publication.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeCompactionReplaySource {
    pub(super) owner: ReplayEpochContext,
    pub(super) plan: ReplayCompactionPlan,
    pub(in crate::runtime::commands::compaction) original:
        Arc<crate::runtime::agent_state::RuntimeCompactionSourceOwner>,
    pub(super) origin: ReplaySourceOrigin,
    pub(super) ephemeral: bool,
    pub(super) turn_id: String,
    config_generation: u64,
    config_layers: Vec<crate::config::ConfigLayer>,
    process: Option<crate::runtime::processes::RuntimePaneProcessIdentity>,
    turn_profile: Option<mez_agent::ModelProfile>,
}

pub(in crate::runtime::commands::compaction) enum Preparation {
    Unowned,
    NoWork,
    Ready(Arc<RuntimeCompactionReplaySource>),
}

impl RuntimeSessionService {
    /// Freezes the actual prefix before rendering provider input. Unowned legacy
    /// contexts remain with their existing producer until explicit migration.
    pub(in crate::runtime::commands::compaction) fn prepare_active_replay_compaction(
        &self,
        turn: &mez_agent::AgentTurnRecord,
        profile: &mez_agent::ModelProfile,
    ) -> Result<Preparation> {
        let Some(context) = self.agent_turn_contexts().get(&turn.turn_id) else {
            return Err(MezError::invalid_state(
                "compaction source context is unavailable",
            ));
        };
        if !context.has_replay_owner() {
            return Ok(Preparation::Unowned);
        }
        if self
            .agent_shell_store()
            .get(&turn.pane_id)
            .is_none_or(|session| session.session_id != turn.conversation_id)
        {
            return Err(MezError::conflict(
                "canonical compaction turn no longer owns its conversation",
            ));
        }
        let owner = ReplayEpochContext::from_owned_context(
            ReplayConversationId::new(turn.conversation_id.clone())?,
            context.clone(),
        )?;
        let budget = profile
            .max_input_tokens()
            .or_else(|| profile.context_window_tokens())
            .map(|limit| limit.min(profile.context_window_tokens().unwrap_or(limit)))
            .ok_or_else(|| {
                MezError::invalid_state("canonical compaction requires a model context limit")
            })?;
        let retained = u8::try_from(self.agent_compaction_raw_retention_percent())
            .map_err(|_| MezError::invalid_state("invalid canonical raw retention percentage"))?;
        let plan = match owner.prepare_compaction(ReplayTailPolicy::new(budget, retained)?)? {
            ReplayCompactionPreparation::Ready(plan) => plan,
            ReplayCompactionPreparation::NoWork(_) => return Ok(Preparation::NoWork),
        };
        let history = self.agent_turn_history_source(&turn.turn_id).cloned();
        let original = Arc::new(crate::runtime::agent_state::RuntimeCompactionSourceOwner {
            store: self.agent_turn_transcript_store(&turn.turn_id),
            history,
        });
        Ok(Preparation::Ready(Arc::new(
            RuntimeCompactionReplaySource {
                owner,
                plan,
                original,
                origin: ReplaySourceOrigin::new(&turn.turn_id, &turn.agent_id, &turn.pane_id)?,
                ephemeral: self.agent_turn_transcript_is_ephemeral(turn),
                turn_id: turn.turn_id.clone(),
                config_generation: self.session.config_generation,
                config_layers: self.integration.config_layers().to_vec(),
                process: self.steering_process_binding(&turn.pane_id)?,
                turn_profile: self.agent_turn_model_profile(&turn.turn_id).cloned(),
            },
        )))
    }
}

impl RuntimeCompactionReplaySource {
    pub(in crate::runtime::commands::compaction) fn input_blocks(
        &self,
    ) -> Vec<mez_agent::ContextBlock> {
        self.plan
            .summary_input()
            .iter()
            .map(|event| event.block().clone())
            .collect()
    }

    /// Checks live ownership without allocating a new head or performing I/O.
    /// Installed-store replacement cannot retarget the original history source.
    pub(super) fn validate_live(
        &self,
        service: &RuntimeSessionService,
        pane: &str,
        generation: u64,
    ) -> Result<()> {
        if !service.agent_compaction_task_is_current(pane, generation)
            || service.agent_shell_store().get(pane).is_none_or(|session| {
                session.session_id != self.owner.conversation_id().as_str()
                    || session.running_turn_id.as_deref() != Some(self.turn_id.as_str())
                    || session.ephemeral != self.ephemeral
            })
            || service.session.config_generation != self.config_generation
            || service.integration.config_layers() != self.config_layers.as_slice()
            || service.agent_turn_model_profile(&self.turn_id) != self.turn_profile.as_ref()
            || service
                .agent_turn_ledger()
                .turn(&self.turn_id)
                .is_none_or(|turn| {
                    turn.state != mez_agent::AgentTurnState::Running
                        || turn.pane_id != pane
                        || turn.conversation_id != self.owner.conversation_id().as_str()
                })
        {
            return Err(MezError::conflict(
                "canonical compaction actor owner became stale",
            ));
        }
        let history = service.agent_turn_history_source(&self.turn_id);
        if !match (self.original.history.as_ref(), history) {
            (Some(original), Some(current)) => Arc::ptr_eq(original, current),
            (None, None) => true,
            _ => false,
        } {
            return Err(MezError::conflict(
                "canonical compaction history owner changed",
            ));
        }
        let current = service.steering_process_binding(pane)?;
        if !match (self.process.as_ref(), current.as_ref()) {
            (Some(original), Some(current)) => original.same_incarnation(current),
            (None, None) => true,
            _ => false,
        } {
            return Err(MezError::conflict(
                "canonical compaction pane incarnation changed",
            ));
        }
        let body = service
            .agent_turn_contexts()
            .get(&self.turn_id)
            .ok_or_else(|| MezError::conflict("canonical compaction live body disappeared"))?;
        ReplayEpochContext::from_owned_context(
            ReplayConversationId::new(self.owner.conversation_id().as_str())?,
            body.clone(),
        )?
        .validate_compaction_source(&self.plan)?;
        Ok(())
    }
}
