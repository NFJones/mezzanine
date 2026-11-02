//! Bounded immutable ACK records in the same private custody projection.
//!
//! One linear lower ACK freezes the recipient interval and exact cumulative
//! decisions after trusted application commit. The session root must bind it to
//! this original store and serialize the plan through settlement. This adapter
//! verifies actual accepted source chunks before committing one bounded version1
//! record with the generic transactional receipt. Lost replies reconcile actual
//! identical bytes, not a saved batch label. No mailbox, application replay or
//! native caller authority is reconstructed here. Restore folding, compaction,
//! control headroom and actor ingress integration remain separate requirements.

use mez_agent::messaging::{
    MessageAcknowledgementStore, MessageAcknowledgementWrite, MessageError,
};

use super::*;

/// Payload-free exact store position retained before physical ACK issuance.
#[derive(Debug, Clone)]
pub(crate) struct AcknowledgementPlan {
    /// Persistent private allocation, not an equal owner label or pane ID.
    store_id: Vec<u8>,
    /// Generation before this original operation, unchanged during retry.
    generation: u64,
}

/// Trusted synchronous port borrowing the serialized original store.
pub(crate) struct PlannedAcknowledgementStore<'a> {
    /// One existing canonical store, never selected from envelope path fields.
    store: &'a mut MmpCustodyStore,
    /// Original source allocation and position retained outside any worker.
    plan: &'a AcknowledgementPlan,
}
impl MmpCustodyStore {
    /// Captures physical position off actor before issuing one original ACK.
    pub fn acknowledgement_plan(&self) -> Result<AcknowledgementPlan> {
        let generation: i64 = self
            .connection
            .query_row(
                "SELECT generation FROM state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        Ok(AcknowledgementPlan {
            store_id: self.store_id.clone(),
            generation: u64::try_from(generation)
                .map_err(|_| MezError::invalid_state("invalid MMP ACK generation"))?,
        })
    }
    /// Borrows exact original ACK storage; all errors remain uncertain.
    pub fn acknowledgement_port<'a>(
        &'a mut self,
        plan: &'a AcknowledgementPlan,
    ) -> PlannedAcknowledgementStore<'a> {
        PlannedAcknowledgementStore { store: self, plan }
    }
}
impl MessageAcknowledgementStore for PlannedAcknowledgementStore<'_> {
    /// Qualifies only actual canonical ACK bytes and complete original sources.
    /// The caller/root still proves actual application commit and live ownership.
    fn commit(&mut self, record: &MessageAcknowledgementWrite) -> mez_agent::messaging::Result<()> {
        self.commit_record(record)
            .map_err(|error| MessageError::invalid_state(format!("MMP ACK storage: {error}")))
    }
}
impl PlannedAcknowledgementStore<'_> {
    /// Commits one bounded immutable record or reconciles its exact postimage.
    fn commit_record(&mut self, record: &MessageAcknowledgementWrite) -> Result<()> {
        if self.store.store_id != self.plan.store_id {
            return Err(MezError::conflict(
                "MMP ACK original store allocation changed",
            ));
        }
        if record.encoded_bytes() > 240 * 1024 {
            return Err(MezError::invalid_args("MMP ACK encoded batch capacity"));
        }
        let mut bytes = Vec::with_capacity(record.encoded_bytes());
        record.write_record(&mut bytes).map_err(MezError::from)?;
        if bytes.len() != record.encoded_bytes() {
            return Err(MezError::invalid_state("MMP ACK original encoding changed"));
        }

        let offset = 48 + record.recipient().as_str().len();
        let (encoded_entries, remainder) = bytes[offset..].as_chunks::<45>();
        if !remainder.is_empty() || encoded_entries.len() != record.entries().count() {
            return Err(MezError::invalid_state("MMP ACK entry framing changed"));
        }
        let mut previous = 0;
        for (entry, encoded) in record.entries().zip(encoded_entries) {
            if entry.sequence <= previous || entry.sequence <= record.registered_after_sequence() {
                return Err(MezError::invalid_state(
                    "MMP ACK original occurrence order changed",
                ));
            }
            let original = self.store.validate_accepted_occurrence(entry.sequence)?;
            if encoded[13..] != original {
                return Err(MezError::conflict(
                    "MMP ACK original source content changed",
                ));
            }
            previous = entry.sequence;
        }
        if previous != record.endpoint_sequence() {
            return Err(MezError::invalid_state("MMP ACK endpoint source missing"));
        }
        let key = ack_key(&bytes);
        if let Some(existing) = self.store.read(&key)? {
            if existing != bytes {
                return Err(MezError::conflict("MMP ACK committed postimage changed"));
            }
            return Ok(());
        }
        self.store.commit(
            self.plan.generation,
            &[CustodyWrite {
                key: &key,
                previous: None,
                value: Some(&bytes),
            }],
        )?;
        Ok(())
    }
}

/// Domain-separated content identity over the full immutable versioned record.
fn ack_key(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"mezzanine-mmp-ack-record-v1\0");
    hash.update(bytes);
    let digest = hash.finalize();
    let mut key = String::from("k1:");
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(key, "{byte:02x}");
    }
    key
}

#[cfg(test)]
mod tests;
