//! Bounded read-only settlement/dependency inspection of one retired original.
//!
//! The actual lower input retains source-absence evidence and charged scratch.
//! One owned SQLite snapshot validates the whole settled store while retaining
//! only this original's frozen slot accumulator. Shared cumulative ACKs and
//! native/canonical journals remain explicit dependency blockers, never deletion
//! permission. A fixed-size shared-ACK closure can be inspected in the same
//! snapshot; native references remain blockers. Neither observation is a cleanup
//! plan, intent or reclamation worker.

use mez_agent::messaging::{
    DeliveryTerminalReason, MessageSourceRetirementInspection, MessageStorageScope,
};
use std::sync::Arc;

use super::inventory::{CustodyInventoryEntry, CustodyJournalKind as Journal, CustodyScanPosition};
use super::ownership::OwnedCustodyStore;
use super::*;

mod commitment;
pub(crate) use commitment::JournalCommitment;
use commitment::JournalHasher;
pub(crate) use commitment::SourceEvidenceCommitment;

mod preservation;
use preservation::PreservationBuffer;
pub(crate) use preservation::PreservedSourceEvidence;

/// Fits the separately charged 256 KiB accumulator, including vector overhead.
const MAX_SLOTS: usize = 4096;
const SLOT_METADATA_BYTES: usize = 256 * 1024;
/// A bounded analysis quantum, not a larger admission or cleanup batch limit.
const MAX_CANDIDATE_SOURCES: usize = 64;
/// Covers source results, their small store identities and the sequence queue.
/// This shares the existing slot accumulator lease, not additional uncharged RAM.
const CANDIDATE_SOURCE_CHARGE: usize = 384;
const CANDIDATE_METADATA_BYTES: usize = 4096 + MAX_CANDIDATE_SOURCES * CANDIDATE_SOURCE_CHARGE;

#[derive(Clone, Copy, Default)]
struct Slot {
    acknowledged: bool,
    terminal: Option<(DeliveryTerminalReason, u64)>,
    released: Option<(DeliveryTerminalReason, u64)>,
}

/// Descriptive facts only. Even a fully settled isolated source still requires
/// a closed dependency plan, preserved native evidence and reserved crash-safe
/// cleanup before physical deletion. A result cannot be used at a new position.
pub(crate) struct SourceRetirementInspection {
    pub(super) position: CustodyScanPosition,
    pub sequence: u64,
    pub source_sha256: [u8; 32],
    pub recipient_count: usize,
    pub unsettled_recipients: usize,
    pub shared_acknowledgements: usize,
    pub native_journal_references: usize,
    /// p1/u1 are unfinished canonical preparations, never settled evidence.
    pub pending_native_journal_references: usize,
    /// Exact k1/t1/d1/r1/s1/p1/c1/u1/h1 rows linked to this original.
    pub dependencies: JournalCommitment,
    /// All b1/n1/o1 rows are conservatively preserved, not selected for deletion.
    pub preserved_registrations: JournalCommitment,
    /// Exact payloadless preservation record size/checksum, not retained bytes.
    pub source_evidence: SourceEvidenceCommitment,
}

/// Descriptive transitive closure of shared cumulative ACK source references.
/// Every member has no live source under the original input and is read in one
/// SQLite snapshot. Unsettled slots and native references remain explicit in
/// `sources`; even an all-settled result grants no deletion or rewrite authority.
/// A closure exceeding the fixed quantum refuses in full, never returns a
/// partial candidate that could be mistaken for dependency closure.
pub(crate) struct SourceRetirementCandidate {
    sources: Vec<SourceRetirementInspection>,
    /// Output storage remains charged through its actual last owner.
    _input: Arc<MessageSourceRetirementInspection>,
    preservation: Option<PreservationBuffer>,
    rewrite: Option<Arc<super::physical_capacity::RewriteCapacityReservation>>,
}

impl SourceRetirementCandidate {
    /// Borrows descriptive results without detaching them from their lease.
    pub(crate) fn sources(&self) -> &[SourceRetirementInspection] {
        &self.sources
    }

