//! Actor construction, run-loop ownership, and actor metrics.

use super::{
    Arc, AsyncRuntimeActorConfig, AsyncRuntimeActorExit, AsyncRuntimeRequestEnvelope,
    AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle, MezError, Notify, Result,
    RuntimeSessionService, RuntimeSnapshotControlAsyncOutcome, RuntimeSnapshotControlAsyncWork,
    RuntimeSnapshotControlAsyncWorkKind, decode_control_frame, mpsc, watch,
};
/// Maximum interactive requests served before pending normal work must advance.
pub(super) const MAX_INTERACTIVE_REQUEST_BURST: u8 = 4;
/// Maximum elapsed actor time spent on one interactive or normal burst before
/// pending downstream work must advance.
pub(super) const MAX_ACTOR_REQUEST_BURST_DURATION: std::time::Duration =
    std::time::Duration::from_millis(16);
/// Maximum request services between cancellation-safe clipboard route cleanups.
const MAX_REQUESTS_BEFORE_CLIPBOARD_CLEANUP: u8 = 4;

/// Returns whether a burst has consumed its bounded actor-time allowance.
pub(super) fn actor_request_burst_duration_exhausted(elapsed: std::time::Duration) -> bool {
    elapsed >= MAX_ACTOR_REQUEST_BURST_DURATION
}

/// Reserves admission capacity for every fair-scheduling lane.
///
/// The reservations sum to the configured mailbox bound. Reserving urgent and
/// interactive slots prevents a saturated normal producer from occupying every
/// admission slot before the actor can observe a later terminal action.
pub(super) const fn actor_request_lane_capacities(
    command_buffer: usize,
) -> Option<(usize, usize, usize, usize)> {
    if command_buffer < 4 {
        return None;
    }
    let urgent = 1;
    let interactive = if command_buffer.div_ceil(4) == 0 {
        1
    } else {
        command_buffer.div_ceil(4)
    };
    let maintenance = if command_buffer.div_ceil(8) == 0 {
        1
    } else {
        command_buffer.div_ceil(8)
    };
    let normal = command_buffer - urgent - interactive - maintenance;
    Some((urgent, interactive, normal, maintenance))
}

/// Returns the oldest queued request age without inspecting request contents.
pub(super) fn oldest_queued_request_wait_ms(
    urgent: Option<&AsyncRuntimeRequestEnvelope>,
    interactive: Option<&AsyncRuntimeRequestEnvelope>,
    normal: Option<&AsyncRuntimeRequestEnvelope>,
    maintenance: Option<&AsyncRuntimeRequestEnvelope>,
) -> u64 {
    [urgent, interactive, normal, maintenance]
        .into_iter()
        .flatten()
        .map(|request| u64::try_from(request.enqueued_at.elapsed().as_millis()).unwrap_or(u64::MAX))
        .max()
        .unwrap_or(0)
}

pub(super) use crate::host::async_runtime::AsyncRuntimeRequestLane as ActorRequestLane;

/// Chooses one lane from bounded queue state without inspecting request payloads.
#[allow(
    clippy::too_many_arguments,
    reason = "the selector keeps lane availability and both deterministic fairness bounds explicit"
)]
pub(super) const fn next_actor_request_lane(
    urgent: bool,
    interactive: bool,
    normal: bool,
    maintenance: bool,
    interactive_burst: u8,
    normal_burst: u8,
    interactive_time_exhausted: bool,
    normal_time_exhausted: bool,
) -> Option<ActorRequestLane> {
    if urgent {
        Some(ActorRequestLane::Urgent)
    } else if interactive
        && (!(normal || maintenance)
            || (interactive_burst < MAX_INTERACTIVE_REQUEST_BURST && !interactive_time_exhausted))
    {
        Some(ActorRequestLane::Interactive)
    } else if normal
        && (!maintenance
            || (normal_burst < MAX_INTERACTIVE_REQUEST_BURST && !normal_time_exhausted))
    {
        Some(ActorRequestLane::Normal)
    } else if maintenance {
        Some(ActorRequestLane::Maintenance)
    } else if normal {
        Some(ActorRequestLane::Normal)
    } else if interactive {
        Some(ActorRequestLane::Interactive)
    } else {
        None
    }
}

