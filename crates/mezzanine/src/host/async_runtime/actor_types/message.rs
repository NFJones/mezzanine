//! Async MMP connection, fanout, and listener adapters over the runtime actor.
//!
//! Decoder/content-type rejection and peer socket I/O terminate only the
//! offending connection. Actor ingress, audit, shared fanout/ack state, listener
//! accept and task JoinErrors remain critical. Continuous reaping and final
//! drain share typed outcome handling: no error-kind suppression, replay or
//! stream resynchronization. Raw hello identities own their subscriptions and
//! are released before connection capacity is reused.

use super::{
    AsyncRuntimeMessageConnectionConfig, AsyncRuntimeSessionHandle, AsyncWriteExt, Framed, JoinSet,
    MessageConnection, MezError, ProtocolFrameCodec, Result, RuntimeLifecycleState, StreamExt,
    UnixListener, UnixStream, encode_frame,
};

/// Socket-local raw hello ownership published before the actor reply.
struct RawMessageConnectionLease {
    /// Actor used to retire only this fresh observational identity.
    handle: AsyncRuntimeSessionHandle,
    /// Latest identity, including replies not yet consumed by the socket task.
    identity: tokio::sync::watch::Receiver<Option<mez_core::ids::AgentId>>,
    /// Whether awaited cleanup completed successfully.
    retired: bool,
}

impl RawMessageConnectionLease {
    /// Normal exit awaits cleanup; actor ingress failures remain critical.
    async fn retire(&mut self) -> Result<()> {
        let identity = self.identity.borrow().clone();
        let result = if let Some(agent_id) = identity {
            self.handle.retire_message_connection(agent_id).await
        } else {
            Ok(())
        };
        self.retired = true;
        result
    }
}

impl Drop for RawMessageConnectionLease {
    /// Cancellation hands exact cleanup to the runtime. Fresh hello ids cannot
    /// be rebound, so duplicate retirement cannot delete a replacement lease.
    fn drop(&mut self) {
        if self.retired {
            return;
        }
        let Some(agent_id) = self.identity.borrow().clone() else {
            return;
        };
        let handle = self.handle.clone();
        tokio::spawn(async move {
            if handle.retire_message_connection(agent_id).await.is_err() {
                eprintln!("mez: cancelled MMP connection cleanup actor unavailable");
            }
        });
    }
}

/// Error provenance assigned at operation boundaries, never by MezError kind.
enum MessageConnectionFailure {
    /// Untrusted framing or socket operation; reason is a fixed private label.
    Peer(&'static str, Box<MezError>),
    /// Actor ingress, shared state, or local configuration failure.
    Critical(MezError),
}

impl From<MezError> for MessageConnectionFailure {
    /// Unmarked errors remain critical by default.
    fn from(error: MezError) -> Self {
        Self::Critical(error)
    }
}

impl MessageConnectionFailure {
    /// Tags a peer-boundary error without growing the common Result layout.
    fn peer(reason: &'static str, error: MezError) -> Self {
        Self::Peer(reason, Box::new(error))
    }

    /// Preserves detailed errors for direct test adapters, not diagnostics.
    #[cfg(test)]
    fn into_error(self) -> MezError {
        match self {
            Self::Peer(_, error) => *error,
            Self::Critical(error) => error,
        }
    }
}

/// Reaps one outcome with bounded, payload-free failure diagnostics. JoinErrors
/// retain the existing fatal policy. Counts saturate; only the first and
/// power-of-two failures are logged, never peer identity, body or error text.
fn reap_message_connection(
    joined: std::result::Result<
        std::result::Result<u64, MessageConnectionFailure>,
        tokio::task::JoinError,
    >,
    peer_failures: &mut u64,
) -> Result<()> {
    match joined.map_err(|error| {
        MezError::invalid_state(format!("async message connection task failed: {error}"))
    })? {
        Ok(_) => Ok(()),
        Err(MessageConnectionFailure::Critical(error)) => Err(error),
        Err(MessageConnectionFailure::Peer(reason, _error)) => {
            *peer_failures = peer_failures.saturating_add(1);
            if peer_failures.is_power_of_two() {
                eprintln!("mez: MMP peer closed ({reason}); peer failures={peer_failures}");
            }
            Ok(())
        }
    }
}

/// Serves one frame for direct fixture tests, preserving detailed errors and
/// fixture-owned identities. The production listener uses the stateful loop.
#[cfg(test)]
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
    flush_async_message_fanout_for_connection(
        &mut framed,
        handle,
        connection,
        now_ms,
        fanout_limit,
    )
    .await
    .map_err(MessageConnectionFailure::into_error)?;
    framed.get_mut().flush().await?;
    Ok(result.consumed)
}

