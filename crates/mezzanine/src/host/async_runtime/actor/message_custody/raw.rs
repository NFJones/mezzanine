//! Original raw send/ACK continuations crossing the actor storage boundary.
//!
//! Decode and parse once, select the idle root before sequence reservation, then
//! retain only bounded connection/reply metadata beside the root's charged lower
//! source. Actual positive/refusal publication formats the original reply without
//! rerunning a frame. Socket cancellation retires only its original raw identity;
//! it cannot turn an issued or ambiguous acceptance into an unsent request.

use mez_agent::messaging::{
    Delivery, DeliveryCursor, MessageConnection, MessageError, MessageErrorKind,
    MmpStoragePreparation, MmpStorageRequest, mmp_acceptance_response,
    mmp_acknowledgement_response, mmp_storage_error_response, parse_mmp_storage_request,
};
use mez_core::ids::AgentId;
use tokio::sync::{oneshot, watch};

use super::*;
use crate::host::async_runtime::AsyncMessageInputResult;
use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};

/// Original socket ownership and observer metadata; contains no frame or payload.
pub(in crate::host::async_runtime::actor) struct RawMessageContinuation {
    /// Original connection state, updated only by its actual adopted ACK.
    pub(in crate::host::async_runtime::actor) connection: MessageConnection,
    /// Clock at original selection, never refreshed to restart an accepted TTL.
    pub(in crate::host::async_runtime::actor) now_ms: u64,
    /// Monotonic selection anchor; storage/retry time consumes the original TTL.
    selected_at: Instant,
    /// Exact consumed input length for the original frame's eventual response.
    pub(in crate::host::async_runtime::actor) consumed: usize,
    /// Socket's exact fresh-registration lease, independent of reply cancellation.
    pub(in crate::host::async_runtime::actor) owner: Option<watch::Sender<Option<AgentId>>>,
    /// Original caller observer; closed receiver does not revoke storage custody.
    pub(in crate::host::async_runtime::actor) reply:
        oneshot::Sender<Result<AsyncMessageInputResult>>,
    /// Prior identity for the shared hello/replacement cleanup boundary.
    pub(in crate::host::async_runtime::actor) previous_agent_id: Option<AgentId>,
    /// Prior lifecycle for the ordinary shared notification boundary.
    pub(in crate::host::async_runtime::actor) previous_lifecycle_state: RuntimeLifecycleState,
}

impl RawMessageContinuation {
    /// Advances the original logical delivery clock by monotonic elapsed time.
    /// This observation never changes acceptance time, envelope TTL or identity;
    /// saturation keeps extreme timestamps from wrapping into fresh eligibility.
    pub(in crate::host::async_runtime::actor) fn observation_now_ms(&self) -> u64 {
        self.now_ms.saturating_add(
            u64::try_from(self.selected_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        )
    }

    /// Captures the original connection before either direct or deferred dispatch.
    pub(in crate::host::async_runtime::actor) fn new(
        connection: MessageConnection,
        now_ms: u64,
        owner: Option<watch::Sender<Option<AgentId>>>,
        reply: oneshot::Sender<Result<AsyncMessageInputResult>>,
        previous_lifecycle_state: RuntimeLifecycleState,
    ) -> Self {
        Self {
            previous_agent_id: connection.agent_id.clone(),
            connection,
            now_ms,
            selected_at: Instant::now(),
            consumed: 0,
            owner,
            reply,
            previous_lifecycle_state,
        }
    }
}

/// Transient decoded input, discarded once reserved or rejected; never queued.
pub(in crate::host::async_runtime::actor) enum RawPreparation {
    /// Semantic request parsed before actual capacity selection/reservation.
    Request(MmpStorageRequest),
    /// Wire rejection of parsing/capacity; no lower write has been issued.
    Reply(String),
}

/// Only actual adoption or a qualified refusal constructs these final outcomes.
pub(super) enum RawCompletion {
    /// Actual root adoption or exact live storage-qualified duplicate.
    Acceptance(Delivery),
    /// Actually adopted original offered cursor, or inert duplicate.
    Acknowledgement(DeliveryCursor),
    /// Never-issued validation/capacity error or qualified original noncommit.
    Rejected(MessageError),
}

impl AsyncRuntimeSessionActor {
    /// Recognizes writes without running ordinary dispatch. Identity/discovery/
    /// receive requests keep their existing audit adapter. Parse errors stay MMP
    /// wire rejections; framing/lifecycle errors retain their existing provenance.
    pub(in crate::host::async_runtime::actor) fn prepare_stored_raw_message(
        &self,
        input: &[u8],
        max_content_length: usize,
        connection: &MessageConnection,
    ) -> Result<Option<(RawPreparation, usize)>> {
        self.service.require_live()?;
        let (body, consumed) = decode_mmp_frame(input, max_content_length)?;
        let request =
            match parse_mmp_storage_request(&body, self.service.message_service(), connection) {
                Ok(Some(request)) => request,
                Ok(None) => return Ok(None),
                Err(error) => {
                    return Ok(Some((
                        RawPreparation::Reply(mmp_storage_error_response(&error)),
                        consumed,
                    )));
                }
            };
        let owner = self
            .message_custody
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("MMP raw storage root unavailable"))?;
        if owner.wake.is_some()
            || owner.pending.is_some()
            || owner.failure.is_some()
            || owner.shutdown.is_some()
        {
            return Ok(Some((
                RawPreparation::Reply(mmp_storage_error_response(&MessageError::new(
                    MessageErrorKind::Capacity,
                    "MMP storage lane busy or unavailable; message not accepted",
                ))),
                consumed,
            )));
        }
        Ok(Some((RawPreparation::Request(request), consumed)))
    }