    /// Bodyless encodings exist only after separately charged preparation and
    /// complete validation. Native blockers remain; these are not live sources.
    pub(crate) fn preserved_sources(&self) -> Option<&[PreservedSourceEvidence]> {
        self.preservation.as_ref().map(PreservationBuffer::records)
    }

    /// Quantitative reserved headroom only, never permission to delete originals.
    pub(crate) fn reserved_rewrite_bytes(&self) -> usize {
        self.rewrite
            .as_ref()
            .map_or(0, |reservation| reservation.bytes())
    }

    /// Called by the original root only after its worker qualified this lease's
    /// exact owner/position and the complete bodyless candidate. No lease copy.
    pub(crate) fn retain_rewrite_capacity(
        &mut self,
        reservation: Arc<super::physical_capacity::RewriteCapacityReservation>,
    ) -> Result<()> {
        if self.preservation.is_none() || self.rewrite.is_some() {
            return Err(unavailable());
        }
        self.rewrite = Some(reservation);
        Ok(())
    }

    /// Deterministic version-two commitment to the sorted closure and its exact
    /// validated store position. This does not confer deletion authority or
    /// replace live ownership/adoption checks. Equal member counts are not enough.
    pub(crate) fn commitment(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"mezzanine-mmp-retirement-candidate-v2\0");
        if let Some(first) = self.sources.first() {
            hash.update((first.position.store_id.len() as u64).to_le_bytes());
            hash.update(&first.position.store_id);
            hash.update(first.position.generation.to_le_bytes());
            hash.update(first.position.sequence_watermark.to_le_bytes());
            hash.update([u8::from(first.position.watermark_origin_trusted)]);
        }
        hash.update((self.sources.len() as u64).to_le_bytes());
        for source in &self.sources {
            hash.update(source.sequence.to_le_bytes());
            hash.update(source.source_sha256);
            hash.update(source.dependencies.records.to_le_bytes());
            hash.update(source.dependencies.sha256);
            hash.update(source.preserved_registrations.records.to_le_bytes());
            hash.update(source.preserved_registrations.sha256);
            hash.update(source.source_evidence.encoded_bytes.to_le_bytes());
            hash.update(source.source_evidence.sha256);
        }
        hash.finalize().into()
    }

    /// Adoption checks the actual issuing lower service, not serialized labels.
    pub(crate) fn is_original(&self, service: &mez_agent::messaging::MessageService) -> bool {
        service.source_retirement_inspection_is_original(&self._input)
    }
}

/// The exact source whose scalar settlement checks failed. A repair must cover
/// this source, not merely another individually valid occurrence in the store.
#[derive(Debug)]
pub(crate) struct SourceRetirementFailure {
    pub error: MezError,
    pub sequence: u64,
}

struct CandidateQueue {
    sequences: [u64; MAX_CANDIDATE_SOURCES],
    len: usize,
    refused: bool,
}

impl CandidateQueue {
    fn insert(&mut self, input: &MessageSourceRetirementInspection, sequence: u64) -> Result<()> {
        if !input.has_no_live_source(sequence) {
            self.refused = true;
            return Ok(());
        }
        if !self.sequences[..self.len].contains(&sequence) {
            if self.len == MAX_CANDIDATE_SOURCES {
                self.refused = true;
                return Ok(());
            }
            self.sequences[self.len] = sequence;
            self.len += 1;
        }
        Ok(())
    }
}

