//! Original unissued canonical settlement and distinct qualified refusal handoff.
//!
//! No native dispatch or worker is manufactured. Positive producer custody and
//! exact actor Queued/Rejected ownership mint immutable refusal provenance.
//! Canonical/context/hooks/audit/publication and original checked storage still
//! run through existing retained owners. An opaque acknowledgement is cached
//! before either actor obligation is removed; its source tokens are non-owning.

use super::*;
use crate::runtime::processes::native_filesystem_producer::{
    NativeFilesystemProducer, NativeFilesystemRefusal,
};

/// Historical canonical/storage handoff of an actually unissued source.
#[derive(Clone)]
pub(crate) struct NativeFilesystemRefusalTransfer {
    /// Positive original source/producer provenance, not a worker join.
    refusal: NativeFilesystemRefusal,
    /// Exact qualified original checked storage/no-save policy.
    storage: NativeFilesystemArchiveSettlement,
}
impl fmt::Debug for NativeFilesystemRefusalTransfer {
    /// Source data and private allocation identities remain opaque.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeFilesystemRefusalTransfer")
    }
}
impl NativeFilesystemRefusalTransfer {
    /// Matches exact original producer allocation without a current actor lookup.
    pub(crate) fn belongs_to(&self, producer: &NativeFilesystemProducer) -> bool {
        self.refusal.belongs_to(producer)
    }
    /// Requires the same positive proof retained in that producer, never closed
    /// flags or absence of a join as a substitute for accepted storage.
    pub(crate) fn acknowledges(&self, producer: &NativeFilesystemProducer) -> bool {
        self.belongs_to(producer)
            && producer
                .refusal()
                .ok()
                .flatten()
                .is_some_and(|actual| actual.same(&self.refusal))
    }
}
impl RuntimeSessionService {
    /// Retains only actual positive unissued provenance; busy producer state
    /// stays pending. Actor identity and enrolled original allocation are checked
    /// before canonical data changes, without filesystem or authority probes.
    pub(crate) fn refuse_native_filesystem_producer(
        &mut self,
        producer: &NativeFilesystemProducer,
    ) -> Result<Option<NativeFilesystemRefusal>> {
        if !self
            .agent
            .native_filesystem_actions
            .owns_realm(producer.attempt())
        {
            return Err(MezError::conflict("filesystem refusal actor realm changed"));
        }
        if let Some(ack) = producer.refusal_acknowledgement()? {
            return if ack.acknowledges(producer) {
                producer.refusal()
            } else {
                Err(MezError::conflict(
                    "filesystem refusal historical producer changed",
                ))
            };
        }
        if let Some(original) = self
            .agent
            .native_filesystem_actions
            .refusal(producer.attempt())
        {
            if !original.belongs_to(producer)
                || !producer
                    .refusal()?
                    .is_some_and(|actual| actual.same(&original))
            {
                return Err(MezError::conflict(
                    "filesystem refusal retained producer changed",
                ));
            }
            self.filesystem_completion_refused(producer.attempt(), &original)?;
            return Ok(Some(original));
        }
        let (work, rejection) = self
            .agent
            .native_filesystem_actions
            .unissued_source(producer.attempt(), producer)?;
        let Some(refusal) = producer.capture_refusal(work, rejection)? else {
            return Ok(None);
        };
        self.agent
            .native_filesystem_actions
            .retain_refusal(producer.attempt(), refusal.clone())?;
        self.filesystem_completion_refused(producer.attempt(), &refusal)?;
        Ok(Some(refusal))
    }

    /// Accepts semantic refusal facts without claiming the action was issued.
    fn filesystem_completion_refused(
        &mut self,
        attempt: &NativeFilesystemAttempt,
        refusal: &NativeFilesystemRefusal,
    ) -> Result<()> {
        let key = self.filesystem_completion_key(attempt)?;
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get_mut(&key)
            .ok_or_else(|| MezError::invalid_state("filesystem refusal receipt unavailable"))?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        if !Arc::ptr_eq(&receipt.work, refusal.work()) {
            return Err(MezError::conflict("filesystem refusal frozen work changed"));
        }
        if let Some(raw) = &receipt.raw {
            return if Arc::ptr_eq(raw, refusal.outcome()) {
                Ok(())
            } else {
                Err(MezError::conflict("filesystem refusal raw source changed"))
            };
        }
        // Canonical retention releases the raw buffer; the registry and producer
        // still retain this exact proof. Polling storage cannot re-enter facts.
        if !matches!(
            receipt.phase,
            FilesystemCompletionPhase::Queued
                | FilesystemCompletionPhase::Issued
                | FilesystemCompletionPhase::OutcomeRetained
                | FilesystemCompletionPhase::Projection
        ) {
            return Ok(());
        }
        if receipt.phase != FilesystemCompletionPhase::Queued {
            return Err(MezError::conflict("filesystem refusal receipt was issued"));
        }
        receipt.raw = Some(refusal.outcome().clone());
        receipt.phase = FilesystemCompletionPhase::OutcomeRetained;
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(&key.0) {
            group.observed.insert((key.1, key.2));
            group.source_changed();
        }
        Ok(())
    }

    /// Transfers refused source only after fresh original storage acceptance.
    /// Joined producer transfer stays separate and cannot consume this proof.
    pub(crate) fn transfer_refused_native_filesystem_producer(
        &mut self,
        producer: &NativeFilesystemProducer,
        storage: &NativeFilesystemArchiveSettlement,
    ) -> Result<Option<NativeFilesystemRefusalTransfer>> {
        if !self
            .agent
            .native_filesystem_actions
            .owns_realm(producer.attempt())
            || storage.source().attempt() != producer.attempt()
            || !storage.source().producer_is(producer)
        {
            return Err(MezError::conflict(
                "filesystem refusal transfer original producer changed",
            ));
        }
        if let Some(ack) = producer.refusal_acknowledgement()? {
            return if ack.acknowledges(producer) && ack.storage.same_storage(storage) {
                Ok(Some(ack))
            } else {
                Err(MezError::conflict(
                    "filesystem refusal transfer source changed",
                ))
            };
        }
        let key = self.filesystem_completion_key(producer.attempt())?;
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get(&key)
            .ok_or_else(|| {
                MezError::invalid_state("filesystem refusal transfer receipt unavailable")
            })?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        let refusal = self
            .agent
            .native_filesystem_actions
            .refusal(producer.attempt())
            .ok_or_else(|| {
                MezError::conflict("filesystem refusal transfer has no positive proof")
            })?;
        if !producer
            .refusal()?
            .is_some_and(|actual| actual.same(&refusal))
            || !refusal.belongs_to(producer)
        {
            return Err(MezError::conflict(
                "filesystem refusal transfer proof changed",
            ));
        }
        producer.validate_confirmed_outcome(refusal.outcome())?;
        if !self.native_filesystem_archive_settlement_is_current(storage) {
            return Ok(None);
        }
        let ack = NativeFilesystemRefusalTransfer {
            refusal: refusal.clone(),
            storage: storage.clone(),
        };
        self.agent.native_filesystem_actions.transfer_refused(
            producer.attempt(),
            producer,
            &refusal,
            || producer.retain_refusal_acknowledgement(ack.clone()),
        )?;
        self.agent.completion_settlements.filesystem.remove(&key);
        self.recover_filesystem_transfer_suffix(&key.0);
        Ok(Some(ack))
    }
}
