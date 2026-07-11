//! Async Runtime Actor implementation.
//!
//! This module owns the async runtime actor boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.
use super::{
    AgentId, AgentProviderEvent, Arc, AsyncControlInputResult, AsyncHookEvent, AsyncMessageFanout,
    AsyncMessageInputResult, AsyncRenderedClientFlush, AsyncRenderedClientFrame,
    AsyncRuntimeActorConfig, AsyncRuntimeActorExit, AsyncRuntimeRequest, AsyncRuntimeSessionActor,
    AsyncRuntimeSessionHandle, AttachedClientStepApplication, AttachedTerminalClientStepPlan,
    AttachedTerminalOutputModes, ClientEvent, ClientId, ClientState, ClientStatusLine,
    ClientViewRole, ControlConnectionState, DEFAULT_ASYNC_IDLE_CLEANUP_INTERVAL, DeliveryCursor,
    FanoutBatch, MessageConnection, MezError, Notify, PaneEvent, PersistenceEvent,
    RenderInvalidationReason, RenderedClientView, Result, RuntimeAgentProviderDispatch,
    RuntimeAgentProviderTask, RuntimeEvent, RuntimeEventBatch, RuntimeEventConnectionTable,
    RuntimeEventIngressReport, RuntimeEventWakeup, RuntimeLifecycleState, RuntimeSessionService,
    RuntimeSideEffect, RuntimeSnapshotControlAsyncOutcome, RuntimeSnapshotControlAsyncWork,
    RuntimeSnapshotControlAsyncWorkKind, RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition,
    ShutdownEvent, Size, TerminalClientLoopConfig, TimerEvent, VecDeque,
    compose_client_presentation_with_styles, delivery_batch_json, encode_mmp_body, mpsc, oneshot,
    watch,
};
use crate::agent::{
    DEFAULT_PROVIDER_TIMEOUT_MS, ProviderErrorRetryClass, provider_error_retry_class_from_parts,
    provider_event_error_from_parts, provider_event_error_kind,
};
use crate::control::{decode_control_frame, encode_control_body};
use crate::runtime::PaneResizeUpdate;
#[cfg(test)]
use crate::runtime::coalesce_config_persistence_effects;

// Serialized runtime actor and handle implementation.

/// Defines the DEFAULT SHELL RECOVERY INTERVAL MS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEFAULT_SHELL_RECOVERY_INTERVAL_MS: u64 = 250;
/// Grace added to provider worker claim leases beyond the provider timeout.
///
/// The async runtime watchdog must never expire a legitimate provider request
/// before the provider transport has had a chance to return its own timeout or
/// failure. A small grace window covers timer scheduling and event-ingress
/// latency without leaving abandoned worker claims unbounded.
const DEFAULT_PROVIDER_CLAIM_TIMEOUT_GRACE_MS: u64 = 30_000;
/// Provider worker claim lease before the runtime fails a still-running turn.
///
/// This lease follows the provider transport timeout instead of using an
/// independent short watchdog, preventing long-running model requests from
/// being failed by the actor while the HTTP provider call is still valid.
const DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS: u64 =
    DEFAULT_PROVIDER_TIMEOUT_MS + DEFAULT_PROVIDER_CLAIM_TIMEOUT_GRACE_MS;
/// Defines the DEFAULT PANE PIPE HEALTH DELAY MS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEFAULT_PANE_PIPE_HEALTH_DELAY_MS: u64 = 50;

