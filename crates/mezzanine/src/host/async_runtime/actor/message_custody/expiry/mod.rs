//! Bounded expiry selection after original native recovery probes have drained.
//!
//! One actor continuation pairs with the existing exclusive root job; no second
//! mailbox, source body or positive fact lives here. Idle deadlines wake a fresh
//! canonical scan before selecting expiry. Pre-issuance capacity failure retains
//! no physical attempt and backs off without excluding unrelated admission.
//! Issued work instead drains through actual join/retry/adoption during shutdown.
//! Sender release, retirement, elapsed-clock policy and reclaim are separate gates.

use super::*;

impl AsyncRuntimeSessionActor {
    /// Selects at most one original expiry after higher-priority native input
    /// work and persistent preparation probes. A timer never bypasses that scan.
    pub(super) fn pump_message_expiry(&mut self) {
        let Some(owner) = self.message_custody.as_ref() else {
            return;
        };
        if owner.pending.is_some()
            || owner.wake.is_some()
            || owner.failure.is_some()
            || owner.shutdown.is_some()
            || owner.native_result.is_some()
            || owner.native_retry.is_some()
            || self.service.has_pending_native_input()
            || (self.service.native_input_storage_required() && owner.native_scan_pending)
            || owner.expiry_retry_at.is_some_and(|at| at > Instant::now())
            || self.service.message_service().payload_observer().is_none()
        {
            return;
        }
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        let scope = owner.root.storage_scope();
        let now_ms = crate::runtime::current_unix_millis();
        let selected = self.service.message_service_mut().prepare_storage_expiry(
            scope,
            now_ms,
            8 * 1024 * 1024,
        );
        match selected {
            Ok(Some(ticket)) => {
                // Install this continuation before the actual worker can run.
                owner.pending = Some(Pending::Expiry);
                owner.wake = Some(Wake::Observe);
                match owner
                    .root
                    .start_expiry(self.service.message_service_mut(), &ticket)
                {
                    Ok(()) => owner.expiry_retry_at = None,
                    Err(error) => {
                        if self
                            .service
                            .message_service_mut()
                            .cancel_storage_expiry(&ticket)
                            .is_err()
                        {
                            owner.failure = Some(error);
                            owner.wake = Some(Wake::Retry(
                                Instant::now() + std::time::Duration::from_secs(1),
                            ));
                        } else {
                            owner.pending = None;
                            owner.wake = None;
                            owner.expiry_retry_at =
                                Some(Instant::now() + std::time::Duration::from_secs(1));
                        }
                    }
                }
            }
            Ok(None) => {
                match self
                    .service
                    .message_service()
                    .next_storage_expiry_at_ms(scope)
                {
                    Ok(at) => {
                        owner.expiry_retry_at = at.and_then(|at| {
                            Instant::now().checked_add(std::time::Duration::from_millis(
                                at.saturating_sub(now_ms),
                            ))
                        })
                    }
                    Err(_) => {
                        owner.expiry_retry_at =
                            Some(Instant::now() + std::time::Duration::from_secs(1))
                    }
                }
            }
            Err(_) => {
                // No original was selected or issued. Real ACK/cache progress
                // may use the idle lane before the next bounded capture try.
                owner.expiry_retry_at = Some(Instant::now() + std::time::Duration::from_secs(1));
            }
        }
    }
}
