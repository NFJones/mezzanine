//! Actor admission of original authorized native message-action sources.
//!
//! Semantic/permission custody precedes the root's capacity selection. One FIFO
//! source is prepared per actor turn; actual issued work joins the same private
//! acceptance lane as raw sends. Provider permits and observer receivers do not
//! own storage progress or final original result attribution.

use super::*;

impl AsyncRuntimeSessionActor {
    /// Selects at most one queued original when the actual root lane is idle.
    /// Errors before issuance settle only an unaccepted original; issued errors
    /// remain under Pending::Action until actual positive/refusal reconciliation.
    pub(super) fn pump_stored_message_actions(&mut self) {
        if let Err(error) = self.service.recover_stored_message_continuations()
            && let Some(owner) = self.message_custody.as_mut()
        {
            owner.failure.get_or_insert(error);
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
        let Some(source) = self
            .service
            .pending_stored_message_actions()
            .into_iter()
            .next()
        else {
            return;
        };
        let now_ms = super::super::coalesce::async_runtime_current_unix_millis();
        let admission = match self.service.prepare_stored_message_action(&source, now_ms) {
            Ok(admission) => admission,
            Err(error) => {
                let result = self
                    .service
                    .reject_queued_stored_message_action(
                        &source,
                        MessageError::invalid_state(error.message()),
                    )
                    .and_then(|()| {
                        self.service
                            .project_stored_message_action(&source)
                            .map(|_| ())
                    });
                if let Err(error) = result {
                    owner.failure = Some(error);
                }
                self.notify_event_delivery();
                return;
            }
        };
        // Install continuation ownership before a physical worker can start.
        owner.pending = Some(Pending::Action(admission));
        owner.wake = Some(Wake::Observe);
        let result = match &owner.pending {
            Some(Pending::Action(admission)) => self
                .service
                .start_stored_message_action(admission, |messages, ticket, requests| {
                    owner.root.start(messages, ticket, requests)
                }),
            _ => Err(MezError::invalid_state(
                "message actor preparation owner unavailable",
            )),
        };
        if let Err(error) = result {
            let cancelled = match &owner.pending {
                Some(Pending::Action(admission)) => {
                    self.service.cancel_stored_message_preparation(admission)
                }
                _ => Err(MezError::invalid_state(
                    "message actor cancellation owner unavailable",
                )),
            };
            match cancelled {
                Ok(()) => {
                    owner.pending.take();
                    owner.wake = None;
                    let result = self
                        .service
                        .reject_queued_stored_message_action(
                            &source,
                            MessageError::invalid_state(error.message()),
                        )
                        .and_then(|()| {
                            self.service
                                .project_stored_message_action(&source)
                                .map(|_| ())
                        });
                    if let Err(error) = result {
                        owner.failure = Some(error);
                    }
                }
                Err(cancel_error) => {
                    // Uncertain/issued work keeps its exact actor/root owner;
                    // failure to cancel is never evidence of noncommit.
                    owner.failure = Some(cancel_error);
                    owner.wake = Some(Wake::Observe);
                }
            }
            self.notify_event_delivery();
        }
    }

    /// Completes only an actually adopted original source's canonical result.
    /// Receiver recovery errors cannot undo that irreversible acceptance.
    pub(super) fn deliver_stored_message_action_input(&mut self) {
        if self
            .service
            .deliver_pending_runtime_agent_messages(
                super::super::coalesce::async_runtime_current_unix_millis(),
            )
            .is_err()
        {
            eprintln!("mez: native MMP delivery deferred after action storage acceptance");
        }
        self.notify_event_delivery();
    }
}
