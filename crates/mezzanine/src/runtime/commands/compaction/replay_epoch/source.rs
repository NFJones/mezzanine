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
    /// Pane-local authority may change without a configuration generation.
    /// Both modes keep the exact accepted policy/action surface, independently
    /// of whether a model request exists. A changed scope requires new admission.
    permissions: mez_agent::permissions::PermissionPolicy,
    allowed_actions: mez_agent::AllowedActionSet,
    process: Option<crate::runtime::processes::RuntimePaneProcessIdentity>,
    model_profile: Option<mez_agent::ModelProfile>,
    /// Exact inference profile, distinct from a routing parent turn's profile.
    compactor_profile: mez_agent::ModelProfile,
    /// The live actor's original cached view. Preparing a selected compactor
    /// view changes only owned copies, and cannot hide an unexpected live switch.
    original_projection: Option<mez_agent::ReplayProviderProjection>,
    /// One private graph, retained across physical wave and retry generations.
    operation: operation::Operation,
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
    frozen: Option<crate::runtime::control::RuntimeFrozenReplayHistory>,
}

pub(in crate::runtime::commands::compaction) enum Preparation {
    NoWork(super::super::NoWorkReason),
    Ready(Arc<RuntimeCompactionReplaySource>),
}

impl RuntimeSessionService {
    /// All triggers derive the identical raw reservation from the selected
    /// model's actual input/context allowance and configured retention.
    fn compaction_tail_policy(
        &self,
        profile: &mez_agent::ModelProfile,
        owner: &ReplayEpochContext,
        turn: &mez_agent::AgentTurnRecord,
        allowed: &mez_agent::AllowedActionSet,
    ) -> Result<ReplayTailPolicy> {
        let budget = profile
            .max_input_tokens()
            .or_else(|| profile.context_window_tokens())
            .map(|limit| limit.min(profile.context_window_tokens().unwrap_or(limit)))
            .ok_or_else(|| {
                MezError::invalid_state("canonical compaction requires a model context limit")
            })?;
        let retained = u8::try_from(self.agent_compaction_raw_retention_percent())
            .map_err(|_| MezError::invalid_state("invalid canonical raw retention percentage"))?;
        let policy = ReplayTailPolicy::new(budget, retained)?;
        match self.agent_compaction_mode() {
            crate::runtime::config::CompactionMode::Summary => {
                Ok(policy.with_head_policy(mez_agent::ReplayHeadPolicy::Summary))
            }
            crate::runtime::config::CompactionMode::RawTail => {
                let provider = self
                    .provider_registry()
                    .provider(&profile.provider)
                    .ok_or_else(|| MezError::config("compaction provider is unavailable"))?;
                let api = mez_agent::resolve_provider_api(&provider.kind, provider.api.as_deref())?;
                let options =
                    crate::runtime::config::runtime_effective_provider_options(provider, profile);
                // This is the ordinary continuation's schema/control framing,
                // not a compactor request. Neither credentials nor transport
                // are consulted during this read-only sizing operation.
                let mut request = crate::integrations::agent::context::assemble_model_request(
                    profile,
                    api,
                    turn,
                    owner.context(),
                )?;
                mez_agent::apply_model_request_control(
                    &mut request,
                    Some(allowed.clone()),
                    Some(mez_agent::ModelInteractionKind::ActionExecution),
                );
                let tools = mez_agent::invoked_mcp_tools_for_context_with_configured(
                    owner.context(),
                    &self.mcp_registry().prompt_summary(),
                    self.integration.always_exposed_mcp_servers(),
                );
                mez_agent::apply_default_action_gates(
                    &mut request,
                    &tools,
                    self.runtime_persistent_memory_enabled(),
                    crate::runtime::commands::runtime_issues_enabled(self),
                );
                let fixed = mez_agent::provider_request_fixed_input_estimate(
                    &request, api, &options, true,
                )?;
                Ok(policy.with_raw_tail_limits(fixed.input_tokens, None))
            }
        }
    }

