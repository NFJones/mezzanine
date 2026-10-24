//! Original execution-group handoff and fail-closed retirement ownership.
//!
//! This is part of the existing boxed completion owner, not an execution queue,
//! second context chronology, or persisted-result history. Each admitted group
//! keeps its original transcript target and action set. A failed forced handoff
//! moves backing state here before ordinary pane/turn cleanup may discard it.
//! Candidate acceptance is only producer ownership, never checked append or disk
//! durability. Only successful destination evidence may advance a milestone.
//! Lexical scopes retain exact groups/errors, not already retired live maps.
//! Canonical lifetime follows actual required consumers and original candidate
//! or no-save destinations; error attribution alone cannot reconstruct context.

use super::*;
use crate::runtime::agent::bookkeeping::{
    RuntimeBookkeepingCandidate, RuntimeBookkeepingTranscriptReadWork,
};
use crate::runtime::persistence::RuntimeCompletionCandidateWitness;

/// Frozen intentional runtime-only policy, distinct from an archive witness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime::agent) enum CompletionNoPersistencePolicy {
    /// The conversation was deliberately ephemeral before work was issued.
    Ephemeral,
    /// This runtime had no transcript repository configured at admission.
    /// There is no production store-clearing API; an attached original handle
    /// is retained instead and is never recomputed from a replacement pane.
    RuntimeOnly,
}

/// Original target sampled while the exact execution group owns admission.
pub(super) enum CompletionTranscriptTarget {
    Original {
        read: Box<RuntimeBookkeepingTranscriptReadWork>,
        directory: Option<TranscriptEntry>,
    },
    NoPersistencePolicy(CompletionNoPersistencePolicy),
}

/// One non-replayed group continuation or terminal effect boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum CompletionMilestone {
    #[default]
    NotEntered,
    Entered,
    Accepted,
}

/// Retention lifetime only, never authorization. Nested lifecycle helpers share
/// the original outer owner until every fallible suffix has returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime) enum CompletionScope {
    Callback,
    Stop,
    Finish,
    PaneRemoval,
}

/// One lexical observation of existing retention, consumed exactly once by its
/// paired close. This is neither live-source nor destructive cleanup authority;
/// it must never be stored, cloned, retried, or carried across an await.
#[must_use = "completion retention must be closed by its synchronous owner"]
pub(in crate::runtime) struct CompletionScopeObservation {
    /// Absence at entry cannot adopt a group admitted later under the same key.
    owner: Option<CompletionScopeOwner>,
}

/// Bounded original observations for one synchronous caller. This is retention
/// and error attribution only, not authority for any caller's destructive work.
#[must_use = "all original completion observations must be closed"]
pub(in crate::runtime) struct CompletionScopeSet(Vec<CompletionScopeObservation>);

/// Frozen group allocation and actual installed outer scope, not a new token.
struct CompletionScopeOwner {
    identity: std::sync::Arc<()>,
    turn: AgentTurnRecord,
    group: mez_agent::ContextExecutionGroupId,
    /// Nested requested kinds do not replace the existing installed kind.
    installed: CompletionScope,
    /// Only the frame that installed retention may clear it at lexical close.
    owns_scope: bool,
}

impl CompletionScopeOwner {
    /// Checks group identity and installed retention only. This cannot validate
    /// live execution/context/token maps or confer cancellation authority.
    fn matches(&self, group: &CompletionGroup) -> bool {
        std::sync::Arc::ptr_eq(&self.identity, &group.identity)
            && self.turn == group.turn
            && self.group == group.group
            && group.scope == Some(self.installed)
    }
}

/// Fixed required-gate provenance; diagnostic text never becomes transcript data.
pub(super) struct CompletionRequiredFailure {
    pub(super) stage: &'static str,
    pub(super) action_id: Option<String>,
    pub(super) attempt: Option<String>,
    pub(super) error: Box<MezError>,
}

/// Moved canonical backing of a failed group, not a cloned parallel timeline.
/// Captured absence is final; it never authorizes adopting a later live value.
#[derive(Default)]
pub(super) enum CompletionCapture<T> {
    #[default]
    NotCaptured,
    Captured(T),
}

impl<T> CompletionCapture<T> {
    /// Runs the actual move only for an unobserved component. Captured absence
    /// is final and must never adopt a replacement inserted under a reused key.
    pub(super) fn capture_once(&mut self, capture: impl FnOnce() -> T) {
        if matches!(self, Self::NotCaptured) {
            *self = Self::Captured(capture());
        }
    }
}

impl<T> CompletionCapture<Option<T>> {
    /// Borrows an actually captured value, without a live-map fallback.
    pub(super) fn as_ref(&self) -> Option<&T> {
        match self {
            Self::Captured(value) => value.as_ref(),
            Self::NotCaptured => None,
        }
    }
}

/// Per-component transfer within the original failed group, not another queue.
#[derive(Default)]
pub(super) struct CompletionFailedBacking {
    pub(super) execution: CompletionCapture<Option<AgentTurnExecution>>,
    pub(super) context: CompletionCapture<Option<mez_agent::AgentContext>>,
    pub(super) pending_native: CompletionCapture<Vec<crate::runtime::RuntimeNativeShellDispatch>>,
    pub(super) patch_states: CompletionCapture<Vec<(String, RuntimeApplyPatchBatchState)>>,
    pub(super) patch_phases: CompletionCapture<Vec<(String, RuntimePendingApplyPatchPhase)>>,
}

/// The exact immutable candidate and its cached actual producer witness.
pub(super) struct CompletionTranscriptHandoff {
    pub(super) candidate: RuntimeBookkeepingCandidate,
    pub(super) witness: Option<RuntimeCompletionCandidateWitness>,
    pub(super) evidence: String,
}

/// Actual producer ownership is distinct from intentional runtime-only policy;
/// neither certifies checked append or disk durability.
pub(in crate::runtime::agent) enum CompletionTranscriptOwnership {
    Candidate(RuntimeCompletionCandidateWitness),
    NoPersistencePolicy(CompletionNoPersistencePolicy),
}

/// One bounded admitted execution-group slot; per-action transfer cannot erase
/// its original target, required errors, or group-finalization milestones.
pub(super) struct CompletionGroup {
    /// Strong private identity keeps an old permit distinct after slot reuse.
    pub(super) identity: std::sync::Arc<()>,
    pub(super) turn: AgentTurnRecord,
    pub(super) group: mez_agent::ContextExecutionGroupId,
    pub(super) actions: Vec<(String, String, usize)>,
    pub(super) issued: BTreeSet<(String, String)>,
    pub(super) observed: BTreeSet<(String, String)>,
    pub(super) context_committed: BTreeSet<(String, String)>,
    pub(super) transcript: CompletionTranscriptTarget,
    pub(super) handoff: Option<Box<CompletionTranscriptHandoff>>,
    pub(super) provider_handoff: Option<ProviderHandoffOwner>,
    pub(super) failed_backing: Option<Box<CompletionFailedBacking>>,
    /// Actual healthy correction source moved before legacy callers discard
    /// their live execution slot; not failed backing or another history queue.
    pub(super) correction_execution: CompletionCapture<Option<AgentTurnExecution>>,
    pub(super) primary_failure: Option<Box<CompletionRequiredFailure>>,
    /// At most one first error per entered action/attempt/stage; duplicates do
    /// not create new diagnostics or a new retry/settlement generation.
    pub(super) secondary_failures: Vec<CompletionRequiredFailure>,
    pub(super) safe_failure_evidence: Option<String>,
    pub(super) paint_revoked: bool,
    pub(super) continuation: CompletionMilestone,
    /// One unresolved ordinary settlement, not a provider claim or payload queue.
    pub(super) pending_settlement: Option<CompletionSettlementIntent>,
    /// Exact group/failure-qualified required audit entry/acceptance state.
    pub(super) failure_audit: Option<CompletionFailureAudit>,
    /// Optional group paint diagnostic when no per-action receipt remains.
    pub(super) publication_failure: Option<Box<MezError>>,
    /// Immutable body/say source still requires publication, unlike trace paint.
    pub(super) publication_held: bool,
    /// Records already-validated human guidance for this exact group. It only
    /// permits provider feedback with settled errors, never action authority.
    pub(super) human_resume_generation: Option<u64>,
    pub(super) task_result: CompletionMilestone,
    pub(super) scheduler: CompletionMilestone,
    pub(super) scope: Option<CompletionScope>,
    /// Diagnostic provenance only; never used to grant cleanup, paint or work.
    pub(super) original_loop_role: Option<serde_json::Value>,
}

