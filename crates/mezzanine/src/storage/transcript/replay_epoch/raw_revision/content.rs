//! Content-only row projection for the durable edit owner. Semantic envelope
//! framing, producers, stable records and native custody are never inferred from
//! edited text. Literal projection uses the same privacy renderer as runtime.

use super::*;
use mez_agent::transcript::{TranscriptContextEvent as Event, TranscriptContextRecord as Record};
use mez_agent::{
    ContextBlock, ContextBlockRecord, ContextPlacement, ContextSourceKind, TranscriptEntry,
    TranscriptRole,
};

/// Strict semantic decoding is limited to original System records. User text
/// that resembles a protocol marker remains ordinary source.
pub(super) fn semantic(row: &TranscriptEntry) -> Result<Option<(Event, Option<Record>)>> {
    if row.role != TranscriptRole::System {
        return Ok(None);
    }
    if mez_agent::transcript::TranscriptExecutionStateRecord::from_transcript_content(&row.content)?
        .is_some()
    {
        return Ok(None);
    }
    if let Some(record) = Record::from_transcript_content(&row.content)? {
        return Ok(Some((record.event().clone(), Some(record))));
    }
    if let Some(event) = Event::from_transcript_content(&row.content) {
        return Ok(Some((event, None)));
    }
    if row.content.starts_with("[mez-transcript-context-event/")
        || row.content.starts_with("[mez-transcript-context-record/")
    {
        return Err(invalid("content edit has malformed semantic source"));
    }
    Ok(None)
}

/// Returns the model block and normalized record for a validated content-only
/// edit. The original canonical block, when present, supplies exact framing.
pub(super) fn revise(
    original: &TranscriptEntry,
    updated: &mut TranscriptEntry,
    canonical: Option<&ContextBlock>,
) -> Result<(Option<ContextBlock>, Option<ContextBlockRecord>)> {
    for row in [original, &*updated] {
        if row.role == TranscriptRole::System
            && mez_agent::transcript::TranscriptExecutionStateRecord::from_transcript_content(
                &row.content,
            )?
            .is_some()
        {
            return Err(invalid(
                "cannot edit original execution lifecycle control records",
            ));
        }
    }
    let before = semantic(original)?;
    let after = semantic(updated)?;
    match (before, after) {
        (Some((old, old_record)), Some((new, new_record))) => {
            if old_record
                .as_ref()
                .map(Record::record)
                .map(|r| r.occurrence_id())
                != new_record
                    .as_ref()
                    .map(Record::record)
                    .map(|r| r.occurrence_id())
                || old_record.as_ref().and_then(Record::stable_records)
                    != new_record.as_ref().and_then(Record::stable_records)
            {
                return Err(invalid(
                    "content edit changed occurrence or stable accounting",
                ));
            }
            let mut expected = old.clone();
            match (&mut expected, &new) {
                (
                    Event::UserEvent { content, .. },
                    Event::UserEvent {
                        content: actual, ..
                    },
                )
                | (
                    Event::TaskAdmission { content, .. },
                    Event::TaskAdmission {
                        content: actual, ..
                    },
                )
                | (Event::RoutedHandoff { content }, Event::RoutedHandoff { content: actual }) => {
                    *content = actual.clone()
                }
                (
                    Event::ExecutionBlock {
                        content,
                        projection_sha256,
                        ..
                    },
                    Event::ExecutionBlock {
                        content: actual,
                        projection_sha256: sha,
                        ..
                    },
                )
                | (
                    Event::PromptBoundary {
                        content,
                        projection_sha256,
                        ..
                    },
                    Event::PromptBoundary {
                        content: actual,
                        projection_sha256: sha,
                        ..
                    },
                )
                | (
                    Event::EnvironmentSnapshot {
                        content,
                        projection_sha256,
                    },
                    Event::EnvironmentSnapshot {
                        content: actual,
                        projection_sha256: sha,
                    },
                )
                | (
                    Event::McpCatalogSnapshot {
                        content,
                        projection_sha256,
                        ..
                    },
                    Event::McpCatalogSnapshot {
                        content: actual,
                        projection_sha256: sha,
                        ..
                    },
                ) => {
                    *content = actual.clone();
                    *projection_sha256 = sha.clone();
                }
                (
                    Event::InterruptedTurn {
                        prompt,
                        reason,
                        evidence,
                    },
                    Event::InterruptedTurn {
                        prompt: p,
                        reason: r,
                        evidence: e,
                    },
                ) => {
                    *prompt = p.clone();
                    *reason = r.clone();
                    *evidence = e.clone();
                }
                _ => {
                    return Err(invalid(
                        "content edit cannot replace original custody or control records",
                    ));
                }
            }
            if expected != new {
                return Err(invalid("content edit changed original semantic framing"));
            }
            let mut block = project(&new)?;
            if let Some(original) = canonical {
                let mut old_block = project(&old)?;
                // Legacy imported user rows can retain TranscriptUser framing;
                // the existing canonical occurrence, not current labels, owns it.
                if matches!(old, Event::UserEvent { .. } | Event::TaskAdmission { .. }) {
                    old_block.source = original.source;
                    block.source = original.source;
                }
                if &old_block != original {
                    return Err(invalid(
                        "content edit original row differs from retained source",
                    ));
                }
            }
            let record = old_record
                .as_ref()
                .map(|record| record.record().revised(&block))
                .transpose()
                .map_err(context_error)?;
            if let (Some(record), Some(old_record)) = (record, old_record) {
                updated.content =
                    Record::new(new, record, old_record.stable_records().map(<[_]>::to_vec))?
                        .to_transcript_content()?;
            }
            Ok((Some(block), record))
        }
        (None, None) => {
            let label = canonical.map_or_else(
                || "edited transcript row".into(),
                |block| block.label.clone(),
            );
            let before =
                mez_agent::transcript::transcript_literal_context_block(original, label.clone());
            let after = mez_agent::transcript::transcript_literal_context_block(updated, label);
            if let Some(canonical) = canonical
                && before.as_ref() != Some(canonical)
            {
                return Err(invalid(
                    "content edit original literal differs from retained source",
                ));
            }
            if before.as_ref().map(|b| (b.source, b.placement, &b.label))
                != after.as_ref().map(|b| (b.source, b.placement, &b.label))
            {
                return Err(invalid("content edit changed literal model framing"));
            }
            Ok((after, None))
        }
        _ => Err(invalid(
            "content edit cannot create or remove semantic envelopes",
        )),
    }
}

