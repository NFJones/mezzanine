//! Actor-owned transfer and bounded retirement of completed action outcomes.
//!
//! A receipt is reserved only for an admitted current execution/action/attempt.
//! The original outcome moves here before delivery authority is retired. Raw
//! payload is released only after canonical execution owns its terminal result;
//! subsequent phase records carry identity and diagnostics, not another result
//! chronology. This owner never dispatches actions, retries hooks/audit, grants
//! policy authority, schedules providers or invents native source evidence.
//!
//! External and native ingress share the original group retirement owner.
//! Publication-only recovery never re-enters outcome projection or required
//! gates; native nonterminal handoff stays with its exact next-phase owner.

use super::*;
use std::fmt;

mod retirement;
use retirement::CompletionGroup;
pub(in crate::runtime) use retirement::CompletionScope;
mod native;
use native::NativeCompletionReceipt;
pub(super) use native::{NativeCompletionKey, NativeCompletionPhase};
mod claim;
mod publication;
use claim::CompletionProviderClaimDisposition;
mod settlement;
use settlement::CompletionFailureAudit;
pub(crate) use settlement::CompletionSettlementDisposition;
pub(in crate::runtime::agent) use settlement::CompletionSettlementIntent;

/// Exact actor-issued delivery generation, distinct from presentation text/IDs.
pub(super) type ExternalCompletionKey = (String, String, String);

/// Entered/accepted stages are deliberately distinct for nonidempotent gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExternalCompletionPhase {
    Issued,
    OutcomeRetained,
    Projection,
    CanonicalRetained,
    ContextEntered,
    ContextCommitted,
    AuditEntered,
    AuditAccepted,
    HooksEntered,
    HooksAccepted,
    ProgressPublication,
    ResultPublication,
    TracePublication,
    PublicationHeld,
    PublicationAccepted,
}

impl ExternalCompletionPhase {
    /// Fixed safe provenance for required failure evidence, never error text.
    fn stage_name(self) -> &'static str {
        match self {
            Self::Issued => "external_issued",
            Self::OutcomeRetained => "external_outcome_retained",
            Self::Projection => "external_projection",
            Self::CanonicalRetained => "external_canonical_retained",
            Self::ContextEntered => "external_context_entered",
            Self::ContextCommitted => "external_context_committed",
            Self::AuditEntered => "external_audit_entered",
            Self::AuditAccepted => "external_audit_accepted",
            Self::HooksEntered => "external_hooks_entered",
            Self::HooksAccepted => "external_hooks_accepted",
            Self::ProgressPublication => "external_progress_publication",
            Self::ResultPublication => "external_result_publication",
            Self::TracePublication => "external_trace_publication",
            Self::PublicationHeld => "external_publication_held",
            Self::PublicationAccepted => "external_publication_accepted",
        }
    }
}

/// Frozen original actor binding; matching a public pane/CID alone is insufficient.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalCompletionOwner {
    turn_id: String,
    agent_id: String,
    pane_id: String,
    conversation_id: String,
    group: mez_agent::ContextExecutionGroupId,
    action_id: String,
    action_type: String,
    ordinal: usize,
    attempt: String,
}

/// One current admitted attempt, with raw outcome only until canonical transfer.
struct ExternalCompletionReceipt {
    owner: ExternalCompletionOwner,
    phase: ExternalCompletionPhase,
    raw: Option<Box<RuntimeApprovedExternalActionOutcome>>,
    /// Original typed diagnostic clone preserves policy/effect flags exactly.
    failure: Option<Box<MezError>>,
    /// Optional installation failure is not required-gate acceptance/failure.
    publication_failure: Option<Box<MezError>>,
    /// Trace is optional and attempted once; entered is not an install receipt.
    trace_attempted: bool,
}

impl fmt::Debug for ExternalCompletionReceipt {
    /// Redacts payload/error text from generic component debug formatting.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExternalCompletionReceipt")
            .field("owner", &self.owner)
            .field("phase", &self.phase)
            .field("has_raw", &self.raw.is_some())
            .field(
                "failure_kind",
                &self.failure.as_ref().map(|error| error.kind()),
            )
            .finish()
    }
}