/// Runs the execute snapshot control async work operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn execute_snapshot_control_async_work(
    snapshots: &crate::storage::snapshot::SnapshotRepository,
    work: &RuntimeSnapshotControlAsyncWork,
) -> RuntimeSnapshotControlAsyncOutcome {
    match &work.kind {
        RuntimeSnapshotControlAsyncWorkKind::ConfigReload { layers, .. } => {
            #[cfg(test)]
            if let RuntimeSnapshotControlAsyncWorkKind::ConfigReload {
                preparation_started,
                preparation_release,
                ..
            } = &work.kind
            {
                if let Some(started) = preparation_started {
                    started.notify_one();
                }
                if let Some(release) = preparation_release {
                    release.notified().await;
                }
            }
            RuntimeSnapshotControlAsyncOutcome::ConfigReload(
                RuntimeSessionService::prepare_runtime_config_reload_async(
                    &work.request,
                    layers.clone(),
                )
                .await,
            )
        }
        RuntimeSnapshotControlAsyncWorkKind::Dispatch { session, context } => {
            RuntimeSnapshotControlAsyncOutcome::Dispatch(
                crate::control::dispatch_snapshot_request_with_context_async(
                    &work.request,
                    session,
                    snapshots,
                    context.as_creation_context(),
                )
                .await,
            )
        }
        RuntimeSnapshotControlAsyncWorkKind::Resume { shell } => {
            let snapshot_id: std::result::Result<String, MezError> = work
                .request
                .params
                .as_deref()
                .and_then(snapshot_id_from_json_params)
                .ok_or_else(|| MezError::invalid_args("snapshot/resume requires snapshot_id"));
            RuntimeSnapshotControlAsyncOutcome::Resume(Box::new(match snapshot_id {
                Ok(snapshot_id) => {
                    let payload = match snapshots.inspect_payload_async(&snapshot_id).await {
                        Ok(payload) => payload,
                        Err(error) => {
                            return RuntimeSnapshotControlAsyncOutcome::Resume(Box::new(Err(
                                error,
                            )));
                        }
                    };
                    snapshots
                        .restore_session_from_payload_async(&snapshot_id, &payload, shell.clone())
                        .await
                        .map(|restored| (payload, restored))
                }
                Err(error) => Err(error),
            }))
        }
    }
}

/// Runs the snapshot id from json params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn snapshot_id_from_json_params(params: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(params)
        .ok()
        .and_then(|value| value.get("snapshot_id")?.as_str().map(str::to_string))
}

