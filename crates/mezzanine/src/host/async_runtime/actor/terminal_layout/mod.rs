//! Ordered terminal layout commands over the existing snapshot custody drain.
//!
//! The accepted AST, original reply and unexecuted attached actions retain one
//! private service owner. Prefix transcript receipts precede selected I/O. The
//! worker asks the actual actor snapshot/resume path to drain its unchanged
//! root, then returns only a descriptive result. Later commands run on the new
//! layout. A lost terminal observer cannot detach an issued custody operation.

use super::*;
use crate::host::terminal::TerminalClientLoopAction;
use crate::runtime::{RuntimeTerminalSequence, RuntimeTerminalSequenceWork};

/// Actual selected I/O result, carrying no native commit or restart authority.
pub(in crate::host::async_runtime) enum TerminalLayoutOutcome {
    Load(Result<()>),
    Status(Result<Vec<crate::ui::command::CommandOutcome>>),
}

/// The original reply after command completion and before buffered actions finish.
pub(in crate::host::async_runtime) struct TerminalLayoutPublication {
    client: ClientId,
    reply: TranscriptReceiptReply,
    result: Result<String>,
    actions: Vec<TerminalClientLoopAction>,
    control: Option<crate::runtime::RuntimeTerminalControlRequest>,
    provisional_mux_actions: usize,
    command_failed: bool,
}

impl AsyncRuntimeSessionActor {
    /// Advances one original sequence through immediate commands and receipts.
    pub(super) fn advance_terminal_layout_sequence(
        &mut self,
        previous_id: u64,
        mut sequence: Box<RuntimeTerminalSequence>,
        reply: TranscriptReceiptReply,
        failure: Option<MezError>,
    ) {
        let failure = original_terminal_reply_error(&reply).or(failure);
        let step = failure.map_or_else(
            || self.service.advance_terminal_layout_sequence(&mut sequence),
            Err,
        );
        let effects = self.queue_terminal_layout_effects();
        let step = match (step, effects) {
            (Ok(Some(RuntimeTerminalSequenceWork::Status(status))), Err(error)) => {
                // The lease was selected, but no report worker was issued.
                // Retire that exact lease before dropping the original plan.
                let _ = self
                    .service
                    .complete_terminal_status_outcomes(&status, Err(error.clone()));
                Err(error)
            }
            (Ok(work), result) => result.map(|()| work),
            (Err(error), _) => Err(error),
        };
        match step {
            Ok(Some(work)) => {
                let next = TranscriptReceiptReply::TerminalLayout {
                    sequence,
                    work: Box::new(work),
                    reply: Box::new(reply),
                };
                if let Some(TranscriptReceiptReply::TerminalLayout {
                    sequence,
                    work,
                    reply,
                }) = self.start_transcript_receipt_admission(previous_id, next)
                {
                    self.dispatch_terminal_layout_work(sequence, work, reply);
                }
            }
            result => {
                let client = sequence.client.clone();
                let control = sequence.control.take();
                let provisional_mux_actions = sequence.provisional_mux_actions;
                let completed = self
                    .service
                    .finish_terminal_layout_sequence(sequence, result.map(|_| ()));
                let completed = completed
                    .and_then(|completed| self.queue_terminal_layout_effects().map(|()| completed));
                let (result, actions) = match completed {
                    Ok((text, actions)) => (Ok(text), actions),
                    Err(error) => (Err(error), Vec::new()),
                };
                let command_failed = result.is_err();
                let final_reply = TranscriptReceiptReply::TerminalLayoutTail(Box::new(
                    TerminalLayoutPublication {
                        client,
                        reply,
                        result,
                        actions,
                        control,
                        provisional_mux_actions,
                        command_failed,
                    },
                ));
                if let Some(TranscriptReceiptReply::TerminalLayoutTail(publication)) =
                    self.start_transcript_receipt_admission(previous_id, final_reply)
                {
                    self.finish_terminal_layout_tail(publication, None);
                }
            }
        }
        self.notify_event_delivery();
    }

    /// Transfers ordinary queued effects exactly as the direct command adapter.
    fn queue_terminal_layout_effects(&mut self) -> Result<()> {
        self.admit_terminal_status_sequence_effects()
    }

