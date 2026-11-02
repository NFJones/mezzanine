//! Deterministic local-agent messaging protocol and service state.
//!
//! This module owns MMP body contracts, sender and payload validation, JSON
//! dispatch, bounded delivery queues, subscriptions, presence, and snapshot
//! conversion. Product crates retain byte framing, sockets, audit logging,
//! wakeups, and concrete fanout writes.

mod dispatch;
mod error;
mod json;
mod outcomes;
mod pending_limits;
mod requests;
mod service;
mod types;
mod validation;

pub use dispatch::{
    MmpStoragePreparation, MmpStorageRequest, dispatch_mmp_body, mmp_acceptance_response,
    mmp_acknowledgement_response, mmp_storage_error_response, parse_mmp_storage_request,
};
pub use error::{MessageError, MessageErrorKind, Result};
pub use json::delivery_batch_json;
pub use outcomes::{
    DeliveryTerminalReason, MessageDeliveryOutcome, MessageDeliveryTerminalSnapshot,
};
pub use pending_limits::PendingDeliveryLimits;
pub use requests::{
    MESSAGE_REQUEST_HANDOFF_BYTES, MessageRequestLimits, MessageRequestOrigin,
    MessageRequestReceipt, MessageRequests,
};
pub use service::{
    MessageAcceptanceAttempt, MessageAcceptanceCommit, MessageAcceptanceDecodeLimits,
    MessageAcceptanceJoin, MessageAcceptanceProof, MessageAcceptanceRejected,
    MessageAcceptanceReservation, MessageAcceptanceSource, MessageAcceptanceStore,
    MessageAcceptanceStoreError, MessageAcceptanceTicket, MessageAcceptanceUncertain,
    MessageAcceptanceWrite, MessageAcceptanceWriteOutcome, MessageAcknowledgementAttempt,
    MessageAcknowledgementCommit, MessageAcknowledgementEntry, MessageAcknowledgementJoin,
    MessageAcknowledgementStore, MessageAcknowledgementTicket, MessageAcknowledgementUncertain,
    MessageAcknowledgementWrite, MessageAcknowledgementWriteOutcome, MessageSenderRegistration,
    MessageStorageScope, decode_message_acceptance,
};
pub use types::{
    AgentPresenceStatus, Delivery, DeliveryBatch, DeliveryCursor, DeliveryStatus, Envelope,
    FanoutBatch, FanoutBudget, MMP_CONTENT_TYPE, MessageAcceptedSnapshot, MessageAudienceSnapshot,
    MessageConnection, MessageDeliveryCursorSnapshot, MessageDeliverySnapshot,
    MessageEnvelopeSnapshot, MessageExtensionFieldSnapshot, MessageFanoutDiagnostics,
    MessageIdentitySnapshot, MessagePresenceSnapshot, MessageQueuedEnvelopeSnapshot,
    MessageRecipientBindingSnapshot, MessageRecipientOfferSnapshot, MessageRecipientSnapshot,
    MessageScope, MessageSenderProvenanceSnapshot, MessageSequence, MessageService,
    MessageServiceSnapshot, PresenceRecord, ProjectMembership, ProjectScopeId, Recipient,
    SenderIdentity, SequencedEnvelope, TaskResultPayload, TaskState, TaskStatusPayload,
};
pub use validation::{
    MMP_OBJECTIVE_MAX_BYTES, MMP_OBJECTIVE_PROMPT_BLOCK_BYTES, MMP_OBJECTIVE_PROMPT_BLOCK_LINES,
    normalize_objective, normalize_optional_objective, objective_spans_prompt_block,
    task_state_name, validate_mmp_payload_metadata,
};

#[doc(hidden)]
pub use json::mmp_error_code;
#[doc(hidden)]
pub use validation::validate_message_type;
