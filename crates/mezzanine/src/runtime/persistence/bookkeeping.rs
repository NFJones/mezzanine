//! Ordered candidate ownership before checked transcript append admission.
//!
//! Candidates survive terminal cleanup but are not receipts or logical rows.
//! Only one candidate per conversation may be checked at a time; a failed read
//! retains its candidate and fences subsequent history admission fail-closed.

use super::RuntimePersistenceComponent;
use crate::error::{MezError, Result};
use crate::runtime::RuntimeBookkeepingCandidate;
use sha2::{Digest, Sha256};

/// Producer-created proof of ownership of one exact immutable candidate.
/// It is neither checked archive acceptance, disk durability nor gate success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeCompletionCandidateWitness {
    generation: u64,
    snapshot_id: String,
    persistence_key: (String, String),
}

impl RuntimeCompletionCandidateWitness {
    /// Returns the generation actually owned by the existing candidate queue.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    /// Returns the exact canonical evidence digest, not merely a turn identity.
    pub(crate) fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }
}

/// Actual producer acceptance; changed evidence never reuses an earlier witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeCompletionCandidateAcceptance {
    Accepted(RuntimeCompletionCandidateWitness),
    AlreadyAccepted(RuntimeCompletionCandidateWitness),
}

impl RuntimeCompletionCandidateAcceptance {
    /// Extracts the same opaque witness from new or identical existing ownership.
    pub(crate) fn witness(&self) -> &RuntimeCompletionCandidateWitness {
        match self {
            Self::Accepted(witness) | Self::AlreadyAccepted(witness) => witness,
        }
    }
}

impl RuntimePersistenceComponent {
    /// Injects generation exhaustion only in isolated ownership tests.
    #[cfg(test)]
    pub(crate) fn set_bookkeeping_generation_for_tests(&mut self, generation: u64) {
        self.next_bookkeeping_generation = generation;
    }

    /// Retains a completion snapshot in the EXISTING candidate owner. The caller
    /// must cache the returned witness before any fallible/async boundary. No
    /// legacy void/zero result is interpreted as acceptance. Required validation,
    /// collision or exhaustion failure occurs before queue mutation.
    pub(crate) fn retain_completion_bookkeeping_candidate(
        &mut self,
        mut candidate: RuntimeBookkeepingCandidate,
    ) -> Result<RuntimeCompletionCandidateAcceptance> {
        candidate.read = candidate.read.freeze_completion();
        let bytes = candidate.completion_snapshot_bytes()?;
        let snapshot_id: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        for existing in self
            .bookkeeping_candidates
            .iter()
            .filter(|existing| existing.persistence_key == candidate.persistence_key)
        {
            let existing_bytes = existing.completion_snapshot_bytes()?;
            let existing_id: String = Sha256::digest(&existing_bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            if existing_id != snapshot_id {
                continue;
            }
            if existing_bytes != bytes {
                return Err(MezError::invalid_state(
                    "completion snapshot identity collision",
                ));
            }
            return Ok(RuntimeCompletionCandidateAcceptance::AlreadyAccepted(
                RuntimeCompletionCandidateWitness {
                    generation: existing.generation,
                    snapshot_id,
                    persistence_key: existing.persistence_key.clone(),
                },
            ));
        }
        let generation = self
            .next_bookkeeping_generation
            .checked_add(1)
            .ok_or_else(|| {
                MezError::invalid_state("completion bookkeeping generation exhausted")
            })?;
        if self.bookkeeping_claims.contains(&generation)
            || self
                .bookkeeping_candidates
                .iter()
                .any(|entry| entry.generation == generation)
        {
            return Err(MezError::invalid_state(
                "completion bookkeeping generation is already owned",
            ));
        }
        candidate.generation = generation;
        let witness = RuntimeCompletionCandidateWitness {
            generation,
            snapshot_id,
            persistence_key: candidate.persistence_key.clone(),
        };
        self.bookkeeping_candidates.push(candidate);
        self.next_bookkeeping_generation = generation;
        Ok(RuntimeCompletionCandidateAcceptance::Accepted(witness))
    }

    /// Retains chronology before its live turn state is removed.
    pub(crate) fn queue_bookkeeping_candidate(
        &mut self,
        mut candidate: RuntimeBookkeepingCandidate,
    ) {
        self.next_bookkeeping_generation = self.next_bookkeeping_generation.saturating_add(1);
        candidate.generation = self.next_bookkeeping_generation;
        self.bookkeeping_candidates.push(candidate);
    }

    /// Claims the oldest unblocked candidate for each conversation.
    pub(crate) fn claim_bookkeeping_candidates(&mut self) -> Vec<RuntimeBookkeepingCandidate> {
        let mut seen = std::collections::BTreeSet::new();
        self.bookkeeping_candidates
            .iter()
            .filter_map(|candidate| {
                if !seen.insert(candidate.turn.conversation_id.clone())
                    || candidate.blocked
                    || !self.bookkeeping_claims.insert(candidate.generation)
                {
                    return None;
                }
                Some(candidate.clone())
            })
            .collect()
    }

    /// Reports whether unchecked history or an unsettled terminal handoff
    /// fences this conversation. Ordinary pending raw rows remain available
    /// for checked logical replay; terminal summary proof must commit first.
    pub(crate) fn bookkeeping_pending(&self, conversation_id: &str) -> bool {
        self.bookkeeping_candidates
            .iter()
            .any(|candidate| candidate.turn.conversation_id == conversation_id)
            || self
                .pending_transcript_entries(conversation_id)
                .iter()
                .any(|entry| {
                    entry.role == mez_agent::transcript::TranscriptRole::System
                        && entry
                            .content
                            .starts_with(crate::storage::transcript::compaction_handoff::MARKER)
                })
    }

    /// Consumes an exact worker claim; duplicate results cannot admit another append.
    pub(crate) fn take_bookkeeping_candidate(
        &mut self,
        generation: u64,
    ) -> Option<RuntimeBookkeepingCandidate> {
        if !self.bookkeeping_claims.remove(&generation) {
            return None;
        }
        let index = self
            .bookkeeping_candidates
            .iter()
            .position(|candidate| candidate.generation == generation)?;
        Some(self.bookkeeping_candidates.remove(index))
    }

    /// Retains a failed candidate without automatically replaying an uncertain append.
    pub(crate) fn block_bookkeeping_candidate(
        &mut self,
        mut candidate: RuntimeBookkeepingCandidate,
    ) {
        candidate.blocked = true;
        self.bookkeeping_candidates.insert(0, candidate);
    }
}
