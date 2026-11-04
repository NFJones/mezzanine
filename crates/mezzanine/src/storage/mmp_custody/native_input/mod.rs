//! Native canonical input preparation and commit on the original custody store.
//!
//! `p1` freezes one bodyless physical transcript append plan before input is
//! written. `c1` replaces it only after the actual original row is read and
//! synced. Source bodies remain in acceptance storage and the canonical input
//! archive. A retained first plan prevents retries from resampling a replacement
//! destination. Reads validate source attribution inside the same SQL snapshot;
//! writes fingerprint every non-target row before and after SQL effects. Neither
//! descriptive inventory nor imported transcript text grants ACK authority.

use std::sync::OnceLock;

use mez_agent::messaging::{
    MessageNativeInputAnchor, MessageNativeInputCommit, MessageNativeInputRecord,
    MessageNativeInputStore,
};
use mez_agent::transcript::{
    TranscriptContextEvent, TranscriptEntry, TranscriptNativeInputSource, TranscriptRole,
};
use serde::{Deserialize, Serialize};

use super::{integrity::unaffected_rows_digest, *};
use crate::storage::transcript::{
    AgentTranscriptStore, CustodyAppendReceipt,
    custody::{CustodyAppendPlan, CustodyExpectation, CustodyReceiptKind, CustodyTranscriptPlan},
};

/// Bounded metadata only; no native payload or transcript content is stored here.
const MAX_JOURNAL_BYTES: usize = 32 * 1024;

mod projection;
pub(crate) use projection::NativeInputProjection;

/// Descriptive source and original physical append receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeInputJournal {
    version: u32,
    generation: u64,
    source: TranscriptNativeInputSource,
    transcript: CustodyTranscriptPlan,
}

/// Immutable first preparation retained outside the actual worker through loss.
struct Preparation {
    journal: NativeInputJournal,
    generation: u64,
    watermark: u64,
    unaffected: [u8; 32],
}

/// Exact final transaction retained before any c1 publication attempt.
struct Completion {
    journal: NativeInputJournal,
    generation: u64,
    watermark: u64,
    unaffected: [u8; 32],
    pending_digest: [u8; 32],
}

/// Original target and application input owned by the serialized root worker.
pub(crate) struct NativeInputAttempt {
    transcript: AgentTranscriptStore,
    row: Option<std::sync::Arc<NativeInputProjection>>,
    /// First original canonical position survives failure before publication.
    canonical_first: OnceLock<(CustodyAppendPlan, Vec<TranscriptEntry>)>,
    /// Original kernel/physical owner survives joined failure and backoff before
    /// the first canonical receipt is published; no later producer can reseat it.
    lease: OnceLock<crate::storage::transcript::custody::CustodyTranscriptLease>,
    /// Actual private row image survives worker loss and does not become a task.
    canonical: OnceLock<std::sync::Arc<CustodyAppendReceipt>>,
    first: OnceLock<Preparation>,
    completion: OnceLock<Completion>,
}
impl NativeInputAttempt {
    /// Captures a real post-context projection or a lookup-only request. The
    /// actual lower anchor/source and typed row are checked again at execution.
    pub(crate) fn new(
        transcript: AgentTranscriptStore,
        row: Option<std::sync::Arc<NativeInputProjection>>,
    ) -> Self {
        Self {
            transcript,
            row,
            canonical_first: OnceLock::new(),
            lease: OnceLock::new(),
            canonical: OnceLock::new(),
            first: OnceLock::new(),
            completion: OnceLock::new(),
        }
    }
}

/// Trusted original-store callback; only its real I/O may seal a lower proof.
pub(crate) struct NativeInputPort<'a> {
    store: &'a mut MmpCustodyStore,
    attempt: &'a NativeInputAttempt,
}
impl MmpCustodyStore {
    /// Observes actual decoder work in root tests; native lookup must qualify
    /// source fingerprints without materializing an additional payload body.
    #[cfg(test)]
    pub(crate) fn native_input_source_decode_bytes(&self) -> usize {
        self.source_decode_probe.maximum_payload.get()
    }