/// Runs the execute snapshot control async work operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn execute_snapshot_control_async_work(
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
        service.use_external_effect_adapter();
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
    fn record_terminal_control_request_metrics(&mut self, input: &[u8], max_content_length: usize) {
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
    fn current_metrics_snapshot(&self) -> super::AsyncRuntimeActorMetrics {
        let mut metrics = self.metrics.clone();
        metrics.side_effect_queue_depth = self.side_effects.len();
        metrics
    }
    /// Copies the current actor metrics snapshot into runtime service state.
    fn sync_metrics_snapshot_to_service(&mut self) {
        self.service
            .set_async_runtime_metrics(self.current_metrics_snapshot());
    }

    /// Runs the handle request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn handle_request(&mut self, request: AsyncRuntimeRequest) -> bool {
        match request {
            AsyncRuntimeRequest::LifecycleState { reply } => {
                let _ = reply.send(self.service.lifecycle_state());
                false
            }
            AsyncRuntimeRequest::Metrics { reply } => {
                let mut metrics = self.metrics.clone();
                metrics.side_effect_queue_depth = self.side_effects.len();
                let _ = reply.send(metrics);
                false
            }
            AsyncRuntimeRequest::RenderClientView {
                role,
                client_size,
                config,
                reply,
            } => {
                self.metrics.render_client_view_requests =
                    self.metrics.render_client_view_requests.saturating_add(1);
                let result = self.service.render_client_view(role, client_size, &config);
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::RenderClientFrame {
                role,
                client_size,
                config,
                render,
                reply,
            } => {
                if render {
                    self.metrics.render_client_frame_requests =
                        self.metrics.render_client_frame_requests.saturating_add(1);
                }
                let result = self
                    .service
                    .terminal_client_loop_config(config)
                    .and_then(|config| {
                        let view = if render {
                            self.service.render_client_view_with_resolved_config(
                                role,
                                client_size,
                                &config,
                            )?
                        } else {
                            None
                        };
                        Ok(AsyncRenderedClientFrame { config, view })
                    });
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::RenderClientSideEffect {
                client_id,
                config,
                status,
                cursor_blink_elapsed_ms,
                reply,
            } => {
                let result = self.render_client_side_effect(
                    client_id,
                    config,
                    status,
                    cursor_blink_elapsed_ms,
                );
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::EnsureClientRenderTimers { client_id, reply } => {
                let result = self.ensure_client_render_timers(&client_id);
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::TerminalClientLoopConfig { config, reply } => {
                let result = self.service.terminal_client_loop_config(config);
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::HandleControlInput {
                input,
                max_content_length,
                mut connection,
                reply,
            } => {
                self.record_terminal_control_request_metrics(&input, max_content_length);
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self
                    .service
                    .handle_control_input_for_connection_transition(
                        &input,
                        max_content_length,
                        &mut connection,
                    )
                    .and_then(|(output, consumed, transition)| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_runtime_side_effects(transition.side_effects)?;
                        Ok(AsyncControlInputResult {
                            output,
                            consumed,
                            connection,
                        })
                    });
                let should_notify = result.as_ref().is_ok_and(|result| result.consumed > 0);
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::HandleControlInputWithSnapshots {
                input,
                max_content_length,
                mut connection,
                snapshots,
                reply,
            } => {
                self.record_terminal_control_request_metrics(&input, max_content_length);
                if let Ok((body, consumed)) = decode_control_frame(&input, max_content_length)
                    && consumed == input.len()
                    && let Some(prepared) = self
                        .service
                        .prepare_runtime_snapshot_control_async_work(&body, &connection)
                {
                    match prepared {
                        Ok(work) => {
                            let sender = self.sender.clone();
                            let join_handle = tokio::spawn(async move {
                                let outcome =
                                    execute_snapshot_control_async_work(&snapshots, &work).await;
                                let _ = sender
                                    .send(AsyncRuntimeRequest::CompleteSnapshotControlInput {
                                        consumed,
                                        connection,
                                        work,
                                        outcome: Box::new(outcome),
                                        reply,
                                    })
                                    .await;
                            });
                            std::mem::drop(join_handle);
                            return false;
                        }
                        Err(body) => {
                            let result = Ok(AsyncControlInputResult {
                                output: encode_control_body(&body),
                                consumed,
                                connection,
                            });
                            let _ = reply.send(result);
                            self.notify_event_delivery();
                            return false;
                        }
                    }
                }
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self
                    .service
                    .handle_control_input_for_connection_with_snapshots_transition(
                        &input,
                        max_content_length,
                        &mut connection,
                        &snapshots,
                    )
                    .await
                    .and_then(|(output, consumed, transition)| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_runtime_side_effects(transition.side_effects)?;
                        Ok(AsyncControlInputResult {
                            output,
                            consumed,
                            connection,
                        })
                    });
                let should_notify = result.as_ref().is_ok_and(|result| result.consumed > 0);
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::CompleteSnapshotControlInput {
                consumed,
                mut connection,
                work,
                outcome,
                reply,
            } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let (body, transition) = self
                    .service
                    .complete_runtime_snapshot_control_async_work_transition(
                        work,
                        *outcome,
                        &mut connection,
                    );
                let result = self
                    .queue_deferred_pane_io_side_effects_from_service()
                    .and_then(|_| self.queue_runtime_side_effects(transition.side_effects))
                    .map(|_| AsyncControlInputResult {
                        output: encode_control_body(&body),
                        consumed,
                        connection,
                    });
                let should_notify = result.is_ok();
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::HandleMessageInput {
                input,
                max_content_length,
                mut connection,
                now_ms,
                reply,
            } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self
                    .service
                    .handle_message_input(&input, max_content_length, &mut connection, now_ms)
                    .and_then(|(output, consumed)| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        Ok(AsyncMessageInputResult {
                            output,
                            consumed,
                            connection,
                        })
                    });
                let should_notify = result.as_ref().is_ok_and(|result| result.consumed > 0);
                let _ = reply.send(result);
                if should_notify {
                    self.notify_message_delivery();
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::MessageFanoutReadyFor {
                recipient,
                now_ms,
                limit,
                reply,
            } => {
                let result = self
                    .service
                    .message_service()
                    .fanout_ready_for(&recipient, now_ms, limit)
                    .map(|fanout| {
                        fanout.map(|batch| {
                            let body = delivery_batch_json(&batch.batch);
                            let frame = encode_mmp_body(&body);
                            let messages = batch.batch.messages.len();
                            AsyncMessageFanout {
                                recipient,
                                frame,
                                messages,
                                batch,
                            }
                        })
                    });
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::AcknowledgeMessageFanout { batch, reply } => {
                let result = self
                    .service
                    .message_service_mut()
                    .acknowledge_fanout_batch(&batch);
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::EventWakeups {
                connections,
                limit_per_connection,
                reply,
            } => {
                let wakeups = connections.wakeups(self.service.event_log(), limit_per_connection);
                let _ = reply.send(Ok(wakeups));
                false
            }
            AsyncRuntimeRequest::ApplyAttachedTerminalStep {
                primary_client_id,
                step,
                reply,
            } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self
                    .service
                    .apply_attached_terminal_step_transition(&primary_client_id, &step)
                    .and_then(|(application, transition)| {
                        self.queue_runtime_side_effects(transition.side_effects)?;
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_pending_provider_dispatch_side_effects()?;
                        Ok(application)
                    });
                let _ = reply.send(result);
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::ApplyAttachedTerminalStepInlinePaneIo {
                primary_client_id,
                step,
                reply,
            } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self
                    .service
                    .apply_attached_terminal_step_plan(&primary_client_id, &step)
                    .and_then(|application| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_pending_provider_dispatch_side_effects()?;
                        Ok(application)
                    });
                let _ = reply.send(result);
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::ResizeAttachedPrimaryTerminal {
                primary_client_id,
                size,
                reply,
            } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self
                    .service
                    .resize_attached_primary_terminal(&primary_client_id, size)
                    .and_then(|updates| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_shell_transaction_timer_side_effects()?;
                        Ok(updates)
                    });
                let should_notify = result.as_ref().is_ok_and(|updates| !updates.is_empty());
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::ExecuteTerminalCommand {
                primary_client_id,
                input,
                reply,
            } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self
                    .service
                    .execute_terminal_command_async(&primary_client_id, &input)
                    .await
                    .and_then(|output| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_command_pane_pipe_health_timer_side_effects()?;
                        Ok(output)
                    });
                let should_notify = result.is_ok();
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::RefreshProviderInfo { reply } => {
                let result = self.service.refresh_provider_info_async().await;
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::ShowPrimaryDisplayOverlay { lines, reply } => {
                let result = self.service.show_primary_display_overlay(lines);
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::ShowPrimaryErrorOverlay { lines, reply } => {
                let result = self.service.show_primary_error_overlay(lines);
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::ExecuteAgentShellCommand {
                primary_client_id,
                input,
                reply,
            } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self
                    .service
                    .execute_agent_shell_command_async(&primary_client_id, &input)
                    .await
                    .and_then(|output| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_shell_transaction_timer_side_effects()?;
                        self.queue_pending_provider_dispatch_side_effects()?;
                        Ok(output)
                    });
                let should_notify = result.is_ok();
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::PendingAgentProviderTasks { reply } => {
                let _ = reply.send(Ok(self.service.pending_agent_provider_tasks()));
                false
            }
            AsyncRuntimeRequest::AgentTurnIsRunning { turn_id, reply } => {
                let _ = reply.send(Ok(self.service.agent_turn_is_running(&turn_id)));
                false
            }
            AsyncRuntimeRequest::QueueProviderPollTimerIfNeeded {
                generation,
                delay_ms,
                reply,
            } => {
                let result = self.queue_provider_poll_timer_if_needed(generation, delay_ms);
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::ClaimConfiguredAgentProviderTask {
                agent_id,
                turn_id,
                reply,
            } => {
                let result = self
                    .service
                    .ensure_runtime_mcp_transports_discovered_async()
                    .await;
                let result = match result {
                    Ok(_) => {
                        let refresh_result =
                            if let Some(auth_store) = self.service.auth_store().cloned() {
                                let leeway_seconds =
                                    self.service.provider_auth_refresh_leeway_seconds();
                                auth_store
                                    .refresh_openai_provider_credential_if_needed_with_leeway_async(
                                        leeway_seconds,
                                    )
                                    .await
                                    .map(|_| ())
                            } else {
                                Ok(())
                            };
                        refresh_result.and_then(|_| {
                            self.service
                                .claim_configured_agent_provider_task(&agent_id, &turn_id)
                        })
                    }
                    Err(error) => Err(error),
                };
                let result = result.and_then(|dispatch| {
                    if let Some(dispatch) = dispatch {
                        self.timers.next_provider_claim_generation =
                            self.timers.next_provider_claim_generation.saturating_add(1);
                        let generation = self.timers.next_provider_claim_generation;
                        let transition = self.service.record_claimed_agent_provider_task(
                            &dispatch,
                            generation,
                            DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS,
                        )?;
                        self.queue_runtime_side_effects(transition.side_effects)?;
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        Ok(Some(dispatch))
                    } else {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_shell_transaction_timer_side_effects()?;
                        Ok(None)
                    }
                });
                let should_notify = result.is_ok();
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                false
            }
            AsyncRuntimeRequest::ClaimAgentCompactionTask { pane_id, reply } => {
                let result = self.service.claim_agent_compaction_task(&pane_id);
                let should_notify = result.is_ok();
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                false
            }
            AsyncRuntimeRequest::ClaimAgentRememberTask { pane_id, reply } => {
                let result = self.service.claim_agent_remember_task(&pane_id);
                let should_notify = result.is_ok();
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                false
            }
            AsyncRuntimeRequest::SubmitRuntimeEvents { batch, reply } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self.apply_runtime_event_batch(batch).await;
                let should_notify = result.as_ref().is_ok_and(|report| report.applied > 0);
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::DrainRuntimeSideEffects { limit, reply } => {
                let _ = reply.send(self.drain_runtime_side_effects(limit));
                false
            }
            AsyncRuntimeRequest::QueueRuntimeSideEffects {
                side_effects,
                reply,
            } => {
                let queued = side_effects.len();
                let result = self
                    .queue_runtime_side_effects(side_effects)
                    .map(|()| queued);
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::DrainAgentProviderDispatchSideEffects { limit, reply } => {
                let _ = reply.send(self.drain_agent_provider_dispatch_side_effects(limit));
                false
            }
            AsyncRuntimeRequest::DrainRenderSideEffects { limit, reply } => {
                let _ = reply.send(self.drain_render_side_effects(limit));
                false
            }
            AsyncRuntimeRequest::DrainRenderSideEffectsForClient {
                client_id,
                limit,
                reply,
            } => {
                let _ = reply.send(self.drain_render_side_effects_for_client(&client_id, limit));
                false
            }
            AsyncRuntimeRequest::DrainClientOutputFlushSideEffects {
                client_id,
                limit,
                reply,
            } => {
                let _ = reply
                    .send(self.drain_client_output_flush_side_effects(client_id.as_ref(), limit));
                false
            }
            AsyncRuntimeRequest::DrainTimerSideEffects { limit, reply } => {
                let _ = reply.send(self.drain_timer_side_effects(limit));
                false
            }
            AsyncRuntimeRequest::DrainPersistenceSideEffects { limit, reply } => {
                let _ = reply.send(self.drain_persistence_side_effects(limit));
                false
            }
            AsyncRuntimeRequest::DrainHookSideEffects { limit, reply } => {
                let _ = reply.send(self.drain_hook_side_effects(limit));
                false
            }
            AsyncRuntimeRequest::DrainPaneIoSideEffects {
                pane_id,
                limit,
                reply,
            } => {
                let _ = reply.send(self.drain_pane_io_side_effects(&pane_id, limit));
                false
            }
            AsyncRuntimeRequest::TakeRunningPaneProcessesForAdapter { limit, reply } => {
                let result = self.service.take_running_pane_processes_for_adapter(limit);
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::Shutdown { reply } => {
                let _ = self.service.clear_runtime_mcp_transports();
                let _ = reply.send(self.service.lifecycle_state());
                true
            }
        }
    }

    /// Runs the notify message delivery operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn notify_message_delivery(&mut self) {
        self.metrics.message_delivery_notifications = self
            .metrics
            .message_delivery_notifications
            .saturating_add(1);
        self.message_delivery_notify.notify_waiters();
    }

    /// Runs the notify event delivery operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn notify_event_delivery(&mut self) {
        self.metrics.event_delivery_notifications =
            self.metrics.event_delivery_notifications.saturating_add(1);
        self.event_delivery_notify.notify_waiters();
        self.event_delivery_notify.notify_one();
    }

    /// Runs the notify side effect delivery operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn notify_side_effect_delivery(&mut self) {
        self.metrics.side_effect_delivery_notifications = self
            .metrics
            .side_effect_delivery_notifications
            .saturating_add(1);
        let _ = self
            .side_effect_delivery_tx
            .send(self.metrics.side_effect_delivery_notifications);
        self.side_effect_delivery_notify.notify_waiters();
        self.side_effect_delivery_notify.notify_one();
    }

    /// Runs the notify lifecycle state if changed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn notify_lifecycle_state_if_changed(&mut self, previous: RuntimeLifecycleState) {
        let current = self.service.lifecycle_state();
        if current == previous {
            return;
        }
        self.metrics.lifecycle_state_notifications =
            self.metrics.lifecycle_state_notifications.saturating_add(1);
        let _ = self.lifecycle_state_tx.send(current);
    }

    /// Runs the apply runtime event batch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn apply_runtime_event_batch(
        &mut self,
        batch: RuntimeEventBatch,
    ) -> Result<RuntimeEventIngressReport> {
        let mut report = batch.ingress_report();
        let mut registry_persistence_queued = false;
        let mut registry_persistence_required = false;
        for event in batch.prioritized_events() {
            let event_requires_registry_persistence =
                runtime_event_requires_registry_persistence(&event);
            let mut application = self.apply_runtime_event(event).await?;
            if application.applied {
                registry_persistence_required =
                    registry_persistence_required || event_requires_registry_persistence;
                self.service.maybe_bootstrap_ready_panes()?;
                let mut actor_progress_turn_ids = self.actor_owned_agent_progress_turn_ids();
                actor_progress_turn_ids.extend(
                    application
                        .side_effects
                        .iter()
                        .filter_map(provider_retry_timer_side_effect_turn_id),
                );
                let reconciled = self
                    .service
                    .reconcile_agent_runtime_progress_paths_with_actor_progress(
                        &actor_progress_turn_ids,
                    )?;
                if reconciled > 0 {
                    application
                        .side_effects
                        .extend(self.render_side_effects(RenderInvalidationReason::FullRedraw));
                    application
                        .side_effects
                        .extend(self.pending_provider_dispatch_side_effects()?);
                }
                application
                    .side_effects
                    .extend(self.deferred_service_side_effects_from_service());
                application
                    .side_effects
                    .extend(self.cancel_stale_shell_transaction_timer_side_effects());
                application
                    .side_effects
                    .extend(self.shell_transaction_timer_side_effects());
                application
                    .side_effects
                    .extend(self.idle_cleanup_timer_side_effects());
            } else {
                application
                    .side_effects
                    .extend(self.deferred_service_side_effects_from_service());
            }
            registry_persistence_queued = registry_persistence_queued
                || side_effects_include_registry_persistence(&application.side_effects);
            report.side_effects = report
                .side_effects
                .saturating_add(application.side_effects.len());
            self.queue_runtime_side_effects(application.side_effects)?;
            if application.applied {
                report.applied = report.applied.saturating_add(1);
            }
        }
        if report.applied > 0
            && registry_persistence_required
            && report
                .families
                .iter()
                .any(|family| family.as_str() != "persistence")
            && !registry_persistence_queued
            && let Some((registry, update)) = self.service.registry_update_for_async_persistence()
        {
            self.queue_runtime_side_effects(vec![RuntimeSideEffect::PersistRegistry {
                registry,
                update,
            }])?;
            report.side_effects = report.side_effects.saturating_add(1);
        }
        self.metrics.runtime_event_batches = self.metrics.runtime_event_batches.saturating_add(1);
        self.metrics.runtime_events_accepted = self
            .metrics
            .runtime_events_accepted
            .saturating_add(u64::try_from(report.accepted).unwrap_or(u64::MAX));
        self.metrics.runtime_events_applied = self
            .metrics
            .runtime_events_applied
            .saturating_add(u64::try_from(report.applied).unwrap_or(u64::MAX));
        self.metrics
            .runtime_event_batch_sizes
            .record(u64::try_from(report.accepted).unwrap_or(u64::MAX));
        Ok(report)
    }

    /// Runs the apply runtime event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn apply_runtime_event(&mut self, event: RuntimeEvent) -> Result<RuntimeTransition> {
        match event {
            RuntimeEvent::Pane(PaneEvent::Output { pane_id, bytes }) => {
                let byte_count = bytes.len();
                let pane_id_for_pipe_health = pane_id.clone();
                let mut transition = self.service.apply_pane_output_transition(pane_id, bytes)?;
                if transition.applied {
                    self.metrics.pane_output_chunks =
                        self.metrics.pane_output_chunks.saturating_add(1);
                    self.metrics.pane_output_bytes = self
                        .metrics
                        .pane_output_bytes
                        .saturating_add(u64::try_from(byte_count).unwrap_or(u64::MAX));
                    self.metrics
                        .pane_output_chunk_bytes
                        .record(u64::try_from(byte_count).unwrap_or(u64::MAX));
                }
                if transition.applied {
                    transition.side_effects.extend(
                        self.pane_pipe_health_timer_side_effects_for_pane(
                            &pane_id_for_pipe_health,
                        )?,
                    );
                }
                Ok(transition)
            }
            RuntimeEvent::Pane(pane_event) => {
                self.service.apply_pane_completion_transition(pane_event)
            }
            RuntimeEvent::Client(client_event) => self.apply_runtime_client_event(client_event),
            RuntimeEvent::AgentProvider(provider_event) => {
                self.apply_runtime_agent_provider_event(provider_event)
                    .await
            }
            RuntimeEvent::AgentCompaction(compaction_event) => {
                let mut transition = self
                    .service
                    .apply_agent_compaction_transition(compaction_event)?;
                if transition.applied {
                    transition
                        .side_effects
                        .extend(self.pending_provider_dispatch_side_effects()?);
                }
                Ok(transition)
            }
            RuntimeEvent::AgentRemember(remember_event) => {
                self.service.apply_agent_remember_transition(remember_event)
            }
            RuntimeEvent::Hook(hook_event) => self.apply_runtime_hook_event(hook_event),
            RuntimeEvent::Persistence(persistence_event) => {
                self.apply_runtime_persistence_event(persistence_event)
            }
            RuntimeEvent::Shutdown(shutdown) => self.apply_runtime_shutdown_event(shutdown),
            RuntimeEvent::Timer(timer) => self.apply_runtime_timer_event(timer),
            RuntimeEvent::Process(process_event) => {
                self.service.apply_process_transition(process_event)
            }
        }
    }

    /// Runs the apply runtime timer event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_runtime_timer_event(&mut self, timer: TimerEvent) -> Result<RuntimeTransition> {
        if runtime_timer_kind_is_shell_transaction(timer.key.kind) {
            self.timers.shell_transactions.remove(&timer.key);
        }
        match timer.key.kind {
            RuntimeTimerKind::ShellTransaction
            | RuntimeTimerKind::ReadinessProbe
            | RuntimeTimerKind::Bootstrap
            | RuntimeTimerKind::FocusedShellHook => {
                let mut transition = self
                    .service
                    .apply_shell_transaction_timer_transition(timer.now_ms)?;
                if !transition.applied {
                    transition
                        .side_effects
                        .extend(self.shell_transaction_timer_side_effects());
                }
                Ok(transition)
            }
            RuntimeTimerKind::ResizeDebounce => {
                let active = self.timers.resize_debounce.remove(&timer.key);
                if !active {
                    self.record_ignored_timer_event();
                }
                Ok(self.service.apply_resize_debounce_timer_transition(active))
            }
            RuntimeTimerKind::CursorBlink => {
                if self.timers.cursor_blink.get(timer.key.owner_id.as_str()) != Some(&timer.key) {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers.cursor_blink.remove(timer.key.owner_id.as_str());
                let mut application =
                    self.apply_render_timer_event(RenderInvalidationReason::CursorBlink);
                application
                    .side_effects
                    .extend(self.cursor_blink_timer_side_effects_for_client(
                        &timer.key.owner_id,
                        timer.now_ms,
                    )?);
                Ok(application)
            }
            RuntimeTimerKind::StatusRefresh => {
                if self.timers.status_refresh.get(timer.key.owner_id.as_str()) != Some(&timer.key) {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers
                    .status_refresh
                    .remove(timer.key.owner_id.as_str());
                let mut application =
                    self.apply_render_timer_event(RenderInvalidationReason::StatusLine);
                application.side_effects.extend(
                    self.status_refresh_timer_side_effects_for_client(
                        &timer.key.owner_id,
                        timer.now_ms,
                    )?,
                );
                Ok(application)
            }
            RuntimeTimerKind::IdleCleanup => {
                if self.timers.idle_cleanup.as_ref() != Some(&timer.key) {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers.idle_cleanup = None;
                let actor_progress_turn_ids = self.actor_owned_agent_progress_turn_ids();
                let cleaned = self
                    .service
                    .apply_idle_cleanup_timer_event_with_actor_progress(&actor_progress_turn_ids)?;
                let mut side_effects = self
                    .idle_cleanup_timer_side_effects_with_actor_progress(&actor_progress_turn_ids);
                if cleaned > 0 {
                    side_effects.extend(self.pending_provider_dispatch_side_effects()?);
                    side_effects
                        .extend(self.render_side_effects(RenderInvalidationReason::FullRedraw));
                }
                Ok(RuntimeTransition {
                    applied: cleaned > 0,
                    side_effects,
                })
            }
            RuntimeTimerKind::ProviderPoll => {
                if self.timers.provider_poll.as_ref() != Some(&timer.key) {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers.provider_poll = None;
                self.apply_provider_poll_timer_event()
            }
            RuntimeTimerKind::ProviderRetry => {
                if self.timers.provider_retry.get(timer.key.owner_id.as_str()) != Some(&timer.key) {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers
                    .provider_retry
                    .remove(timer.key.owner_id.as_str());
                self.apply_provider_retry_timer_event(&timer.key)
            }
            RuntimeTimerKind::ProviderClaim => {
                if self.timers.provider_claim.get(timer.key.owner_id.as_str()) != Some(&timer.key) {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers
                    .provider_claim
                    .remove(timer.key.owner_id.as_str());
                self.apply_provider_claim_timer_event(&timer.key)
            }
            RuntimeTimerKind::PanePipeHealth => {
                if self
                    .timers
                    .pane_pipe_health
                    .get(timer.key.owner_id.as_str())
                    != Some(&timer.key)
                {
                    self.record_ignored_timer_event();
                    return Ok(RuntimeTransition::default());
                }
                self.timers
                    .pane_pipe_health
                    .remove(timer.key.owner_id.as_str());
                let stopped = self
                    .service
                    .stop_completed_command_pane_pipe_for(&timer.key.owner_id)?;
                let mut side_effects = if stopped > 0 {
                    self.render_side_effects(RenderInvalidationReason::FullRedraw)
                } else {
                    Vec::new()
                };
                if stopped == 0 {
                    side_effects.extend(
                        self.pane_pipe_health_timer_side_effects_for_pane(&timer.key.owner_id)?,
                    );
                }
                Ok(RuntimeTransition {
                    applied: stopped > 0,
                    side_effects,
                })
            }
        }
    }

    /// Runs the record ignored timer event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn record_ignored_timer_event(&mut self) {
        self.metrics.runtime_timer_events_ignored =
            self.metrics.runtime_timer_events_ignored.saturating_add(1);
    }

    /// Runs the apply render timer event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_render_timer_event(&self, reason: RenderInvalidationReason) -> RuntimeTransition {
        let side_effects = self.render_side_effects(reason);
        RuntimeTransition {
            applied: !side_effects.is_empty(),
            side_effects,
        }
    }

    /// Runs the apply provider poll timer event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_provider_poll_timer_event(&self) -> Result<RuntimeTransition> {
        let side_effects = self.pending_provider_dispatch_side_effects()?;
        Ok(RuntimeTransition {
            applied: !side_effects.is_empty(),
            side_effects,
        })
    }

    /// Runs the apply provider retry timer event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_provider_retry_timer_event(
        &mut self,
        key: &RuntimeTimerKey,
    ) -> Result<RuntimeTransition> {
        self.service
            .apply_agent_provider_retry_timer_transition(&key.owner_id, key.generation)
    }

    /// Applies the timeout for an async provider worker claim lease.
    fn apply_provider_claim_timer_event(
        &mut self,
        key: &RuntimeTimerKey,
    ) -> Result<RuntimeTransition> {
        let applied = self
            .service
            .fail_expired_claimed_agent_provider_task(&key.owner_id, key.generation)?;
        let mut side_effects = if applied {
            self.pending_provider_dispatch_side_effects()?
        } else {
            Vec::new()
        };
        if applied {
            side_effects.extend(self.render_side_effects(RenderInvalidationReason::FullRedraw));
        }
        Ok(RuntimeTransition {
            applied,
            side_effects,
        })
    }

    /// Removes and returns a timer cancellation for a claimed provider task.
    fn provider_claim_cancel_timer_side_effects(
        &mut self,
        turn_id: &str,
    ) -> Vec<RuntimeSideEffect> {
        self.timers
            .provider_claim
            .remove(turn_id)
            .map(|key| RuntimeSideEffect::CancelTimer { key })
            .into_iter()
            .collect()
    }

    /// Runs the pending provider dispatch side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pending_provider_dispatch_side_effects(&self) -> Result<Vec<RuntimeSideEffect>> {
        let mut side_effects = Vec::new();
        for task in self.service.pending_agent_provider_tasks() {
            if self.provider_dispatch_is_already_queued(&task.turn_id) {
                continue;
            }
            let Some(agent_id) = AgentId::opaque(task.agent_id.clone()) else {
                return Err(MezError::invalid_state(format!(
                    "runtime provider task has invalid agent id `{}`",
                    task.agent_id
                )));
            };
            side_effects.push(RuntimeSideEffect::DispatchAgentProvider {
                agent_id,
                turn_id: task.turn_id,
            });
        }
        for pane_id in self.service.pending_agent_compaction_tasks() {
            if self.compaction_dispatch_is_already_queued(&pane_id) {
                continue;
            }
            side_effects.push(RuntimeSideEffect::DispatchAgentCompaction { pane_id });
        }
        for pane_id in self.service.pending_agent_remember_tasks() {
            if self.remember_dispatch_is_already_queued(&pane_id) {
                continue;
            }
            side_effects.push(RuntimeSideEffect::DispatchAgentRemember { pane_id });
        }
        Ok(side_effects)
    }

    /// Returns true when a queued compaction dispatch already exists for a pane.
    fn compaction_dispatch_is_already_queued(&self, target_pane_id: &str) -> bool {
        self.side_effects.iter().any(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::DispatchAgentCompaction { pane_id }
                    if pane_id == target_pane_id
            )
        })
    }

    /// Returns true when a queued durable memory dispatch already exists for a pane.
    fn remember_dispatch_is_already_queued(&self, target_pane_id: &str) -> bool {
        self.side_effects.iter().any(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::DispatchAgentRemember { pane_id } if pane_id == target_pane_id
            )
        })
    }

    /// Returns agent turns whose progress currently depends on actor-owned
    /// scheduling state instead of service-owned runtime state.
    ///
    /// Provider retry timers and automatic output-limit compaction dispatch are
    /// intentionally held outside ordinary service-owned provider tasks, so
    /// service-level reconciliation must be told that these turns still have a
    /// valid path to progress.
    fn actor_owned_agent_progress_turn_ids(&self) -> std::collections::BTreeSet<String> {
        let mut turn_ids: std::collections::BTreeSet<String> = self
            .service
            .agent_provider_retry_turn_ids()
            .chain(self.timers.provider_retry.keys())
            .cloned()
            .collect();
        turn_ids.extend(self.service.agent_compaction_resume_turn_ids());
        turn_ids
    }

    /// Runs the queue pending provider dispatch side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn queue_pending_provider_dispatch_side_effects(&mut self) -> Result<usize> {
        let side_effects = self.pending_provider_dispatch_side_effects()?;
        let count = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(count)
    }

    /// Runs the queue deferred pane io side effects from service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn queue_deferred_pane_io_side_effects_from_service(&mut self) -> Result<usize> {
        let side_effects = self.deferred_service_side_effects_from_service();
        let count = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(count)
    }

    /// Runs the deferred service side effects from service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn deferred_service_side_effects_from_service(&mut self) -> Vec<RuntimeSideEffect> {
        self.service
            .drain_deferred_effects_transition()
            .side_effects
    }

    /// Runs the deferred pane io side effects from service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// Runs the queue provider poll timer if needed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn queue_provider_poll_timer_if_needed(
        &mut self,
        generation: u64,
        delay_ms: u64,
    ) -> Result<bool> {
        let transition = self.service.provider_poll_timer_transition(
            self.timers.provider_poll.is_some(),
            generation,
            delay_ms,
        );
        let queued = !transition.side_effects.is_empty();
        self.queue_runtime_side_effects(transition.side_effects)?;
        Ok(queued)
    }

    /// Runs the provider dispatch is already queued operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_dispatch_is_already_queued(&self, turn_id: &str) -> bool {
        self.side_effects.iter().any(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::DispatchAgentProvider {
                    turn_id: queued_turn_id,
                    ..
                } if queued_turn_id == turn_id
            )
        })
    }

    /// Runs the apply runtime client event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_runtime_client_event(
        &mut self,
        client_event: ClientEvent,
    ) -> Result<RuntimeTransition> {
        match client_event {
            event @ (ClientEvent::Resize { .. } | ClientEvent::Disconnected { .. }) => {
                self.service.apply_client_lifecycle_transition(event)
            }
            ClientEvent::ResizeSignal { client_id } => Ok(self
                .apply_runtime_client_render_signal_event(
                    client_id,
                    RenderInvalidationReason::Resize,
                )),
            ClientEvent::OutputReady { client_id } => Ok(self
                .apply_runtime_client_render_signal_event(
                    client_id,
                    RenderInvalidationReason::FullRedraw,
                )),
            ClientEvent::Input { client_id, bytes } => self
                .service
                .apply_client_input_transition(&client_id, &bytes),
        }
    }

    /// Runs the apply runtime client render signal event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_runtime_client_render_signal_event(
        &self,
        client_id: ClientId,
        reason: RenderInvalidationReason,
    ) -> RuntimeTransition {
        if !self
            .service
            .session()
            .clients()
            .iter()
            .any(|client| client.id == client_id && client.state == ClientState::Attached)
        {
            return RuntimeTransition::default();
        }
        RuntimeTransition {
            applied: true,
            side_effects: vec![RuntimeSideEffect::RenderClient { client_id, reason }],
        }
    }

    /// Runs the apply runtime client input event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// Runs the attached client size operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn attached_client_size(&self, client_id: &ClientId) -> Result<Option<Size>> {
        let Some(client) = self
            .service
            .session()
            .clients()
            .iter()
            .find(|client| client.id == *client_id && client.state == ClientState::Attached)
        else {
            return Ok(None);
        };
        if let Some(terminal) = client.terminal.as_ref() {
            return Size::new(terminal.columns, terminal.rows).map(Some);
        }
        Ok(self
            .service
            .session()
            .active_window()
            .map(|window| window.size))
    }

    /// Runs the client step application side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// Runs the apply runtime hook event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_runtime_hook_event(
        &mut self,
        hook_event: AsyncHookEvent,
    ) -> Result<RuntimeTransition> {
        self.service.apply_hook_transition(hook_event)
    }

    /// Runs the apply runtime persistence event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_runtime_persistence_event(
        &mut self,
        persistence_event: PersistenceEvent,
    ) -> Result<RuntimeTransition> {
        self.service.apply_persistence_transition(persistence_event)
    }

    /// Runs the apply runtime agent provider event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn apply_runtime_agent_provider_event(
        &mut self,
        provider_event: AgentProviderEvent,
    ) -> Result<RuntimeTransition> {
        match provider_event {
            AgentProviderEvent::Failed {
                agent_id,
                turn_id,
                kind,
                message,
                provider_failure_json,
                provider_raw_text,
            } => {
                let claim_cancellations = self.provider_claim_cancel_timer_side_effects(&turn_id);
                self.service.clear_claimed_agent_provider_task(&turn_id);
                let retry_class = provider_error_retry_class_from_parts(
                    provider_event_error_kind(&kind),
                    &message,
                    provider_failure_json.as_deref(),
                );
                let error = provider_event_error_from_parts(
                    &kind,
                    &message,
                    provider_failure_json.as_deref(),
                    provider_raw_text.as_deref(),
                );
                if let Some(mut application) =
                    self.service.schedule_agent_provider_retry_transition(
                        &agent_id,
                        &turn_id,
                        retry_class,
                        &error,
                    )?
                {
                    if application.applied {
                        application
                            .side_effects
                            .extend(self.render_side_effects(RenderInvalidationReason::FullRedraw));
                    } else {
                        self.timers.provider_retry.remove(turn_id.as_str());
                    }
                    application.side_effects.extend(claim_cancellations);
                    return Ok(application);
                }
                if matches!(retry_class, ProviderErrorRetryClass::OutputLimit)
                    && !self
                        .provider_output_limit_compaction_turns
                        .contains(turn_id.as_str())
                    && self
                        .service
                        .queue_agent_output_limit_recovery_compaction(&agent_id, &turn_id, &error)?
                {
                    self.service
                        .clear_agent_provider_retry_attempt(turn_id.as_str());
                    self.timers.provider_retry.remove(turn_id.as_str());
                    self.provider_output_limit_compaction_turns
                        .insert(turn_id.clone());
                    let mut side_effects =
                        self.render_side_effects(RenderInvalidationReason::FullRedraw);
                    side_effects.extend(self.pending_provider_dispatch_side_effects()?);
                    side_effects.extend(claim_cancellations);
                    return Ok(RuntimeTransition {
                        applied: true,
                        side_effects,
                    });
                }
                self.service
                    .clear_agent_provider_retry_attempt(turn_id.as_str());
                self.timers.provider_retry.remove(turn_id.as_str());
                self.provider_output_limit_compaction_turns
                    .remove(turn_id.as_str());
                let mut transition = self.service.apply_agent_provider_failed_transition(
                    &agent_id,
                    &turn_id,
                    &kind,
                    &message,
                    provider_failure_json.as_deref(),
                    provider_raw_text.as_deref(),
                )?;
                transition.side_effects.extend(claim_cancellations);
                Ok(transition)
            }
            AgentProviderEvent::OutputProgress {
                agent_id: _,
                turn_id: _,
                pane_id,
                action_id: _,
                lines,
            } => Ok(self
                .service
                .apply_agent_provider_output_progress_transition(&pane_id, &lines)),
            AgentProviderEvent::Completed {
                agent_id,
                turn_id,
                execution,
            } => {
                let claim_cancellations = self.provider_claim_cancel_timer_side_effects(&turn_id);
                self.service.clear_claimed_agent_provider_task(&turn_id);
                self.service
                    .clear_agent_provider_retry_attempt(turn_id.as_str());
                self.timers.provider_retry.remove(turn_id.as_str());
                self.provider_output_limit_compaction_turns
                    .remove(turn_id.as_str());
                let mut transition = self
                    .service
                    .apply_agent_provider_completed_transition(&agent_id, &turn_id, *execution)
                    .await?;
                if transition.applied {
                    transition
                        .side_effects
                        .extend(self.pending_provider_dispatch_side_effects()?);
                }
                transition.side_effects.extend(claim_cancellations);
                Ok(transition)
            }
        }
    }

    /// Runs the apply runtime shutdown event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_runtime_shutdown_event(
        &mut self,
        shutdown: ShutdownEvent,
    ) -> Result<RuntimeTransition> {
        self.service.apply_shutdown_transition(shutdown)
    }

    /// Applies timer scheduling bookkeeping in emitted side-effect order.
    ///
    /// A cancellation followed by a schedule for the same generation must
    /// leave that timer active, while a schedule followed by cancellation must
    /// remove it. Keeping this ordering here mirrors the timer worker contract.
    fn track_runtime_timer_side_effect(&mut self, effect: &RuntimeSideEffect) {
        let (key, scheduled) = match effect {
            RuntimeSideEffect::ScheduleTimer { key, .. } => (key, true),
            RuntimeSideEffect::CancelTimer { key } => (key, false),
            _ => return,
        };
        match key.kind {
            RuntimeTimerKind::ShellTransaction
            | RuntimeTimerKind::ReadinessProbe
            | RuntimeTimerKind::Bootstrap
            | RuntimeTimerKind::FocusedShellHook => {
                if scheduled {
                    self.timers.shell_transactions.insert(key.clone());
                } else {
                    self.timers.shell_transactions.remove(key);
                }
            }
            RuntimeTimerKind::IdleCleanup => {
                if scheduled {
                    self.timers.idle_cleanup = Some(key.clone());
                } else if self.timers.idle_cleanup.as_ref() == Some(key) {
                    self.timers.idle_cleanup = None;
                }
            }
            RuntimeTimerKind::ResizeDebounce => {
                if scheduled {
                    self.timers.resize_debounce.insert(key.clone());
                } else {
                    self.timers.resize_debounce.remove(key);
                }
            }
            RuntimeTimerKind::CursorBlink => {
                Self::track_owned_timer_key(&mut self.timers.cursor_blink, key, scheduled);
            }
            RuntimeTimerKind::StatusRefresh => {
                Self::track_owned_timer_key(&mut self.timers.status_refresh, key, scheduled);
            }
            RuntimeTimerKind::ProviderPoll => {
                if scheduled {
                    self.timers.provider_poll = Some(key.clone());
                } else if self.timers.provider_poll.as_ref() == Some(key) {
                    self.timers.provider_poll = None;
                }
            }
            RuntimeTimerKind::ProviderRetry => {
                Self::track_owned_timer_key(&mut self.timers.provider_retry, key, scheduled);
            }
            RuntimeTimerKind::ProviderClaim => {
                Self::track_owned_timer_key(&mut self.timers.provider_claim, key, scheduled);
            }
            RuntimeTimerKind::PanePipeHealth => {
                Self::track_owned_timer_key(&mut self.timers.pane_pipe_health, key, scheduled);
            }
        }
    }

    /// Updates one owner-keyed timer generation without discarding effect order.
    fn track_owned_timer_key(
        timers: &mut std::collections::HashMap<String, RuntimeTimerKey>,
        key: &RuntimeTimerKey,
        scheduled: bool,
    ) {
        if scheduled {
            timers.insert(key.owner_id.clone(), key.clone());
        } else if timers.get(key.owner_id.as_str()) == Some(key) {
            timers.remove(key.owner_id.as_str());
        }
    }

    /// Runs the queue runtime side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn queue_runtime_side_effects(&mut self, side_effects: Vec<RuntimeSideEffect>) -> Result<()> {
        let (side_effects, coalesced) =
            coalesce_output_side_effects_for_enqueue(&mut self.side_effects, side_effects);
        if self.side_effects.len().saturating_add(side_effects.len()) > self.side_effect_buffer {
            return Err(MezError::invalid_state(format!(
                "async runtime side-effect queue is full: queued={} incoming={} capacity={} queued_kinds={} incoming_kinds={}",
                self.side_effects.len(),
                side_effects.len(),
                self.side_effect_buffer,
                runtime_side_effect_kind_summary(self.side_effects.iter()),
                runtime_side_effect_kind_summary(side_effects.iter())
            )));
        }
        let timer_schedules = side_effects
            .iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::ScheduleTimer { .. }))
            .count();
        let timer_cancellations = side_effects
            .iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::CancelTimer { .. }))
            .count();
        let queued = side_effects.len();
        let should_notify = !side_effects.is_empty();
        for effect in &side_effects {
            self.track_runtime_timer_side_effect(effect);
        }
        for effect in side_effects {
            self.enqueue_runtime_side_effect(effect);
        }
        if should_notify {
            self.metrics.runtime_side_effects_queued = self
                .metrics
                .runtime_side_effects_queued
                .saturating_add(u64::try_from(queued).unwrap_or(u64::MAX));
            self.metrics.runtime_timer_schedules_queued = self
                .metrics
                .runtime_timer_schedules_queued
                .saturating_add(u64::try_from(timer_schedules).unwrap_or(u64::MAX));
            self.metrics.runtime_timer_cancellations_queued = self
                .metrics
                .runtime_timer_cancellations_queued
                .saturating_add(u64::try_from(timer_cancellations).unwrap_or(u64::MAX));
        }
        self.metrics
            .runtime_side_effect_enqueue_sizes
            .record(u64::try_from(queued).unwrap_or(u64::MAX));
        self.metrics.render_invalidations_coalesced = self
            .metrics
            .render_invalidations_coalesced
            .saturating_add(u64::try_from(coalesced).unwrap_or(u64::MAX));
        self.metrics.side_effect_queue_depth = self.side_effects.len();
        self.metrics.side_effect_queue_high_water = self
            .metrics
            .side_effect_queue_high_water
            .max(self.side_effects.len());
        self.metrics
            .side_effect_queue_depth_samples
            .record(u64::try_from(self.side_effects.len()).unwrap_or(u64::MAX));
        if should_notify {
            self.notify_side_effect_delivery();
        }
        Ok(())
    }

    /// Enqueues one runtime side effect, preserving priority pane input order.
    fn enqueue_runtime_side_effect(&mut self, effect: RuntimeSideEffect) {
        match &effect {
            RuntimeSideEffect::WritePaneInputPriority { pane_id, .. } => {
                let insert_at = self
                    .side_effects
                    .iter()
                    .position(|queued| pane_io_side_effect_targets_pane(queued, pane_id))
                    .unwrap_or(self.side_effects.len());
                self.side_effects.insert(insert_at, effect);
            }
            _ => self.side_effects.push_back(effect),
        }
    }

    /// Runs the queue shell transaction timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn queue_shell_transaction_timer_side_effects(&mut self) -> Result<usize> {
        let side_effects = self.shell_transaction_timer_side_effects();
        let queued = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(queued)
    }

    /// Runs the ensure client render timers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn ensure_client_render_timers(&mut self, client_id: &ClientId) -> Result<usize> {
        let generation_base_ms = async_runtime_current_unix_millis();
        let mut side_effects = self
            .service
            .client_cursor_blink_timer_transition(
                client_id.as_str(),
                self.timers.cursor_blink.get(client_id.as_str()).cloned(),
                generation_base_ms,
            )?
            .side_effects;
        side_effects.extend(
            self.service
                .client_status_refresh_timer_transition(
                    client_id.as_str(),
                    self.timers.status_refresh.get(client_id.as_str()).cloned(),
                    generation_base_ms,
                )?
                .side_effects,
        );
        let queued = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(queued)
    }

    /// Runs the shell transaction timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn shell_transaction_timer_side_effects(&mut self) -> Vec<RuntimeSideEffect> {
        self.service
            .shell_transaction_timer_transition(
                &self.timers.shell_transactions,
                async_runtime_current_unix_millis(),
            )
            .side_effects
            .into_iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::ScheduleTimer { .. }))
            .collect()
    }

    /// Runs the cancel stale shell transaction timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn cancel_stale_shell_transaction_timer_side_effects(&mut self) -> Vec<RuntimeSideEffect> {
        self.service
            .shell_transaction_timer_transition(
                &self.timers.shell_transactions,
                async_runtime_current_unix_millis(),
            )
            .side_effects
            .into_iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::CancelTimer { .. }))
            .collect()
    }

    /// Runs the idle cleanup timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn idle_cleanup_timer_side_effects(&self) -> Vec<RuntimeSideEffect> {
        let actor_progress_turn_ids = self.actor_owned_agent_progress_turn_ids();
        self.idle_cleanup_timer_side_effects_with_actor_progress(&actor_progress_turn_ids)
    }

    /// Returns idle-cleanup timer side effects while honoring actor-owned
    /// progress such as delayed provider retry timers.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns whose progress is represented
    ///   by async actor state rather than service-owned queues.
    fn idle_cleanup_timer_side_effects_with_actor_progress(
        &self,
        actor_progress_turn_ids: &std::collections::BTreeSet<String>,
    ) -> Vec<RuntimeSideEffect> {
        self.service
            .idle_cleanup_timer_transition_with_actor_progress(
                actor_progress_turn_ids,
                self.timers.idle_cleanup.is_some(),
                async_runtime_current_unix_millis(),
                async_runtime_duration_millis(DEFAULT_ASYNC_IDLE_CLEANUP_INTERVAL),
                DEFAULT_SHELL_RECOVERY_INTERVAL_MS,
            )
            .side_effects
    }

    /// Runs the cursor blink timer side effects for client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn cursor_blink_timer_side_effects_for_client(
        &mut self,
        client_id: &str,
        generation_base_ms: u64,
    ) -> Result<Vec<RuntimeSideEffect>> {
        Ok(self
            .service
            .client_cursor_blink_timer_transition(
                client_id,
                self.timers.cursor_blink.get(client_id).cloned(),
                generation_base_ms,
            )?
            .side_effects)
    }

    /// Runs the status refresh timer side effects for client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn status_refresh_timer_side_effects_for_client(
        &mut self,
        client_id: &str,
        generation_base_ms: u64,
    ) -> Result<Vec<RuntimeSideEffect>> {
        Ok(self
            .service
            .client_status_refresh_timer_transition(
                client_id,
                self.timers.status_refresh.get(client_id).cloned(),
                generation_base_ms,
            )?
            .side_effects)
    }

    /// Runs the pane pipe health timer side effects for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_pipe_health_timer_side_effects_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Result<Vec<RuntimeSideEffect>> {
        let next_generation = self
            .timers
            .next_pane_pipe_health_generation
            .saturating_add(1);
        let transition = self.service.pane_pipe_health_timer_transition(
            pane_id,
            self.timers.pane_pipe_health.get(pane_id).cloned(),
            next_generation,
            DEFAULT_PANE_PIPE_HEALTH_DELAY_MS,
        )?;
        if transition.side_effects.iter().any(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::PanePipeHealth
            )
        }) {
            self.timers.next_pane_pipe_health_generation = next_generation;
        }
        Ok(transition.side_effects)
    }

    /// Runs the command pane pipe health timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn command_pane_pipe_health_timer_side_effects(&mut self) -> Result<Vec<RuntimeSideEffect>> {
        let pane_ids = self.service.active_command_pane_pipe_ids();
        let mut side_effects = Vec::new();
        for pane_id in pane_ids {
            side_effects.extend(self.pane_pipe_health_timer_side_effects_for_pane(&pane_id)?);
        }
        Ok(side_effects)
    }

    /// Runs the queue command pane pipe health timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn queue_command_pane_pipe_health_timer_side_effects(&mut self) -> Result<usize> {
        let side_effects = self.command_pane_pipe_health_timer_side_effects()?;
        let count = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(count)
    }

    /// Runs the drain runtime side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drain_runtime_side_effects(&mut self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime side-effect drain limit must be greater than zero",
            ));
        }
        let drain_count = limit.min(self.side_effects.len());
        let effects = self.side_effects.drain(..drain_count).collect();
        self.record_side_effect_drain(drain_count);
        Ok(effects)
    }

    /// Records a side-effect drain and wakes peers when retained work remains.
    ///
    /// The side-effect queue is shared by several filtered workers. One worker
    /// can drain its own work and retain work for another worker, consuming the
    /// original queue notification in the process. Re-notifying only after real
    /// drain progress keeps retained work responsive without spinning workers
    /// that inspected an unrelated non-empty queue.
    fn record_side_effect_drain(&mut self, drained: usize) {
        self.metrics.runtime_side_effects_drained = self
            .metrics
            .runtime_side_effects_drained
            .saturating_add(u64::try_from(drained).unwrap_or(u64::MAX));
        self.metrics
            .runtime_side_effect_drain_sizes
            .record(u64::try_from(drained).unwrap_or(u64::MAX));
        self.metrics.side_effect_queue_depth = self.side_effects.len();
        self.metrics
            .side_effect_queue_depth_samples
            .record(u64::try_from(self.side_effects.len()).unwrap_or(u64::MAX));
        if drained > 0 && !self.side_effects.is_empty() {
            self.notify_side_effect_delivery();
        }
    }

    /// Runs the drain agent provider dispatch side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drain_agent_provider_dispatch_side_effects(
        &mut self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime provider dispatch drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit
                && matches!(
                    effect,
                    RuntimeSideEffect::DispatchAgentProvider { .. }
                        | RuntimeSideEffect::DispatchAgentCompaction { .. }
                        | RuntimeSideEffect::DispatchAgentRemember { .. }
                )
            {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Runs the drain render side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drain_render_side_effects(&mut self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime render side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained: Vec<(ClientId, RenderInvalidationReason)> = Vec::new();
        let mut removed = 0usize;
        let mut coalesced = 0usize;
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            match effect {
                RuntimeSideEffect::RenderClient { client_id, reason } => {
                    if let Some((_, retained_reason)) = drained
                        .iter_mut()
                        .find(|(drained_client_id, _)| drained_client_id == &client_id)
                    {
                        *retained_reason =
                            coalesce_render_invalidation_reason(*retained_reason, reason);
                        removed = removed.saturating_add(1);
                        coalesced = coalesced.saturating_add(1);
                    } else if drained.len() < limit {
                        drained.push((client_id, reason));
                        removed = removed.saturating_add(1);
                    } else {
                        retained.push_back(RuntimeSideEffect::RenderClient { client_id, reason });
                    }
                }
                effect => retained.push_back(effect),
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(removed);
        self.metrics.render_invalidations_coalesced = self
            .metrics
            .render_invalidations_coalesced
            .saturating_add(u64::try_from(coalesced).unwrap_or(u64::MAX));
        Ok(drained
            .into_iter()
            .map(|(client_id, reason)| RuntimeSideEffect::RenderClient { client_id, reason })
            .collect())
    }

    /// Runs the drain render side effects for client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drain_render_side_effects_for_client(
        &mut self,
        client_id: &ClientId,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime render side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained: Vec<RenderInvalidationReason> = Vec::new();
        let mut removed = 0usize;
        let mut coalesced = 0usize;
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            match effect {
                RuntimeSideEffect::RenderClient {
                    client_id: effect_client_id,
                    reason,
                } if &effect_client_id == client_id => {
                    if let Some(retained_reason) = drained.first_mut() {
                        *retained_reason =
                            coalesce_render_invalidation_reason(*retained_reason, reason);
                        coalesced = coalesced.saturating_add(1);
                    } else if drained.len() < limit {
                        drained.push(reason);
                    } else {
                        retained.push_back(RuntimeSideEffect::RenderClient {
                            client_id: effect_client_id,
                            reason,
                        });
                        continue;
                    }
                    removed = removed.saturating_add(1);
                }
                effect => retained.push_back(effect),
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(removed);
        self.metrics.render_invalidations_coalesced = self
            .metrics
            .render_invalidations_coalesced
            .saturating_add(u64::try_from(coalesced).unwrap_or(u64::MAX));
        Ok(drained
            .into_iter()
            .map(|reason| RuntimeSideEffect::RenderClient {
                client_id: client_id.clone(),
                reason,
            })
            .collect())
    }

    /// Runs the drain client output flush side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drain_client_output_flush_side_effects(
        &mut self,
        client_id: Option<&ClientId>,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime client output flush drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit {
                match &effect {
                    RuntimeSideEffect::FlushClientOutput {
                        client_id: effect_client_id,
                        ..
                    } if client_id.is_none_or(|target| target == effect_client_id) => {
                        drained.push(effect);
                    }
                    _ => retained.push_back(effect),
                }
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Runs the drain timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drain_timer_side_effects(&mut self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime timer side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit && timer_side_effect_targets_timer_worker(&effect) {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Runs the drain persistence side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drain_persistence_side_effects(&mut self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime persistence side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit
                && matches!(
                    effect,
                    RuntimeSideEffect::Persist { .. }
                        | RuntimeSideEffect::PersistAuditLog { .. }
                        | RuntimeSideEffect::PersistTranscriptEntries { .. }
                        | RuntimeSideEffect::PersistPromptHistory { .. }
                        | RuntimeSideEffect::PersistCommandPromptHistory { .. }
                        | RuntimeSideEffect::PersistRegistry { .. }
                )
            {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Runs the drain hook side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drain_hook_side_effects(&mut self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime hook side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit && matches!(effect, RuntimeSideEffect::RunProgramHook { .. }) {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Runs the drain pane io side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drain_pane_io_side_effects(
        &mut self,
        pane_id: &str,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        if pane_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "async runtime pane I/O side-effect drain pane id must not be empty",
            ));
        }
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async runtime pane I/O side-effect drain limit must be greater than zero",
            ));
        }
        let mut drained = Vec::new();
        let mut retained = VecDeque::with_capacity(self.side_effects.len());
        while let Some(effect) = self.side_effects.pop_front() {
            if drained.len() < limit && pane_io_side_effect_targets_pane(&effect, pane_id) {
                drained.push(effect);
            } else {
                retained.push_back(effect);
            }
        }
        self.side_effects = retained;
        self.record_side_effect_drain(drained.len());
        Ok(drained)
    }

    /// Runs the render client side effect operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn render_client_side_effect(
        &mut self,
        client_id: ClientId,
        config: TerminalClientLoopConfig,
        status: Option<ClientStatusLine>,
        cursor_blink_elapsed_ms: u64,
    ) -> Result<Option<AsyncRenderedClientFlush>> {
        let Some(client_size) = self.attached_client_size(&client_id)? else {
            return Ok(None);
        };
        let role = if self.service.session().primary_client_id() == Some(&client_id) {
            ClientViewRole::Primary
        } else {
            ClientViewRole::Observer
        };
        let config = self.service.terminal_client_loop_config(config)?;
        let Some(view) =
            self.service
                .render_client_view_with_resolved_config(role, client_size, &config)?
        else {
            return Ok(None);
        };
        let cursor_visible = view.cursor_visible;
        let cursor_row = view.cursor_row;
        let cursor_column = view.cursor_column;
        let application_keypad = view.application_keypad;
        let bracketed_paste = view.bracketed_paste;
        let (lines, line_style_spans) =
            compose_client_presentation_with_styles(&view, status.as_ref());
        let flush = AsyncRenderedClientFlush {
            client_id,
            lines,
            line_style_spans,
            modes: AttachedTerminalOutputModes {
                application_keypad,
                bracketed_paste,
                focus_events: view.focus_events,
                alternate_screen: view.alternate_screen,
                host_mouse_reporting: config.mouse_policy.enabled,
                cursor_style: config.cursor_style,
                cursor_blink: config.cursor_blink,
                cursor_blink_interval_ms: config.cursor_blink_interval_ms,
                cursor_blink_elapsed_ms,
                animation_refresh_interval_ms: view.animation_refresh_interval_ms,
                cursor_visible,
                cursor_row,
                cursor_column,
            },
        };
        let generation_base_ms = async_runtime_current_unix_millis();
        let mut timer_effects = self.cursor_blink_timer_side_effects_for_client(
            flush.client_id.as_str(),
            generation_base_ms,
        )?;
        timer_effects.extend(self.status_refresh_timer_side_effects_for_client(
            flush.client_id.as_str(),
            generation_base_ms,
        )?);
        self.queue_runtime_side_effects(timer_effects)?;
        Ok(Some(flush))
    }

    /// Runs the render side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn render_side_effects(&self, reason: RenderInvalidationReason) -> Vec<RuntimeSideEffect> {
        self.service
            .session()
            .clients()
            .iter()
            .filter(|client| client.state == ClientState::Attached)
            .map(|client| RuntimeSideEffect::RenderClient {
                client_id: client.id.clone(),
                reason,
            })
            .collect()
    }
}

