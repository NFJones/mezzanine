//! Runtime agent shell-action dispatch helpers.
//!
//! This module owns pending shell dispatch detection, readiness/hook waiting,
//! shell action loop guards, apply-patch follow-up dispatch, and conversion of
//! shell dispatch failures into normal action results. It keeps pane-shell
//! execution orchestration out of the runtime agent facade while the low-level
//! pane transaction writer remains in the facade for now.

use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnExecution,
    AgentTurnRecord, AgentTurnState, ApplyPatchPathBoundary, ApplyPatchTransactionPhase, BTreeSet,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, EventKind, HookEvent, MezError, PaneReadinessState,
    PendingFocusedShellHookContinuation, Result, RunningShellTransactionKind,
    RunningShellTransactionRef, RuntimeApplyPatchBatchState, RuntimeHookPipelineBlock,
    RuntimeHookPipelineDecision, RuntimePendingApplyPatchPhase, RuntimeSessionService,
    apply_patch_error_plan, apply_patch_read_plan_for_paths_with_boundary,
    apply_patch_touched_paths, apply_patch_transaction_phase,
    apply_patch_write_plan_from_read_outputs_with_boundary,
    decode_shell_output_transport_with_diagnostics, exact_command_sha256, json_escape,
    local_action_plan, runtime_action_result_is_suppressed_duplicate_file_mutation,
    runtime_agent_action_rejects_duplicate_success, runtime_agent_context_command,
    runtime_agent_execution_prompt_display_lines, runtime_agent_terminal_preview,
    runtime_agent_turn_state_from_action_results, runtime_agent_turn_state_name,
    runtime_execution_ready_for_provider_continuation, runtime_mezzanine_error_code,
    runtime_pane_readiness_state_name, runtime_pre_shell_hook_payload,
};
use crate::runtime::render::RuntimeAgentShellPreviewOwner;
use crate::runtime::{RuntimeSandboxFallbackAudit, SandboxConfig, runtime_post_shell_hook_payload};
use mez_agent::semantic_patch_planning::{
    APPLY_PATCH_RESULT_MARKER, ApplyPatchFileOutcome, parse_apply_patch_file_outcomes,
};
use mez_agent::shell_observation::latest_agent_shell_transaction_output_lines;
use mez_agent::{
    LocalExecutionOutput, local_execution_output_to_action_result, postprocess_local_shell_output,
};

/// Describes why an `apply_patch` snapshot cannot safely reach write planning.
///
/// The message retains the generic safety boundary while naming the observed
/// transport fault so the terminal action result can guide a bounded retry or
/// subsequent manual recovery.
fn apply_patch_read_transport_failure_message(
    diagnostics: &mez_agent::ShellTransportDiagnostics,
    observed_output_truncated: bool,
) -> String {
    let cause = if diagnostics.missing_frame {
        "the transport frame was missing"
    } else if diagnostics.missing_end_marker {
        "the transport end marker was missing"
    } else if diagnostics.invalid_base64_blocks > 0 {
        "the transport contained invalid base64"
    } else if diagnostics.partial_base64_bytes_dropped > 0 {
        "the transport ended with a partial base64 block"
    } else if diagnostics.output_bytes_dropped > 0 {
        "the transport exceeded its output retention limit"
    } else if observed_output_truncated {
        "pane observation capture was truncated"
    } else {
        "the transport was incomplete"
    };
    format!(
        "apply_patch read phase output was truncated or transport-incomplete before Rust could build the write phase: {cause}"
    )
}

impl RuntimeSessionService {
    /// Reports whether a native action still belongs to a live turn execution.
    ///
    /// Joined parents release provider capacity by moving to ledger `Blocked`
    /// and scheduler `Waiting`, but sibling native actions from the same batch
    /// remain authorized and must continue to accept worker ownership/results.
    /// Approval-blocked and terminal turns are excluded because they do not own
    /// a dependency wait capable of preserving the in-flight execution.
    fn native_shell_action_turn_is_current(&self, turn_id: &str, action_id: &str) -> bool {
        let turn_is_live = self.agent_turn_ledger().turns().iter().any(|turn| {
            turn.turn_id == turn_id
                && (turn.state == AgentTurnState::Running
                    || (turn.state == AgentTurnState::Blocked
                        && self
                            .agent
                            .agent_scheduler
                            .waiting_turns()
                            .any(|work| work.turn_id == turn_id)))
        });
        turn_is_live
            && self
                .agent_turn_executions()
                .get(turn_id)
                .is_some_and(|execution| {
                    execution.action_results.iter().any(|result| {
                        result.action_id == action_id && result.status == ActionStatus::Running
                    })
                })
    }

