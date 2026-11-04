//! Original-source native dedup, canonical storage and exact ACK continuations.
//!
//! One actor continuation owns each real root job. Pending canonical receipts
//! and sealed ACKs take precedence over new probes; cold payloads use the same
//! owned read lane only after qualified absence. A historical positive settles
//! old transport without reinjection or provider/task creation. Scan progress is
//! payload-free and stops when exhausted until an actual delivery notification.
//! A notification behind an active cursor coalesces into one further full pass.

use super::*;
use mez_agent::messaging::{MessageNativeInputAnchor, MessageNativeInputCommit};

/// One actually joined result retained until its exact application/ACK owner
/// accepts it. Failure before handoff cannot lose proof or retry an idle root.
pub(super) struct NativeInputResult {
    pub(super) receipt: Option<String>,
    pub(super) proof: MessageNativeInputCommit,
    applied: bool,
    retry: Option<(Instant, MezError)>,
}
impl NativeInputResult {
    /// Captures the actual root reply before any fallible application handoff.
    pub(super) fn new(receipt: Option<String>, proof: MessageNativeInputCommit) -> Self {
        Self {
            receipt,
            proof,
            applied: false,
            retry: None,
        }
    }
    /// Deadline belongs only to this original continuation, not physical I/O.
    pub(super) fn wake_at(&self) -> Instant {
        self.retry.as_ref().map_or_else(Instant::now, |(at, _)| *at)
    }
    /// Historical results can ACK only after current-backing observation.
    pub(super) fn ready_for_ack(&self) -> bool {
        self.applied && self.wake_at() <= Instant::now()
    }
    /// Application retries may wake beside a physical worker; ACK admission
    /// retries wait for the actual root to become available instead of spinning.
    pub(super) fn application_at(&self, root_idle: bool) -> Option<Instant> {
        (!self.applied || root_idle).then(|| self.wake_at())
    }
    /// Keeps a bounded diagnostic and monotonic retry with the actual proof.
    fn defer(&mut self, error: MezError) {
        self.retry = Some((Instant::now() + std::time::Duration::from_secs(1), error));
    }
}

impl AsyncRuntimeSessionActor {
    /// Starts at most one real root operation. Source bodies stay with their
    /// existing lower/cache/context owners rather than an actor side mailbox.
    pub(super) fn pump_native_message_input(&mut self) {
        let Some(owner) = self.message_custody.as_ref() else {
            return;
        };
        if owner.pending.is_some() || owner.wake.is_some() || owner.failure.is_some() {
            return;
        }
        if !self.service.native_input_storage_required() {
            return;
        }
        if let Some((id, proof)) = self.service.pending_native_input_ack() {
            if let Err(error) = self.start_native_message_ack(Some(id.clone()), proof) {
                self.service.defer_native_input_completion(&id, error);
            }
            return;
        }
        if let Some(result) = self
            .message_custody
            .as_ref()
            .and_then(|o| o.native_result.as_ref())
        {
            if result.ready_for_ack() {
                let proof = result.proof.clone();
                match self.start_native_message_ack(None, proof) {
                    Ok(()) => {
                        self.message_custody
                            .as_mut()
                            .and_then(|o| o.native_result.take());
                    }
                    Err(error) => {
                        if let Some(result) = self
                            .message_custody
                            .as_mut()
                            .and_then(|o| o.native_result.as_mut())
                        {
                            result.defer(error);
                        }
                    }
                }
            }
            // An unresolved handoff still owns the one root-result slot. Other
            // action, raw and ACK owners may use the physically idle root.
            return;
        }
        if let Some((id, source, transcript, projection)) = self.service.pending_native_input_work()
        {
            let Some(owner) = self.message_custody.as_mut() else {
                return;
            };
            let result = owner.root.start_native_input(
                self.service.message_service(),
                source.clone(),
                transcript,
                Some(projection),
            );
            match result {
                Ok(()) => {
                    self.service.note_native_input_write_issued(&id);
                    owner.pending = Some(Pending::NativeInput {
                        source,
                        receipt: Some(id),
                    });
                    owner.wake = Some(Wake::Observe);
                }
                Err(error) => {
                    owner.failure = Some(error);
                }
            }
            return;
        }
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        if owner.shutdown.is_some() {
            return;
        }
        let watermark = self.service.message_service().next_message_sequence();
        if watermark != owner.native_scan_watermark {
            owner.request_native_scan();
            owner.native_scan_watermark = watermark;
        }
        let retry = if owner
            .native_retry
            .as_ref()
            .is_some_and(|(at, _)| *at <= Instant::now())
        {
            owner.native_retry.take().map(|(_, source)| source)
        } else {
            None
        };
        if !owner.native_scan_pending && retry.is_none() {
            return;
        }
        let retrying = retry.is_some();
        let source = if let Some(source) = retry {
            source
        } else {
            match self
                .service
                .message_service()
                .next_native_input_lookup(owner.native_scan_after)
            {
                Ok(Some(source)) => source,
                Ok(None) => {
                    // Consume only this pass. A real notification may have
                    // changed an earlier cold/ineligible slot while a later
                    // lookup was owned; preserve one fresh pass for that state.
                    owner.native_scan_pending = std::mem::take(&mut owner.native_scan_again);
                    owner.native_scan_after = None;
                    return;
                }
                Err(error) => {
                    owner.failure = Some(error.into());
                    return;
                }
            }
        };
        if !retrying
            && owner.native_retry.as_ref().is_some_and(|(_, parked)| {
                parked.sequence() == source.sequence()
                    && parked.binding_index() == source.binding_index()
            })
        {
            owner.native_scan_after = Some((source.sequence(), source.binding_index()));
            return;
        }
        if self.service.native_input_source_is_owned(&source) {
            // The original live receipt owns pre-ACK application work. A c1
            // lookup must not bypass a failed request-reply decoration.
            if !retrying {
                owner.native_scan_after = Some((source.sequence(), source.binding_index()));
            }
            return;
        }
        let Some(transcript) = self.service.agent_transcript_store().cloned() else {
            owner.failure = Some(MezError::invalid_state(
                "native canonical transcript target missing",
            ));
            return;
        };
        let result = owner.root.start_native_input(
            self.service.message_service(),
            source.clone(),
            transcript,
            None,
        );
        match result {
            Ok(()) => {
                if !retrying {
                    owner.native_scan_after = Some((source.sequence(), source.binding_index()));
                }
                owner.pending = Some(Pending::NativeInput {
                    source,
                    receipt: None,
                });
                owner.wake = Some(Wake::Observe);
            }
            Err(error) => {
                owner.failure = Some(error);
            }
        }
    }

