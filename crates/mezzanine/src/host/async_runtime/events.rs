//! Async Runtime Events implementation.
//!
//! This module owns the async runtime events boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AsRawFd, AsyncRuntimeEventConnectionConfig, AsyncRuntimeSessionHandle, AsyncWriteExt, Framed,
    JoinSet, MezError, ProtocolFrameCodec, Result, RuntimeLifecycleState, StreamExt, UnixListener,
    UnixStream, authenticated_unix_peer_uid, encode_control_body, encode_event_notification,
};
#[cfg(test)]
use super::{EventAudience, authorize_unix_peer_raw_fd};
#[cfg(test)]
use crate::runtime::RuntimeEventConnectionTable;
use crate::runtime::{RenderInvalidationReason, RuntimeSideEffect};
use std::io::ErrorKind;
use std::sync::Arc;

const UNIX_EVENT_INITIALIZE_MAX_CONTENT_LENGTH: usize = 64 * 1024;

// Async runtime event stream handling.

/// Carries Async Runtime Event Flush state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[cfg(test)]
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
#[cfg(test)]
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
#[cfg(test)]
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
#[cfg(test)]
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
    let mut event_delivery = handle.event_delivery_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(delivered, state) {
            return Ok(delivered);
        }

        let _ = event_delivery.borrow_and_update();
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
                changed = event_delivery.changed() => {
                    if changed.is_err() {
                        return Ok(delivered);
                    }
                }
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

/// Returns the output-base invalidation required by exact-client render effects.
///
/// Unix attached clients fetch an authoritative view after this wakeup, so the
/// classification must match the legacy Iroh notification path. A divider drag
/// is intentionally differential: it replaces only the retained body and
/// divider cells without forcing a terminal-wide erase.
fn unix_render_wakeup_invalidation(effects: &[RuntimeSideEffect]) -> Option<bool> {
    let mut render_required = false;
    let mut invalidate_output = false;
    for effect in effects {
        let RuntimeSideEffect::RenderClient { reason, .. } = effect else {
            continue;
        };
        render_required = true;
        invalidate_output |= matches!(reason, RenderInvalidationReason::Resize);
    }
    render_required.then_some(invalidate_output)
}

/// Encodes one non-durable exact-client render wakeup for a Unix attach.
///
/// Render wakeups deliberately do not use `event/*`: they carry no replayable
/// runtime event and only tell the bound foreground client to pull its current
/// authoritative terminal view.
fn unix_render_wakeup_notification(invalidate_output: bool) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "render/wakeup",
        "params": {"invalidate_output": invalidate_output},
    })
    .to_string()
}

