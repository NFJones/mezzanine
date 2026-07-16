//! Actor construction, run-loop ownership, and actor metrics.

use super::*;

/// Runs the execute snapshot control async work operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn execute_snapshot_control_async_work(
    snapshots: &crate::snapshot::SnapshotRepository,
    work: &RuntimeSnapshotControlAsyncWork,
) -> RuntimeSnapshotControlAsyncOutcome {
    match &work.kind {
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
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(
        mut service: RuntimeSessionService,
        config: AsyncRuntimeActorConfig,
    ) -> Result<(AsyncRuntimeSessionHandle, Self)> {
        if config.command_buffer == 0 {
            return Err(MezError::invalid_args(
                "async runtime command buffer must be greater than zero",
            ));
        }
        if config.side_effect_buffer == 0 {
            return Err(MezError::invalid_args(
                "async runtime side-effect buffer must be greater than zero",
            ));
        }

        let (sender, receiver) = mpsc::channel(config.command_buffer);
        let message_delivery_notify = Arc::new(Notify::new());
        let event_delivery_notify = Arc::new(Notify::new());
        let side_effect_delivery_notify = Arc::new(Notify::new());
        let (side_effect_delivery_tx, side_effect_delivery_rx) = watch::channel(0u64);
        let (lifecycle_state_tx, lifecycle_state_rx) = watch::channel(service.lifecycle_state());
        service.use_audit_effect_adapter();
        service.use_pane_pipe_effect_adapter();
        service.use_transcript_effect_adapter();
        service.use_registry_effect_adapter();
        service.use_config_effect_adapter();
        service.use_hook_effect_adapter();
        Ok((
            AsyncRuntimeSessionHandle {
                sender: sender.clone(),
                message_delivery_notify: message_delivery_notify.clone(),
                event_delivery_notify: event_delivery_notify.clone(),
                side_effect_delivery_notify: side_effect_delivery_notify.clone(),
                side_effect_delivery_rx,
                lifecycle_state_rx,
            },
            Self {
                service,
                sender: sender.clone(),
                receiver,
                message_delivery_notify,
                event_delivery_notify,
                side_effect_delivery_notify,
                side_effect_delivery_tx,
                lifecycle_state_tx,
                side_effects: VecDeque::with_capacity(config.side_effect_buffer),
                timers: Default::default(),
                provider_output_limit_compaction_turns: Default::default(),
                side_effect_buffer: config.side_effect_buffer,
                commands_processed: 0,
                metrics: Default::default(),
            },
        ))
    }

    /// Runs the run operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn run(mut self) -> AsyncRuntimeActorExit {
        while let Some(request) = self.receiver.recv().await {
            self.commands_processed += 1;
            self.metrics.commands_processed = self.commands_processed;
            self.sync_metrics_snapshot_to_service();
            if self.handle_request(request).await {
                break;
            }
            self.sync_metrics_snapshot_to_service();
        }

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
    ) -> crate::async_runtime::AsyncRuntimeActorMetrics {
        let mut metrics = self.metrics.clone();
        metrics.side_effect_queue_depth = self.side_effects.len();
        metrics
    }
    /// Copies the current actor metrics snapshot into runtime service state.
    pub(super) fn sync_metrics_snapshot_to_service(&mut self) {
        self.service
            .set_async_runtime_metrics(self.current_metrics_snapshot());
    }
}