    /// Selects the actual configured compactor view on an isolated canonical
    /// owner, without credential access or changing the live history body.
    fn bind_compaction_provider_projection(
        &self,
        owner: &mut ReplayEpochContext,
        profile: &mez_agent::ModelProfile,
    ) -> Result<()> {
        let provider = self
            .provider_registry()
            .provider(&profile.provider)
            .ok_or_else(|| MezError::config("compaction provider is unavailable"))?;
        let api = mez_agent::resolve_provider_api(&provider.kind, provider.api.as_deref())?;
        owner.select_provider_projection(mez_agent::ReplayProviderProjection::new(
            api,
            profile.provider.clone(),
            profile.model.clone(),
        )?)?;
        Ok(())
    }

    /// Freezes the actual prefix before rendering provider input. A descriptive
    /// legacy body cannot authorize another model producer: the original history
    /// importer or task admission must establish its canonical owner first.
    pub(in crate::runtime::commands::compaction) fn prepare_active_replay_compaction(
        &self,
        turn: &mez_agent::AgentTurnRecord,
        profile: &mez_agent::ModelProfile,
        trigger: RuntimeActiveTurnCompactionTrigger,
        allowed: &mez_agent::AllowedActionSet,
    ) -> Result<Preparation> {
        let Some(context) = self.agent_turn_contexts().get(&turn.turn_id) else {
            return Err(MezError::invalid_state(
                "compaction source context is unavailable",
            ));
        };
        if !context.has_replay_owner() {
            return Err(MezError::invalid_state(
                "active compaction history lacks its canonical source owner",
            ));
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
        let mut owner = ReplayEpochContext::from_owned_context(
            ReplayConversationId::new(turn.conversation_id.clone())?,
            context.clone(),
        )?;
        let original_projection = owner.provider_projection().cloned();
        self.bind_compaction_provider_projection(&mut owner, profile)?;
        let policy = self.compaction_tail_policy(profile, &owner, turn, allowed)?;
        if !owner.automatic_compaction_is_fresh(policy)? {
            return Ok(Preparation::NoWork(
                super::super::NoWorkReason::UnchangedPublishedSource {
                    head_revision: owner.revision().get(),
                },
            ));
        }
        let preparation = if self.agent_compaction_mode()
            == crate::runtime::config::CompactionMode::RawTail
            && matches!(
                trigger,
                RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { .. }
            ) {
            owner.prepare_raw_compaction_after_input_rejection(policy)?
        } else {
            owner.prepare_compaction(policy)?
        };
        let plan = match preparation {
            ReplayCompactionPreparation::Ready(plan) => plan,
            ReplayCompactionPreparation::NoWork(
                mez_agent::ReplayCompactionNoWork::ProtectedInputExceedsBudget,
            ) => {
                return Ok(Preparation::NoWork(
                    super::super::NoWorkReason::ProtectedInputExceedsBudget,
                ));
            }
            ReplayCompactionPreparation::NoWork(
                mez_agent::ReplayCompactionNoWork::RetainedSourceUnchanged,
            ) => {
                return Ok(Preparation::NoWork(
                    super::super::NoWorkReason::RetainedRawTailUnchanged,
                ));
            }
            ReplayCompactionPreparation::NoWork(_) => {
                return Ok(Preparation::NoWork(
                    super::super::NoWorkReason::NoEligibleClosedPrefix,
                ));
            }
        };
        let history = self.agent_turn_history_source(&turn.turn_id).cloned();
        let original = Arc::new(crate::runtime::agent_state::RuntimeCompactionSourceOwner {
            store: self.agent_turn_transcript_store(&turn.turn_id),
            history,
        });
        let permit = self
            .agent_source_preparation_uses_adapter()
            .then(|| self.reserve_agent_source_preparation())
            .transpose()?;
        let operation = operation::Operation::new(plan.clone(), permit);
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
                permissions: self.permission_policy_for_pane(&turn.pane_id),
                allowed_actions: allowed.clone(),
                process: self.steering_process_binding(&turn.pane_id)?,
                model_profile: self.agent_turn_model_profile(&turn.turn_id).cloned(),
                compactor_profile: profile.clone(),
                original_projection,
                operation,
            },
        )))
    }

    /// Freezes already checked idle history without admitting another task or
    /// inferring native completion. Pane counts are freshness guards only.
    pub(in crate::runtime::commands::compaction) fn prepare_manual_replay_compaction(
        &self,
        pane: &str,
        mut owner: ReplayEpochContext,
        profile: (&str, &mez_agent::ModelProfile),
        history: Option<Arc<crate::runtime::control::RuntimeAgentHistorySource>>,
        permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
        allowed: &mez_agent::AllowedActionSet,
    ) -> Result<Preparation> {
        let (profile_name, profile) = profile;
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
        let original_projection = owner.provider_projection().cloned();
        self.bind_compaction_provider_projection(&mut owner, profile)?;
        // An idle sizing descriptor is never admitted as an ordinary turn.
        let sizing_turn = mez_agent::AgentTurnRecord {
            turn_id: "manual-compaction-sizing".into(),
            conversation_id: session.session_id.clone(),
            agent_id: format!("agent-{pane}"),
            pane_id: pane.into(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 0,
            deadline_at_unix_millis: 0,
            policy_profile: "runtime".into(),
            model_profile: profile_name.into(),
            parent_turn_id: None,
            state: mez_agent::AgentTurnState::Queued,
            cooperation_mode: None,
            initial_capability: None,
        };
        let policy = self.compaction_tail_policy(profile, &owner, &sizing_turn, allowed)?;
        let plan = match owner.prepare_compaction(policy)? {
            ReplayCompactionPreparation::Ready(plan) => plan,
            ReplayCompactionPreparation::NoWork(
                mez_agent::ReplayCompactionNoWork::ProtectedInputExceedsBudget,
            ) => {
                return Ok(Preparation::NoWork(
                    super::super::NoWorkReason::ProtectedInputExceedsBudget,
                ));
            }
            ReplayCompactionPreparation::NoWork(
                mez_agent::ReplayCompactionNoWork::RetainedSourceUnchanged,
            ) => {
                return Ok(Preparation::NoWork(
                    super::super::NoWorkReason::RetainedRawTailUnchanged,
                ));
            }
            ReplayCompactionPreparation::NoWork(_) => {
                // With no selectable prefix all raw source must remain. Use
                // the same cached cost view and reservation as the planner;
                // this diagnostic cannot relax protection or create model work.
                let estimates = owner
                    .provider_input_estimates()
                    .unwrap_or(owner.input_estimates());
                let reason = if estimates.raw_input_tokens() > policy.retained_raw_tokens() {
                    super::super::NoWorkReason::IrreducibleExactTail
                } else {
                    super::super::NoWorkReason::NoEligibleClosedPrefix
                };
                return Ok(Preparation::NoWork(reason));
            }
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
        let model_work = operation::Operation::new(plan.clone(), permit);
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
                    frozen: session
                        .ephemeral
                        .then(|| self.frozen_replay_binding(pane, &session.session_id))
                        .flatten()
                        .cloned(),
                })),
                config_generation: self.session.config_generation,
                config_layers: self.integration.config_layers().to_vec(),
                permissions: self.permission_policy_for_pane(pane),
                allowed_actions: allowed.clone(),
                process: self.steering_process_binding(pane)?,
                model_profile: Some(profile.clone()),
                compactor_profile: profile.clone(),
                original_projection,
                operation: model_work,
            },
        )))
    }
}

