//! Immutable original-source backing carried between epoch publications.
//!
//! This is per-publication audit escrow, not selectable model history or an
//! append journal. Covered source remains available to recognize its later
//! ordinary row without restoring it to the model. Original turn provenance,
//! occurrence, canonical framing and group ordinal must all agree. Settlement
//! and cursor integration must qualify retirement; until then references remain
//! required and bounded rather than being guessed away from pane counts.

use super::{Payload, SOURCE_LIMIT, context_error, invalid};
use crate::Result;
use mez_agent::{
    ContextBlockOccurrenceId, ContextBlockRecord, ContextSourceKind, ReplayEpochContext,
    TranscriptEntry, TranscriptRole,
    transcript::{TranscriptContextEvent, TranscriptContextRecord},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Original controller turn that owns newly authored, not yet archived source.
/// This is descriptive provenance, never an execution or append acknowledgment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplaySourceOrigin {
    turn_id: String,
    agent_id: String,
    pane_id: String,
}

impl ReplaySourceOrigin {
    /// Captures actual original turn fields. Empty/oversized/control-bearing
    /// identifiers refuse; a later pane lookup cannot replace this provenance.
    pub(crate) fn new(turn_id: &str, agent_id: &str, pane_id: &str) -> Result<Self> {
        let origin = Self {
            turn_id: turn_id.into(),
            agent_id: agent_id.into(),
            pane_id: pane_id.into(),
        };
        origin.validate()?;
        Ok(origin)
    }

    fn validate(&self) -> Result<()> {
        if [&self.turn_id, &self.agent_id, &self.pane_id]
            .into_iter()
            .any(|value| {
                value.is_empty()
                    || value.trim() != value
                    || value.len() > 4096
                    || value.chars().any(char::is_control)
            })
        {
            return Err(invalid("invalid replay source turn provenance"));
        }
        Ok(())
    }
}

/// One original occurrence, independent of summary anchors and row positions.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EscrowOccurrence {
    record: ContextBlockRecord,
    source: ContextSourceKind,
    label: String,
    content: Payload,
    sequence: u64,
    group: Option<String>,
    ordinal: Option<u64>,
    provider: Option<(String, String)>,
    origin: ReplaySourceOrigin,
}

impl std::fmt::Debug for EscrowOccurrence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EscrowOccurrence")
    }
}

/// Retains inherited originals and adds only actual current raw occurrences.
/// Inherited origin and ordinal cannot be reassigned by a newer caller. No
/// settlement claim is inferred here; a future receipt-qualified cut retires it.
pub(super) fn extend(
    inherited: &[EscrowOccurrence],
    source: &ReplayEpochContext,
    origin: &ReplaySourceOrigin,
) -> Result<Vec<EscrowOccurrence>> {
    validate(inherited)?;
    origin.validate()?;
    let mut originals = inherited
        .iter()
        .map(|item| (item.record.occurrence_id(), item.clone()))
        .collect::<BTreeMap<_, _>>();
    for event in source
        .context()
        .chronology()
        .iter()
        .skip(usize::from(source.head().is_some()))
    {
        let item = EscrowOccurrence {
            record: event.record(),
            source: event.block().source,
            label: event.block().label.clone(),
            content: Payload::new(event.block().content.as_bytes()),
            sequence: event.sequence().get(),
            group: event
                .execution_group_id()
                .map(|group| group.as_str().into()),
            ordinal: None,
            provider: event
                .provider_owner()
                .map(|owner| (owner.api().as_str().into(), owner.provider_id().into())),
            origin: origin.clone(),
        };
        match originals.get(&item.record.occurrence_id()) {
            Some(previous) => {
                let mut expected = item;
                expected.origin = previous.origin.clone();
                expected.ordinal = previous.ordinal;
                if &expected != previous {
                    return Err(invalid("replay escrow original occurrence changed"));
                }
            }
            None => {
                originals.insert(item.record.occurrence_id(), item);
            }
        }
    }
    let mut result = originals.into_values().collect::<Vec<_>>();
    result.sort_by_key(|item| item.sequence);
    let mut ordinals = BTreeMap::<String, u64>::new();
    for item in &mut result {
        if let Some(group) = &item.group {
            let ordinal = ordinals.entry(group.clone()).or_default();
            *ordinal = ordinal
                .checked_add(1)
                .ok_or_else(|| invalid("replay escrow ordinal exhausted"))?;
            if item.ordinal.is_some_and(|old| old != *ordinal) {
                return Err(invalid("replay escrow original ordinal changed"));
            }
            item.ordinal = Some(*ordinal);
        }
    }
    validate(&result)?;
    Ok(result)
}