    /// Applies one fenced native-shell output preview to the transient pane tail.
    pub(crate) fn apply_native_shell_progress(
        &mut self,
        progress: crate::runtime::RuntimeNativeShellProgress,
    ) -> Result<bool> {
        let identity = (progress.turn_id.clone(), progress.action_id.clone());
        let claimed = self.agent.claimed_native_shell_dispatches.get(&identity);
        let pending = self
            .agent
            .pending_native_shell_dispatches
            .get(&identity)
            .map(|dispatch| &dispatch.marker);
        if claimed != Some(&progress.marker) || pending != Some(&progress.marker) {
            return Ok(false);
        }
        if !self.native_shell_action_turn_is_current(&progress.turn_id, &progress.action_id) {
            return Ok(false);
        }
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == progress.turn_id)
            .cloned()
        else {
            return Ok(false);
        };
        if !self.agent_shell_transaction_action_shows_live_output(
            &progress.turn_id,
            &progress.action_id,
        ) {
            return Ok(false);
        }
        let lines = latest_agent_shell_transaction_output_lines(
            &progress.output_preview,
            self.terminal_shell_output_preview_lines(),
        );
        if lines.is_empty() {
            return Ok(false);
        }
        self.update_agent_shell_output_preview(
            &turn.pane_id,
            RuntimeAgentShellPreviewOwner {
                turn_id: progress.turn_id,
                action_id: progress.action_id,
                marker: progress.marker,
            },
            progress.revision,
            &lines,
        )?;
        Ok(true)
    }

    /// Returns native shell actions ready for external worker dispatch.
    pub(crate) fn pending_native_shell_actions(&self) -> Vec<(String, String)> {
        self.agent
            .pending_native_shell_dispatches
            .keys()
            .filter(|identity| {
                !self
                    .agent
                    .claimed_native_shell_dispatches
                    .contains_key(*identity)
            })
            .cloned()
            .collect()
    }

    /// Returns turns whose native shell work has a queued or active owner.
    pub(crate) fn native_shell_progress_turn_ids(&self) -> Vec<String> {
        self.agent
            .pending_native_shell_dispatches
            .keys()
            .chain(self.agent.claimed_native_shell_dispatches.keys())
            .map(|(turn_id, _)| turn_id.clone())
            .collect()
    }

    /// Claims one authorized native shell action for worker execution.
    pub(crate) fn claim_native_shell_action(
        &mut self,
        turn_id: &str,
        action_id: &str,
    ) -> Result<Option<crate::runtime::RuntimeNativeShellDispatch>> {
        let identity = (turn_id.to_string(), action_id.to_string());
        if self
            .agent
            .claimed_native_shell_dispatches
            .contains_key(&identity)
        {
            return Ok(None);
        }
        if !self.native_shell_action_turn_is_current(turn_id, action_id) {
            self.agent.pending_native_shell_dispatches.remove(&identity);
            return Ok(None);
        }
        let Some(dispatch) = self
            .agent
            .pending_native_shell_dispatches
            .get(&identity)
            .cloned()
        else {
            return Ok(None);
        };
        self.agent
            .claimed_native_shell_dispatches
            .insert(identity, dispatch.marker.clone());
        Ok(Some(dispatch))
    }

    /// Applies one native shell worker result through actor-owned state.
    ///
    /// The exact turn, action, and marker must still own the pending work.
    /// Stale completions are discarded after releasing only their matching
    /// claim, so a cancelled or replaced turn cannot receive worker output.
    pub(crate) fn complete_native_shell_action(
        &mut self,
        outcome: crate::runtime::RuntimeNativeShellOutcome,
    ) -> Result<bool> {
        let identity = (outcome.turn_id.clone(), outcome.action_id.clone());
        let claimed_marker = self
            .agent
            .claimed_native_shell_dispatches
            .get(&identity)
            .cloned();
        let pending_marker = self
            .agent
            .pending_native_shell_dispatches
            .get(&identity)
            .map(|dispatch| dispatch.marker.clone());
        if claimed_marker.as_deref() != Some(outcome.marker.as_str())
            || pending_marker.as_deref() != Some(outcome.marker.as_str())
        {
            if claimed_marker.as_deref() == Some(outcome.marker.as_str()) {
                self.agent.claimed_native_shell_dispatches.remove(&identity);
            }
            return Ok(false);
        }
        let current =
            self.native_shell_action_turn_is_current(&outcome.turn_id, &outcome.action_id);
        self.agent.pending_native_shell_dispatches.remove(&identity);
        self.agent.claimed_native_shell_dispatches.remove(&identity);
        if !current {
            return Ok(false);
        }
        if let Some(capability) = outcome.bubblewrap_capability.as_deref().cloned() {
            self.record_bubblewrap_capability(capability.cache_key.clone(), capability);
        }
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == outcome.turn_id)
            .cloned()
        else {
            return Ok(false);
        };
        let Some(mut execution) = self.agent_turn_executions().get(&outcome.turn_id).cloned()
        else {
            return Ok(false);
        };
        let Some(action) = execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| {
                batch
                    .actions
                    .iter()
                    .find(|action| action.id == outcome.action_id)
            })
            .cloned()
        else {
            return Ok(false);
        };
        let Some(result_index) = execution
            .action_results
            .iter()
            .position(|result| result.action_id == outcome.action_id)
        else {
            return Ok(false);
        };
        if execution.action_results[result_index].status != ActionStatus::Running {
            return Ok(false);
        }
        let preview_owner = RuntimeAgentShellPreviewOwner {
            turn_id: outcome.turn_id.clone(),
            action_id: outcome.action_id.clone(),
            marker: outcome.marker.clone(),
        };

        let mut shell_output = match outcome.result {
            Ok(output) => output,
            Err(failure) => {
                self.settle_agent_shell_output_preview(&turn.pane_id, &preview_owner);
                let mut result = ActionResult::failed(
                    &turn,
                    &action,
                    ActionStatus::Failed,
                    failure.kind,
                    failure.message.clone(),
                )?;
                result.structured_content_json =
                    Some(mez_agent::shell_action_structured_content_json(
                        &action,
                        &local_action_plan(&action)?.ok_or_else(|| {
                            MezError::invalid_state(
                                "native shell completion does not match a local action plan",
                            )
                        })?,
                        Some("spawned_shell"),
                        false,
                        serde_json::Value::Null,
                        &[],
                        serde_json::json!({
                            "source": "spawned_shell_worker",
                            "marker": outcome.marker,
                            "boundary_state": "worker_failed",
                            "error": failure.message
                        }),
                    ));
                execution.action_results[result_index] = result.clone();
                execution.terminal_state = runtime_agent_turn_state_from_action_results(
                    &execution.action_results,
                    execution.final_turn,
                );
                self.append_agent_trace_maap_action_results(
                    &turn.pane_id,
                    &turn.turn_id,
                    "spawned_shell_action_result",
                    std::slice::from_ref(&result),
                )?;
                self.settle_native_shell_execution(&turn, execution, vec![result])?;
                return Ok(true);
            }
        };
        let exit_code = shell_output.exit_code;
        let output_truncated = shell_output.transport_diagnostics.output_bytes_dropped > 0;
        let is_apply_patch_read = matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
            && apply_patch_transaction_phase(&outcome.command)
                == Some(ApplyPatchTransactionPhase::Read);
        let is_apply_patch_write = matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
            && apply_patch_transaction_phase(&outcome.command)
                == Some(ApplyPatchTransactionPhase::Write);
        let apply_patch_file_outcomes = if is_apply_patch_write && !output_truncated {
            let combined = format!("{}{}", shell_output.stdout, shell_output.stderr);
            parse_apply_patch_file_outcomes(&combined)
                .ok()
                .filter(|outcomes| !outcomes.is_empty())
        } else {
            None
        };
        if apply_patch_file_outcomes.is_some() {
            let combined = format!("{}{}", shell_output.stdout, shell_output.stderr);
            shell_output.stdout = combined
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .lines()
                .filter(|line| !line.starts_with(APPLY_PATCH_RESULT_MARKER))
                .collect::<Vec<_>>()
                .join("\n");
            shell_output.stderr.clear();
        }
        let shell_output = postprocess_local_shell_output(&action, shell_output);
        let combined_output = format!("{}{}", shell_output.stdout, shell_output.stderr);
        if matches!(action.payload, AgentActionPayload::ShellCommand { .. }) {
            let lines = latest_agent_shell_transaction_output_lines(
                &combined_output,
                self.terminal_shell_output_preview_lines(),
            );
            if !lines.is_empty() {
                self.update_agent_shell_output_preview(
                    &turn.pane_id,
                    preview_owner.clone(),
                    u64::MAX,
                    &lines,
                )?;
            }
            self.settle_agent_shell_output_preview(&turn.pane_id, &preview_owner);
        }

        self.integration
            .runtime_metrics_mut()
            .record_shell_transaction_completion(
                outcome.started_at_unix_ms,
                super::current_unix_millis(),
                combined_output.len(),
                exit_code.unwrap_or(0),
            );
        if exit_code == Some(0) {
            self.record_shell_dispatch_success(&turn.turn_id, &outcome.command);
        }

        if is_apply_patch_read {
            let path_boundary = self
                .agent
                .apply_patch_batch_states
                .get(&Self::apply_patch_batch_state_key(
                    &turn.turn_id,
                    &action.id,
                ))
                .map(|state| state.path_boundary.clone())
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "native apply_patch read completion lost its path boundary",
                    )
                })?;
            if let Some(plan) = self.plan_apply_patch_followup_from_read_output(
                &turn,
                &action,
                &path_boundary,
                exit_code.unwrap_or(1),
                &combined_output,
                output_truncated,
            )? {
                self.agent.pending_apply_patch_phases.insert(
                    Self::apply_patch_batch_state_key(&turn.turn_id, &action.id),
                    RuntimePendingApplyPatchPhase {
                        plan,
                        path_boundary,
                    },
                );
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "action {} completed spawned_shell read phase marker={}",
                        action.id, outcome.marker
                    ),
                )?;
                let _ = self.dispatch_stored_running_shell_actions(&turn.turn_id)?;
                return Ok(true);
            }
        }

        let marker = mez_agent::MarkerToken::new(&outcome.marker).map_err(|error| {
            MezError::invalid_state(format!(
                "spawned shell completion marker was invalid: {}",
                error.message()
            ))
        })?;
        let result = local_execution_output_to_action_result(
            &turn,
            &action,
            LocalExecutionOutput::spawned_shell(shell_output),
            &marker,
        )
        .map_err(|error| {
            MezError::invalid_state(format!(
                "spawned shell result projection failed: {}",
                error.message()
            ))
        })?;
        execution.action_results[result_index] = result.clone();
        let mut settled_results = vec![result.clone()];
        if matches!(exit_code, Some(code) if code != 0)
            && matches!(action.payload, AgentActionPayload::ShellCommand { .. })
        {
            let batch = execution.response.action_batch.as_ref().ok_or_else(|| {
                MezError::invalid_state("native shell execution has no action batch")
            })?;
            let skipped_content = vec![format!(
                "shell command not run because `{}` exited with status {}",
                action.id,
                exit_code.unwrap_or(1)
            )];
            for pending in &mut execution.action_results {
                if pending.status != ActionStatus::Running || pending.action_id == action.id {
                    continue;
                }
                let Some(skipped_action) = batch
                    .actions
                    .iter()
                    .find(|candidate| candidate.id == pending.action_id)
                else {
                    continue;
                };
                let Some(skipped_plan) = local_action_plan(skipped_action)? else {
                    continue;
                };
                *pending = ActionResult::succeeded(
                    &turn,
                    skipped_action,
                    skipped_content.clone(),
                    Some(mez_agent::shell_action_structured_content_json(
                        skipped_action,
                        &skipped_plan,
                        Some("spawned_shell"),
                        false,
                        serde_json::Value::Null,
                        &[],
                        serde_json::json!({
                            "source": "runtime",
                            "boundary_state": "skipped-after-nonzero-shell-exit",
                            "skipped": true,
                            "previous_action_id": action.id,
                            "previous_exit_code": exit_code
                        }),
                    )),
                );
                settled_results.push(pending.clone());
            }
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );

        let confirmed_partial_apply = apply_patch_file_outcomes.as_ref().is_some_and(|outcomes| {
            outcomes
                .iter()
                .any(|outcome| matches!(outcome, ApplyPatchFileOutcome::Applied { .. }))
        });
        if is_apply_patch_write && (exit_code == Some(0) || confirmed_partial_apply) {
            self.record_agent_modified_files_from_diff(&turn.pane_id, &combined_output);
        }
        if !is_apply_patch_read {
            if self.agent_shell_view_enabled(&turn.pane_id) && !combined_output.trim().is_empty() {
                self.append_agent_pty_diagnostic_bytes_to_terminal_buffer(
                    &turn.pane_id,
                    combined_output.as_bytes(),
                )?;
            } else if (exit_code == Some(0) || (is_apply_patch_write && confirmed_partial_apply))
                && local_action_plan(&action)?
                    .is_some_and(|plan| plan.display_output_after_completion)
                && (self.agent_debug_enabled(&turn.pane_id)
                    || self.agent_action_result_renders_in_normal_mode(&action))
                && !combined_output.trim().is_empty()
            {
                self.append_agent_action_result_text_to_terminal_buffer(
                    &turn.pane_id,
                    &action,
                    &result,
                    &combined_output,
                )?;
            }
            self.run_configured_completed_hooks(
                HookEvent::PostShellCommand,
                &runtime_post_shell_hook_payload(&turn, &action, &result, exit_code.unwrap_or(0)),
            )?;
        }
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} completed spawned_shell exit_code={:?} marker={}",
                action.id, exit_code, outcome.marker
            ),
        )?;
        self.append_agent_trace_maap_action_results(
            &turn.pane_id,
            &turn.turn_id,
            "spawned_shell_action_result",
            &settled_results,
        )?;
        self.settle_native_shell_execution(&turn, execution, settled_results)?;
        Ok(true)
    }

    /// Applies shared continuation or terminal lifecycle after native output.
    fn settle_native_shell_execution(
        &mut self,
        turn: &AgentTurnRecord,
        mut execution: AgentTurnExecution,
        settled_results: Vec<ActionResult>,
    ) -> Result<()> {
        self.record_runtime_agent_patch_results_for_turn(turn, &execution);
        let failed_native_apply_patch_ids = settled_results
            .iter()
            .filter(|result| result.is_error)
            .filter_map(|result| {
                execution
                    .response
                    .action_batch
                    .as_ref()
                    .filter(|batch| {
                        batch.actions.iter().any(|action| {
                            action.id == result.action_id
                                && matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
                        })
                    })
                    .map(|_| result.action_id.clone())
            })
            .collect::<BTreeSet<_>>();
        if failed_native_apply_patch_ids.is_empty() {
            self.present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)?;
        } else {
            let mut presentation_execution = execution.clone();
            presentation_execution
                .action_results
                .retain(|result| !failed_native_apply_patch_ids.contains(&result.action_id));
            self.present_agent_action_outcomes_to_terminal_buffer(
                &turn.pane_id,
                &presentation_execution,
            )?;
        }
        if matches!(
            execution.terminal_state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            let failure_feedback_queued = if execution.terminal_state == AgentTurnState::Failed {
                self.append_runtime_agent_execution_failure_audit(turn, &execution)?;
                self.queue_agent_failure_feedback_for_correction(
                    turn,
                    &mut execution,
                    "spawned_shell_failed_action",
                )?
            } else {
                false
            };
            if failure_feedback_queued {
                self.agent_turn_executions_mut().remove(&turn.turn_id);
                return Ok(());
            }
            self.present_deferred_agent_say_actions_to_terminal_buffer(&turn.pane_id, &execution)?;
            self.persist_runtime_agent_turn_execution_transcript(turn, &execution)?;
            self.emit_subagent_task_result_for_execution(turn, &execution)?;
            self.complete_running_agent_turn_and_start_ready(
                turn,
                execution.terminal_state,
                "spawned_shell_settled",
            )?;
            return Ok(());
        }

        self.commit_settled_action_results_context(&turn.turn_id, &settled_results)?;
        self.agent_turn_executions_mut()
            .insert(turn.turn_id.clone(), execution.clone());
        if runtime_execution_ready_for_provider_continuation(&execution) {
            if !self.resume_dependency_wait_if_ready(&turn.turn_id, "spawned_shell_result_ready")? {
                self.queue_agent_provider_task(turn.turn_id.clone());
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    "provider_task queued reason=spawned_shell_result_ready",
                )?;
            }
        } else if self.execution_has_pending_shell_dispatch(&turn.turn_id, &execution) {
            let _ = self.dispatch_stored_running_shell_actions(&turn.turn_id)?;
        }
        Ok(())
    }

    /// Reports whether a native worker owns one running action.
    fn agent_action_has_native_shell_owner(&self, turn_id: &str, action_id: &str) -> bool {
        let identity = (turn_id.to_string(), action_id.to_string());
        self.agent
            .pending_native_shell_dispatches
            .contains_key(&identity)
            || self
                .agent
                .claimed_native_shell_dispatches
                .contains_key(&identity)
    }

    /// Appends one transported read chunk to an active apply-patch batch.
    pub(crate) fn append_apply_patch_batch_transport(
        &mut self,
        state_key: &str,
        transport_chunk: &[u8],
    ) {
        if let Some(state) = self.agent.apply_patch_batch_states.get_mut(state_key) {
            state
                .current_read_transport
                .extend_from_slice(transport_chunk);
        }
    }

    /// Builds the state key for one batched shell-backed `apply_patch` action.
    pub(crate) fn apply_patch_batch_state_key(turn_id: &str, action_id: &str) -> String {
        format!("{turn_id}/{action_id}")
    }

    /// Replaces the initial shell-backed `apply_patch` read plan with the next
    /// one-path batch read plan.
    fn prepare_apply_patch_batched_read_plan(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        plan: &mut mez_agent::LocalActionPlan,
        path_boundary: &ApplyPatchPathBoundary,
    ) -> Result<()> {
        let AgentActionPayload::ApplyPatch { patch, .. } = &action.payload else {
            return Ok(());
        };
        let key = Self::apply_patch_batch_state_key(&turn.turn_id, &action.id);
        if !self.agent.apply_patch_batch_states.contains_key(&key) {
            self.agent.apply_patch_batch_states.insert(
                key.clone(),
                RuntimeApplyPatchBatchState {
                    path_boundary: path_boundary.clone(),
                    remaining_paths: apply_patch_touched_paths(patch)?,
                    current_path: None,
                    current_path_read_retries: 0,
                    current_read_transport: Vec::new(),
                    read_outputs: Vec::new(),
                },
            );
        }
        if let Some(state) = self.agent.apply_patch_batch_states.get_mut(&key)
            && !state.remaining_paths.is_empty()
        {
            let path = state.remaining_paths.remove(0);
            let mut paths = BTreeSet::new();
            paths.insert(path.clone());
            state.current_path = Some(path);
            state.current_path_read_retries = 0;
            *plan = apply_patch_read_plan_for_paths_with_boundary(&paths, path_boundary);
        }
        Ok(())
    }

    /// Queues one generated apply-patch phase for ordinary shell dispatch.
    ///
    /// Generated read retries and write plans must re-enter the same hook and
    /// authorization path as provider-authored shell actions. The retained
    /// plan prevents the dispatcher from rebuilding the initial read phase.
    fn dispatch_generated_apply_patch_phase(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        plan: mez_agent::LocalActionPlan,
        path_boundary: ApplyPatchPathBoundary,
    ) -> Result<()> {
        self.agent.pending_apply_patch_phases.insert(
            Self::apply_patch_batch_state_key(&turn.turn_id, &action.id),
            RuntimePendingApplyPatchPhase {
                plan,
                path_boundary,
            },
        );
        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Ready);
        let _ = self.dispatch_stored_running_shell_actions(&turn.turn_id)?;
        Ok(())
    }

    /// Runs the execution has pending shell dispatch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn execution_has_pending_shell_dispatch(
        &self,
        turn_id: &str,
        execution: &AgentTurnExecution,
    ) -> bool {
        if self.agent.sandbox_failure_assessments.contains_key(turn_id) {
            return false;
        }
        let batch = execution.response.action_batch.as_ref();
        execution.terminal_state == AgentTurnState::Running
            && execution.action_results.iter().any(|result| {
                let local_shell_backed = batch
                    .and_then(|batch| {
                        batch
                            .actions
                            .iter()
                            .find(|action| action.id == result.action_id)
                    })
                    .and_then(|action| local_action_plan(action).ok().flatten())
                    .is_some();
                result.status == ActionStatus::Running
                    && local_shell_backed
                    && !self.agent_action_has_pending_pre_shell_hook(turn_id, &result.action_id)
                    && !self.agent_action_has_running_shell_transaction(turn_id, &result.action_id)
                    && !self.agent_action_has_native_shell_owner(turn_id, &result.action_id)
            })
    }

    /// Runs the agent action has pending pre shell hook operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn agent_action_has_pending_pre_shell_hook(
        &self,
        turn_id: &str,
        action_id: &str,
    ) -> bool {
        self.integration
            .focused_shell_hook_transactions()
            .values()
            .any(|pending| {
                pending.continuation.as_ref().is_some_and(|continuation| {
                    continuation.turn_id == turn_id && continuation.action_id == action_id
                })
            })
            || self
                .integration
                .pending_program_hook_continuations()
                .iter()
                .any(|continuation| {
                    continuation.turn_id == turn_id && continuation.action_id == action_id
                })
    }

    /// Runs the turn has running readiness probe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn turn_has_running_readiness_probe(&self, turn_id: &str) -> bool {
        self.turn_has_running_shell_transaction_kind(
            turn_id,
            &RunningShellTransactionKind::ReadinessProbe,
        )
    }

    /// Returns a local result when a shell-backed mutation has already
    /// succeeded with the exact same generated command in the current turn.
    ///
    /// This intentionally does not cap the number of shell dispatches in a
    /// turn. Failed shell commands are model-visible results, and large but
    /// finite inspection batches should be allowed to run.
    fn shell_dispatch_loop_guard_failure(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        command: &str,
    ) -> Result<Option<ActionResult>> {
        let history = self
            .agent
            .agent_turn_shell_dispatch_history
            .get(&turn.turn_id)
            .cloned()
            .unwrap_or_default();
        let dispatched_count = history.dispatched_count();
        let successful_duplicate_count = history.exact_success_count(command);
        let is_file_mutation = runtime_agent_action_rejects_duplicate_success(action)
            && apply_patch_transaction_phase(command) == Some(ApplyPatchTransactionPhase::Write);
        if is_file_mutation && successful_duplicate_count > 0 {
            let context_command = runtime_agent_context_command(action, command);
            return Ok(Some(ActionResult::succeeded(
                turn,
                action,
                vec![
                    "duplicate file mutation skipped because the same mutation already succeeded"
                        .to_string(),
                ],
                Some(format!(
                    r#"{{"guard":"shell_dispatch_loop","reason":"repeated_successful_file_mutation","command":"{}","dispatch_count":{},"successful_duplicate_count":{}}}"#,
                    json_escape(&context_command),
                    dispatched_count,
                    successful_duplicate_count
                )),
            )));
        }
        Ok(None)
    }

    /// Runs the record shell dispatch history operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn record_shell_dispatch_history(&mut self, turn_id: &str, command: &str) {
        self.agent
            .agent_turn_shell_dispatch_history
            .entry(turn_id.to_string())
            .or_default()
            .record(command.to_string());
    }

    /// Records a shell command that exited successfully for loop detection and
    /// mutation/validation phase tracking.
    pub(crate) fn record_shell_dispatch_success(&mut self, turn_id: &str, command: &str) {
        self.agent
            .agent_turn_shell_dispatch_history
            .entry(turn_id.to_string())
            .or_default()
            .record_success(command.to_string());
    }

    /// Keeps the network action dispatch boundary symmetrical with shell
    /// actions without enforcing a count-based per-turn cap.
    pub(super) fn network_action_loop_guard_failure(
        &self,
        _turn: &AgentTurnRecord,
        _action: &AgentAction,
        _request: &str,
    ) -> Result<Option<ActionResult>> {
        Ok(None)
    }

    /// Records a runtime-owned network request for loop detection.
    pub(super) fn record_network_action_history(&mut self, turn_id: &str, request: &str) {
        self.agent
            .agent_turn_network_action_history
            .entry(turn_id.to_string())
            .or_default()
            .record(request.to_string());
    }

    /// Runs the dispatch stored running shell actions operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn dispatch_stored_running_shell_actions(
        &mut self,
        turn_id: &str,
    ) -> Result<Option<AgentTurnExecution>> {
        let Some(mut execution) = self.agent_turn_executions().get(turn_id).cloned() else {
            return Ok(None);
        };
        if !self.execution_has_pending_shell_dispatch(turn_id, &execution) {
            return Ok(None);
        }
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            "pending_shell_dispatch resume started",
        )?;
        let mut dispatched = self.dispatch_running_shell_actions_to_panes(&turn, &mut execution)?;
        while self
            .agent
            .pending_apply_patch_phases
            .keys()
            .any(|key| key.starts_with(&format!("{turn_id}/")))
            && self.execution_has_pending_shell_dispatch(turn_id, &execution)
        {
            dispatched = dispatched.saturating_add(
                self.dispatch_running_shell_actions_to_panes(&turn, &mut execution)?,
            );
        }
        self.record_runtime_agent_patch_results_for_turn(&turn, &execution);
        // Routed workers own ephemeral presentation panes. A terminal dispatch
        // result settles the routed workflow and closes that pane below, so
        // action outcomes must be rendered while the worker still owns its
        // presentation target.
        self.present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)?;
        let mut terminal_state = execution.terminal_state;
        let mut transcript_entries = 0usize;
        if terminal_state == AgentTurnState::Blocked {
            self.apply_permission_request_hooks_for_execution(&turn, &mut execution)?;
            terminal_state = execution.terminal_state;
        }
        if matches!(
            terminal_state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            let failure_feedback_queued = if terminal_state == AgentTurnState::Failed {
                self.append_runtime_agent_execution_failure_audit(&turn, &execution)?;
                self.queue_agent_failure_feedback_for_correction(
                    &turn,
                    &mut execution,
                    "pending_shell_dispatch_failed_action",
                )?
            } else {
                false
            };
            if failure_feedback_queued {
                self.agent_turn_executions_mut().remove(turn_id);
                terminal_state = AgentTurnState::Running;
            } else {
                transcript_entries =
                    self.persist_runtime_agent_turn_execution_transcript(&turn, &execution)?;
                self.emit_subagent_task_result_for_execution(&turn, &execution)?;
                self.complete_running_agent_turn_and_start_ready(
                    &turn,
                    terminal_state,
                    "pending_shell_dispatch_settled",
                )?;
            }
        } else if terminal_state == AgentTurnState::Blocked {
            transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(&turn, &execution)?;
            self.queue_blocked_approvals_for_execution(&turn, &execution)?;
            self.agent_turn_executions_mut()
                .insert(turn_id.to_string(), execution.clone());
            let _ = self.agent.agent_scheduler.block_running(turn_id);
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            self.agent_turn_ledger_mut()
                .finish_turn(turn_id, AgentTurnState::Blocked)?;
            self.reconcile_active_turn_sleep_inhibition();
            self.append_agent_trace_turn_transition(
                &turn,
                turn.state,
                AgentTurnState::Blocked,
                "bubblewrap_preparation_fallback_approval",
            )?;
            self.start_ready_agent_turns()?;
        } else {
            self.agent_turn_executions_mut()
                .insert(turn_id.to_string(), execution.clone());
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "pending_shell_dispatch stored state={} dispatched={}",
                    runtime_agent_turn_state_name(terminal_state),
                    dispatched
                ),
            )?;
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","pending_shell_dispatch":true,"shell_actions_dispatched":{},"transcript_entries":{}}}"#,
                json_escape(&turn.pane_id),
                json_escape(turn_id),
                runtime_agent_turn_state_name(terminal_state),
                dispatched,
                transcript_entries
            ),
        )?;
        self.set_agent_prompt_display_lines_if_pane_present(
            &turn.pane_id,
            runtime_agent_execution_prompt_display_lines(
                turn_id,
                &execution.response.provider,
                &execution,
                dispatched,
                transcript_entries,
            ),
        )?;
        Ok(Some(execution))
    }

    /// Runs the fail pending shell action for hook block operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn fail_pending_shell_action_for_hook_block(
        &mut self,
        continuation: &PendingFocusedShellHookContinuation,
        block: &RuntimeHookPipelineBlock,
    ) -> Result<usize> {
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == continuation.turn_id)
            .cloned()
        else {
            return Ok(0);
        };
        let Some(mut execution) = self
            .agent_turn_executions()
            .get(&continuation.turn_id)
            .cloned()
        else {
            return Ok(0);
        };
        let batch = execution.response.action_batch.as_ref().ok_or_else(|| {
            MezError::invalid_state("running agent execution has no action batch")
        })?;
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == continuation.action_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("hook continuation action is unavailable"))?;
        let result_index = execution
            .action_results
            .iter()
            .position(|result| result.action_id == continuation.action_id)
            .ok_or_else(|| MezError::invalid_state("hook continuation result is unavailable"))?;
        if execution.action_results[result_index].status != ActionStatus::Running {
            return Ok(0);
        }
        let mut blocked = ActionResult::failed(
            &turn,
            &action,
            ActionStatus::Denied,
            "hook_blocked",
            block.message.clone(),
        )?;
        blocked.structured_content_json = Some(block.structured_json());
        execution.action_results[result_index] = blocked.clone();
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        self.agent_turn_executions_mut()
            .insert(continuation.turn_id.clone(), execution.clone());
        self.append_agent_error_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: shell command blocked by hook {}: {}",
                block.hook_id, block.message
            ),
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} denied reason=pre_shell_hook hook={}",
                action.id, block.hook_id
            ),
        )?;
        self.present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)?;
        if matches!(
            execution.terminal_state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            let transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(&turn, &execution)?;
            self.emit_subagent_task_result_for_execution(&turn, &execution)?;
            self.complete_running_agent_turn_and_start_ready(
                &turn,
                execution.terminal_state,
                "pre_shell_hook_blocked",
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","hook_blocked":true,"hook_id":"{}","transcript_entries":{}}}"#,
                    json_escape(&turn.pane_id),
                    json_escape(&turn.turn_id),
                    runtime_agent_turn_state_name(execution.terminal_state),
                    json_escape(&block.hook_id),
                    transcript_entries
                ),
            )?;
        }
        Ok(1)
    }

    /// Runs the dispatch running shell actions to panes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_running_shell_actions_to_panes(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        if execution.terminal_state != AgentTurnState::Running {
            return Ok(0);
        }
        let Some(batch) = execution.response.action_batch.clone() else {
            return Ok(0);
        };
        let mut dispatched = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .ok_or_else(|| {
                    MezError::invalid_state("running shell result does not match an action")
                })?;
            let action_index = batch
                .actions
                .iter()
                .position(|candidate| candidate.id == action.id)
                .ok_or_else(|| {
                    MezError::invalid_state("running shell result action has no batch position")
                })?;
            let is_apply_patch = matches!(action.payload, AgentActionPayload::ApplyPatch { .. });
            let permission_evaluation = execution.action_results[index]
                .permission_evaluation
                .clone();
            let permission_policy = self.permission_policy_for_turn(turn);
            let sandbox_config = self.sandbox_config_for_pane(&turn.pane_id);
            let bubblewrap_applies = crate::runtime::config::sandbox_applies_to_policy(
                &sandbox_config,
                &permission_policy,
            );
            let native_mode = self.effective_agent_shell_mode_for_pane(&turn.pane_id)
                == crate::runtime::config::ShellMode::Native;
            let mut sandbox_bypassed = is_apply_patch
                && bubblewrap_applies
                && self.activate_sandbox_bypass_after_approval(&turn.turn_id, &action.id);
            if is_apply_patch && bubblewrap_applies && !sandbox_bypassed && !native_mode {
                match self.ensure_bubblewrap_path_resolution_for_action(
                    turn,
                    &action.id,
                    permission_evaluation.as_deref(),
                ) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        execution.action_results[index] = self.shell_action_runtime_error_result(
                            turn,
                            action,
                            "apply_patch",
                            "bubblewrap_path_resolution",
                            &error,
                        )?;
                        continue;
                    }
                }
                match self
                    .ensure_bubblewrap_capability_for_action_with_environment_profile_and_child_shell(
                        turn,
                        &action.id,
                        crate::runtime::BubblewrapEnvironmentProfile::SemanticPatchNoForwarding,
                        Some("/bin/sh"),
                    )
                {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        execution.action_results[index] = self.shell_action_runtime_error_result(
                            turn,
                            action,
                            "apply_patch",
                            "bubblewrap_capability_probe",
                            &error,
                        )?;
                        continue;
                    }
                }
            }
            let path_boundary = if is_apply_patch {
                self.apply_patch_path_boundary_for_action(turn, &action.id)?
            } else {
                ApplyPatchPathBoundary::CurrentDirectoryOnly
            };
            if is_apply_patch {
                self.record_agent_loop_apply_patch_for_turn(&turn.turn_id);
            }
            let apply_patch_state_key =
                Self::apply_patch_batch_state_key(&turn.turn_id, &action.id);
            let pending_apply_patch_phase = self
                .agent
                .pending_apply_patch_phases
                .remove(&apply_patch_state_key);
            let has_pending_apply_patch_phase = pending_apply_patch_phase.is_some();
            let mut plan = match pending_apply_patch_phase {
                Some(pending) => {
                    if pending.path_boundary != path_boundary {
                        execution.action_results[index] = self.shell_action_runtime_error_result(
                            turn,
                            action,
                            "apply_patch",
                            "apply_patch_authority_changed",
                            &MezError::conflict(
                                "apply_patch sandbox write authority changed before dispatch",
                            ),
                        )?;
                        continue;
                    }
                    pending.plan
                }
                None => match local_action_plan(action) {
                    Ok(Some(plan)) => plan,
                    Ok(None) => continue,
                    Err(error) => {
                        let error = MezError::from(error);
                        let command = match &action.payload {
                            AgentActionPayload::ShellCommand { command, .. } => command.as_str(),
                            _ => "",
                        };
                        execution.action_results[index] = self.shell_action_runtime_error_result(
                            turn,
                            action,
                            command,
                            "local_action_plan",
                            &error,
                        )?;
                        continue;
                    }
                },
            };
            if is_apply_patch && !has_pending_apply_patch_phase {
                self.prepare_apply_patch_batched_read_plan(
                    turn,
                    action,
                    &mut plan,
                    &path_boundary,
                )?;
            }
            let command = plan.command.as_str();
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "action {} type={} readiness={}",
                    action.id,
                    action.action_type(),
                    runtime_pane_readiness_state_name(self.pane_readiness_state(&turn.pane_id))
                ),
            )?;
            if let Some(result) = self.shell_dispatch_loop_guard_failure(turn, action, command)? {
                let suppressed_duplicate =
                    runtime_action_result_is_suppressed_duplicate_file_mutation(&result);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "action {} {} reason=shell_dispatch_loop_guard",
                        action.id,
                        if suppressed_duplicate {
                            "succeeded"
                        } else {
                            "failed"
                        }
                    ),
                )?;
                if suppressed_duplicate {
                    self.append_agent_status_text_to_terminal_buffer(
                        &turn.pane_id,
                        "agent: duplicate file mutation skipped because the same mutation already succeeded",
                    )?;
                    self.append_action_result_context_if_absent(&turn.turn_id, &result)?;
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} continuing turn reason=duplicate_successful_file_mutation",
                            action.id
                        ),
                    )?;
                } else {
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} failed reason=shell_dispatch_loop_guard",
                            action.id
                        ),
                    )?;
                }
                execution.action_results[index] = result;
                continue;
            }
            match if native_mode {
                PaneReadinessState::Ready
            } else {
                self.pane_readiness_state(&turn.pane_id)
            } {
                PaneReadinessState::Ready => {}
                PaneReadinessState::Unknown
                | PaneReadinessState::PromptCandidate
                | PaneReadinessState::Degraded => {
                    if !self.turn_has_running_readiness_probe(&turn.turn_id) {
                        let status = if self.agent_verbose_enabled(&turn.pane_id)
                            || self.agent_trace_enabled(&turn.pane_id)
                        {
                            format!(
                                "agent: shell command waiting for shell readiness: {}",
                                runtime_agent_terminal_preview(command)
                            )
                        } else {
                            "agent: shell command waiting for shell readiness".to_string()
                        };
                        self.append_agent_status_text_to_terminal_buffer(&turn.pane_id, &status)?;
                        if let Err(error) = self.dispatch_readiness_probe_to_pane(turn) {
                            execution.action_results[index] = self
                                .shell_action_runtime_error_result(
                                    turn,
                                    action,
                                    command,
                                    "readiness_probe_dispatch",
                                    &error,
                                )?;
                            continue;
                        }
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!("action {} waiting reason=readiness_probe_sent", action.id),
                        )?;
                    } else {
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "action {} waiting reason=readiness_probe_already_running",
                                action.id
                            ),
                        )?;
                    }
                    self.integration
                        .runtime_metrics_mut()
                        .record_shell_action_batch(dispatched);
                    return Ok(dispatched);
                }
                PaneReadinessState::Busy => {
                    match self.pane_foreground_certified_shell_state(&turn.pane_id) {
                        Some(true) => {
                            self.set_pane_readiness(
                                &turn.pane_id,
                                PaneReadinessState::PromptCandidate,
                            );
                            self.append_agent_status_text_to_terminal_buffer(
                                &turn.pane_id,
                                "agent: shell readiness looked stale; probing before pending shell command",
                            )?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "pane_readiness busy -> prompt-candidate reason=stale_busy_dispatch_recovery action={}",
                                    action.id
                                ),
                            )?;
                            if let Err(error) = self.dispatch_readiness_probe_to_pane(turn) {
                                execution.action_results[index] = self
                                    .shell_action_runtime_error_result(
                                        turn,
                                        action,
                                        command,
                                        "readiness_probe_dispatch",
                                        &error,
                                    )?;
                                continue;
                            }
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "action {} waiting reason=stale_busy_readiness_probe_sent",
                                    action.id
                                ),
                            )?;
                        }
                        None => {
                            self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Degraded);
                            self.append_agent_status_text_to_terminal_buffer(
                                &turn.pane_id,
                                "agent: shell readiness metadata unavailable; probing before pending shell command",
                            )?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "pane_readiness busy -> degraded reason=unknown_busy_dispatch_recovery action={}",
                                    action.id
                                ),
                            )?;
                            if let Err(error) = self.dispatch_readiness_probe_to_pane(turn) {
                                execution.action_results[index] = self
                                    .shell_action_runtime_error_result(
                                        turn,
                                        action,
                                        command,
                                        "readiness_probe_dispatch",
                                        &error,
                                    )?;
                                continue;
                            }
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "action {} waiting reason=unknown_busy_readiness_probe_sent",
                                    action.id
                                ),
                            )?;
                        }
                        Some(false) => {
                            let attempts = self.pending_shell_dispatch_blocked_recovery_attempts(
                                &turn.turn_id,
                                &action.id,
                            );
                            let deadline_exhausted = self
                                .pending_shell_dispatch_blocked_recovery_deadline_exhausted(
                                    &turn.turn_id,
                                    &action.id,
                                );
                            if attempts >= 3 || deadline_exhausted {
                                let foreground_diagnostic =
                                    self.pane_foreground_process_diagnostic(&turn.pane_id);
                                let message = format!(
                                    "pane {} kept an uncertified foreground process group active; shell command was not dispatched ({})",
                                    turn.pane_id,
                                    foreground_diagnostic.summary(),
                                );
                                let mut result = ActionResult::failed(
                                    turn,
                                    action,
                                    ActionStatus::Denied,
                                    "foreground_process_blocked_dispatch",
                                    message.clone(),
                                )?;
                                result.structured_content_json = Some(
                                    serde_json::json!({
                                        "state": "dispatch_blocked",
                                        "reason": "uncertified_foreground_process",
                                        "confirmations": attempts,
                                        "deadline_exhausted": deadline_exhausted,
                                        "command": runtime_agent_context_command(action, command),
                                        "foreground_process": foreground_diagnostic.json(),
                                    })
                                    .to_string(),
                                );
                                execution.action_results[index] = result;
                                self.clear_pending_shell_dispatch_blocked_recovery_attempt(
                                    &turn.turn_id,
                                    &action.id,
                                );
                                self.append_agent_error_text_to_terminal_buffer(
                                    &turn.pane_id,
                                    &format!("agent: {message}"),
                                )?;
                                self.append_agent_trace_turn_event(
                                    &turn.pane_id,
                                    &turn.turn_id,
                                    &format!(
                                        "action {} failed reason=foreground_process_blocked_dispatch confirmations={} deadline_exhausted={} {}",
                                        action.id, attempts, deadline_exhausted,
                                        foreground_diagnostic.summary(),
                                    ),
                                )?;
                                break;
                            }
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "action {} waiting reason=pane_readiness_busy attempts={}",
                                    action.id, attempts
                                ),
                            )?;
                        }
                    }
                    self.integration
                        .runtime_metrics_mut()
                        .record_shell_action_batch(dispatched);
                    return Ok(dispatched);
                }
                PaneReadinessState::Probing => {
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} waiting reason=pane_readiness_{}",
                            action.id,
                            runtime_pane_readiness_state_name(
                                self.pane_readiness_state(&turn.pane_id)
                            )
                        ),
                    )?;
                    self.integration
                        .runtime_metrics_mut()
                        .record_shell_action_batch(dispatched);
                    return Ok(dispatched);
                }
                state @ (PaneReadinessState::FullScreen
                | PaneReadinessState::PasswordPrompt
                | PaneReadinessState::InteractiveBlocked)
                    if self.pane_foreground_certified_shell_state(&turn.pane_id) == Some(true) =>
                {
                    self.set_pane_readiness(&turn.pane_id, PaneReadinessState::PromptCandidate);
                    self.append_agent_status_text_to_terminal_buffer(
                        &turn.pane_id,
                        "agent: shell interactivity block looked stale; probing before pending shell command",
                    )?;
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "pane_readiness {} -> prompt-candidate reason=stale_interactive_blocked_dispatch_recovery action={}",
                            runtime_pane_readiness_state_name(state),
                            action.id
                        ),
                    )?;
                    if !self.turn_has_running_readiness_probe(&turn.turn_id) {
                        if let Err(error) = self.dispatch_readiness_probe_to_pane(turn) {
                            execution.action_results[index] = self
                                .shell_action_runtime_error_result(
                                    turn,
                                    action,
                                    command,
                                    "readiness_probe_dispatch",
                                    &error,
                                )?;
                            continue;
                        }
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "action {} waiting reason=stale_interactive_blocked_readiness_probe_sent",
                                action.id
                            ),
                        )?;
                    } else {
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "action {} waiting reason=stale_interactive_blocked_readiness_probe_already_running",
                                action.id
                            ),
                        )?;
                    }
                    self.integration
                        .runtime_metrics_mut()
                        .record_shell_action_batch(dispatched);
                    return Ok(dispatched);
                }
                state => {
                    let foreground_diagnostic =
                        self.pane_foreground_process_diagnostic(&turn.pane_id);
                    let message = format!(
                        "pane {} is not ready for agent shell input: {} ({})",
                        turn.pane_id,
                        runtime_pane_readiness_state_name(state),
                        foreground_diagnostic.summary(),
                    );
                    let mut result = ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        "pane_not_ready",
                        message.clone(),
                    )?;
                    result.structured_content_json = Some(
                        serde_json::json!({
                            "state": "not_ready",
                            "readiness_state": runtime_pane_readiness_state_name(state),
                            "command": runtime_agent_context_command(action, command),
                            "foreground_process": foreground_diagnostic.json(),
                        })
                        .to_string(),
                    );
                    execution.action_results[index] = result;
                    self.append_agent_error_text_to_terminal_buffer(
                        &turn.pane_id,
                        &format!("agent: {message}"),
                    )?;
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} failed reason=pane_not_ready readiness={} {}",
                            action.id,
                            runtime_pane_readiness_state_name(state),
                            foreground_diagnostic.summary(),
                        ),
                    )?;
                    break;
                }
            }
            let hook_decision = self.run_configured_pre_action_hooks_with_continuation(
                HookEvent::PreShellCommand,
                &runtime_pre_shell_hook_payload(turn, action, command),
                Some(PendingFocusedShellHookContinuation {
                    turn_id: turn.turn_id.clone(),
                    action_id: action.id.clone(),
                    phase_command_sha256: exact_command_sha256(
                        DEFAULT_COMMAND_SHELL_CLASSIFICATION,
                        command,
                    ),
                }),
            )?;
            match hook_decision {
                RuntimeHookPipelineDecision::Continue => {}
                RuntimeHookPipelineDecision::Pending => {
                    if is_apply_patch {
                        self.agent.pending_apply_patch_phases.insert(
                            apply_patch_state_key.clone(),
                            RuntimePendingApplyPatchPhase {
                                plan: plan.clone(),
                                path_boundary: path_boundary.clone(),
                            },
                        );
                    }
                    execution.action_results[index].structured_content_json =
                        Some(mez_agent::shell_action_structured_content_json(
                            action,
                            &plan,
                            Some("pane_shell"),
                            false,
                            serde_json::json!({
                                "state": "pre_shell_hook_pending",
                                "kind": action.action_type(),
                                "action_id": action.id.as_str(),
                                "command": runtime_agent_context_command(action, command)
                            }),
                            &[],
                            serde_json::json!({"state":"pre_shell_hook_pending"}),
                        ));
                    self.append_agent_status_text_to_terminal_buffer(
                        &turn.pane_id,
                        "agent: shell command waiting for pre-action hook",
                    )?;
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!("action {} waiting reason=pre_shell_hook_pending", action.id),
                    )?;
                    self.integration
                        .runtime_metrics_mut()
                        .record_shell_action_batch(dispatched);
                    return Ok(dispatched);
                }
                RuntimeHookPipelineDecision::Block(block) => {
                    let mut blocked = ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Denied,
                        "hook_blocked",
                        block.message.clone(),
                    )?;
                    blocked.structured_content_json = Some(block.structured_json());
                    execution.action_results[index] = blocked;
                    self.append_agent_error_text_to_terminal_buffer(
                        &turn.pane_id,
                        &format!(
                            "agent: shell command blocked by hook {}: {}",
                            block.hook_id, block.message
                        ),
                    )?;
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} denied reason=pre_shell_hook hook={}",
                            action.id, block.hook_id
                        ),
                    )?;
                    continue;
                }
            }
            if !is_apply_patch {
                sandbox_bypassed = bubblewrap_applies
                    && self.activate_sandbox_bypass_after_approval(&turn.turn_id, &action.id);
            }
            if !is_apply_patch && bubblewrap_applies && !sandbox_bypassed && !native_mode {
                match self.ensure_bubblewrap_path_resolution_for_action(
                    turn,
                    &action.id,
                    permission_evaluation.as_deref(),
                ) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        execution.action_results[index] = self.shell_action_runtime_error_result(
                            turn,
                            action,
                            command,
                            "bubblewrap_path_resolution",
                            &error,
                        )?;
                        continue;
                    }
                }
                match self.ensure_bubblewrap_environment_evidence_for_action(turn, &action.id) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        execution.action_results[index] = self.shell_action_runtime_error_result(
                            turn,
                            action,
                            command,
                            "bubblewrap_environment_evidence",
                            &error,
                        )?;
                        continue;
                    }
                }
                match self.ensure_bubblewrap_capability_for_action(turn, &action.id) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        execution.action_results[index] = self.shell_action_runtime_error_result(
                            turn,
                            action,
                            command,
                            "bubblewrap_capability_probe",
                            &error,
                        )?;
                        continue;
                    }
                }
            }
            let dispatch_outcome = self.dispatch_shell_action_to_pane(
                turn,
                action,
                super::shell_state::ShellActionDispatch {
                    command,
                    preview_already_presented: self.agent_streaming_say_action_is_promoted(
                        &turn.pane_id,
                        &turn.turn_id,
                        action_index,
                    ),
                    input_sidecar: plan.input_sidecar.as_deref(),
                    program_dialect: plan.program_dialect,
                    stateful: plan.stateful,
                    interactive: plan.interactive,
                    timeout_ms: plan.timeout_ms,
                    permission_evaluation: permission_evaluation.as_deref(),
                },
            );
            match dispatch_outcome {
                Ok(super::shell_state::ShellActionDispatchOutcome::Dispatched) => {
                    self.record_shell_dispatch_history(&turn.turn_id, command);
                    dispatched = dispatched.saturating_add(1);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} dispatched shell_transaction dispatched_count={}",
                            action.id, dispatched
                        ),
                    )?;
                    break;
                }
                Ok(super::shell_state::ShellActionDispatchOutcome::NativeDispatched) => {
                    self.record_shell_dispatch_history(&turn.turn_id, command);
                    dispatched = dispatched.saturating_add(1);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} dispatched spawned_shell dispatched_count={}",
                            action.id, dispatched
                        ),
                    )?;
                    break;
                }
                Ok(super::shell_state::ShellActionDispatchOutcome::SandboxFallbackEligible {
                    marker,
                    proof,
                }) => {
                    self.mark_sandbox_preparation_fallback_pending(
                        turn, action, execution, index, &marker, &proof,
                    )?;
                    continue;
                }
                Err(error) => {
                    execution.action_results[index] = self.shell_action_runtime_error_result(
                        turn,
                        action,
                        command,
                        "shell_dispatch",
                        &error,
                    )?;
                    continue;
                }
            }
        }
        if !self.turn_has_running_agent_action_shell_transaction(&turn.turn_id)
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            self.queue_agent_provider_task(turn.turn_id.clone());
        }
        execution.terminal_state =
            if self.turn_has_running_agent_action_shell_transaction(&turn.turn_id) {
                AgentTurnState::Running
            } else {
                runtime_agent_turn_state_from_action_results(
                    &execution.action_results,
                    execution.final_turn,
                )
            };
        self.integration
            .runtime_metrics_mut()
            .record_shell_action_batch(dispatched);
        Ok(dispatched)
    }

    /// Marks one typed Bubblewrap preparation failure as approval-pending.
    ///
    /// The caller owns the in-flight execution, so this transition mutates
    /// that execution in place and records only the exact fallback identity.
    /// Ordinary provider or stored-dispatch settlement then persists the
    /// blocked execution and queues the approval through the shared path.
    fn mark_sandbox_preparation_fallback_pending(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        execution: &mut AgentTurnExecution,
        result_index: usize,
        marker: &str,
        proof: &str,
    ) -> Result<()> {
        let plan = local_action_plan(action)?.ok_or_else(|| {
            MezError::invalid_state("sandbox preparation fallback action is not shell-backed")
        })?;
        let evaluation = execution.action_results[result_index]
            .permission_evaluation
            .clone()
            .ok_or_else(|| {
                MezError::invalid_state(
                    "sandbox preparation fallback requires a permission evaluation",
                )
            })?;
        if evaluation.decision != mez_agent::permissions::RuleDecision::Allow {
            return Err(MezError::invalid_state(
                "sandbox preparation fallback requires an explicitly allowed action",
            ));
        }
        let mut blocked = ActionResult::blocked(
            turn,
            action,
            vec![
                "Bubblewrap could not represent the approved policy requirements before payload execution"
                    .to_string(),
                "approval is required for one exact unsandboxed retry".to_string(),
            ],
            mez_agent::shell_action_structured_content_json(
                action,
                &plan,
                Some("pane_shell"),
                true,
                serde_json::json!({
                    "state": "pending",
                    "kind": action.action_type(),
                    "action_id": action.id.as_str(),
                    "command": plan.policy_command,
                    "sandbox_fallback": {
                        "backend": "bubblewrap",
                        "reason": "preparation_failure",
                        "proof": proof,
                        "payload_exec_proven": false,
                        "partial_effect_warning": false
                    }
                }),
                &evaluation.matched_rule_ids,
                serde_json::json!({
                    "source": "runtime",
                    "marker": marker,
                    "boundary_state": "preparation_failure",
                    "payload_exec_proven": false,
                    "partial_effect_warning": false
                }),
            ),
        );
        blocked.permission_evaluation = Some(evaluation);
        execution.action_results[result_index] = blocked;
        self.agent.sandbox_fallback_audits.insert(
            (turn.turn_id.clone(), action.id.clone()),
            RuntimeSandboxFallbackAudit {
                reason: "preparation_failure".to_string(),
                proof: proof.to_string(),
                partial_effect_warning: false,
                approving_client_id: None,
            },
        );
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} blocked reason=bubblewrap_preparation_failure marker={marker}",
                action.id
            ),
        )?;
        Ok(())
    }

    /// Dispatches the verified write phase for a completed `apply_patch`
    /// snapshot transaction.
    ///
    /// `apply_patch` is multi-phase by design: the first shell transaction only
    /// snapshots remote file bytes, Rust applies the Mezzanine patch internally, and
    /// the second shell transaction verifies the snapshots and writes final bytes.
    /// Returning `true` means the original action remains running while the
    /// generated write transaction settles.
    ///
    /// # Parameters
    /// - `turn`: The running agent turn that owns the action.
    /// - `action_id`: The action whose read transaction completed.
    /// - `transaction`: The completed read transaction state.
    /// - `exit_code`: The shell exit status observed for the read transaction.
    pub(crate) fn dispatch_apply_patch_followup_if_needed(
        &mut self,
        turn: &AgentTurnRecord,
        action_id: &str,
        transaction: &RunningShellTransactionRef,
        exit_code: i32,
    ) -> Result<bool> {
        let state_key = Self::apply_patch_batch_state_key(&turn.turn_id, action_id);
        if exit_code != 0 {
            self.agent.apply_patch_batch_states.remove(&state_key);
            return Ok(false);
        }
        if apply_patch_transaction_phase(&transaction.command)
            != Some(ApplyPatchTransactionPhase::Read)
        {
            return Ok(false);
        }
        let execution = self
            .agent_turn_executions()
            .get(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("running agent execution is unavailable"))?;
        let batch = execution.response.action_batch.as_ref().ok_or_else(|| {
            MezError::invalid_state("running agent execution has no action batch")
        })?;
        let Some(action) = batch
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .cloned()
        else {
            self.agent.apply_patch_batch_states.remove(&state_key);
            return Ok(false);
        };
        let AgentActionPayload::ApplyPatch { patch, .. } = &action.payload else {
            return Ok(false);
        };
        let path_boundary = self.apply_patch_path_boundary_for_action(turn, &action.id)?;
        let write_plan = if let Some(mut state) =
            self.agent.apply_patch_batch_states.remove(&state_key)
        {
            if state.path_boundary != path_boundary {
                return Err(MezError::conflict(
                    "apply_patch sandbox write authority changed after the read phase",
                ));
            }
            let retained_transport;
            let retained_transport = if state.current_read_transport.is_empty() {
                transaction.observed_output_preview.as_str()
            } else {
                retained_transport = String::from_utf8_lossy(&state.current_read_transport);
                retained_transport.as_ref()
            };
            let decoded_output = decode_shell_output_transport_with_diagnostics(retained_transport);
            if (state.current_read_transport.is_empty() && transaction.observed_output_truncated)
                || decoded_output.diagnostics.transport_incomplete()
                || decoded_output.diagnostics.output_truncated()
            {
                if state.current_path.is_some() && state.current_path_read_retries == 0 {
                    let mut paths = BTreeSet::new();
                    paths.insert(state.current_path.clone().unwrap_or_default());
                    state.current_path_read_retries = 1;
                    state.current_read_transport.clear();
                    let read_plan =
                        apply_patch_read_plan_for_paths_with_boundary(&paths, &path_boundary);
                    self.agent.apply_patch_batch_states.insert(state_key, state);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} apply_patch_phase=read reason=retry_incomplete_transport",
                            action.id
                        ),
                    )?;
                    self.dispatch_generated_apply_patch_phase(
                        turn,
                        &action,
                        read_plan,
                        path_boundary,
                    )?;
                    return Ok(true);
                }
                apply_patch_error_plan(&apply_patch_read_transport_failure_message(
                    &decoded_output.diagnostics,
                    transaction.observed_output_truncated,
                ))
            } else {
                state.read_outputs.push(decoded_output.output);
                state.current_read_transport.clear();
                state.current_path = None;
                if !state.remaining_paths.is_empty() {
                    let path = state.remaining_paths.remove(0);
                    let mut paths = BTreeSet::new();
                    paths.insert(path.clone());
                    state.current_path = Some(path);
                    state.current_path_read_retries = 0;
                    let read_plan =
                        apply_patch_read_plan_for_paths_with_boundary(&paths, &path_boundary);
                    self.agent.apply_patch_batch_states.insert(state_key, state);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} apply_patch_phase=read reason=next_batch_read",
                            action.id
                        ),
                    )?;
                    self.dispatch_generated_apply_patch_phase(
                        turn,
                        &action,
                        read_plan,
                        path_boundary,
                    )?;
                    return Ok(true);
                }
                apply_patch_write_plan_from_read_outputs_with_boundary(
                    patch,
                    &state.read_outputs,
                    &path_boundary,
                )
                .unwrap_or_else(|error| apply_patch_error_plan(error.message()))
            }
        } else {
            let decoded_output = decode_shell_output_transport_with_diagnostics(
                &transaction.observed_output_preview,
            );
            if transaction.observed_output_truncated
                || decoded_output.diagnostics.transport_incomplete()
                || decoded_output.diagnostics.output_truncated()
            {
                apply_patch_error_plan(&apply_patch_read_transport_failure_message(
                    &decoded_output.diagnostics,
                    transaction.observed_output_truncated,
                ))
            } else {
                apply_patch_write_plan_from_read_outputs_with_boundary(
                    patch,
                    std::slice::from_ref(&decoded_output.output),
                    &path_boundary,
                )
                .unwrap_or_else(|error| apply_patch_error_plan(error.message()))
            }
        };

        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} apply_patch_phase=write reason=read_phase_completed",
                action.id
            ),
        )?;
        self.dispatch_generated_apply_patch_phase(turn, &action, write_plan, path_boundary)?;
        Ok(true)
    }

    /// Plans the next generated `apply_patch` phase after a spawned-shell
    /// read transaction completed, without dispatching it.
    ///
    /// Native read output arrives as one raw combined capture, so transport
    /// decoding is skipped and the batch state machine runs directly on the
    /// captured bytes. This mirrors `dispatch_apply_patch_followup_if_needed`
    /// for the pane transport: a truncated first read retries once, complete
    /// reads advance to the next batched path or the write phase, and terminal
    /// failures produce an error plan. The caller stores the returned plan in
    /// `pending_apply_patch_phases` so the ordinary dispatch loop re-runs the
    /// same hook and authorization path as provider-authored shell actions.
    ///
    /// Returns `None` when the read completed nonzero or the action is not an
    /// `apply_patch` read, meaning the projected read result settles normally.
    fn plan_apply_patch_followup_from_read_output(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        path_boundary: &ApplyPatchPathBoundary,
        exit_code: i32,
        raw_output: &str,
        output_truncated: bool,
    ) -> Result<Option<mez_agent::LocalActionPlan>> {
        let state_key = Self::apply_patch_batch_state_key(&turn.turn_id, &action.id);
        if exit_code != 0 {
            self.agent.apply_patch_batch_states.remove(&state_key);
            return Ok(None);
        }
        let AgentActionPayload::ApplyPatch { patch, .. } = &action.payload else {
            return Ok(None);
        };
        let followup_plan =
            if let Some(mut state) = self.agent.apply_patch_batch_states.remove(&state_key) {
                if &state.path_boundary != path_boundary {
                    return Err(MezError::conflict(
                        "apply_patch sandbox write authority changed after the read phase",
                    ));
                }
                if output_truncated {
                    if state.current_path.is_some() && state.current_path_read_retries == 0 {
                        let mut paths = BTreeSet::new();
                        paths.insert(state.current_path.clone().unwrap_or_default());
                        state.current_path_read_retries = 1;
                        state.current_read_transport.clear();
                        let read_plan =
                            apply_patch_read_plan_for_paths_with_boundary(&paths, path_boundary);
                        self.agent.apply_patch_batch_states.insert(state_key, state);
                        self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} apply_patch_phase=read reason=retry_incomplete_transport",
                            action.id
                        ),
                    )?;
                        return Ok(Some(read_plan));
                    }
                    Some(apply_patch_error_plan(
                        &apply_patch_read_transport_failure_message(
                            &mez_agent::ShellTransportDiagnostics::default(),
                            output_truncated,
                        ),
                    ))
                } else {
                    state.read_outputs.push(raw_output.to_string());
                    state.current_read_transport.clear();
                    state.current_path = None;
                    if !state.remaining_paths.is_empty() {
                        let path = state.remaining_paths.remove(0);
                        let mut paths = BTreeSet::new();
                        paths.insert(path.clone());
                        state.current_path = Some(path);
                        state.current_path_read_retries = 0;
                        let read_plan =
                            apply_patch_read_plan_for_paths_with_boundary(&paths, path_boundary);
                        self.agent.apply_patch_batch_states.insert(state_key, state);
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "action {} apply_patch_phase=read reason=next_batch_read",
                                action.id
                            ),
                        )?;
                        return Ok(Some(read_plan));
                    }
                    Some(
                        apply_patch_write_plan_from_read_outputs_with_boundary(
                            patch,
                            &state.read_outputs,
                            path_boundary,
                        )
                        .unwrap_or_else(|error| apply_patch_error_plan(error.message())),
                    )
                }
            } else if output_truncated {
                Some(apply_patch_error_plan(
                    &apply_patch_read_transport_failure_message(
                        &mez_agent::ShellTransportDiagnostics::default(),
                        output_truncated,
                    ),
                ))
            } else {
                Some(
                    apply_patch_write_plan_from_read_outputs_with_boundary(
                        patch,
                        std::slice::from_ref(&raw_output.to_string()),
                        path_boundary,
                    )
                    .unwrap_or_else(|error| apply_patch_error_plan(error.message())),
                )
            };
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} apply_patch_phase=write reason=read_phase_completed",
                action.id
            ),
        )?;
        Ok(followup_plan)
    }

    /// Converts a local shell dispatch failure into a normal agent action
    /// result instead of allowing the async provider service to fail upward.
    ///
    /// Runtime shell dispatch sits after provider completion, so pane I/O,
    /// readiness-probe, or terminal-presentation failures are actionable agent
    /// failures rather than daemon supervision failures. The returned result is
    /// structured for transcript/audit/debug consumers, and the best-effort pane
    /// log keeps the active user informed when the pane still exists.
    fn shell_action_runtime_error_result(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        command: &str,
        stage: &str,
        error: &MezError,
    ) -> Result<ActionResult> {
        self.append_sandbox_fallback_result_audit(&turn.turn_id, &action.id, "failed")?;
        self.clear_sandbox_bypass_for_action(&turn.turn_id, &action.id);
        let error_kind = runtime_mezzanine_error_code(error.kind());
        let failure_message = if stage.starts_with("bubblewrap_")
            || (stage == "shell_dispatch"
                && matches!(
                    self.sandbox_config_for_pane(&turn.pane_id),
                    SandboxConfig::Bubblewrap(_)
                )) {
            crate::security::sandbox::bubblewrap_failure_remediation(error.message())
        } else {
            error.message().to_string()
        };
        let error_message = format!("{stage}: {failure_message}");
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Failed,
            error_kind,
            error_message.clone(),
        )?;
        let execution_transport = "pane_shell";
        let plan = local_action_plan(action)?.ok_or_else(|| {
            MezError::invalid_state("shell dispatch failure requires a shell-backed action")
        })?;
        result.structured_content_json = Some(mez_agent::shell_action_structured_content_json(
            action,
            &plan,
            Some(execution_transport),
            false,
            serde_json::Value::Null,
            &[],
            serde_json::json!({
                "state": "dispatch_failed",
                "stage": stage,
                "command": runtime_agent_context_command(action, command),
                "error": {
                    "kind": error_kind,
                    "message": failure_message
                }
            }),
        ));
        let _ = self.append_agent_error_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: shell command failed before execution: {}",
                failure_message
            ),
        );
        let _ = self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} failed reason={} error_kind={} message={}",
                action.id,
                stage,
                error_kind,
                error.message()
            ),
        );
        let _ = self.append_agent_shell_command_audit(turn, action, command, None, None, "failed");
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::apply_patch_read_transport_failure_message;
    use mez_agent::ShellTransportDiagnostics;

    /// Verifies terminal patch-read failures retain a specific decoded
    /// transport cause instead of collapsing it into generic truncation.
    #[test]
    fn apply_patch_read_transport_failure_message_prefers_detected_cause() {
        let diagnostics = ShellTransportDiagnostics {
            missing_end_marker: true,
            ..Default::default()
        };

        let message = apply_patch_read_transport_failure_message(&diagnostics, true);

        assert!(
            message.contains("transport end marker was missing"),
            "{message}"
        );
        assert!(
            !message.contains("pane observation capture was truncated"),
            "{message}"
        );
    }

    /// Verifies a capture-boundary failure remains actionable when decoding
    /// cannot identify a more specific transport fault.
    #[test]
    fn apply_patch_read_transport_failure_message_reports_capture_truncation() {
        let message =
            apply_patch_read_transport_failure_message(&ShellTransportDiagnostics::default(), true);

        assert!(
            message.contains("pane observation capture was truncated"),
            "{message}"
        );
    }
}