impl OwnedCustodyStore<'_> {
    /// Follows shared ACK dependencies without materializing source payloads or
    /// a second inventory index. At most 64 full scans reuse the same charged
    /// decoder and slot scratch. This synchronous method belongs in an owned
    /// blocking worker; no runtime scheduling/cleanup lane is installed here.
    /// Admission and adoption must separately revalidate the exact lower input.
    /// `None` is a typed live-source/quantum refusal, never a validation error.
    /// A refused member's scan still completes its slot/inventory checks before
    /// returning None; no weaker fallback may erase an observed integrity error.
    pub(crate) fn inspect_source_retirement_candidate(
        &mut self,
        input: &mut Arc<MessageSourceRetirementInspection>,
        scope: &MessageStorageScope,
        sequence: u64,
    ) -> std::result::Result<Option<SourceRetirementCandidate>, SourceRetirementFailure> {
        self.inspect_source_retirement_with_preservation(input, scope, sequence, None)
    }

    /// Optional in-memory staging must already be funded by the actual lower
    /// service. It changes no store rows and grants no disk/rewrite authority.
    pub(crate) fn inspect_source_retirement_with_preservation(
        &mut self,
        input: &mut Arc<MessageSourceRetirementInspection>,
        scope: &MessageStorageScope,
        sequence: u64,
        staging: Option<Arc<mez_agent::messaging::MessageSourcePreservationReservation>>,
    ) -> std::result::Result<Option<SourceRetirementCandidate>, SourceRetirementFailure> {
        let mut failed_sequence = sequence;
        let result = (|| -> Result<Option<SourceRetirementCandidate>> {
            // One lower input funds one retained result. While a prior result or
            // another worker/weak observer retains it, refuse instead of multiplying
            // that loan. Exclusive access plus get_mut prevents a concurrent upgrade.
            if Arc::get_mut(input).is_none() || !input.belongs_to(scope) {
                return Err(unavailable());
            }
            if staging.as_ref().is_some_and(|s| !s.belongs_to(input)) {
                return Err(unavailable());
            }
            let mut preservation = staging.map(PreservationBuffer::new).transpose()?;
            self.revalidate()?;
            let store = self.store_mut();
            let transaction = store
                .connection
                .unchecked_transaction()
                .map_err(database_error)?;
            let mut queue = CandidateQueue {
                sequences: [0; MAX_CANDIDATE_SOURCES],
                len: 0,
                refused: false,
            };
            queue.insert(input, sequence)?;
            let mut sources = Vec::new();
            sources
                .try_reserve_exact(MAX_CANDIDATE_SOURCES)
                .map_err(|_| unavailable())?;
            let mut index = 0;
            while index < queue.len && !queue.refused {
                let sequence = queue.sequences[index];
                failed_sequence = sequence;
                sources.push(inspect_in_snapshot(
                    store,
                    &transaction,
                    input,
                    sequence,
                    &mut |dependency| queue.insert(input, dependency),
                    preservation.as_mut(),
                )?);
                index += 1;
            }
            sources.sort_unstable_by_key(|source| source.sequence);
            store.finish_transaction(transaction)?;
            self.revalidate()?;
            if queue.refused || preservation.as_ref().is_some_and(|p| p.refused) {
                return Ok(None);
            }
            if let Some(preservation) = &mut preservation {
                preservation.sort();
            }
            Ok(Some(SourceRetirementCandidate {
                sources,
                _input: input.clone(),
                preservation,
                rewrite: None,
            }))
        })();
        result.map_err(|error| SourceRetirementFailure {
            error,
            sequence: failed_sequence,
        })
    }

    /// Reads original facts under the live directory/database fence. The root
    /// must also validate the input against its exact current MessageService at
    /// admission/adoption. No row, watermark, clock or receipt is changed here.
    pub(crate) fn inspect_source_retirement(
        &mut self,
        input: &MessageSourceRetirementInspection,
        scope: &MessageStorageScope,
        sequence: u64,
    ) -> Result<SourceRetirementInspection> {
        if !input.belongs_to(scope) || !input.has_no_live_source(sequence) {
            return Err(unavailable());
        }
        self.revalidate()?;
        let result = inspect(self.store_mut(), input, sequence)?;
        self.revalidate()?;
        Ok(result)
    }
}

fn inspect(
    store: &MmpCustodyStore,
    input: &MessageSourceRetirementInspection,
    sequence: u64,
) -> Result<SourceRetirementInspection> {
    let transaction = store
        .connection
        .unchecked_transaction()
        .map_err(database_error)?;
    let result = inspect_in_snapshot(store, &transaction, input, sequence, &mut |_| Ok(()), None)?;
    store.finish_transaction(transaction)?;
    Ok(result)
}