    /// Borrows the retained original attempt; caller owns the root filesystem
    /// fence and the exclusive lane for the entire callback.
    pub(crate) fn native_input_port<'a>(
        &'a mut self,
        attempt: &'a NativeInputAttempt,
    ) -> NativeInputPort<'a> {
        NativeInputPort {
            store: self,
            attempt,
        }
    }

    /// Validates a bodyless journal and its immutable accepted native source in
    /// one snapshot. Inventory exposes no canonical capability from these DTOs.
    pub(super) fn read_native_input_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
        limits: mez_agent::messaging::MessageAcceptanceDecodeLimits,
    ) -> Result<Option<NativeInputJournal>> {
        let Some(journal) = self.read_native_input_metadata_in_snapshot(transaction, key)? else {
            return Ok(None);
        };
        let (source, digest, _) = self.read_accepted_source_in_snapshot(
            transaction,
            journal.source.message_sequence,
            limits,
        )?;
        let index = journal.source.binding_index as usize;
        let binding = source.recipient_bindings().get(index).ok_or_else(invalid)?;
        if digest != journal.source.source_sha256
            || binding.recipient != journal.source.recipient
            || binding.registered_after_sequence != journal.source.registered_after_sequence
            || source.native_recipient(index) != Some(&journal.source.native_owner)
            || source.envelope().ttl_ms.is_some_and(|ttl| {
                source.accepted_at_ms().saturating_add(ttl) < journal.source.offered_at_ms
            })
        {
            return Err(invalid());
        }
        Ok(Some(journal))
    }
    /// Reads only bounded original journal metadata. Live root work separately
    /// compares it with an opaque, already-qualified source anchor and streams
    /// that source fingerprint. It must not decode an extra unreserved payload.
    fn read_native_input_metadata_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
    ) -> Result<Option<NativeInputJournal>> {
        let Some(bytes) = self.read(key)? else {
            return Ok(None);
        };
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(invalid());
        }
        let journal: NativeInputJournal = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        let state = self.checked_state(transaction)?;
        journal.source.validate().map_err(|_| invalid())?;
        journal.transcript.validate()?;
        if journal.version != 1
            || journal.generation == 0
            || journal.generation > state.generation as u64
            || journal.source.store_id.as_slice() != self.store_id
            || journal.source.message_sequence > state.watermark
            || journal.transcript.conversation_id() != journal.source.native_owner.conversation_id()
            || (key != record_key("p1", &journal.source.occurrence())
                && key != record_key("c1", &journal.source.occurrence()))
        {
            return Err(invalid());
        }
        Ok(Some(journal))
    }
}

