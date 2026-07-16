//! Unit tests for message service delivery, MMP dispatch, and fanout behavior.

use super::{
    MessageFanoutSink, decode_mmp_frame, encode_mmp_body, flush_message_fanout,
    flush_message_fanout_for, handle_mmp_frame,
};
use crate::MezError;
use crate::error::Result;
use mez_agent::messaging::{
    AgentPresenceStatus, DeliveryStatus, Envelope, MessageConnection, MessageErrorKind,
    MessageService, Recipient, SenderIdentity, TaskResultPayload, TaskState, TaskStatusPayload,
    dispatch_mmp_body, mmp_error_code, validate_message_type,
};
use mez_core::ids::IdFactory;
use mez_core::ids::{AgentId, PaneId, WindowId};

/// Carries Collecting Fanout Sink state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Default)]
struct CollectingFanoutSink {
    /// Stores the frames value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    frames: Vec<(AgentId, Vec<u8>)>,
}

impl MessageFanoutSink for CollectingFanoutSink {
    /// Runs the send frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_frame(&mut self, recipient: &AgentId, frame: &[u8]) -> Result<()> {
        self.frames.push((recipient.clone(), frame.to_vec()));
        Ok(())
    }
}

/// Carries Failing Fanout Sink state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct FailingFanoutSink;

impl MessageFanoutSink for FailingFanoutSink {
    /// Runs the send frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_frame(&mut self, _recipient: &AgentId, _frame: &[u8]) -> Result<()> {
        Err(MezError::new(
            crate::error::MezErrorKind::Io,
            "fixture write failed",
        ))
    }
}

/// Runs the envelope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn envelope(sender: SenderIdentity) -> Envelope {
    Envelope {
        protocol: "mmp/1",
        id: "m1".to_string(),
        message_type: "send".to_string(),
        time: "message:test".to_string(),
        sender,
        recipient: Recipient::Group("session".to_string()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".to_string(),
        payload: "hello".to_string(),
        extension_fields: Vec::new(),
    }
}

mod availability;
mod delivery;
mod fanout;
mod payloads;
mod presence;
mod retention;
mod service;
mod transport_body;
