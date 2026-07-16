//! Runtime control protocol, messaging, and event-log state ownership.
//!
//! This component owns replay/idempotency state and the canonical message and
//! lifecycle-event services used by control clients and observer fanout.

use crate::control::ControlIdempotencyCache;
use crate::event::EventLog;
use mez_agent::messaging::MessageService;

/// Owns control replay, messaging, and event-fanout state.
#[derive(Debug)]
pub(crate) struct RuntimeControlComponent {
    idempotency: ControlIdempotencyCache,
    message_service: MessageService,
    event_log: Option<EventLog>,
}

impl RuntimeControlComponent {
    /// Builds control ownership from constructor-provided services.
    pub(crate) fn new(
        idempotency: ControlIdempotencyCache,
        message_service: MessageService,
        event_log: Option<EventLog>,
    ) -> Self {
        Self {
            idempotency,
            message_service,
            event_log,
        }
    }

    /// Returns the idempotency cache for read-only diagnostics.
    pub(crate) fn idempotency(&self) -> &ControlIdempotencyCache {
        &self.idempotency
    }

    /// Returns the idempotency cache for request dispatch mutation.
    pub(crate) fn idempotency_mut(&mut self) -> &mut ControlIdempotencyCache {
        &mut self.idempotency
    }

    /// Returns the canonical message service.
    pub(crate) fn message_service(&self) -> &MessageService {
        &self.message_service
    }

    /// Returns the canonical message service for queue and presence mutation.
    pub(crate) fn message_service_mut(&mut self) -> &mut MessageService {
        &mut self.message_service
    }

    /// Returns the optional lifecycle event log.
    pub(crate) fn event_log(&self) -> Option<&EventLog> {
        self.event_log.as_ref()
    }

    /// Returns the lifecycle event log for append operations.
    pub(crate) fn event_log_mut(&mut self) -> Option<&mut EventLog> {
        self.event_log.as_mut()
    }
}
