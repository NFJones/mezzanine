//! Shared synchronous driver for the real canonical persistence worker.
//!
//! Model completion only proposes a rewrite. Owning runtime tests execute its
//! exact queued attempt and separately apply the returned receipt, preserving
//! the observable boundary between inference, storage and live adoption.

use super::*;

pub(super) fn execute_pending_publication(
    service: &mut RuntimeSessionService,
) -> crate::runtime::RuntimeReplayEpochCompletion {
    crate::test_support::compaction_publication::execute_pending_publication(service)
}