/// Writes a ready delivery once and acknowledges only after a successful write.
/// Actor selection/ack failures remain critical; a failed peer write never
/// advances the cursor and is never replayed here.
async fn flush_async_message_fanout_for_connection(
    framed: &mut Framed<&mut UnixStream, ProtocolFrameCodec>,
    handle: &AsyncRuntimeSessionHandle,
    connection: &MessageConnection,
    now_ms: u64,
    fanout_limit: usize,
) -> std::result::Result<usize, MessageConnectionFailure> {
    let Some(agent_id) = connection.agent_id.clone() else {
        return Ok(0);
    };
    let Some(fanout) = handle
        .message_fanout_ready_for(agent_id, now_ms, fanout_limit)
        .await?
    else {
        return Ok(0);
    };
    framed
        .get_mut()
        .write_all(&fanout.frame)
        .await
        .map_err(|error| MessageConnectionFailure::peer("write", error.into()))?;
    handle.acknowledge_message_fanout(fanout.batch).await?;
    Ok(fanout.messages)
}

/// Validates content type before actor ingress. The codec already checks UTF-8
/// and bounds frame size. No framing error crosses the actor boundary.
fn validated_message_frame(
    frame: crate::protocol::framing::ProtocolFrame,
    max_content_length: usize,
) -> std::result::Result<Vec<u8>, MessageConnectionFailure> {
    let input = encode_frame(&frame);
    crate::protocol::message::decode_mmp_frame(&input, max_content_length)
        .map_err(|error| MessageConnectionFailure::peer("framing", error))?;
    Ok(input)
}

/// Serves stateful input and idle delivery until EOF or lifecycle stop. Peer
/// errors are tagged where they occur; all actor operations are critical.
async fn run_message_connection_loop<F>(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection_owner: (
        &mut MessageConnection,
        Option<&tokio::sync::watch::Sender<Option<mez_core::ids::AgentId>>>,
    ),
    config: AsyncRuntimeMessageConnectionConfig,
    mut now_ms: impl FnMut(u64) -> u64,
    mut should_stop: F,
) -> std::result::Result<u64, MessageConnectionFailure>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    let (connection, owner) = connection_owner;
    let mut served = 0u64;
    let mut framed = Framed::new(stream, ProtocolFrameCodec::new(config.max_content_length)?);
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(served, state) {
            return Ok(served);
        }
        if connection.agent_id.is_some()
            && flush_async_message_fanout_for_connection(
                &mut framed,
                handle,
                connection,
                now_ms(served),
                config.fanout_limit,
            )
            .await?
                > 0
        {
            framed
                .get_mut()
                .flush()
                .await
                .map_err(|error| MessageConnectionFailure::peer("flush", error.into()))?;
        }
        tokio::select! {
            frame = framed.next() => {
                let Some(frame) = frame else {
                    return Ok(served);
                };
                let frame = frame.map_err(|error| MessageConnectionFailure::peer("framing/read", error))?;
                let input = validated_message_frame(frame, config.max_content_length)?;
                let now = now_ms(served);
                let result = handle
                    .handle_message_input_with_owner(input, config.max_content_length, connection.clone(), now, owner.cloned())
                    .await?;
                *connection = result.connection;
                framed.get_mut().write_all(&result.output).await
                    .map_err(|error| MessageConnectionFailure::peer("write", error.into()))?;
                flush_async_message_fanout_for_connection(
                    &mut framed, handle, connection, now, config.fanout_limit,
                ).await?;
                framed.get_mut().flush().await
                    .map_err(|error| MessageConnectionFailure::peer("flush", error.into()))?;
                served = served.saturating_add(1);
            }
            _ = handle.wait_for_message_delivery(), if connection.agent_id.is_some() => {
                if flush_async_message_fanout_for_connection(
                    &mut framed, handle, connection, now_ms(served), config.fanout_limit,
                ).await? > 0 {
                    framed.get_mut().flush().await
                        .map_err(|error| MessageConnectionFailure::peer("flush", error.into()))?;
                }
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Err(MezError::invalid_state("async message actor lifecycle unavailable").into());
                }
            }
        }
    }
}

