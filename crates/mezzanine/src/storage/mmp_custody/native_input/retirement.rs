//! Original lifetime/end observations following actual canonical input absence.
//!
//! The port's retained source proves that this invocation already inspected the
//! original canonical store. One same-connection snapshot then links the frozen
//! native owner to immutable b1/o1 rows. Missing legacy provenance stays unknown;
//! the returned DTO cannot itself create a source, terminal effect or live owner.

use super::*;
use mez_agent::messaging::MessageNativeRetirementRecord;

impl NativeInputPort<'_> {
    /// Reads fixed bounded provenance only for this port's actual absent source.
    pub(super) fn observe_retired_owner(
        &self,
        anchor: &MessageNativeInputAnchor,
    ) -> Result<Option<MessageNativeRetirementRecord>> {
        if self
            .absent_for
            .as_ref()
            .is_none_or(|actual| !actual.same_source(anchor))
        {
            return Err(invalid());
        }
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
        let lifetime_key = format!("b1:{}", anchor.native_owner().incarnation());
        let Some(lifetime) = self
            .store
            .read_native_lifetime_in_snapshot(&transaction, &lifetime_key)?
        else {
            self.store.finish_transaction(transaction)?;
            return Ok(None);
        };
        let end_key =
            super::super::registration_end::record_key(&lifetime.native.registration_namespace);
        let Some(end) = self
            .store
            .read_registration_end_in_snapshot(&transaction, &end_key)?
        else {
            self.store.finish_transaction(transaction)?;
            return Ok(None);
        };
        if lifetime.native.owner != *anchor.native_owner()
            || lifetime.native.agent_id != anchor.recipient().as_str()
            || lifetime.registered_after_sequence != anchor.registered_after_sequence()
            || end.snapshot.agent_id != lifetime.native.agent_id
            || end.snapshot.registered_after_sequence != lifetime.registered_after_sequence
            || anchor.sequence() <= lifetime.registered_after_sequence
            || anchor.sequence() > end.snapshot.ended_after_sequence
            || lifetime.publication_generation >= end.generation
        {
            return Err(invalid());
        }
        let lifetime_bytes = self.store.read(&lifetime_key)?.ok_or_else(invalid)?;
        let end_bytes = self.store.read(&end_key)?.ok_or_else(invalid)?;
        let record = MessageNativeRetirementRecord {
            store_id: self
                .store
                .store_id
                .as_slice()
                .try_into()
                .map_err(|_| invalid())?,
            native: lifetime.native,
            native_publication: lifetime.publication_generation,
            end: end.snapshot,
            end_publication: end.generation,
            lifetime_sha256: Sha256::digest(&lifetime_bytes).into(),
            end_sha256: Sha256::digest(&end_bytes).into(),
        };
        self.store.finish_transaction(transaction)?;
        Ok(Some(record))
    }
}
