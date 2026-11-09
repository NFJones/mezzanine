//! Qualified task boundaries partition original arrivals before canonical import.
//!
//! A task admission references its first original suffix occurrence, including
//! preceding prelude records. Only that typed boundary retires the earlier task;
//! ordinary user/peer text cannot do so. IDs, cached costs, sequence reservations
//! and native metadata are sliced structurally, never matched by display text.

use super::*;
use std::collections::BTreeMap;

/// Initial source has no reserved live frontier. Assign synthetic slots around
/// checked anchors before canonical membership or lifecycle handles exist.
pub(super) fn bootstrap(
    conversation: mez_agent::ReplayConversationId,
    blocks: Vec<ContextBlock>,
    arrivals: &RuntimeContextRecords,
    execution: &[mez_agent::ImportedExecutionEvent],
) -> Result<mez_agent::ReplayEpochContext> {
    let mut reservations = arrivals
        .records
        .iter()
        .filter_map(|(_, record)| {
            arrivals
                .source_sequences
                .get(&record.occurrence_id())
                .map(|sequence| (record.occurrence_id(), *sequence))
        })
        .collect::<BTreeMap<_, _>>();
    let current = arrivals
        .records
        .iter()
        .map(|(_, record)| record.occurrence_id())
        .collect::<std::collections::BTreeSet<_>>();
    // Qualified controls carry original sequences for otherwise unnumbered
    // execution rows. Interpolation must not move those independently backed
    // members, and a contradictory carrier must not overwrite another anchor.
    for state in &arrivals.execution_controls {
        for (id, sequence) in state
            .members()
            .iter()
            .filter(|(id, _)| current.contains(id))
        {
            let sequence = mez_agent::ContextEventSequence::new(*sequence)?;
            if reservations
                .insert(*id, sequence)
                .is_some_and(|prior| prior != sequence)
            {
                return Err(invalid());
            }
        }
    }
    let reservations = reservations.into_iter().collect::<Vec<_>>();
    let tasks = arrivals
        .task_admissions
        .iter()
        .map(|(root, first)| (*root, *first))
        .collect::<Vec<_>>();
    let mut owner = mez_agent::ReplayEpochContext::from_imported_history(
        conversation,
        blocks,
        &arrivals.records,
        execution,
        &reservations,
        &tasks,
    )?;
    restore_controls(&mut owner, arrivals)?;
    Ok(owner)
}

pub(super) fn append(
    mut owner: mez_agent::ReplayEpochContext,
    conversation: mez_agent::ReplayConversationId,
    blocks: Vec<ContextBlock>,
    arrivals: &RuntimeContextRecords,
    execution: &[mez_agent::ImportedExecutionEvent],
) -> Result<mez_agent::ReplayEpochContext> {
    let by_id = arrivals
        .records
        .iter()
        .map(|(index, record)| (record.occurrence_id(), *index))
        .collect::<BTreeMap<_, _>>();
    if by_id.len() != blocks.len() || arrivals.records.len() != blocks.len() {
        return Err(invalid());
    }
    let mut tasks = arrivals
        .task_admissions
        .iter()
        .map(|(prompt, first)| {
            let start = *by_id.get(first).ok_or_else(invalid)?;
            let root = *by_id.get(prompt).ok_or_else(invalid)?;
            if start > root || root >= blocks.len() {
                return Err(invalid());
            }
            Ok((start, root))
        })
        .collect::<Result<Vec<_>>>()?;
    tasks.sort_unstable();
    if tasks.windows(2).any(|pair| pair[0].1 >= pair[1].0) {
        return Err(invalid());
    }
    let metadata = execution
        .iter()
        .map(|event| {
            let record = event.record().ok_or_else(invalid)?;
            let index = *by_id.get(&record.occurrence_id()).ok_or_else(invalid)?;
            Ok((index, event))
        })
        .collect::<Result<Vec<_>>>()?;
    let prefix_end = tasks.first().map_or(blocks.len(), |task| task.0);
    let parts = std::iter::once((0, prefix_end, None)).chain(tasks.iter().enumerate().map(
        |(index, (start, prompt))| {
            (
                *start,
                tasks.get(index + 1).map_or(blocks.len(), |task| task.0),
                Some(prompt - start),
            )
        },
    ));
    for (start, end, prompt) in parts {
        if start == end {
            continue;
        }
        let records = arrivals
            .records
            .iter()
            .filter(|(index, _)| (start..end).contains(index))
            .map(|(index, record)| (index - start, *record))
            .collect::<Vec<_>>();
        let sequences = records
            .iter()
            .filter_map(|(index, record)| {
                arrivals
                    .source_sequences
                    .get(&record.occurrence_id())
                    .map(|sequence| (*index, *sequence))
            })
            .collect::<Vec<_>>();
        let execution = metadata
            .iter()
            .filter(|(index, _)| (start..end).contains(index))
            .map(|(_, event)| (*event).clone())
            .collect::<Vec<_>>();
        if let Some(prompt) = prompt {
            owner.restore_instruction_task(
                blocks[start..end].to_vec(),
                &records,
                &execution,
                prompt,
                &sequences,
            )?;
        } else {
            let mut body = owner.into_owned_context();
            body.append_imported_replay_arrivals(
                blocks[start..end].to_vec(),
                &records,
                &execution,
                &sequences,
            )?;
            owner = mez_agent::ReplayEpochContext::from_owned_context(conversation.clone(), body)?;
        }
    }
    restore_controls(&mut owner, arrivals)?;
    Ok(owner)
}

/// Controls still need their independently qualified original source. Neither
/// bootstrap allocation nor ordinary arrival import creates closure authority.
fn restore_controls(
    owner: &mut mez_agent::ReplayEpochContext,
    arrivals: &RuntimeContextRecords,
) -> Result<()> {
    let current = owner
        .context()
        .chronology()
        .iter()
        .map(|event| event.record().occurrence_id())
        .collect::<std::collections::BTreeSet<_>>();
    for state in &arrivals.execution_controls {
        // Escrow independently qualified fully covered original source. Its
        // delayed control cannot restore old raw membership behind this head.
        if state.members().iter().all(|(id, _)| !current.contains(id)) {
            continue;
        }
        owner.restore_execution_state(state)?;
    }
    Ok(())
}