impl RuntimeCompactionReplaySource {
    /// Captured source policy decides whether inference may be constructed.
    pub(in crate::runtime::commands::compaction) fn requires_model_summary(&self) -> bool {
        self.plan.requires_model_summary()
    }

    /// Nonblocking, content-free display observation, never a wave receipt.
    pub(crate) fn model_progress(&self) -> operation::Snapshot {
        if self.requires_model_summary() {
            self.operation.progress()
        } else {
            operation::Snapshot {
                phase: "publishing",
                model_calls: 0,
                serial_depth: 0,
            }
        }
    }

    /// Uses the existing observed provider/expense owner for one bounded wave.
    pub(crate) async fn execute_model_wave<
        P: crate::integrations::agent::provider::AsyncModelProvider,
    >(
        &self,
        provider: &P,
        task: &crate::runtime::RuntimeAgentCompactionTask,
        shape: (
            mez_agent::ProviderApiCompatibility,
            std::collections::BTreeMap<String, String>,
            bool,
        ),
    ) -> Result<mez_agent::ModelResponse> {
        let input = task
            .model_profile
            .max_input_tokens()
            .or_else(|| task.model_profile.context_window_tokens())
            .map(|limit| limit.min(task.model_profile.context_window_tokens().unwrap_or(limit)))
            .ok_or_else(|| {
                MezError::invalid_state("compaction requires a provider input capacity")
            })?;
        self.operation
            .execute(
                provider,
                task.task_generation,
                task.request.clone().ok_or_else(|| {
                    MezError::invalid_state("raw omission cannot execute a compactor model wave")
                })?,
                shape,
                input,
            )
            .await
    }

