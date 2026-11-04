//! Original native sender canonical handoff, separate from outcome consumption.
//!
//! u1 freezes the first bodyless source/before-image and physical row position;
//! h1 replaces it only after the actual typed neutral row is read and synced.
//! The shared canonical receipt retires AFTER h1 and BEFORE returning proof, so
//! a crash in that window leaves the terminal source pending and discoverable.
//! First lease, row image and SQL plans live outside abortable workers. Unknown
//! writes reconcile exact bytes, original backing and every non-target row.
//! h1 itself never releases an outcome, ACKs mail or recreates a task/provider.

use super::{integrity::unaffected_rows_digest, *};
use crate::storage::transcript::{
    AgentTranscriptStore, CustodyAppendReceipt,
    custody::{
        CustodyAppendPlan, CustodyExpectation, CustodyReceiptKind, CustodyTranscriptLease,
        CustodyTranscriptPlan,
    },
};
use mez_agent::messaging::{
    MessageTerminalHandoffAnchor, MessageTerminalHandoffIo, MessageTerminalHandoffRecord,
    MessageTerminalHandoffStore,
};
use mez_agent::transcript::{TranscriptContextEvent, TranscriptEntry, TranscriptRole};
use std::sync::{Arc, OnceLock};

mod journal;
mod projection;
pub(in crate::storage::mmp_custody) use journal::HandoffConsumption;
use journal::{HandoffJournal, Source};
pub(crate) use projection::TerminalHandoffProjection;

/// Actual post-commit crash boundaries, used only by the isolated test child.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandoffCrashPoint {
    Preparation,
    Canonical,
}

/// Frozen actual transaction baseline, retained before its first physical write.
struct JournalPlan {
    journal: HandoffJournal,
    generation: u64,
    watermark: u64,
    unaffected: [u8; 32],
    previous: Option<[u8; 32]>,
}

/// Exact original canonical work, retained by the root through error and panic.
pub(crate) struct TerminalHandoffAttempt {
    transcript: AgentTranscriptStore,
    projection: Option<Arc<TerminalHandoffProjection>>,
    lease: OnceLock<CustodyTranscriptLease>,
    canonical_first: OnceLock<(CustodyAppendPlan, Vec<TranscriptEntry>)>,
    canonical: OnceLock<Arc<CustodyAppendReceipt>>,
    preparation: OnceLock<JournalPlan>,
    completion: OnceLock<JournalPlan>,
    #[cfg(test)]
    crash_after: Option<HandoffCrashPoint>,
}
impl TerminalHandoffAttempt {
    /// Captures only actual application work or an explicit historical probe.
    pub(crate) fn new(
        transcript: AgentTranscriptStore,
        projection: Option<Arc<TerminalHandoffProjection>>,
        #[cfg(test)] crash_after: Option<HandoffCrashPoint>,
    ) -> Self {
        Self {
            transcript,
            projection,
            lease: OnceLock::new(),
            canonical_first: OnceLock::new(),
            canonical: OnceLock::new(),
            preparation: OnceLock::new(),
            completion: OnceLock::new(),
            #[cfg(test)]
            crash_after,
        }
    }
}