/// Receipts belong to existing admitted active execution slots, not history.
#[derive(Default)]
pub(super) struct RuntimeCompletionSettlements {
    external: BTreeMap<ExternalCompletionKey, Box<ExternalCompletionReceipt>>,
    native: BTreeMap<NativeCompletionKey, Box<NativeCompletionReceipt>>,
    /// Active execution slots only; a required failure fences new work before
    /// failed owners could accumulate into a historical completion archive.
    groups: BTreeMap<String, Box<CompletionGroup>>,
    /// First required failure, never an optional publication installation error.
    critical_failure: Option<Box<MezError>>,
    /// A nested lifecycle drain cannot enter the same publication pump twice.
    publication_pump_active: bool,
    /// Isolated lost-ack fault after the unchanged audit append returns.
    #[cfg(test)]
    reject_failure_audit_ack_for_tests: bool,
    /// Observational only: the actual admission error before refusal settlement.
    #[cfg(test)]
    provider_lease_refusal_for_tests: Option<(String, u64, MezError)>,
}

impl fmt::Debug for RuntimeCompletionSettlements {
    /// Retained full typed errors stay live but never enter generic debug text.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeCompletionSettlements")
            .field("external", &self.external)
            .field("native", &self.native)
            .field("groups", &self.groups)
            .field(
                "critical_failure_kind",
                &self.critical_failure.as_ref().map(|error| error.kind()),
            )
            .finish()
    }
}