/// Coalesces bursty output effects before they enter the bounded actor queue.
///
/// Render invalidations and full client-output frames are level-triggered:
/// multiple pending requests for the same client can be represented by one
/// request with the latest frame or strongest invalidation reason. Coalescing
/// at enqueue time prevents pane-output bursts from filling the shared
/// side-effect queue with stale repaint work before the attached client or
/// render worker can drain it.
fn coalesce_output_side_effects_for_enqueue(
    queued: &mut VecDeque<RuntimeSideEffect>,
    side_effects: Vec<RuntimeSideEffect>,
) -> (Vec<RuntimeSideEffect>, usize) {
    let mut retained = Vec::new();
    let mut coalesced = 0usize;
    for effect in side_effects {
        match effect {
            RuntimeSideEffect::RenderClient { client_id, reason } => {
                if coalesce_render_side_effect_into_queue(queued, &client_id, reason)
                    || coalesce_render_side_effect_into_vec(&mut retained, &client_id, reason)
                {
                    coalesced = coalesced.saturating_add(1);
                } else {
                    retained.push(RuntimeSideEffect::RenderClient { client_id, reason });
                }
            }
            RuntimeSideEffect::FlushClientOutput {
                client_id,
                lines,
                line_style_spans,
                modes,
            } => {
                let mut effect = Some(RuntimeSideEffect::FlushClientOutput {
                    client_id: client_id.clone(),
                    lines,
                    line_style_spans,
                    modes,
                });
                if coalesce_flush_side_effect_into_queue(queued, &client_id, &mut effect)
                    || coalesce_flush_side_effect_into_vec(&mut retained, &client_id, &mut effect)
                {
                    coalesced = coalesced.saturating_add(1);
                } else if let Some(effect) = effect {
                    retained.push(effect);
                }
            }
            RuntimeSideEffect::PersistRegistry { registry, update } => {
                let session_id = registry_update_session_id(&update).to_string();
                let mut effect = Some(RuntimeSideEffect::PersistRegistry {
                    registry: registry.clone(),
                    update,
                });
                if coalesce_registry_side_effect_into_queue(
                    queued,
                    &registry,
                    &session_id,
                    &mut effect,
                ) || coalesce_registry_side_effect_into_vec(
                    &mut retained,
                    &registry,
                    &session_id,
                    &mut effect,
                ) {
                    coalesced = coalesced.saturating_add(1);
                } else if let Some(effect) = effect {
                    retained.push(effect);
                }
            }
            effect => retained.push(effect),
        }
    }
    (retained, coalesced)
}