    /// Performs captured queries off actor, and uses real actor snapshot control
    /// for the only layout mutation. The original reply is never used as a job.
    pub(super) fn dispatch_terminal_layout_work(
        &mut self,
        sequence: Box<RuntimeTerminalSequence>,
        work: Box<RuntimeTerminalSequenceWork>,
        reply: Box<TranscriptReceiptReply>,
    ) {
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let outcome = match work.as_ref() {
                RuntimeTerminalSequenceWork::Status(status) => {
                    #[cfg(test)]
                    if let (Some(started), Some(release)) = &status.worker_gate {
                        started.notify_one();
                        release.notified().await;
                    }
                    let captured = status.clone();
                    TerminalLayoutOutcome::Status(
                        tokio::time::timeout(
                            std::time::Duration::from_secs(30),
                            tokio::task::spawn_blocking(move || captured.render_outcomes()),
                        )
                        .await
                        .map_err(|_| MezError::invalid_state("terminal status deadline exceeded"))
                        .and_then(|joined| {
                            joined.map_err(|error| {
                                MezError::invalid_state(format!(
                                    "terminal status worker failed: {error}"
                                ))
                            })
                        })
                        .and_then(|result| result),
                    )
                }
                RuntimeTerminalSequenceWork::Load {
                    snapshots,
                    request_key,
                    ..
                } => {
                    let captured = work.clone();
                    let snapshots = snapshots.clone();
                    let id = tokio::task::spawn_blocking(move || captured.snapshot_id())
                        .await
                        .map_err(|error| {
                            MezError::invalid_state(format!(
                                "layout selector worker failed: {error}"
                            ))
                        })
                        .and_then(|result| result);
                    let result = async {
                        let id = id?;
                        let body = serde_json::json!({"jsonrpc":"2.0","id":"terminal-layout","method":"snapshot/resume","params":{"snapshot_id":id,"idempotency_key":request_key}}).to_string();
                        let (control_reply, response) = oneshot::channel();
                        sender.send(AsyncRuntimeRequestEnvelope::new(AsyncRuntimeRequest::HandleControlInputWithSnapshots {
                            input: encode_control_body(&body), output_prefix: Vec::new(), consumed_prefix: 0,
                            record_metrics: false, max_content_length: 1024 * 1024,
                            connection: ControlConnectionState::trusted_existing_client(sequence.client.clone()),
                            snapshots, reply: control_reply,
                        })).await.map_err(|_| MezError::invalid_state("layout restore actor unavailable"))?;
                        let result = response.await.map_err(|_| MezError::invalid_state("layout restore reply unavailable"))??;
                        crate::runtime::RuntimeTerminalSequenceWork::validate_load_response(&result.output)
                    }.await;
                    TerminalLayoutOutcome::Load(result)
                }
            };
            let _ = sender
                .send(AsyncRuntimeRequestEnvelope::new(
                    AsyncRuntimeRequest::CompleteTerminalLayout {
                        sequence,
                        work,
                        outcome: Box::new(outcome),
                        reply,
                    },
                ))
                .await;
        });
    }

    /// Completes only the stage paired with this exact private sequence owner.
    pub(super) fn complete_terminal_layout_work(
        &mut self,
        sequence: &mut RuntimeTerminalSequence,
        work: &RuntimeTerminalSequenceWork,
        outcome: TerminalLayoutOutcome,
    ) -> Result<()> {
        match outcome {
            TerminalLayoutOutcome::Load(result) => self
                .service
                .complete_terminal_layout_work(sequence, work, None, result),
            TerminalLayoutOutcome::Status(result) => {
                self.service
                    .complete_terminal_layout_work(sequence, work, Some(result), Ok(()))
            }
        }
    }

    /// Runs buffered actions only after command-prefix receipt settlement. The
    /// actual attached-step pipeline owns any nested deferred load and reply.
    pub(super) fn finish_terminal_layout_tail(
        &mut self,
        mut publication: Box<TerminalLayoutPublication>,
        failure: Option<MezError>,
    ) {
        if let Some(error) = failure {
            publication.result = Err(error);
            publication.command_failed = true;
        }
        if publication.result.is_err() || publication.actions.is_empty() {
            self.publish_terminal_layout_reply(publication, None);
            return;
        }
        let can_apply = matches!(publication.reply, TranscriptReceiptReply::TerminalStep(..))
            || publication
                .control
                .as_ref()
                .is_some_and(|request| request.is_step());
        if !can_apply {
            publication.result = Err(MezError::invalid_state(
                "buffered terminal actions lost their original step",
            ));
            self.publish_terminal_layout_reply(publication, None);
            return;
        }
        let step = AttachedTerminalClientStepPlan {
            actions: std::mem::take(&mut publication.actions),
            output_lines: Vec::new(),
            output_line_style_spans: Vec::new(),
            input_hangup: false,
            output_hangup: false,
            error_roles: Vec::new(),
        };
        if Self::step_uses_render_coordinates(&step) {
            let cancelled = self
                .service
                .cancel_stale_client_coordinate_input(&publication.client);
            self.publish_terminal_layout_reply(publication, Some(cancelled));
            return;
        }
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let (reply, response) = oneshot::channel();
            let tail = async {
                sender
                    .send(AsyncRuntimeRequestEnvelope::new(
                        AsyncRuntimeRequest::ApplyAttachedTerminalStep {
                            primary_client_id: publication.client.clone(),
                            render_token: None,
                            step,
                            reply,
                        },
                    ))
                    .await
                    .map_err(|_| {
                        MezError::invalid_state("terminal action continuation lost actor")
                    })?;
                response.await.map_err(|_| {
                    MezError::invalid_state("terminal action continuation lost reply")
                })?
            }
            .await;
            let _ = sender
                .send(AsyncRuntimeRequestEnvelope::new(
                    AsyncRuntimeRequest::CompleteTerminalLayoutReply { publication, tail },
                ))
                .await;
        });
    }

    /// Publishes original text/RPC/attached results after all selected work. RPC
    /// terminal-step views are rebuilt with empty input, never replayed input.
    pub(super) fn publish_terminal_layout_reply(
        &mut self,
        mut publication: Box<TerminalLayoutPublication>,
        tail: Option<Result<AttachedClientStepApplication>>,
    ) {
        let tail = match tail {
            Some(Ok(report)) => Some(report),
            Some(Err(error)) => {
                publication.result = Err(error);
                None
            }
            None => None,
        };
        if let Err(error) = self.queue_terminal_layout_effects() {
            publication.result = Err(error);
        }
        let is_step = publication
            .control
            .as_ref()
            .is_some_and(|control| control.is_step());
        let framed_text = match &publication.reply {
            TranscriptReceiptReply::Control(_, result) => result
                .as_ref()
                .as_ref()
                .ok()
                .map(|result| terminal_reply_value(&result.output)),
            TranscriptReceiptReply::ControlContinuation(request) => match request.as_ref() {
                AsyncRuntimeRequest::HandleControlInputWithSnapshots { output_prefix, .. } => {
                    Some(terminal_reply_value(output_prefix))
                }
                _ => None,
            },
            _ => None,
        };
        if is_step {
            let refreshed = (|| {
                let original = publication
                    .control
                    .as_ref()
                    .ok_or_else(|| MezError::invalid_state("terminal step source missing"))?;
                let prefix = framed_text
                    .ok_or_else(|| MezError::invalid_state("terminal step reply missing"))??;
                if let Err(error) = &publication.result {
                    self.service
                        .show_primary_error_overlay(vec![format!("mez error: {error}")])?;
                }
                self.service.refresh_terminal_layout_step_result(
                    &publication.client,
                    original,
                    &prefix["result"],
                    tail.as_ref(),
                    publication.provisional_mux_actions,
                    publication.command_failed,
                )
            })();
            publication.result = refreshed;
        }
        let mut cache_response = None;
        let reply = match publication.reply {
            TranscriptReceiptReply::Command(sender, original) => {
                let _ = sender.send(original.and(publication.result));
                None
            }
            TranscriptReceiptReply::Control(sender, original) => {
                let result = (*original).and_then(|mut result| {
                    cache_response = Some(replace_terminal_control_response(
                        &mut result.output,
                        &publication.result,
                    ));
                    cache_response.as_ref().map_or(Ok(()), |result| {
                        result.as_ref().map(|_| ()).map_err(Clone::clone)
                    })?;
                    Ok(result)
                });
                if let Err(error) = &result {
                    cache_response = Some(Err(error.clone()));
                }
                let _ = sender.send(result);
                None
            }
            TranscriptReceiptReply::ControlContinuation(mut request) => {
                let result = if let AsyncRuntimeRequest::HandleControlInputWithSnapshots {
                    output_prefix,
                    ..
                } = request.as_mut()
                {
                    replace_terminal_control_response(output_prefix, &publication.result)
                } else {
                    Err(MezError::invalid_state(
                        "terminal control continuation changed kind",
                    ))
                };
                cache_response = Some(result.clone());
                match result {
                    Ok(_) => Some(TranscriptReceiptReply::ControlContinuation(request)),
                    Err(error) => {
                        if let AsyncRuntimeRequest::HandleControlInputWithSnapshots {
                            reply, ..
                        } = *request
                        {
                            let _ = reply.send(Err(error));
                        }
                        None
                    }
                }
            }
            TranscriptReceiptReply::TerminalStep(sender, original) => {
                let report = original.and_then(|mut report| {
                    if publication.command_failed {
                        report.mux_actions_applied = report
                            .mux_actions_applied
                            .saturating_sub(publication.provisional_mux_actions);
                    }
                    if let Some(tail) = tail {
                        report = merge_terminal_reports(report, tail);
                    }
                    if let Err(error) = &publication.result {
                        self.service
                            .show_primary_error_overlay(vec![format!("mez error: {error}")])?;
                        report.view_refresh_required = true;
                        report.full_redraw_required = true;
                    }
                    Ok(report)
                });
                let _ = sender.send(report);
                None
            }
            other => Some(other),
        };
        if let Some(control) = publication.control {
            self.service.remember_terminal_layout_control_result(
                control,
                cache_response.unwrap_or_else(|| {
                    Err(MezError::invalid_state(
                        "terminal control response unavailable",
                    ))
                }),
            );
        }
        if let Some(reply) = reply {
            match reply {
                TranscriptReceiptReply::ControlContinuation(request) => {
                    self.dispatch_control_continuation(request)
                }
                other => self.dispatch_control_continuation(Box::new(
                    AsyncRuntimeRequest::CompleteTranscriptReceipts {
                        results: Vec::new(),
                        reply: other,
                    },
                )),
            }
        }
        self.notify_event_delivery();
    }
}

