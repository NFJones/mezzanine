//! Filesystem-native canonical facts under the existing completion group owner.
//!
//! Queue admission freezes original transcript policy before presentation can
//! change. Exact actor attempts register issued/observed/context milestones and
//! retain raw evidence independently of cancellation. Projection updates only
//! the actual original execution and context, including moved failed backing.
//! Canonical/context and non-executable hook acceptance are NOT audit,
//! publication, archive-candidate or
//! disk durability acknowledgement. Those remaining gates must finish before
//! worker/attempt custody may transfer; this module never authorizes replay.

use super::*;
use crate::runtime::agent::native_filesystem::{NativeFilesystemAttempt, NativeFilesystemWork};
use crate::runtime::processes::native_patch::worker::NativePatchWorkerOutcome;
use std::sync::Arc;

mod hooks;
mod projection;
mod result;

/// Entered and accepted canonical gates, not a producer/durability receipt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum FilesystemCompletionPhase {
    /// Original accepted slot is retained; no dispatch was issued.
    Queued,
    /// Actual dispatch owns its fence, independently of producer start.
    Issued,
    /// Original immutable raw evidence is owned before projection.
    OutcomeRetained,
    /// Validation/projection entered; failure cannot replay that boundary.
    Projection,
    /// Original execution now owns terminal facts.
    CanonicalRetained,
    /// Chronology commit entered; acceptance is still unproven.
    ContextEntered,
    /// Canonical/context facts accepted; later required gates remain.
    ContextCommitted,
    /// Non-executable completed-hook classification entered, not acknowledged.
    HooksEntered,
    /// Original configured completion hooks were diagnosed without execution.
    HooksAccepted,
}

impl FilesystemCompletionPhase {
    /// Fixed public provenance; diagnostic bytes never become safe audit facts.
    fn stage(self) -> &'static str {
        match self {
            Self::Queued => "filesystem_queued",
            Self::Issued => "filesystem_issued",
            Self::OutcomeRetained => "filesystem_outcome_retained",
            Self::Projection => "filesystem_projection",
            Self::CanonicalRetained => "filesystem_canonical_retained",
            Self::ContextEntered => "filesystem_context_entered",
            Self::ContextCommitted => "filesystem_context_committed",
            Self::HooksEntered => "filesystem_hooks_entered",
            Self::HooksAccepted => "filesystem_hooks_accepted",
        }
    }
}

/// One actual original filesystem owner, never a shell or network receipt.
pub(super) struct FilesystemCompletionReceipt {
    /// Frozen canonical attribution shared with common group accounting.
    owner: ExternalCompletionOwner,
    /// Opaque actual actor allocation, checked independently of tuple labels.
    attempt: NativeFilesystemAttempt,
    /// Exact immutable admitted metadata until remaining transfer gates finish.
    work: Arc<NativeFilesystemWork>,
    /// Original raw result until canonical execution has accepted its facts.
    raw: Option<Arc<NativePatchWorkerOutcome>>,
    /// Actual entered/accepted projection boundary, not inferred from a result.
    phase: FilesystemCompletionPhase,
    /// Original completion definitions, independent of later config changes.
    completed_hooks: Vec<crate::integrations::hooks::HookDefinition>,
    /// Bounded semantic outcome captured with canonical facts, never a command.
    completed_hook_payload: Option<String>,
    /// First required failure; duplicates never reset it or replay the gate.
    failure: Option<Box<MezError>>,
}

impl fmt::Debug for FilesystemCompletionReceipt {
    /// Shows ownership and phase only, without patch, diff or diagnostic text.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FilesystemCompletionReceipt")
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

/// Validated unused receipt removed only in its same synchronous transfer.
/// This metadata permit must not be stored or carried across an await.
#[must_use = "prepared filesystem retirement must remain with its synchronous owner"]
pub(in crate::runtime::agent) struct UnissuedFilesystemCompletion(ExternalCompletionKey);

impl RuntimeSessionService {
    /// Reports entered phase for focused gate tests without changing any owner.
    #[cfg(test)]
    pub(crate) fn filesystem_completion_phase_for_tests(
        &self,
        attempt: &NativeFilesystemAttempt,
    ) -> Result<&'static str> {
        let key = self.filesystem_completion_key(attempt)?;
        Ok(self
            .agent
            .completion_settlements
            .filesystem
            .get(&key)
            .ok_or_else(|| MezError::invalid_state("filesystem test receipt unavailable"))?
            .phase
            .stage())
    }