/// Serves one Unix event stream after a client-bound initialization frame.
///
/// The binding token is consumed exactly once through the actor. Every event
/// batch is then reauthorized against the exact live client, so observer
/// attachment cutoffs and detach transitions take effect immediately.
#[cfg_attr(test, allow(dead_code))]
pub(crate) async fn serve_bound_async_runtime_event_connection<F>(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeEventConnectionConfig,
    connection_id: String,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    let peer_uid = authenticated_unix_peer_uid(stream.as_raw_fd(), config.owner_uid)?;
    let mut framed = Framed::new(
        stream,
        ProtocolFrameCodec::new(UNIX_EVENT_INITIALIZE_MAX_CONTENT_LENGTH)?,
    );
    let frame = framed
        .next()
        .await
        .ok_or_else(|| MezError::forbidden("event stream requires initialization"))??;
    let initialize: serde_json::Value = serde_json::from_str(&frame.body)
        .map_err(|_| MezError::invalid_args("event initialization is not valid JSON"))?;
    if initialize.get("method").and_then(serde_json::Value::as_str) != Some("event/initialize") {
        return Err(MezError::forbidden(
            "first event request must be event/initialize",
        ));
    }
    let params = initialize
        .get("params")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_args("event/initialize requires params"))?;
    let token = params
        .get("binding_token")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty() && token.len() <= 512)
        .ok_or_else(|| MezError::invalid_args("event/initialize requires binding_token"))?;
    let after_event_id = params
        .get("after_event_id")
        .map(|value| {
            value.as_u64().ok_or_else(|| {
                MezError::invalid_args(
                    "event/initialize after_event_id must be a non-negative integer",
                )
            })
        })
        .transpose()?
        .unwrap_or(0);
    let caller_client_id = handle
        .consume_unix_event_binding(token.to_string(), peer_uid)
        .await?;

    let mut delivered = 0u64;
    let mut last_delivered_event_id = after_event_id;
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut event_delivery = handle.event_delivery_watcher();
    let mut side_effect_delivery = handle.side_effect_delivery_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(delivered, state) {
            return Ok(delivered);
        }
        let _ = event_delivery.borrow_and_update();
        let _ = side_effect_delivery.borrow_and_update();
        let wakeups = match handle
            .event_wakeups_for_client(
                caller_client_id.clone(),
                connection_id.clone(),
                last_delivered_event_id,
                config.limit_per_connection,
            )
            .await
        {
            Ok(wakeups) => wakeups,
            Err(_) => return Ok(delivered),
        };
        let render_wakeup = match handle
            .drain_render_side_effects_for_client(
                caller_client_id.clone(),
                config.limit_per_connection,
            )
            .await
        {
            Ok(effects) => unix_render_wakeup_invalidation(&effects),
            Err(_) => return Ok(delivered),
        };
        let mut batch_last = None;
        for wakeup in wakeups {
            for event in wakeup.events {
                let frame = encode_control_body(&encode_event_notification(&event));
                if let Err(error) = framed.get_mut().write_all(&frame).await {
                    if event_stream_disconnect(error.kind()) {
                        return Ok(delivered);
                    }
                    return Err(error.into());
                }
                batch_last = Some(event.id);
                delivered = delivered.saturating_add(1);
            }
        }
        if let Some(invalidate_output) = render_wakeup {
            let frame = encode_control_body(&unix_render_wakeup_notification(invalidate_output));
            if let Err(error) = framed.get_mut().write_all(&frame).await {
                if event_stream_disconnect(error.kind()) {
                    return Ok(delivered);
                }
                return Err(error.into());
            }
        }
        if batch_last.is_some() || render_wakeup.is_some() {
            if let Err(error) = framed.get_mut().flush().await {
                if event_stream_disconnect(error.kind()) {
                    return Ok(delivered);
                }
                return Err(error.into());
            }
            if let Some(batch_last) = batch_last {
                last_delivered_event_id = batch_last;
            }
            continue;
        }
        tokio::select! {
            changed = event_delivery.changed() => {
                if changed.is_err() {
                    return Ok(delivered);
                }
            }
            changed = side_effect_delivery.changed() => {
                if changed.is_err() {
                    return Ok(delivered);
                }
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Ok(delivered);
                }
            }
        }
    }
}

/// Accepts Unix event sockets that authenticate with one-time client bindings.
pub async fn serve_bound_async_runtime_event_listener<F>(
    listener: &UnixListener,
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeEventConnectionConfig,
    max_connections: u64,
    should_stop: F,
) -> Result<u64>
where
    F: Fn(u64, u64, RuntimeLifecycleState) -> bool + Send + Sync + 'static,
{
    if max_connections == 0 {
        return Err(MezError::invalid_args(
            "async event listener max connections must be greater than zero",
        ));
    }
    let mut accepted_connections = 0u64;
    let mut tasks = JoinSet::new();
    let should_stop = Arc::new(should_stop);
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(accepted_connections, 0, state) {
            break;
        }
        let (mut stream, _addr) = tokio::select! {
            accepted = listener.accept(), if (tasks.len() as u64) < max_connections => accepted?,
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    break;
                }
                continue;
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                let Some(joined) = joined else {
                    continue;
                };
                joined.map_err(|error| {
                    MezError::invalid_state(format!(
                        "async event connection task failed: {error}"
                    ))
                })??;
                continue;
            }
        };
        let connection_index = accepted_connections;
        let connection_id = format!("unix-events-{connection_index}");
        let connection_handle = handle.clone();
        let connection_should_stop = should_stop.clone();
        tasks.spawn(async move {
            let connection_result = serve_bound_async_runtime_event_connection(
                &mut stream,
                &connection_handle,
                config,
                connection_id,
                |delivered, state| connection_should_stop(connection_index, delivered, state),
            )
            .await;
            if let Err(error) = connection_result
                && !matches!(
                    error.kind(),
                    crate::error::MezErrorKind::Forbidden | crate::error::MezErrorKind::InvalidArgs
                )
            {
                return Err(error);
            }
            Ok(())
        });
        accepted_connections = accepted_connections.saturating_add(1);
    }

    while let Some(joined) = tasks.join_next().await {
        joined.map_err(|error| {
            MezError::invalid_state(format!("async event connection task failed: {error}"))
        })??;
    }

    Ok(accepted_connections)
}

/// Runs the serve async runtime event listener operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
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
