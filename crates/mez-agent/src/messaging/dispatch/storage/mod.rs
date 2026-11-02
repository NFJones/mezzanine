//! Typed raw transport storage preparation without implicit in-memory success.
//!
//! Parsing captures the authenticated raw sender and validates the original wire
//! envelope without allocating a sequence. The actor selects its available store
//! lane before preparation reserves the existing lower ledger. Only exact live
//! storage-qualified replay may return an accepted duplicate; snapshots and
//! ordinary memory receipts cannot synthesize that qualification. Completion
//! formatting is shared with ordinary dispatch and grants no storage authority.

use super::*;
use crate::messaging::{
    Delivery, DeliveryCursor, MessageAcceptanceTicket, MessageAcknowledgementTicket,
    MessageStorageScope,
};

/// Original parsed semantic input, private so wire data cannot construct a proof.
pub struct MmpStorageRequest {
    /// No actor queue retains both this transient payload and its lower source.
    kind: RequestKind,
}

/// One of the only raw operations that requires storage before publication.
enum RequestKind {
    /// Authenticated original full envelope; selector resolution awaits admission.
    Acceptance {
        sender: AgentId,
        envelope: Box<Envelope>,
        scope: MessageScope,
    },
    /// Caller-confirmed transport cursor; lower offered-prefix validation follows.
    Acknowledgement {
        recipient: AgentId,
        sequence: MessageSequence,
    },
}

/// Prepared canonical reservation or exact qualified/inert replay, never I/O.
#[derive(Debug)]
pub enum MmpStoragePreparation {
    /// New occurrence, invisible until actual storage commit and actor adoption.
    Acceptance(MessageAcceptanceTicket),
    /// Exact previously storage-qualified live occurrence; no new write.
    Accepted(Delivery),
    /// Original offered cursor, unchanged until durable ACK adoption.
    Acknowledgement(MessageAcknowledgementTicket),
    /// Duplicate/regressive ACK, with the existing cursor and no new write.
    Acknowledged(DeliveryCursor),
}

impl std::fmt::Debug for MmpStorageRequest {
    /// Reveals no original payload, sender identity or private authority.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MmpStorageRequest")
            .finish_non_exhaustive()
    }
}

impl MmpStorageRequest {
    /// Reserves only after the actor selects actual root capacity. Returns no
    /// accepted=true for a fresh occurrence, memory-only receipt or restored fact.
    /// All validation/capacity errors leave fresh acceptance unpublished.
    pub fn prepare(
        self,
        service: &mut MessageService,
        storage_scope: &MessageStorageScope,
        now_ms: u64,
        receipt_budget: usize,
        acknowledgement_budget: usize,
    ) -> Result<MmpStoragePreparation> {
        match self.kind {
            RequestKind::Acceptance {
                sender,
                envelope,
                scope,
            } => {
                if let Some(delivery) = service.storage_qualified_duplicate(
                    &sender,
                    &envelope,
                    storage_scope,
                    scope,
                    now_ms,
                )? {
                    return Ok(MmpStoragePreparation::Accepted(delivery));
                }
                service
                    .prepare_acceptance(&sender, *envelope, scope, now_ms, receipt_budget, None)
                    .map(MmpStoragePreparation::Acceptance)
            }
            RequestKind::Acknowledgement {
                recipient,
                sequence,
            } => {
                match service.prepare_acknowledgement(
                    &recipient,
                    sequence,
                    acknowledgement_budget,
                )? {
                    Some(ticket) => Ok(MmpStoragePreparation::Acknowledgement(ticket)),
                    None => service
                        .subscription(&recipient)
                        .cloned()
                        .map(MmpStoragePreparation::Acknowledged)
                        .ok_or_else(|| MessageError::invalid_state("MMP subscription unavailable")),
                }
            }
        }
    }
}

/// Parses storage-affecting wire operations without changing service or connection.
/// Other valid request types return None for the ordinary identity/audit adapter.
/// Errors are protocol errors; the product still owns byte framing and limits.
pub fn parse_mmp_storage_request(
    body: &str,
    service: &MessageService,
    connection: &MessageConnection,
) -> Result<Option<MmpStorageRequest>> {
    let message_type = request_message_type(body)?;
    let kind = match message_type.as_str() {
        "send" | "task_status" | "task_result" => {
            let sender_id = require_registered_connection(connection)?.clone();
            let sender = service
                .registered_identity(&sender_id)
                .cloned()
                .ok_or_else(|| MessageError::forbidden("unregistered agent connection"))?;
            let scope = parse_transport_message_scope(body)?;
            let envelope = parse_transport_envelope(body, message_type, sender)?;
            RequestKind::Acceptance {
                sender: sender_id,
                envelope: Box::new(envelope),
                scope,
            }
        }
        "ack" => {
            let recipient = require_registered_connection(connection)?.clone();
            let sequence = json_number_field(body, "sequence")
                .or_else(|| json_number_field(body, "last_sequence"))
                .ok_or_else(|| MessageError::invalid_args("ack requires sequence"))?
                .parse::<MessageSequence>()
                .map_err(|_| MessageError::invalid_args("ack sequence is invalid"))?;
            RequestKind::Acknowledgement {
                recipient,
                sequence,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(MmpStorageRequest { kind }))
}

/// Formats actual canonical acceptance or an exact qualified duplicate.
/// Calling this formatter is not proof that its delivery was committed.
pub fn mmp_acceptance_response(delivery: &Delivery) -> String {
    format!(
        r#"{{"protocol":"mmp/1","type":"ack","message_id":"{}","queued_recipients":{},"status":"{}"}}"#,
        json_escape(&delivery.message_id),
        delivery.queued_recipients,
        delivery_status_name(delivery.status)
    )
}

/// Formats an actually adopted or inert duplicate raw transport cursor.
pub fn mmp_acknowledgement_response(cursor: &DeliveryCursor) -> String {
    format!(
        r#"{{"protocol":"mmp/1","type":"ack","last_sequence":{}}}"#,
        cursor.last_sequence
    )
}

/// Encodes a lower protocol rejection with no fabricated acceptance or certainty.
pub fn mmp_storage_error_response(error: &MessageError) -> String {
    mmp_error(
        mmp_error_code(error),
        error.message(),
        false,
        mmp_delivery_status(error),
    )
}

#[cfg(test)]
mod tests;