    /// Reserves original group/action metadata while the accepted slot is live.
    /// This is queued ownership only; actual claim records issuance separately.
    pub(in crate::runtime::agent) fn reserve_filesystem_completion(
        &mut self,
        attempt: &NativeFilesystemAttempt,
        work: Arc<NativeFilesystemWork>,
    ) -> Result<()> {
        let key = (
            work.owner.turn.turn_id.clone(),
            work.owner.action.id.clone(),
            work.identity.transaction.clone(),
        );
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(&key.0)
            .ok_or_else(|| {
                MezError::invalid_state("filesystem original completion group unavailable")
            })?;
        if group.group != work.owner.group
            || !group.actions.iter().any(|(id, kind, ordinal)| {
                id == &key.1
                    && *kind == work.owner.action.action_type()
                    && *ordinal == work.owner.ordinal
            })
            || self
                .agent
                .completion_settlements
                .filesystem
                .contains_key(&key)
        {
            return Err(MezError::conflict(
                "filesystem completion differs from original admitted group",
            ));
        }
        let owner = ExternalCompletionOwner {
            turn_id: key.0.clone(),
            agent_id: work.owner.turn.agent_id.clone(),
            pane_id: work.owner.turn.pane_id.clone(),
            conversation_id: work.owner.turn.conversation_id.clone(),
            group: work.owner.group.clone(),
            action_id: key.1.clone(),
            action_type: work.owner.action.action_type().to_owned(),
            ordinal: work.owner.ordinal,
            attempt: key.2.clone(),
        };
        self.agent.completion_settlements.filesystem.insert(
            key,
            Box::new(FilesystemCompletionReceipt {
                owner,
                attempt: attempt.clone(),
                work,
                raw: None,
                phase: FilesystemCompletionPhase::Queued,
                completed_hooks: self
                    .integration
                    .hook_definitions()
                    .iter()
                    .filter(|definition| definition.event == HookEvent::PostShellCommand)
                    .cloned()
                    .collect(),
                completed_hook_payload: None,
                failure: None,
            }),
        );
        Ok(())
    }

    /// Finds only an actual opaque attempt; equal public tuple labels cannot.
    fn filesystem_completion_key(
        &self,
        attempt: &NativeFilesystemAttempt,
    ) -> Result<ExternalCompletionKey> {
        self.agent
            .completion_settlements
            .filesystem
            .iter()
            .find(|(_, receipt)| receipt.attempt == *attempt)
            .map(|(key, _)| key.clone())
            .ok_or_else(|| MezError::conflict("filesystem completion attempt unavailable"))
    }

    /// Records actual claim only after native dispatch owns its exact fence.
    pub(in crate::runtime::agent) fn filesystem_completion_issued(
        &mut self,
        attempt: &NativeFilesystemAttempt,
        work: &Arc<NativeFilesystemWork>,
    ) -> Result<()> {
        let key = self.filesystem_completion_key(attempt)?;
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get(&key)
            .ok_or_else(|| MezError::invalid_state("filesystem issued receipt disappeared"))?;
        if receipt.phase != FilesystemCompletionPhase::Queued || !Arc::ptr_eq(&receipt.work, work) {
            return Err(MezError::conflict(
                "filesystem issued original owner changed",
            ));
        }
        self.filesystem_completion_phase(&key, FilesystemCompletionPhase::Issued)?;
        self.record_completion_issued_attempt(&key.0, &key.1, &key.2)
    }