impl MessageNativeInputStore for NativeInputPort<'_> {
    /// Returns a sealed result only after real original-root journal/row checks.
    fn verify(
        &mut self,
        anchor: &MessageNativeInputAnchor,
    ) -> mez_agent::messaging::Result<Option<MessageNativeInputRecord>> {
        self.execute(anchor).map_err(|error| {
            mez_agent::messaging::MessageError::invalid_state(format!(
                "MMP canonical native input: {error}"
            ))
        })
    }
}
impl NativeInputPort<'_> {
    /// Existing c1 is historical commit proof even after legitimate archive
    /// cleanup or context replacement. An unfinished p1 always retains its first
    /// target; qualified absence requires both journals to be absent.
    fn execute(
        &mut self,
        anchor: &MessageNativeInputAnchor,
    ) -> Result<Option<MessageNativeInputRecord>> {
        let store_id: [u8; 16] = self
            .store
            .store_id
            .as_slice()
            .try_into()
            .map_err(|_| invalid())?;
        let occurrence = occurrence(anchor, store_id);
        let pending_key = record_key("p1", &occurrence);
        let committed_key = record_key("c1", &occurrence);
        let transaction = self
            .store
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        if self
            .store
            .validate_accepted_occurrence_in_snapshot(&transaction, anchor.sequence())?
            != anchor.source_sha256()
        {
            return Err(invalid());
        }
        self.store.ensure_slot_not_expired_in_snapshot(
            &transaction,
            anchor.sequence(),
            anchor.binding_index() as u32,
        )?;
        let committed = self
            .store
            .read_native_input_metadata_in_snapshot(&transaction, &committed_key)?;
        let pending = self
            .store
            .read_native_input_metadata_in_snapshot(&transaction, &pending_key)?;
        if committed.is_some() && pending.is_some() {
            return Err(invalid());
        }
        self.store.finish_transaction(transaction)?;
        if let Some(committed) = committed {
            validate_anchor(&committed, anchor, store_id)?;
            if self.attempt.first.get().is_some_and(|first| {
                first.journal.source != committed.source
                    || first.journal.transcript != committed.transcript
            }) {
                return Err(invalid());
            }
            if let Some(completion) = self.attempt.completion.get() {
                if completion.journal != committed {
                    return Err(invalid());
                }
                self.publish_completion(completion)?;
            }
            self.retire_canonical_receipt(&committed)?;
            return Ok(Some(receipt(&committed, false)));
        }
        if self.attempt.lease.get().is_none() {
            let (canonical, lease) = self.attempt.transcript.recover_custody_lease(
                anchor,
                &store_id,
                &occurrence,
                pending
                    .as_ref()
                    .map(|p| CustodyExpectation::NativeInput(&p.source, &p.transcript)),
                self.attempt.row.is_some(),
            )?;
            if let Some(canonical) = canonical {
                if self
                    .attempt
                    .canonical_first
                    .get()
                    .is_some_and(|(p, _)| p != canonical.plan())
                {
                    return Err(invalid());
                }
                self.attempt
                    .canonical
                    .set(canonical)
                    .map_err(|_| invalid())?;
            }
            if let Some(lease) = lease {
                self.attempt.lease.set(lease).map_err(|_| invalid())?;
            } else {
                return Ok(None);
            }
        }
        let lease = self.attempt.lease.get().ok_or_else(invalid)?;
        if self.attempt.canonical.get().is_none() && pending.is_none() {
            if self.attempt.canonical_first.get().is_none() {
                let projection = self.attempt.row.as_ref().ok_or_else(invalid)?;
                let mut row = projection.materialize(anchor, store_id)?;
                let mut prefix = projection.prefix().to_vec();
                let plan = lease.prepare_custody_append(&mut prefix, &mut row)?;
                prefix.push(row);
                self.attempt
                    .canonical_first
                    .set((plan, prefix))
                    .map_err(|_| invalid())?;
            }
            // Retain both placement and selected rows before publication. A
            // retry must not deduplicate anew against a later archive suffix.
            let (plan, rows) = self.attempt.canonical_first.get().ok_or_else(invalid)?;
            let canonical = lease.publish_append_receipt(plan.clone(), rows)?;
            self.attempt
                .canonical
                .set(canonical)
                .map_err(|_| invalid())?;
        }
        if let Some(canonical) = self.attempt.canonical.get() {
            canonical.revalidate()?;
            lease.append_custody_prefix(canonical.plan(), &canonical.rows()?)?;
        }
        let pending = match pending {
            Some(pending) => {
                validate_anchor(&pending, anchor, store_id)?;
                if self
                    .attempt
                    .first
                    .get()
                    .is_some_and(|first| first.journal != pending)
                {
                    return Err(invalid());
                }
                pending
            }
            None => {
                if self.attempt.first.get().is_none() {
                    let canonical = self.attempt.canonical.get().ok_or_else(invalid)?;
                    let source = canonical
                        .plan()
                        .source()
                        .native_input()
                        .ok_or_else(invalid)?
                        .clone();
                    // Existing canonical ownership recovers its original clock;
                    // fresh selection alone must still prove its genuine offer.
                    if !source.matches_anchor(anchor, &store_id) {
                        return Err(invalid());
                    }
                    let transcript = canonical.plan().input().clone();
                    lease.check_preparation(&transcript)?;
                    let transaction = self
                        .store
                        .connection
                        .unchecked_transaction()
                        .map_err(database_error)?;
                    let state = self.store.checked_state(&transaction)?;
                    let unaffected =
                        unaffected_rows_digest(&transaction, &[&pending_key, &committed_key])?;
                    self.store.finish_transaction(transaction)?;
                    let generation = state.generation as u64;
                    let journal = NativeInputJournal {
                        version: 1,
                        generation: generation.checked_add(1).ok_or_else(invalid)?,
                        source,
                        transcript,
                    };
                    self.attempt
                        .first
                        .set(Preparation {
                            journal,
                            generation,
                            watermark: state.watermark,
                            unaffected,
                        })
                        .map_err(|_| invalid())?;
                }
                let first = self.attempt.first.get().ok_or_else(invalid)?;
                // Recheck the original destination before publishing a plan
                // retained after an earlier worker failed before the SQL commit.
                validate_anchor(&first.journal, anchor, store_id)?;
                lease.check_preparation(&first.journal.transcript)?;
                let bytes = encode(&first.journal)?;
                let transaction = self
                    .store
                    .connection
                    .unchecked_transaction()
                    .map_err(database_error)?;
                let state = self.store.checked_state(&transaction)?;
                if state.watermark != first.watermark
                    || unaffected_rows_digest(&transaction, &[&pending_key, &committed_key])?
                        != first.unaffected
                {
                    return Err(invalid());
                }
                self.store.commit_in_transaction(
                    &transaction,
                    first.generation,
                    &[CustodyWrite {
                        key: &pending_key,
                        previous: None,
                        value: Some(&bytes),
                    }],
                )?;
                if unaffected_rows_digest(&transaction, &[&pending_key, &committed_key])?
                    != first.unaffected
                    || read_record(&transaction, &committed_key)?.is_some()
                {
                    return Err(invalid());
                }
                self.store.finish_transaction(transaction)?;
                first.journal.clone()
            }
        };
        // The actual canonical receipt is application ownership, not a saved
        // display frame. It can finish missing/partial original input after
        // process loss without decoding an envelope or recreating a turn.
        if let Some(canonical) = self.attempt.canonical.get() {
            if canonical.plan().source().native_input() != Some(&pending.source)
                || canonical.plan().input() != &pending.transcript
            {
                return Err(invalid());
            }
            lease.append_custody_receipt(canonical.plan(), &canonical.rows()?)?;
        }
        let row = lease.seal(&pending.transcript)?;
        if checked_event(&row, anchor, store_id)? != pending.source {
            return Err(invalid());
        }
        if self.attempt.completion.get().is_none() {
            let old = encode(&pending)?;
            let pending_digest = Sha256::digest(&old).into();
            let transaction = self
                .store
                .connection
                .unchecked_transaction()
                .map_err(database_error)?;
            let state = self.store.checked_state(&transaction)?;
            let unaffected = unaffected_rows_digest(&transaction, &[&pending_key, &committed_key])?;
            if self
                .attempt
                .first
                .get()
                .is_some_and(|first| first.unaffected != unaffected)
            {
                return Err(invalid());
            }
            self.store.finish_transaction(transaction)?;
            let generation = state.generation as u64;
            let mut journal = pending;
            journal.generation = generation.checked_add(1).ok_or_else(invalid)?;
            self.attempt
                .completion
                .set(Completion {
                    journal,
                    generation,
                    watermark: state.watermark,
                    unaffected,
                    pending_digest,
                })
                .map_err(|_| invalid())?;
        }
        let completion = self.attempt.completion.get().ok_or_else(invalid)?;
        self.publish_completion(completion)?;
        self.retire_canonical_receipt(&completion.journal)?;
        Ok(Some(receipt(&completion.journal, true)))
    }
    /// Canonical row images retire only after c1. Historical proof does not
    /// grant rights over a replacement canonical root or conversation backing.
    fn retire_canonical_receipt(&self, journal: &NativeInputJournal) -> Result<()> {
        if !journal
            .transcript
            .root_matches_store(&self.attempt.transcript)?
        {
            return Ok(());
        }
        let retained = self.attempt.canonical.get().cloned();
        let canonical = match retained {
            Some(canonical) => Some(canonical),
            None => match self.attempt.lease.get() {
                Some(lease) => lease.lookup_append_receipt(
                    CustodyReceiptKind::NativeInput,
                    &journal.source.occurrence(),
                )?,
                None => self.attempt.transcript.custody_append_receipt(
                    journal.source.native_owner.conversation_id(),
                    CustodyReceiptKind::NativeInput,
                    &journal.source.occurrence(),
                )?,
            },
        };
        if let Some(canonical) = canonical {
            if canonical.plan().source().native_input() != Some(&journal.source)
                || canonical.plan().input() != &journal.transcript
            {
                return Err(invalid());
            }
            canonical.retire()?;
        }
        Ok(())
    }

    /// Publishes or reconciles only the exact retained final batch. Later row
    /// changes or positions cannot become a fresh retry baseline after loss.
    fn publish_completion(&self, completion: &Completion) -> Result<()> {
        let occurrence = completion.journal.source.occurrence();
        let pending_key = record_key("p1", &occurrence);
        let committed_key = record_key("c1", &occurrence);
        let bytes = encode(&completion.journal)?;
        let transaction = self
            .store
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let state = self.store.checked_state(&transaction)?;
        if state.watermark != completion.watermark
            || unaffected_rows_digest(&transaction, &[&pending_key, &committed_key])?
                != completion.unaffected
        {
            return Err(invalid());
        }
        self.store.commit_in_transaction(
            &transaction,
            completion.generation,
            &[
                CustodyWrite {
                    key: &pending_key,
                    previous: Some(&completion.pending_digest),
                    value: None,
                },
                CustodyWrite {
                    key: &committed_key,
                    previous: None,
                    value: Some(&bytes),
                },
            ],
        )?;
        if unaffected_rows_digest(&transaction, &[&pending_key, &committed_key])?
            != completion.unaffected
        {
            return Err(invalid());
        }
        self.store.finish_transaction(transaction)
    }
}