fn inspect_in_snapshot(
    store: &MmpCustodyStore,
    transaction: &rusqlite::Transaction<'_>,
    input: &MessageSourceRetirementInspection,
    sequence: u64,
    dependency: &mut dyn FnMut(u64) -> Result<()>,
    preservation: Option<&mut PreservationBuffer>,
) -> Result<SourceRetirementInspection> {
    let limits = input.decode_limits();
    let (source, source_sha256, encoded_bytes) =
        store.read_accepted_metadata_in_snapshot(transaction, sequence, limits)?;
    let recipient_count = source.recipient_bindings().len();
    let source_evidence = commitment::source_evidence(&source, &source_sha256, encoded_bytes)?;
    if let Some(preservation) = preservation {
        preservation.capture(&source, &source_sha256, encoded_bytes, &source_evidence)?;
    }
    drop(source);
    if recipient_count == 0
        || recipient_count > MAX_SLOTS
        || recipient_count
            .saturating_mul(std::mem::size_of::<Slot>())
            .saturating_add(CANDIDATE_METADATA_BYTES)
            > SLOT_METADATA_BYTES
    {
        return Err(unavailable());
    }
    let mut slots = Vec::new();
    slots
        .try_reserve_exact(recipient_count)
        .map_err(|_| unavailable())?;
    slots.resize(recipient_count, Slot::default());
    let mut shared_acknowledgements = 0usize;
    let mut native_journal_references = 0usize;
    let mut pending_native_journal_references = 0usize;
    let mut dependencies = JournalHasher::new(
        b"mezzanine-mmp-retirement-dependencies-v1\0",
        sequence,
        &source_sha256,
    );
    let mut preserved_registrations = JournalHasher::new(
        b"mezzanine-mmp-retirement-registrations-v1\0",
        sequence,
        &source_sha256,
    );
    let mut original_seen = false;
    let mut visit = |entry: CustodyInventoryEntry<'_>| {
        match entry {
            CustodyInventoryEntry::Occurrence(original) if original.sequence == sequence => {
                if original_seen
                    || original.digest != source_sha256
                    || original.recipient_count != recipient_count
                {
                    return Err(unavailable());
                }
                original_seen = true;
            }
            CustodyInventoryEntry::Occurrence(_) | CustodyInventoryEntry::Aborted(_) => (),
            CustodyInventoryEntry::Journal(kind, key) => match kind {
                Journal::Acknowledgement => {
                    let ack =
                        store.read_stored_acknowledgement_in_snapshot(transaction, key, limits)?;
                    if ack.decisions.iter().any(|d| d.sequence == sequence) {
                        dependencies.observe(transaction, key)?;
                        for decision in &ack.decisions {
                            dependency(decision.sequence)?;
                        }
                        shared_acknowledgements +=
                            usize::from(ack.decisions.iter().any(|d| d.sequence != sequence));
                        for decision in ack.decisions.iter().filter(|d| d.sequence == sequence) {
                            if decision.acknowledged {
                                slot(&mut slots, decision.binding_index)?.acknowledged = true;
                            }
                            // An expiry exclusion alone does not release the
                            // sender's independently retained terminal outcome.
                        }
                    }
                }
                Journal::Expiry => {
                    let frame = store.read_stored_expiry_in_snapshot(transaction, key, limits)?;
                    if frame.sequence == sequence {
                        dependencies.observe(transaction, key)?;
                        for index in frame.binding_indices {
                            terminal(
                                slot(&mut slots, index)?,
                                DeliveryTerminalReason::Expired,
                                frame.observed_at_ms,
                            )?;
                        }
                    }
                }
                Journal::Retirement => {
                    let frame =
                        store.read_stored_retirement_in_snapshot(transaction, key, limits)?;
                    if frame.sequence == sequence {
                        dependencies.observe(transaction, key)?;
                        terminal(
                            slot(&mut slots, frame.binding_index)?,
                            DeliveryTerminalReason::RecipientRetired,
                            frame.observed_at_ms,
                        )?;
                    }
                }
                Journal::Release => {
                    let frame = store.read_stored_release_in_snapshot(transaction, key, limits)?;
                    if frame.sequence == sequence {
                        dependencies.observe(transaction, key)?;
                        for (index, first, reason) in frame.entries {
                            release(slot(&mut slots, index)?, reason, first)?;
                        }
                    }
                }
                Journal::Consumption => {
                    let frame =
                        store.read_stored_consumption_in_snapshot(transaction, key, limits)?;
                    if frame.sequence == sequence {
                        dependencies.observe(transaction, key)?;
                        release(
                            slot(&mut slots, frame.binding_index)?,
                            frame.terminal.reason,
                            frame.terminal.observed_at_ms,
                        )?;
                    }
                }
                Journal::NativeInput => {
                    let journal = store
                        .read_native_input_in_snapshot(transaction, key, limits)?
                        .ok_or_else(unavailable)?;
                    if journal.original_sequence() == sequence {
                        native_journal_references += 1;
                        pending_native_journal_references += usize::from(key.starts_with("p1:"));
                        dependencies.observe(transaction, key)?;
                    }
                }
                Journal::TerminalHandoff => {
                    let journal = store
                        .read_handoff_in_snapshot(transaction, key, limits)?
                        .ok_or_else(unavailable)?;
                    if journal.original_sequence() == sequence {
                        native_journal_references += 1;
                        pending_native_journal_references += usize::from(key.starts_with("u1:"));
                        dependencies.observe(transaction, key)?;
                    }
                }
                // Registration provenance is deliberately preserved. This
                // observation never proposes deletion of b1/n1/o1 records.
                Journal::NativeContinuity | Journal::NativeLifetime | Journal::RegistrationEnd => {
                    preserved_registrations.observe(transaction, key)?;
                }
            },
        }
        Ok(())
    };
    // The lower input already owns full source/scratch/slot/framing leases.
    // Give the scanner no allowance for a second retained reference index.
    let position =
        store.visit_inventory_in_snapshot(transaction, limits, 8 * 1024 * 1024, &mut visit)?;
    if !original_seen
        || !position.watermark_origin_trusted
        || sequence > position.sequence_watermark
    {
        return Err(unavailable());
    }
    for slot in &slots {
        if (slot.acknowledged && slot.terminal.is_some())
            || (slot.released.is_some() && slot.released != slot.terminal)
        {
            return Err(unavailable());
        }
    }
    let unsettled_recipients = slots
        .iter()
        .filter(|s| !s.acknowledged && s.released.is_none())
        .count();
    Ok(SourceRetirementInspection {
        position,
        sequence,
        source_sha256,
        recipient_count,
        unsettled_recipients,
        shared_acknowledgements,
        native_journal_references,
        pending_native_journal_references,
        dependencies: dependencies.finish(),
        preserved_registrations: preserved_registrations.finish(),
        source_evidence,
    })
}

fn slot(slots: &mut [Slot], index: u32) -> Result<&mut Slot> {
    slots.get_mut(index as usize).ok_or_else(unavailable)
}

fn terminal(slot: &mut Slot, reason: DeliveryTerminalReason, first: u64) -> Result<()> {
    let observation = (reason, first);
    if slot.terminal.is_some_and(|prior| prior != observation) {
        return Err(unavailable());
    }
    slot.terminal = Some(observation);
    Ok(())
}

fn release(slot: &mut Slot, reason: DeliveryTerminalReason, first: u64) -> Result<()> {
    if slot.released.replace((reason, first)).is_some() {
        return Err(unavailable());
    }
    Ok(())
}

fn unavailable() -> MezError {
    MezError::invalid_state("MMP original source retirement inspection unavailable")
}

#[cfg(test)]
mod tests;
