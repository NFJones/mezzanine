//! Native retired-slot selection after actual original canonical/end lookup.
//!
//! Existing canonical positives retain their normal ACK owner. An ended-owner
//! witness can select only an otherwise unprotected original slot on the common
//! terminal lane. Pre-issue pressure retains one ordinary lookup retry; a later
//! attempt requalifies storage rather than replaying a mutation or inventing ACK.

use super::*;
use mez_agent::messaging::MessageNativeRetirementWitness;

impl AsyncRuntimeSessionActor {
    /// Selects no work during Stop, and preserves source custody through actual
    /// terminal issuance. Namespace evidence alone never bypasses a live input
    /// owner or turns a capacity refusal into a successful disposition.
    pub(in crate::host::async_runtime::actor::message_custody) fn activate_retired_native_message(
        &mut self,
        witness: MessageNativeRetirementWitness,
    ) {
        let source = witness.anchor().clone();
        let Some(owner) = self.message_custody.as_ref() else {
            return;
        };
        if owner.shutdown.is_some() {
            return;
        }
        if owner.pending.is_some() || owner.wake.is_some() || owner.failure.is_some() {
            self.defer_retirement_lookup(source);
            return;
        }
        let scope = owner.root.storage_scope().clone();
        let prepared = self
            .service
            .message_service_mut()
            .prepare_storage_native_retirement(
                &scope,
                &witness,
                crate::runtime::current_unix_millis(),
                8 * 1024 * 1024,
            );
        let ticket = match prepared {
            Ok(Some(ticket)) => ticket,
            Ok(None) => return,
            Err(_) => {
                self.defer_retirement_lookup(source);
                return;
            }
        };
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        owner.pending = Some(Pending::TerminalWrite);
        owner.wake = Some(Wake::Observe);
        if let Err(error) = owner
            .root
            .start_terminal(self.service.message_service_mut(), &ticket)
        {
            if self
                .service
                .message_service_mut()
                .cancel_storage_terminal(&ticket)
                .is_err()
            {
                owner.failure = Some(error);
                owner.wake = Some(Wake::Retry(
                    Instant::now() + std::time::Duration::from_secs(1),
                ));
            } else {
                owner.pending = None;
                owner.wake = None;
                self.defer_retirement_lookup(source);
            }
        }
    }

    /// Keeps one bounded original descriptor and coalesces other refused slots
    /// into a future pass. The timer cannot spin while another root job owns I/O.
    fn defer_retirement_lookup(&mut self, source: MessageNativeInputAnchor) {
        if let Some(owner) = self.message_custody.as_mut()
            && owner.native_retry.is_none()
        {
            owner.native_retry = Some((Instant::now() + std::time::Duration::from_secs(1), source));
        }
    }
}
