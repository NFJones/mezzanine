//! Runtime message and event fanout connection tables.
//!
//! This module owns the small state machines used to track writable runtime
//! message/event socket connections and to flush pending fanout frames. Keeping
//! these tables outside the central runtime service state makes the socket
//! delivery boundary explicit while preserving the existing message and control
//! framing contracts.

use super::{
    AgentId, EventAudience, EventLog, FocusedShellHookDispatch, MessageService, MezError, Result,
    VisibleEvent, delivery_batch_json, encode_control_body, encode_event_notification,
    encode_mmp_body,
};

/// Carries Runtime Message Connection state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMessageConnection {
    /// Stores the connection id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub connection_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: AgentId,
    /// Stores the writable value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub writable: bool,
}

/// Carries Runtime Message Wakeup state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMessageWakeup {
    /// Stores the connection id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub connection_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: AgentId,
    /// Stores the pending messages value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pending_messages: usize,
}

/// Defines the Runtime Message Fanout Sink behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary
/// consumed by higher-level orchestration code.
pub trait RuntimeMessageFanoutSink {
    /// Runs the send frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_frame(&mut self, connection_id: &str, frame: &[u8]) -> Result<()>;
}

/// Carries Runtime Event Connection state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEventConnection {
    /// Stores the connection id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub connection_id: String,
    /// Stores the audience value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub audience: EventAudience,
    /// Stores the writable value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub writable: bool,
    /// Stores the last delivered event id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub last_delivered_event_id: u64,
}

/// Carries Runtime Event Wakeup state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEventWakeup {
    /// Stores the connection id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub connection_id: String,
    /// Stores the events value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub events: Vec<VisibleEvent>,
}

/// Defines the Runtime Event Fanout Sink behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary
/// consumed by higher-level orchestration code.
pub trait RuntimeEventFanoutSink {
    /// Runs the send frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_frame(&mut self, connection_id: &str, frame: &[u8]) -> Result<()>;
}

/// Carries Runtime Focused Shell Hook Run state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFocusedShellHookRun {
    /// Stores the enqueued value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub enqueued: Vec<u64>,
    /// Stores the dispatches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub dispatches: Vec<FocusedShellHookDispatch>,
    /// Stores the pending hooks value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pending_hooks: usize,
}

/// Carries Runtime Message Connection Table state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
pub struct RuntimeMessageConnectionTable {
    /// Stores the connections value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) connections: Vec<RuntimeMessageConnection>,
}

impl RuntimeMessageConnectionTable {
    /// Runs the attach operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn attach(
        &mut self,
        connection_id: impl Into<String>,
        agent_id: AgentId,
        writable: bool,
    ) -> Result<()> {
        let connection_id = connection_id.into();
        if connection_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "message connection id must not be empty",
            ));
        }
        if self
            .connections
            .iter()
            .any(|connection| connection.connection_id == connection_id)
        {
            return Err(MezError::conflict("message connection id already exists"));
        }
        self.connections.push(RuntimeMessageConnection {
            connection_id,
            agent_id,
            writable,
        });
        Ok(())
    }

    /// Runs the detach operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn detach(&mut self, connection_id: &str) -> bool {
        let before = self.connections.len();
        self.connections
            .retain(|connection| connection.connection_id != connection_id);
        self.connections.len() != before
    }

    /// Runs the set writable operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_writable(&mut self, connection_id: &str, writable: bool) -> Result<()> {
        let connection = self
            .connections
            .iter_mut()
            .find(|connection| connection.connection_id == connection_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "message connection not found",
                )
            })?;
        connection.writable = writable;
        Ok(())
    }

    /// Runs the wakeups operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn wakeups(
        &self,
        service: &MessageService,
        now_ms: u64,
        limit_per_connection: usize,
    ) -> Vec<RuntimeMessageWakeup> {
        self.connections
            .iter()
            .filter(|connection| connection.writable)
            .filter_map(|connection| {
                let batch = service
                    .fanout_ready_for(&connection.agent_id, now_ms, limit_per_connection)
                    .ok()??;
                Some(RuntimeMessageWakeup {
                    connection_id: connection.connection_id.clone(),
                    agent_id: connection.agent_id.clone(),
                    pending_messages: batch.batch.messages.len(),
                })
            })
            .collect()
    }

    /// Runs the len operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn len(&self) -> usize {
        self.connections.len()
    }

    /// Runs the is empty operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }
}

/// Runs the flush runtime message wakeup operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn flush_runtime_message_wakeup<S>(
    service: &mut MessageService,
    wakeup: &RuntimeMessageWakeup,
    now_ms: u64,
    limit: usize,
    sink: &mut S,
) -> Result<usize>
where
    S: RuntimeMessageFanoutSink,
{
    let Some(batch) = service.fanout_ready_for(&wakeup.agent_id, now_ms, limit)? else {
        return Ok(0);
    };
    let body = delivery_batch_json(&batch.batch);
    let frame = encode_mmp_body(&body);
    sink.send_frame(&wakeup.connection_id, &frame)?;
    service.acknowledge_fanout_batch(&batch)?;
    Ok(batch.batch.messages.len())
}

