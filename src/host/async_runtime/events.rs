//! Async Runtime Events implementation.
//!
//! This module owns the async runtime events boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AsRawFd, AsyncRuntimeEventConnectionConfig, AsyncRuntimeSessionHandle, AsyncWriteExt,
    EventAudience, Result, RuntimeEventConnectionTable, RuntimeLifecycleState, UnixListener,
    UnixStream, authorize_unix_peer_raw_fd, encode_control_body, encode_event_notification,
};
use std::io::ErrorKind;

// Async runtime event stream handling.

/// Carries Async Runtime Event Flush state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AsyncRuntimeEventFlush {
    /// Represents the Delivered case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Delivered(usize),
    /// Represents the Disconnected case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Disconnected(usize),
}

/// Runs the flush async runtime event wakeups to stream operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn flush_async_runtime_event_wakeups_to_stream(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connections: &mut RuntimeEventConnectionTable,
    limit_per_connection: usize,
) -> Result<usize> {
    match flush_async_runtime_event_wakeups_to_stream_outcome(
        stream,
        handle,
        connections,
        limit_per_connection,
    )
    .await?
    {
        AsyncRuntimeEventFlush::Delivered(delivered)
        | AsyncRuntimeEventFlush::Disconnected(delivered) => Ok(delivered),
    }
}

/// Runs the flush async runtime event wakeups to stream outcome operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn flush_async_runtime_event_wakeups_to_stream_outcome(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connections: &mut RuntimeEventConnectionTable,
    limit_per_connection: usize,
) -> Result<AsyncRuntimeEventFlush> {
    if limit_per_connection == 0 {
        return Ok(AsyncRuntimeEventFlush::Delivered(0));
    }

    let wakeups = handle
        .event_wakeups(connections.clone(), limit_per_connection)
        .await?;
    let mut delivered = 0usize;
    for wakeup in wakeups {
        for event in wakeup.events {
            let notification = encode_event_notification(&event);
            let frame = encode_control_body(&notification);
            if let Err(error) = stream.write_all(&frame).await {
                if event_stream_disconnect(error.kind()) {
                    return Ok(AsyncRuntimeEventFlush::Disconnected(delivered));
                }
                return Err(error.into());
            }
            connections.mark_delivered(&wakeup.connection_id, event.id)?;
            delivered += 1;
        }
    }
    if let Err(error) = stream.flush().await {
        if event_stream_disconnect(error.kind()) {
            return Ok(AsyncRuntimeEventFlush::Disconnected(delivered));
        }
        return Err(error.into());
    }
    Ok(AsyncRuntimeEventFlush::Delivered(delivered))
}

/// Runs the serve async runtime event connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_event_connection<F>(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connections: &mut RuntimeEventConnectionTable,
    config: AsyncRuntimeEventConnectionConfig,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    authorize_unix_peer_raw_fd(stream.as_raw_fd(), config.owner_uid)?;
    let mut delivered = 0u64;
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(delivered, state) {
            return Ok(delivered);
        }

        let outcome = flush_async_runtime_event_wakeups_to_stream_outcome(
            stream,
            handle,
            connections,
            config.limit_per_connection,
        )
        .await?;
        let count = match outcome {
            AsyncRuntimeEventFlush::Delivered(count) => count,
            AsyncRuntimeEventFlush::Disconnected(count) => {
                return Ok(delivered.saturating_add(count as u64));
            }
        };
        if count == 0 {
            tokio::select! {
                _ = handle.wait_for_event_delivery() => {}
                changed = lifecycle.changed() => {
                    if changed.is_err() {
                        return Ok(delivered);
                    }
                }
            }
        } else {
            delivered = delivered.saturating_add(count as u64);
        }
    }
}

/// Runs the event stream disconnect operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn event_stream_disconnect(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::BrokenPipe | ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset
    )
}

/// Runs the serve async runtime event listener operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_event_listener<F, G>(
    listener: &UnixListener,
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeEventConnectionConfig,
    mut connection_factory: G,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, u64, RuntimeLifecycleState) -> bool,
    G: FnMut(u64) -> Result<(String, EventAudience, u64)>,
{
    let mut served_connections = 0u64;
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(served_connections, 0, state) {
            return Ok(served_connections);
        }

        let (mut stream, _addr) = tokio::select! {
            accepted = listener.accept() => accepted?,
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Ok(served_connections);
                }
                continue;
            }
        };
        let (connection_id, audience, last_delivered_event_id) =
            connection_factory(served_connections)?;
        let mut connections = RuntimeEventConnectionTable::default();
        connections.attach(connection_id, audience, true, last_delivered_event_id)?;
        serve_async_runtime_event_connection(
            &mut stream,
            handle,
            &mut connections,
            config,
            |delivered, state| should_stop(served_connections, delivered, state),
        )
        .await?;
        served_connections = served_connections.saturating_add(1);
    }
}
