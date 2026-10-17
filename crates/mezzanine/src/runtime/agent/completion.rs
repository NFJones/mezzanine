//! Actor-owned transfer of already-completed external action outcomes.
//!
//! A receipt is reserved only for an admitted current execution/action/attempt.
//! The original outcome moves here before delivery authority is retired. Raw
//! payload is released only after canonical execution owns its terminal result;
//! subsequent phase records carry identity and diagnostics, not another result
//! chronology. This owner never dispatches actions, retries hooks/audit, grants
//! policy authority, schedules providers or invents native source evidence.
//!
//! This first slice is external-only. Native nonterminal handoff, forced terminal
//! retirement and receipt-owned idle recovery require their reviewed lifecycle
//! integration; they must not be approximated by replaying a completion callback.

use super::*;
use std::fmt;

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
#[derive(Debug, Default)]
pub(super) struct RuntimeCompletionSettlements {
    external: BTreeMap<ExternalCompletionKey, Box<ExternalCompletionReceipt>>,
}

impl RuntimeSessionService {
    /// Captures authoritative current group/action metadata before worker release.
    /// Missing or inconsistent group ownership fails before physical dispatch.
    pub(super) fn reserve_external_completion(
        &mut self,
        turn_id: &str,
        action_id: &str,
        attempt: &str,
    ) -> Result<()> {
        let owner = self.external_completion_owner(turn_id, action_id, attempt)?;
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
        self.agent.completion_settlements.external.insert(
            key,
            Box::new(ExternalCompletionReceipt {
                owner,
                phase: ExternalCompletionPhase::Issued,
                raw: None,
                failure: None,
                publication_failure: None,
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
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .ok_or_else(|| MezError::invalid_state("completion turn owner is unavailable"))?;
        if turn.state != AgentTurnState::Running
            && !(turn.state == AgentTurnState::Blocked
                && self
                    .agent
                    .agent_scheduler
                    .waiting_turns()
                    .any(|work| work.turn_id == turn_id))
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
        let current = self.external_completion_owner(turn_id, action_id, attempt);
        if !current.is_ok_and(|current| current == receipt.owner) {
            return Ok(Some(false));
        }
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
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
            return Err(error);
        }
        Ok(key)
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
        }
        Ok(())
    }

    /// Preserves the original typed failure and entered phase without replay.
    pub(super) fn fail_external_completion(
        &mut self,
        key: &ExternalCompletionKey,
        error: &MezError,
    ) {
        if let Some(receipt) = self.agent.completion_settlements.external.get_mut(key) {
            receipt
                .failure
                .get_or_insert_with(|| Box::new(error.clone()));
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
            .is_some_and(|(action, retained, text, _)| {
                action.id == owner.action_id
                    && action.action_type() == owner.action_type
                    && retained == result
                    && *text == result.content_text()
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
        Ok(())
    }
}
