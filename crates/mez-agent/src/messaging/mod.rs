//! Deterministic local-agent messaging protocol and service state.
//!
//! This module owns MMP body contracts, sender and payload validation, JSON
//! dispatch, bounded delivery queues, subscriptions, presence, and snapshot
//! conversion. Product crates retain byte framing, sockets, audit logging,
//! wakeups, and concrete fanout writes.

mod dispatch;
mod error;
mod json;
mod service;
mod types;
mod validation;

pub use dispatch::dispatch_mmp_body;
pub use error::{MessageError, MessageErrorKind, Result};
pub use json::delivery_batch_json;
pub use types::{
    AgentPresenceStatus, Delivery, DeliveryBatch, DeliveryCursor, DeliveryStatus, Envelope,
    FanoutBatch, MMP_CONTENT_TYPE, MessageAcceptedSnapshot, MessageConnection,
    MessageDeliveryCursorSnapshot, MessageDeliverySnapshot, MessageEnvelopeSnapshot,
    MessageExtensionFieldSnapshot, MessageIdentitySnapshot, MessagePresenceSnapshot,
    MessageQueuedEnvelopeSnapshot, MessageRecipientSnapshot, MessageSequence, MessageService,
    MessageServiceSnapshot, PresenceRecord, Recipient, SenderIdentity, SequencedEnvelope,
    TaskResultPayload, TaskState, TaskStatusPayload,
};
pub use validation::{task_state_name, validate_mmp_payload_metadata};

#[doc(hidden)]
pub use json::mmp_error_code;
#[doc(hidden)]
pub use validation::validate_message_type;