/// Trusted original-store callback under the actual root's retained kernel fence.
pub(crate) struct TerminalHandoffPort<'a> {
    store: &'a mut MmpCustodyStore,
    attempt: &'a TerminalHandoffAttempt,
    io: &'a MessageTerminalHandoffIo,
}
impl MmpCustodyStore {
    /// Borrows the first immutable work and its pre-issued decoder reservation.
    pub(crate) fn terminal_handoff_port<'a>(
        &'a mut self,
        attempt: &'a TerminalHandoffAttempt,
        io: &'a MessageTerminalHandoffIo,
    ) -> TerminalHandoffPort<'a> {
        TerminalHandoffPort {
            store: self,
            attempt,
            io,
        }
    }
}
impl MessageTerminalHandoffStore for TerminalHandoffPort<'_> {
    /// Seals proof only after actual source/journal/row/receipt qualification.
    fn verify(
        &mut self,
        source: &MessageTerminalHandoffAnchor,
    ) -> mez_agent::messaging::Result<Option<MessageTerminalHandoffRecord>> {
        self.execute(source).map_err(|error| {
            mez_agent::messaging::MessageError::invalid_state(format!(
                "MMP canonical terminal handoff: {error}"
            ))
        })
    }
}
impl TerminalHandoffPort<'_> {
    /// None means actual journal/image absence. An original h1 is historical
    /// proof even after legitimate archive cleanup; it cannot materialize input.
    fn execute(
        &mut self,
        source: &MessageTerminalHandoffAnchor,
    ) -> Result<Option<MessageTerminalHandoffRecord>> {
        let store_id: [u8; 16] = self
            .store
            .store_id
            .as_slice()
            .try_into()
            .map_err(|_| invalid())?;
        let source_id = Source::capture(source, store_id)?;
        if source_id != Source::capture(self.io.source(), store_id)? {
            return Err(invalid());
        }
        let occurrence = source.occurrence(&store_id);
        let pending_key = key_for("u1", &occurrence);
        let committed_key = key_for("h1", &occurrence);
        let transaction = self
            .store
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        self.store
            .qualify_handoff_source(&transaction, &source_id, self.io.decode_limits())?;
        let committed = self
            .store
            .read_handoff_metadata(&transaction, &committed_key)?;
        let pending = self
            .store
            .read_handoff_metadata(&transaction, &pending_key)?;
        if (committed.is_some() && pending.is_some())
            || committed
                .as_ref()
                .is_some_and(|p| !p.matches(source, store_id))
            || pending
                .as_ref()
                .is_some_and(|p| !p.matches(source, store_id))
        {
            return Err(invalid());
        }
        if committed.is_none() {
            self.store
                .ensure_handoff_unreleased(&transaction, &source_id)?;
        }
        self.store.finish_transaction(transaction)?;
        if let Some(committed) = committed {
            if !committed.matches(source, store_id)
                || self.attempt.preparation.get().is_some_and(|first| {
                    first.journal.source != committed.source
                        || first.journal.transcript != committed.transcript
                })
            {
                return Err(invalid());
            }
            if let Some(completion) = self.attempt.completion.get() {
                if completion.journal != committed {
                    return Err(invalid());
                }
                self.persist(completion)?;
            }
            self.retire(&committed, source, store_id)?;
            return Ok(Some(committed.record(false)));
        }
        if self.attempt.lease.get().is_none() {
            let (canonical, lease) = self.attempt.transcript.recover_custody_lease(
                source,
                &store_id,
                &occurrence,
                pending
                    .as_ref()
                    .map(|p| CustodyExpectation::TerminalHandoff(&p.transcript)),
                self.attempt.projection.is_some(),
            )?;
            if let Some(canonical) = canonical {
                if self
                    .attempt
                    .canonical_first
                    .get()
                    .is_some_and(|(first, _)| first != canonical.plan())
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
                let projection = self.attempt.projection.as_ref().ok_or_else(invalid)?;
                let (mut prefix, mut row) = projection.materialize(source, store_id)?;
                let plan = lease.prepare_custody_append(&mut prefix, &mut row)?;
                prefix.push(row);
                self.attempt
                    .canonical_first
                    .set((plan, prefix))
                    .map_err(|_| invalid())?;
            }
            let (plan, rows) = self.attempt.canonical_first.get().ok_or_else(invalid)?;
            self.attempt
                .canonical
                .set(lease.publish_append_receipt(plan.clone(), rows)?)
                .map_err(|_| invalid())?;
        }
        if let Some(canonical) = self.attempt.canonical.get() {
            canonical.revalidate()?;
            // A retained image is checked against the real source, including
            // the original sender role, before any preceding archive bytes.
            let rows = canonical.rows()?;
            checked_row(rows.last().ok_or_else(invalid)?, source, store_id)?;
            lease.append_custody_prefix(canonical.plan(), &rows)?;
        }
        let pending = match pending {
            Some(pending) => {
                if !pending.matches(source, store_id)
                    || self
                        .attempt
                        .preparation
                        .get()
                        .is_some_and(|first| first.journal != pending)
                {
                    return Err(invalid());
                }
                pending
            }
            None => {
                if self.attempt.preparation.get().is_none() {
                    let canonical = self.attempt.canonical.get().ok_or_else(invalid)?;
                    let plan = canonical.plan().input().clone();
                    lease.check_preparation(&plan)?;
                    let first =
                        self.freeze(HandoffJournal::capture(source, store_id, 0, plan)?, None)?;
                    self.attempt.preparation.set(first).map_err(|_| invalid())?;
                }
                let first = self.attempt.preparation.get().ok_or_else(invalid)?;
                if !first.journal.matches(source, store_id) {
                    return Err(invalid());
                }
                lease.check_preparation(&first.journal.transcript)?;
                self.persist(first)?;
                #[cfg(test)]
                if self.attempt.crash_after == Some(HandoffCrashPoint::Preparation) {
                    std::process::exit(0);
                }
                first.journal.clone()
            }
        };
        if let Some(canonical) = self.attempt.canonical.get() {
            if canonical.plan().input() != &pending.transcript {
                return Err(invalid());
            }
            lease.append_custody_receipt(canonical.plan(), &canonical.rows()?)?;
        }
        let row = lease.seal(&pending.transcript)?;
        checked_row(&row, source, store_id)?;
        if self.attempt.completion.get().is_none() {
            let digest = Sha256::digest(pending.encode()?).into();
            let completion = self.freeze(pending, Some(digest))?;
            if self
                .attempt
                .preparation
                .get()
                .is_some_and(|first| first.unaffected != completion.unaffected)
            {
                return Err(invalid());
            }
            self.attempt
                .completion
                .set(completion)
                .map_err(|_| invalid())?;
        }
        let completion = self.attempt.completion.get().ok_or_else(invalid)?;
        self.persist(completion)?;
        #[cfg(test)]
        if self.attempt.crash_after == Some(HandoffCrashPoint::Canonical) {
            std::process::exit(0);
        }
        self.retire(&completion.journal, source, store_id)?;
        Ok(Some(completion.journal.record(true)))
    }

    /// Freezes one original SQL baseline before issuing the corresponding write.
    fn freeze(
        &self,
        mut journal: HandoffJournal,
        previous: Option<[u8; 32]>,
    ) -> Result<JournalPlan> {
        let occurrence = journal.source.occurrence();
        let transaction = self
            .store
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let state = self.store.checked_state(&transaction)?;
        let unaffected = unaffected_rows_digest(
            &transaction,
            &[&key_for("u1", &occurrence), &key_for("h1", &occurrence)],
        )?;
        self.store.finish_transaction(transaction)?;
        let generation = state.generation as u64;
        journal.generation = generation.checked_add(1).ok_or_else(invalid)?;
        Ok(JournalPlan {
            journal,
            generation,
            watermark: state.watermark,
            unaffected,
            previous,
        })
    }

    /// Original same-generation batch reconciliation never reseats its baseline.
    /// Every SQL effect must preserve all non-target rows before commit.
    fn persist(&self, plan: &JournalPlan) -> Result<()> {
        let occurrence = plan.journal.source.occurrence();
        let pending = key_for("u1", &occurrence);
        let committed = key_for("h1", &occurrence);
        let bytes = plan.journal.encode()?;
        let transaction = self
            .store
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let state = self.store.checked_state(&transaction)?;
        if state.watermark != plan.watermark
            || unaffected_rows_digest(&transaction, &[&pending, &committed])? != plan.unaffected
        {
            return Err(invalid());
        }
        if let Some(previous) = &plan.previous {
            self.store.commit_in_transaction(
                &transaction,
                plan.generation,
                &[
                    CustodyWrite {
                        key: &pending,
                        previous: Some(previous),
                        value: None,
                    },
                    CustodyWrite {
                        key: &committed,
                        previous: None,
                        value: Some(&bytes),
                    },
                ],
            )?;
        } else {
            self.store.commit_in_transaction(
                &transaction,
                plan.generation,
                &[CustodyWrite {
                    key: &pending,
                    previous: None,
                    value: Some(&bytes),
                }],
            )?;
        }
        if unaffected_rows_digest(&transaction, &[&pending, &committed])? != plan.unaffected
            || (plan.previous.is_none() && read_record(&transaction, &committed)?.is_some())
        {
            return Err(invalid());
        }
        self.store.finish_transaction(transaction)
    }

    /// Only h1 permits image retirement. A legitimate missing/replaced archive
    /// cannot retarget historical proof to a new conversation or physical root.
    fn retire(
        &self,
        journal: &HandoffJournal,
        source: &MessageTerminalHandoffAnchor,
        store_id: [u8; 16],
    ) -> Result<()> {
        if !journal
            .transcript
            .root_matches_store(&self.attempt.transcript)?
        {
            return Ok(());
        }
        let occurrence = journal.source.occurrence();
        let owner = match self.attempt.canonical.get() {
            Some(owner) => Some(owner.clone()),
            None => match self.attempt.lease.get() {
                Some(lease) => {
                    lease.lookup_append_receipt(CustodyReceiptKind::TerminalHandoff, &occurrence)?
                }
                None => self.attempt.transcript.custody_append_receipt(
                    journal.transcript.conversation_id(),
                    CustodyReceiptKind::TerminalHandoff,
                    &occurrence,
                )?,
            },
        };
        if let Some(owner) = owner {
            if owner.plan().input() != &journal.transcript {
                return Err(invalid());
            }
            let rows = owner.rows()?;
            checked_row(rows.last().ok_or_else(invalid)?, source, store_id)?;
            owner.retire()?;
        }
        Ok(())
    }
}

/// A fixed neutral terminal event must name its original sender, never recipient
/// input. Equal presentation text or generic imported DTOs cannot qualify it.
fn checked_row(
    row: &TranscriptEntry,
    source: &MessageTerminalHandoffAnchor,
    store_id: [u8; 16],
) -> Result<()> {
    row.validate()?;
    let Some(TranscriptContextEvent::TerminalHandoff { event }) =
        TranscriptContextEvent::from_transcript_content(&row.content)
    else {
        return Err(invalid());
    };
    if row.role != TranscriptRole::System
        || row.agent_id != source.sender().as_str()
        || row.conversation_id != source.native_owner().conversation_id()
        || !event.source.matches_anchor(source, &store_id)
    {
        return Err(invalid());
    }
    Ok(())
}
/// Stable descriptive routing, never a caller-selected path or capability.
fn key_for(prefix: &str, occurrence: &[u8; 32]) -> String {
    super::native_input::record_key(prefix, occurrence)
}
/// Errors never expose private sender data, original message IDs or paths.
fn invalid() -> MezError {
    MezError::invalid_state("original MMP canonical terminal handoff unavailable")
}
