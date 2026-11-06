//! Shared display-copy boundaries for canonical transcript projection.
//!
//! Callers supply decoded rows and independently validated execution groups.
//! This pure classification grants no source, execution or append authority;
//! it only identifies presentation/audit rows that emit no model blocks. Origin
//! matching includes conversation, turn, agent and pane. Mismatched input lengths
//! return no proof rather than suppressing unchecked source.

use super::{TranscriptContextEvent, TranscriptEntry, TranscriptRole};
use crate::{ContextExecutionGroupId, ProviderTranscriptEvent};
use std::collections::{BTreeMap, BTreeSet};

/// Identifies preceding display rows covered by each first validated group row.
/// Canonical rows are never removed by this classification; consumers apply it
/// only to ordinary assistant/tool display. Different producers cannot borrow
/// one another's group even when a turn spelling or text is repeated.
pub fn transcript_display_copy_indices(
    rows: &[TranscriptEntry],
    events: &[Option<TranscriptContextEvent>],
    groups: &BTreeSet<ContextExecutionGroupId>,
) -> BTreeSet<usize> {
    if rows.len() != events.len() {
        return BTreeSet::new();
    }
    let mut copies = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut previous = BTreeMap::new();
    for (index, (row, event)) in rows.iter().zip(events).enumerate() {
        let Some(TranscriptContextEvent::ExecutionBlock {
            execution_group_id, ..
        }) = event
        else {
            continue;
        };
        if execution_group_id
            .as_ref()
            .is_some_and(|group| groups.contains(group) && seen.insert(group))
        {
            let start = previous.get(&origin(row)).map_or(0, |last| last + 1);
            copies.extend(
                (start..index).filter(|candidate| origin(&rows[*candidate]) == origin(row)),
            );
        }
        previous.insert(origin(row), index);
    }
    copies
}

/// Proves only unaccounted display/audit rows emit no model source. Decoded
/// semantic rows need their own occurrence proof, including zero-render control
/// markers. Opaque provider display requires a validated group from the same
/// producer; ordinary user text never becomes a display copy.
pub fn transcript_zero_projection_indices(
    rows: &[TranscriptEntry],
    events: &[Option<TranscriptContextEvent>],
    groups: &BTreeSet<ContextExecutionGroupId>,
) -> BTreeSet<usize> {
    if rows.len() != events.len() {
        return BTreeSet::new();
    }
    let copies = transcript_display_copy_indices(rows, events, groups);
    let producers = rows
        .iter()
        .zip(events)
        .filter_map(|(row, event)| match event {
            Some(TranscriptContextEvent::ExecutionBlock {
                execution_group_id: Some(group),
                ..
            }) if groups.contains(group) => Some(origin(row)),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    rows.iter()
        .enumerate()
        .filter_map(|(index, row)| {
            if events[index].is_some() {
                return None;
            }
            let empty = row.content.trim().is_empty();
            let ignored = match row.role {
                TranscriptRole::System => {
                    ProviderTranscriptEvent::from_transcript_content(&row.content).is_none()
                        || producers.contains(&origin(row))
                }
                TranscriptRole::Assistant | TranscriptRole::Tool => copies.contains(&index),
                TranscriptRole::User => false,
            };
            (empty || ignored).then_some(index)
        })
        .collect()
}

/// A display relationship cannot cross any original producer field.
fn origin(row: &TranscriptEntry) -> (&str, &str, &str, &str) {
    (
        &row.conversation_id,
        &row.turn_id,
        &row.agent_id,
        &row.pane_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContextSourceKind;

    /// Identical turn text cannot suppress a foreign producer's row, and input
    /// misalignment or an unqualified group yields no suppression proof.
    #[test]
    fn display_proof_is_bound_to_complete_original_producer() {
        let row = |role| TranscriptEntry {
            conversation_id: "conversation".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role,
            turn_id: "turn".into(),
            agent_id: "agent".into(),
            pane_id: "pane".into(),
            content: "original source".into(),
        };
        let mut rows = vec![
            row(TranscriptRole::Assistant),
            row(TranscriptRole::Tool),
            row(TranscriptRole::System),
        ];
        let group = ContextExecutionGroupId::new("original-group").unwrap();
        let event = TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::TranscriptAssistant,
            "assistant",
            "canonical",
            group.clone(),
            1,
            None,
        )
        .unwrap();
        let events = vec![None, None, Some(event)];
        let groups = BTreeSet::from([group]);
        assert_eq!(
            transcript_zero_projection_indices(&rows, &events, &groups),
            BTreeSet::from([0, 1])
        );
        rows[1].agent_id = "other-agent".into();
        assert_eq!(
            transcript_zero_projection_indices(&rows, &events, &groups),
            BTreeSet::from([0])
        );
        assert!(transcript_zero_projection_indices(&rows, &events, &BTreeSet::new()).is_empty());
        assert!(transcript_display_copy_indices(&rows, &events[..2], &groups).is_empty());
    }
}