impl RuntimeSessionService {
    /// Preserves a bounded optional diagnostic on an existing receipt without
    /// changing required failure truth or authorizing any publication replay.
    pub(crate) fn note_completion_optional_error(&mut self, turn_id: &str, result: Result<()>) {
        let Err(error) = result else {
            return;
        };
        if let Some(receipt) = self
            .agent
            .completion_settlements
            .external
            .values_mut()
            .find(|receipt| receipt.owner.turn_id == turn_id)
        {
            receipt.publication_failure = Some(Box::new(error));
        } else if let Some(receipt) = self
            .agent
            .completion_settlements
            .native
            .values_mut()
            .find(|receipt| receipt.owner.turn_id == turn_id)
        {
            receipt.publication_failure = Some(Box::new(error));
        } else if let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id) {
            group.publication_failure = Some(Box::new(error));
        }
    }

    /// A required original-archive failure fences NEW work while its existing
    /// immutable candidate remains owned; admitted callbacks/cancel/reap stay open.
    pub(in crate::runtime::agent) fn record_completion_archive_failure(
        &mut self,
        error: &MezError,
    ) {
        self.agent
            .completion_settlements
            .critical_failure
            .get_or_insert_with(|| Box::new(error.clone()));
    }

    /// Captures authoritative current group/action metadata before worker release.
    /// Missing or inconsistent group ownership fails before physical dispatch.
    pub(super) fn reserve_external_completion(
        &mut self,
        turn_id: &str,
        action_id: &str,
        attempt: &str,
    ) -> Result<()> {
        let owner = self.external_completion_owner(turn_id, action_id, attempt)?;
        self.reserve_completion_group(turn_id, &owner.group)?;
        if self
            .agent
            .completion_settlements
            .external
            .values()
            .any(|receipt| receipt.owner.turn_id == turn_id && receipt.owner.group != owner.group)
        {
            return Err(MezError::invalid_state(
                "previous execution completion still owns settlement",
            ));
        }
        let key = (
            turn_id.to_string(),
            action_id.to_string(),
            attempt.to_string(),
        );
        if self
            .agent
            .completion_settlements
            .external
            .contains_key(&key)
        {
            return Err(MezError::invalid_state(
                "external completion attempt is already reserved",
            ));
        }
        self.record_completion_issued_attempt(turn_id, action_id, attempt)?;
        self.agent.completion_settlements.external.insert(
            key,
            Box::new(ExternalCompletionReceipt {
                owner,
                phase: ExternalCompletionPhase::Issued,
                raw: None,
                failure: None,
                publication_failure: None,
                trace_attempted: false,
            }),
        );
        Ok(())
    }

    /// Builds a frozen owner from canonical actor state, never worker labels.
    fn external_completion_owner(
        &self,
        turn_id: &str,
        action_id: &str,
        attempt: &str,
    ) -> Result<ExternalCompletionOwner> {
        self.completion_owner(turn_id, action_id, attempt, false)
    }

    /// Paint may settle under an exact human pause; this does not admit a new
    /// worker delivery or permit continuation through that pause.
    fn completion_owner(
        &self,
        turn_id: &str,
        action_id: &str,
        attempt: &str,
        allow_human_pause: bool,
    ) -> Result<ExternalCompletionOwner> {
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .ok_or_else(|| MezError::invalid_state("completion turn owner is unavailable"))?;
        if turn.state != AgentTurnState::Running
            && !(turn.state == AgentTurnState::Blocked
                && ((allow_human_pause && self.agent_turn_is_human_paused(turn_id))
                    || self
                        .agent
                        .agent_scheduler
                        .waiting_turns()
                        .any(|work| work.turn_id == turn_id)))
        {
            return Err(MezError::invalid_state(
                "completion turn binding is retired",
            ));
        }
        let session = self.agent_shell_store().get(&turn.pane_id).ok_or_else(|| {
            MezError::invalid_state("completion conversation owner is unavailable")
        })?;
        if session.session_id != turn.conversation_id {
            return Err(MezError::invalid_state(
                "completion conversation binding changed",
            ));
        }
        let execution = self
            .agent_turn_executions()
            .get(turn_id)
            .ok_or_else(|| MezError::invalid_state("completion execution owner is unavailable"))?;
        let (ordinal, action) = execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| {
                batch
                    .actions
                    .iter()
                    .enumerate()
                    .find(|(_, action)| action.id == action_id)
            })
            .ok_or_else(|| MezError::invalid_state("completion action owner is unavailable"))?;
        let group = self
            .agent
            .agent_execution_groups_by_turn
            .get(turn_id)
            .and_then(|groups| groups.get(action_id))
            .cloned()
            .ok_or_else(|| MezError::invalid_state("completion canonical group is unavailable"))?;
        if super::provider_execution::provider_log_execution_group_id(execution)? != group {
            return Err(MezError::invalid_state(
                "completion execution group differs from canonical owner",
            ));
        }
        Ok(ExternalCompletionOwner {
            turn_id: turn_id.to_string(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            conversation_id: turn.conversation_id.clone(),
            group,
            action_id: action_id.to_string(),
            action_type: action.action_type().to_string(),
            ordinal,
            attempt: attempt.to_string(),
        })
    }

    /// Validates the full frozen issued owner before outcome acceptance.
    pub(super) fn issued_external_completion_is_current(
        &self,
        turn_id: &str,
        action_id: &str,
        attempt: &str,
    ) -> bool {
        let key = (
            turn_id.to_string(),
            action_id.to_string(),
            attempt.to_string(),
        );
        self.agent
            .completion_settlements
            .external
            .get(&key)
            .is_some_and(|receipt| {
                receipt.phase == ExternalCompletionPhase::Issued
                    && self
                        .external_completion_owner(turn_id, action_id, attempt)
                        .is_ok_and(|current| current == receipt.owner)
            })
    }

    /// Identifies whether a pending slot still belongs to this original group,
    /// so stale completion cleanup cannot remove a replacement group's work.
    pub(super) fn external_completion_pending_group_is_current(
        &self,
        turn_id: &str,
        action_id: &str,
        attempt: &str,
    ) -> bool {
        let key = (
            turn_id.to_string(),
            action_id.to_string(),
            attempt.to_string(),
        );
        self.agent
            .completion_settlements
            .external
            .get(&key)
            .is_some_and(|receipt| {
                self.agent
                    .agent_execution_groups_by_turn
                    .get(turn_id)
                    .and_then(|groups| groups.get(action_id))
                    == Some(&receipt.owner.group)
            })
    }

    /// Distinguishes an exact retained duplicate from a new delivery. Original
    /// mandatory failure remains an Err; no phase/effect is retried here.
    pub(super) fn external_completion_duplicate(
        &self,
        turn_id: &str,
        action_id: &str,
        attempt: &str,
    ) -> Result<Option<bool>> {
        let key = (
            turn_id.to_string(),
            action_id.to_string(),
            attempt.to_string(),
        );
        let Some(receipt) = self.agent.completion_settlements.external.get(&key) else {
            return Ok(None);
        };
        if receipt.phase == ExternalCompletionPhase::Issued {
            return Ok(None);
        }
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        let current = self.external_completion_owner(turn_id, action_id, attempt);
        if !current.is_ok_and(|current| current == receipt.owner) {
            return Ok(Some(false));
        }
        Ok(Some(false))
    }

    /// Moves a validated original outcome into its reserved slot. Once inserted,
    /// no fallible operation precedes caller retirement of matching claims.
    pub(super) fn retain_external_completion(
        &mut self,
        outcome: RuntimeApprovedExternalActionOutcome,
    ) -> Result<ExternalCompletionKey> {
        let key = (
            outcome.turn_id.clone(),
            outcome.action_id.clone(),
            outcome.attempt.clone(),
        );
        let current = self.external_completion_owner(&key.0, &key.1, &key.2);
        let receipt = self
            .agent
            .completion_settlements
            .external
            .get_mut(&key)
            .ok_or_else(|| MezError::invalid_state("external outcome has no admitted receipt"))?;
        if receipt.phase != ExternalCompletionPhase::Issued {
            return Err(MezError::invalid_state(
                "external outcome already has a retained settlement",
            ));
        }
        receipt.raw = Some(Box::new(outcome));
        receipt.phase = ExternalCompletionPhase::OutcomeRetained;
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(&key.0) {
            group.observed.insert((key.1.clone(), key.2.clone()));
            group.source_changed();
        }
        let validation = current.and_then(|current| {
            if current == receipt.owner {
                Ok(())
            } else {
                Err(MezError::invalid_state(
                    "external outcome differs from reserved execution owner",
                ))
            }
        });
        if let Err(error) = validation {
            receipt.failure = Some(Box::new(error.clone()));
            self.record_completion_required_failure(
                &key.0,
                ExternalCompletionPhase::OutcomeRetained.stage_name(),
                Some(&key.1),
                Some(&key.2),
                &error,
            );
            return Err(error);
        }
        Ok(key)
    }

    /// Returns raw result by borrow; failed projection cannot consume its owner.
    #[cfg(test)]
    pub(crate) fn retained_external_result_for_tests(
        &self,
        turn_id: &str,
        action_id: &str,
        attempt: &str,
    ) -> Option<ActionResult> {
        self.agent
            .completion_settlements
            .external
            .get(&(
                turn_id.to_string(),
                action_id.to_string(),
                attempt.to_string(),
            ))
            .and_then(|receipt| receipt.raw.as_ref())
            .and_then(|outcome| outcome.result.as_ref().ok())
            .cloned()
    }

    /// Returns raw result by borrow; failed projection cannot consume its owner.
    pub(super) fn retained_external_result(
        &self,
        key: &ExternalCompletionKey,
    ) -> Result<&Result<ActionResult>> {
        self.agent
            .completion_settlements
            .external
            .get(key)
            .and_then(|receipt| receipt.raw.as_ref())
            .map(|outcome| &outcome.result)
            .ok_or_else(|| MezError::invalid_state("retained external outcome is unavailable"))
    }

    /// Returns the transport exactly once, without changing canonical ownership.
    pub(super) fn take_retained_mcp_transport(
        &mut self,
        key: &ExternalCompletionKey,
    ) -> Option<(String, crate::runtime::RuntimeMcpTransport)> {
        self.agent
            .completion_settlements
            .external
            .get_mut(key)
            .and_then(|receipt| receipt.raw.as_mut())
            .and_then(|outcome| outcome.mcp_transport.take())
    }

    /// Records one entered/accepted phase; failed required gates never resume.
    pub(super) fn external_completion_phase(
        &mut self,
        key: &ExternalCompletionKey,
        phase: ExternalCompletionPhase,
    ) -> Result<()> {
        let receipt = self
            .agent
            .completion_settlements
            .external
            .get_mut(key)
            .ok_or_else(|| MezError::invalid_state("external settlement owner is unavailable"))?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        receipt.phase = phase;
        if phase == ExternalCompletionPhase::CanonicalRetained {
            receipt.raw = None;
            self.invalidate_completion_handoff(&key.0);
        }
        if phase == ExternalCompletionPhase::ContextCommitted
            && let Some(group) = self.agent.completion_settlements.groups.get_mut(&key.0)
        {
            group
                .context_committed
                .insert((key.1.clone(), key.2.clone()));
            group.source_changed();
        }
        Ok(())
    }

    /// Preserves the original typed failure and entered phase without replay.
    pub(super) fn fail_external_completion(
        &mut self,
        key: &ExternalCompletionKey,
        error: &MezError,
    ) {
        let mut stage = None;
        if let Some(receipt) = self.agent.completion_settlements.external.get_mut(key) {
            stage = Some(receipt.phase.stage_name());
            receipt
                .failure
                .get_or_insert_with(|| Box::new(error.clone()));
        }
        if let Some(stage) = stage {
            self.record_completion_required_failure(
                &key.0,
                stage,
                Some(&key.1),
                Some(&key.2),
                error,
            );
        }
    }

    /// Checks original full source ownership, not merely matching displayed text.
    pub(super) fn external_result_publication_is_retained(
        &self,
        key: &ExternalCompletionKey,
        result: &ActionResult,
    ) -> bool {
        let Some(receipt) = self.agent.completion_settlements.external.get(key) else {
            return false;
        };
        let owner = &receipt.owner;
        let source_key = (
            owner.pane_id.clone(),
            owner.turn_id.clone(),
            owner.conversation_id.clone(),
            owner.group.clone(),
            owner.ordinal,
        );
        self.presentation
            .agent_queued_provider_results
            .get(&source_key)
            .is_some_and(|(action, retained, text, _, transaction, raw)| {
                action.id == owner.action_id
                    && action.action_type() == owner.action_type
                    && retained == result
                    && *text == result.content_text()
                    && transaction.is_none()
                    && !raw
            })
    }

    /// Retains optional publication truth without replaying or erasing a gate Err.
    pub(super) fn hold_external_publication(
        &mut self,
        key: &ExternalCompletionKey,
        error: &MezError,
    ) -> Result<()> {
        let receipt = self
            .agent
            .completion_settlements
            .external
            .get_mut(key)
            .ok_or_else(|| {
                MezError::invalid_state("publication settlement owner is unavailable")
            })?;
        if let Some(required) = &receipt.failure {
            return Err((**required).clone());
        }
        receipt.publication_failure = Some(Box::new(error.clone()));
        receipt.phase = ExternalCompletionPhase::PublicationHeld;
        Ok(())
    }

    /// A settled sibling cannot schedule another provider across an earlier
    /// required completion gate, including when the sibling is native work.
    pub(crate) fn ensure_completion_required_gates_ready(&self, turn_id: &str) -> Result<()> {
        if let Some(error) = self.completion_group_failure(turn_id) {
            return Err(error);
        }
        if let Some(error) = self
            .agent
            .completion_settlements
            .external
            .values()
            .filter(|receipt| receipt.owner.turn_id == turn_id)
            .find_map(|receipt| receipt.failure.as_deref())
        {
            return Err(error.clone());
        }
        Ok(())
    }

    /// A settled sibling cannot schedule another provider across an earlier
    /// required failure whose already-observed outcome still owns an obligation.
    pub(super) fn external_group_required_failure(
        &self,
        key: &ExternalCompletionKey,
    ) -> Option<MezError> {
        let owner = &self.agent.completion_settlements.external.get(key)?.owner;
        self.agent
            .completion_settlements
            .external
            .values()
            .filter(|receipt| {
                receipt.owner.turn_id == owner.turn_id && receipt.owner.group == owner.group
            })
            .find_map(|receipt| receipt.failure.as_deref().cloned())
    }

    /// Retires only an unused reservation after no dispatch/outcome was accepted.
    pub(super) fn discard_issued_external_completion(
        &mut self,
        turn_id: &str,
        action_id: &str,
        attempt: &str,
    ) {
        let key = (
            turn_id.to_string(),
            action_id.to_string(),
            attempt.to_string(),
        );
        if self
            .agent
            .completion_settlements
            .external
            .get(&key)
            .is_some_and(|receipt| receipt.phase == ExternalCompletionPhase::Issued)
        {
            self.agent.completion_settlements.external.remove(&key);
            if let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id) {
                group
                    .issued
                    .remove(&(action_id.to_string(), attempt.to_string()));
            }
        }
    }

    /// Transfers settled per-action evidence back to the existing execution/context
    /// owner. Group lifecycle/forced retirement require their separate reviewed hook.
    pub(super) fn transfer_external_completion(
        &mut self,
        key: &ExternalCompletionKey,
    ) -> Result<()> {
        let receipt = self
            .agent
            .completion_settlements
            .external
            .get(key)
            .ok_or_else(|| MezError::invalid_state("external transfer owner is unavailable"))?;
        if receipt.phase != ExternalCompletionPhase::PublicationAccepted
            || receipt.failure.is_some()
        {
            return Err(MezError::invalid_state(
                "external completion is not ready for canonical transfer",
            ));
        }
        self.agent.completion_settlements.external.remove(key);
        self.retire_completion_group_after_transfer(&key.0);
        Ok(())
    }
}
