//! Bounded descriptive source retained by an actual NoStore controller.
//!
//! This immutable value has no files, append owner or execution capability.
//! Current raw metadata keeps its actual original producer across memory-only
//! rewrites and turn cleanup. An explicit fork projects those originals into
//! independent child rows; only that child's actual initial publication owner
//! (an initial receipt or saved-fork generation) grants durability.

use super::*;
use mez_agent::{
    ContextBlockRecord, TranscriptEntry, TranscriptRole, transcript::TranscriptContextRecord,
};

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
    /// Freezes an already rendered historical prefix without publishing or
    /// rereading its parent. Placement and costs come from that exact read;
    /// covered task dependencies use its audit projection only when required.
    /// No archived custody, execution interval, or append receipt is inferred.
    pub(crate) fn capture_legacy_prefix(
        owner: &ReplayEpochContext,
        epoch: &super::super::AgentCompactionEpoch,
        rows: &[TranscriptEntry],
        origins: &super::super::TranscriptRowOrigins,
        records: &super::super::TranscriptRowRecords,
        resolve_dependencies: impl FnOnce() -> Result<(AgentContext, Vec<(u64, ContextBlockRecord)>)>,
    ) -> Result<Self> {
        owner.checkpoint().map_err(context_error)?;
        let head = owner
            .context()
            .chronology()
            .first()
            .ok_or_else(|| invalid("frozen legacy prefix has no original head"))?;
        if epoch.version != 3
            || !epoch.ranges.is_empty()
            || epoch.conversation_id != owner.conversation_id().as_str()
            || epoch.summary != head.block().content
            || epoch
                .summary_records
                .as_ref()
                .and_then(|records| records.prefix)
                != Some(head.record())
            || owner.head().is_none()
            || owner.revision().get() != 1
            || !owner.execution_states().map_err(context_error)?.is_empty()
            || owner
                .context()
                .chronology()
                .iter()
                .filter_map(|event| event.execution_group_id())
                .any(|group| !owner.context().execution_group_is_pending(group))
        {
            return Err(invalid(
                "frozen legacy prefix differs from its original authority",
            ));
        }
        Self::capture_legacy_source(
            owner,
            epoch.through_sequence,
            rows,
            origins,
            records,
            resolve_dependencies,
        )
    }

    /// Freezes the actual checked raw projection before its first head. Original
    /// cached records and producer images qualify every member. Unknown native
    /// groups remain protected; this supplies no completion or append receipt.
    pub(crate) fn capture_legacy_raw(
        owner: &ReplayEpochContext,
        rows: &[TranscriptEntry],
        origins: &super::super::TranscriptRowOrigins,
        records: &super::super::TranscriptRowRecords,
    ) -> Result<Self> {
        if owner.head().is_some() || owner.revision().get() != 0 {
            return Err(invalid("raw source capture already has summary authority"));
        }
        let covered = rows.first().map_or(0, |row| row.sequence.saturating_sub(1));
        let mut capture =
            Self::capture_legacy_source(owner, covered, rows, origins, records, || {
                Err(invalid(
                    "raw source dependency is outside its original projection",
                ))
            })?;
        // A headless initial copy has no manifest checkpoint to carry original
        // lifecycle. Keep its checked zero-model controls beside the model rows;
        // only the child's existing initial receipt can make that copy durable.
        // A zero-model legacy window (for example an MCP boundary alone) is
        // valid no-work history, not an empty required publication escrow.
        if !owner.members().is_empty() {
            capture.capture_raw_controls(owner, None, None, rows)?;
        }
        Ok(capture)
    }

    fn capture_legacy_source(
        owner: &ReplayEpochContext,
        covered: u64,
        rows: &[TranscriptEntry],
        origins: &super::super::TranscriptRowOrigins,
        records: &super::super::TranscriptRowRecords,
        resolve_dependencies: impl FnOnce() -> Result<(AgentContext, Vec<(u64, ContextBlockRecord)>)>,
    ) -> Result<Self> {
        let mut by_id = BTreeMap::new();
        let mut previous = 0;
        for row in rows {
            if row.conversation_id != owner.conversation_id().as_str() || row.sequence <= previous {
                return Err(invalid("frozen legacy row window changed original order"));
            }
            previous = row.sequence;
            let authored = if row.role == TranscriptRole::System {
                TranscriptContextRecord::from_transcript_content(&row.content)?
            } else {
                None
            };
            let id = authored
                .as_ref()
                .map(|record| record.record().occurrence_id())
                .or_else(|| origins.get(&row.sequence).copied());
            if let Some(id) = id
                && by_id.insert(id, row).is_some()
            {
                return Err(invalid("frozen legacy occurrence placement is duplicated"));
            }
        }
        let current = owner
            .members()
            .iter()
            .map(|member| member.occurrence_id())
            .collect::<BTreeSet<_>>();
        let dependency_records = std::cell::RefCell::new(BTreeMap::new());
        let qualified = escrow::qualify_legacy_prefix(
            owner,
            escrow::LegacyPrefixRows {
                covered,
                through: rows.last().map_or(covered, |row| row.sequence),
                rows,
                archived: false,
            },
            |id| {
                by_id
                    .get(&id)
                    .copied()
                    .ok_or_else(|| invalid("frozen legacy member has no original row placement"))
            },
            |placements| {
                let dependencies = dependency_records.borrow();
                Ok(placements
                    .iter()
                    .filter_map(|(sequence, id)| {
                        (if current.contains(id) {
                            records.get(sequence)
                        } else {
                            dependencies.get(sequence).or_else(|| records.get(sequence))
                        })
                        .filter(|record| record.occurrence_id() == *id)
                        .map(|record| (*sequence, *record))
                    })
                    .collect())
            },
            || {
                let (context, updates) = resolve_dependencies()?;
                dependency_records.borrow_mut().extend(updates);
                Ok(context)
            },
        )?;
        let captured = qualified
            .items
            .iter()
            .map(|item| {
                by_id
                    .get(&item.occurrence_id())
                    .map(|row| (*row).clone())
                    .ok_or_else(|| invalid("frozen legacy dependency lost its original row"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            escrow: qualified.items,
            rows: captured,
            payloads: qualified.payloads,
            controls: Vec::new(),
        })
    }

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
        if !needed.is_empty() || owner.revision().get() > 0 {
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
        if owner.revision().get() == 0 && !needed.is_empty() {
            capture.capture_raw_controls(owner, previous, durable, rows)?;
        }
        Ok(capture)
    }

    /// Creates descriptive memory provenance, never a made-up committed parent
    /// identity. Native wire projection remains independently checked.
    pub(crate) fn fork_source(&self, owner: &ReplayEpochContext) -> Result<ReplayForkSource> {
        let origin = (owner.revision().get() > 0).then(|| {
            initial::ForkOrigin::Memory(copy::MemoryCopyOrigin {
                conversation: owner.conversation_id().as_str().into(),
                epoch_revision: owner.revision().get(),
                head: owner.head().map(|head| head.occurrence_id()),
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
        if owner.revision().get() == 0 {
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