/// Merges one render invalidation into an existing queued invalidation.
fn coalesce_render_side_effect_into_queue(
    queued: &mut VecDeque<RuntimeSideEffect>,
    client_id: &ClientId,
    reason: RenderInvalidationReason,
) -> bool {
    queued.iter_mut().any(|effect| {
        let RuntimeSideEffect::RenderClient {
            client_id: queued_client_id,
            reason: queued_reason,
        } = effect
        else {
            return false;
        };
        if queued_client_id != client_id {
            return false;
        }
        *queued_reason = coalesce_render_invalidation_reason(*queued_reason, reason);
        true
    })
}

/// Merges one render invalidation into a same-batch invalidation.
fn coalesce_render_side_effect_into_vec(
    retained: &mut [RuntimeSideEffect],
    client_id: &ClientId,
    reason: RenderInvalidationReason,
) -> bool {
    retained.iter_mut().any(|effect| {
        let RuntimeSideEffect::RenderClient {
            client_id: retained_client_id,
            reason: retained_reason,
        } = effect
        else {
            return false;
        };
        if retained_client_id != client_id {
            return false;
        }
        *retained_reason = coalesce_render_invalidation_reason(*retained_reason, reason);
        true
    })
}

/// Replaces a pending client-output flush already queued for the same client.
fn coalesce_flush_side_effect_into_queue(
    queued: &mut VecDeque<RuntimeSideEffect>,
    client_id: &ClientId,
    effect: &mut Option<RuntimeSideEffect>,
) -> bool {
    queued.iter_mut().any(|queued_effect| {
        let RuntimeSideEffect::FlushClientOutput {
            client_id: queued_client_id,
            ..
        } = queued_effect
        else {
            return false;
        };
        if queued_client_id != client_id {
            return false;
        }
        let Some(replacement) = effect.take() else {
            return false;
        };
        *queued_effect = replacement;
        true
    })
}

