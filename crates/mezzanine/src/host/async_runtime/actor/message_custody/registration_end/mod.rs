//! Actor drain of actual ended registration allocations, including during Stop.
//!
//! Lower intents were reserved before native publication and remain owned after
//! live removal. The actor starts only one end on the original exclusive root,
//! retains its continuation through actual join/backoff and releases it only on
//! exact adoption. No source slots, provider work or live owners are fabricated.

use super::*;

impl AsyncRuntimeSessionActor {
    /// Starts one retained actual end before new native publication can advance
    /// the original store. Capture refusals issue no I/O; genuine ACKs can still
    /// use the idle lane to release capacity before the bounded retry.
    pub(super) fn pump_message_registration_end(&mut self) {
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        if owner.wake.is_some() || owner.pending.is_some() || owner.failure.is_some() {
            return;
        }
        if matches!(
            self.service
                .message_service()
                .has_pending_registration_end(),
            Ok(false)
        ) {
            return;
        }
        owner.pending = Some(Pending::RegistrationEnd);
        owner.wake = Some(Wake::Observe);
        match owner
            .root
            .start_registration_end(self.service.message_service())
        {
            Ok(true) => {}
            Ok(false) => {
                owner.pending = None;
                owner.wake = None;
            }
            Err(_) => {
                owner.pending = None;
                owner.wake = Some(Wake::MetadataCaptureRetry(
                    Instant::now() + std::time::Duration::from_secs(1),
                ));
            }
        }
    }
}