/// Updates only the last original command response; prior framed replies stay exact.
fn replace_terminal_control_response(
    output: &mut Vec<u8>,
    result: &Result<String>,
) -> Result<String> {
    let mut offset = 0;
    let mut last = None;
    while offset < output.len() {
        let (body, consumed) = decode_control_frame(&output[offset..], usize::MAX)?;
        last = Some((offset, body));
        offset += consumed;
    }
    let (start, body) =
        last.ok_or_else(|| MezError::invalid_state("terminal control reply missing"))?;
    let value: serde_json::Value = serde_json::from_str(&body)
        .map_err(|_| MezError::invalid_state("terminal control reply malformed"))?;
    let id = value
        .get("id")
        .ok_or_else(|| MezError::invalid_state("terminal control reply ID missing"))?
        .to_string();
    let body = match result {
        Ok(result) => format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#),
        Err(error) => crate::runtime::runtime_json_rpc_error(&id, error.kind(), error.message()),
    };
    output.truncate(start);
    output.extend_from_slice(&encode_control_body(&body));
    Ok(body)
}

/// Combines actual applied prefixes without replaying input or clipboard effects.
fn merge_terminal_reports(
    mut first: AttachedClientStepApplication,
    mut next: AttachedClientStepApplication,
) -> AttachedClientStepApplication {
    first.forwarded_bytes = first.forwarded_bytes.saturating_add(next.forwarded_bytes);
    first.mux_actions_applied = first
        .mux_actions_applied
        .saturating_add(next.mux_actions_applied);
    first.mouse_actions_reported = first
        .mouse_actions_reported
        .saturating_add(next.mouse_actions_reported);
    first.agent_prompt_inputs_applied = first
        .agent_prompt_inputs_applied
        .saturating_add(next.agent_prompt_inputs_applied);
    first
        .unsupported_actions
        .append(&mut next.unsupported_actions);
    first.view_refresh_required |= next.view_refresh_required;
    first.full_redraw_required |= next.full_redraw_required;
    first.registry_persistence_required |= next.registry_persistence_required;
    if first.client_clipboard_write.is_none() {
        first.client_clipboard_write = next.client_clipboard_write;
    }
    first
}

/// A failure after source capture still precedes deferred layout issuance.
fn original_terminal_reply_error(reply: &TranscriptReceiptReply) -> Option<MezError> {
    match reply {
        TranscriptReceiptReply::Command(_, Err(error))
        | TranscriptReceiptReply::TerminalStep(_, Err(error)) => Some(error.clone()),
        TranscriptReceiptReply::Control(_, result) => result.as_ref().as_ref().err().cloned(),
        TranscriptReceiptReply::Applied(_, Err(error))
        | TranscriptReceiptReply::EventError(_, error) => Some(error.clone()),
        _ => None,
    }
}

/// Reads only the original last response without changing earlier framed replies.
fn terminal_reply_value(output: &[u8]) -> Result<serde_json::Value> {
    let mut offset = 0;
    let mut last = None;
    while offset < output.len() {
        let (body, consumed) = decode_control_frame(&output[offset..], usize::MAX)?;
        last = Some(body);
        offset += consumed;
    }
    serde_json::from_str(&last.ok_or_else(|| MezError::invalid_state("terminal response missing"))?)
        .map_err(|_| MezError::invalid_state("terminal response malformed"))
}