/// Replaces a same-batch client-output flush for the same client.
fn coalesce_flush_side_effect_into_vec(
    retained: &mut [RuntimeSideEffect],
    client_id: &ClientId,
    effect: &mut Option<RuntimeSideEffect>,
) -> bool {
    retained.iter_mut().any(|retained_effect| {
        let RuntimeSideEffect::FlushClientOutput {
            client_id: retained_client_id,
            ..
        } = retained_effect
        else {
            return false;
        };
        if retained_client_id != client_id {
            return false;
        }
        let Some(replacement) = effect.take() else {
            return false;
        };
        *retained_effect = replacement;
        true
    })
}

/// Replaces a pending registry persistence effect for the same session.
fn coalesce_registry_side_effect_into_queue(
    queued: &mut VecDeque<RuntimeSideEffect>,
    registry: &crate::registry::SessionRegistry,
    session_id: &str,
    effect: &mut Option<RuntimeSideEffect>,
) -> bool {
    queued.iter_mut().any(|queued_effect| {
        let RuntimeSideEffect::PersistRegistry {
            registry: queued_registry,
            update,
        } = queued_effect
        else {
            return false;
        };
        if queued_registry != registry || registry_update_session_id(update) != session_id {
            return false;
        }
        let Some(replacement) = effect.take() else {
            return false;
        };
        *queued_effect = replacement;
        true
    })
}

