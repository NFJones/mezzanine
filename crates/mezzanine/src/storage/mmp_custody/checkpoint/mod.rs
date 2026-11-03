//! Read-only qualification of payload-free MMP session checkpoints.
//!
//! The root holds its original kernel fence and serialized lane. All source
//! digests are streamed through one SQLite snapshot; no canonical payload is
//! decoded or added to the cache. Returned physical facts are descriptive and
//! must never be used as a generic restore or reconnect credential.

use super::*;
use mez_agent::messaging::{MessageStorageBinding, MessageStorageSnapshotWrite};

impl MmpCustodyStore {
    /// Captures physical identity and verifies each referenced committed original.
    /// Missing/corrupt rows, stale allocators, and changed store state fail closed.
    pub(super) fn storage_snapshot_binding(
        &self,
        request: &MessageStorageSnapshotWrite,
    ) -> Result<MessageStorageBinding> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let state = self.checked_state(&transaction)?;
        if state.watermark >= request.next_sequence() {
            return Err(MezError::invalid_state(
                "MMP checkpoint allocator does not exceed the original watermark",
            ));
        }
        for source in request.sources() {
            if self.validate_accepted_occurrence_in_snapshot(&transaction, source.sequence)?
                != source.source_sha256
            {
                return Err(MezError::invalid_state(
                    "MMP checkpoint original source digest changed",
                ));
            }
        }
        let directory = self.directory.metadata()?;
        let binding = MessageStorageBinding {
            owner: self.owner.clone(),
            store_id: self
                .store_id
                .as_slice()
                .try_into()
                .map_err(|_| MezError::invalid_state("MMP checkpoint store allocation size"))?,
            generation: u64::try_from(state.generation)
                .map_err(|_| MezError::invalid_state("MMP checkpoint generation"))?,
            sequence_watermark: state.watermark,
            directory_device: directory.dev(),
            directory_inode: directory.ino(),
            database_device: self.database_identity.device,
            database_inode: self.database_identity.inode,
        };
        self.finish_transaction(transaction)?;
        Ok(binding)
    }
}
