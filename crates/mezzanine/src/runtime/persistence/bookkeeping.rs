//! Ordered candidate ownership before checked transcript append admission.
//!
//! Candidates survive terminal cleanup but are not receipts or logical rows.
//! Only one candidate per conversation may be checked at a time; a failed read
//! retains its candidate and fences subsequent history admission fail-closed.

use super::RuntimePersistenceComponent;
use crate::runtime::RuntimeBookkeepingCandidate;

impl RuntimePersistenceComponent {
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

    /// Reports whether unchecked history fences this conversation.
    pub(crate) fn bookkeeping_pending(&self, conversation_id: &str) -> bool {
        self.bookkeeping_candidates
            .iter()
            .any(|candidate| candidate.turn.conversation_id == conversation_id)
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
