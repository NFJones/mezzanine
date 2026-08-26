//! Serialized request dispatch for the runtime actor.

use super::construction::execute_snapshot_control_async_work;
use super::{
    AsyncControlInputResult, AsyncMessageFanout, AsyncMessageInputResult, AsyncRenderedClientFrame,
    AsyncRuntimeRequest, AsyncRuntimeRequestEnvelope, AsyncRuntimeSessionActor,
    AsyncTerminalClientConfigInput, AsyncTerminalClientConfigSnapshot,
    DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS, RuntimeSessionService, decode_control_frame,
    delivery_batch_json, encode_control_body, encode_mmp_body,
};
use crate::host::async_runtime::actor_types::AsyncClientRenderToken;
use crate::host::terminal::AttachedTerminalClientStepPlan;

impl AsyncRuntimeSessionActor {
    /// Removes one clipboard route only when the requesting event-stream
    /// generation still owns it.
    pub(super) fn cleanup_client_clipboard_route(
        &mut self,
        client_id: mez_core::ids::ClientId,
        generation: u64,
    ) -> bool {
        if self.client_clipboard_route_generations.get(&client_id) != Some(&generation) {
            return false;
        }
        self.client_clipboard_route_generations.remove(&client_id);
        self.client_clipboard_sequences.remove(&client_id);
        self.client_clipboard_routes.remove(&client_id).is_some()
    }

    /// Captures the exact primary view identity used to derive coordinate actions.
    fn client_render_token(
        &mut self,
        client_id: &mez_core::ids::ClientId,
        role: mez_mux::presentation::ClientViewRole,
    ) -> crate::Result<Option<AsyncClientRenderToken>> {
        if role != mez_mux::presentation::ClientViewRole::Primary {
            return Ok(None);
        }
        let (window_id, navigation_revision, layout_revision, presentation_revision) =
            self.service.client_render_identity(client_id)?;
        Ok(Some(AsyncClientRenderToken {
            client_id: client_id.clone(),
            window_id,
            navigation_revision,
            layout_revision,
            presentation_revision,
        }))
    }

    /// Reports whether a planned step contains coordinates resolved from a frame.
    fn step_uses_render_coordinates(step: &AttachedTerminalClientStepPlan) -> bool {
        step.actions.iter().any(|action| {
            matches!(
                action,
                crate::host::terminal::TerminalClientLoopAction::HandleMouse(
                    crate::host::terminal::MouseAction::FocusWindow { .. }
                        | crate::host::terminal::MouseAction::FocusGroup { .. }
                        | crate::host::terminal::MouseAction::PressWindowAction { .. }
                        | crate::host::terminal::MouseAction::ReleaseWindowAction { .. }
                        | crate::host::terminal::MouseAction::OpenPaneAgentStatusSelector { .. }
                        | crate::host::terminal::MouseAction::HoverPaneAgentStatusSelector { .. }
                        | crate::host::terminal::MouseAction::SelectPaneAgentStatusSelector { .. }
                        | crate::host::terminal::MouseAction::BeginDisplayOverlaySelection { .. }
                        | crate::host::terminal::MouseAction::UpdateDisplayOverlaySelection { .. }
                        | crate::host::terminal::MouseAction::FinishDisplayOverlaySelection { .. }
                        | crate::host::terminal::MouseAction::SelectDisplayOverlay { .. }
                        | crate::host::terminal::MouseAction::FocusPane(_)
                        | crate::host::terminal::MouseAction::FocusPaneOnly(_)
                        | crate::host::terminal::MouseAction::PasteClipboard(_)
                        | crate::host::terminal::MouseAction::ShowWindowChooser { .. }
                        | crate::host::terminal::MouseAction::ResizePane { .. }
                        | crate::host::terminal::MouseAction::CopySelectionStart(_)
                        | crate::host::terminal::MouseAction::CopyWord(_)
                        | crate::host::terminal::MouseAction::CopySelectionUpdate(_)
                        | crate::host::terminal::MouseAction::CopySelectionFinish(_)
                        | crate::host::terminal::MouseAction::ScrollHistory { .. }
                )
            )
        })
    }

