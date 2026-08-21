//! Async control-connection and listener adapters over the runtime actor handle.

#[cfg(test)]
use super::UnixStream;
use super::{
    AsRawFd, AsyncControlInputResult, AsyncRuntimeControlConnectionConfig,
    AsyncRuntimeSessionHandle, AsyncWriteExt, AuthenticatedPeer, ClientEvent,
    ControlConnectionState, Framed, JoinSet, MezError, ProtocolFrameCodec, Result, RuntimeEvent,
    RuntimeEventBatch, RuntimeLifecycleState, SnapshotRepository, StreamExt, UnixListener,
    authenticated_unix_peer_uid, encode_frame,
};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

/// Maximum time terminal daemon shutdown waits for active control responses to
/// finish writing before aborting non-responsive connection tasks.
const TERMINAL_CONTROL_CONNECTION_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// Runs the serve async runtime control connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub async fn serve_async_runtime_control_connection(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
    config: AsyncRuntimeControlConnectionConfig,
) -> Result<usize> {
    serve_async_runtime_control_connection_with_snapshots(stream, handle, connection, config, None)
        .await
}

/// Runs the serve async runtime control connection with snapshots operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub async fn serve_async_runtime_control_connection_with_snapshots(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
    config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<&SnapshotRepository>,
) -> Result<usize> {
    let peer_uid = authenticated_unix_peer_uid(stream.as_raw_fd(), config.owner_uid)?;
    serve_authenticated_async_runtime_control_connection_with_snapshots(
        stream,
        AuthenticatedPeer::unix_user(peer_uid),
        handle,
        connection,
        config,
        snapshots,
    )
    .await
}

#[cfg(test)]
async fn serve_authenticated_async_runtime_control_connection_with_snapshots<S>(
    stream: &mut S,
    peer: AuthenticatedPeer,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
    config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<&SnapshotRepository>,
) -> Result<usize>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    connection.bind_authenticated_peer(peer)?;
    let mut framed = Framed::new(stream, ProtocolFrameCodec::new(config.max_content_length)?);
    let Some(frame) = framed.next().await else {
        return Ok(0);
    };
    let input = encode_frame(&frame?);
    let result = handle_control_input_with_optional_snapshots(
        handle,
        input,
        config.max_content_length,
        connection,
        snapshots,
    )
    .await?;
    *connection = result.connection;
    framed.get_mut().write_all(&result.output).await?;
    framed.get_mut().flush().await?;
    Ok(result.consumed)
}

/// Runs the serve async runtime control connection loop operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub async fn serve_async_runtime_control_connection_loop<F>(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
    config: AsyncRuntimeControlConnectionConfig,
    should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    serve_async_runtime_control_connection_loop_with_snapshots(
        stream,
        handle,
        connection,
        config,
        None,
        should_stop,
    )
    .await
}

/// Runs the serve async runtime control connection loop with snapshots operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub async fn serve_async_runtime_control_connection_loop_with_snapshots<F>(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
    config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<&SnapshotRepository>,
    should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    let peer_uid = authenticated_unix_peer_uid(stream.as_raw_fd(), config.owner_uid)?;
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
        stream,
        AuthenticatedPeer::unix_user(peer_uid),
        handle,
        connection,
        config,
        snapshots,
        should_stop,
    )
    .await
}

/// Serves ordered control frames over an authenticated async byte stream.
///
/// Concrete adapters authenticate the peer before entering this function. The
/// peer identity is transport evidence only; control initialization and method
/// authorization continue to determine application authority.
pub async fn serve_authenticated_async_runtime_control_connection_loop_with_snapshots<S, F>(
    stream: &mut S,
    peer: AuthenticatedPeer,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
    config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<&SnapshotRepository>,
    mut should_stop: F,
) -> Result<u64>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    connection.bind_authenticated_peer(peer)?;
    let mut framed = Framed::new(stream, ProtocolFrameCodec::new(config.max_content_length)?);
    let mut served = 0u64;
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(served, state) {
            return Ok(served);
        }

        tokio::select! {
            frame = framed.next() => {
                let Some(frame) = frame else {
                    submit_control_connection_disconnect_event(handle, connection).await?;
                    return Ok(served);
                };
                let input = encode_frame(&frame?);
                let result = handle_control_input_with_optional_snapshots(
                    handle,
                    input,
                    config.max_content_length,
                    connection,
                    snapshots,
                )
                .await?;
                *connection = result.connection;
                framed.get_mut().write_all(&result.output).await?;
                framed.get_mut().flush().await?;
                served = served.saturating_add(1);
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Ok(served);
                }
            }
        }
    }
}

