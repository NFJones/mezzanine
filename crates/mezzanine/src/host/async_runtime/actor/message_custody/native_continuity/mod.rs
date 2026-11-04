//! Native registration journal publication before source admission.
//!
//! The actor serializes one actual interval at a time on its original root.
//! No message body, sequence or request-generation reservation is retained here:
//! authorized actions stay in their existing source owner, while direct ingress
//! gets an explicit never-accepted busy result. A refused capture issued no I/O
//! and uses a bounded fresh retry; once issued, the actual root owns immutable
//! input and only actual join/backoff/adoption may advance it. Replacements are
//! reobserved before any new admission. This does not qualify restart authority.

use super::*;

impl AsyncRuntimeSessionActor {
    /// Starts at most one journal write while the actual root lane is idle.
    /// Metadata capture precedes every lower source reservation; failure before
    /// issuance retains no payload and allows ACKs to release existing capacity.
    pub(super) fn pump_native_message_continuity(&mut self) {
        let Some(owner) = self.message_custody.as_ref() else {
            return;
        };
        if owner.wake.is_some()
            || owner.pending.is_some()
            || owner.failure.is_some()
            || owner.shutdown.is_some()
        {
            return;
        }
        let refreshed = self.service.refresh_native_message_bindings();
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        if refreshed.is_err() {
            owner.wake = Some(Wake::NativeCaptureRetry(
                Instant::now() + std::time::Duration::from_secs(1),
            ));
            return;
        }
        let Some(id) = self
            .service
            .message_service()
            .pending_native_continuity()
            .cloned()
        else {
            return;
        };
        // Install the actor continuation before the worker can run. A failed
        // synchronous start is a qualified pre-issuance refusal, not lost I/O.
        owner.pending = Some(Pending::NativeContinuity);
        owner.wake = Some(Wake::Observe);
        if owner
            .root
            .start_native_continuity(self.service.message_service(), &id)
            .is_err()
        {
            owner.pending.take();
            owner.wake = Some(Wake::NativeCaptureRetry(
                Instant::now() + std::time::Duration::from_secs(1),
            ));
        }
    }
}