/// Runs the flush runtime message wakeups operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn flush_runtime_message_wakeups<S>(
    service: &mut MessageService,
    wakeups: &[RuntimeMessageWakeup],
    now_ms: u64,
    limit_per_connection: usize,
    sink: &mut S,
) -> Result<usize>
where
    S: RuntimeMessageFanoutSink,
{
    if limit_per_connection == 0 {
        return Err(MezError::invalid_args(
            "runtime message wakeup limit must be greater than zero",
        ));
    }
    let mut delivered = 0usize;
    for wakeup in wakeups {
        delivered +=
            flush_runtime_message_wakeup(service, wakeup, now_ms, limit_per_connection, sink)?;
    }
    Ok(delivered)
}

/// Carries Runtime Event Connection Table state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default)]
pub struct RuntimeEventConnectionTable {
    /// Stores the connections value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) connections: Vec<RuntimeEventConnection>,
}

impl RuntimeEventConnectionTable {
    /// Runs the attach operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn attach(
        &mut self,
        connection_id: impl Into<String>,
        audience: EventAudience,
        writable: bool,
        last_delivered_event_id: u64,
    ) -> Result<()> {
        let connection_id = connection_id.into();
        if connection_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "event connection id must not be empty",
            ));
        }
        if self
            .connections
            .iter()
            .any(|connection| connection.connection_id == connection_id)
        {
            return Err(MezError::conflict("event connection id already exists"));
        }
        if matches!(audience, EventAudience::PendingObserver { .. }) {
            return Err(MezError::forbidden(
                "pending observer event streams are not allowed before approval",
            ));
        }
        self.connections.push(RuntimeEventConnection {
            connection_id,
            audience,
            writable,
            last_delivered_event_id,
        });
        Ok(())
    }

    /// Runs the detach operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn detach(&mut self, connection_id: &str) -> bool {
        let before = self.connections.len();
        self.connections
            .retain(|connection| connection.connection_id != connection_id);
        self.connections.len() != before
    }

    /// Runs the set writable operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_writable(&mut self, connection_id: &str, writable: bool) -> Result<()> {
        let connection = self
            .connections
            .iter_mut()
            .find(|connection| connection.connection_id == connection_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "event connection not found",
                )
            })?;
        connection.writable = writable;
        Ok(())
    }

    /// Runs the mark delivered operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mark_delivered(&mut self, connection_id: &str, event_id: u64) -> Result<()> {
        let connection = self
            .connections
            .iter_mut()
            .find(|connection| connection.connection_id == connection_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "event connection not found",
                )
            })?;
        connection.last_delivered_event_id = connection.last_delivered_event_id.max(event_id);
        Ok(())
    }

    /// Runs the wakeups operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn wakeups(
        &self,
        event_log: Option<&EventLog>,
        limit_per_connection: usize,
    ) -> Vec<RuntimeEventWakeup> {
        let Some(event_log) = event_log else {
            return Vec::new();
        };
        self.connections
            .iter()
            .filter(|connection| connection.writable)
            .filter_map(|connection| {
                let events = event_log.replay_after_for(
                    &connection.audience,
                    connection.last_delivered_event_id,
                    limit_per_connection,
                );
                if events.is_empty() {
                    None
                } else {
                    Some(RuntimeEventWakeup {
                        connection_id: connection.connection_id.clone(),
                        events,
                    })
                }
            })
            .collect()
    }

    /// Runs the len operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn len(&self) -> usize {
        self.connections.len()
    }

    /// Runs the is empty operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }
}

/// Runs the flush runtime event wakeup operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn flush_runtime_event_wakeup<S>(
    connections: &mut RuntimeEventConnectionTable,
    wakeup: &RuntimeEventWakeup,
    sink: &mut S,
) -> Result<usize>
where
    S: RuntimeEventFanoutSink,
{
    if wakeup.events.is_empty() {
        return Ok(0);
    }
    let mut frame = Vec::new();
    for event in &wakeup.events {
        let notification = encode_event_notification(event);
        frame.extend_from_slice(&encode_control_body(&notification));
    }
    sink.send_frame(&wakeup.connection_id, &frame)?;
    let mut delivered = 0usize;
    for event in &wakeup.events {
        connections.mark_delivered(&wakeup.connection_id, event.id)?;
        delivered += 1;
    }
    Ok(delivered)
}

/// Runs the flush runtime event wakeups operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn flush_runtime_event_wakeups<S>(
    connections: &mut RuntimeEventConnectionTable,
    wakeups: &[RuntimeEventWakeup],
    sink: &mut S,
) -> Result<usize>
where
    S: RuntimeEventFanoutSink,
{
    let mut delivered = 0usize;
    for wakeup in wakeups {
        delivered += flush_runtime_event_wakeup(connections, wakeup, sink)?;
    }
    Ok(delivered)
}