/// Rechecks an actual completed arrival group's native pairs using only its
/// qualified original control and source rows. This isolated descriptive owner
/// grants no live execution or append right and never changes the saved epoch.
pub(super) fn validate_closed_group(
    cid: &str,
    rows: &[TranscriptEntry],
    group: &mez_agent::ContextExecutionGroupId,
    state: &mez_agent::ReplayExecutionState,
) -> Result<()> {
    let mut blocks = Vec::new();
    let mut records = Vec::new();
    let mut imported = Vec::new();
    let mut sequences = Vec::new();
    let mut originals = BTreeMap::new();
    let mut frontier = Vec::new();
    for row in rows {
        if let Some((event, Some(record))) = semantic(row)?
            && matches!(&event, Event::ExecutionBlock {execution_group_id: Some(actual), ..} if actual == group)
        {
            let id = record.record().occurrence_id();
            frontier.push(id);
            if originals.insert(id, (event, record)).is_some() {
                return Err(invalid("content edit arrival repeats native source"));
            }
        }
    }
    if !frontier
        .iter()
        .copied()
        .eq(state.members().iter().map(|(id, _)| *id))
    {
        return Err(invalid(
            "content edit cannot replace execution source beyond its closed frontier",
        ));
    }
    for (id, sequence) in state.members() {
        let (event, record) = originals
            .remove(id)
            .ok_or_else(|| invalid("content edit arrival has missing native source"))?;
        let Event::ExecutionBlock {
            execution_group_id: Some(actual),
            ordinal: Some(ordinal),
            provider_owner,
            ..
        } = &event
        else {
            return Err(invalid(
                "content edit arrival changed native source framing",
            ));
        };
        if actual != group {
            return Err(invalid("content edit arrival changed native group"));
        }
        let block = project(&event)?;
        let index = blocks.len();
        imported.push(
            mez_agent::ImportedExecutionEvent::new(
                block.clone(),
                group.clone(),
                *ordinal,
                provider_owner.clone(),
            )
            .and_then(|event| event.with_record(record.record()))
            .map_err(context_error)?,
        );
        records.push((index, record.record()));
        sequences.push((
            index,
            mez_agent::ContextEventSequence::new(*sequence).map_err(context_error)?,
        ));
        blocks.push(block);
    }
    let mut context = AgentContext::empty();
    context
        .append_imported_replay_arrivals(blocks, &records, &imported, &sequences)
        .map_err(context_error)?;
    let mut owner = ReplayEpochContext::new(
        ReplayConversationId::new(cid).map_err(context_error)?,
        context,
    )
    .map_err(context_error)?;
    owner
        .restore_execution_state(state)
        .map_err(context_error)?;
    if owner.context().execution_group_is_pending(group) {
        return Err(invalid("content edit native source remains pending"));
    }
    Ok(())
}

fn project(event: &Event) -> Result<ContextBlock> {
    let (source, label, content) = match event {
        Event::UserEvent { label, content, .. } | Event::TaskAdmission { label, content, .. } => (
            ContextSourceKind::UserInstruction,
            label.clone(),
            content.clone(),
        ),
        Event::ExecutionBlock {
            source,
            label,
            content,
            ..
        }
        | Event::PromptBoundary {
            source,
            label,
            content,
            ..
        } => (*source, label.clone(), content.clone()),
        Event::EnvironmentSnapshot { content, .. } => (
            ContextSourceKind::Configuration,
            "task environment snapshot".into(),
            content.clone(),
        ),
        Event::McpCatalogSnapshot { content, .. } => (
            ContextSourceKind::McpCatalogSnapshot,
            mez_agent::MCP_CATALOG_SNAPSHOT_CONTEXT_LABEL.into(),
            content.clone(),
        ),
        Event::RoutedHandoff { content } => (
            ContextSourceKind::RoutedHandoff,
            "routed worker handoff context".into(),
            content.clone(),
        ),
        Event::InterruptedTurn {
            prompt,
            reason,
            evidence,
        } => {
            return Ok(mez_agent::transcript::interrupted_turn_context_block(
                prompt, reason, evidence,
            ));
        }
        _ => {
            return Err(invalid(
                "content edit selected immutable custody/control source",
            ));
        }
    };
    Ok(ContextBlock {
        source,
        placement: ContextPlacement::ConversationAppend,
        label,
        content,
    })
}
