//! Qualified joined archive-prefix facts under original filesystem ownership.
//!
//! Source admission is logical; only the actual registered verifier's retained
//! join may add physical source evidence. Saved source remains freshness-fenced;
//! original no-save policy is explicit. Old-source errors remain attributable
//! after source replacement. This gate neither retires a native producer nor
//! grants continuation, epoch publication or permanent storage retention.

use super::*;
use crate::runtime::agent::RuntimeTranscriptPrefixReceipt;

/// Distinct physical fact versus intentional absence of storage obligation.
#[derive(Clone)]
enum ArchiveStorageDestination {
    /// Original exact verifier proved read/sync and its actual join was observed.
    Saved(RuntimeTranscriptPrefixReceipt),
    /// Only original no-save policy is accepted; no sync or join is fabricated.
    NoPersistence,
}

/// Opaque storage gate; raw/source/producer transfer remains separately required.
#[derive(Clone)]
pub(crate) struct NativeFilesystemArchiveSettlement {
    source: NativeFilesystemArchiveAcceptance,
    destination: ArchiveStorageDestination,
}

impl std::fmt::Debug for NativeFilesystemArchiveSettlement {
    /// Does not expose source, paths, bytes, digests, private errors or addresses.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeFilesystemArchiveSettlement")
    }
}

impl RuntimeSessionService {
    /// Observes actual root-held verification, never a caller result bit. Old
    /// verification errors retain original attribution; success additionally
    /// requires the exact current canonical source and original destination.
    pub(crate) fn observe_native_filesystem_archive_storage(
        &mut self,
        source: &NativeFilesystemArchiveAcceptance,
    ) -> Result<Option<NativeFilesystemArchiveSettlement>> {
        let key = self.filesystem_completion_key(source.attempt())?;
        let producer = self.retained_native_filesystem_producer(source.attempt());
        let physical = if let Some(producer) = &producer {
            producer
                .archive_worker()?
                .filter(|(original, _)| original.same_source(source))
                .map(|(_, receipt)| receipt)
        } else {
            None
        };
        if let Some(receipt) = &physical
            && let Some(Err(error)) = receipt.observed()
        {
            self.fail_filesystem_completion(source.attempt(), &error);
            let first = self
                .agent
                .completion_settlements
                .filesystem
                .get(&key)
                .and_then(|receipt| receipt.failure.as_ref())
                .map(|error| (**error).clone())
                .unwrap_or(error);
            return Err(first);
        }
        if !self.native_filesystem_archive_acceptance_is_current(source) {
            return Ok(None);
        }
        let destination = match source.physical_receipt()? {
            None => ArchiveStorageDestination::NoPersistence,
            Some(expected) => {
                let Some(actual) = physical else {
                    return Ok(None);
                };
                if actual != expected || !matches!(actual.observed(), Some(Ok(true))) {
                    return Ok(None);
                }
                ArchiveStorageDestination::Saved(actual)
            }
        };
        let settlement = NativeFilesystemArchiveSettlement {
            source: source.clone(),
            destination,
        };
        self.agent
            .completion_settlements
            .filesystem
            .get_mut(&key)
            .ok_or_else(|| MezError::invalid_state("physical archive receipt disappeared"))?
            .archive_storage = Some(settlement.clone());
        Ok(Some(settlement))
    }

    /// Checks the stored original physical gate with fresh canonical projection.
    /// No cache or transport scalar can substitute for the original joined work.
    pub(crate) fn native_filesystem_archive_settlement_is_current(
        &mut self,
        settlement: &NativeFilesystemArchiveSettlement,
    ) -> bool {
        if !self.native_filesystem_archive_acceptance_is_current(&settlement.source) {
            return false;
        }
        let Ok(key) = self.filesystem_completion_key(settlement.source.attempt()) else {
            return false;
        };
        let Some(stored) = self
            .agent
            .completion_settlements
            .filesystem
            .get(&key)
            .and_then(|receipt| receipt.archive_storage.as_ref())
        else {
            return false;
        };
        if !stored.source.same_source(&settlement.source) {
            return false;
        }
        match (&stored.destination, &settlement.destination) {
            (
                ArchiveStorageDestination::NoPersistence,
                ArchiveStorageDestination::NoPersistence,
            ) => true,
            (ArchiveStorageDestination::Saved(left), ArchiveStorageDestination::Saved(right)) => {
                left == right && matches!(right.observed(), Some(Ok(true)))
            }
            _ => false,
        }
    }
}
