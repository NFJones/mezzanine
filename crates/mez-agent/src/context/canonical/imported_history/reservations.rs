//! Original sparse reservations at an explicit unowned history import boundary.
//!
//! Imported rows may mix explicit producer sequences with older unnumbered
//! source. Allocate only the latter between checked anchors. This operation
//! transports chronology; it cannot rebase a live owner or grant completion.

use super::*;
use crate::ContextBlockOccurrenceId;
use crate::context::canonical::{
    CONTEXT_EVENT_SEQUENCE_STRIDE, history::history_prefix_replacement_sequences,
};

impl AgentContext {
    /// Restores externally qualified occurrence reservations in source order.
    /// Unknown/duplicate occurrences, reversed anchors, insufficient gaps and
    /// exhaustion reject atomically. Unnumbered events receive deterministic
    /// local slots between anchors; bytes, records and protection survive.
    /// The caller must qualify the original rows before this descriptive import.
    /// Owned replay must use its append/admission ports instead of resequencing.
    pub fn restore_imported_history_sequences(
        &mut self,
        reservations: &[(ContextBlockOccurrenceId, ContextEventSequence)],
    ) -> AgentContextResult<()> {
        if self.has_replay_owner() {
            return Err(AgentContextError::new(
                "owned replay cannot reassign imported reservations",
            ));
        }
        self.validate_durable()?;
        if reservations.is_empty() {
            return Ok(());
        }
        let mut by_id = BTreeMap::new();
        for (id, sequence) in reservations {
            if by_id.insert(*id, *sequence).is_some() {
                return Err(AgentContextError::new("duplicate imported reservation"));
            }
        }
        let mut anchors = Vec::new();
        for (index, event) in self.chronology.iter().enumerate() {
            if let Some(sequence) = by_id.remove(&event.record().occurrence_id()) {
                anchors.push((index, Some(sequence.get())));
            }
        }
        if !by_id.is_empty() {
            return Err(AgentContextError::new(
                "imported reservation has no source occurrence",
            ));
        }
        anchors.push((self.chronology.len(), None));
        let mut assigned = Vec::with_capacity(self.chronology.len());
        let mut first = 0;
        let mut previous = 0u64;
        for (index, anchor) in anchors {
            let room = anchor
                .map(|sequence| {
                    sequence
                        .checked_sub(previous)
                        .filter(|room| *room > 0)
                        .ok_or_else(|| {
                            AgentContextError::new("imported reservations are not ordered")
                        })
                })
                .transpose()?;
            for offset in history_prefix_replacement_sequences(index - first, room)? {
                assigned.push(
                    previous
                        .checked_add(offset)
                        .ok_or_else(|| AgentContextError::new("imported reservation exhausted"))?,
                );
            }
            if let Some(sequence) = anchor {
                assigned.push(sequence);
                previous = sequence;
                first = index + 1;
            }
        }
        let mut candidate = self.clone();
        let mut remapped = BTreeMap::new();
        for (event, sequence) in candidate.chronology.iter_mut().zip(assigned) {
            let sequence = ContextEventSequence::new(sequence)?;
            remapped.insert(event.sequence, sequence);
            event.sequence = sequence;
        }
        let remap = |sequences: &BTreeSet<ContextEventSequence>| -> AgentContextResult<BTreeSet<ContextEventSequence>> {
            sequences.iter().map(|sequence| remapped.get(sequence).copied()
                .ok_or_else(|| AgentContextError::new("imported protection lost its occurrence"))).collect()
        };
        candidate.pending_peer_messages = remap(&candidate.pending_peer_messages)?;
        candidate.historical_instruction_events = remap(&candidate.historical_instruction_events)?;
        candidate.next_event_sequence = candidate
            .chronology
            .last()
            .and_then(|event| {
                event
                    .sequence
                    .get()
                    .checked_add(CONTEXT_EVENT_SEQUENCE_STRIDE)
            })
            .ok_or_else(|| AgentContextError::new("imported reservation exhausted"))?;
        candidate.rebuild_projections();
        candidate.validate_durable()?;
        *self = candidate;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_reservations_keep_exact_records_and_peer_protection() {
        let mut context = AgentContext::new(vec![
            ContextBlock::reference_event(
                crate::ContextSourceKind::CompactionPrefix,
                "head",
                "old head",
            ),
            ContextBlock::user_event("prompt", "original task"),
        ])
        .unwrap();
        let peer = context
            .append_conversation_events([crate::ContextConversationAppend::peer_message(
                "peer",
                "original evidence",
            )])
            .unwrap()[0];
        context.retain_pending_peer_message(peer).unwrap();
        let events = context.chronology().to_vec();
        context
            .restore_imported_history_sequences(&[
                (
                    events[1].record().occurrence_id(),
                    ContextEventSequence::new(1024).unwrap(),
                ),
                (
                    events[2].record().occurrence_id(),
                    ContextEventSequence::new(8192).unwrap(),
                ),
            ])
            .unwrap();
        for (actual, original) in context.chronology().iter().zip(&events) {
            assert_eq!(actual.record(), original.record());
            assert_eq!(actual.block(), original.block());
        }
        assert_eq!(context.chronology()[0].sequence().get(), 512);
        assert_eq!(context.chronology()[1].sequence().get(), 1024);
        assert!(context.peer_message_is_pending(ContextEventSequence::new(8192).unwrap()));
    }

    #[test]
    fn imported_reservations_reject_conflicts_and_live_owners_atomically() {
        let original = AgentContext::new(vec![
            ContextBlock::user_event("one", "first"),
            ContextBlock::user_event("two", "second"),
        ])
        .unwrap();
        let a = original.chronology()[0].record().occurrence_id();
        let b = original.chronology()[1].record().occurrence_id();
        let sequence = |value| ContextEventSequence::new(value).unwrap();
        for hints in [
            vec![(a, sequence(1024)), (a, sequence(2048))],
            vec![(ContextBlockOccurrenceId::allocate(), sequence(1024))],
            vec![(a, sequence(2048)), (b, sequence(1024))],
            vec![(b, sequence(1))],
            vec![(b, sequence(u64::MAX))],
        ] {
            let mut candidate = original.clone();
            assert!(
                candidate
                    .restore_imported_history_sequences(&hints)
                    .is_err()
            );
            assert_eq!(candidate, original);
        }
        let owner = crate::ReplayEpochContext::new(
            crate::ReplayConversationId::new("conversation").unwrap(),
            original,
        )
        .unwrap();
        let mut body = owner.into_owned_context();
        let before = body.clone();
        assert!(
            body.restore_imported_history_sequences(&[(a, sequence(2048))])
                .is_err()
        );
        assert_eq!(body, before);
    }
}
