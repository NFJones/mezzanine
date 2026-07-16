//! Async control-connection and listener adapters over the runtime actor handle.

use super::*;

/// Runs the serve async runtime control connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
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
pub async fn serve_async_runtime_control_connection_with_snapshots(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
    config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<&SnapshotRepository>,
) -> Result<usize> {
    authorize_unix_peer_raw_fd(stream.as_raw_fd(), config.owner_uid)?;
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
pub async fn serve_async_runtime_control_connection_loop_with_snapshots<F>(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
    config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<&SnapshotRepository>,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    authorize_unix_peer_raw_fd(stream.as_raw_fd(), config.owner_uid)?;
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
    connection: &ControlConnectionState,
) -> Result<()> {
    if !connection.detach_primary_on_disconnect() {
        return Ok(());
    }
    let Some(client_id) = connection.caller_client_id().cloned() else {
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
        };
        let connection_handle = handle.clone();
        let connection_snapshots = snapshots.clone();
        tasks.spawn(async move {
            let mut connection = ControlConnectionState::new(true, true);
            serve_async_runtime_control_connection_loop_with_snapshots(
                &mut stream,
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

    while let Some(joined) = tasks.join_next().await {
        joined.map_err(|error| {
            MezError::invalid_state(format!("async control connection task failed: {error}"))
        })??;
    }

    Ok(accepted)
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
