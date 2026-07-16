//! Async MMP connection, fanout, and listener adapters over the runtime actor handle.

use super::{
    AsyncRuntimeMessageConnectionConfig, AsyncRuntimeSessionHandle, AsyncWriteExt, Framed, JoinSet,
    MessageConnection, MezError, ProtocolFrameCodec, Result, RuntimeLifecycleState, StreamExt,
    UnixListener, UnixStream, encode_frame,
};

/// Runs the serve async runtime message connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_message_connection(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut MessageConnection,
    max_content_length: usize,
    now_ms: u64,
    fanout_limit: usize,
) -> Result<usize> {
    if max_content_length == 0 {
        return Err(MezError::invalid_args(
            "async message max content length must be greater than zero",
        ));
    }

    let mut framed = Framed::new(stream, ProtocolFrameCodec::new(max_content_length)?);
    let Some(frame) = framed.next().await else {
        return Ok(0);
    };
    let input = encode_frame(&frame?);
    let result = handle
        .handle_message_input(input, max_content_length, connection.clone(), now_ms)
        .await?;
    *connection = result.connection;
    framed.get_mut().write_all(&result.output).await?;

    if let Some(agent_id) = connection.agent_id.clone()
        && let Some(fanout) = handle
            .message_fanout_ready_for(agent_id, now_ms, fanout_limit)
            .await?
    {
        framed.get_mut().write_all(&fanout.frame).await?;
        handle.acknowledge_message_fanout(fanout.batch).await?;
    }

    framed.get_mut().flush().await?;
    Ok(result.consumed)
}

/// Runs the flush async message fanout for connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn flush_async_message_fanout_for_connection(
    framed: &mut Framed<&mut UnixStream, ProtocolFrameCodec>,
    handle: &AsyncRuntimeSessionHandle,
    connection: &MessageConnection,
    now_ms: u64,
    fanout_limit: usize,
) -> Result<usize> {
    let Some(agent_id) = connection.agent_id.clone() else {
        return Ok(0);
    };
    let Some(fanout) = handle
        .message_fanout_ready_for(agent_id, now_ms, fanout_limit)
        .await?
    else {
        return Ok(0);
    };

    framed.get_mut().write_all(&fanout.frame).await?;
    handle.acknowledge_message_fanout(fanout.batch).await?;
    Ok(fanout.messages)
}

/// Runs the serve async runtime message connection loop operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_message_connection_loop<F>(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut MessageConnection,
    config: AsyncRuntimeMessageConnectionConfig,
    mut now_ms: impl FnMut(u64) -> u64,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    let mut served = 0u64;
    let mut framed = Framed::new(stream, ProtocolFrameCodec::new(config.max_content_length)?);
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(served, state) {
            return Ok(served);
        }

        if connection.agent_id.is_some() {
            let now = now_ms(served);
            if flush_async_message_fanout_for_connection(
                &mut framed,
                handle,
                connection,
                now,
                config.fanout_limit,
            )
            .await?
                > 0
            {
                framed.get_mut().flush().await?;
            }
            tokio::select! {
                frame = framed.next() => {
                    let Some(frame) = frame else {
                        return Ok(served);
                    };
                    let input = encode_frame(&frame?);
                    let now = now_ms(served);
                    let result = handle
                        .handle_message_input(input, config.max_content_length, connection.clone(), now)
                        .await?;
                    *connection = result.connection;
                    framed.get_mut().write_all(&result.output).await?;
                    let _ = flush_async_message_fanout_for_connection(
                        &mut framed,
                        handle,
                        connection,
                        now,
                        config.fanout_limit,
                    )
                        .await?;
                    framed.get_mut().flush().await?;
                    served = served.saturating_add(1);
                }
                _ = handle.wait_for_message_delivery() => {
                    let now = now_ms(served);
                    if flush_async_message_fanout_for_connection(
                        &mut framed,
                        handle,
                        connection,
                        now,
                        config.fanout_limit,
                    )
                    .await? > 0
                    {
                        framed.get_mut().flush().await?;
                    }
                }
                changed = lifecycle.changed() => {
                    if changed.is_err() {
                        return Ok(served);
                    }
                }
            }
        } else {
            tokio::select! {
                frame = framed.next() => {
                    let Some(frame) = frame else {
                        return Ok(served);
                    };
                    let input = encode_frame(&frame?);
                    let now = now_ms(served);
                    let result = handle
                        .handle_message_input(input, config.max_content_length, connection.clone(), now)
                        .await?;
                    *connection = result.connection;
                    framed.get_mut().write_all(&result.output).await?;
                    let _ = flush_async_message_fanout_for_connection(
                        &mut framed,
                        handle,
                        connection,
                        now,
                        config.fanout_limit,
                    )
                    .await?;
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
}

/// Runs the serve async runtime message listener operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_message_listener<F>(
    listener: &UnixListener,
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeMessageConnectionConfig,
    base_now_ms: u64,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    let mut served = 0u64;
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(served, state) {
            return Ok(served);
        }

        let (mut stream, _addr) = tokio::select! {
            accepted = listener.accept() => accepted?,
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Ok(served);
                }
                continue;
            }
        };
        let mut connection = MessageConnection::default();
        serve_async_runtime_message_connection_loop(
            &mut stream,
            handle,
            &mut connection,
            config,
            |request_index| {
                base_now_ms
                    .saturating_add(served)
                    .saturating_add(request_index)
            },
            |_, state| should_stop(served, state),
        )
        .await?;
        served = served.saturating_add(1);
    }
}

/// Runs the serve async runtime message listener concurrent operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_message_listener_concurrent<F>(
    listener: &UnixListener,
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeMessageConnectionConfig,
    base_now_ms: u64,
    max_connections: u64,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    if max_connections == 0 {
        return Err(MezError::invalid_args(
            "async message listener max connections must be greater than zero",
        ));
    }

    let mut accepted = 0u64;
    let mut tasks = JoinSet::new();
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if accepted >= max_connections || should_stop(accepted, state) {
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
        let connection_now_base = base_now_ms.saturating_add(accepted);
        tasks.spawn(async move {
            let mut connection = MessageConnection::default();
            serve_async_runtime_message_connection_loop(
                &mut stream,
                &connection_handle,
                &mut connection,
                config,
                |request_index| connection_now_base.saturating_add(request_index),
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
            MezError::invalid_state(format!("async message connection task failed: {error}"))
        })??;
    }

    Ok(accepted)
}