    pub(in crate::runtime::commands::compaction) fn model_wave_outcome(
        &self,
        generation: u64,
    ) -> Result<WaveOutcome> {
        self.operation.outcome(generation)
    }

    pub(in crate::runtime::commands::compaction) fn model_work_can_retry(
        &self,
        generation: u64,
    ) -> bool {
        self.operation.can_retry(generation)
    }

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
            Some(turn) => {
                let body = service
                    .agent_turn_contexts()
                    .get(turn)
                    .cloned()
                    .ok_or_else(|| MezError::conflict("canonical live context disappeared"))?;
                let mut owner = ReplayEpochContext::from_owned_context(
                    self.owner.conversation_id().clone(),
                    body,
                )?;
                if owner.provider_projection() != self.original_projection.as_ref() {
                    return Err(MezError::conflict(
                        "canonical live provider projection changed",
                    ));
                }
                if let Some(projection) = self.plan.provider_projection() {
                    owner.select_provider_projection(projection.clone())?;
                }
                Ok(owner.into_owned_context())
            }
            None => Ok(self.owner.clone().into_owned_context()),
        }
    }
    /// Number of immutable source occurrences, without cloning source bodies
    /// on actor admission just to populate content-free status metadata.
    pub(in crate::runtime::commands::compaction) fn input_entry_count(&self) -> usize {
        self.plan.removed_events().count()
    }

    /// Borrows the original frozen selection for publication/inspection tests.
    /// This is not a mutable planner or another admission path.
    #[cfg(test)]
    pub(crate) fn frozen_plan(&self) -> &mez_agent::ReplayCompactionPlan {
        &self.plan
    }

    #[cfg(test)]
    pub(crate) fn trigger(&self) -> Option<RuntimeActiveTurnCompactionTrigger> {
        match &self.target {
            SourceTarget::Turn { trigger, .. } => Some(*trigger),
            SourceTarget::Manual(_) => None,
        }
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
                    || session.allowed_actions.as_ref() != Some(&self.allowed_actions)
            })
            || service.session.config_generation != self.config_generation
            || service.integration.config_layers() != self.config_layers.as_slice()
        {
            return Err(MezError::conflict(
                "canonical compaction actor owner became stale",
            ));
        }
        let current_policy = service.permission_policy_for_pane(pane);
        if self.permissions.preset != current_policy.preset
            || self.permissions.approval_policy != current_policy.approval_policy
            || self.permissions.approval_bypass() != current_policy.approval_bypass()
            || self.permissions.rules() != current_policy.rules()
            || self.permissions.trusted_directories != current_policy.trusted_directories
        {
            return Err(MezError::conflict(
                "canonical compaction permission scope changed from accepted source",
            ));
        }
        let accepted = service
            .retained_agent_compaction_task(pane, generation)
            .ok_or_else(|| MezError::conflict("canonical compaction task is no longer retained"))?;
        if !accepted
            .replay_source
            .as_deref()
            .is_some_and(|source| std::ptr::eq(source, self))
            || !accepted
                .source_owner
                .as_ref()
                .is_some_and(|owner| Arc::ptr_eq(owner, &self.original))
        {
            return Err(MezError::conflict(
                "canonical compaction source is not the accepted task source",
            ));
        }
        if accepted.model_profile != self.compactor_profile {
            return Err(MezError::conflict(
                "canonical compactor model profile changed",
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
                // Request admission already checked the installed source. An
                // accepted logical operation keeps its original store through
                // correction and publication; a later installation cannot
                // retarget or invalidate that original physical ownership.
                if session.transcript_entries != original.transcript_entries
                    || session.transcript_high_water != original.transcript_high_water
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
