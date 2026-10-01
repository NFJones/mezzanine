//! Connection-local control serving, authority validation, and bounded teardown.
//!
//! The runtime actor remains the shared state owner. Transport, event worker,
//! sampler, and bridge lifetimes stay bound to this exact connection; final
//! response FIN acknowledgement precedes close and failures remain local.

use super::*;

#[allow(
    clippy::too_many_arguments,
    reason = "connection ownership, diagnostics, runtime state, framing, snapshots, compression, and timeouts are independent adapter inputs"
)]
pub(super) async fn serve_runtime_iroh_control_connection(
    connection: iroh::endpoint::Connection,
    connection_guard: RuntimeIrohConnectionGuard,
    handle: &AsyncRuntimeSessionHandle,
    control_config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<&SnapshotRepository>,
    authority: Option<RuntimeIrohAuthority>,
    compression: IrohCompressionPolicy,
    setup_timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
) -> Result<u64> {
    let endpoint_id = connection.remote_id().to_string();
    let _transport_owner = IrohConnectionTransportOwner(connection.clone());
    let (send, recv) = tokio::time::timeout(setup_timeout, connection.accept_bi())
        .await
        .map_err(|_| MezError::invalid_state("Iroh control stream setup timed out"))?
        .map_err(|error| {
            MezError::invalid_state(format!("failed to accept Iroh control stream: {error}"))
        })?;
    let compression_metrics = IrohCompressionMetrics::new(compression.codec());
    let mut bridge = IrohCompressionBridge::spawn_with_metrics(
        recv,
        send,
        compression,
        compression_metrics.clone(),
        control_config.max_content_length,
    )?;
    let mut connection_state = ControlConnectionState::new(false, false);
    connection_state.bind_x11_connection_id(format!("iroh-{}", connection.stable_id()))?;
    let (event_start_tx, event_start_rx) =
        tokio::sync::oneshot::channel::<(ClientId, u32, bool, bool)>();
    let mut event_start_tx = Some(event_start_tx);
    let (event_stop_tx, event_stop_rx) = tokio::sync::watch::channel(false);
    let event_connection = connection.clone();
    let event_handle = handle.clone();
    let event_compression_metrics = compression_metrics.clone();
    let event_task = tokio::spawn(async move {
        let Ok((client_id, version, client_clipboard_write, push_render)) = event_start_rx.await
        else {
            return Ok(0);
        };
        serve_runtime_iroh_event_stream(
            event_connection,
            event_handle,
            client_id,
            version,
            client_clipboard_write,
            push_render,
            compression,
            event_compression_metrics,
            setup_timeout,
            idle_timeout,
            event_stop_rx,
        )
        .await
    });
    let mut event_task = crate::runtime::IrohEventTask::new(Some(event_task));
    let sampler = Arc::new(Mutex::new(
        connection_guard.sampler(compression_metrics.clone()),
    ));
    let periodic_sampler = sampler.clone();
    let periodic_connection = connection.clone();
    let mut sample_task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Ok(mut sampler) = periodic_sampler.lock() {
                sampler.sample_current(&periodic_connection);
            }
        }
    }));
    let sample_connection = connection.clone();
    let response_sampler = sampler.clone();
    let request_authority = authority.clone();
    let cancellation_authority = authority;
    let (principal_tx, mut principal_rx) =
        tokio::sync::watch::channel::<Option<RemotePrincipal>>(None);
    let authority_cancelled = async move {
        let Some(authority) = cancellation_authority else {
            std::future::pending::<()>().await;
            return;
        };
        let mut trust_changes = authority.trust.authority_changes();
        loop {
            let principal = principal_rx.borrow().clone();
            if principal.as_ref().is_some_and(|principal| {
                authority
                    .trust
                    .validate_bound_principal(&authority.server_endpoint_id, principal)
                    .is_err()
            }) {
                return;
            }
            tokio::select! {
                changed = trust_changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                changed = principal_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    };
    let control =
        serve_authenticated_async_runtime_control_connection_loop_with_snapshots_hooks_and_cancellation(
            bridge.stream_mut(),
            AuthenticatedPeer::iroh_endpoint(endpoint_id),
            handle,
            &mut connection_state,
            control_config,
            snapshots,
            |_, state| terminal_daemon_state(state),
            move |connection_state| {
                let Some(authority) = request_authority.as_ref() else {
                    return Ok(());
                };
                let Some(principal) = connection_state.remote_principal() else {
                    return Ok(());
                };
                authority
                    .trust
                    .validate_bound_principal(&authority.server_endpoint_id, principal)
            },
            move |connection_state| {
                principal_tx.send_replace(connection_state.remote_principal().cloned());
                if let Some(client_id) = connection_state.caller_client_id()
                    && let Ok(mut sampler) = response_sampler.lock()
                {
                    sampler.sample(&sample_connection, client_id);
                }
                if let Some(start) = connection_state.take_event_stream_start()
                    && let Some(sender) = event_start_tx.take()
                {
                    let _ = sender.send(start);
                }
                if let Some(route) = connection_state.take_x11_route_start() {
                    route.activate(
                        sample_connection.clone(),
                        compression,
                        compression_metrics.clone(),
                    )?;
                }
                Ok(())
            },
            authority_cancelled,
        );
    // Only transport-local state is unwound. The actor owns shared runtime
    // mutation; its failure remains a separate infrastructure result.
    let (result, event_completed) =
        match std::panic::AssertUnwindSafe(event_task.supervise(control))
            .catch_unwind()
            .await
        {
            Ok(result) => result,
            Err(_) => (
                Err(MezError::invalid_state("Iroh control connection panicked")),
                true,
            ),
        };
    let disconnect_result = if event_completed {
        tokio::time::timeout(
            setup_timeout,
            crate::host::async_runtime::submit_control_connection_disconnect_event(
                handle,
                &mut connection_state,
            ),
        )
        .await
        .map_err(|_| MezError::invalid_state("Iroh event failure disconnect timed out"))
        .and_then(|result| result)
    } else {
        Ok(())
    };
    let x11_route_result = connection_state.deactivate_x11_route();
    sample_task.abort();
    let _ = (&mut sample_task).await;
    let _ = event_stop_tx.send(true);
    let shutdown_deadline = tokio::time::Instant::now() + setup_timeout;
    let bridge_finish_result = bridge.finish_outbound_until(shutdown_deadline).await;
    // Closing the connection while the outbound FIN is still unacknowledged
    // discards that FIN, leaving a peer that drains the framed response stream
    // to observe a connection error where the stream ends. Order the close
    // after the peer's acknowledgement, bounded by the same shutdown deadline.
    let bridge_outbound_result = bridge.settle_outbound_until(shutdown_deadline).await;
    connection.close(
        VarInt::from_u32(u32::from(result.is_err())),
        if result.is_ok() {
            b"control complete"
        } else {
            b"control failed"
        },
    );
    let event_result = event_task.settle_until(shutdown_deadline).await;
    let bridge_result = bridge.settle_until(shutdown_deadline).await;
    let served = crate::runtime::iroh_event_task::merge_event_result(result, event_result)?;
    disconnect_result?;
    x11_route_result?;
    bridge_finish_result?;
    bridge_outbound_result?;
    bridge_result?;
    Ok(served)
}