/// Direct fixture adapter retaining detailed connection errors and existing
/// fixture registrations. Production listener cleanup is separate.
#[cfg(test)]
pub async fn serve_async_runtime_message_connection_loop<F>(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut MessageConnection,
    config: AsyncRuntimeMessageConnectionConfig,
    now_ms: impl FnMut(u64) -> u64,
    should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    run_message_connection_loop(
        stream,
        handle,
        (connection, None),
        config,
        now_ms,
        should_stop,
    )
    .await
    .map_err(MessageConnectionFailure::into_error)
}

/// Sequential fixture listener; production uses bounded concurrent handling.
#[cfg(test)]
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

/// Runs the bounded concurrent listener with continuous reaping and deterministic
/// final drain. Ordinary peer failures free capacity after identity/subscription
/// cleanup. Actor/cleanup/accept errors and JoinErrors still fail the listener.
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
    let mut peer_failures = 0u64;
    let mut tasks = JoinSet::new();
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(accepted, state) {
            break;
        }
        let (mut stream, _addr) = tokio::select! {
            accepted = listener.accept(), if (tasks.len() as u64) < max_connections => accepted?,
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Err(MezError::invalid_state("async message actor lifecycle unavailable"));
                }
                continue;
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                let Some(joined) = joined else {
                    continue;
                };
                reap_message_connection(joined, &mut peer_failures)?;
                continue;
            }
        };
        let connection_handle = handle.clone();
        let connection_now_base = base_now_ms.saturating_add(accepted);
        tasks.spawn(async move {
            let mut connection = MessageConnection::default();
            let (owner, identity) = tokio::sync::watch::channel(None);
            let mut lease = RawMessageConnectionLease {
                handle: connection_handle.clone(),
                identity,
                retired: false,
            };
            let result = run_message_connection_loop(
                &mut stream,
                &connection_handle,
                (&mut connection, Some(&owner)),
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
            .await;
            drop(stream);
            // Cleanup ingress failure cannot be hidden by a peer error.
            lease.retire().await?;
            result
        });
        accepted = accepted.saturating_add(1);
    }
    while let Some(joined) = tasks.join_next().await {
        reap_message_connection(joined, &mut peer_failures)?;
    }
    Ok(accepted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same Io/InvalidState/InvalidArgs errors can arise at different
    /// boundaries. Only the typed peer origin is contained, including in final
    /// drain; critical errors retain their detail and never increment peer counts.
    #[tokio::test]
    async fn message_reaping_preserves_typed_origin_not_error_kind() {
        let mut failures = 0;
        for error in [
            MezError::invalid_state("shared corruption"),
            MezError::invalid_args("actor admission failed"),
            std::io::Error::other("audit unavailable").into(),
        ] {
            let expected = error.message().to_string();
            let mut tasks = JoinSet::new();
            tasks.spawn(async move { Err(MessageConnectionFailure::Critical(error)) });
            let error = reap_message_connection(tasks.join_next().await.unwrap(), &mut failures)
                .unwrap_err();
            assert_eq!(error.message(), expected);
            assert_eq!(failures, 0);
        }
        let mut tasks = JoinSet::new();
        tasks.spawn(async {
            Err(MessageConnectionFailure::peer(
                "read",
                MezError::invalid_state("untrusted private text"),
            ))
        });
        reap_message_connection(tasks.join_next().await.unwrap(), &mut failures).unwrap();
        assert_eq!(failures, 1);
        failures = u64::MAX;
        reap_message_connection(
            Ok(Err(MessageConnectionFailure::peer(
                "write",
                std::io::Error::other("peer text").into(),
            ))),
            &mut failures,
        )
        .unwrap();
        assert_eq!(failures, u64::MAX);
    }

    /// Task panics remain JoinErrors under the existing listener policy. This
    /// ordinary Result isolation change must not introduce a panic containment
    /// policy or disguise a failed connection task as an ordinary peer closure.
    #[tokio::test]
    async fn message_reaping_keeps_join_errors_critical() {
        let mut tasks = JoinSet::new();
        tasks.spawn(async {
            panic!("intentional message task policy control");
            #[allow(unreachable_code)]
            Ok(0)
        });
        let mut failures = 0;
        let error =
            reap_message_connection(tasks.join_next().await.unwrap(), &mut failures).unwrap_err();
        assert!(error.message().contains("connection task failed"));
        assert_eq!(failures, 0);
    }
}