impl AsyncRuntimeSessionActor {
    /// Terminates pane processes still owned by an actor that has not started.
    ///
    /// Reusable session construction uses this during rollback and when a
    /// ready runtime is dropped before its actor loop begins. Processes already
    /// handed to supervised async workers remain owned by those workers.
    pub(crate) fn terminate_owned_pane_processes(&mut self) -> Result<()> {
        self.service.terminate_all_pane_processes()?;
        Ok(())
    }

    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(
        mut service: RuntimeSessionService,
        config: AsyncRuntimeActorConfig,
    ) -> Result<(AsyncRuntimeSessionHandle, Self)> {
        let Some((urgent_capacity, interactive_capacity, normal_capacity, maintenance_capacity)) =
            actor_request_lane_capacities(config.command_buffer)
        else {
            return Err(MezError::invalid_args(
                "async runtime command buffer must reserve at least one slot per scheduling lane",
            ));
        };
        if config.side_effect_buffer == 0 {
            return Err(MezError::invalid_args(
                "async runtime side-effect buffer must be greater than zero",
            ));
        }

        // Lane-local queues make every accepted request visible to the fair
        // scheduler immediately. Their fixed reservations sum exactly to the
        // configured bound while preserving urgent and interactive admission.
        let request_ingress = Arc::new(std::sync::Mutex::new(Default::default()));
        let request_ingress_notify = Arc::new(Notify::new());
        let sender = crate::host::async_runtime::config::AsyncRuntimeRequestSender {
            ingress: request_ingress.clone(),
            ingress_notify: request_ingress_notify.clone(),
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            urgent_admission: Arc::new(tokio::sync::Semaphore::new(urgent_capacity)),
            interactive_admission: Arc::new(tokio::sync::Semaphore::new(interactive_capacity)),
            normal_admission: Arc::new(tokio::sync::Semaphore::new(normal_capacity)),
            maintenance_admission: Arc::new(tokio::sync::Semaphore::new(maintenance_capacity)),
        };
        let (client_clipboard_route_cleanup_tx, client_clipboard_route_cleanup_rx) =
            mpsc::unbounded_channel();
        let message_delivery_notify = Arc::new(Notify::new());
        let event_delivery_notify = Arc::new(Notify::new());
        let (event_delivery_revision_tx, event_delivery_revision_rx) = watch::channel(0u64);
        let side_effect_delivery_notify = Arc::new(Notify::new());
        let (side_effect_delivery_tx, side_effect_delivery_rx) = watch::channel(0u64);
        let (lifecycle_state_tx, lifecycle_state_rx) = watch::channel(service.lifecycle_state());
        let (terminal_config_generation_tx, terminal_config_generation_rx) = watch::channel(0u64);
        service.use_audit_effect_adapter();
        service.use_pane_pipe_effect_adapter();
        service.use_transcript_effect_adapter();
        service.use_token_usage_effect_adapter();
        service.use_provider_settlement_effect_adapter();
        service.use_registry_effect_adapter();
        service.use_config_effect_adapter();
        service.use_hook_effect_adapter();
        let now_ms = super::coalesce::async_runtime_current_unix_millis();
        let mut initial_side_effects = service
            .queue_saved_session_retention_operation(now_ms / 1_000, true)?
            .side_effects;
        initial_side_effects.extend(
            service
                .drain_transcript_persistence_transition()
                .side_effects,
        );
        let handle = AsyncRuntimeSessionHandle {
            sender: sender.clone(),
            client_clipboard_route_cleanup_tx,
            message_delivery_notify: message_delivery_notify.clone(),
            event_delivery_notify: event_delivery_notify.clone(),
            event_delivery_revision_rx,
            side_effect_delivery_notify: side_effect_delivery_notify.clone(),
            side_effect_delivery_rx,
            lifecycle_state_rx,
            terminal_config_generation_rx,
        };
        let mut actor = Self {
            service,
            sender: Box::new(sender.clone()),
            request_admission_guard:
                crate::host::async_runtime::config::AsyncRuntimeRequestAdmissionGuard::new(
                    sender.clone(),
                ),
            request_ingress,
            request_ingress_notify,
            request_scheduler: Box::new(
                crate::host::async_runtime::config::AsyncRuntimeRequestScheduler {
                    interactive_request_burst: 0,
                    interactive_request_burst_started_at: None,
                    normal_request_burst: 0,
                    normal_request_burst_started_at: None,
                    requests_since_clipboard_cleanup: 0,
                },
            ),
            message_delivery_notify,
            event_delivery_notify,
            event_delivery_revision_tx,
            client_clipboard_routes: Default::default(),
            client_clipboard_route_generations: Default::default(),
            next_client_clipboard_route_generation: 0,
            client_clipboard_sequences: Default::default(),
            client_clipboard_route_cleanup_rx,
            side_effect_delivery_notify,
            side_effect_delivery_tx,
            lifecycle_state_tx,
            terminal_config_generation: 0,
            terminal_config_generation_tx,
            side_effects: Default::default(),
            side_effect_queue_nonempty_since: None,
            pane_input_leases: Default::default(),
            timers: Default::default(),
            side_effect_buffer: config.side_effect_buffer,
            commands_processed: 0,
            metrics: Default::default(),
        };
        actor.queue_runtime_side_effects(initial_side_effects)?;
        actor.queue_peer_message_delivery_timer_if_needed(now_ms)?;
        Ok((handle, actor))
    }

    /// Returns the total number of requests retained by bounded lane queues.
    fn queued_request_count(&self) -> usize {
        let ingress = self
            .request_ingress
            .lock()
            .expect("actor request ingress lock must not be poisoned");
        ingress
            .urgent_requests
            .len()
            .saturating_add(ingress.interactive_requests.len())
            .saturating_add(ingress.normal_requests.len())
            .saturating_add(ingress.maintenance_requests.len())
    }

