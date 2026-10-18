//! Exact native worker outcome ownership, including nonterminal probe/read phases.
//!
//! The actor captures command/phase identity from its dispatch, not a callback's
//! labels. Raw output is retained before pending/claimed delivery is removed.
//! Projection failures keep that raw owner; canonical/context transfer or an
//! exact existing next-phase owner may release it. Nothing here launches work,
//! retries a command/hook, or treats capability/read output as tool success.

use super::*;
use crate::runtime::{RuntimeNativeShellDispatch, RuntimeNativeShellOutcome};

/// Exact actor marker generation within the original admitted group.
pub(in crate::runtime::agent) type NativeCompletionKey = ExternalCompletionKey;

/// Entered/accepted native stages, distinct from physical execution or durability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime::agent) enum NativeCompletionPhase {
    Issued,
    OutcomeRetained,
    Projection,
    CanonicalRetained,
    ContextCommitted,
    NextPhaseOwned,
    HooksEntered,
    HooksAccepted,
    PublicationHeld,
    PublicationAccepted,
}

impl NativeCompletionPhase {
    /// Fixed safe stage names for required failure provenance.
    fn stage_name(self) -> &'static str {
        match self {
            Self::Issued => "native_issued",
            Self::OutcomeRetained => "native_outcome_retained",
            Self::Projection => "native_projection",
            Self::CanonicalRetained => "native_canonical_retained",
            Self::ContextCommitted => "native_context_committed",
            Self::NextPhaseOwned => "native_next_phase_owned",
            Self::HooksEntered => "native_hooks_entered",
            Self::HooksAccepted => "native_hooks_accepted",
            Self::PublicationHeld => "native_publication_held",
            Self::PublicationAccepted => "native_publication_accepted",
        }
    }
}

/// One actual actor-issued native phase, never an inferred completed action.
pub(super) struct NativeCompletionReceipt {
    pub(super) owner: ExternalCompletionOwner,
    command_sha256: String,
    /// Original actor bytes until first delivery validation; never debugged.
    expected_command: Option<String>,
    started_at_unix_ms: u64,
    probe_only: bool,
    pub(super) phase: NativeCompletionPhase,
    pub(super) raw: Option<Box<RuntimeNativeShellOutcome>>,
    pub(super) failure: Option<Box<MezError>>,
    /// Optional diagnostics/installation never become required-gate failures.
    pub(super) publication_failure: Option<Box<MezError>>,
}

impl fmt::Debug for NativeCompletionReceipt {
    /// Do not print command/output/private diagnostics in generic runtime debug.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeCompletionReceipt")
            .field("owner", &self.owner)
            .field("probe_only", &self.probe_only)
            .field("phase", &self.phase)
            .field("has_raw", &self.raw.is_some())
            .field(
                "failure_kind",
                &self.failure.as_ref().map(|error| error.kind()),
            )
            .finish()
    }
}

impl RuntimeSessionService {
    /// Reserves from the existing actor dispatch before its exact claim is
    /// returned. Worker command/probe labels cannot select another phase later.
    pub(in crate::runtime::agent) fn reserve_native_completion(
        &mut self,
        dispatch: &RuntimeNativeShellDispatch,
    ) -> Result<()> {
        let owner = self.external_completion_owner(
            &dispatch.turn_id,
            &dispatch.action_id,
            &dispatch.marker,
        )?;
        self.reserve_completion_group(&dispatch.turn_id, &owner.group)?;
        let key = (
            dispatch.turn_id.clone(),
            dispatch.action_id.clone(),
            dispatch.marker.clone(),
        );
        if self.agent.completion_settlements.native.contains_key(&key) {
            return Err(MezError::invalid_state(
                "native phase already has a completion reservation",
            ));
        }
        self.record_completion_issued_attempt(
            &dispatch.turn_id,
            &dispatch.action_id,
            &dispatch.marker,
        )?;
        self.agent.completion_settlements.native.insert(
            key,
            Box::new(NativeCompletionReceipt {
                owner,
                command_sha256: exact_command_sha256(&dispatch.request.transaction.command),
                expected_command: Some(dispatch.request.transaction.command.clone()),
                started_at_unix_ms: dispatch.started_at_unix_ms,
                probe_only: dispatch.capability_probe_only,
                phase: NativeCompletionPhase::Issued,
                raw: None,
                failure: None,
                publication_failure: None,
            }),
        );
        Ok(())
    }

