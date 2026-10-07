//! Serialized request dispatch for the runtime actor.

use super::TranscriptReceiptReply;
use super::claim_admission::WorkerClaimLease;
use super::construction::execute_snapshot_control_async_work;
use super::{
    AsyncControlInputResult, AsyncIrohRenderSnapshot, AsyncMessageFanout, AsyncMessageInputResult,
    AsyncRenderedClientFrame, AsyncRuntimeRequest, AsyncRuntimeRequestEnvelope,
    AsyncRuntimeSessionActor, AsyncTerminalClientConfigInput, AsyncTerminalClientConfigSnapshot,
    DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS, MezError, RuntimeSessionService, RuntimeSideEffect,
    RuntimeTimerKey, RuntimeTimerKind, decode_control_frame, delivery_batch_json,
    encode_control_body, encode_mmp_body,
};
use crate::host::async_runtime::actor_types::AsyncClientRenderToken;
use crate::host::terminal::AttachedTerminalClientStepPlan;

impl AsyncRuntimeSessionActor {
    /// Runs a finite status history query off actor and retains ordered RPC reply ownership.
    fn dispatch_status_control_query(
        &self,
        work: crate::runtime::StatusControlWork,
        mut completion: AsyncRuntimeRequest,
    ) {
        let sender = self.sender.clone();
        tokio::spawn(async move {
            #[cfg(test)]
            if let (Some(started), Some(release)) = &work.worker_gate {
                started.notify_one();
                release.notified().await;
            }
            let report = work.report.clone();
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                tokio::task::spawn_blocking(move || report.render()),
            )
            .await
            .map_err(|_| MezError::invalid_state("status history deadline exceeded"))
            .and_then(|joined| {
                joined.map_err(|_| MezError::invalid_state("status history worker failed"))
            })
            .and_then(|result| result);
            if let AsyncRuntimeRequest::CompleteStatusControlInput { result: target, .. } =
                &mut completion
            {
                *target = result;
            }
            let _ = sender
                .send(AsyncRuntimeRequestEnvelope::new(completion))
                .await;
        });
    }

    /// Recognizes only one finite enrollment frame and reserves actor-owned work.
    fn prepare_external_enrollment_input(
        &mut self,
        input: &[u8],
        max_content_length: usize,
        connection: &crate::control::ControlConnectionState,
    ) -> Option<(
        String,
        usize,
        crate::Result<crate::runtime::ExternalEnrollmentWork>,
    )> {
        let (body, consumed) = decode_control_frame(input, max_content_length).ok()?;
        let request = crate::control::parse_json_rpc_request(&body).ok()?;
        if request.method != "agent/external/enroll" {
            return None;
        }
        let prepared = if consumed != input.len() || body.len() > 8192 {
            Err(MezError::invalid_args(
                "external enrollment requires one bounded control frame",
            ))
        } else {
            self.service
                .prepare_external_enrollment(&request, connection)
        };
        Some((request.id, consumed, prepared))
    }

    /// Owns bounded off-actor native observation and always returns settlement
    /// to release its reservation; disconnected callers acquire no client role.
    #[allow(
        clippy::too_many_arguments,
        reason = "ordered control framing and native admission own independent state"
    )]
    fn dispatch_external_enrollment_observation(
        &self,
        request_id: String,
        prepared: crate::Result<crate::runtime::ExternalEnrollmentWork>,
        connection: crate::control::ControlConnectionState,
        mut output_prefix: Vec<u8>,
        consumed: usize,
        reply: tokio::sync::oneshot::Sender<crate::Result<AsyncControlInputResult>>,
    ) {
        let work = match prepared {
            Ok(work) => work,
            Err(error) => {
                output_prefix.extend_from_slice(&encode_control_body(
                    &crate::runtime::runtime_json_rpc_error(
                        &request_id,
                        error.kind(),
                        error.message(),
                    ),
                ));
                let _ = reply.send(Ok(AsyncControlInputResult {
                    output: output_prefix,
                    consumed,
                    connection,
                    connection_cleanup: None,
                    terminal_lifecycle_flush: None,
                }));
                return;
            }
        };
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let worker = work.clone();
            #[cfg(test)]
            if let Some((started, release)) = &worker.worker_gate {
                started.notify_one();
                release.notified().await;
            }
            // Native reads cannot be hard-cancelled. Keep the reservation until
            // this worker actually finishes, even after its cooperative budget;
            // late evidence is rejected by observe/actor commit, never admitted.
            let result = tokio::task::spawn_blocking(move || worker.observe())
                .await
                .map_err(|_| MezError::invalid_state("external enrollment worker failed"))
                .and_then(|result| result);
            let _ = sender
                .send(AsyncRuntimeRequestEnvelope::new(
                    AsyncRuntimeRequest::CompleteExternalEnrollmentInput {
                        work,
                        result,
                        connection,
                        output_prefix,
                        consumed,
                        reply,
                    },
                ))
                .await;
        });
    }

    /// Runs bounded durable usage I/O outside serialized actor ownership. The
    /// task retains settlement after reply loss; a retry reads the same checkpoint.
    fn dispatch_external_usage_commit(
        &self,
        work: crate::runtime::ExternalUsageWork,
        connection: crate::control::ControlConnectionState,
        output_prefix: Vec<u8>,
        consumed: usize,
        reply: tokio::sync::oneshot::Sender<crate::Result<AsyncControlInputResult>>,
    ) {
        let sender = self.sender.clone();
        tokio::spawn(async move {
            #[cfg(test)]
            if let Some((started, release)) = &work.worker_gate {
                started.notify_one();
                release.notified().await;
            }
            let worker = work.clone();
            let result = tokio::task::spawn_blocking(move || {
                worker.store.ingest_external(&worker.report, worker.now)
            })
            .await
            .map_err(|_| MezError::invalid_state("external accounting worker failed"))
            .and_then(|result| result);
            let _ = sender
                .send(AsyncRuntimeRequestEnvelope::new(
                    AsyncRuntimeRequest::CompleteExternalUsageInput {
                        work,
                        result,
                        connection,
                        output_prefix,
                        consumed,
                        reply,
                    },
                ))
                .await;
        });
    }

    /// Admits a control continuation without waiting on the actor's own bounded ingress.
    fn dispatch_control_continuation(&self, continuation: Box<AsyncRuntimeRequest>) {
        let sender = self.sender.clone();
        let task = tokio::spawn(async move {
            let _ = sender
                .send(AsyncRuntimeRequestEnvelope::new(*continuation))
                .await;
        });
        std::mem::drop(task);
    }

    /// Holds newly queued transcript claims and syncs receipts in enqueue order
    /// before returning the producer's reply to the serialized actor.
    pub(super) fn start_transcript_receipt_admission(
        &mut self,
        previous_id: u64,
        reply: TranscriptReceiptReply,
    ) -> Option<TranscriptReceiptReply> {
        let receipts = self
            .side_effect_routes
            .hold_transcript_receipts_after(previous_id);
        if receipts.is_empty() {
            return Some(reply);
        }
        let sender = self.sender.clone();
        let predecessor = self.transcript_receipt_predecessor.take();
        let (finished, successor) = tokio::sync::oneshot::channel();
        self.transcript_receipt_predecessor = Some(successor);
        tokio::spawn(async move {
            if let Some(predecessor) = predecessor {
                let _ = predecessor.await;
            }
            let ids = receipts.iter().map(|(id, _, _)| *id).collect::<Vec<_>>();
            let results = tokio::task::spawn_blocking(move || {
                receipts
                    .into_iter()
                    .map(|(id, store, entries)| (id, store.accept_append_receipt(&entries, id)))
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_else(|error| {
                ids.into_iter()
                    .map(|id| {
                        (
                            id,
                            Err(MezError::invalid_state(format!(
                                "transcript receipt worker failed: {error}"
                            ))),
                        )
                    })
                    .collect()
            });
            let _ = finished.send(());
            let _ = sender
                .send(AsyncRuntimeRequestEnvelope::new(
                    AsyncRuntimeRequest::CompleteTranscriptReceipts { results, reply },
                ))
                .await;
        });
        None
    }

    /// Starts worker preparation for every prompt-history dispatch currently owned by the service.
    pub(super) fn dispatch_pending_agent_prompt_history(&mut self) {
        for dispatch in self.service.take_pending_agent_prompt_history() {
            if !self
                .service
                .claim_agent_prompt_history_preparation(&dispatch)
            {
                continue;
            }
            let sender = self.sender.clone();
            let join_handle = tokio::spawn(async move {
                #[cfg(test)]
                if let (Some(started), Some(release)) = (
                    dispatch.prompt_history_preparation_started.as_ref(),
                    dispatch.prompt_history_preparation_release.as_ref(),
                ) {
                    started.notify_one();
                    release.notified().await;
                }
                let history_work = dispatch.history_work.clone();
                let history = tokio::task::spawn_blocking(move || {
                    crate::runtime::execute_runtime_agent_prompt_history_work(history_work)
                })
                .await
                .map_err(|error| {
                    crate::error::MezError::invalid_state(format!(
                        "prompt history worker failed: {error}"
                    ))
                })
                .and_then(|history| history);
                let _ = sender
                    .send(AsyncRuntimeRequestEnvelope::new(
                        AsyncRuntimeRequest::CompleteAgentPromptHistoryPreparation {
                            dispatch,
                            history,
                        },
                    ))
                    .await;
            });
            std::mem::drop(join_handle);
        }
    }

    /// Executes admitted manual source preparation under its retained capacity
    /// permit. Logical cancellation does not release an in-flight blocking read;
    /// only exact actor completion may adopt it into provider work.
    pub(super) fn dispatch_manual_compaction_preparations(&mut self) {
        for work in self.service.take_manual_compaction_preparations() {
            let sender = self.sender.clone();
            tokio::spawn(async move {
                #[cfg(test)]
                if let Some((started, release, _)) = work.probe.as_ref() {
                    started.notify_one();
                    release.notified().await;
                }
                let read_work = work.clone();
                let result = tokio::task::spawn_blocking(move || read_work.execute_source())
                    .await
                    .map_err(|error| {
                        crate::error::MezError::invalid_state(format!(
                            "manual compaction source worker failed: {error}"
                        ))
                    })
                    .and_then(|result| result);
                let _ = sender
                    .send(AsyncRuntimeRequestEnvelope::new(
                        AsyncRuntimeRequest::CompleteManualCompactionPreparation { work, result },
                    ))
                    .await;
            });
        }
        for work in self.service.take_manual_compaction_requests() {
            let sender = self.sender.clone();
            tokio::spawn(async move {
                #[cfg(test)]
                if let Some((started, release, _)) = work.probe.as_ref() {
                    started.notify_one();
                    release.notified().await;
                }
                let construction = work.clone();
                let result = tokio::task::spawn_blocking(move || construction.execute_request())
                    .await
                    .map_err(|error| {
                        crate::error::MezError::invalid_state(format!(
                            "manual compaction request worker failed: {error}"
                        ))
                    })
                    .and_then(|result| result);
                let _ = sender
                    .send(AsyncRuntimeRequestEnvelope::new(
                        AsyncRuntimeRequest::CompleteManualCompactionRequest {
                            work: Box::new(work),
                            result: Box::new(result),
                        },
                    ))
                    .await;
            });
        }
    }

    /// Checks retained chronology outside the actor, one candidate per conversation.
    pub(super) fn dispatch_bookkeeping_candidates(&mut self) {
        for work in self.service.claim_bookkeeping_candidates() {
            let sender = self.sender.clone();
            tokio::spawn(async move {
                let read_work = work.clone();
                let history = tokio::task::spawn_blocking(move || read_work.execute())
                    .await
                    .map_err(|error| {
                        crate::error::MezError::invalid_state(format!(
                            "bookkeeping worker failed: {error}"
                        ))
                    })
                    .and_then(|history| history);
                let _ = sender
                    .send(AsyncRuntimeRequestEnvelope::new(
                        AsyncRuntimeRequest::CompleteBookkeepingCandidate { work, history },
                    ))
                    .await;
            });
        }
    }

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

    /// Settles cancellation ownership on the serialized actor, using ordinary
    /// exact-client event reduction. Cleanup errors remain visible without
    /// leaking client identifiers, transport payloads, or arbitrary diagnostics.
    pub(super) async fn apply_transport_cancellation_cleanup(
        &mut self,
        cleanup: super::ClientClipboardRouteCleanup,
    ) {
        match cleanup {
            super::ClientClipboardRouteCleanup::Clipboard {
                client_id,
                generation,
            } => {
                self.cleanup_client_clipboard_route(client_id, generation);
            }
            super::ClientClipboardRouteCleanup::Connection {
                mut connection,
                route_result,
                reply,
            } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let route_result = route_result.and(connection.deactivate_x11_route().map(|_| ()));
                let result = if let Some(client_id) = connection.take_disconnect_client_id() {
                    let mut batch = super::RuntimeEventBatch::new();
                    batch.push(super::RuntimeEvent::Client(
                        super::ClientEvent::Disconnected {
                            client_id,
                            reason: "routed control connection ended".to_string(),
                        },
                    ));
                    let result = self.apply_runtime_event_batch(batch).await.map(|_| ());
                    self.notify_event_delivery();
                    result.and(route_result)
                } else {
                    route_result
                };
                if result.is_err() {
                    eprintln!("mez: routed attachment cleanup failed");
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
        }
    }

    /// Captures the exact client view identity used to fence worker rendering.
    pub(super) fn client_render_token(
        &mut self,
        client_id: &mez_core::ids::ClientId,
        role: mez_mux::presentation::ClientViewRole,
    ) -> crate::Result<Option<AsyncClientRenderToken>> {
        let crate::runtime::RuntimeClientRenderIdentity {
            view_source_client_id,
            window_id,
            navigation_revision,
            layout_revision,
            presentation_revision,
            external_editor_session,
            pane_render_generations,
        } = self.service.client_render_identity(client_id, role)?;
        Ok(Some(AsyncClientRenderToken {
            client_id: client_id.clone(),
            view_source_client_id,
            window_id,
            navigation_revision,
            layout_revision,
            presentation_revision,
            external_editor_session,
            pane_render_generations,
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
                        | crate::host::terminal::MouseAction::OpenPaneAgentStatusSelectorIdentity { .. }
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
            AsyncRuntimeRequest::PowerInhibitionStatus { reply } => {
                let _ = reply.send(self.service.power_inhibition_status());
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
                let _ = reply.send(self.current_metrics_snapshot());
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
                        let (view, presentation_ids) = if render {
                            self.service
                                .render_client_view_for_client_with_resolved_config_and_receipts(
                                    &client_id,
                                    role,
                                    client_size,
                                    config.config(),
                                )?
                        } else {
                            (None, Vec::new())
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
                            presentation_ids,
                            view,
                        })
                    });
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::CaptureClientRenderWork {
                client_id,
                role,
                client_size,
                config,
                reply,
            } => {
                self.metrics.render_client_frame_requests =
                    self.metrics.render_client_frame_requests.saturating_add(1);
                let result = self
                    .service
                    .prepare_client_render(&client_id, role)
                    .and_then(|()| {
                        self.resolve_terminal_client_config_snapshot_for_client(&client_id, config)
                    })
                    .and_then(|config| {
                        let render_token = self.client_render_token(&client_id, role)?;
                        let snapshot = self.service.capture_client_render_snapshot(
                            &client_id,
                            role,
                            client_size,
                            config.config(),
                        )?;
                        let apply_overlays = snapshot.requires_actor_overlays();
                        Ok(
                            crate::host::async_runtime::actor_types::AsyncClientRenderWork {
                                client_id,
                                role,
                                render_token,
                                config,
                                snapshot,
                                apply_overlays,
                                #[cfg(test)]
                                composition_started: self
                                    .service
                                    .client_render_composition_probe_for_tests()
                                    .0,
                                #[cfg(test)]
                                composition_release: self
                                    .service
                                    .client_render_composition_probe_for_tests()
                                    .1,
                            },
                        )
                    });
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::CompleteClientRenderWork { work, view, reply } => {
                let result = self
                    .service
                    .prepare_client_render(&work.client_id, work.role)
                    .and_then(|()| {
                        if work.config.generation() != self.terminal_config_generation
                            || self.client_render_token(&work.client_id, work.role)?
                                != work.render_token
                        {
                            return Err(crate::MezError::conflict(
                                "client render work became stale",
                            ));
                        }
                        if work.apply_overlays {
                            self.service.complete_client_render_snapshot(
                                work.role,
                                work.config.config(),
                                view,
                            )
                        } else {
                            Ok((view, Vec::new()))
                        }
                    })
                    .and_then(|(view, presentation_ids)| {
                        let effects = self
                            .service
                            .drain_status_pill_refresh_transition()
                            .side_effects;
                        if !effects.is_empty() {
                            self.queue_runtime_side_effects(effects)?;
                        }
                        Ok(AsyncRenderedClientFrame {
                            config: work.config,
                            render_token: work.render_token,
                            presentation_ids,
                            view,
                        })
                    });
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::CaptureIrohClientRenderWork { client_id, reply } => {
                self.metrics.render_client_frame_requests =
                    self.metrics.render_client_frame_requests.saturating_add(1);
                let result = (|| {
                    let Some(client_size) = self.attached_client_size(&client_id)? else {
                        return Ok(None);
                    };
                    let role = match self
                        .service
                        .session()
                        .clients()
                        .iter()
                        .find(|client| client.id == client_id)
                        .map(|client| client.role)
                    {
                        Some(mez_mux::session::ClientRole::Primary) => {
                            mez_mux::presentation::ClientViewRole::Primary
                        }
                        Some(mez_mux::session::ClientRole::Observer) => {
                            mez_mux::presentation::ClientViewRole::Observer
                        }
                        _ => return Ok(None),
                    };
                    self.service.prepare_client_render(&client_id, role)?;
                    let config = self.resolve_terminal_client_config_snapshot_for_client(
                        &client_id,
                        AsyncTerminalClientConfigInput::Raw(Box::default()),
                    )?;
                    let render_token = self.client_render_token(&client_id, role)?;
                    let snapshot = self.service.capture_client_render_snapshot(
                        &client_id,
                        role,
                        client_size,
                        config.config(),
                    )?;
                    let apply_overlays = snapshot.requires_actor_overlays();
                    Ok(Some(
                        crate::host::async_runtime::actor_types::AsyncClientRenderWork {
                            client_id,
                            role,
                            render_token,
                            config,
                            snapshot,
                            apply_overlays,
                            #[cfg(test)]
                            composition_started: self
                                .service
                                .client_render_composition_probe_for_tests()
                                .0,
                            #[cfg(test)]
                            composition_release: self
                                .service
                                .client_render_composition_probe_for_tests()
                                .1,
                        },
                    ))
                })();
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::CompleteIrohClientRenderWork {
                work,
                view,
                invalidate_output,
                reply,
            } => {
                let result = self
                    .service
                    .prepare_client_render(&work.client_id, work.role)
                    .and_then(|()| {
                        if work.config.generation() != self.terminal_config_generation
                            || self.client_render_token(&work.client_id, work.role)?
                                != work.render_token
                        {
                            return Err(crate::MezError::conflict(
                                "Iroh client render work became stale",
                            ));
                        }
                        if work.apply_overlays {
                            self.service.complete_client_render_snapshot(
                                work.role,
                                work.config.config(),
                                view,
                            )
                        } else {
                            Ok((view, Vec::new()))
                        }
                    })
                    .and_then(|(view, presentation_ids)| {
                        let Some(view) = view else {
                            return Ok(None);
                        };
                        let iroh_status_slot = self
                            .service
                            .terminal_iroh_status_slot(&view, work.config.config());
                        let event_cutoff = self
                            .service
                            .event_log()
                            .map(|event_log| event_log.latest_event_id())
                            .unwrap_or(0);
                        let effects = self
                            .service
                            .drain_status_pill_refresh_transition()
                            .side_effects;
                        if !effects.is_empty() {
                            self.queue_runtime_side_effects(effects)?;
                        }
                        self.ensure_client_render_timers(&work.client_id)?;
                        Ok(Some(AsyncIrohRenderSnapshot {
                            view,
                            render_rate_limit_fps: work.config.config().render_rate_limit_fps,
                            presentation_ids,
                            iroh_status_slot,
                            event_cutoff,
                            invalidate_output,
                        }))
                    });
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::AcknowledgeZenFocusLabelPresentations {
                client_id,
                presentation_ids,
                presented_at_ms,
                reply,
            } => {
                let changed = self.service.acknowledge_zen_focus_label_presentations(
                    &client_id,
                    &presentation_ids,
                    presented_at_ms,
                );
                let result = if changed {
                    self.ensure_client_render_timers(&client_id)
                } else {
                    Ok(0)
                };
                let _ = reply.send(result);
                false
            }
            AsyncRuntimeRequest::RenderClientSideEffect {
                client_id,
                reason,
                config,
                status,
                cursor_blink_elapsed_ms,
                reply,
            } => {
                let result = self.render_client_side_effect(
                    client_id,
                    reason,
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
                retain_connection_cleanup,
                reply,
            } => {
                if let Ok((body, consumed)) = decode_control_frame(&input, max_content_length)
                    && let Some(prepared) =
                        self.service.prepare_status_control_work(&body, &connection)
                {
                    match prepared {
                        Ok(work) => {
                            let completion = AsyncRuntimeRequest::CompleteStatusControlInput {
                                work: Some(Box::new(work.clone())),
                                result: Ok(String::new()),
                                connection,
                                output_prefix: Vec::new(),
                                consumed_prefix: consumed,
                                remaining_input: input[consumed..].to_vec(),
                                max_content_length,
                                snapshots: None,
                                reply,
                            };
                            self.dispatch_status_control_query(work, completion);
                        }
                        Err(body) => {
                            self.dispatch_control_continuation(Box::new(
                                AsyncRuntimeRequest::CompleteStatusControlInput {
                                    work: None,
                                    result: Ok(body),
                                    output_prefix: Vec::new(),
                                    consumed_prefix: consumed,
                                    remaining_input: input[consumed..].to_vec(),
                                    max_content_length,
                                    snapshots: None,
                                    connection,
                                    reply,
                                },
                            ));
                        }
                    }
                    return false;
                }
                if let Some((id, consumed, prepared)) =
                    self.prepare_external_enrollment_input(&input, max_content_length, &connection)
                {
                    self.dispatch_external_enrollment_observation(
                        id,
                        prepared,
                        connection,
                        Vec::new(),
                        consumed,
                        reply,
                    );
                    return false;
                }
                if let Ok((body, consumed)) = decode_control_frame(&input, max_content_length)
                    && let Ok(request) = crate::control::parse_json_rpc_request(&body)
                    && request.method == "agent/external/usage"
                {
                    let prepared = if consumed == input.len() {
                        self.service.prepare_external_usage(&request, &connection)
                    } else {
                        Err(MezError::invalid_args(
                            "external usage requires one control frame per request",
                        ))
                    };
                    match prepared {
                        Ok(work) => self.dispatch_external_usage_commit(
                            work,
                            connection,
                            Vec::new(),
                            consumed,
                            reply,
                        ),
                        Err(error) => {
                            let body = crate::runtime::runtime_json_rpc_error(
                                &request.id,
                                error.kind(),
                                error.message(),
                            );
                            let _ = reply.send(Ok(AsyncControlInputResult {
                                output: encode_control_body(&body),
                                consumed: input.len(),
                                connection,
                                connection_cleanup: None,
                                terminal_lifecycle_flush: None,
                            }));
                        }
                    }
                    return false;
                }
                let previous_lifecycle_state = self.service.lifecycle_state();
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
                // Every frame re-enters actor admission so later history work
                // cannot hide behind an ordinary synchronous first frame.
                let frame_len = decode_control_frame(&input, max_content_length)
                    .map(|(_, consumed)| consumed)
                    .unwrap_or(input.len());
                self.record_terminal_control_request_metrics(
                    &input[..frame_len],
                    max_content_length,
                );
                let transition_result =
                    self.service.handle_control_input_for_connection_transition(
                        &input[..frame_len],
                        max_content_length,
                        &mut connection,
                    );
                let connection_cleanup = retain_connection_cleanup.then(|| {
                    Box::new(
                        crate::host::async_runtime::config::ControlConnectionCleanupLease {
                            cleanup_tx: self.client_clipboard_route_cleanup_tx.clone(),
                            connection: Some(Box::new(connection.clone())),
                        },
                    )
                });
                let mut result = transition_result.and_then(|(output, consumed, transition)| {
                    self.queue_deferred_pane_io_side_effects_from_service()?;
                    self.queue_runtime_side_effects(transition.side_effects)?;
                    self.queue_pending_provider_dispatch_side_effects()?;
                    self.queue_pending_deferred_agent_command_side_effects()?;
                    self.queue_shell_lifecycle_timer_side_effects()?;
                    if let Some(client_id) = connection.caller_client_id().cloned() {
                        self.ensure_client_render_timers_or_defer_to_pending_render(&client_id)?;
                    }
                    Ok(AsyncControlInputResult {
                        output,
                        consumed,
                        connection,
                        connection_cleanup,
                        terminal_lifecycle_flush: None,
                    })
                });
                let terminal_lifecycle_deferred = self
                    .defer_terminal_lifecycle_until_response_flush(
                        previous_lifecycle_state,
                        &mut result,
                    );
                let should_notify = result.as_ref().is_ok_and(|result| result.consumed > 0);
                // Wait for this frame's receipts before submitting its successor.
                // Retain initialization cleanup until the aggregate reply is owned
                // by the transport; a lost continuation must still release it.
                let reply = if frame_len < input.len()
                    && result.is_ok()
                    && !terminal_lifecycle_deferred
                {
                    let (frame_reply, frame_result) =
                        tokio::sync::oneshot::channel::<crate::Result<AsyncControlInputResult>>();
                    let sender = self.sender.clone();
                    let remaining_input = input[frame_len..].to_vec();
                    tokio::spawn(async move {
                        let aggregate = async {
                            let mut first = frame_result.await.map_err(|_| {
                                MezError::invalid_state("control frame lost its receipt reply")
                            })??;
                            let (next_reply, next_result) = tokio::sync::oneshot::channel();
                            sender
                                .send(AsyncRuntimeRequestEnvelope::new(
                                    AsyncRuntimeRequest::HandleControlInput {
                                        input: remaining_input,
                                        max_content_length,
                                        connection: first.connection.clone(),
                                        retain_connection_cleanup: false,
                                        reply: next_reply,
                                    },
                                ))
                                .await
                                .map_err(|_| {
                                    MezError::invalid_state("control continuation lost its actor")
                                })?;
                            let mut next = next_result.await.map_err(|_| {
                                MezError::invalid_state("control continuation lost its reply")
                            })??;
                            first.output.extend_from_slice(&next.output);
                            next.output = first.output;
                            next.consumed = first.consumed.saturating_add(next.consumed);
                            if next.connection_cleanup.is_none() {
                                next.connection_cleanup = first.connection_cleanup.take();
                            }
                            Ok(next)
                        }
                        .await;
                        let _ = reply.send(aggregate);
                    });
                    frame_reply
                } else {
                    reply
                };
                if let Some(TranscriptReceiptReply::Control(reply, result)) = self
                    .start_transcript_receipt_admission(
                        previous_id,
                        TranscriptReceiptReply::Control(reply, Box::new(result)),
                    )
                {
                    let _ = reply.send(*result);
                }
                if should_notify {
                    self.notify_event_delivery();
                }
                if !terminal_lifecycle_deferred {
                    self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                }
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
                if let Ok((body, consumed)) = decode_control_frame(&input, max_content_length)
                    && let Some(prepared) =
                        self.service.prepare_status_control_work(&body, &connection)
                {
                    match prepared {
                        Ok(work) => {
                            let completion = AsyncRuntimeRequest::CompleteStatusControlInput {
                                work: Some(Box::new(work.clone())),
                                result: Ok(String::new()),
                                connection,
                                output_prefix,
                                consumed_prefix: consumed_prefix.saturating_add(consumed),
                                remaining_input: input[consumed..].to_vec(),
                                max_content_length,
                                snapshots: Some(snapshots),
                                reply,
                            };
                            self.dispatch_status_control_query(work, completion);
                        }
                        Err(body) => {
                            self.dispatch_control_continuation(Box::new(
                                AsyncRuntimeRequest::CompleteStatusControlInput {
                                    work: None,
                                    result: Ok(body),
                                    output_prefix,
                                    consumed_prefix: consumed_prefix.saturating_add(consumed),
                                    remaining_input: input[consumed..].to_vec(),
                                    max_content_length,
                                    snapshots: Some(snapshots),
                                    connection,
                                    reply,
                                },
                            ));
                        }
                    }
                    return false;
                }
                if let Some((id, consumed, prepared)) =
                    self.prepare_external_enrollment_input(&input, max_content_length, &connection)
                {
                    self.dispatch_external_enrollment_observation(
                        id,
                        prepared,
                        connection,
                        output_prefix,
                        consumed_prefix.saturating_add(consumed),
                        reply,
                    );
                    return false;
                }
                if let Ok((body, consumed)) = decode_control_frame(&input, max_content_length)
                    && let Ok(request) = crate::control::parse_json_rpc_request(&body)
                    && request.method == "agent/external/usage"
                {
                    let prepared = if consumed == input.len() {
                        self.service.prepare_external_usage(&request, &connection)
                    } else {
                        Err(MezError::invalid_args(
                            "external usage requires one control frame per request",
                        ))
                    };
                    match prepared {
                        Ok(work) => self.dispatch_external_usage_commit(
                            work,
                            connection,
                            output_prefix,
                            consumed_prefix.saturating_add(consumed),
                            reply,
                        ),
                        Err(error) => {
                            let body = crate::runtime::runtime_json_rpc_error(
                                &request.id,
                                error.kind(),
                                error.message(),
                            );
                            output_prefix.extend_from_slice(&encode_control_body(&body));
                            let _ = reply.send(Ok(AsyncControlInputResult {
                                output: output_prefix,
                                consumed: consumed_prefix.saturating_add(input.len()),
                                connection,
                                connection_cleanup: None,
                                terminal_lifecycle_flush: None,
                            }));
                        }
                    }
                    return false;
                }
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
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
                                    connection_cleanup: None,
                                    terminal_lifecycle_flush: None,
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
                        self.queue_pending_deferred_agent_command_side_effects()?;
                        self.queue_shell_lifecycle_timer_side_effects()?;
                        if let Some(client_id) = connection.caller_client_id().cloned() {
                            self.ensure_client_render_timers_or_defer_to_pending_render(
                                &client_id,
                            )?;
                        }
                        Ok(consumed)
                    });
                let mut terminal_lifecycle_deferred = false;
                match result {
                    Err(error) => {
                        if let Some(TranscriptReceiptReply::Control(reply, result)) = self
                            .start_transcript_receipt_admission(
                                previous_id,
                                TranscriptReceiptReply::Control(reply, Box::new(Err(error))),
                            )
                        {
                            let _ = reply.send(*result);
                        }
                    }
                    Ok(consumed) if remaining_input.is_empty() => {
                        let mut result = Ok(AsyncControlInputResult {
                            output: output_prefix,
                            consumed: consumed_prefix.saturating_add(consumed),
                            connection,
                            connection_cleanup: None,
                            terminal_lifecycle_flush: None,
                        });
                        terminal_lifecycle_deferred = self
                            .defer_terminal_lifecycle_until_response_flush(
                                previous_lifecycle_state,
                                &mut result,
                            );
                        if let Some(TranscriptReceiptReply::Control(reply, result)) = self
                            .start_transcript_receipt_admission(
                                previous_id,
                                TranscriptReceiptReply::Control(reply, Box::new(result)),
                            )
                        {
                            let _ = reply.send(*result);
                        }
                        self.notify_event_delivery();
                    }
                    Ok(consumed) => {
                        let consumed_prefix = consumed_prefix.saturating_add(consumed);
                        let continuation = AsyncRuntimeRequest::HandleControlInputWithSnapshots {
                            input: remaining_input,
                            output_prefix,
                            consumed_prefix,
                            record_metrics: false,
                            max_content_length,
                            connection,
                            snapshots,
                            reply,
                        };
                        if let Some(TranscriptReceiptReply::ControlContinuation(continuation)) =
                            self.start_transcript_receipt_admission(
                                previous_id,
                                TranscriptReceiptReply::ControlContinuation(Box::new(continuation)),
                            )
                        {
                            self.dispatch_control_continuation(continuation);
                        }
                    }
                }
                if !terminal_lifecycle_deferred {
                    self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                }
                false
            }
            AsyncRuntimeRequest::CompleteExternalEnrollmentInput {
                work,
                result,
                connection,
                mut output_prefix,
                consumed,
                reply,
            } => {
                let body = self
                    .service
                    .complete_external_enrollment(work, result, &connection);
                output_prefix.extend_from_slice(&encode_control_body(&body));
                let _ = reply.send(Ok(AsyncControlInputResult {
                    output: output_prefix,
                    consumed,
                    connection,
                    connection_cleanup: None,
                    terminal_lifecycle_flush: None,
                }));
                self.notify_event_delivery();
                false
            }
            AsyncRuntimeRequest::CompleteExternalUsageInput {
                work,
                result,
                connection,
                mut output_prefix,
                consumed,
                reply,
            } => {
                let body = self.service.complete_external_usage(work, result);
                output_prefix.extend_from_slice(&encode_control_body(&body));
                let _ = reply.send(Ok(AsyncControlInputResult {
                    output: output_prefix,
                    consumed,
                    connection,
                    connection_cleanup: None,
                    terminal_lifecycle_flush: None,
                }));
                self.notify_event_delivery();
                false
            }
            AsyncRuntimeRequest::CompleteStatusControlInput {
                work,
                result,
                connection,
                mut output_prefix,
                consumed_prefix,
                remaining_input,
                max_content_length,
                snapshots,
                reply,
            } => {
                let body = match work {
                    Some(work) => self.service.complete_status_control_work(*work, result),
                    None => result.unwrap_or_else(|error| {
                        crate::runtime::runtime_json_rpc_error(
                            "null",
                            error.kind(),
                            error.message(),
                        )
                    }),
                };
                output_prefix.extend_from_slice(&encode_control_body(&body));
                if remaining_input.is_empty() {
                    let _ = reply.send(Ok(AsyncControlInputResult {
                        output: output_prefix,
                        consumed: consumed_prefix,
                        connection,
                        connection_cleanup: None,
                        terminal_lifecycle_flush: None,
                    }));
                } else {
                    if let Some(snapshots) = snapshots {
                        self.dispatch_control_continuation(Box::new(
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
                        ));
                    } else {
                        // Retain plain ingress: a report cannot grant repository access.
                        let (next_reply, next_result) = tokio::sync::oneshot::channel();
                        self.dispatch_control_continuation(Box::new(
                            AsyncRuntimeRequest::HandleControlInput {
                                input: remaining_input,
                                max_content_length,
                                connection,
                                retain_connection_cleanup: false,
                                reply: next_reply,
                            },
                        ));
                        tokio::spawn(async move {
                            let result = next_result
                                .await
                                .map_err(|_| {
                                    MezError::invalid_state("control continuation lost its reply")
                                })
                                .and_then(|result| result)
                                .map(|mut result| {
                                    output_prefix.extend_from_slice(&result.output);
                                    result.output = output_prefix;
                                    result.consumed =
                                        consumed_prefix.saturating_add(result.consumed);
                                    result
                                });
                            let _ = reply.send(result);
                        });
                    }
                }
                self.notify_event_delivery();
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
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
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
                let mut terminal_lifecycle_deferred = false;
                if let Err(error) = queued {
                    if let Some(TranscriptReceiptReply::Control(reply, result)) = self
                        .start_transcript_receipt_admission(
                            previous_id,
                            TranscriptReceiptReply::Control(reply, Box::new(Err(error))),
                        )
                    {
                        let _ = reply.send(*result);
                    }
                } else if remaining_input.is_empty() {
                    let mut result = Ok(AsyncControlInputResult {
                        output: output_prefix,
                        consumed: consumed_prefix,
                        connection,
                        connection_cleanup: None,
                        terminal_lifecycle_flush: None,
                    });
                    terminal_lifecycle_deferred = self
                        .defer_terminal_lifecycle_until_response_flush(
                            previous_lifecycle_state,
                            &mut result,
                        );
                    if let Some(TranscriptReceiptReply::Control(reply, result)) = self
                        .start_transcript_receipt_admission(
                            previous_id,
                            TranscriptReceiptReply::Control(reply, Box::new(result)),
                        )
                    {
                        let _ = reply.send(*result);
                    }
                    self.notify_event_delivery();
                } else {
                    let continuation = AsyncRuntimeRequest::HandleControlInputWithSnapshots {
                        input: remaining_input,
                        output_prefix,
                        consumed_prefix,
                        record_metrics: false,
                        max_content_length,
                        connection,
                        snapshots,
                        reply,
                    };
                    if let Some(TranscriptReceiptReply::ControlContinuation(continuation)) = self
                        .start_transcript_receipt_admission(
                            previous_id,
                            TranscriptReceiptReply::ControlContinuation(Box::new(continuation)),
                        )
                    {
                        self.dispatch_control_continuation(continuation);
                    }
                }
                if !terminal_lifecycle_deferred {
                    self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                }
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
                        self.queue_peer_message_delivery_timer_if_needed(now_ms)?;
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
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
                let stale_coordinate_input = render_token
                    .as_ref()
                    .filter(|_| Self::step_uses_render_coordinates(&step))
                    .map(|token| {
                        self.service
                            .client_render_identity(
                                &primary_client_id,
                                mez_mux::presentation::ClientViewRole::Primary,
                            )
                            .map(
                                |crate::runtime::RuntimeClientRenderIdentity {
                                     view_source_client_id,
                                     window_id,
                                     navigation_revision,
                                     layout_revision,
                                     presentation_revision,
                                     external_editor_session,
                                     pane_render_generations,
                                 }| {
                                    token.client_id != primary_client_id
                                        || token.view_source_client_id != view_source_client_id
                                        || token.window_id != window_id
                                        || token.navigation_revision != navigation_revision
                                        || token.layout_revision != layout_revision
                                        || token.presentation_revision != presentation_revision
                                        || token.external_editor_session != external_editor_session
                                        || token.pane_render_generations != pane_render_generations
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
                    let suppress_host_clipboard_copy = self
                        .client_clipboard_routes
                        .contains_key(&primary_client_id);
                    let (mut application, transition) = self
                        .service
                        .apply_attached_terminal_step_transition_with_clipboard_policy(
                            &primary_client_id,
                            &step,
                            suppress_host_clipboard_copy,
                        )?;
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
                    self.queue_pending_deferred_agent_command_side_effects()?;
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
                    self.dispatch_pending_agent_prompt_history();
                    // Deferred slash commands queued by this prompt submission
                    // become worker-claimed effects in the same drain, so the
                    // actor request that applied the input never performs the
                    // command's store or filesystem read itself.
                    self.queue_pending_deferred_agent_command_side_effects()?;
                    Ok(application)
                });
                if let Some(TranscriptReceiptReply::TerminalStep(reply, result)) = self
                    .start_transcript_receipt_admission(
                        previous_id,
                        TranscriptReceiptReply::TerminalStep(reply, result),
                    )
                {
                    let _ = reply.send(result);
                }
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
                        let resize_timer =
                            self.resize_debounce_timer_side_effects(&primary_client_id)?;
                        self.queue_runtime_side_effects(resize_timer)?;
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
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
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
                if let Some(TranscriptReceiptReply::Command(reply, result)) = self
                    .start_transcript_receipt_admission(
                        previous_id,
                        TranscriptReceiptReply::Command(reply, result),
                    )
                {
                    let _ = reply.send(result);
                }
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
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
                let result = self
                    .service
                    .execute_agent_shell_command_async(&primary_client_id, &input)
                    .await
                    .and_then(|output| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_shell_lifecycle_timer_side_effects()?;
                        self.queue_pending_provider_dispatch_side_effects()?;
                        self.queue_pending_deferred_agent_command_side_effects()?;
                        self.dispatch_pending_agent_prompt_history();
                        Ok(output)
                    });
                let should_notify = result.is_ok();
                if let Some(TranscriptReceiptReply::Command(reply, result)) = self
                    .start_transcript_receipt_admission(
                        previous_id,
                        TranscriptReceiptReply::Command(reply, result),
                    )
                {
                    let _ = reply.send(result);
                }
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
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
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
                        self.queue_pending_deferred_agent_command_side_effects()?;
                        self.dispatch_pending_agent_prompt_history();
                        Ok(output)
                    });
                let should_notify = result.is_ok();
                if let Some(TranscriptReceiptReply::Command(reply, result)) = self
                    .start_transcript_receipt_admission(
                        previous_id,
                        TranscriptReceiptReply::Command(reply, result),
                    )
                {
                    let _ = reply.send(result);
                }
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
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
                let result = self
                    .service
                    .complete_agent_shell_provider_info_refresh(&primary_client_id, &input, outcome)
                    .and_then(|output| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        Ok(output)
                    });
                let should_notify = result.is_ok();
                if let Some(TranscriptReceiptReply::Command(reply, result)) = self
                    .start_transcript_receipt_admission(
                        previous_id,
                        TranscriptReceiptReply::Command(reply, result),
                    )
                {
                    let _ = reply.send(result);
                }
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
            AsyncRuntimeRequest::CompleteBookkeepingCandidate { work, history } => {
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
                let result = self.service.complete_bookkeeping_candidate(work, history);
                if result.as_ref().is_ok_and(|applied| *applied) {
                    let _ = self.service.start_ready_agent_turns();
                    let _ = self.queue_deferred_pane_io_side_effects_from_service();
                    let _ = self.queue_pending_provider_dispatch_side_effects();
                    let _ = self.start_transcript_receipt_admission(
                        previous_id,
                        TranscriptReceiptReply::Startup,
                    );
                    self.dispatch_pending_agent_prompt_history();
                }
                self.dispatch_bookkeeping_candidates();
                self.notify_event_delivery();
                false
            }
            AsyncRuntimeRequest::CompleteManualCompactionPreparation { work, result } => {
                let pane_id = work.pane_id.clone();
                let applied = self
                    .service
                    .complete_manual_compaction_preparation(&work, result);
                #[cfg(test)]
                if let Some((_, _, completed)) = work.probe.as_ref() {
                    completed.notify_one();
                }
                if applied.as_ref().is_ok_and(|applied| *applied) || applied.is_err() {
                    let _ = self.queue_pending_provider_dispatch_side_effects();
                    let transition = self.service.runtime_pane_transition_with_render(
                        &pane_id,
                        true,
                        Some(crate::runtime::RenderInvalidationReason::AgentPrompt),
                    );
                    let _ = self.queue_runtime_side_effects(transition.side_effects);
                    self.dispatch_pending_agent_prompt_history();
                    self.notify_event_delivery();
                }
                false
            }
            AsyncRuntimeRequest::CompleteManualCompactionRequest { work, result } => {
                let pane_id = work.owner.pane_id.clone();
                let applied = self
                    .service
                    .complete_manual_compaction_request(&work, *result);
                #[cfg(test)]
                if let Some((_, _, completed)) = work.probe.as_ref() {
                    completed.notify_one();
                }
                if applied.as_ref().is_ok_and(|applied| *applied) || applied.is_err() {
                    let _ = self.queue_pending_provider_dispatch_side_effects();
                    let transition = self.service.runtime_pane_transition_with_render(
                        &pane_id,
                        true,
                        Some(crate::runtime::RenderInvalidationReason::AgentPrompt),
                    );
                    let _ = self.queue_runtime_side_effects(transition.side_effects);
                    self.dispatch_pending_agent_prompt_history();
                    self.notify_event_delivery();
                }
                false
            }
            AsyncRuntimeRequest::CompleteAgentPromptHistoryPreparation { dispatch, history } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let primary_client_id = dispatch.primary_client_id.clone();
                let result = self
                    .service
                    .complete_agent_prompt_history_preparation(&dispatch, history)
                    .and_then(|applied| {
                        if applied {
                            self.queue_deferred_pane_io_side_effects_from_service()?;
                            self.queue_pending_provider_dispatch_side_effects()?;
                            self.queue_runtime_side_effects(vec![
                                crate::runtime::RuntimeSideEffect::RenderClient {
                                    client_id: primary_client_id.clone(),
                                    reason: crate::runtime::RenderInvalidationReason::AgentPrompt,
                                },
                            ])?;
                        }
                        Ok(applied)
                    });
                let should_notify =
                    result.as_ref().is_ok_and(|applied| *applied) || result.is_err();
                if result.is_err() {
                    let _ = self.queue_runtime_side_effects(vec![
                        crate::runtime::RuntimeSideEffect::RenderClient {
                            client_id: primary_client_id,
                            reason: crate::runtime::RenderInvalidationReason::AgentPrompt,
                        },
                    ]);
                }
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
            AsyncRuntimeRequest::PrepareConfiguredAgentProviderTask { turn_id, reply } => {
                let result = self.service.prepare_agent_provider_work(&turn_id);
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
                    if let Some(mut dispatch) = dispatch {
                        self.timers.next_provider_claim_generation =
                            self.timers.next_provider_claim_generation.saturating_add(1);
                        let generation = self.timers.next_provider_claim_generation;
                        let transition = match self.service.record_claimed_agent_provider_task(
                            &dispatch,
                            generation,
                            DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS,
                        ) {
                            Ok(transition) => transition,
                            Err(error) => {
                                self.service
                                    .fail_configured_agent_provider_task(&turn_id, &error)?;
                                self.queue_deferred_pane_io_side_effects_from_service()?;
                                self.queue_shell_transaction_timer_side_effects()?;
                                return Ok(None);
                            }
                        };
                        dispatch.claim_generation = generation;
                        if !self.admit_worker_claim_lease(
                            WorkerClaimLease::Provider {
                                agent_id: &agent_id,
                                turn_id: &turn_id,
                                generation,
                            },
                            transition.side_effects,
                        )? {
                            self.queue_deferred_pane_io_side_effects_from_service()?;
                            self.queue_shell_transaction_timer_side_effects()?;
                            return Ok(None);
                        }
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.service.acknowledge_admitted_steering(&dispatch);
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
            #[cfg(test)]
            AsyncRuntimeRequest::RecordClaimedAgentProviderTaskForTests {
                turn_id,
                generation,
                reply,
            } => {
                let result = self
                    .service
                    .record_claimed_agent_provider_generation_for_tests(&turn_id, generation);
                let _ = reply.send(result);
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
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
                let result = self
                    .service
                    .complete_approved_external_action(outcome)
                    .and_then(|applied| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        self.queue_pending_provider_dispatch_side_effects()?;
                        Ok(applied)
                    });
                let should_notify = result.is_ok();
                if let Some(TranscriptReceiptReply::Applied(reply, result)) = self
                    .start_transcript_receipt_admission(
                        previous_id,
                        TranscriptReceiptReply::Applied(reply, result),
                    )
                {
                    let _ = reply.send(result);
                }
                if should_notify {
                    self.notify_event_delivery();
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::ClaimAgentCommandWork {
                primary_client_id,
                pane_id,
                command,
                input,
                claim_generation,
                conversation_id,
                reply,
            } => {
                let result = self.service.claim_agent_command_work(
                    &primary_client_id,
                    &pane_id,
                    &command,
                    &input,
                    claim_generation,
                    &conversation_id,
                );
                let should_notify = result.is_ok();
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                false
            }
            AsyncRuntimeRequest::CompleteAgentCommandWork {
                work,
                outcome,
                reply,
            } => {
                let previous_lifecycle_state = self.service.lifecycle_state();
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
                let result = self
                    .service
                    .complete_agent_command_work(&work, *outcome)
                    .and_then(|applied| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        Ok(applied)
                    });
                let should_notify = result.as_ref().is_ok_and(|applied| *applied);
                if let Some(TranscriptReceiptReply::Applied(reply, result)) = self
                    .start_transcript_receipt_admission(
                        previous_id,
                        TranscriptReceiptReply::Applied(reply, result),
                    )
                {
                    let _ = reply.send(result);
                }
                if should_notify {
                    self.notify_event_delivery();
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                false
            }
            AsyncRuntimeRequest::ClaimRecordBrowserRefresh {
                refresh_key,
                generation,
                reply,
            } => {
                let result = self
                    .service
                    .claim_record_browser_refresh(&refresh_key, generation);
                let should_notify = result.is_ok();
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                false
            }
            AsyncRuntimeRequest::CompleteRecordBrowserRefresh {
                work,
                outcome,
                reply,
            } => {
                let result = self
                    .service
                    .complete_record_browser_refresh(&work, *outcome);
                let should_notify = result.as_ref().is_ok_and(|applied| *applied);
                let _ = reply.send(result);
                if should_notify {
                    self.notify_event_delivery();
                }
                false
            }
            AsyncRuntimeRequest::ClaimAgentCompactionTask {
                pane_id,
                task_generation,
                reply,
            } => {
                let result = self
                    .service
                    .claim_agent_compaction_task(&pane_id, task_generation);
                if self
                    .service
                    .agent_compaction_task_is_claimed(&pane_id, task_generation)
                {
                    let timer = RuntimeSideEffect::ScheduleTimer {
                        key: RuntimeTimerKey::new(
                            RuntimeTimerKind::CompactionClaim,
                            pane_id.clone(),
                            task_generation,
                        ),
                        delay_ms: DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS,
                    };
                    match self.admit_worker_claim_lease(
                        WorkerClaimLease::Compaction {
                            pane_id: &pane_id,
                            generation: task_generation,
                        },
                        vec![timer],
                    ) {
                        Ok(true) => {}
                        Ok(false) => {
                            let _ = reply.send(Ok(None));
                            self.notify_event_delivery();
                            return false;
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            self.notify_event_delivery();
                            return false;
                        }
                    }
                }
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
            AsyncRuntimeRequest::ClaimAgentSessionTitleTask {
                conversation_id,
                reply,
            } => {
                let result = self
                    .service
                    .claim_agent_session_title_task(&conversation_id);
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
            AsyncRuntimeRequest::TakeAgentPresentationResizeWork { pane_id, reply } => {
                let result = self.service.take_agent_presentation_resize_work(&pane_id);
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
            AsyncRuntimeRequest::ApplyAgentPresentationResize { result, reply } => {
                let applied = self
                    .service
                    .apply_agent_presentation_resize_result(*result)
                    .and_then(|applied| {
                        self.queue_deferred_pane_io_side_effects_from_service()?;
                        Ok(applied)
                    });
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
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
                let result = self.apply_runtime_event_batch(batch).await;
                let should_notify = result.as_ref().is_ok_and(|report| report.applied > 0);
                if should_notify {
                    // An applied event can queue a deferred dispatch - a settled
                    // generated title queues one saved-session refresh - and the
                    // control-input and command arms are the only other drains, so
                    // an otherwise idle session would keep the stale page.
                    // Queue pressure retains the service-owned dispatches for
                    // admission after the next side-effect drain.
                    let _ = self.queue_pending_deferred_agent_command_side_effects();
                }
                if should_notify {
                    self.notify_event_delivery();
                }
                self.notify_lifecycle_state_if_changed(previous_lifecycle_state);
                if let Ok(report) = result {
                    if let Some(TranscriptReceiptReply::Event(reply, report)) = self
                        .start_transcript_receipt_admission(
                            previous_id,
                            TranscriptReceiptReply::Event(reply, report),
                        )
                    {
                        let _ = reply.send(Ok(report));
                    }
                } else if let Err(error) = result
                    && let Some(TranscriptReceiptReply::EventError(reply, error)) = self
                        .start_transcript_receipt_admission(
                            previous_id,
                            TranscriptReceiptReply::EventError(reply, error),
                        )
                {
                    let _ = reply.send(Err(error));
                }
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
                let previous_id = self.side_effect_routes.next_transcript_claim_id();
                let result = self
                    .queue_runtime_side_effects(side_effects)
                    .map(|()| queued);
                if result.is_ok() {
                    self.side_effect_routes
                        .mark_direct_transcript_claims_after(previous_id);
                    if let Some(TranscriptReceiptReply::SideEffects(reply, queued)) = self
                        .start_transcript_receipt_admission(
                            previous_id,
                            TranscriptReceiptReply::SideEffects(reply, queued),
                        )
                    {
                        let _ = reply.send(Ok(queued));
                    }
                } else {
                    let _ = reply.send(result);
                }
                false
            }
            AsyncRuntimeRequest::CompleteTranscriptReceipts { results, reply } => {
                let mut failure = None;
                let mut admitted = 0;
                for (id, result) in results {
                    let accepted = result.is_ok();
                    if !self
                        .side_effect_routes
                        .finish_transcript_receipt(id, accepted)
                    {
                        failure.get_or_insert_with(|| {
                            MezError::invalid_state("transcript receipt claim is no longer queued")
                        });
                    } else if accepted {
                        admitted += 1;
                    }
                    if let Err(error) = result {
                        failure.get_or_insert(error);
                    }
                }
                // A successful prefix may be ready even when a later receipt
                // in the same producer submission was rejected.
                self.notify_side_effect_delivery();
                match reply {
                    TranscriptReceiptReply::Startup => {}
                    TranscriptReceiptReply::Recovery(sender, already_recovered) => {
                        let _ = sender.send(already_recovered.saturating_add(admitted));
                    }
                    TranscriptReceiptReply::ControlContinuation(continuation) => {
                        if let Some(error) = failure {
                            if let AsyncRuntimeRequest::HandleControlInputWithSnapshots {
                                reply,
                                ..
                            } = *continuation
                            {
                                let _ = reply.send(Err(error));
                            }
                        } else {
                            self.dispatch_control_continuation(continuation);
                        }
                    }
                    TranscriptReceiptReply::Control(sender, result) => {
                        let _ = sender.send(match failure {
                            Some(error) if result.is_ok() => Err(error),
                            _ => *result,
                        });
                    }
                    TranscriptReceiptReply::Command(sender, result) => {
                        let _ = sender.send(match failure {
                            Some(error) if result.is_ok() => Err(error),
                            _ => result,
                        });
                    }
                    TranscriptReceiptReply::Applied(sender, result) => {
                        let _ = sender.send(match failure {
                            Some(error) if result.is_ok() => Err(error),
                            _ => result,
                        });
                    }
                    TranscriptReceiptReply::TerminalStep(sender, result) => {
                        let _ = sender.send(match failure {
                            Some(error) if result.is_ok() => Err(error),
                            _ => result,
                        });
                    }
                    TranscriptReceiptReply::Event(sender, report) => {
                        let _ = sender.send(failure.map_or(Ok(report), Err));
                    }
                    TranscriptReceiptReply::EventError(sender, error) => {
                        let _ = sender.send(Err(error));
                    }
                    TranscriptReceiptReply::SideEffects(sender, queued) => {
                        let _ = sender.send(failure.map_or(Ok(queued), Err));
                    }
                }
                false
            }
            AsyncRuntimeRequest::DrainAgentProviderDispatchSideEffects { limit, reply } => {
                let _ = reply.send(self.drain_agent_provider_dispatch_side_effects(limit));
                false
            }
            AsyncRuntimeRequest::DrainAgentCommandDispatchSideEffects { limit, reply } => {
                let _ = reply.send(self.drain_agent_command_dispatch_side_effects(limit));
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
            AsyncRuntimeRequest::DrainPersistenceClaims { limit, reply } => {
                let _ = reply.send(self.drain_persistence_claims(limit));
                false
            }
            AsyncRuntimeRequest::RecoverClaimedTranscripts { reply } => {
                let recovered = self.side_effect_routes.recover_claimed_transcripts();
                self.side_effect_routes.retry_failed_transcript_receipts();
                if let Some(TranscriptReceiptReply::Recovery(reply, recovered)) = self
                    .start_transcript_receipt_admission(
                        0,
                        TranscriptReceiptReply::Recovery(reply, recovered),
                    )
                {
                    let _ = reply.send(recovered);
                }
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