    /// Removes one request using bounded priority while retaining FIFO order per lane.
    fn dequeue_request(&mut self) -> Option<AsyncRuntimeRequestEnvelope> {
        let interactive_time_exhausted = self
            .request_scheduler
            .interactive_request_burst_started_at
            .is_some_and(|started_at| actor_request_burst_duration_exhausted(started_at.elapsed()));
        let normal_time_exhausted = self
            .request_scheduler
            .normal_request_burst_started_at
            .is_some_and(|started_at| actor_request_burst_duration_exhausted(started_at.elapsed()));
        let mut ingress = self
            .request_ingress
            .lock()
            .expect("actor request ingress lock must not be poisoned");
        let interactive_was_forced = self.request_scheduler.interactive_request_burst
            >= MAX_INTERACTIVE_REQUEST_BURST
            || interactive_time_exhausted
                && !ingress.interactive_requests.is_empty()
                && (!ingress.normal_requests.is_empty()
                    || !ingress.maintenance_requests.is_empty());
        let interactive_was_forced = interactive_was_forced
            && !ingress.interactive_requests.is_empty()
            && (!ingress.normal_requests.is_empty() || !ingress.maintenance_requests.is_empty());
        let maintenance_was_forced = self.request_scheduler.normal_request_burst
            >= MAX_INTERACTIVE_REQUEST_BURST
            || normal_time_exhausted
                && !ingress.normal_requests.is_empty()
                && !ingress.maintenance_requests.is_empty();
        let maintenance_was_forced = maintenance_was_forced
            && !ingress.normal_requests.is_empty()
            && !ingress.maintenance_requests.is_empty();
        match next_actor_request_lane(
            !ingress.urgent_requests.is_empty(),
            !ingress.interactive_requests.is_empty(),
            !ingress.normal_requests.is_empty(),
            !ingress.maintenance_requests.is_empty(),
            self.request_scheduler.interactive_request_burst,
            self.request_scheduler.normal_request_burst,
            interactive_time_exhausted,
            normal_time_exhausted,
        )? {
            ActorRequestLane::Urgent => ingress
                .urgent_requests
                .pop_front()
                .map(|request| request.envelope),
            ActorRequestLane::Interactive => {
                if self.request_scheduler.interactive_request_burst == 0 {
                    self.request_scheduler.interactive_request_burst_started_at =
                        Some(std::time::Instant::now());
                }
                self.request_scheduler.interactive_request_burst = self
                    .request_scheduler
                    .interactive_request_burst
                    .saturating_add(1);
                ingress
                    .interactive_requests
                    .pop_front()
                    .map(|request| request.envelope)
            }
            ActorRequestLane::Normal => {
                self.request_scheduler.interactive_request_burst = 0;
                self.request_scheduler.interactive_request_burst_started_at = None;
                if self.request_scheduler.normal_request_burst == 0 {
                    self.request_scheduler.normal_request_burst_started_at =
                        Some(std::time::Instant::now());
                }
                self.request_scheduler.normal_request_burst = self
                    .request_scheduler
                    .normal_request_burst
                    .saturating_add(1);
                if interactive_was_forced {
                    self.metrics.actor_normal_fairness_services = self
                        .metrics
                        .actor_normal_fairness_services
                        .saturating_add(1);
                }
                ingress
                    .normal_requests
                    .pop_front()
                    .map(|request| request.envelope)
            }
            ActorRequestLane::Maintenance => {
                self.request_scheduler.interactive_request_burst = 0;
                self.request_scheduler.interactive_request_burst_started_at = None;
                self.request_scheduler.normal_request_burst = 0;
                self.request_scheduler.normal_request_burst_started_at = None;
                if maintenance_was_forced {
                    self.metrics.actor_maintenance_fairness_services = self
                        .metrics
                        .actor_maintenance_fairness_services
                        .saturating_add(1);
                }
                ingress
                    .maintenance_requests
                    .pop_front()
                    .map(|request| request.envelope)
            }
        }
    }