/// Replaces a same-batch registry persistence effect for the same session.
fn coalesce_registry_side_effect_into_vec(
    retained: &mut [RuntimeSideEffect],
    registry: &crate::registry::SessionRegistry,
    session_id: &str,
    effect: &mut Option<RuntimeSideEffect>,
) -> bool {
    retained.iter_mut().any(|retained_effect| {
        let RuntimeSideEffect::PersistRegistry {
            registry: retained_registry,
            update,
        } = retained_effect
        else {
            return false;
        };
        if retained_registry != registry || registry_update_session_id(update) != session_id {
            return false;
        }
        let Some(replacement) = effect.take() else {
            return false;
        };
        *retained_effect = replacement;
        true
    })
}

/// Returns the session targeted by a registry persistence plan.
fn registry_update_session_id(update: &crate::runtime::RuntimeRegistryUpdatePlan) -> &str {
    match update {
        crate::runtime::RuntimeRegistryUpdatePlan::Upsert(record) => &record.session_id,
        crate::runtime::RuntimeRegistryUpdatePlan::Remove { session_id } => session_id,
    }
}

/// Returns whether applying an event can change the session registry record.
fn runtime_event_requires_registry_persistence(event: &RuntimeEvent) -> bool {
    match event {
        RuntimeEvent::Pane(
            PaneEvent::Output { .. }
            | PaneEvent::InputWritten { .. }
            | PaneEvent::WriteFailed { .. }
            | PaneEvent::Resized { .. }
            | PaneEvent::ForegroundProcess { .. },
        )
        | RuntimeEvent::Hook(_)
        | RuntimeEvent::Persistence(_)
        | RuntimeEvent::Timer(_) => false,
        RuntimeEvent::Client(_)
        | RuntimeEvent::Process(_)
        | RuntimeEvent::AgentProvider(_)
        | RuntimeEvent::AgentCompaction(_)
        | RuntimeEvent::AgentRemember(_)
        | RuntimeEvent::Shutdown(_) => true,
    }
}