    /// Resolves terminal configuration for one exact client's prepared view.
    fn resolve_terminal_client_config_snapshot_for_client(
        &self,
        client_id: &mez_core::ids::ClientId,
        input: AsyncTerminalClientConfigInput,
    ) -> crate::Result<AsyncTerminalClientConfigSnapshot> {
        match input {
            AsyncTerminalClientConfigInput::Snapshot(snapshot)
                if snapshot.generation() == self.terminal_config_generation
                    && snapshot.client_id() == Some(client_id) =>
            {
                Ok(snapshot)
            }
            AsyncTerminalClientConfigInput::Raw(config) => self
                .service
                .terminal_client_loop_config(*config)
                .map(|config| {
                    AsyncTerminalClientConfigSnapshot::new_for_client(
                        self.terminal_config_generation,
                        client_id.clone(),
                        config,
                    )
                }),
            AsyncTerminalClientConfigInput::Snapshot(snapshot) => self
                .service
                .terminal_client_loop_config(snapshot.config().clone())
                .map(|config| {
                    AsyncTerminalClientConfigSnapshot::new_for_client(
                        self.terminal_config_generation,
                        client_id.clone(),
                        config,
                    )
                }),
        }
    }

    /// Resolves stale terminal configuration while reusing current snapshots.
    fn resolve_terminal_client_config_snapshot(
        &self,
        input: AsyncTerminalClientConfigInput,
    ) -> crate::Result<AsyncTerminalClientConfigSnapshot> {
        match input {
            AsyncTerminalClientConfigInput::Snapshot(snapshot)
                if snapshot.generation() == self.terminal_config_generation =>
            {
                Ok(snapshot)
            }
            AsyncTerminalClientConfigInput::Raw(config) => self
                .service
                .terminal_client_loop_config(*config)
                .map(|config| {
                    AsyncTerminalClientConfigSnapshot::new(self.terminal_config_generation, config)
                }),
            AsyncTerminalClientConfigInput::Snapshot(snapshot) => self
                .service
                .terminal_client_loop_config(snapshot.config().clone())
                .map(|config| {
                    AsyncTerminalClientConfigSnapshot::new(self.terminal_config_generation, config)
                }),
        }
    }