    /// Only a real original-root absence can reach native selection or reload.
    pub(super) fn activate_absent_native_message(&mut self, source: MessageNativeInputAnchor) {
        if self
            .message_custody
            .as_ref()
            .is_none_or(|o| o.shutdown.is_some())
        {
            return;
        }
        let now_ms = crate::runtime::current_unix_millis();
        if !self
            .service
            .message_service()
            .native_input_can_receive(&source, now_ms)
        {
            return;
        }
        if source.resident_envelope().is_err() {
            let Some(owner) = self.message_custody.as_mut() else {
                return;
            };
            match owner
                .root
                .start_payload_load(self.service.message_service_mut(), source.sequence())
            {
                Ok(true) => {
                    owner.pending = Some(Pending::NativePayload(source));
                    owner.wake = Some(Wake::Observe);
                }
                Ok(false) if source.resident_envelope().is_ok() => {
                    let _ = self.service.activate_native_input_absence(&source, now_ms);
                }
                Ok(false) | Err(_) => {
                    // No read was issued. Keep one original descriptor while
                    // unrelated slots/actions continue; re-probe actual custody
                    // at the deadline before retrying cold input.
                    if owner.native_retry.is_none() {
                        owner.native_retry =
                            Some((Instant::now() + std::time::Duration::from_secs(1), source));
                    }
                }
            }
            return;
        }
        // Pre-commit application refusal leaves the original accepted source
        // intact. Other recipients can continue; a later real lifecycle or
        // capacity change reopens this scan instead of failing the whole root.
        let _ = self.service.activate_native_input_absence(&source, now_ms);
    }

    /// Advances bounded local application work even while unrelated physical
    /// storage is owned. Per-receipt backoff prevents one fault starving others.
    pub(super) fn pump_native_message_applications(&mut self) {
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        let continue_turn = owner.shutdown.is_none();
        if let Some(result) = owner.native_result.as_mut()
            && !result.applied
            && result.wake_at() <= Instant::now()
        {
            let outcome = match &result.receipt {
                Some(id) => self
                    .service
                    .complete_native_input_commit(id, result.proof.clone()),
                None => self.service.observe_native_input_archive(&result.proof),
            };
            match outcome {
                Ok(()) if result.receipt.is_some() => {
                    owner.native_result.take();
                }
                Ok(()) => {
                    result.applied = true;
                    result.retry = None;
                }
                Err(error) => result.defer(error),
            }
        }
        for _ in 0..16 {
            let Some(id) = self.service.pending_native_input_application() else {
                break;
            };
            if let Err(error) = self
                .service
                .apply_native_input_completion(&id, continue_turn)
            {
                self.service.defer_native_input_completion(&id, error);
            }
        }
    }

    /// Reserves exact transport settlement only from sealed, decorated proof.
    /// Pre-issuance errors keep the original proof in its existing owner.
    fn start_native_message_ack(
        &mut self,
        receipt: Option<String>,
        proof: MessageNativeInputCommit,
    ) -> Result<()> {
        let owner = self
            .message_custody
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("native ACK owner missing"))?;
        let service = self.service.message_service_mut();
        match service.prepare_native_input_acknowledgement(&proof, ACK_METADATA_BYTES)? {
            Some(ticket) => {
                if let Err(error) = owner.root.start_acknowledgement(service, &ticket) {
                    service.cancel_acknowledgement(&ticket)?;
                    return Err(error);
                }
                owner.pending = Some(Pending::Acknowledgement(AcknowledgementReply::Native {
                    receipt,
                    _proof: proof,
                }));
                owner.wake = Some(Wake::Observe);
            }
            None => {
                owner.request_native_scan();
                if let Some(id) = receipt {
                    self.finish_native_message_ack(&id);
                }
            }
        }
        Ok(())
    }

    /// Retains post-ACK scheduler failures as application continuations instead
    /// of replaying physical ACK or dropping the original work at cleanup.
    pub(super) fn finish_native_message_ack(&mut self, id: &str) {
        let continue_turn = self
            .message_custody
            .as_ref()
            .is_some_and(|o| o.shutdown.is_none());
        if let Err(error) = self.service.complete_native_input_ack(id, continue_turn) {
            self.service.defer_native_input_completion(id, error);
        }
    }
}
