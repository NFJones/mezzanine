//! Original-source native dedup, canonical storage and exact ACK continuations.
//!
//! One actor continuation owns each real root job. Pending canonical receipts
//! and sealed ACKs take precedence over new probes; cold payloads use the same
//! owned read lane only after qualified absence. A historical positive settles
//! old transport without reinjection or provider/task creation. Scan progress is
//! payload-free and stops when exhausted until an actual delivery notification.
//! A notification behind an active cursor coalesces into one further full pass.

use mez_agent::messaging::MessageNativeInputAnchor;

use super::*;

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
            self.start_native_message_ack(Some(id), proof);
            return;
        }
        if let Some(proof) = self
            .message_custody
            .as_mut()
            .and_then(|o| o.recovered_native_ack.take())
        {
            self.start_native_message_ack(None, proof);
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

    /// Reserves exact original transport settlement only from sealed proof.
    fn start_native_message_ack(
        &mut self,
        receipt: Option<String>,
        proof: mez_agent::messaging::MessageNativeInputCommit,
    ) {
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        let service = self.service.message_service_mut();
        match service.prepare_native_input_acknowledgement(&proof, ACK_METADATA_BYTES) {
            Ok(Some(ticket)) => match owner.root.start_acknowledgement(service, &ticket) {
                Ok(()) => {
                    owner.pending = Some(Pending::Acknowledgement(AcknowledgementReply::Native {
                        receipt,
                        _proof: proof,
                    }));
                    owner.wake = Some(Wake::Observe);
                }
                Err(error) => {
                    let _ = service.cancel_acknowledgement(&ticket);
                    owner.failure = Some(error);
                }
            },
            Ok(None) => {
                owner.request_native_scan();
                if let Some(id) = receipt {
                    let _ = self.service.complete_native_input_ack(
                        &id,
                        crate::runtime::current_unix_millis(),
                        owner.shutdown.is_none(),
                    );
                }
            }
            Err(error) => {
                owner.failure = Some(error.into());
            }
        }
    }
}
