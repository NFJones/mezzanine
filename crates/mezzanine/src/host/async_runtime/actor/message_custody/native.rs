//! Exact paired native tracked admission on the actor's owned storage root.
//!
//! This entry accepts only trusted harness work after original permission/source
//! capture; it is not a wire endpoint or a permission decision. Capacity selection
//! precedes lower reservation. Runtime metadata and the original reply survive
//! task/observer teardown until actual lower/request/native adoption or qualified
//! refusal. Normal native action/approval continuation activation remains separate;
//! this receipt boundary cannot substitute for canonical action-result publication.

use super::*;
use mez_agent::{
    AgentTurnRecord,
    messaging::{Delivery, Envelope, MessageScope},
};

impl AsyncRuntimeSessionActor {
    /// Starts one already-authorized tracked native send at selected root capacity.
    /// Busy/error leaves no issued replacement; failed start cancels only the exact
    /// never-issued paired reservation. Reply cancellation never revokes custody.
    #[allow(
        dead_code,
        reason = "native action/approval source activation continues under e048f9a8"
    )]
    pub(in crate::host::async_runtime) fn start_tracked_native_message(
        &mut self,
        turn: &AgentTurnRecord,
        envelope: Envelope,
        scope: MessageScope,
        now_ms: u64,
        reply: oneshot::Sender<Result<Delivery>>,
    ) -> Result<()> {
        if self.message_custody_shutdown_pending() {
            return Err(MezError::conflict("MMP storage shutdown pending"));
        }
        let _ = self.poll_message_custody();
        let owner = self
            .message_custody
            .as_mut()
            .ok_or_else(|| MezError::conflict("MMP storage root unavailable"))?;
        if owner.wake.is_some()
            || owner.pending.is_some()
            || owner.failure.is_some()
            || owner.shutdown.is_some()
        {
            return Err(MezError::conflict("MMP storage root busy or unavailable"));
        }
        let ticket = self
            .service
            .prepare_tracked_runtime_message(turn, envelope, scope, now_ms)?;
        if let Err(error) = self
            .service
            .start_prepared_tracked_runtime_message(&ticket, |messages, requests, original| {
                owner.root.start(messages, original, Some(requests))
            })
        {
            self.service
                .cancel_prepared_tracked_runtime_message(&ticket)?;
            return Err(error);
        }
        owner.pending = Some(Pending::Native { ticket, reply });
        owner.wake = Some(Wake::Observe);
        Ok(())
    }
}
