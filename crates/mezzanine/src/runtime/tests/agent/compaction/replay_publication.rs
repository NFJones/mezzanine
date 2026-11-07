//! Shared synchronous driver for the real canonical persistence worker.
//!
//! Model completion only proposes a rewrite. Owning runtime tests execute its
//! exact queued attempt and separately apply the returned receipt, preserving
//! the observable boundary between inference, storage and live adoption.

use super::*;

pub(super) fn execute_pending_publication(
    service: &mut RuntimeSessionService,
) -> crate::runtime::RuntimeReplayEpochCompletion {
    let mut attempt = None;
    for effect in service
        .drain_transcript_persistence_transition()
        .side_effects
    {
        match effect {
            crate::runtime::RuntimeSideEffect::PersistReplayEpoch { attempt: actual } => {
                assert!(attempt.replace(actual).is_none());
            }
            effect => service.queue_transcript_for_tests(effect),
        }
    }
    let completion = attempt.expect("canonical publication attempt").execute();
    assert!(completion.error().is_none(), "{:?}", completion.error());
    completion
}