impl fmt::Debug for CompletionGroup {
    /// Generic runtime debug never includes raw output/context/error messages.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletionGroup")
            .field("turn_id", &self.turn.turn_id)
            .field("group", &self.group)
            .field("actions", &self.actions.len())
            .field("issued", &self.issued.len())
            .field("has_handoff", &self.handoff.is_some())
            .field("has_failed_backing", &self.failed_backing.is_some())
            .field("paint_revoked", &self.paint_revoked)
            .field("continuation", &self.continuation)
            .field("task_result", &self.task_result)
            .field("scheduler", &self.scheduler)
            .field(
                "failure_kind",
                &self
                    .primary_failure
                    .as_ref()
                    .map(|failure| failure.error.kind()),
            )
            .finish()
    }
}

impl CompletionGroup {
    /// A failed source remains owned while admitted callbacks settle. Healthy
    /// source changes discard only the cache; immutable accepted destinations
    /// remain independent. Neither path manufactures a new handoff witness.
    pub(super) fn source_changed(&mut self) {
        if self.primary_failure.is_some() {
            if let Some(handoff) = self.handoff.as_mut() {
                handoff.witness = None;
            }
        } else {
            self.handoff = None;
        }
    }
}

impl RuntimeSessionService {
    /// Arms one inert failure at an admitted caller or cleanup-helper entry.
    /// This is absent in production and cannot grant cleanup authority.
    #[cfg(test)]
    pub(in crate::runtime) fn fail_pane_retention_entry_for_tests(
        &mut self,
        stage: &'static str,
        error: MezError,
    ) {
        self.agent
            .completion_settlements
            .pane_retention_fault_for_tests = Some((stage, error));
    }

    /// Consumes only the matching fixed test cut; other entry paths leave it
    /// armed so a test can distinguish an unreached boundary from success.
    #[cfg(test)]
    pub(in crate::runtime) fn take_pane_retention_fault_for_tests(
        &mut self,
        stage: &'static str,
    ) -> Option<MezError> {
        if self
            .agent
            .completion_settlements
            .pane_retention_fault_for_tests
            .as_ref()
            .is_some_and(|(armed, _)| *armed == stage)
        {
            self.agent
                .completion_settlements
                .pane_retention_fault_for_tests
                .take()
                .map(|(_, error)| error)
        } else {
            None
        }
    }

    /// Arms one inert failure after shutdown capture and before descendants.
    /// The original typed error is consumed once; no production fault exists.
    #[cfg(test)]
    pub(in crate::runtime) fn fail_completion_shutdown_before_descendants_for_tests(
        &mut self,
        error: MezError,
    ) {
        self.agent.completion_settlements.shutdown_fault_for_tests = Some(error);
    }

    /// Takes the isolated shutdown cut so tests can prove it was consumed.
    #[cfg(test)]
    pub(in crate::runtime) fn take_completion_shutdown_fault_for_tests(
        &mut self,
    ) -> Option<MezError> {
        self.agent
            .completion_settlements
            .shutdown_fault_for_tests
            .take()
    }

    /// Combines only actually issued lexical observations for close-order tests.
    /// No identity, claim, scope or cleanup authority is manufactured here.
    #[cfg(test)]
    pub(in crate::runtime) fn completion_scope_set_for_tests(
        observations: Vec<CompletionScopeObservation>,
    ) -> CompletionScopeSet {
        CompletionScopeSet(observations)
    }

