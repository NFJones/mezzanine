//! Bounded descriptive source retained by an actual NoStore controller.
//!
//! This immutable value has no files, append owner or execution capability.
//! Current raw metadata keeps its actual original producer across memory-only
//! rewrites and turn cleanup. An explicit fork projects those originals into
//! independent child rows; only that child's initial receipt grants durability.

use super::*;
use mez_agent::{TranscriptEntry, TranscriptRole, transcript::TranscriptContextRecord};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ReplayMemorySource {
    escrow: Vec<escrow::EscrowOccurrence>,
    rows: Vec<TranscriptEntry>,
    payloads: BTreeMap<String, String>,
    /// Original or locally witnessed controls before checkpoint restoration
    /// deliberately strips controller-minted closure witnesses.
    controls: Vec<(ContextBlockOccurrenceId, TranscriptEntry)>,
}

impl std::fmt::Debug for ReplayMemorySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReplayMemorySource")
    }
}

impl ReplayMemorySource {
    /// Retains only current raw originals. The producer boundary comes from
    /// actual turn admission: older unknown occurrences cannot borrow the new
    /// turn's provenance. Qualified original wire images retain their own
    /// producer independently. No archive is sampled or acknowledged here.
    pub(crate) fn capture(
        owner: &ReplayEpochContext,
        previous: Option<&Self>,
        durable: Option<&ReplayEpochSnapshot>,
        producer: Option<(&ReplaySourceOrigin, u64)>,
        rows: &[TranscriptEntry],
    ) -> Result<Self> {
        owner.checkpoint()?;
        let inherited = previous
            .map(|source| source.escrow.as_slice())
            .or_else(|| durable.and_then(|snapshot| snapshot.escrow.as_deref()))
            .unwrap_or_default();
        let needed = owner
            .context()
            .chronology()
            .iter()
            .skip(usize::from(owner.head().is_some()))
            .map(|event| (event.record().occurrence_id(), event.sequence().get()))
            .collect::<BTreeMap<_, _>>();
        let mut wire = BTreeMap::new();
        for row in previous
            .into_iter()
            .flat_map(|source| source.rows.iter())
            .chain(rows)
        {
            if row.role == TranscriptRole::System
                && let Some(record) =
                    TranscriptContextRecord::from_transcript_content(&row.content)?
                && needed.contains_key(&record.record().occurrence_id())
            {
                let id = record.record().occurrence_id();
                if wire
                    .get(&id)
                    .is_some_and(|old: &TranscriptEntry| old != row)
                {
                    return Err(invalid("memory source original wire image changed"));
                }
                wire.insert(id, row.clone());
            }
        }
        let known = inherited
            .iter()
            .map(|item| item.occurrence_id())
            .collect::<BTreeSet<_>>();
        if needed.iter().any(|(id, sequence)| {
            !known.contains(id)
                && !wire.contains_key(id)
                && producer.is_none_or(|(_, boundary)| *sequence <= boundary)
        }) {
            return Err(invalid(
                "memory source has no original producer for an inherited occurrence",
            ));
        }
        let captured = wire.into_values().collect::<Vec<_>>();
        let fallback = producer
            .map(|(origin, _)| origin.clone())
            .or_else(|| inherited.first().map(|item| item.original_origin().clone()))
            .or_else(|| {
                captured.first().and_then(|row| {
                    ReplaySourceOrigin::new(&row.turn_id, &row.agent_id, &row.pane_id).ok()
                })
            });
        let items = if needed.is_empty() {
            Vec::new()
        } else {
            escrow::qualification_source(
                inherited,
                owner,
                &fallback.ok_or_else(|| invalid("memory source original producer is missing"))?,
                &captured,
            )?
        };
        let items = escrow::copy_selection(&items, &needed.keys().copied().collect())?
            .into_iter()
            .map(|item| item.for_unsettled_copy())
            .collect::<Vec<_>>();
        escrow::validate(&items)?;
        if !needed.is_empty() || owner.head().is_some() {
            escrow::validate_current(&items, owner, &[])?;
        }
        let current = owner
            .context()
            .chronology()
            .iter()
            .map(|event| {
                (
                    event.record().occurrence_id(),
                    event.block().content.as_str(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut payloads = BTreeMap::new();
        for item in &items {
            let reference = item.payload();
            let content = current
                .get(&item.occurrence_id())
                .map(|content| (*content).to_owned())
                .or_else(|| {
                    previous
                        .and_then(|source| source.payloads.get(&reference.sha256))
                        .cloned()
                })
                .or_else(|| {
                    durable
                        .and_then(|source| source.copy_backing.get(&reference.sha256))
                        .cloned()
                })
                .ok_or_else(|| invalid("memory copy source dependency payload is missing"))?;
            reference.validate(content.as_bytes())?;
            payloads.insert(reference.sha256.clone(), content);
        }
        let mut capture = Self {
            escrow: items,
            rows: captured,
            payloads,
            controls: Vec::new(),
        };
        if owner.head().is_none() && !needed.is_empty() {
            capture.capture_raw_controls(owner, previous, durable, rows)?;
        }
        Ok(capture)
    }

    /// Creates descriptive memory provenance, never a made-up committed parent
    /// identity. Native wire projection remains independently checked.
    pub(crate) fn fork_source(&self, owner: &ReplayEpochContext) -> Result<ReplayForkSource> {
        let origin = owner.head().map(|head| {
            initial::ForkOrigin::Memory(copy::MemoryCopyOrigin {
                conversation: owner.conversation_id().as_str().into(),
                epoch_revision: owner.revision().get(),
                head: head.occurrence_id(),
                capture: ContextBlockOccurrenceId::allocate(),
            })
        });
        let mut source = ReplayForkSource::from_parts(
            origin,
            owner,
            self.escrow.clone(),
            &self.rows,
            self.payloads.clone(),
        )?;
        if owner.head().is_none() {
            source.append_raw_controls(&self.controls)?;
        }
        Ok(source)
    }

    /// A raw child replays original lifecycle rows instead of manufacturing a
    /// summary. Existing controls must qualify against their original window;
    /// new controls come only from this actual live owner's witnessed states.
    fn capture_raw_controls(
        &mut self,
        owner: &ReplayEpochContext,
        previous: Option<&Self>,
        durable: Option<&ReplayEpochSnapshot>,
        rows: &[TranscriptEntry],
    ) -> Result<()> {
        use mez_agent::transcript::TranscriptExecutionStateRecord;
        let projected = ReplayForkSource::from_parts(
            None,
            owner,
            self.escrow.clone(),
            &self.rows,
            BTreeMap::new(),
        )?;
        let mut selected = previous.map_or_else(Vec::new, |source| source.controls.clone());
        if let Some(first) = rows.first() {
            // Check original ordering before rebinding any physical row position.
            super::qualify_execution_controls(durable, &first.conversation_id, rows)?;
        }
        for row in rows {
            if row.role == TranscriptRole::System
                && TranscriptExecutionStateRecord::from_transcript_content(&row.content)?.is_some()
            {
                selected.push((ContextBlockOccurrenceId::allocate(), row.clone()));
            }
        }
        for state in owner.execution_states()? {
            let first = state
                .members()
                .first()
                .ok_or_else(|| invalid("empty raw lifecycle"))?
                .0;
            let mut row = projected
                .rows()
                .iter()
                .find(|row| {
                    TranscriptContextRecord::from_transcript_content(&row.content)
                        .ok()
                        .flatten()
                        .is_some_and(|record| record.record().occurrence_id() == first)
                })
                .cloned()
                .ok_or_else(|| invalid("raw lifecycle original producer is missing"))?;
            row.content = TranscriptExecutionStateRecord::new(state)?.to_transcript_content()?;
            selected.push((ContextBlockOccurrenceId::allocate(), row));
        }
        let mut qualified = projected.rows().to_vec();
        let mut seen = BTreeSet::new();
        for (origin, mut row) in selected {
            if !seen.insert((
                row.turn_id.clone(),
                row.agent_id.clone(),
                row.pane_id.clone(),
                row.content.clone(),
            )) {
                continue;
            }
            row.conversation_id = owner.conversation_id().as_str().into();
            row.sequence = qualified.len() as u64 + 1;
            qualified.push(row.clone());
            self.controls.push((origin, row));
        }
        escrow::qualify_controls(
            &self.escrow,
            owner.conversation_id().as_str(),
            &qualified,
            &BTreeMap::new(),
        )?;
        Ok(())
    }
}
