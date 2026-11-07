//! Frozen canonical input and actor admission fences for one logical compactor.
//! This source precedes inference; a completed selective summary cannot create it.

use super::*;
use crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger;
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
    target: SourceTarget,
    config_generation: u64,
    config_layers: Vec<crate::config::ConfigLayer>,
    process: Option<crate::runtime::processes::RuntimePaneProcessIdentity>,
    model_profile: Option<mez_agent::ModelProfile>,
}

/// An active turn has a mutable canonical body. Idle manual work retains the
/// exact frozen body and binding instead; it never manufactures a running turn.
#[derive(Debug, Clone)]
enum SourceTarget {
    Turn {
        turn: String,
        trigger: RuntimeActiveTurnCompactionTrigger,
    },
    Manual(Box<ManualSource>),
}

/// Idle source freshness, independent of canonical membership and storage CAS.
/// A NoStore view uses its original opaque capture, even when counters match.
#[derive(Debug, Clone)]
struct ManualSource {
    transcript_entries: u64,
    transcript_high_water: u64,
    profile_name: String,
    store: Option<crate::storage::transcript::AgentTranscriptStore>,
    frozen: Option<crate::runtime::control::RuntimeFrozenReplayHistory>,
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
        trigger: RuntimeActiveTurnCompactionTrigger,
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
                target: SourceTarget::Turn {
                    turn: turn.turn_id.clone(),
                    trigger,
                },
                config_generation: self.session.config_generation,
                config_layers: self.integration.config_layers().to_vec(),
                process: self.steering_process_binding(&turn.pane_id)?,
                model_profile: self.agent_turn_model_profile(&turn.turn_id).cloned(),
            },
        )))
    }

    /// Freezes already checked idle history without admitting another task or
    /// inferring native completion. Pane counts are freshness guards only.
    pub(in crate::runtime::commands::compaction) fn prepare_manual_replay_compaction(
        &self,
        pane: &str,
        owner: ReplayEpochContext,
        profile_name: &str,
        profile: &mez_agent::ModelProfile,
        history: Option<Arc<crate::runtime::control::RuntimeAgentHistorySource>>,
    ) -> Result<Preparation> {
        let session = self
            .agent_shell_store()
            .get(pane)
            .ok_or_else(|| MezError::conflict("manual replay conversation disappeared"))?;
        if session.running_turn_id.is_some()
            || session.session_id != owner.conversation_id().as_str()
        {
            return Err(MezError::conflict(
                "manual replay source does not match its idle conversation",
            ));
        }
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
        let store = history.as_ref().map_or_else(
            || self.persistence.transcript_store().cloned(),
            |source| source.store(),
        );
        let original =
            Arc::new(crate::runtime::agent_state::RuntimeCompactionSourceOwner { store, history });
        let operation = format!(
            "manual-compaction-{}",
            String::from(mez_agent::ContextBlockOccurrenceId::allocate())
        );
        Ok(Preparation::Ready(Arc::new(
            RuntimeCompactionReplaySource {
                owner,
                plan,
                original,
                origin: ReplaySourceOrigin::new(&operation, &format!("agent-{pane}"), pane)?,
                ephemeral: session.ephemeral,
                target: SourceTarget::Manual(Box::new(ManualSource {
                    transcript_entries: session.transcript_entries,
                    transcript_high_water: session.transcript_high_water,
                    profile_name: profile_name.to_owned(),
                    store: self.persistence.transcript_store().cloned(),
                    frozen: session
                        .ephemeral
                        .then(|| self.frozen_replay_binding(pane, &session.session_id))
                        .flatten()
                        .cloned(),
                })),
                config_generation: self.session.config_generation,
                config_layers: self.integration.config_layers().to_vec(),
                process: self.steering_process_binding(pane)?,
                model_profile: Some(profile.clone()),
            },
        )))
    }
}

impl RuntimeCompactionReplaySource {
    pub(super) fn turn_id(&self) -> Option<&str> {
        match &self.target {
            SourceTarget::Turn { turn, .. } => Some(turn),
            SourceTarget::Manual(_) => None,
        }
    }

