//! Executes the real queued epoch writer without adopting its returned receipt.
//!
//! Runtime and actor tests share this boundary to distinguish accepted model
//! output, durable publication and actor adoption. Other original transcript
//! work stays with its existing persistence owner; no append ACK is invented.

use crate::runtime::{RuntimeReplayEpochCompletion, RuntimeSessionService};

/// Runs the single queued original attempt and returns its actual receipt.
/// Adoption stays caller-owned; missing, duplicate or failed attempts panic.
pub(crate) fn execute_pending_publication(
    service: &mut RuntimeSessionService,
) -> RuntimeReplayEpochCompletion {
    let attempt = service
        .take_replay_epoch_attempt_for_tests()
        .expect("canonical publication attempt");
    assert!(
        service.take_replay_epoch_attempt_for_tests().is_none(),
        "duplicate publication attempt"
    );
    let completion = attempt.execute();
    assert!(completion.error().is_none(), "{:?}", completion.error());
    completion
}