/// Returns the owning turn for a provider retry timer side effect.
///
/// Retry timer side effects are created before they are registered in the
/// actor's scheduled-timer map. Reconciliation inspects the not-yet-queued
/// side-effect list through this helper so retryable provider failures are not
/// mistaken for unreachable running turns.
fn provider_retry_timer_side_effect_turn_id(effect: &RuntimeSideEffect) -> Option<String> {
    match effect {
        RuntimeSideEffect::ScheduleTimer { key, .. }
            if key.kind == RuntimeTimerKind::ProviderRetry =>
        {
            Some(key.owner_id.clone())
        }
        _ => None,
    }
}

/// Runs the runtime client step application applied operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the coalesce render invalidation reason operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn coalesce_render_invalidation_reason(
    current: RenderInvalidationReason,
    incoming: RenderInvalidationReason,
) -> RenderInvalidationReason {
    if render_invalidation_reason_priority(incoming) >= render_invalidation_reason_priority(current)
    {
        incoming
    } else {
        current
    }
}

/// Runs the render invalidation reason priority operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn render_invalidation_reason_priority(reason: RenderInvalidationReason) -> u8 {
    match reason {
        RenderInvalidationReason::CursorBlink => 0,
        RenderInvalidationReason::StatusLine => 1,
        RenderInvalidationReason::PaneOutput => 2,
        RenderInvalidationReason::AgentPrompt => 3,
        RenderInvalidationReason::Overlay => 4,
        RenderInvalidationReason::Configuration => 5,
        RenderInvalidationReason::Resize => 6,
        RenderInvalidationReason::Layout => 7,
        RenderInvalidationReason::FullRedraw => 8,
    }
}

/// Runs the pane io side effect targets pane operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_io_side_effect_targets_pane(effect: &RuntimeSideEffect, target_pane_id: &str) -> bool {
    match effect {
        RuntimeSideEffect::WritePaneInput { pane_id, .. }
        | RuntimeSideEffect::WritePaneInputPriority { pane_id, .. }
        | RuntimeSideEffect::ResizePane { pane_id, .. }
        | RuntimeSideEffect::TerminatePane { pane_id, .. } => pane_id == target_pane_id,
        _ => false,
    }
}

/// Runs the timer side effect targets timer worker operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn timer_side_effect_targets_timer_worker(effect: &RuntimeSideEffect) -> bool {
    matches!(
        effect,
        RuntimeSideEffect::ScheduleTimer { .. } | RuntimeSideEffect::CancelTimer { .. }
    )
}

/// Builds a compact count summary for queued side-effect diagnostics.
fn runtime_side_effect_kind_summary<'a>(
    effects: impl Iterator<Item = &'a RuntimeSideEffect>,
) -> String {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    for effect in effects {
        let kind = runtime_side_effect_kind(effect);
        if let Some((_, count)) = counts.iter_mut().find(|(existing, _)| *existing == kind) {
            *count = count.saturating_add(1);
        } else {
            counts.push((kind, 1));
        }
    }
    if counts.is_empty() {
        return "none".to_string();
    }
    counts
        .into_iter()
        .map(|(kind, count)| format!("{kind}:{count}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Returns a stable diagnostic family for one queued side effect.
fn runtime_side_effect_kind(effect: &RuntimeSideEffect) -> &'static str {
    match effect {
        RuntimeSideEffect::WritePaneInput { .. } => "write-pane-input",
        RuntimeSideEffect::WritePaneInputPriority { .. } => "write-pane-input-priority",
        RuntimeSideEffect::ResizePane { .. } => "resize-pane",
        RuntimeSideEffect::TerminatePane { .. } => "terminate-pane",
        RuntimeSideEffect::RenderClient { .. } => "render-client",
        RuntimeSideEffect::ScheduleTimer { .. } => "schedule-timer",
        RuntimeSideEffect::CancelTimer { .. } => "cancel-timer",
        RuntimeSideEffect::DispatchAgentProvider { .. } => "dispatch-agent-provider",
        RuntimeSideEffect::DispatchAgentCompaction { .. } => "dispatch-agent-compaction",
        RuntimeSideEffect::DispatchAgentRemember { .. } => "dispatch-agent-remember",
        RuntimeSideEffect::RunProgramHook { .. } => "run-program-hook",
        RuntimeSideEffect::Persist { .. } => "persist",
        RuntimeSideEffect::PersistAuditLog { .. } => "persist-audit-log",
        RuntimeSideEffect::PersistTranscriptEntries { .. } => "persist-transcript",
        RuntimeSideEffect::PersistPromptHistory { .. } => "persist-prompt-history",
        RuntimeSideEffect::PersistCommandPromptHistory { .. } => "persist-command-prompt-history",
        RuntimeSideEffect::PersistRegistry { .. } => "persist-registry",
        RuntimeSideEffect::FlushClientOutput { .. } => "flush-client-output",
    }
}

/// Runs the runtime timer kind is shell transaction operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_timer_kind_is_shell_transaction(kind: RuntimeTimerKind) -> bool {
    matches!(
        kind,
        RuntimeTimerKind::ShellTransaction
            | RuntimeTimerKind::ReadinessProbe
            | RuntimeTimerKind::Bootstrap
            | RuntimeTimerKind::FocusedShellHook
    )
}

/// Runs the shell transaction schedule timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the shell transaction cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the idle cleanup schedule timer key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the idle cleanup cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the resize debounce schedule timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the resize debounce cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the cursor blink schedule timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the cursor blink cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the status refresh required by config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
/// Runs the provider failure is retryable operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the provider poll schedule timer key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the provider poll cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the provider retry schedule timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the provider retry cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the provider claim schedule timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the provider claim cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the pane pipe health schedule timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the pane pipe health cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the side effects include registry persistence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn side_effects_include_registry_persistence(effects: &[RuntimeSideEffect]) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, RuntimeSideEffect::PersistRegistry { .. }))
}

/// Runs the async runtime current unix millis operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn async_runtime_current_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Runs the async runtime duration millis operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn async_runtime_duration_millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis())
        .unwrap_or(u64::MAX)
        .max(1)
}

