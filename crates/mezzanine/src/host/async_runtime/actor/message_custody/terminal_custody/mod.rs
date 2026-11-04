//! Bounded expiry and horizon selection on the original actor/root lane.
//!
//! One continuation pairs with each exact terminal operation; no second mailbox,
//! body or positive lives here. Idle expiry/horizon deadlines reopen canonical
//! scans before selection. Actual horizon release can free logical pending
//! custody before new observation admission. Pre-issuance refusal backs off
//! without excluding unrelated work; issued jobs retain real join/retry/adoption
//! through shutdown. Durable sender consumption, validated retirement, elapsed
//! clocks and physical reclaim remain separate activation gates.

use super::*;

impl AsyncRuntimeSessionActor {
    /// Selects at most one original expiry after higher-priority native input
    /// work and persistent preparation probes. A timer never bypasses that scan.
    pub(super) fn pump_message_terminal(&mut self) {
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
            || owner
                .terminal_retry_at
                .is_some_and(|at| at > Instant::now())
            || self.service.message_service().payload_observer().is_none()
        {
            return;
        }
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        let scope = owner.root.storage_scope();
        let now_ms = crate::runtime::current_unix_millis();
        // Releasing actual aged outcomes can free logical custody before a
        // fresh observation needs its decoder/metadata reservation.
        let selected = match self
            .service
            .message_service_mut()
            .prepare_storage_horizon_release(scope, now_ms, 8 * 1024 * 1024)
        {
            Ok(None) => self.service.message_service_mut().prepare_storage_expiry(
                scope,
                now_ms,
                8 * 1024 * 1024,
            ),
            other => other,
        };
        match selected {
            Ok(Some(ticket)) => {
                // Install this continuation before the actual worker can run.
                owner.pending = Some(Pending::TerminalWrite);
                owner.wake = Some(Wake::Observe);
                match owner
                    .root
                    .start_terminal(self.service.message_service_mut(), &ticket)
                {
                    Ok(()) => owner.terminal_retry_at = None,
                    Err(error) => {
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
                            owner.terminal_retry_at =
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
                    .and_then(|expiry| {
                        self.service
                            .message_service()
                            .next_storage_horizon_at_ms(scope)
                            .map(|horizon| expiry.into_iter().chain(horizon).min())
                    }) {
                    Ok(at) => {
                        owner.terminal_retry_at = at.and_then(|at| {
                            Instant::now().checked_add(std::time::Duration::from_millis(
                                at.saturating_sub(now_ms),
                            ))
                        })
                    }
                    Err(_) => {
                        owner.terminal_retry_at =
                            Some(Instant::now() + std::time::Duration::from_secs(1))
                    }
                }
            }
            Err(_) => {
                // No original was selected or issued. Real ACK/cache progress
                // may use the idle lane before the next bounded capture try.
                owner.terminal_retry_at = Some(Instant::now() + std::time::Duration::from_secs(1));
            }
        }
    }
}