/// Validates bounded metadata before payload lookup or canonical row matching.
pub(super) fn validate(items: &[EscrowOccurrence]) -> Result<()> {
    let mut ids = BTreeSet::new();
    let mut sequence = 0;
    let mut bytes = 0usize;
    let mut ordinals = BTreeMap::<&str, u64>::new();
    for item in items {
        item.origin.validate()?;
        item.record.validate_format().map_err(context_error)?;
        item.content.validate_format()?;
        bytes = bytes
            .checked_add(item.content.bytes)
            .filter(|bytes| *bytes <= SOURCE_LIMIT)
            .ok_or_else(|| invalid("unsettled replay source exceeds backing limit"))?;
        if !ids.insert(item.record.occurrence_id())
            || item.sequence == 0
            || item.sequence < sequence
            || item.label.is_empty()
            || item.label.len() > SOURCE_LIMIT
            || item.group.is_some() != item.ordinal.is_some()
            || matches!(
                item.source,
                ContextSourceKind::CompactionPrefix | ContextSourceKind::CompactionSelective
            )
        {
            return Err(invalid("invalid replay escrow occurrence framing"));
        }
        if let Some(group) = &item.group {
            mez_agent::ContextExecutionGroupId::new(group.clone()).map_err(context_error)?;
            let next = ordinals.entry(group.as_str()).or_default();
            *next = next
                .checked_add(1)
                .ok_or_else(|| invalid("replay escrow ordinal exhausted"))?;
            if item.ordinal != Some(*next) {
                return Err(invalid("invalid replay escrow group ordinal"));
            }
        }
        sequence = item.sequence;
    }
    Ok(())
}

/// Every current raw member must already have its exact original backing.
/// A summary anchor is not an original source identity or a second membership.
pub(super) fn validate_current(
    items: &[EscrowOccurrence],
    source: &ReplayEpochContext,
    covered: &[ContextBlockOccurrenceId],
) -> Result<()> {
    let first = items
        .first()
        .ok_or_else(|| invalid("required source escrow is empty"))?;
    let ids = items
        .iter()
        .map(EscrowOccurrence::occurrence_id)
        .collect::<BTreeSet<_>>();
    if extend(items, source, &first.origin)? != items
        || covered.iter().any(|id| !ids.contains(id))
        || source
            .head()
            .is_some_and(|head| ids.contains(&head.occurrence_id()))
    {
        return Err(invalid(
            "replay escrow does not back current original source",
        ));
    }
    Ok(())
}

impl EscrowOccurrence {
    pub(super) fn occurrence_id(&self) -> ContextBlockOccurrenceId {
        self.record.occurrence_id()
    }

    pub(super) fn payload(&self) -> &Payload {
        &self.content
    }

    /// A known ID must match its exact original producer and semantic bytes.
    /// Saved stable carriers on delayed copies cannot become new current slots.
    pub(super) fn matches(
        &self,
        row: &TranscriptEntry,
        authored: &TranscriptContextRecord,
        content: &str,
    ) -> bool {
        if row.role != TranscriptRole::System
            || row.turn_id != self.origin.turn_id
            || row.agent_id != self.origin.agent_id
            || row.pane_id != self.origin.pane_id
            || authored.record() != self.record
        {
            return false;
        }
        match authored.event() {
            TranscriptContextEvent::UserEvent {
                event_sequence,
                label,
                content: actual,
            } => {
                matches!(
                    self.source,
                    ContextSourceKind::UserInstruction | ContextSourceKind::TranscriptUser
                ) && *event_sequence == self.sequence
                    && label == &self.label
                    && actual == content
            }
            TranscriptContextEvent::PromptBoundary {
                source,
                event_sequence,
                label,
                content: actual,
                ..
            } => {
                *source == self.source
                    && *event_sequence == Some(self.sequence)
                    && label == &self.label
                    && actual == content
                    && self.group.is_none()
            }
            TranscriptContextEvent::ExecutionBlock {
                source,
                execution_group_id,
                ordinal,
                provider_owner,
                label,
                content: actual,
                ..
            } => {
                *source == self.source
                    && execution_group_id.as_ref().map(|group| group.as_str())
                        == self.group.as_deref()
                    && *ordinal == self.ordinal
                    && provider_owner
                        .as_ref()
                        .map(|owner| (owner.api().as_str(), owner.provider_id()))
                        == self
                            .provider
                            .as_ref()
                            .map(|(api, provider)| (api.as_str(), provider.as_str()))
                    && label == &self.label
                    && actual == content
            }
            TranscriptContextEvent::EnvironmentSnapshot {
                content: actual, ..
            } => {
                self.source == ContextSourceKind::Configuration
                    && self.label == "task environment snapshot"
                    && actual == content
            }
            TranscriptContextEvent::McpCatalogSnapshot {
                event_sequence,
                content: actual,
                ..
            } => {
                self.source == ContextSourceKind::McpCatalogSnapshot
                    && *event_sequence == self.sequence
                    && self.label == mez_agent::MCP_CATALOG_SNAPSHOT_CONTEXT_LABEL
                    && actual == content
            }
            TranscriptContextEvent::NativePeerMessage { event } => {
                self.source == ContextSourceKind::PeerMessage
                    && event.label == self.label
                    && event.content == content
            }
            TranscriptContextEvent::TerminalHandoff { event } => {
                self.source == ContextSourceKind::RuntimeHint
                    && event.label == self.label
                    && event.content == content
            }
            TranscriptContextEvent::RoutedHandoff { content: actual } => {
                self.source == ContextSourceKind::RoutedHandoff
                    && self.label == "routed worker handoff context"
                    && actual == content
            }
            _ => false,
        }
    }
}