    /// Checks original turn/CID/group/action/ordinal/marker ownership before
    /// accepting a new delivery; never relies on a public marker alone.
    pub(in crate::runtime::agent) fn issued_native_completion_is_current(
        &self,
        key: &NativeCompletionKey,
    ) -> bool {
        self.agent
            .completion_settlements
            .native
            .get(key)
            .is_some_and(|receipt| {
                receipt.phase == NativeCompletionPhase::Issued
                    && self
                        .external_completion_owner(&key.0, &key.1, &key.2)
                        .is_ok_and(|current| current == receipt.owner)
            })
    }

    /// Exact retained duplicates do not repeat projection, native phases or
    /// gates. An original required error stays typed and fatal, never success.
    pub(in crate::runtime::agent) fn native_completion_duplicate(
        &self,
        key: &NativeCompletionKey,
    ) -> Result<Option<bool>> {
        let Some(receipt) = self.agent.completion_settlements.native.get(key) else {
            return Ok(None);
        };
        if receipt.phase == NativeCompletionPhase::Issued {
            return Ok(None);
        }
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        Ok(Some(false))
    }

    /// Owns actual raw payload before claim retirement. Projection identity
    /// failure retains private output here without adopting it as tool output.
    pub(in crate::runtime::agent) fn retain_native_completion(
        &mut self,
        outcome: RuntimeNativeShellOutcome,
    ) -> Result<NativeCompletionKey> {
        let key = (
            outcome.turn_id.clone(),
            outcome.action_id.clone(),
            outcome.marker.clone(),
        );
        let receipt = self
            .agent
            .completion_settlements
            .native
            .get_mut(&key)
            .ok_or_else(|| MezError::invalid_state("native outcome has no admitted receipt"))?;
        if receipt.phase != NativeCompletionPhase::Issued {
            return Err(MezError::invalid_state(
                "native outcome is already retained",
            ));
        }
        let identity_matches = outcome.capability_probe_only == receipt.probe_only
            && outcome.started_at_unix_ms == receipt.started_at_unix_ms
            && exact_command_sha256(&outcome.command) == receipt.command_sha256
            && receipt.expected_command.as_deref() == Some(outcome.command.as_str());
        receipt.raw = Some(Box::new(outcome));
        receipt.phase = NativeCompletionPhase::OutcomeRetained;
        if identity_matches {
            receipt.expected_command = None;
        }
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(&key.0) {
            group.observed.insert((key.1.clone(), key.2.clone()));
            group.source_changed();
        }
        if !identity_matches {
            let error =
                MezError::invalid_state("native outcome differs from the actor-issued phase");
            self.fail_native_completion(&key, &error);
            return Err(error);
        }
        Ok(key)
    }

    /// Borrows the retained phase, so fallible projection cannot consume it.
    pub(in crate::runtime::agent) fn retained_native_outcome(
        &self,
        key: &NativeCompletionKey,
    ) -> Result<&RuntimeNativeShellOutcome> {
        self.agent
            .completion_settlements
            .native
            .get(key)
            .and_then(|receipt| receipt.raw.as_deref())
            .ok_or_else(|| MezError::invalid_state("retained native outcome is unavailable"))
    }

    /// Records one native phase transfer; only actual canonical/context or
    /// nonterminal next-owner establishment permits releasing the raw payload.
    pub(in crate::runtime::agent) fn native_completion_phase(
        &mut self,
        key: &NativeCompletionKey,
        phase: NativeCompletionPhase,
    ) -> Result<()> {
        let receipt = self
            .agent
            .completion_settlements
            .native
            .get_mut(key)
            .ok_or_else(|| MezError::invalid_state("native completion owner is unavailable"))?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        receipt.phase = phase;
        if matches!(
            phase,
            NativeCompletionPhase::CanonicalRetained | NativeCompletionPhase::NextPhaseOwned
        ) {
            receipt.raw = None;
        }
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(&key.0) {
            if phase == NativeCompletionPhase::ContextCommitted {
                group
                    .context_committed
                    .insert((key.1.clone(), key.2.clone()));
            }
            group.source_changed();
        }
        Ok(())
    }