    /// Reads each bounded typed failure without changing group custody.
    #[cfg(test)]
    pub(in crate::runtime) fn completion_required_failures_for_tests(
        &self,
        turn_id: &str,
    ) -> Vec<(&'static str, MezError)> {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .map_or_else(Vec::new, |group| {
                group
                    .primary_failure
                    .iter()
                    .map(Box::as_ref)
                    .chain(group.secondary_failures.iter())
                    .map(|failure| (failure.stage, (*failure.error).clone()))
                    .collect()
            })
    }

    /// Observes actual retention for inert tests without issuing a frame,
    /// replacing a group, or manufacturing archive/dispatch evidence.
    #[cfg(test)]
    pub(in crate::runtime) fn completion_scope_is_active_for_tests(
        &self,
        turn_id: &str,
        scope: CompletionScope,
    ) -> bool {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .is_some_and(|group| group.scope == Some(scope))
    }

    /// Observes frozen issued accounting without changing or settling expense.
    #[cfg(test)]
    pub(crate) fn provider_refusal_accounting_for_tests(
        &self,
        turn_id: &str,
        generation: u64,
    ) -> Option<(
        AgentTurnRecord,
        String,
        String,
        crate::storage::token_usage::AccountingOrigin,
    )> {
        let owner = self
            .agent
            .provider_accounting_owners
            .get(&(turn_id.to_string(), generation))?;
        Some((
            owner.turn.clone(),
            owner.profile.provider.clone(),
            owner.profile.model.clone(),
            owner.origin.clone(),
        ))
    }

    /// Observes the actual lease error; never changes claims or group failures.
    #[cfg(test)]
    pub(crate) fn observe_provider_lease_refusal_for_tests(
        &mut self,
        turn_id: &str,
        generation: u64,
        error: &MezError,
    ) {
        self.agent
            .completion_settlements
            .provider_lease_refusal_for_tests =
            Some((turn_id.to_string(), generation, error.clone()));
    }

    /// Reads test evidence only, not a production refusal-recovery owner.
    #[cfg(test)]
    pub(crate) fn provider_lease_refusal_for_tests(&self) -> Option<&(String, u64, MezError)> {
        self.agent
            .completion_settlements
            .provider_lease_refusal_for_tests
            .as_ref()
    }

    /// Observes the actual admitted source policy and group, never a target
    /// manufactured by a test. Policy strings are observation only.
    #[cfg(test)]
    pub(crate) fn completion_source_group_for_tests(
        &self,
        turn_id: &str,
    ) -> Option<(&'static str, String)> {
        let group = self.agent.completion_settlements.groups.get(turn_id)?;
        let policy = match &group.transcript {
            CompletionTranscriptTarget::Original { .. } => "original",
            CompletionTranscriptTarget::NoPersistencePolicy(
                CompletionNoPersistencePolicy::RuntimeOnly,
            ) => "runtime_only",
            CompletionTranscriptTarget::NoPersistencePolicy(
                CompletionNoPersistencePolicy::Ephemeral,
            ) => "ephemeral",
        };
        Some((policy, group.group.as_str().to_string()))
    }

    /// Returns exact original-group source and typed primary for custody tests.
    /// A live replacement execution is never evidence of original custody.
    /// Clones are observation values only and are not installed or retained.
    #[cfg(test)]
    pub(crate) fn original_completion_source_for_tests(
        &self,
        turn_id: &str,
        expected_group: &str,
    ) -> Option<(
        AgentTurnExecution,
        mez_agent::AgentContext,
        Option<MezError>,
    )> {
        let group = self.agent.completion_settlements.groups.get(turn_id)?;
        if group.group.as_str() != expected_group {
            return None;
        }
        let (execution, context) = if let Some(backing) = group.failed_backing.as_ref() {
            (backing.execution.as_ref()?, backing.context.as_ref()?)
        } else {
            (
                self.agent_turn_executions()
                    .get(turn_id)
                    .or_else(|| group.correction_execution.as_ref())?,
                self.agent_turn_contexts().get(turn_id)?,
            )
        };
        if crate::runtime::agent::provider_execution::provider_log_execution_group_id(execution)
            .ok()?
            != group.group
        {
            return None;
        }
        Some((
            execution.clone(),
            context.clone(),
            group
                .primary_failure
                .as_ref()
                .map(|failure| (*failure.error).clone()),
        ))
    }

    /// Fences only NEW work after a required settlement failure. Already-issued
    /// result ingress, cancellation and reap must never call this admission gate.
    /// Keeping existing admitted slots while refusing new ones bounds failed
    /// completion ownership without creating a historical receipt store.
    pub(crate) fn ensure_completion_admission_open(&self) -> Result<()> {
        match self
            .agent
            .completion_settlements
            .critical_failure
            .as_deref()
        {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    /// Reserves the current canonical group's original read and complete action
    /// set before physical worker dispatch. An issued attempt is recorded only
    /// after its actual actor claim, never inferred from an expected sibling.
    pub(in crate::runtime::agent) fn reserve_completion_group(
        &mut self,
        turn_id: &str,
        group_id: &mez_agent::ContextExecutionGroupId,
    ) -> Result<()> {
        self.ensure_completion_admission_open()?;
        if let Some(group) = self.agent.completion_settlements.groups.get(turn_id) {
            return if &group.group == group_id
                && !group.paint_revoked
                && group.failed_backing.is_none()
            {
                Ok(())
            } else {
                Err(MezError::invalid_state(
                    "previous completion group still owns its execution slot",
                ))
            };
        }
        let turn = self
            .agent_turn_ledger()
            .turn(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("completion group turn is unavailable"))?;
        let execution = self
            .agent_turn_executions()
            .get(turn_id)
            .ok_or_else(|| MezError::invalid_state("completion group execution is unavailable"))?;
        if crate::runtime::agent::provider_execution::provider_log_execution_group_id(execution)?
            != *group_id
        {
            return Err(MezError::invalid_state(
                "completion group differs from canonical execution",
            ));
        }
        let actions = execution
            .response
            .action_batch
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("completion group action set is unavailable"))?
            .actions
            .iter()
            .enumerate()
            .map(|(ordinal, action)| (action.id.clone(), action.action_type().to_string(), ordinal))
            .collect();
        let session = self.agent_shell_store().get(&turn.pane_id).ok_or_else(|| {
            MezError::invalid_state("completion group original binding is unavailable")
        })?;
        if session.session_id != turn.conversation_id {
            return Err(MezError::invalid_state(
                "completion group original conversation changed",
            ));
        }
        let transcript = if session.ephemeral {
            CompletionTranscriptTarget::NoPersistencePolicy(
                CompletionNoPersistencePolicy::Ephemeral,
            )
        } else if let Some(store) = self.persistence.cloned_transcript_store() {
            CompletionTranscriptTarget::Original {
                read: Box::new(self.capture_completion_transcript_read(store, &turn)?),
                directory: self.capture_completion_directory_entry(&turn),
            }
        } else {
            CompletionTranscriptTarget::NoPersistencePolicy(
                CompletionNoPersistencePolicy::RuntimeOnly,
            )
        };
        let original_loop_role = self.agent_loop_turn(turn_id).and_then(|loop_turn| {
            self.agent_loop_state_by_id(&loop_turn.loop_id).map(|state| serde_json::json!({
                "loop_id": loop_turn.loop_id, "old_work_turn_id": turn_id,
                "work_agent_id": turn.agent_id, "work_conversation_id": turn.conversation_id,
                "iteration": loop_turn.iteration, "execution_pane_id": state.execution_pane_id,
                "invoking_pane_id": state.invoking_pane_id,
                "parent_conversation_id": state.parent_conversation_id,
                "routed_parent_turn_id": state.routed_parent_turn_id,
                "completion": state.completion.as_ref().map(|completion| serde_json::json!({
                    "parent_turn_id": completion.parent_turn_id,
                    "parent_action_id": completion.parent_action_id,
                    "first_child_turn_id": completion.child_turn_id,
                    "child_agent_id": completion.child_agent_id,
                })),
            }))
        });
        self.agent.completion_settlements.groups.insert(
            turn_id.to_string(),
            Box::new(CompletionGroup {
                identity: std::sync::Arc::new(()),
                turn,
                group: group_id.clone(),
                actions,
                issued: BTreeSet::new(),
                observed: BTreeSet::new(),
                context_committed: BTreeSet::new(),
                transcript,
                handoff: None,
                provider_handoff: None,
                failed_backing: None,
                correction_execution: CompletionCapture::NotCaptured,
                primary_failure: None,
                secondary_failures: Vec::new(),
                safe_failure_evidence: None,
                paint_revoked: false,
                human_resume_generation: None,
                continuation: CompletionMilestone::NotEntered,
                pending_settlement: None,
                failure_audit: None,
                publication_failure: None,
                publication_held: false,
                task_result: CompletionMilestone::NotEntered,
                scheduler: CompletionMilestone::NotEntered,
                scope: None,
                original_loop_role,
            }),
        );
        Ok(())
    }

    /// Records one actual claimed action/phase and invalidates any older frozen
    /// snapshot. This is not dispatch acceptance or result acknowledgement.
    pub(in crate::runtime::agent) fn record_completion_issued_attempt(
        &mut self,
        turn_id: &str,
        action_id: &str,
        attempt: &str,
    ) -> Result<()> {
        let group = self
            .agent
            .completion_settlements
            .groups
            .get_mut(turn_id)
            .ok_or_else(|| MezError::invalid_state("issued completion group is unavailable"))?;
        if !group.actions.iter().any(|(id, _, _)| id == action_id) {
            return Err(MezError::invalid_state(
                "issued action is outside admitted completion group",
            ));
        }
        if group
            .issued
            .insert((action_id.to_string(), attempt.to_string()))
        {
            group.source_changed();
        }
        Ok(())
    }

    /// A new canonical observation or required failure makes an older candidate
    /// witness insufficient. Already-owned immutable candidates remain in their
    /// destination; the group must hand off its changed evidence separately.
    pub(in crate::runtime::agent) fn invalidate_completion_handoff(&mut self, turn_id: &str) {
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id) {
            group.source_changed();
        }
    }

    /// Retains the full typed primary error and bounded first secondary errors
    /// under actual admitted action/phase/stage identities. Transcript projection
    /// uses only fixed provenance and kind, never these private message bytes.
    pub(in crate::runtime::agent) fn record_completion_required_failure(
        &mut self,
        turn_id: &str,
        stage: &'static str,
        action_id: Option<&str>,
        attempt: Option<&str>,
        error: &MezError,
    ) {
        let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id) else {
            return;
        };
        let same = |failure: &CompletionRequiredFailure| {
            failure.stage == stage
                && failure.action_id.as_deref() == action_id
                && failure.attempt.as_deref() == attempt
        };
        if group.primary_failure.as_deref().is_some_and(same)
            || group.secondary_failures.iter().any(same)
        {
            return;
        }
        // Action failures can name only an actually issued owner. Group-level
        // stages are fixed call-site constants, not arbitrary worker labels.
        if let Some(action_id) = action_id
            && !group.issued.iter().any(|(action, issued)| {
                action == action_id && attempt.is_none_or(|attempt| attempt == issued)
            })
        {
            return;
        }
        let failure = CompletionRequiredFailure {
            stage,
            action_id: action_id.map(str::to_string),
            attempt: attempt.map(str::to_string),
            error: Box::new(error.clone()),
        };
        if group.primary_failure.is_none() {
            group.primary_failure = Some(Box::new(failure));
        } else {
            group.secondary_failures.push(failure);
        }
        if let Some(handoff) = group.handoff.as_mut() {
            handoff.witness = None;
        }
        self.agent
            .completion_settlements
            .critical_failure
            .get_or_insert_with(|| Box::new(error.clone()));
    }

    /// Revokes only this exact group's paint eligibility. Raw outcomes, claims,
    /// queued immutable sources and canonical backing remain owned until handoff.
    pub(in crate::runtime) fn revoke_completion_paint(&mut self, turn_id: &str) {
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id) {
            group.paint_revoked = true;
        }
    }

    /// Moves failed backing before any bypass clear can destroy it. A second
    /// cleanup cannot overwrite the original owner or create another timeline.
    pub(in crate::runtime::agent) fn detach_failed_completion_backing(&mut self, turn_id: &str) {
        let Some(group) = self.agent.completion_settlements.groups.get(turn_id) else {
            return;
        };
        let backing = group.failed_backing.as_ref();
        let take_execution = backing
            .is_none_or(|backing| matches!(backing.execution, CompletionCapture::NotCaptured));
        let take_context = !self.provider_refusal_context_is_live(turn_id)
            && backing
                .is_none_or(|backing| matches!(backing.context, CompletionCapture::NotCaptured));
        let take_native = backing
            .is_none_or(|backing| matches!(backing.pending_native, CompletionCapture::NotCaptured));
        let take_states = backing
            .is_none_or(|backing| matches!(backing.patch_states, CompletionCapture::NotCaptured));
        let take_phases = backing
            .is_none_or(|backing| matches!(backing.patch_phases, CompletionCapture::NotCaptured));
        let from_correction = take_execution && group.correction_execution.as_ref().is_some();
        if self.provider_refusal_is_armed(turn_id)
            && (take_execution || take_context || take_native || take_states || take_phases)
            && (!self.agent_turn_ledger().turn(turn_id).is_some_and(|turn| {
                turn.agent_id == group.turn.agent_id
                    && turn.pane_id == group.turn.pane_id
                    && turn.conversation_id == group.turn.conversation_id
            }) || self
                .agent_shell_store()
                .get(&group.turn.pane_id)
                .is_some_and(|_| !self.provider_refusal_shell_binding_owned(turn_id)))
        {
            let error =
                MezError::invalid_state("refusal component capture original binding changed");
            self.record_completion_required_failure(
                turn_id,
                "refusal_capture_binding",
                None,
                None,
                &error,
            );
            return;
        }
        // Negative custody may never adopt the newly installed provider-error
        // execution (or a later group). Leave the actual live value untouched.
        if take_execution
            && !from_correction
            && self.provider_refusal_is_armed(turn_id)
            && self
                .agent_turn_executions()
                .get(turn_id)
                .is_some_and(|execution| {
                    crate::runtime::agent::provider_execution::provider_log_execution_group_id(
                        execution,
                    )
                    .map_or(true, |identity| identity != group.group)
                })
        {
            let error =
                MezError::invalid_state("refusal capture does not own the original execution");
            self.record_completion_required_failure(
                turn_id,
                "refusal_capture_owner",
                None,
                None,
                &error,
            );
            return;
        }
        let action_ids = group
            .actions
            .iter()
            .map(|(id, _, _)| id.clone())
            .collect::<Vec<_>>();
        let pending_native = if take_native {
            let keys = self
                .agent
                .pending_native_shell_dispatches
                .keys()
                .filter(|(owner, _)| owner == turn_id)
                .cloned()
                .collect::<Vec<_>>();
            Some(
                keys.into_iter()
                    .filter_map(|key| self.agent.pending_native_shell_dispatches.remove(&key))
                    .collect(),
            )
        } else {
            None
        };
        let mut patch_states = Vec::new();
        let mut patch_phases = Vec::new();
        for action_id in action_ids {
            let key = Self::apply_patch_batch_state_key(turn_id, &action_id);
            if take_states && let Some(state) = self.agent.apply_patch_batch_states.remove(&key) {
                patch_states.push((action_id.clone(), state));
            }
            if take_phases && let Some(phase) = self.agent.pending_apply_patch_phases.remove(&key) {
                patch_phases.push((action_id, phase));
            }
        }
        let execution = if from_correction {
            self.agent
                .completion_settlements
                .groups
                .get_mut(turn_id)
                .map(|group| {
                    match std::mem::replace(
                        &mut group.correction_execution,
                        CompletionCapture::Captured(None),
                    ) {
                        CompletionCapture::Captured(execution) => execution,
                        CompletionCapture::NotCaptured => None,
                    }
                })
        } else {
            take_execution.then(|| self.agent_turn_executions_mut().remove(turn_id))
        };
        let context = take_context.then(|| self.agent_turn_contexts_mut().remove(turn_id));
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id) {
            let backing = group.failed_backing.get_or_insert_with(Default::default);
            if let Some(execution) = execution {
                backing.execution.capture_once(|| execution);
            }
            if let Some(context) = context {
                backing.context.capture_once(|| context);
            }
            if let Some(native) = pending_native {
                backing.pending_native.capture_once(|| native);
            }
            if take_states {
                backing.patch_states.capture_once(|| patch_states);
            }
            if take_phases {
                backing.patch_phases.capture_once(|| patch_phases);
            }
            group.paint_revoked = true;
        }
    }

    /// Returns original group failure truth without consulting replacement pane
    /// metadata or requiring backing to remain indexed in live execution maps.
    pub(in crate::runtime::agent) fn completion_group_failure(
        &self,
        turn_id: &str,
    ) -> Option<MezError> {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .and_then(|group| group.primary_failure.as_ref())
            .map(|failure| (*failure.error).clone())
    }

    /// Builds only fixed-version provenance/kind/identity facts. A retained
    /// outcome, context commit, candidate witness and gate acceptance are
    /// deliberately different claims. No private error or invalid output enters
    /// this SYSTEM evidence; full typed diagnostics remain in the live owner.
    fn completion_settlement_evidence(&self, turn_id: &str) -> Result<String> {
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(turn_id)
            .ok_or_else(|| MezError::invalid_state("completion evidence group is unavailable"))?;
        let failures = group
            .primary_failure
            .iter()
            .map(Box::as_ref)
            .chain(group.secondary_failures.iter())
            .map(|failure| {
                serde_json::json!({
                    "stage": failure.stage,
                    "error_kind": runtime_mezzanine_error_code(failure.error.kind()),
                    "action_id": failure.action_id,
                    "attempt": failure.attempt,
                })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({
            "completion_settlement": {
                "version": 1, "group": group.group.as_str(),
                "turn_id": group.turn.turn_id,
                "admitted_actions": group.actions,
                "issued_attempts": group.issued,
                "retained_delivery_ids": group.observed,
                "context_committed_ids": group.context_committed,
                "required_failures": failures,
                "possible_effects": !group.issued.is_empty(),
                "automatic_replay_allowed": false,
                "candidate_witness_is_gate_or_disk_acceptance": false,
                "paint_revoked": group.paint_revoked,
                "original_loop_role": group.original_loop_role,
                "task_result_milestone": format!("{:?}", group.task_result),
                "scheduler_milestone": format!("{:?}", group.scheduler),
            }
        })
        .to_string())
    }

    /// Stages immutable source in the existing bounded group before required
    /// effects. This does not accept a candidate, append rows or grant a receipt.
    pub(in crate::runtime::agent) fn stage_completion_group_transcript(
        &mut self,
        turn_id: &str,
    ) -> Result<()> {
        let evidence = self.completion_settlement_evidence(turn_id)?;
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(turn_id)
            .ok_or_else(|| MezError::invalid_state("completion handoff group is unavailable"))?;
        if let CompletionTranscriptTarget::NoPersistencePolicy(_) = &group.transcript {
            if let Some(failure) = &group.primary_failure {
                return Err((*failure.error).clone());
            }
            return Ok(());
        }
        let existing = group.handoff.as_ref();
        // Only an actual ordinary refusal with prequalified original source may
        // use this frozen projection. General forced cleanup must still project
        // its current original backing; an arbitrary stale cache is not proof.
        let frozen_refusal = group
            .provider_handoff
            .as_ref()
            .is_some_and(|owner| owner.rejection.is_some() && owner.source_frozen);
        let execution = self
            .agent_turn_executions()
            .get(turn_id)
            .or_else(|| group.correction_execution.as_ref())
            .filter(|_| !frozen_refusal && self.agent_turn_contexts().contains_key(turn_id));
        let fresh_source = execution.is_some();
        let mut candidate = if let Some(execution) = execution {
            let CompletionTranscriptTarget::Original { read, directory } = &group.transcript else {
                return Err(MezError::invalid_state(
                    "completion archive target is unavailable",
                ));
            };
            self.completion_bookkeeping_candidate(
                &group.turn,
                execution,
                read.as_ref().clone(),
                directory.clone(),
            )?
        } else if let Some(handoff) = existing {
            // Canonical backing may already have transferred after correction.
            // Its accepted immutable candidate remains the source, not ambient
            // replacement pane state or newly sampled repository policy.
            handoff.candidate.clone()
        } else {
            return Err(MezError::invalid_state(
                "completion handoff execution is unavailable",
            ));
        };
        if fresh_source || existing.is_none_or(|handoff| handoff.evidence != evidence) {
            Self::append_completion_settlement_evidence(
                &mut candidate,
                &group.group,
                evidence.clone(),
            )?;
        }
        if let Some(handoff) = existing
            && handoff.evidence == evidence
            && handoff.candidate.entries.len() == candidate.entries.len()
        {
            // New projection wall-clock stamps are not new canonical source.
            // Compare a temporary projection with the original accepted times;
            // never rewrite the actual candidate/read/hash/witness or pending
            // archive rows. Changed status/context/content must get a new exact
            // candidate acceptance even when receipt metadata did not change.
            let mut comparable = candidate.clone();
            for (new, original) in comparable
                .entries
                .iter_mut()
                .zip(&handoff.candidate.entries)
            {
                new.created_at_unix_seconds = original.created_at_unix_seconds;
            }
            if comparable.completion_snapshot_bytes()?
                == handoff.candidate.completion_snapshot_bytes()?
            {
                return Ok(());
            }
        }
        // Install the exact rows in their same admitted group before asking the
        // destination to accept them. A rejection keeps all source bytes live.
        let group = self
            .agent
            .completion_settlements
            .groups
            .get_mut(turn_id)
            .ok_or_else(|| MezError::invalid_state("completion handoff owner disappeared"))?;
        group.handoff = Some(Box::new(CompletionTranscriptHandoff {
            candidate,
            witness: None,
            evidence,
        }));
        Ok(())
    }

    /// Accepts staged original-target source in the EXISTING checked candidate
    /// owner and caches its real witness without a fallible/await gap. Staging
    /// alone is never acceptance, append durability or provider continuation.
    pub(in crate::runtime::agent) fn retain_completion_group_transcript(
        &mut self,
        turn_id: &str,
    ) -> Result<CompletionTranscriptOwnership> {
        self.stage_completion_group_transcript(turn_id)?;
        let group = self
            .agent
            .completion_settlements
            .groups
            .get_mut(turn_id)
            .ok_or_else(|| MezError::invalid_state("completion handoff owner disappeared"))?;
        if let CompletionTranscriptTarget::NoPersistencePolicy(policy) = &group.transcript {
            return Ok(CompletionTranscriptOwnership::NoPersistencePolicy(*policy));
        }
        let handoff = group
            .handoff
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("completion snapshot was not installed"))?;
        if let Some(witness) = &handoff.witness {
            return Ok(CompletionTranscriptOwnership::Candidate(witness.clone()));
        }
        let accepted = self
            .persistence
            .retain_completion_bookkeeping_candidate(handoff.candidate.clone())?;
        // The mutable source owner was acquired before producer acceptance. No
        // fallible lookup, I/O, callback or await separates acceptance and cache.
        handoff.witness = Some(accepted.witness().clone());
        if !self.persistence.transcript_uses_adapter() {
            self.complete_completion_candidate_inline(accepted.witness().generation())?;
        }
        Ok(CompletionTranscriptOwnership::Candidate(
            accepted.witness().clone(),
        ))
    }

    /// Distinguishes actual archive ownership from intentional runtime-only
    /// policy. Legacy persist_* Ok(0) is never consulted as transfer evidence.
    pub(in crate::runtime::agent) fn completion_archive_handoff_owned(
        &self,
        turn_id: &str,
    ) -> bool {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .and_then(|group| group.handoff.as_ref())
            .is_some_and(|handoff| handoff.witness.is_some())
    }

    /// Existing groups may continue only from settled canonical execution and
    /// publication. Initial provider work without a completion group is separate.
    fn completion_continuation_source_ready(&self, turn_id: &str) -> bool {
        if !self.completion_group_publication_ready(turn_id) {
            return false;
        }
        let Some(group) = self.agent.completion_settlements.groups.get(turn_id) else {
            return true;
        };
        let Some(execution) = self.agent_turn_executions().get(turn_id) else {
            return true;
        };
        runtime_execution_ready_for_provider_continuation(execution)
            || ((group.human_resume_generation.is_some()
                || self.agent.agent_turn_interaction_kinds.get(turn_id)
                    == Some(&mez_agent::ModelInteractionKind::RoutedFailureExplanation))
                && execution
                    .action_results
                    .iter()
                    .all(ActionResult::is_terminal))
    }

    /// Records the existing exact pause/resume contract after caller validation.
    /// This does not approve an action, reset a failure budget or relabel errors.
    pub(in crate::runtime::agent) fn record_completion_human_resume(
        &mut self,
        turn_id: &str,
        generation: u64,
    ) -> Result<()> {
        let Some(group) = self.agent.completion_settlements.groups.get(turn_id) else {
            return Ok(());
        };
        if !self.human_pause_matches_completion_resume(&group.turn, generation) {
            return Err(MezError::conflict("completion resume owner changed"));
        }
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id) {
            group.human_resume_generation = Some(generation);
        }
        Ok(())
    }

    /// Stored execution eligibility for dependency wake and its liveness owner;
    /// initial provider work without stored execution is not a dependency wake.
    pub(crate) fn completion_stored_execution_continuation_ready(&self, turn_id: &str) -> bool {
        self.agent_turn_executions()
            .get(turn_id)
            .is_some_and(|execution| {
                if self.completion_owns_transcript_policy(turn_id) {
                    self.completion_continuation_source_ready(turn_id)
                } else {
                    runtime_execution_ready_for_provider_continuation(execution)
                }
            })
    }

    /// Read-only wake eligibility shared by dependency admission and its timer.
    /// Human pause blocks continuation, not original archive retention or paint.
    pub(crate) fn completion_provider_continuation_ready(&self, turn_id: &str) -> bool {
        !self.agent_turn_is_human_paused(turn_id)
            && self.completion_continuation_source_ready(turn_id)
    }

    /// Prepares only original archive/policy ownership; this does not insert work
    /// and is permitted while staging an explicit human resume under inhibition.
    pub(crate) fn prepare_completion_continuation_handoff(
        &mut self,
        turn_id: &str,
    ) -> Result<bool> {
        self.ensure_completion_required_gates_ready(turn_id)?;
        if !self.completion_continuation_source_ready(turn_id) {
            return Ok(false);
        }
        if !self.completion_owns_transcript_policy(turn_id) {
            return Ok(true);
        }
        if let Err(error) = self.retain_completion_group_transcript(turn_id) {
            self.record_completion_required_failure(
                turn_id,
                "continuation_handoff",
                None,
                None,
                &error,
            );
            return Err(error);
        }
        Ok(true)
    }

    /// Shared fallible boundary for callback, resume and scheduler continuation.
    /// Held paint leaves its existing receipt actionable without requeue replay.
    pub(crate) fn admit_completion_provider_continuation(&mut self, turn_id: &str) -> Result<bool> {
        if !self.completion_owns_transcript_policy(turn_id) {
            if !self.completion_provider_continuation_ready(turn_id)
                || !self.prepare_completion_continuation_handoff(turn_id)?
            {
                return Ok(false);
            }
            return Ok(self.queue_completion_provider_task(turn_id.to_string()));
        }
        if let Some(execution) = self.agent_turn_executions().get(turn_id).cloned() {
            let turn = self.completion_original_turn(turn_id).ok_or_else(|| {
                MezError::invalid_state("ordinary settlement original turn is unavailable")
            })?;
            if let Err(error) = self.ensure_completion_settlement_source_current(&turn, &execution)
            {
                self.record_completion_required_failure(
                    turn_id,
                    "settlement_source",
                    None,
                    None,
                    &error,
                );
                return Err(error);
            }
        }
        let scope = self.begin_completion_scope(turn_id, CompletionScope::Callback);
        let result = (|| -> Result<bool> {
            self.stage_completion_group_transcript(turn_id)?;
            let intent = if self
                .agent_turn_executions()
                .get(turn_id)
                .is_some_and(|execution| execution.terminal_state == AgentTurnState::Failed)
            {
                CompletionSettlementIntent::Failure
            } else {
                CompletionSettlementIntent::Ordinary
            };
            self.retain_completion_settlement_intent(turn_id, intent);
            if !self.completion_settlement_binding_current(turn_id)
                || !self.completion_provider_continuation_ready(turn_id)
                || self.completion_settlement_publication_pending(turn_id)
                || !self.prepare_completion_continuation_handoff(turn_id)?
            {
                return Ok(false);
            }
            let queued = self.queue_completion_provider_task(turn_id.to_string());
            if queued {
                self.clear_completion_settlement_intent(turn_id);
            }
            Ok(queued)
        })();
        let result = result.map_err(|error| {
            self.retain_completion_settlement_error(turn_id, "ordinary_continuation", error)
        });
        self.end_completion_scope(scope, "ordinary_continuation", false, result)
    }

    /// Records an actual pending-provider insertion and its group milestone in
    /// one infallible actor mutation. False remains an unchanged domain result,
    /// never an acknowledgement fabricated for a duplicate or blocked gate.
    pub(in crate::runtime::agent) fn queue_completion_provider_task(
        &mut self,
        turn_id: String,
    ) -> bool {
        if self.agent.completion_settlements.critical_failure.is_some() {
            return false;
        }
        if self
            .agent
            .completion_settlements
            .groups
            .contains_key(&turn_id)
            && !self.completion_group_publication_ready(&turn_id)
        {
            return false;
        }
        if self
            .agent
            .completion_settlements
            .groups
            .get(&turn_id)
            .is_some_and(|group| {
                matches!(
                    &group.transcript,
                    CompletionTranscriptTarget::Original { .. }
                ) && !self.completion_archive_handoff_owned(&turn_id)
            })
        {
            return false;
        }
        if self
            .agent
            .completion_settlements
            .groups
            .get(&turn_id)
            .is_some_and(|group| group.pending_settlement.is_some())
            && self.completion_settlement_publication_pending(&turn_id)
        {
            return false;
        }
        let (pending, settlements) = (
            &mut self.agent.pending_agent_provider_tasks,
            &mut self.agent.completion_settlements,
        );
        let group = settlements.groups.get_mut(&turn_id);
        if let Some(group) = group.as_ref()
            && (group.primary_failure.is_some()
                || group.paint_revoked
                || group.continuation == CompletionMilestone::Entered)
        {
            return false;
        }
        if let Some(group) = group {
            if group.continuation == CompletionMilestone::NotEntered {
                if pending.contains(&turn_id) {
                    return false;
                }
                group.continuation = CompletionMilestone::Entered;
            }
            let changed = pending.insert(turn_id);
            if changed {
                group.continuation = CompletionMilestone::Accepted;
            }
            changed
        } else {
            pending.insert(turn_id)
        }
    }

    /// Observes only the existing queue's boolean refusal/duplicate predicates.
    /// This bounded test diagnostic cannot clear, claim, admit or acknowledge
    /// anything; it exposes no IDs, source payloads, errors or environment data.
    #[cfg(test)]
    pub(crate) fn completion_provider_queue_flags_for_tests(
        &self,
        turn_id: &str,
    ) -> [(&'static str, bool); 12] {
        let group = self.agent.completion_settlements.groups.get(turn_id);
        [
            ("pending", self.agent_provider_task_is_pending(turn_id)),
            ("claimed", self.agent_provider_task_is_claimed(turn_id)),
            ("group", group.is_some()),
            (
                "critical_failure",
                self.agent.completion_settlements.critical_failure.is_some(),
            ),
            (
                "publication_ready",
                self.completion_group_publication_ready(turn_id),
            ),
            (
                "original_archive",
                group.is_some_and(|group| {
                    matches!(
                        &group.transcript,
                        CompletionTranscriptTarget::Original { .. }
                    )
                }),
            ),
            (
                "archive_handoff_owned",
                self.completion_archive_handoff_owned(turn_id),
            ),
            (
                "pending_settlement",
                group.is_some_and(|group| group.pending_settlement.is_some()),
            ),
            (
                "settlement_publication_pending",
                self.completion_settlement_publication_pending(turn_id),
            ),
            (
                "primary_failure",
                group.is_some_and(|group| group.primary_failure.is_some()),
            ),
            (
                "paint_revoked",
                group.is_some_and(|group| group.paint_revoked),
            ),
            (
                "continuation_entered",
                group.is_some_and(|group| group.continuation == CompletionMilestone::Entered),
            ),
        ]
    }

    /// Defers NEW dispatch inside a terminal task-result/scheduler boundary;
    /// entered milestones are not success and are not paint-recovery work.
    pub(crate) fn completion_finalization_holds_new_work(&self) -> bool {
        self.agent
            .completion_settlements
            .groups
            .values()
            .any(|group| {
                group.task_result == CompletionMilestone::Entered
                    || group.scheduler == CompletionMilestone::Entered
            })
    }

    /// Validates provider readiness without consuming original completion
    /// backing during fallible or asynchronous request preparation.
    pub(in crate::runtime::agent) fn ensure_completion_provider_claim_ready(
        &self,
        turn_id: &str,
    ) -> Result<()> {
        let Some(group) = self.agent.completion_settlements.groups.get(turn_id) else {
            return Ok(());
        };
        if let Some(failure) = &group.primary_failure {
            return Err((*failure.error).clone());
        }
        if self.completion_provider_claim_disposition(turn_id)?
            == CompletionProviderClaimDisposition::RetainedManagedAssessment
        {
            // Existing preterminal analysis retains every group/receipt/backing
            // obligation. It is not an Accepted ordinary continuation or a
            // terminal archive handoff, and never discharges either one.
            self.ensure_completion_required_gates_ready(turn_id)?;
            return Ok(());
        }
        if matches!(
            &group.transcript,
            CompletionTranscriptTarget::Original { .. }
        ) && !self.completion_archive_handoff_owned(turn_id)
        {
            return Err(MezError::invalid_state(
                "completion original transcript handoff is not owned",
            ));
        }
        let actions_owned = !self.completion_group_publication_ready(turn_id)
            || (group.pending_settlement.is_some()
                && self.completion_settlement_publication_pending(turn_id))
            || self
                .agent
                .pending_native_shell_dispatches
                .keys()
                .any(|(owner, _)| owner == turn_id)
            || self
                .agent
                .claimed_native_shell_dispatches
                .keys()
                .any(|(owner, _)| owner == turn_id);
        if group.continuation != CompletionMilestone::Accepted
            || group.paint_revoked
            || actions_owned
        {
            return Err(MezError::invalid_state(
                "completion group has not transferred provider continuation",
            ));
        }
        Ok(())
    }

    /// A known retained delivery must never be rewritten as an issued-unknown
    /// result merely because projection failed. Compare the exact current claim
    /// generation, not any older observed phase of the same action.
    pub(in crate::runtime::agent) fn completion_current_claim_has_retained_outcome(
        &self,
        turn_id: &str,
        action_id: &str,
    ) -> bool {
        let identity = (turn_id.to_string(), action_id.to_string());
        let attempt = self
            .agent
            .claimed_approved_external_actions
            .get(&identity)
            .or_else(|| self.agent.claimed_native_shell_dispatches.get(&identity));
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .is_some_and(|group| {
                attempt.is_some_and(|attempt| {
                    group
                        .observed
                        .contains(&(action_id.to_string(), attempt.clone()))
                })
            })
    }

    /// Runs the unconditional safety prefix, classifies still-issued unknown
    /// effects while claim evidence exists, and tries the original-target handoff.
    /// Every failure moves backing into the same group before caller cleanup can
    /// erase indices. No worker, hook, audit or finalization effect is replayed.
    pub(in crate::runtime) fn prepare_completion_forced_retirement(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        self.request_agent_worker_cancellation_for_turn(&turn.turn_id);
        self.revoke_completion_paint(&turn.turn_id);
        self.retire_cancelled_mcp_leases_for_turn(&turn.turn_id);
        if !self
            .agent
            .completion_settlements
            .groups
            .contains_key(&turn.turn_id)
        {
            return Ok(());
        }
        if let Err(error) = self.retain_interrupted_worker_effects(turn) {
            self.record_completion_required_failure(
                &turn.turn_id,
                "forced_context",
                None,
                None,
                &error,
            );
        }
        let raw_pending = self
            .agent
            .completion_settlements
            .external
            .values()
            .any(|receipt| receipt.owner.turn_id == turn.turn_id && receipt.raw.is_some())
            || self
                .agent
                .completion_settlements
                .native
                .values()
                .any(|receipt| receipt.owner.turn_id == turn.turn_id && receipt.raw.is_some());
        if raw_pending && self.completion_group_failure(&turn.turn_id).is_none() {
            let error = MezError::invalid_state(
                "accepted completion still owns unprojected phase evidence",
            );
            self.record_completion_required_failure(
                &turn.turn_id,
                "forced_projection_pending",
                None,
                None,
                &error,
            );
        }
        if let Err(error) = self.retain_completion_group_transcript(&turn.turn_id) {
            self.record_completion_required_failure(
                &turn.turn_id,
                "forced_transcript_handoff",
                None,
                None,
                &error,
            );
        }
        if let Some(error) = self.completion_group_failure(&turn.turn_id) {
            let evidence = self.completion_settlement_evidence(&turn.turn_id)?;
            if let Some(group) = self
                .agent
                .completion_settlements
                .groups
                .get_mut(&turn.turn_id)
            {
                group.safe_failure_evidence = Some(evidence);
            }
            self.detach_failed_completion_backing(&turn.turn_id);
            self.retire_completion_worker_deliveries(&turn.turn_id);
            return Err(error);
        }
        self.retire_completion_worker_deliveries(&turn.turn_id);
        Ok(())
    }

    /// Removes only exact-turn delivery authority after outcomes/uncertainty
    /// have transferred or moved into failed backing. Every token is signalled
    /// first, independently of any preceding handoff error.
    pub(in crate::runtime::agent) fn retire_completion_worker_deliveries(&mut self, turn_id: &str) {
        self.request_agent_worker_cancellation_for_turn(turn_id);
        self.retire_cancelled_mcp_leases_for_turn(turn_id);
        self.agent
            .pending_approved_external_actions
            .retain(|(owner, _), _| owner != turn_id);
        self.agent
            .claimed_approved_external_actions
            .retain(|(owner, _), _| owner != turn_id);
        self.agent
            .approved_external_cancellations
            .retain(|(owner, _), _| owner != turn_id);
        self.agent
            .pending_native_shell_dispatches
            .retain(|(owner, _), _| owner != turn_id);
        self.agent
            .claimed_native_shell_dispatches
            .retain(|(owner, _), _| owner != turn_id);
    }

    /// A legacy transcript producer must not recapture a live/replacement
    /// target after this group froze its original store or no-save policy.
    pub(in crate::runtime::agent) fn completion_owns_transcript_policy(
        &self,
        turn_id: &str,
    ) -> bool {
        self.agent
            .completion_settlements
            .groups
            .contains_key(turn_id)
    }

    /// Returns the frozen original turn for cleanup, not a replacement shell's
    /// conversation or role metadata.
    pub(in crate::runtime::agent) fn completion_original_turn(
        &self,
        turn_id: &str,
    ) -> Option<AgentTurnRecord> {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .map(|group| group.turn.clone())
    }

    /// Performs only a group's remaining terminal milestones. Entry is recorded
    /// before each effectful boundary; an ambiguous prior entry never retries the
    /// whole finalizer. A producer witness is not gate acceptance or durability.
    pub(in crate::runtime::agent) fn completion_group_requires_terminal_finalize(
        &self,
        turn_id: &str,
    ) -> bool {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .is_some_and(|group| group.scheduler == CompletionMilestone::NotEntered)
    }

    /// Performs only the group's remaining entered/accepted milestones.
    pub(in crate::runtime::agent) fn finalize_completion_group(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
        reason: &str,
    ) -> Result<()> {
        if self.provider_refusal_is_armed(&turn.turn_id) {
            return self.finalize_completion_provider_refusal(turn, execution, reason);
        }
        self.ensure_completion_required_gates_ready(&turn.turn_id)?;
        let publication_pending =
            self.agent
                .completion_settlements
                .external
                .values()
                .any(|receipt| {
                    receipt.owner.turn_id == turn.turn_id
                        && (receipt.raw.is_some()
                            || receipt.phase != ExternalCompletionPhase::PublicationAccepted)
                })
                || self
                    .agent
                    .completion_settlements
                    .native
                    .values()
                    .any(|receipt| {
                        receipt.owner.turn_id == turn.turn_id
                            && (receipt.raw.is_some()
                                || !matches!(
                                    receipt.phase,
                                    NativeCompletionPhase::PublicationAccepted
                                        | NativeCompletionPhase::NextPhaseOwned
                                ))
                    });
        if publication_pending {
            return Err(MezError::invalid_state(
                "completion publication still owns group finalization",
            ));
        }
        let ownership = match self.retain_completion_group_transcript(&turn.turn_id) {
            Ok(ownership) => ownership,
            Err(error) => {
                self.record_completion_required_failure(
                    &turn.turn_id,
                    "transcript_handoff",
                    None,
                    None,
                    &error,
                );
                return Err(error);
            }
        };
        match ownership {
            CompletionTranscriptOwnership::Candidate(witness) => {
                if self
                    .agent
                    .completion_settlements
                    .groups
                    .get(&turn.turn_id)
                    .and_then(|group| group.handoff.as_ref())
                    .and_then(|handoff| handoff.witness.as_ref())
                    != Some(&witness)
                {
                    return Err(MezError::invalid_state(
                        "completion candidate witness is not owned by this group",
                    ));
                }
            }
            CompletionTranscriptOwnership::NoPersistencePolicy(policy) => {
                let matches = self.agent.completion_settlements.groups.get(&turn.turn_id).is_some_and(|group| {
                    matches!(&group.transcript, CompletionTranscriptTarget::NoPersistencePolicy(original) if *original == policy)
                });
                if !matches {
                    return Err(MezError::invalid_state(
                        "completion no-persistence policy changed",
                    ));
                }
            }
        }
        let task_result = self
            .agent
            .completion_settlements
            .groups
            .get(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("completion finalization owner is unavailable"))?
            .task_result;
        match task_result {
            CompletionMilestone::Entered => {
                return Err(MezError::invalid_state(
                    "terminal task-result acceptance is ambiguous; replay prohibited",
                ));
            }
            CompletionMilestone::Accepted => {}
            CompletionMilestone::NotEntered => {
                if let Some(group) = self
                    .agent
                    .completion_settlements
                    .groups
                    .get_mut(&turn.turn_id)
                {
                    group.task_result = CompletionMilestone::Entered;
                }
                if let Err(error) = self.emit_subagent_task_result_for_execution(turn, execution) {
                    self.record_completion_required_failure(
                        &turn.turn_id,
                        "terminal_task_result",
                        None,
                        None,
                        &error,
                    );
                    return Err(error);
                }
                // The outer retention scope prevents nested lifecycle helpers
                // from removing this group before the actual return is cached.
                let group = self
                    .agent
                    .completion_settlements
                    .groups
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("task-result milestone owner disappeared")
                    })?;
                group.task_result = CompletionMilestone::Accepted;
            }
        }
        let scheduler = self
            .agent
            .completion_settlements
            .groups
            .get(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("scheduler milestone owner is unavailable"))?
            .scheduler;
        match scheduler {
            CompletionMilestone::Entered => {
                return Err(MezError::invalid_state(
                    "terminal scheduler acceptance is ambiguous; replay prohibited",
                ));
            }
            CompletionMilestone::Accepted => return Ok(()),
            CompletionMilestone::NotEntered => {}
        }
        if let Some(group) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&turn.turn_id)
        {
            group.scheduler = CompletionMilestone::Entered;
        }
        if let Err(error) =
            self.complete_running_agent_turn_and_start_ready(turn, execution.terminal_state, reason)
        {
            self.record_completion_required_failure(
                &turn.turn_id,
                "terminal_scheduler",
                None,
                None,
                &error,
            );
            return Err(error);
        }
        let group = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("scheduler milestone owner disappeared"))?;
        group.scheduler = CompletionMilestone::Accepted;
        if let Err(error) = self.start_ready_agent_turns() {
            self.record_completion_required_failure(
                &turn.turn_id,
                "terminal_ready_work",
                None,
                None,
                &error,
            );
            return Err(error);
        }
        Ok(())
    }

    /// Normal finalization cannot clear a group's backing before required gates,
    /// publication and its actual transcript/task-result transfer. Forced cleanup
    /// is separately admitted only after cancellation and original-target handoff.
    pub(in crate::runtime::agent) fn ensure_completion_terminal_retirement(
        &self,
        turn_id: &str,
    ) -> Result<()> {
        let Some(group) = self.agent.completion_settlements.groups.get(turn_id) else {
            return Ok(());
        };
        if let Some(failure) = &group.primary_failure {
            return Err((*failure.error).clone());
        }
        let original_owned = match &group.transcript {
            CompletionTranscriptTarget::Original { .. } => {
                self.completion_archive_handoff_owned(turn_id)
            }
            CompletionTranscriptTarget::NoPersistencePolicy(_) => true,
        };
        let raw_pending = self
            .agent
            .completion_settlements
            .external
            .values()
            .any(|receipt| {
                receipt.owner.turn_id == turn_id
                    && (receipt.raw.is_some()
                        || (!group.paint_revoked
                            && receipt.phase != ExternalCompletionPhase::PublicationAccepted))
            })
            || self
                .agent
                .completion_settlements
                .native
                .values()
                .any(|receipt| {
                    receipt.owner.turn_id == turn_id
                        && (receipt.raw.is_some()
                            || (!group.paint_revoked
                                && !matches!(
                                    receipt.phase,
                                    NativeCompletionPhase::PublicationAccepted
                                        | NativeCompletionPhase::NextPhaseOwned
                                )))
                });
        if !original_owned || raw_pending || group.failed_backing.is_some() {
            return Err(MezError::invalid_state(
                "completion group has not transferred its retirement evidence",
            ));
        }
        if !group.paint_revoked
            && (group.task_result != CompletionMilestone::Accepted
                || group.scheduler == CompletionMilestone::NotEntered)
        {
            return Err(MezError::invalid_state(
                "completion group finalization milestone is unresolved",
            ));
        }
        Ok(())
    }

    /// Latches a shared-settlement failure while preserving live delivery and
    /// canonical ownership for already-issued callbacks. Forced retirement is
    /// the separate boundary that cancels exact workers and moves backing before
    /// destructive cleanup; an archive/audit error alone must not make them stale.
    pub(in crate::runtime::agent) fn retain_completion_settlement_error(
        &mut self,
        turn_id: &str,
        stage: &'static str,
        error: MezError,
    ) -> MezError {
        self.record_completion_required_failure(turn_id, stage, None, None, &error);
        if let Ok(evidence) = self.completion_settlement_evidence(turn_id)
            && let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id)
        {
            group.safe_failure_evidence = Some(evidence);
        }
        self.completion_group_failure(turn_id).unwrap_or(error)
    }

    /// Drops only a successfully transferred terminal/forced group. Failed
    /// owners stay bounded behind the critical admission fence; no timer treats
    /// them as paint work. Current callback receipts may outlive backing only
    /// while the same group holds an actual immutable candidate witness.
    pub(in crate::runtime::agent) fn retire_completion_group_after_transfer(
        &mut self,
        turn_id: &str,
    ) {
        let Some(group) = self.agent.completion_settlements.groups.get(turn_id) else {
            return;
        };
        if group.scope.is_some()
            || group.primary_failure.is_some()
            || group.failed_backing.is_some()
            || self.agent_turn_executions().contains_key(turn_id)
            || (!group.paint_revoked && group.scheduler != CompletionMilestone::Accepted)
        {
            return;
        }
        let group_id = group.group.clone();
        self.agent
            .completion_settlements
            .external
            .retain(|_, receipt| {
                receipt.owner.turn_id != turn_id || receipt.owner.group != group_id
            });
        self.agent
            .completion_settlements
            .native
            .retain(|_, receipt| {
                receipt.owner.turn_id != turn_id || receipt.owner.group != group_id
            });
        self.agent.completion_settlements.groups.remove(turn_id);
    }

    /// Captures existing groups for the caller's selected panes in stable turn
    /// order, before any fallible suffix. Newly admitted groups are not added.
    /// This does not select turns for cancellation or validate live components.
    pub(in crate::runtime) fn begin_completion_scopes_for_panes(
        &mut self,
        pane_ids: &BTreeSet<&str>,
        scope: CompletionScope,
    ) -> CompletionScopeSet {
        let turn_ids = self
            .agent
            .completion_settlements
            .groups
            .iter()
            .filter(|(_, group)| pane_ids.contains(group.turn.pane_id.as_str()))
            .map(|(turn_id, _)| turn_id.clone())
            .collect::<Vec<_>>();
        CompletionScopeSet(
            turn_ids
                .into_iter()
                .map(|turn_id| self.begin_completion_scope(&turn_id, scope))
                .collect(),
        )
    }

    /// Closes every original observation without destructive error cleanup.
    /// Operation failure stays aggregate primary; otherwise exact required
    /// failures outrank mismatches. Each group sees only the operation's error,
    /// never another group's derived primary. No close short-circuits the rest.
    pub(in crate::runtime) fn end_completion_scopes<T>(
        &mut self,
        observations: CompletionScopeSet,
        stage: &'static str,
        result: Result<T>,
    ) -> Result<T> {
        let operation_error = result.as_ref().err().cloned();
        let mut required_failure = None;
        let mut mismatch = None;
        for observation in observations.0 {
            if operation_error.is_none() && required_failure.is_none() {
                required_failure = observation.owner.as_ref().and_then(|owner| {
                    self.agent
                        .completion_settlements
                        .groups
                        .get(&owner.turn.turn_id)
                        .filter(|group| owner.matches(group))
                        .and_then(|group| group.primary_failure.as_ref())
                        .map(|failure| (*failure.error).clone())
                });
            }
            let own_result = operation_error.clone().map_or(Ok(()), Err);
            if let Err(error) = self.end_completion_scope(observation, stage, false, own_result)
                && operation_error.is_none()
                && mismatch.is_none()
            {
                mismatch = Some(error);
            }
        }
        match result {
            Err(error) => Err(error),
            Ok(value) => match required_failure.or(mismatch) {
                Some(error) => Err(error),
                None => Ok(value),
            },
        }
    }

    /// Begins an outer retention boundary only after its caller validated the
    /// actual turn/claim authority. Delegated finish/cleanup never replaces it.
    pub(in crate::runtime) fn begin_completion_scope(
        &mut self,
        turn_id: &str,
        scope: CompletionScope,
    ) -> CompletionScopeObservation {
        let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id) else {
            return CompletionScopeObservation { owner: None };
        };
        let owns_scope = group.scope.is_none();
        let installed = *group.scope.get_or_insert(scope);
        CompletionScopeObservation {
            owner: Some(CompletionScopeOwner {
                identity: group.identity.clone(),
                turn: group.turn.clone(),
                group: group.group.clone(),
                installed,
                owns_scope,
            }),
        }
    }

    /// Ends a retention boundary after its complete fallible suffix. Required
    /// errors remain primary; a successful nested helper cannot retire the
    /// outer owner's snapshot before a later restore/checkpoint can still fail.
    /// Only the exact observed group is eligible for existing error cleanup;
    /// stale close preserves an operation error without mutating a replacement.
    pub(in crate::runtime) fn end_completion_scope<T>(
        &mut self,
        observation: CompletionScopeObservation,
        stage: &'static str,
        cleanup_on_error: bool,
        result: Result<T>,
    ) -> Result<T> {
        let Some(owner) = observation.owner else {
            return result;
        };
        let turn_id = owner.turn.turn_id.as_str();
        if !self
            .agent
            .completion_settlements
            .groups
            .get(turn_id)
            .is_some_and(|group| owner.matches(group))
        {
            return result.and_then(|_| {
                Err(MezError::invalid_state(
                    "completion retention scope owner changed before close",
                ))
            });
        }
        let result = match result {
            Ok(value) => Ok(value),
            Err(error) => {
                self.record_completion_required_failure(turn_id, stage, None, None, &error);
                if cleanup_on_error {
                    // Every exact token is signalled before any handoff error
                    // can return; the helper retains typed primary/secondary
                    // failures and moves backing instead of dropping payloads.
                    let _retained_failure = self.prepare_completion_forced_retirement(&owner.turn);
                }
                Err(self.completion_group_failure(turn_id).unwrap_or(error))
            }
        };
        if owner.owns_scope
            && let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id)
            && owner.matches(group)
        {
            group.scope = None;
        }
        if result.is_ok() {
            self.retire_completion_group_after_transfer(turn_id);
        }
        result
    }
}
