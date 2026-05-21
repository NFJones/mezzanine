//! Local agent message protocol primitives.
//!
//! The transport listener is still pending. This module models agent
//! registration, sender identity validation, bounded local delivery queues,
//! recipient matching, TTL filtering, and MMP frame helpers so spoofing and
//! delivery rules are testable early.

/// Exposes the dispatch module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod dispatch;
/// Exposes the framing module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod framing;
/// Exposes the json module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod json;
/// Exposes the service module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod service;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;
/// Exposes the validation module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod validation;

pub use dispatch::{
    MessageFanoutSink, dispatch_mmp_body, flush_message_fanout, flush_message_fanout_for,
};
pub use framing::{decode_mmp_frame, encode_mmp_body, handle_mmp_frame};
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
pub(crate) use validation::validate_mmp_payload_metadata;

#[cfg(test)]
use json::mmp_error_code;
#[cfg(test)]
use validation::validate_message_type;

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