    /// Runs the handle request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn handle_request(&mut self, request: AsyncRuntimeRequest) -> bool {
        match request {
            AsyncRuntimeRequest::SetHostRoutedIrohDiagnostics { diagnostics, reply } => {
                self.service.set_host_routed_iroh_diagnostics(diagnostics);
                let _ = reply.send(());
                false
            }
            AsyncRuntimeRequest::LifecycleState { reply } => {
                let _ = reply.send(self.service.lifecycle_state());
                false
            }
            AsyncRuntimeRequest::CreateHostCheckpoint {
                snapshots,
                snapshot_id,
                name,
                reply,
            } => {
                let (session, context) = self.service.host_checkpoint_snapshot();
                let task = tokio::spawn(async move {
                    let result = snapshots
                        .create_from_session_with_context_async(
                            &snapshot_id,
                            name,
                            &session,
                            context.as_creation_context(),
                        )
                        .await;
                    let _ = reply.send(result);
                });
                std::mem::drop(task);
                false
            }
            AsyncRuntimeRequest::Metrics { reply } => {
                let mut metrics = self.metrics.clone();
                metrics.side_effect_queue_depth = self.side_effects.len();
                let _ = reply.send(metrics);
                false
            }
            AsyncRuntimeRequest::RecordLatencyPhase { phase, elapsed_ms } => {
                self.metrics.record_phase_latency(phase, elapsed_ms);
                false
            }
            #[cfg(test)]
            AsyncRuntimeRequest::WriteInputToPane {
                primary_client_id,
                pane_id,
                input,
                reply,
            } => {
                let result = self
                    .service
                    .write_input_to_pane(&primary_client_id, Some(&pane_id), &input)
                    .and_then(|dispatch| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        Ok(dispatch)
                    });
                let _ = reply.send(result);
                false
            }
            #[cfg(test)]
            AsyncRuntimeRequest::ManagedShellLifecycleState { pane_id, reply } => {
                let _ = reply.send((
                    self.service.agent_subshell_is_active(&pane_id),
                    self.service.pane_bootstrap_is_pending_for_tests(&pane_id),
                    self.service
                        .managed_shell_parent_restoration_is_pending_for_tests(&pane_id),
                ));
                false
            }
            #[cfg(test)]
            AsyncRuntimeRequest::PaneCertificationSnapshot { pane_id, reply } => {
                let _ = reply.send(crate::host::async_runtime::AsyncPaneCertificationSnapshot {
                    child_active: self.service.agent_subshell_is_active(&pane_id),
                    bootstrap_pending: self.service.pane_bootstrap_is_pending_for_tests(&pane_id),
                    foreign_bootstrap_phase: self
                        .service
                        .foreign_shell_bootstrap_phase_for_tests(&pane_id),
                    certification_pending: self
                        .service
                        .pane_agent_subshell_certification_is_pending(&pane_id),
                    environment_signature_present: self
                        .service
                        .pane_environment_signature(&pane_id)
                        .is_some(),
                    readiness: self.service.pane_readiness_state(&pane_id),
                    certification_rejection: self
                        .service
                        .pane_agent_subshell_certification_rejection(&pane_id),
                    foreground_certified_shell: self
                        .service
                        .pane_foreground_certified_shell_state(&pane_id),
                    shell_interaction_generation: self
                        .service
                        .pane_shell_interaction_generation_for_tests(&pane_id),
                    foreground_diagnostic: self
                        .service
                        .pane_foreground_process_diagnostic(&pane_id)
                        .json(),
                });
                false
            }
            #[cfg(test)]
            AsyncRuntimeRequest::ManagedShellProcessScreenText { pane_id, reply } => {
                let text = self
                    .service
                    .process_pane_screen(&pane_id)
                    .map(|screen| screen.normal_content_lines().join("\n"))
                    .unwrap_or_default();
                let _ = reply.send(text);
                false
            }
            #[cfg(test)]
            AsyncRuntimeRequest::ManagedZshAdmissionReady { pane_id, reply } => {
                let _ = reply.send(
                    self.service
                        .managed_zsh_admission_is_ready_for_tests(&pane_id),
                );
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
                let result = self
                    .service
                    .render_client_view(role, client_size, &config)
                    .and_then(|view| {
                        let effects = self
                            .service
                            .drain_status_pill_refresh_transition()
                            .side_effects;
                        if !effects.is_empty() {
                            self.queue_runtime_side_effects(effects)?;
                        }
                        Ok(view)
                    });
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::RenderClientFrame {
                client_id,
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
                    .prepare_client_render(&client_id, role)
                    .and_then(|()| {
                        self.resolve_terminal_client_config_snapshot_for_client(&client_id, config)
                    })
                    .and_then(|config| {
                        let view = if render {
                            self.service
                                .render_client_view_for_client_with_resolved_config(
                                    &client_id,
                                    role,
                                    client_size,
                                    config.config(),
                                )?
                        } else {
                            None
                        };
                        let render_token = if render {
                            self.client_render_token(&client_id, role)?
                        } else {
                            None
                        };
                        let effects = self
                            .service
                            .drain_status_pill_refresh_transition()
                            .side_effects;
                        if !effects.is_empty() {
                            self.queue_runtime_side_effects(effects)?;
                        }
                        Ok(AsyncRenderedClientFrame {
                            config,
                            render_token,
                            view,
                        })
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
            AsyncRuntimeRequest::TerminalClientLoopConfigSnapshot { config, reply } => {
                let result = self.resolve_terminal_client_config_snapshot(config);
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
                        self.queue_pending_provider_dispatch_side_effects()?;
                        self.queue_shell_lifecycle_timer_side_effects()?;
                        if let Some(client_id) = connection.caller_client_id().cloned() {
                            self.ensure_client_render_timers_or_defer_to_pending_render(
                                &client_id,
                            )?;
                        }
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
                mut output_prefix,
                consumed_prefix,
                record_metrics,
                max_content_length,
                mut connection,
                snapshots,
                reply,
            } => {
                if record_metrics {
                    self.record_terminal_control_request_metrics(&input, max_content_length);
                }
                if let Ok((body, frame_consumed)) = decode_control_frame(&input, max_content_length)
                    && let Some(prepared) = self
                        .service
                        .prepare_runtime_snapshot_control_async_work(&body, &connection)
                {
                    let remaining_input = input[frame_consumed..].to_vec();
                    let consumed_prefix = consumed_prefix.saturating_add(frame_consumed);
                    match prepared {
                        Ok(work) => {
                            let sender = self.sender.clone();
                            let join_handle = tokio::spawn(async move {
                                let outcome =
                                    execute_snapshot_control_async_work(&snapshots, &work).await;
                                let _ = sender
                                    .send(AsyncRuntimeRequestEnvelope::new(
                                        AsyncRuntimeRequest::CompleteSnapshotControlInput {
                                            consumed_prefix,
                                            output_prefix,
                                            remaining_input,
                                            max_content_length,
                                            snapshots,
                                            connection,
                                            work,
                                            outcome: Box::new(outcome),
                                            reply,
                                        },
                                    ))
                                    .await;
                            });
                            std::mem::drop(join_handle);
                            return false;
                        }
                        Err(body) => {
                            output_prefix.extend_from_slice(&encode_control_body(&body));
                            if remaining_input.is_empty() {
                                let _ = reply.send(Ok(AsyncControlInputResult {
                                    output: output_prefix,
                                    consumed: consumed_prefix,
                                    connection,
                                }));
                                self.notify_event_delivery();
                            } else {
                                let sender = self.sender.clone();
                                let join_handle = tokio::spawn(async move {
                                    let _ = sender
                                        .send(AsyncRuntimeRequestEnvelope::new(
                                            AsyncRuntimeRequest::HandleControlInputWithSnapshots {
                                                input: remaining_input,
                                                output_prefix,
                                                consumed_prefix,
                                                record_metrics: false,
                                                max_content_length,
                                                connection,
                                                snapshots,
                                                reply,
                                            },
                                        ))
                                        .await;
                                });
                                std::mem::drop(join_handle);
                            }
                            return false;
                        }
                    }
                }
                let previous_lifecycle_state = self.service.lifecycle_state();
                let frame_consumed = decode_control_frame(&input, max_content_length)
                    .map(|(_, consumed)| consumed)
                    .unwrap_or(input.len());
                let remaining_input = input[frame_consumed..].to_vec();
                let result = self
                    .service
                    .handle_control_input_for_connection_with_snapshots_transition(
                        &input[..frame_consumed],
                        max_content_length,
                        &mut connection,
                        &snapshots,
                    )
                    .await
                    .and_then(|(output, consumed, transition)| {
                        output_prefix.extend_from_slice(&output);
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_runtime_side_effects(transition.side_effects)?;
                        self.queue_pending_provider_dispatch_side_effects()?;
                        self.queue_shell_lifecycle_timer_side_effects()?;
                        if let Some(client_id) = connection.caller_client_id().cloned() {
                            self.ensure_client_render_timers_or_defer_to_pending_render(
                                &client_id,
                            )?;
                        }
                        Ok(consumed)
                    });
                match result {
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                    Ok(consumed) if remaining_input.is_empty() => {
                        let _ = reply.send(Ok(AsyncControlInputResult {
                            output: output_prefix,
                            consumed: consumed_prefix.saturating_add(consumed),
                            connection,
                        }));
                        self.notify_event_delivery();
                    }
                    Ok(consumed) => {
                        let sender = self.sender.clone();
                        let consumed_prefix = consumed_prefix.saturating_add(consumed);
                        let join_handle = tokio::spawn(async move {
                            let _ = sender
                                .send(AsyncRuntimeRequestEnvelope::new(
                                    AsyncRuntimeRequest::HandleControlInputWithSnapshots {
                                        input: remaining_input,
                                        output_prefix,
                                        consumed_prefix,
                                        record_metrics: false,
                                        max_content_length,
                                        connection,
                                        snapshots,
                                        reply,
                                    },
                                ))
                                .await;
                        });
                        std::mem::drop(join_handle);
                    }
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::CompleteSnapshotControlInput {
                consumed_prefix,
                mut output_prefix,
                remaining_input,
                max_content_length,
                snapshots,
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
                output_prefix.extend_from_slice(&encode_control_body(&body));
                let queued = self
                    .queue_deferred_pane_io_side_effects_from_service()
                    .and_then(|_| self.queue_runtime_side_effects(transition.side_effects));
                if let Err(error) = queued {
                    let _ = reply.send(Err(error));
                } else if remaining_input.is_empty() {
                    let _ = reply.send(Ok(AsyncControlInputResult {
                        output: output_prefix,
                        consumed: consumed_prefix,
                        connection,
                    }));
                    self.notify_event_delivery();
                } else {
                    let sender = self.sender.clone();
                    let join_handle = tokio::spawn(async move {
                        let _ = sender
                            .send(AsyncRuntimeRequestEnvelope::new(
                                AsyncRuntimeRequest::HandleControlInputWithSnapshots {
                                    input: remaining_input,
                                    output_prefix,
                                    consumed_prefix,
                                    record_metrics: false,
                                    max_content_length,
                                    connection,
                                    snapshots,
                                    reply,
                                },
                            ))
                            .await;
                    });
                    std::mem::drop(join_handle);
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
            #[cfg(test)]
            AsyncRuntimeRequest::EventWakeups {
                connections,
                limit_per_connection,
                reply,
            } => {
                let wakeups = connections.wakeups(self.service.event_log(), limit_per_connection);
                let _ = reply.send(Ok(wakeups));
                false
            }
            AsyncRuntimeRequest::EventWakeupsForClient {
                caller_client_id,
                connection_id,
                last_delivered_event_id,
                limit_per_connection,
                reply,
            } => {
                let result = self.service.authorized_event_wakeups(
                    &caller_client_id,
                    &connection_id,
                    last_delivered_event_id,
                    limit_per_connection,
                );
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::RegisterClientClipboardRoute { client_id, reply } => {
                self.next_client_clipboard_route_generation = self
                    .next_client_clipboard_route_generation
                    .saturating_add(1);
                let generation = self.next_client_clipboard_route_generation;
                self.client_clipboard_routes.insert(client_id.clone(), None);
                self.client_clipboard_route_generations
                    .insert(client_id.clone(), generation);
                self.client_clipboard_sequences.insert(client_id, 0);
                let _ = reply.send(generation);
                false
            }
            AsyncRuntimeRequest::UnregisterClientClipboardRoute {
                client_id,
                generation,
                reply,
            } => {
                let removed = self.cleanup_client_clipboard_route(client_id, generation);
                let _ = reply.send(removed);
                false
            }
            #[cfg(test)]
            AsyncRuntimeRequest::EnqueueClientClipboardWrite {
                client_id,
                content,
                reply,
            } => {
                let accepted =
                    if let Some(pending) = self.client_clipboard_routes.get_mut(&client_id) {
                        let sequence = self
                            .client_clipboard_sequences
                            .get(&client_id)
                            .copied()
                            .unwrap_or(0)
                            .saturating_add(1);
                        if let Some(write) =
                            crate::runtime::ClientClipboardWrite::new(sequence, content)
                        {
                            self.client_clipboard_sequences.insert(client_id, sequence);
                            *pending = Some(write);
                            self.notify_event_delivery();
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                let _ = reply.send(accepted);
                false
            }
            AsyncRuntimeRequest::TakeClientClipboardWrite {
                client_id,
                generation,
                reply,
            } => {
                let pending = (self.client_clipboard_route_generations.get(&client_id)
                    == Some(&generation))
                .then(|| {
                    self.client_clipboard_routes
                        .get_mut(&client_id)
                        .and_then(Option::take)
                })
                .flatten();
                let _ = reply.send(pending);
                false
            }
            AsyncRuntimeRequest::ConsumeUnixEventBinding {
                token,
                peer_uid,
                reply,
            } => {
                let result = self.service.consume_unix_event_binding(&token, peer_uid);
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::ApplyAttachedTerminalStep {
                primary_client_id,
                render_token,
                step,
                reply,
            } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let stale_coordinate_input = render_token
                    .as_ref()
                    .filter(|_| Self::step_uses_render_coordinates(&step))
                    .map(|token| {
                        self.service.client_render_identity(&primary_client_id).map(
                            |(
                                window_id,
                                navigation_revision,
                                layout_revision,
                                presentation_revision,
                            )| {
                                token.client_id != primary_client_id
                                    || token.window_id != window_id
                                    || token.navigation_revision != navigation_revision
                                    || token.layout_revision != layout_revision
                                    || token.presentation_revision != presentation_revision
                            },
                        )
                    })
                    .transpose();
                let result = stale_coordinate_input.and_then(|stale| {
                    if stale == Some(true) {
                        let application = self
                            .service
                            .cancel_stale_client_coordinate_input(&primary_client_id)?;
                        self.queue_runtime_side_effects(vec![
                            crate::runtime::RuntimeSideEffect::RenderClient {
                                client_id: primary_client_id.clone(),
                                reason: crate::runtime::RenderInvalidationReason::FullRedraw,
                            },
                        ])?;
                        return Ok(application);
                    }
                    let (mut application, transition) = self
                        .service
                        .apply_attached_terminal_step_transition(&primary_client_id, &step)?;
                    if let Some(candidate) = application.client_clipboard_write.take()
                        && let Some(pending) =
                            self.client_clipboard_routes.get_mut(&primary_client_id)
                    {
                        let sequence = self
                            .client_clipboard_sequences
                            .get(&primary_client_id)
                            .copied()
                            .unwrap_or(0)
                            .saturating_add(1);
                        if let Some(write) = crate::runtime::ClientClipboardWrite::new(
                            sequence,
                            candidate.into_content(),
                        ) {
                            self.client_clipboard_sequences
                                .insert(primary_client_id.clone(), sequence);
                            *pending = Some(write);
                            self.notify_event_delivery();
                        }
                    }
                    self.queue_runtime_side_effects(transition.side_effects)?;
                    self.queue_deferred_pane_io_side_effects_from_service()?;
                    self.queue_pending_provider_dispatch_side_effects()?;
                    self.queue_shell_lifecycle_timer_side_effects()?;
                    self.ensure_client_render_timers_or_defer_to_pending_render(
                        &primary_client_id,
                    )?;
                    for mut refresh in self
                        .service
                        .take_pending_agent_prompt_provider_info_refreshes()
                    {
                        let Some(work) = refresh.work.take() else {
                            continue;
                        };
                        let sender = self.sender.clone();
                        let join_handle = tokio::spawn(async move {
                            let outcome =
                                RuntimeSessionService::execute_provider_info_refresh(work).await;
                            let _ = sender
                                .send(AsyncRuntimeRequestEnvelope::new(
                                    AsyncRuntimeRequest::CompleteAgentPromptProviderInfoRefresh {
                                        refresh,
                                        outcome,
                                    },
                                ))
                                .await;
                        });
                        std::mem::drop(join_handle);
                    }
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
                        self.queue_shell_lifecycle_timer_side_effects()?;
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
                match self.service.prepare_provider_info_refresh() {
                    Ok(work) => {
                        let sender = self.sender.clone();
                        let join_handle = tokio::spawn(async move {
                            let outcome =
                                RuntimeSessionService::execute_provider_info_refresh(work).await;
                            let _ = sender
                                .send(AsyncRuntimeRequestEnvelope::new(
                                    AsyncRuntimeRequest::CompleteProviderInfoRefresh {
                                        outcome,
                                        reply,
                                    },
                                ))
                                .await;
                        });
                        std::mem::drop(join_handle);
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
                false
            }
            AsyncRuntimeRequest::CompleteProviderInfoRefresh { outcome, reply } => {
                let result = self.service.apply_provider_info_refresh(outcome);
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
                match self
                    .service
                    .prepare_agent_shell_provider_info_refresh(&primary_client_id, &input)
                {
                    Ok(Some(work)) => {
                        let sender = self.sender.clone();
                        let join_handle = tokio::spawn(async move {
                            let outcome =
                                RuntimeSessionService::execute_provider_info_refresh(work).await;
                            let _ = sender
                                .send(AsyncRuntimeRequestEnvelope::new(
                                    AsyncRuntimeRequest::CompleteAgentShellProviderInfoRefresh {
                                        primary_client_id,
                                        input,
                                        outcome,
                                        reply,
                                    },
                                ))
                                .await;
                        });
                        std::mem::drop(join_handle);
                        return false;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return false;
                    }
                }
                match self
                    .service
                    .prepare_agent_shell_mcp_discovery(&primary_client_id, &input)
                {
                    Ok(Some(work)) => {
                        let sender = self.sender.clone();
                        let join_handle = tokio::spawn(async move {
                            let preparation =
                                RuntimeSessionService::execute_agent_provider_preparation(work)
                                    .await;
                            let _ = sender
                                .send(AsyncRuntimeRequestEnvelope::new(
                                    AsyncRuntimeRequest::CompleteAgentShellMcpDiscovery {
                                        primary_client_id,
                                        input,
                                        preparation,
                                        reply,
                                    },
                                ))
                                .await;
                        });
                        std::mem::drop(join_handle);
                        return false;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return false;
                    }
                }
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self
                    .service
                    .execute_agent_shell_command_async(&primary_client_id, &input)
                    .await
                    .and_then(|output| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_shell_lifecycle_timer_side_effects()?;
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
            AsyncRuntimeRequest::CompleteAgentShellMcpDiscovery {
                primary_client_id,
                input,
                preparation,
                reply,
            } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self
                    .service
                    .apply_agent_provider_preparation(preparation)
                    .and_then(|_| {
                        self.service
                            .execute_agent_shell_command(&primary_client_id, &input)
                    })
                    .and_then(|output| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_shell_lifecycle_timer_side_effects()?;
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
            AsyncRuntimeRequest::CompleteAgentShellProviderInfoRefresh {
                primary_client_id,
                input,
                outcome,
                reply,
            } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self
                    .service
                    .complete_agent_shell_provider_info_refresh(&primary_client_id, &input, outcome)
                    .and_then(|output| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
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
            AsyncRuntimeRequest::CompleteAgentPromptProviderInfoRefresh { refresh, outcome } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let primary_client_id = refresh.primary_client_id.clone();
                let result = self
                    .service
                    .complete_agent_prompt_provider_info_refresh(refresh, outcome)
                    .and_then(|()| {
                        let mut side_effects = self.deferred_service_side_effects_from_service();
                        side_effects.push(crate::runtime::RuntimeSideEffect::RenderClient {
                            client_id: primary_client_id,
                            reason: crate::runtime::RenderInvalidationReason::AgentPrompt,
                        });
                        self.queue_runtime_side_effects(side_effects)
                    });
                if result.is_ok() {
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
            AsyncRuntimeRequest::PrepareConfiguredAgentProviderTask { reply } => {
                let result = self.service.prepare_agent_provider_work();
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::ClaimConfiguredAgentProviderTask {
                agent_id,
                turn_id,
                preparation,
                reply,
            } => {
                let result = self
                    .service
                    .apply_agent_provider_preparation(preparation)
                    .and_then(|_| {
                        self.service
                            .claim_configured_agent_provider_task(&agent_id, &turn_id)
                    });
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
            AsyncRuntimeRequest::ClaimApprovedExternalAction {
                turn_id,
                action_id,
                reply,
            } => {
                let result = self
                    .service
                    .claim_approved_external_action(&turn_id, &action_id);
                let should_notify = result.is_ok();
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                false
            }
            AsyncRuntimeRequest::ClaimNativeShellAction {
                turn_id,
                action_id,
                reply,
            } => {
                let result = self.service.claim_native_shell_action(&turn_id, &action_id);
                let should_notify = result.is_ok();
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                false
            }
            AsyncRuntimeRequest::CompleteApprovedExternalAction { outcome, reply } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let result = self
                    .service
                    .complete_approved_external_action(outcome)
                    .and_then(|applied| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_pending_provider_dispatch_side_effects()?;
                        Ok(applied)
                    });
                let should_notify = result.is_ok();
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
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
            AsyncRuntimeRequest::TakeStreamingSayProjectionWork {
                pane_id,
                turn_id,
                reply,
            } => {
                let result = self
                    .service
                    .take_agent_streaming_say_projection_work(&pane_id, &turn_id);
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::ApplyStreamingSayProjection { result, reply } => {
                let applied = self
                    .service
                    .apply_agent_streaming_say_projection_result(result);
                if applied.as_ref().is_ok_and(|applied| *applied) {
                    let side_effects = self
                        .render_side_effects(crate::runtime::RenderInvalidationReason::PaneOutput);
                    let _ = self.queue_runtime_side_effects(side_effects);
                }
                let _ = reply.send(applied);
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
            AsyncRuntimeRequest::DrainHostClipboardSideEffects { limit, reply } => {
                let _ = reply.send(self.drain_host_clipboard_side_effects(limit));
                false
            }
            AsyncRuntimeRequest::DrainStatusPillSideEffects { limit, reply } => {
                let _ = reply.send(self.drain_status_pill_side_effects(limit));
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
            AsyncRuntimeRequest::DrainPaneProcessIoSideEffects {
                instance,
                limit,
                reply,
            } => {
                let _ = reply.send(self.drain_pane_process_io_side_effects(&instance, limit));
                false
            }
            AsyncRuntimeRequest::TakeRunningPaneProcessesForAdapter { limit, reply } => {
                let result = self
                    .service
                    .take_running_pane_process_instances_for_adapter(limit);
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