    /// Checks actual immutable native RESULT acceptance or its install ack.
    pub(in crate::runtime::agent) fn native_result_publication_is_owned(
        &self,
        key: &NativeCompletionKey,
        action: &mez_agent::AgentAction,
        result: &ActionResult,
        text: &str,
        raw_diagnostic: bool,
    ) -> bool {
        let Some(receipt) = self.agent.completion_settlements.native.get(key) else { return false; };
        let owner = &receipt.owner;
        let source_key = (owner.pane_id.clone(), owner.turn_id.clone(), owner.conversation_id.clone(),
            owner.group.clone(), owner.ordinal);
        self.presentation.agent_queued_provider_results.get(&source_key)
            .is_some_and(|(accepted_action, accepted_result, accepted_text, _, transaction, raw)| {
                accepted_action == action && accepted_result == result && accepted_text == text
                    && transaction.as_deref() == Some(key.2.as_str())
                    && *raw == raw_diagnostic
            }) || self.presentation.agent_published_provider_results.contains(&(source_key, text.to_string()))
    }

    /// Retains optional paint diagnostics without changing next-phase authority
    /// or declaring a required completion failure.
    pub(in crate::runtime::agent) fn note_native_publication_error(
        &mut self,
        key: &NativeCompletionKey,
        result: Result<()>,
    ) {
        if let Err(error) = result
            && let Some(receipt) = self.agent.completion_settlements.native.get_mut(key)
        {
            receipt.publication_failure = Some(Box::new(error));
        }
    }

    /// Retains original typed failure and finite stage provenance without retry.
    pub(in crate::runtime::agent) fn fail_native_completion(
        &mut self,
        key: &NativeCompletionKey,
        error: &MezError,
    ) {
        let stage = if let Some(receipt) = self.agent.completion_settlements.native.get_mut(key) {
            receipt
                .failure
                .get_or_insert_with(|| Box::new(error.clone()));
            Some(receipt.phase.stage_name())
        } else {
            None
        };
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

    /// Retires only an unused stale reservation, never a retained native result.
    pub(in crate::runtime::agent) fn discard_issued_native_completion(
        &mut self,
        key: &NativeCompletionKey,
    ) {
        if self
            .agent
            .completion_settlements
            .native
            .get(key)
            .is_some_and(|receipt| receipt.phase == NativeCompletionPhase::Issued)
        {
            self.agent.completion_settlements.native.remove(key);
            if let Some(group) = self.agent.completion_settlements.groups.get_mut(&key.0) {
                group.issued.remove(&(key.1.clone(), key.2.clone()));
            }
        }
    }

    /// Accepts only a concrete replacement dispatch or the existing exact
    /// human-pause/dependency barrier after a probe. A Running label alone never
    /// proves that an unowned next phase may be launched or fabricated terminal.
    pub(in crate::runtime::agent) fn accept_native_next_phase_owner(
        &mut self,
        key: &NativeCompletionKey,
    ) -> Result<()> {
        let dispatch_owned = self
            .agent
            .pending_native_shell_dispatches
            .get(&(key.0.clone(), key.1.clone()))
            .is_some_and(|dispatch| dispatch.marker != key.2);
        let paused = self.agent_turn_is_human_paused(&key.0);
        let waiting = self
            .agent_turn_ledger()
            .turn(&key.0)
            .is_some_and(|turn| turn.state == AgentTurnState::Blocked)
            && self
                .agent
                .agent_scheduler
                .waiting_turns()
                .any(|work| work.turn_id == key.0);
        if !dispatch_owned && !paused && !waiting {
            return Err(MezError::invalid_state(
                "native probe has no exact next-phase owner",
            ));
        }
        self.native_completion_phase(key, NativeCompletionPhase::NextPhaseOwned)
    }
}