    /// Retains actual immutable evidence before marking the shared observation.
    /// The registry already validates exact original identity/allocation first.
    pub(in crate::runtime::agent) fn filesystem_completion_retained(
        &mut self,
        attempt: &NativeFilesystemAttempt,
        outcome: Arc<NativePatchWorkerOutcome>,
    ) -> Result<()> {
        let key = self.filesystem_completion_key(attempt)?;
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get_mut(&key)
            .ok_or_else(|| MezError::invalid_state("filesystem retained receipt disappeared"))?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        if receipt.phase != FilesystemCompletionPhase::Issued {
            return Ok(());
        }
        receipt.raw = Some(outcome);
        receipt.phase = FilesystemCompletionPhase::OutcomeRetained;
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(&key.0) {
            group.observed.insert((key.1, key.2));
            group.source_changed();
        }
        Ok(())
    }

    /// A queued, never-issued owner may transfer without implying any effects.
    pub(in crate::runtime::agent) fn prepare_unissued_filesystem_completion(
        &mut self,
        attempt: &NativeFilesystemAttempt,
    ) -> Result<UnissuedFilesystemCompletion> {
        let key = self.filesystem_completion_key(attempt)?;
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get(&key)
            .ok_or_else(|| MezError::invalid_state("filesystem unused receipt disappeared"))?;
        if receipt.phase != FilesystemCompletionPhase::Queued {
            return Err(MezError::conflict(
                "filesystem issued receipt cannot be discarded",
            ));
        }
        Ok(UnissuedFilesystemCompletion(key))
    }

    /// Completes only its prevalidated same-scope metadata transfer. No fallible
    /// operation follows removal of the actual unissued dispatch owner.
    pub(in crate::runtime::agent) fn discard_prepared_filesystem_completion(
        &mut self,
        reservation: UnissuedFilesystemCompletion,
    ) {
        self.agent
            .completion_settlements
            .filesystem
            .remove(&reservation.0);
    }

    /// Preserves first required-gate failure and common original-group custody.
    pub(in crate::runtime::agent) fn fail_filesystem_completion(
        &mut self,
        attempt: &NativeFilesystemAttempt,
        error: &MezError,
    ) {
        let Ok(key) = self.filesystem_completion_key(attempt) else {
            return;
        };
        let stage =
            if let Some(receipt) = self.agent.completion_settlements.filesystem.get_mut(&key) {
                receipt
                    .failure
                    .get_or_insert_with(|| Box::new(error.clone()));
                receipt.phase.stage()
            } else {
                return;
            };
        self.record_completion_required_failure(&key.0, stage, Some(&key.1), Some(&key.2), error);
    }

    /// Records an entered/accepted phase without erasing failed-boundary truth.
    fn filesystem_completion_phase(
        &mut self,
        key: &ExternalCompletionKey,
        phase: FilesystemCompletionPhase,
    ) -> Result<()> {
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get_mut(key)
            .ok_or_else(|| MezError::invalid_state("filesystem phase owner unavailable"))?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        receipt.phase = phase;
        if phase == FilesystemCompletionPhase::CanonicalRetained {
            receipt.raw = None;
        }
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(&key.0) {
            if phase == FilesystemCompletionPhase::ContextCommitted {
                group
                    .context_committed
                    .insert((key.1.clone(), key.2.clone()));
            }
            group.source_changed();
        }
        Ok(())
    }

    /// Filesystem canonical facts still own audit/publication/archive obligations.
    /// No current phase is a finished transfer; later integration supplies it.
    pub(in crate::runtime::agent) fn filesystem_completion_pending(&self, turn_id: &str) -> bool {
        self.agent
            .completion_settlements
            .filesystem
            .values()
            .any(|receipt| receipt.owner.turn_id == turn_id)
    }

    /// Reports issued/observed work that cannot be erased by forced retirement.
    pub(in crate::runtime::agent) fn filesystem_completion_issued_pending(
        &self,
        turn_id: &str,
    ) -> bool {
        self.agent
            .completion_settlements
            .filesystem
            .values()
            .any(|receipt| {
                receipt.owner.turn_id == turn_id
                    && receipt.phase != FilesystemCompletionPhase::Queued
            })
    }
}