/// Checks original semantic copies and zero-model display/audit rows from one
/// already validated archive view. The resolver uses proposed bytes before
/// publication or required immutable files during replay; no lock is acquired.
pub(super) fn qualify_rows(
    items: &[EscrowOccurrence],
    conversation: &str,
    rows: &[TranscriptEntry],
    mut resolve: impl FnMut(&Payload) -> Result<String>,
) -> Result<BTreeSet<u64>> {
    let index = items
        .iter()
        .map(|item| (item.occurrence_id(), item))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let mut qualified = BTreeSet::new();
    let mut events = Vec::with_capacity(rows.len());
    let mut groups = BTreeSet::new();
    for row in rows {
        if row.conversation_id != conversation {
            return Err(invalid("replay source copy has a foreign conversation"));
        }
        if row.role != TranscriptRole::System {
            events.push(None);
            continue;
        }
        let Some(authored) = TranscriptContextRecord::from_transcript_content(&row.content)? else {
            if TranscriptContextEvent::from_transcript_content(&row.content).is_some()
                || row.content.starts_with("[mez-transcript-context-event/")
            {
                return Err(invalid(
                    "unaccounted replay semantic source lacks arrival custody",
                ));
            }
            events.push(None);
            continue;
        };
        let original = index
            .get(&authored.record().occurrence_id())
            .ok_or_else(|| invalid("new replay semantic source lacks original arrival custody"))?;
        let content = resolve(original.payload())?;
        if !original.matches(row, &authored, &content) || !seen.insert(original.occurrence_id()) {
            return Err(invalid(
                "late replay row conflicts with its original source escrow",
            ));
        }
        if let TranscriptContextEvent::ExecutionBlock {
            execution_group_id: Some(group),
            ..
        } = authored.event()
        {
            groups.insert(group.clone());
        }
        events.push(Some(authored.into_event()));
        qualified.insert(row.sequence);
    }
    let zero = mez_agent::transcript::transcript_zero_projection_indices(rows, &events, &groups);
    if rows
        .iter()
        .enumerate()
        .any(|(index, row)| !qualified.contains(&row.sequence) && !zero.contains(&index))
    {
        return Err(invalid("replay archive has unqualified model source"));
    }
    Ok(qualified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mez_agent::{AgentContext, ReplayConversationId};

    /// Equal text remains separate backing; inherited provenance cannot be
    /// rebound to the next turn, and the logical backing limit is checked from
    /// metadata before any large payload allocation or file read.
    #[test]
    fn escrow_keeps_original_producers_distinct_and_bounds_unsettled_backing() {
        let origin = ReplaySourceOrigin::new("first-turn", "agent", "pane").unwrap();
        let other = ReplaySourceOrigin::new("next-turn", "agent", "pane").unwrap();
        let mut source = ReplayEpochContext::new(
            ReplayConversationId::new("escrow").unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        source.append_fact("fact", "same text").unwrap();
        let first = extend(&[], &source, &origin).unwrap();
        source.append_fact("fact", "same text").unwrap();
        let both = extend(&first, &source, &other).unwrap();
        assert_eq!(both[0].origin, origin);
        assert_eq!(both[1].origin, other);
        assert_ne!(
            both[0].record.occurrence_id(),
            both[1].record.occurrence_id()
        );
        assert_eq!(both[0].content, both[1].content);
        let mut oversized = both.clone();
        oversized[0].content.bytes = SOURCE_LIMIT;
        assert!(validate(&oversized).is_err());
        let mut duplicated = both;
        duplicated[1].record = duplicated[0].record;
        assert!(validate(&duplicated).is_err());
        assert!(ReplaySourceOrigin::new("", "agent", "pane").is_err());
    }
}