    /// Resumes the original trigger only after storage receipt adoption. A
    /// provider rejection must finish its exact retry reducer attempt; merely
    /// setting a generic pending-provider flag leaves that attempt recovering.
    /// Idle manual work has no provider continuation to resume.
    pub(super) fn resume_after_publication(
        &self,
        service: &mut RuntimeSessionService,
        pane: &str,
    ) -> Result<()> {
        let SourceTarget::Turn { turn, trigger } = &self.target else {
            return Ok(());
        };
        let (queued, trace) = match trigger {
            RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { attempt } => (
                service
                    .queue_agent_provider_recovery_task_after_context_compaction(turn, *attempt)?,
                "provider_request recovery_resuming reason=provider_context_limit_compaction_completed",
            ),
            RuntimeActiveTurnCompactionTrigger::ObservedInputLimit { .. } => (
                service.queue_agent_provider_recovery_task_after_compaction(
                    turn,
                    "observed_input_limit_compaction",
                )?,
                "provider_request recovery_resuming reason=observed_input_limit_compaction_completed",
            ),
        };
        if queued {
            service.append_agent_trace_turn_event(pane, turn, trace)?;
        }
        Ok(())
    }

    /// Supplies the current actual turn body or the immutable idle source. The
    /// latter carries its original continuation seal, not a cold reconstruction.
    pub(super) fn current_body(&self, service: &RuntimeSessionService) -> Result<AgentContext> {
        match self.turn_id() {
            Some(turn) => service
                .agent_turn_contexts()
                .get(turn)
                .cloned()
                .ok_or_else(|| MezError::conflict("canonical live context disappeared")),
            None => Ok(self.owner.clone().into_owned_context()),
        }
    }
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
                    || session.running_turn_id.as_deref() != self.turn_id()
                    || session.ephemeral != self.ephemeral
            })
            || service.session.config_generation != self.config_generation
            || service.integration.config_layers() != self.config_layers.as_slice()
        {
            return Err(MezError::conflict(
                "canonical compaction actor owner became stale",
            ));
        }
        match &self.target {
            SourceTarget::Turn { turn, .. } => {
                if service.agent_turn_model_profile(turn) != self.model_profile.as_ref()
                    || service.agent_turn_ledger().turn(turn).is_none_or(|record| {
                        record.state != mez_agent::AgentTurnState::Running
                            || record.pane_id != pane
                            || record.conversation_id != self.owner.conversation_id().as_str()
                    })
                {
                    return Err(MezError::conflict(
                        "canonical compaction turn owner changed",
                    ));
                }
                let history = service.agent_turn_history_source(turn);
                if !match (self.original.history.as_ref(), history) {
                    (Some(original), Some(current)) => Arc::ptr_eq(original, current),
                    (None, None) => true,
                    _ => false,
                } {
                    return Err(MezError::conflict(
                        "canonical compaction history owner changed",
                    ));
                }
            }
            SourceTarget::Manual(original) => {
                let session = service
                    .agent_shell_store()
                    .get(pane)
                    .ok_or_else(|| MezError::conflict("manual replay binding disappeared"))?;
                let (name, profile) =
                    service.active_model_profile_for_pane(pane, &format!("agent-{pane}"), None)?;
                if session.transcript_entries != original.transcript_entries
                    || session.transcript_high_water != original.transcript_high_water
                    || service.persistence.transcript_store() != original.store.as_ref()
                    || session
                        .ephemeral
                        .then(|| service.frozen_replay_binding(pane, &session.session_id))
                        .flatten()
                        != original.frozen.as_ref()
                    || name != original.profile_name
                    || Some(&profile) != self.model_profile.as_ref()
                {
                    return Err(MezError::conflict(
                        "manual canonical compaction source became stale",
                    ));
                }
            }
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
        let body = self.current_body(service)?;
        ReplayEpochContext::from_owned_context(
            ReplayConversationId::new(self.owner.conversation_id().as_str())?,
            body,
        )?
        .validate_compaction_source(&self.plan)?;
        Ok(())
    }
}