    /// Reserves/starts exactly one selected raw send or explicit ACK. Original
    /// metadata is retained before any await; errors cancel only never-issued
    /// tickets. No input body or selector is replayed after physical execution.
    pub(in crate::host::async_runtime::actor) fn start_stored_raw_message(
        &mut self,
        raw: RawMessageContinuation,
        prepared: RawPreparation,
    ) {
        let RawPreparation::Request(request) = prepared else {
            if let RawPreparation::Reply(body) = prepared {
                let consumed = raw.consumed;
                self.finish_raw_message_input(raw, Ok((encode_mmp_body(&body), consumed)));
            }
            return;
        };
        let Some(owner) = self.message_custody.as_mut() else {
            self.finish_raw_message_input(
                raw,
                Err(MezError::invalid_state("MMP raw storage root unavailable")),
            );
            return;
        };
        let service = self.service.message_service_mut();
        let mut completion = None;
        match request.prepare(
            service,
            owner.root.storage_scope(),
            raw.now_ms,
            4096,
            ACK_METADATA_BYTES,
        ) {
            Ok(MmpStoragePreparation::Acceptance(ticket)) => {
                match owner.root.start(service, &ticket, None) {
                    Ok(()) => {
                        owner.pending = Some(Pending::Acceptance(raw));
                        owner.wake = Some(Wake::Observe);
                    }
                    Err(error) => {
                        let cancelled = service.cancel_acceptance(&ticket).map_err(Into::into);
                        self.finish_raw_message_input(raw, cancelled.and(Err(error)));
                    }
                }
            }
            Ok(MmpStoragePreparation::Acknowledgement(ticket)) => {
                match owner.root.start_acknowledgement(service, &ticket) {
                    Ok(()) => {
                        owner.pending =
                            Some(Pending::Acknowledgement(AcknowledgementReply::Raw(raw)));
                        owner.wake = Some(Wake::Observe);
                    }
                    Err(error) => {
                        let cancelled = service.cancel_acknowledgement(&ticket).map_err(Into::into);
                        self.finish_raw_message_input(raw, cancelled.and(Err(error)));
                    }
                }
            }
            Ok(MmpStoragePreparation::Accepted(delivery)) => {
                completion = Some((raw, RawCompletion::Acceptance(delivery)))
            }
            Ok(MmpStoragePreparation::Acknowledged(cursor)) => {
                completion = Some((raw, RawCompletion::Acknowledgement(cursor)))
            }
            Err(error) => completion = Some((raw, RawCompletion::Rejected(error))),
        }
        if let Some((raw, completion)) = completion {
            self.complete_stored_raw_message(raw, completion);
        }
    }

    /// Formats a settled original response; never calls frame dispatch again.
    /// Receiver delivery failures remain recoverable after acceptance and cannot
    /// undo it. Shared finalization owns exact lease cleanup and effect wakeups.
    pub(super) fn complete_stored_raw_message(
        &mut self,
        mut raw: RawMessageContinuation,
        completion: RawCompletion,
    ) {
        let body = match completion {
            RawCompletion::Acceptance(delivery) => {
                if self
                    .service
                    .deliver_pending_runtime_agent_messages(raw.observation_now_ms())
                    .is_err()
                {
                    eprintln!("mez: native MMP delivery deferred after storage acceptance");
                }
                mmp_acceptance_response(&delivery)
            }
            RawCompletion::Acknowledgement(cursor) => {
                raw.connection.delivery_cursor = Some(cursor.clone());
                mmp_acknowledgement_response(&cursor)
            }
            RawCompletion::Rejected(error) => mmp_storage_error_response(&error),
        };
        let consumed = raw.consumed;
        self.finish_raw_message_input(raw, Ok((encode_mmp_body(&body), consumed)));
    }
}
