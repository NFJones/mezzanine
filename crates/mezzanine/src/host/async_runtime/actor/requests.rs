//! Serialized request dispatch for the runtime actor.

use super::construction::execute_snapshot_control_async_work;
use super::{
    AsyncControlInputResult, AsyncMessageFanout, AsyncMessageInputResult, AsyncRenderedClientFrame,
    AsyncRuntimeRequest, AsyncRuntimeSessionActor, DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS,
    decode_control_frame, delivery_batch_json, encode_control_body, encode_mmp_body,
};

impl AsyncRuntimeSessionActor {
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
                    })
                    .map_err(Into::into);
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::AcknowledgeMessageFanout { batch, reply } => {
                let result = self
                    .service
                    .message_service_mut()
                    .acknowledge_fanout_batch(&batch)
                    .map_err(Into::into);
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
}