/// The actual typed canonical input must retain exact source and recipient
/// attribution; ordinary system messages and peer presentation labels refuse.
fn checked_event(
    row: &TranscriptEntry,
    anchor: &MessageNativeInputAnchor,
    store_id: [u8; 16],
) -> Result<TranscriptNativeInputSource> {
    row.validate()?;
    let Some(TranscriptContextEvent::NativePeerMessage { event }) =
        TranscriptContextEvent::from_transcript_content(&row.content)
    else {
        return Err(invalid());
    };
    if row.role != TranscriptRole::System
        || row.conversation_id != anchor.native_owner().conversation_id()
        || row.agent_id != anchor.recipient().as_str()
        || !event.source.matches_anchor(anchor, &store_id)
    {
        return Err(invalid());
    }
    Ok(event.source)
}
/// Compares source bytes against the actual opaque original owner.
fn validate_anchor(
    journal: &NativeInputJournal,
    anchor: &MessageNativeInputAnchor,
    store_id: [u8; 16],
) -> Result<()> {
    if !journal.source.matches_anchor(anchor, &store_id) {
        return Err(invalid());
    }
    Ok(())
}
/// Stable key derives solely from original source identity and frozen slot.
fn occurrence(anchor: &MessageNativeInputAnchor, store_id: [u8; 16]) -> [u8; 32] {
    source_occurrence(
        store_id,
        anchor.source_sha256(),
        anchor.binding_index() as u32,
    )
}
/// Derives descriptive journal routing from the actual allocation/source/slot.
/// These deterministic bytes confer no native input or ACK authority.
pub(super) fn source_occurrence(store_id: [u8; 16], source: [u8; 32], index: u32) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"mez-native-canonical-input-v1\0");
    hash.update(store_id);
    hash.update(source);
    hash.update(index.to_le_bytes());
    hash.finalize().into()
}
/// Key text is a fixed lowercase digest, never a path or native capability.
pub(super) fn record_key(prefix: &str, occurrence: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut key = String::with_capacity(67);
    key.push_str(prefix);
    key.push(':');
    for byte in occurrence {
        let _ = write!(key, "{byte:02x}");
    }
    key
}
/// Small deterministic record encoding; no transcript payload is serialized.
fn encode(journal: &NativeInputJournal) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(journal).map_err(|_| invalid())?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(invalid());
    }
    Ok(bytes)
}
/// Converts only an actual qualified journal/row boundary into descriptive
/// facts; the lower trusted callback separately seals its opaque commit.
fn receipt(journal: &NativeInputJournal, archive_observed: bool) -> MessageNativeInputRecord {
    MessageNativeInputRecord {
        store_id: journal.source.store_id,
        occurrence: journal.source.occurrence(),
        archive_sequence: journal.transcript.archive_sequence,
        canonical_sha256: journal.transcript.canonical_sha256,
        offered_at_ms: journal.source.offered_at_ms,
        archive_observed,
    }
}
/// Content-free diagnostic never prints private source data or physical paths.
fn invalid() -> MezError {
    MezError::invalid_state("original MMP canonical native input unavailable")
}