    /// Runs the run operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn run(mut self) -> AsyncRuntimeActorExit {
        loop {
            if self.request_scheduler.requests_since_clipboard_cleanup
                >= MAX_REQUESTS_BEFORE_CLIPBOARD_CLEANUP
                && self.queued_request_count() > 0
                && let Ok(cleanup) = self.client_clipboard_route_cleanup_rx.try_recv()
            {
                self.cleanup_client_clipboard_route(cleanup.client_id, cleanup.generation);
                self.request_scheduler.requests_since_clipboard_cleanup = 0;
                continue;
            }
            let envelope = if let Some(envelope) = self.dequeue_request() {
                envelope
            } else {
                tokio::select! {
                    biased;
                    () = self.request_ingress_notify.notified() => continue,
                    Some(cleanup) = self.client_clipboard_route_cleanup_rx.recv() => {
                        self.cleanup_client_clipboard_route(cleanup.client_id, cleanup.generation);
                        self.request_scheduler.requests_since_clipboard_cleanup = 0;
                        // A cleanup is cancellation bookkeeping rather than an actor request.
                        // Re-enter the loop so ingress is checked before another cleanup.
                        continue;
                    }
                }
            };
            let queue_wait_ms =
                u64::try_from(envelope.enqueued_at.elapsed().as_millis()).unwrap_or(u64::MAX);
            let handler_started = std::time::Instant::now();
            self.request_scheduler.requests_since_clipboard_cleanup = self
                .request_scheduler
                .requests_since_clipboard_cleanup
                .saturating_add(1);
            self.commands_processed += 1;
            self.metrics.commands_processed = self.commands_processed;
            // The cached snapshot is read only by display commands, which arrive
            // as control requests or carried terminal steps, so it is published
            // on demand for those families before the handler that reads it.
            // Every other request - render, event, provider, side-effect - no
            // longer pays two full metric clones on the actor's hot path.
            if matches!(
                envelope.family,
                crate::host::async_runtime::AsyncRuntimeRequestFamily::Control
                    | crate::host::async_runtime::AsyncRuntimeRequestFamily::Terminal
            ) {
                self.sync_metrics_snapshot_to_service();
            }
            let should_shutdown = self.handle_request(envelope.request).await;
            let handler_duration_ms =
                u64::try_from(handler_started.elapsed().as_millis()).unwrap_or(u64::MAX);
            if envelope.record_actor_latency {
                self.metrics.record_request_latency(
                    envelope.family,
                    queue_wait_ms,
                    handler_duration_ms,
                );
            }
            if should_shutdown {
                break;
            }
        }

        self.sender.close();

        AsyncRuntimeActorExit {
            service: self.service,
            commands_processed: self.commands_processed,
            metrics: self.metrics,
        }
    }
    /// Records terminal control request counters from framed control input.
    ///
    /// The metrics path is best-effort: malformed or partial frames are left
    /// to the normal control dispatcher so diagnostics never change request
    /// handling semantics.
    pub(super) fn record_terminal_control_request_metrics(
        &mut self,
        input: &[u8],
        max_content_length: usize,
    ) {
        let mut offset = 0usize;
        while offset < input.len() {
            let Ok((body, consumed)) = decode_control_frame(&input[offset..], max_content_length)
            else {
                break;
            };
            if consumed == 0 {
                break;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body)
                && let Some(method) = value.get("method").and_then(serde_json::Value::as_str)
            {
                match method {
                    "terminal/step" => {
                        self.metrics.terminal_step_control_requests = self
                            .metrics
                            .terminal_step_control_requests
                            .saturating_add(1);
                    }
                    "terminal/view" => {
                        self.metrics.terminal_view_control_requests = self
                            .metrics
                            .terminal_view_control_requests
                            .saturating_add(1);
                    }
                    _ => {}
                }
            }
            offset = offset.saturating_add(consumed);
        }
    }
    /// Returns the current metrics snapshot with live queue depth included.
    pub(super) fn current_metrics_snapshot(
        &self,
    ) -> crate::host::async_runtime::AsyncRuntimeActorMetrics {
        let mut metrics = self.metrics.clone();
        let ingress = self
            .request_ingress
            .lock()
            .expect("actor request ingress lock must not be poisoned");
        metrics.actor_ingress_queue_depth = ingress
            .urgent_requests
            .len()
            .saturating_add(ingress.interactive_requests.len())
            .saturating_add(ingress.normal_requests.len())
            .saturating_add(ingress.maintenance_requests.len());
        metrics.actor_urgent_queue_depth = ingress.urgent_requests.len();
        metrics.actor_interactive_queue_depth = ingress.interactive_requests.len();
        metrics.actor_normal_queue_depth = ingress.normal_requests.len();
        metrics.actor_maintenance_queue_depth = ingress.maintenance_requests.len();
        metrics.actor_oldest_local_queue_wait_ms = oldest_queued_request_wait_ms(
            ingress
                .urgent_requests
                .front()
                .map(|request| &request.envelope),
            ingress
                .interactive_requests
                .front()
                .map(|request| &request.envelope),
            ingress
                .normal_requests
                .front()
                .map(|request| &request.envelope),
            ingress
                .maintenance_requests
                .front()
                .map(|request| &request.envelope),
        );
        metrics.side_effect_queue_depth = self.side_effects.len();
        metrics
    }
    /// Copies the current actor metrics snapshot into runtime service state.
    pub(super) fn sync_metrics_snapshot_to_service(&mut self) {
        self.service
            .set_async_runtime_metrics(self.current_metrics_snapshot());
    }
}