/// Submits a best-effort client disconnect event when a control connection EOFs.
///
/// The async control socket owns the live connection state, so it is the only
/// layer that can reliably convert a foreground attach fd hangup into the
/// runtime event that clears stale attached-primary session state. Request-local
/// control clients do not opt into this behavior because their EOF is just the
/// end of one RPC exchange.
async fn submit_control_connection_disconnect_event(
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
) -> Result<()> {
    let Some(client_id) = connection.take_disconnect_client_id() else {
        return Ok(());
    };
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::Client(ClientEvent::Disconnected {
        client_id,
        reason: "control socket EOF".to_string(),
    }));
    handle.submit_runtime_events(batch).await?;
    Ok(())
}

/// Runs the serve async runtime control listener operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub async fn serve_async_runtime_control_listener<F>(
    listener: &UnixListener,
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeControlConnectionConfig,
    should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    serve_async_runtime_control_listener_with_snapshots(listener, handle, config, None, should_stop)
        .await
}

/// Runs the serve async runtime control listener with snapshots operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_control_listener_with_snapshots<F>(
    listener: &UnixListener,
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<SnapshotRepository>,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    let mut accepted = 0u64;
    let mut tasks = JoinSet::new();
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(accepted, state) {
            break;
        }

        let (mut stream, _addr) = tokio::select! {
            accepted = listener.accept() => accepted?,
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
                        "async control connection task failed: {error}"
                    ))
                })??;
                continue;
            }
        };
        let peer_uid = authenticated_unix_peer_uid(stream.as_raw_fd(), config.owner_uid)?;
        let peer = AuthenticatedPeer::unix_user(peer_uid);
        let connection_handle = handle.clone();
        let connection_snapshots = snapshots.clone();
        tasks.spawn(async move {
            let mut connection = ControlConnectionState::new(true, true);
            serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
                &mut stream,
                peer,
                &connection_handle,
                &mut connection,
                config,
                connection_snapshots.as_ref(),
                |_, state| {
                    matches!(
                        state,
                        RuntimeLifecycleState::Stopping
                            | RuntimeLifecycleState::Killed
                            | RuntimeLifecycleState::Failed
                    )
                },
            )
            .await
        });
        accepted = accepted.saturating_add(1);
    }

    let terminal_drain = matches!(
        *lifecycle.borrow(),
        RuntimeLifecycleState::Stopping
            | RuntimeLifecycleState::Killed
            | RuntimeLifecycleState::Failed
    );
    if terminal_drain {
        if tokio::time::timeout(
            TERMINAL_CONTROL_CONNECTION_DRAIN_TIMEOUT,
            drain_control_connection_tasks(&mut tasks),
        )
        .await
        .is_err()
        {
            tasks.abort_all();
            while let Some(joined) = tasks.join_next().await {
                if let Err(error) = joined
                    && !error.is_cancelled()
                {
                    return Err(MezError::invalid_state(format!(
                        "async control connection task failed: {error}"
                    )));
                }
            }
        }
    } else {
        drain_control_connection_tasks(&mut tasks).await?;
    }

    Ok(accepted)
}

/// Joins all accepted control connections and propagates task or service
/// failures after their response loops finish.
async fn drain_control_connection_tasks(tasks: &mut JoinSet<Result<u64>>) -> Result<()> {
    while let Some(joined) = tasks.join_next().await {
        joined.map_err(|error| {
            MezError::invalid_state(format!("async control connection task failed: {error}"))
        })??;
    }
    Ok(())
}

/// Runs the handle control input with optional snapshots operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn handle_control_input_with_optional_snapshots(
    handle: &AsyncRuntimeSessionHandle,
    input: Vec<u8>,
    max_content_length: usize,
    connection: &ControlConnectionState,
    snapshots: Option<&SnapshotRepository>,
) -> Result<AsyncControlInputResult> {
    match snapshots {
        Some(snapshots) => {
            handle
                .handle_control_input_for_connection_with_snapshots(
                    input,
                    max_content_length,
                    connection.clone(),
                    snapshots.clone(),
                )
                .await
        }
        None => {
            handle
                .handle_control_input_for_connection(input, max_content_length, connection.clone())
                .await
        }
    }
}