impl AsyncRuntimeSessionHandle {
    /// Runs the lifecycle state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn lifecycle_state(&self) -> Result<RuntimeLifecycleState> {
        self.request(|reply| AsyncRuntimeRequest::LifecycleState { reply })
            .await
    }

    /// Returns a watch receiver for actor-owned lifecycle state changes.
    ///
    /// Long-running socket services keep one receiver for their whole loop so
    /// they cannot miss a transition that occurs between a state check and an
    /// awaited socket read or accept.
    pub fn lifecycle_state_watcher(&self) -> watch::Receiver<RuntimeLifecycleState> {
        self.lifecycle_state_rx.clone()
    }

    /// Returns actor metrics captured at the serialized runtime boundary.
    pub async fn metrics(&self) -> Result<super::AsyncRuntimeActorMetrics> {
        self.request(|reply| AsyncRuntimeRequest::Metrics { reply })
            .await
    }

    /// Runs the render client view operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn render_client_view(
        &self,
        role: ClientViewRole,
        client_size: Size,
        config: TerminalClientLoopConfig,
    ) -> Result<Option<RenderedClientView>> {
        self.request(|reply| AsyncRuntimeRequest::RenderClientView {
            role,
            client_size,
            config,
            reply,
        })
        .await?
    }

    /// Runs the render client frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn render_client_frame(
        &self,
        role: ClientViewRole,
        client_size: Size,
        config: TerminalClientLoopConfig,
        render: bool,
    ) -> Result<AsyncRenderedClientFrame> {
        self.request(|reply| AsyncRuntimeRequest::RenderClientFrame {
            role,
            client_size,
            config,
            render,
            reply,
        })
        .await?
    }

    /// Runs the render client side effect operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn render_client_side_effect(
        &self,
        client_id: ClientId,
        config: TerminalClientLoopConfig,
        status: Option<ClientStatusLine>,
        cursor_blink_elapsed_ms: u64,
    ) -> Result<Option<AsyncRenderedClientFlush>> {
        self.request(|reply| AsyncRuntimeRequest::RenderClientSideEffect {
            client_id,
            config,
            status,
            cursor_blink_elapsed_ms,
            reply,
        })
        .await?
    }

    /// Runs the ensure client render timers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn ensure_client_render_timers(&self, client_id: ClientId) -> Result<usize> {
        self.request(|reply| AsyncRuntimeRequest::EnsureClientRenderTimers { client_id, reply })
            .await?
    }

    /// Runs the terminal client loop config operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn terminal_client_loop_config(
        &self,
        config: TerminalClientLoopConfig,
    ) -> Result<TerminalClientLoopConfig> {
        self.request(|reply| AsyncRuntimeRequest::TerminalClientLoopConfig { config, reply })
            .await?
    }

    /// Runs the handle control input for connection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn handle_control_input_for_connection(
        &self,
        input: Vec<u8>,
        max_content_length: usize,
        connection: ControlConnectionState,
    ) -> Result<AsyncControlInputResult> {
        self.request(|reply| AsyncRuntimeRequest::HandleControlInput {
            input,
            max_content_length,
            connection,
            reply,
        })
        .await?
    }

    /// Runs the handle control input for connection with snapshots operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn handle_control_input_for_connection_with_snapshots(
        &self,
        input: Vec<u8>,
        max_content_length: usize,
        connection: ControlConnectionState,
        snapshots: crate::snapshot::SnapshotRepository,
    ) -> Result<AsyncControlInputResult> {
        self.request(
            |reply| AsyncRuntimeRequest::HandleControlInputWithSnapshots {
                input,
                max_content_length,
                connection,
                snapshots,
                reply,
            },
        )
        .await?
    }

    /// Runs the handle message input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn handle_message_input(
        &self,
        input: Vec<u8>,
        max_content_length: usize,
        connection: MessageConnection,
        now_ms: u64,
    ) -> Result<AsyncMessageInputResult> {
        self.request(|reply| AsyncRuntimeRequest::HandleMessageInput {
            input,
            max_content_length,
            connection,
            now_ms,
            reply,
        })
        .await?
    }

    /// Runs the message fanout ready for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn message_fanout_ready_for(
        &self,
        recipient: AgentId,
        now_ms: u64,
        limit: usize,
    ) -> Result<Option<AsyncMessageFanout>> {
        self.request(|reply| AsyncRuntimeRequest::MessageFanoutReadyFor {
            recipient,
            now_ms,
            limit,
            reply,
        })
        .await?
    }

    /// Runs the acknowledge message fanout operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn acknowledge_message_fanout(&self, batch: FanoutBatch) -> Result<DeliveryCursor> {
        self.request(|reply| AsyncRuntimeRequest::AcknowledgeMessageFanout { batch, reply })
            .await?
    }

    /// Runs the wait for message delivery operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn wait_for_message_delivery(&self) {
        self.message_delivery_notify.notified().await;
    }

    /// Runs the wait for event delivery operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn wait_for_event_delivery(&self) {
        self.event_delivery_notify.notified().await;
    }

    /// Waits until the actor queues at least one runtime side effect.
    pub async fn wait_for_runtime_side_effects(&self) {
        self.side_effect_delivery_notify.notified().await;
    }

    /// Returns a non-consuming side-effect delivery revision watcher.
    pub fn side_effect_delivery_watcher(&self) -> watch::Receiver<u64> {
        self.side_effect_delivery_rx.clone()
    }

    /// Runs the event wakeups operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn event_wakeups(
        &self,
        connections: RuntimeEventConnectionTable,
        limit_per_connection: usize,
    ) -> Result<Vec<RuntimeEventWakeup>> {
        self.request(|reply| AsyncRuntimeRequest::EventWakeups {
            connections,
            limit_per_connection,
            reply,
        })
        .await?
    }

    /// Runs the apply attached terminal step plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn apply_attached_terminal_step_plan(
        &self,
        primary_client_id: ClientId,
        step: AttachedTerminalClientStepPlan,
    ) -> Result<AttachedClientStepApplication> {
        self.request(|reply| AsyncRuntimeRequest::ApplyAttachedTerminalStep {
            primary_client_id,
            step,
            reply,
        })
        .await?
    }

    /// Runs the apply attached terminal step plan inline pane io operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn apply_attached_terminal_step_plan_inline_pane_io(
        &self,
        primary_client_id: ClientId,
        step: AttachedTerminalClientStepPlan,
    ) -> Result<AttachedClientStepApplication> {
        self.request(
            |reply| AsyncRuntimeRequest::ApplyAttachedTerminalStepInlinePaneIo {
                primary_client_id,
                step,
                reply,
            },
        )
        .await?
    }

    /// Runs the resize attached primary terminal operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn resize_attached_primary_terminal(
        &self,
        primary_client_id: ClientId,
        size: Size,
    ) -> Result<Vec<PaneResizeUpdate>> {
        self.request(|reply| AsyncRuntimeRequest::ResizeAttachedPrimaryTerminal {
            primary_client_id,
            size,
            reply,
        })
        .await?
    }

    /// Runs the execute terminal command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn execute_terminal_command(
        &self,
        primary_client_id: ClientId,
        input: String,
    ) -> Result<String> {
        self.request(|reply| AsyncRuntimeRequest::ExecuteTerminalCommand {
            primary_client_id,
            input,
            reply,
        })
        .await?
    }
    /// Refreshes cached provider metadata through actor-owned runtime state.
    pub async fn refresh_provider_info(&self) -> Result<String> {
        self.request(|reply| AsyncRuntimeRequest::RefreshProviderInfo { reply })
            .await?
    }

    /// Shows a primary-client modal display overlay through actor-owned state.
    pub async fn show_primary_display_overlay(&self, lines: Vec<String>) -> Result<()> {
        self.request(|reply| AsyncRuntimeRequest::ShowPrimaryDisplayOverlay { lines, reply })
            .await?
    }

    /// Shows a primary-client recoverable error overlay through actor-owned state.
    pub async fn show_primary_error_overlay(&self, lines: Vec<String>) -> Result<()> {
        self.request(|reply| AsyncRuntimeRequest::ShowPrimaryErrorOverlay { lines, reply })
            .await?
    }

    /// Runs the execute agent shell command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn execute_agent_shell_command(
        &self,
        primary_client_id: ClientId,
        input: String,
    ) -> Result<String> {
        self.request(|reply| AsyncRuntimeRequest::ExecuteAgentShellCommand {
            primary_client_id,
            input,
            reply,
        })
        .await?
    }

    /// Runs the pending agent provider tasks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn pending_agent_provider_tasks(&self) -> Result<Vec<RuntimeAgentProviderTask>> {
        self.request(|reply| AsyncRuntimeRequest::PendingAgentProviderTasks { reply })
            .await?
    }

    /// Checks whether a provider worker should continue waiting for a turn.
    pub async fn agent_turn_is_running(&self, turn_id: &str) -> Result<bool> {
        let turn_id = turn_id.to_string();
        self.request(|reply| AsyncRuntimeRequest::AgentTurnIsRunning { turn_id, reply })
            .await?
    }

    /// Queues a provider-poll timer when pending provider work exists and no
    /// provider-poll generation is already scheduled.
    pub async fn queue_provider_poll_timer_if_needed(
        &self,
        generation: u64,
        delay_ms: u64,
    ) -> Result<bool> {
        self.request(
            |reply| AsyncRuntimeRequest::QueueProviderPollTimerIfNeeded {
                generation,
                delay_ms,
                reply,
            },
        )
        .await?
    }

    /// Runs the claim configured agent provider task operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn claim_configured_agent_provider_task(
        &self,
        agent_id: AgentId,
        turn_id: String,
    ) -> Result<Option<RuntimeAgentProviderDispatch>> {
        self.request(
            |reply| AsyncRuntimeRequest::ClaimConfiguredAgentProviderTask {
                agent_id,
                turn_id,
                reply,
            },
        )
        .await?
    }

    /// Claims one queued model-backed compaction task for async execution.
    pub async fn claim_agent_compaction_task(
        &self,
        pane_id: String,
    ) -> Result<Option<super::RuntimeAgentCompactionDispatch>> {
        self.request(|reply| AsyncRuntimeRequest::ClaimAgentCompactionTask { pane_id, reply })
            .await?
    }

    /// Claims one queued model-backed durable memory task for async execution.
    pub async fn claim_agent_remember_task(
        &self,
        pane_id: String,
    ) -> Result<Option<super::RuntimeAgentRememberDispatch>> {
        self.request(|reply| AsyncRuntimeRequest::ClaimAgentRememberTask { pane_id, reply })
            .await?
    }

    /// Runs the submit runtime events operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn submit_runtime_events(
        &self,
        batch: super::RuntimeEventBatch,
    ) -> Result<RuntimeEventIngressReport> {
        self.request(|reply| AsyncRuntimeRequest::SubmitRuntimeEvents { batch, reply })
            .await?
    }

    /// Drains queued actor side effects for supervised external adapters.
    ///
    /// The returned effects are already ordered by the runtime events that
    /// produced them. A zero limit is rejected so callers cannot accidentally
    /// spin while making no progress.
    pub async fn drain_runtime_side_effects(&self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainRuntimeSideEffects { limit, reply })
            .await?
    }

    /// Runs the queue runtime side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn queue_runtime_side_effects(
        &self,
        side_effects: Vec<RuntimeSideEffect>,
    ) -> Result<usize> {
        self.request(|reply| AsyncRuntimeRequest::QueueRuntimeSideEffects {
            side_effects,
            reply,
        })
        .await?
    }

    /// Runs the drain agent provider dispatch side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_agent_provider_dispatch_side_effects(
        &self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        self.request(
            |reply| AsyncRuntimeRequest::DrainAgentProviderDispatchSideEffects { limit, reply },
        )
        .await?
    }

    /// Runs the drain render side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_render_side_effects(&self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainRenderSideEffects { limit, reply })
            .await?
    }

    /// Runs the drain render side effects for client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_render_side_effects_for_client(
        &self,
        client_id: ClientId,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        self.request(
            |reply| AsyncRuntimeRequest::DrainRenderSideEffectsForClient {
                client_id,
                limit,
                reply,
            },
        )
        .await?
    }

    /// Runs the drain client output flush side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_client_output_flush_side_effects(
        &self,
        client_id: Option<ClientId>,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        self.request(
            |reply| AsyncRuntimeRequest::DrainClientOutputFlushSideEffects {
                client_id,
                limit,
                reply,
            },
        )
        .await?
    }

    /// Runs the drain timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_timer_side_effects(&self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainTimerSideEffects { limit, reply })
            .await?
    }

    /// Runs the drain persistence side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_persistence_side_effects(
        &self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainPersistenceSideEffects { limit, reply })
            .await?
    }

    /// Runs the drain hook side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_hook_side_effects(&self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainHookSideEffects { limit, reply })
            .await?
    }

    /// Runs the drain pane io side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_pane_io_side_effects(
        &self,
        pane_id: impl Into<String>,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        let pane_id = pane_id.into();
        self.request(|reply| AsyncRuntimeRequest::DrainPaneIoSideEffects {
            pane_id,
            limit,
            reply,
        })
        .await?
    }

    /// Moves running pane process handles out of the serialized runtime owner
    /// so external pane process adapters can own PTY I/O.
    pub async fn take_running_pane_processes_for_adapter(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, super::PaneProcess)>> {
        self.request(
            |reply| AsyncRuntimeRequest::TakeRunningPaneProcessesForAdapter { limit, reply },
        )
        .await?
    }

    /// Runs the shutdown operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn shutdown(&self) -> Result<RuntimeLifecycleState> {
        self.request(|reply| AsyncRuntimeRequest::Shutdown { reply })
            .await
    }

    /// Runs the request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn request<T>(
        &self,
        build_request: impl FnOnce(oneshot::Sender<T>) -> AsyncRuntimeRequest,
    ) -> Result<T> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(build_request(reply))
            .await
            .map_err(|_| MezError::invalid_state("async runtime session actor is closed"))?;
        response
            .await
            .map_err(|_| MezError::invalid_state("async runtime session actor reply was dropped"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{PersistenceTarget, PersistenceWriteMode};
    use std::path::PathBuf;

    /// Verifies that the provider worker watchdog cannot fire before the
    /// provider transport timeout. The watchdog cleans up abandoned async
    /// claims, so it must leave enough time for a legitimate long-running
    /// provider request to settle through the provider layer first.
    #[test]
    fn provider_claim_timeout_exceeds_provider_transport_timeout() {
        let claim_timeout_ms = std::hint::black_box(DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS);
        let provider_timeout_ms = std::hint::black_box(DEFAULT_PROVIDER_TIMEOUT_MS);

        assert!(
            claim_timeout_ms > provider_timeout_ms,
            "provider claim watchdog {} ms must exceed provider timeout {} ms",
            claim_timeout_ms,
            provider_timeout_ms
        );
    }

    /// Verifies repeated deferred config replacements for the same destination
    /// collapse to the newest complete document.
    ///
    /// A single provider response can contain many `config_change` actions for
    /// adjacent theme slots. The actor should persist only the final config text
    /// for each file instead of queueing a long series of superseded full-file
    /// replacements.
    #[test]
    fn coalesce_config_persistence_effects_keeps_latest_text_per_target() {
        let config_path = PathBuf::from("/tmp/mez/config.toml");
        let project_path = PathBuf::from("/tmp/project/.mezzanine/config.toml");

        let coalesced = coalesce_config_persistence_effects(vec![
            RuntimeSideEffect::Persist {
                target: PersistenceTarget::Config,
                path: config_path.clone(),
                bytes: b"first".to_vec(),
                mode: PersistenceWriteMode::Replace,
            },
            RuntimeSideEffect::Persist {
                target: PersistenceTarget::ProjectConfig,
                path: project_path.clone(),
                bytes: b"project".to_vec(),
                mode: PersistenceWriteMode::Replace,
            },
            RuntimeSideEffect::Persist {
                target: PersistenceTarget::Config,
                path: config_path.clone(),
                bytes: b"second".to_vec(),
                mode: PersistenceWriteMode::Replace,
            },
        ]);

        assert_eq!(coalesced.len(), 2);
        assert!(matches!(
            &coalesced[0],
            RuntimeSideEffect::Persist { target: PersistenceTarget::Config, path, bytes, .. }
                if path == &config_path && bytes == b"second"
        ));
        assert!(matches!(
            &coalesced[1],
            RuntimeSideEffect::Persist { target: PersistenceTarget::ProjectConfig, path, bytes, .. }
                if path == &project_path && bytes == b"project"
        ));
    }
}
