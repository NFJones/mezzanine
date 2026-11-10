//! Select one optional bridge source into the existing exclusive root lane.
//!
//! No second acceptance queue, worker or retry clock exists here. Native
//! continuity precedes source selection; after issuance, the common original
//! root owns reconciliation, guarded ACK yielding and shutdown settlement.

use super::*;

impl AsyncRuntimeSessionActor {
    pub(super) fn pump_stored_bridge_message(&mut self) {
        if !self.service.stored_bridge_message_has_priority()
            || self
                .service
                .message_service()
                .pending_native_continuity()
                .is_some()
        {
            return;
        }
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        if owner.wake.is_some()
            || owner.pending.is_some()
            || owner.failure.is_some()
            || owner.shutdown.is_some()
        {
            return;
        }
        let ticket = match self.service.prepare_stored_bridge_message(
            super::super::coalesce::async_runtime_current_unix_millis(),
        ) {
            Ok(Some(ticket)) => ticket,
            Ok(None) => return,
            Err(_) => {
                // Selection has issued no effect and returned no acceptance.
                eprintln!("mez: runtime bridge refused before storage acceptance");
                return;
            }
        };
        owner.pending = Some(Pending::Bridge);
        owner.wake = Some(Wake::Observe);
        if let Err(error) = owner
            .root
            .start(self.service.message_service_mut(), &ticket, None)
        {
            match self
                .service
                .message_service_mut()
                .cancel_acceptance(&ticket)
            {
                Ok(()) => {
                    owner.pending.take();
                    owner.wake = None;
                    eprintln!("mez: runtime bridge storage start refused before issuance");
                }
                Err(_) => {
                    // Unknown issuance stays with the actual root/lower owner.
                    owner.failure = Some(error);
                }
            }
        }
    }
}
