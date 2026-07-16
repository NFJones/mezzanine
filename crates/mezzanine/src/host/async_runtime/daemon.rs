//! Async Runtime Daemon implementation.
//!
//! This module owns the async runtime daemon boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::provider::build_async_agent_provider_service;
use super::{
    AsyncRuntimeDaemonConfig, AsyncRuntimeDaemonListeners, AsyncRuntimeMessageConnectionConfig,
    AsyncRuntimeService, AsyncRuntimeServiceExit, AsyncRuntimeSessionHandle, MezError, Result,
    RuntimeLifecycleState, build_async_hook_side_effect_service,
    build_async_pane_process_supervisor_service, build_async_persistence_side_effect_service,
    build_async_runtime_timer_side_effect_service,
    serve_async_runtime_control_listener_with_snapshots, serve_async_runtime_event_listener,
    serve_async_runtime_message_listener_concurrent,
};
#[cfg(test)]
use super::{AsyncRuntimeSupervisionReport, Future, supervise_async_runtime_services};

// Daemon service construction and socket listener orchestration.

/// Runs the build async runtime daemon services operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn build_async_runtime_daemon_services(
    handle: AsyncRuntimeSessionHandle,
    listeners: AsyncRuntimeDaemonListeners,
    config: AsyncRuntimeDaemonConfig,
) -> Result<Vec<AsyncRuntimeService>> {
    config.validate()?;
    let mut services = Vec::new();

    if let Some(listener) = listeners.control {
        let handle = handle.clone();
        let control_config = config.control;
        let snapshots = config.snapshots.clone();
        let max_connections = config.max_control_connections;
        services.push(AsyncRuntimeService::new("control", async move {
            let served = serve_async_runtime_control_listener_with_snapshots(
                &listener,
                &handle,
                control_config,
                snapshots,
                |served, state| served >= max_connections || is_terminal_daemon_state(state),
            )
            .await?;
            Ok(AsyncRuntimeServiceExit::completed(served))
        }));
    }

    if let Some(listener) = listeners.message {
        let handle = handle.clone();
        let message_config = AsyncRuntimeMessageConnectionConfig::new(
            config.message_max_content_length,
            config.message_fanout_limit,
        )?;
        let base_now_ms = config.message_base_now_ms;
        let max_connections = config.max_message_connections;
        services.push(AsyncRuntimeService::new("message", async move {
            let served = serve_async_runtime_message_listener_concurrent(
                &listener,
                &handle,
                message_config,
                base_now_ms,
                max_connections,
                |_, state| is_terminal_daemon_state(state),
            )
            .await?;
            Ok(AsyncRuntimeServiceExit::completed(served))
        }));
    }

    if let Some(listener) = listeners.event {
        let handle = handle.clone();
        let event_config = config.event;
        let max_connections = config.max_event_connections;
        let max_batches = config.max_event_batches_per_connection;
        let audience = config.event_audience.clone();
        services.push(AsyncRuntimeService::new("event", async move {
            let served = serve_async_runtime_event_listener(
                &listener,
                &handle,
                event_config,
                move |index| Ok((format!("event-{index}"), audience.clone(), 0)),
                |served, delivered, state| {
                    served >= max_connections
                        || delivered >= max_batches
                        || is_terminal_daemon_state(state)
                },
            )
            .await?;
            Ok(AsyncRuntimeServiceExit::completed(served))
        }));
    }

    if services.is_empty() {
        return Err(MezError::invalid_args(
            "async daemon requires at least one listener",
        ));
    }

    services.push(build_async_runtime_timer_side_effect_service(
        "timer",
        handle.clone(),
        Default::default(),
        config.timer_base_now_ms,
    )?);
    services.push(build_async_pane_process_supervisor_service(
        "pane-process-supervisor",
        handle.clone(),
        Default::default(),
    )?);
    services.push(build_async_persistence_side_effect_service(
        "persistence",
        handle.clone(),
        Default::default(),
    )?);
    services.push(build_async_hook_side_effect_service(
        "hook",
        handle.clone(),
        Default::default(),
    )?);
    services.push(build_async_agent_provider_service(
        "agent-provider",
        handle,
        Default::default(),
    )?);
    Ok(services)
}

/// Runs the is terminal daemon state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_terminal_daemon_state(state: RuntimeLifecycleState) -> bool {
    matches!(
        state,
        RuntimeLifecycleState::Stopping
            | RuntimeLifecycleState::Killed
            | RuntimeLifecycleState::Failed
    )
}

/// Runs the run async runtime daemon operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub async fn run_async_runtime_daemon<C>(
    handle: AsyncRuntimeSessionHandle,
    listeners: AsyncRuntimeDaemonListeners,
    config: AsyncRuntimeDaemonConfig,
    cancellation: C,
) -> Result<AsyncRuntimeSupervisionReport>
where
    C: Future<Output = ()>,
{
    let services = build_async_runtime_daemon_services(handle, listeners, config)?;
    supervise_async_runtime_services(services, cancellation).await
}
