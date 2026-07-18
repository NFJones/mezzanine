//! Agent shell transaction start and completion.

use super::{
    ActionContentBlock, ActionResult, ActionStatus, AgentActionPayload, AgentTurnState,
    ApplyPatchTransactionPhase, EventKind, HookEvent, MezError, PaneReadinessState, Result,
    RunningShellTransactionKind, RuntimeSessionService, apply_patch_transaction_phase,
    current_unix_millis, decode_shell_output_transport_with_diagnostics, json_escape,
    local_action_plan, postprocess_shell_action_success_output,
    runtime_agent_turn_state_from_action_results, runtime_agent_turn_state_name,
    runtime_execution_ready_for_provider_continuation, runtime_post_shell_hook_payload,
    runtime_running_shell_transaction_kind_name, shell_action_failure_diagnostic,
    shell_command_result_content,
};

impl RuntimeSessionService {
    /// Sends any deferred transaction payload after the shell wrapper receiver
    /// has started.
    pub(crate) fn observe_agent_shell_transaction_start(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        turn_id: &str,
        _agent_id: &str,
        pane_id: &str,
    ) -> Result<usize> {
        let Some(transaction) = self.process.running_shell_transactions.get(marker).cloned() else {
            return Ok(0);
        };
        if transaction.turn_id != turn_id
            || transaction.pane_id != pane_id
            || output_pane_id != pane_id
        {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "start-marker-metadata-mismatch",
                "shell transaction start marker metadata does not match runtime dispatch state",
            );
        }
        if self
            .process
            .shell_transaction_started_markers
            .contains(marker)
        {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "duplicate-start-marker",
                "shell transaction emitted a duplicate start marker",
            );
        }
        self.process
            .shell_transaction_started_markers
            .insert(marker.to_string());
        let kind_name = runtime_running_shell_transaction_kind_name(&transaction.kind).to_string();
        let payload = self
            .process
            .running_shell_transactions
            .get_mut(marker)
            .and_then(|transaction| transaction.pending_input_payload.take());
        if let Some(transaction) = self.process.running_shell_transactions.get_mut(marker) {
            transaction.started_at_unix_ms = current_unix_millis();
        }
        let Some(payload) = payload else {
            return Ok(1);
        };
        let payload_len = payload.len();
        if let Err(error) = self.write_runtime_pane_input_priority(pane_id, &payload) {
            self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
            return Ok(1);
        }
        self.append_agent_trace_turn_event(
            pane_id,
            turn_id,
            &format!(
                "shell_transaction payload_sent marker={} kind={} bytes={}",
                marker, kind_name, payload_len
            ),
        )?;
        Ok(1)
    }

    /// Runs the observe agent shell transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn observe_agent_shell_transaction_end(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        turn_id: &str,
        agent_id: &str,
        pane_id: &str,
        exit_code: i32,
    ) -> Result<usize> {
        let Some(transaction_ref) = self.process.running_shell_transactions.get(marker).cloned()
        else {
            return Ok(0);
        };
        self.append_agent_trace_turn_event(
            pane_id,
            turn_id,
            &format!(
                "shell_transaction observed marker={} kind={} exit_code={}",
                marker,
                runtime_running_shell_transaction_kind_name(&transaction_ref.kind),
                exit_code
            ),
        )?;
        if transaction_ref.turn_id != turn_id
            || transaction_ref.pane_id != pane_id
            || output_pane_id != pane_id
        {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction_ref,
                "end-marker-metadata-mismatch",
                "shell transaction marker metadata does not match runtime dispatch state",
            );
        }
        if self
            .process
            .shell_transaction_require_start_markers
            .contains(marker)
            && !self
                .process
                .shell_transaction_started_markers
                .contains(marker)
        {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction_ref,
                "end-marker-before-start-marker",
                "shell transaction end marker arrived before the start marker",
            );
        }
        let Some(mut transaction_ref) = self.process.running_shell_transactions.remove(marker)
        else {
            return Ok(0);
        };
        self.clear_shell_transaction_protocol_state(marker);
        if transaction_ref.kind == RunningShellTransactionKind::ReadinessProbe {
            return self.observe_readiness_probe_transaction_end(
                marker, turn_id, agent_id, pane_id, exit_code,
            );
        }
        if transaction_ref.kind == RunningShellTransactionKind::Bootstrap {
            return self.observe_bootstrap_transaction_end(
                marker,
                pane_id,
                exit_code,
                &transaction_ref.observed_output_preview,
                transaction_ref.observed_output_truncated,
            );
        }
        let RunningShellTransactionKind::AgentAction { ref action_id } = transaction_ref.kind
        else {
            return Err(MezError::invalid_state(
                "shell transaction kind was not handled",
            ));
        };
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        if turn.agent_id != agent_id || turn.pane_id != pane_id {
            return Err(MezError::invalid_state(
                "shell transaction marker identity does not match agent turn",
            ));
        }
        if self.dispatch_apply_patch_followup_if_needed(
            &turn,
            action_id,
            &transaction_ref,
            exit_code,
        )? {
            return Ok(1);
        }

        let (
            mut terminal_state,
            ready_for_provider_continuation,
            post_shell_hook_payload,
            action_transition_trace,
            observed_result,
            observed_results,
            observed_action,
            display_output_after_completion,
        ) = {
            let execution = self
                .agent_turn_executions_mut()
                .get_mut(turn_id)
                .ok_or_else(|| MezError::invalid_state("running agent execution is unavailable"))?;
            let batch = execution.response.action_batch.as_ref().ok_or_else(|| {
                MezError::invalid_state("running agent execution has no action batch")
            })?;
            let Some(action) = batch
                .actions
                .iter()
                .find(|action| action.id == action_id.as_str())
                .cloned()
            else {
                // A delayed marker for an already-superseded action is stale.
                return Ok(0);
            };
            let mut shell_backed_actions = Vec::new();
            for candidate in &batch.actions {
                if local_action_plan(candidate)?.is_some() {
                    shell_backed_actions.push(candidate.clone());
                }
            }
            let Some(result_index) = execution
                .action_results
                .iter()
                .position(|result| result.action_id == action_id.as_str())
            else {
                // A delayed marker for an already-superseded result is stale.
                return Ok(0);
            };
            if execution.action_results[result_index].status != ActionStatus::Running {
                return Ok(0);
            }
            let Some(local_plan) = local_action_plan(&action)? else {
                return Err(MezError::invalid_state(
                    "shell transaction does not match shell-backed action payload",
                ));
            };
            let raw_output_preview = transaction_ref.observed_output_preview.clone();
            let decoded_transport =
                decode_shell_output_transport_with_diagnostics(&raw_output_preview);
            let transport_diagnostics = decoded_transport.diagnostics.clone();
            transaction_ref.observed_output_preview = if transport_diagnostics.saw_begin_marker {
                decoded_transport.output
            } else {
                raw_output_preview.clone()
            };
            transaction_ref.observed_output_bytes = transaction_ref.observed_output_preview.len();
            if exit_code == 0 {
                let processed_output = postprocess_shell_action_success_output(
                    &action,
                    transaction_ref.observed_output_preview.clone(),
                );
                transaction_ref.observed_output_preview = processed_output;
                transaction_ref.observed_output_bytes =
                    transaction_ref.observed_output_preview.len();
            }
            let signal: Option<i32> = if exit_code > 128 && exit_code < 256 {
                Some(exit_code - 128)
            } else {
                None
            };
            let structured_content = mez_agent::shell_action_structured_content_json(
                &action,
                &local_plan,
                Some("pane_shell"),
                true,
                serde_json::Value::Null,
                &[],
                serde_json::json!({
                    "source": "pty",
                    "stream": "pty_combined",
                    "marker": marker,
                    "exit_code": exit_code,
                    "signal": signal,
                    "timed_out": false,
                    "combined_output_bytes": transaction_ref.observed_output_bytes,
                    "combined_output_preview": transaction_ref.observed_output_preview,
                    "boundary_state": "end-marker-observed",
                    "output_truncated": transaction_ref.observed_output_truncated || transport_diagnostics.output_truncated(),
                    "transport_incomplete": transport_diagnostics.transport_incomplete(),
                    "transport_diagnostics": transport_diagnostics.to_json()
                }),
            );
            let plain_shell_command =
                matches!(action.payload, AgentActionPayload::ShellCommand { .. });
            execution.action_results[result_index] = if exit_code == 0 || plain_shell_command {
                let success_content = if plain_shell_command && exit_code != 0 {
                    shell_command_result_content(
                        &transaction_ref.observed_output_preview,
                        Some(exit_code),
                        false,
                        false,
                    )
                } else if local_plan.display_output_after_completion
                    && !transaction_ref.observed_output_preview.trim().is_empty()
                {
                    vec![transaction_ref.observed_output_preview.clone()]
                } else {
                    vec!["shell command exited with status 0".to_string()]
                };
                ActionResult::succeeded(&turn, &action, success_content, Some(structured_content))
            } else {
                let (failure_code, failure_message) = shell_action_failure_diagnostic(
                    &action,
                    exit_code,
                    &transaction_ref.observed_output_preview,
                    &transaction_ref.command,
                );
                let mut result = ActionResult::failed(
                    &turn,
                    &action,
                    ActionStatus::Failed,
                    failure_code,
                    failure_message,
                )?;
                if !transaction_ref.observed_output_preview.trim().is_empty() {
                    result.content = vec![ActionContentBlock::text(
                        transaction_ref.observed_output_preview.clone(),
                    )];
                }
                result.structured_content_json = Some(structured_content);
                result
            };
            let shell_command_nonzero_result = exit_code != 0 && plain_shell_command;
            execution.terminal_state = if shell_command_nonzero_result {
                AgentTurnState::Running
            } else {
                runtime_agent_turn_state_from_action_results(
                    &execution.action_results,
                    execution.final_turn,
                )
            };
            let mut observed_results = vec![execution.action_results[result_index].clone()];
            if shell_command_nonzero_result {
                let skipped_content = vec![format!(
                    "shell command not run because `{action_id}` exited with status {exit_code}"
                )];
                for result in &mut execution.action_results {
                    if result.status != ActionStatus::Running
                        || result.action_id == action_id.as_str()
                    {
                        continue;
                    }
                    let Some(skipped_action) = shell_backed_actions
                        .iter()
                        .find(|candidate| candidate.id == result.action_id)
                    else {
                        continue;
                    };
                    let skipped_plan = local_action_plan(skipped_action)?.ok_or_else(|| {
                        MezError::invalid_state(
                            "pending shell result does not match shell-backed action payload",
                        )
                    })?;
                    let structured_content = mez_agent::shell_action_structured_content_json(
                        skipped_action,
                        &skipped_plan,
                        Some("pane_shell"),
                        false,
                        serde_json::Value::Null,
                        &[],
                        serde_json::json!({
                            "source": "runtime",
                            "stream": "pty_input",
                            "marker": marker,
                            "exit_code": null,
                            "signal": null,
                            "timed_out": false,
                            "combined_output_bytes": 0,
                            "combined_output_preview": "",
                            "boundary_state": "skipped-after-nonzero-shell-exit",
                            "output_truncated": false,
                            "skipped": true,
                            "previous_action_id": action_id,
                            "previous_exit_code": exit_code
                        }),
                    );
                    *result = ActionResult::succeeded(
                        &turn,
                        skipped_action,
                        skipped_content.clone(),
                        Some(structured_content),
                    );
                    observed_results.push(result.clone());
                }
            }
            let action_transition_trace = format!(
                "action {} {} reason=shell_transaction_exit terminal_state={}",
                action_id,
                if execution.action_results[result_index].status == ActionStatus::Succeeded {
                    "succeeded"
                } else {
                    "failed"
                },
                runtime_agent_turn_state_name(execution.terminal_state)
            );
            let observed_result = execution.action_results[result_index].clone();
            let post_shell_hook_payload =
                runtime_post_shell_hook_payload(&turn, &action, &observed_result, exit_code);
            let ready_for_provider_continuation = shell_command_nonzero_result
                || runtime_execution_ready_for_provider_continuation(execution);
            (
                execution.terminal_state,
                ready_for_provider_continuation,
                post_shell_hook_payload,
                action_transition_trace,
                observed_result,
                observed_results,
                action,
                local_plan.display_output_after_completion,
            )
        };
        self.integration
            .runtime_metrics_mut()
            .record_shell_transaction_completion(
                transaction_ref.started_at_unix_ms,
                current_unix_millis(),
                transaction_ref.observed_output_bytes,
                exit_code,
            );
        if exit_code == 0 {
            self.record_shell_dispatch_success(turn_id, &transaction_ref.command);
        }
        if exit_code == 0
            && matches!(
                observed_action.payload,
                AgentActionPayload::ApplyPatch { .. }
            )
            && apply_patch_transaction_phase(&transaction_ref.command)
                == Some(ApplyPatchTransactionPhase::Write)
        {
            self.record_agent_modified_files_from_diff(
                pane_id,
                &transaction_ref.observed_output_preview,
            );
        }
        self.append_agent_trace_turn_event(pane_id, turn_id, &action_transition_trace)?;
        self.append_agent_trace_maap_action_results(
            pane_id,
            turn_id,
            "shell_transaction_action_result",
            &observed_results,
        )?;
        if let Some(execution) = self.agent_turn_executions().get(turn_id).cloned() {
            self.record_runtime_agent_patch_results_for_turn(&turn, &execution);
        }
        if exit_code == 0
            && display_output_after_completion
            && (self.agent_debug_enabled(pane_id)
                || self.agent_action_result_renders_in_normal_mode(&observed_action))
            && !self.agent_shell_view_enabled(pane_id)
            && !transaction_ref.observed_output_preview.trim().is_empty()
        {
            self.append_agent_action_result_text_to_terminal_buffer(
                pane_id,
                &observed_action,
                &observed_result,
                &transaction_ref.observed_output_preview,
            )?;
        }

        self.run_configured_completed_hooks(HookEvent::PostShellCommand, &post_shell_hook_payload)?;

        let mut transcript_entries = 0usize;
        if matches!(
            terminal_state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
            let mut execution = self
                .agent_turn_executions()
                .get(turn_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("observed agent execution was not stored")
                })?;
            let failure_feedback_queued = if terminal_state == AgentTurnState::Failed {
                self.append_runtime_agent_execution_failure_audit(&turn, &execution)?;
                self.queue_agent_failure_feedback_for_correction(
                    &turn,
                    &mut execution,
                    "shell_transaction_failed_action",
                )?
            } else {
                false
            };
            if failure_feedback_queued {
                self.agent_turn_executions_mut().remove(turn_id);
                terminal_state = AgentTurnState::Running;
            } else {
                self.present_deferred_agent_say_actions_to_terminal_buffer(pane_id, &execution)?;
                transcript_entries =
                    self.persist_runtime_agent_turn_execution_transcript(&turn, &execution)?;
                self.emit_subagent_task_result_for_execution(&turn, &execution)?;
                self.complete_running_agent_turn_and_start_ready(
                    &turn,
                    terminal_state,
                    "shell_transaction_settled",
                )?;
            }
        } else if terminal_state == AgentTurnState::Running {
            self.commit_settled_action_results_context(turn_id, &observed_results)?;
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
            if ready_for_provider_continuation {
                self.queue_agent_provider_task(turn_id.to_string());
                self.append_agent_trace_turn_event(
                    pane_id,
                    turn_id,
                    "provider_task queued reason=shell_transaction_result_ready",
                )?;
            } else {
                let should_dispatch_stored_shell = self
                    .agent_turn_executions()
                    .get(turn_id)
                    .is_some_and(|execution| {
                        self.execution_has_pending_shell_dispatch(turn_id, execution)
                    });
                if should_dispatch_stored_shell {
                    self.append_agent_trace_turn_event(
                        pane_id,
                        turn_id,
                        "pending_shell_dispatch available reason=shell_transaction_result",
                    )?;
                    let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
                }
            }
        } else {
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
        }

        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","shell_transaction":"observed","marker":"{}","exit_code":{},"transcript_entries":{}}}"#,
                json_escape(pane_id),
                json_escape(turn_id),
                runtime_agent_turn_state_name(terminal_state),
                json_escape(marker),
                exit_code,
                transcript_entries
            ),
        )?;
        Ok(1)
    }
}
