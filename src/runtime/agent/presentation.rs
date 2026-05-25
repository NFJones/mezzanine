//! Runtime agent terminal presentation helpers.
//!
//! This module owns model-response and action-outcome rendering decisions for
//! pane transcript buffers. It centralizes visible rationale, `say` action,
//! deferred output, and terminal failure diagnostics so execution modules can
//! update state without duplicating display policy.

use super::*;

impl RuntimeSessionService {
    /// Runs the present agent response actions to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn present_agent_response_actions_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            if execution.terminal_state == AgentTurnState::Completed
                && !execution.response.raw_text.trim().is_empty()
            {
                self.append_agent_assistant_text_to_terminal_buffer(
                    pane_id,
                    &execution.response.raw_text,
                )?;
            }
            return Ok(());
        };

        let visible_action_texts = runtime_agent_batch_visible_action_texts(batch);
        if !batch.rationale.trim().is_empty()
            && !runtime_agent_batch_rationale_repeats_visible_batch_text(
                batch,
                &visible_action_texts,
            )
        {
            self.append_agent_thinking_text_to_terminal_buffer(pane_id, batch.rationale.trim())?;
        }
        if self.agent_verbose_enabled(pane_id)
            && let Some(thought) = batch.thought.as_deref()
            && !thought.trim().is_empty()
        {
            self.append_agent_thinking_text_to_terminal_buffer(pane_id, thought.trim())?;
        }
        let mut emitted_user_visible_action = false;
        let mut pending_runtime_visible_action = false;
        let mut emitted_action_rationale_keys = BTreeSet::new();
        let has_runtime_visible_action = batch
            .actions
            .iter()
            .any(runtime_agent_action_has_runtime_visible_effect);
        for action in &batch.actions {
            let rationale_key = normalize_agent_user_visible_text(&action.rationale);
            if !action.rationale.trim().is_empty()
                && !runtime_agent_action_rationale_repeats_visible_summary(action)
                && !runtime_agent_action_rationale_repeats_visible_batch_text(
                    action,
                    &visible_action_texts,
                )
                && emitted_action_rationale_keys.insert(rationale_key)
            {
                self.append_agent_thinking_text_to_terminal_buffer(
                    pane_id,
                    action.rationale.trim(),
                )?;
            }
            match &action.payload {
                AgentActionPayload::Say {
                    status,
                    text,
                    content_type,
                } => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    if has_runtime_visible_action && *status != SayStatus::Progress {
                        pending_runtime_visible_action = true;
                    } else {
                        emitted_user_visible_action = true;
                        self.append_agent_assistant_content_to_terminal_buffer(
                            pane_id,
                            text,
                            content_type,
                        )?;
                    }
                }
                AgentActionPayload::RequestCapability { .. }
                | AgentActionPayload::RequestSkills
                | AgentActionPayload::CallSkill { .. } => {}
                AgentActionPayload::Abort { reason } => {
                    emitted_user_visible_action = true;
                    self.append_agent_error_text_to_terminal_buffer(
                        pane_id,
                        &format!("agent: aborted: {reason}"),
                    )?;
                }
                AgentActionPayload::ShellCommand { .. }
                | AgentActionPayload::ApplyPatch { .. }
                | AgentActionPayload::WebSearch { .. }
                | AgentActionPayload::FetchUrl { .. } => {
                    pending_runtime_visible_action = true;
                }
                AgentActionPayload::McpCall { .. }
                | AgentActionPayload::SendMessage { .. }
                | AgentActionPayload::SpawnAgent { .. }
                | AgentActionPayload::ConfigChange { .. } => {
                    pending_runtime_visible_action = true;
                }
                AgentActionPayload::Complete => {}
            }
        }
        if execution.terminal_state == AgentTurnState::Completed
            && !emitted_user_visible_action
            && !pending_runtime_visible_action
        {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                "agent: completed without a user-facing response",
            )?;
        }
        Ok(())
    }

    /// Presents deferred `say` actions once a mixed response's runtime-visible
    /// actions have finished and emitted their own logs or diffs.
    pub(in crate::runtime) fn present_deferred_agent_say_actions_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
    ) -> Result<usize> {
        if execution.terminal_state != AgentTurnState::Completed {
            return Ok(0);
        }
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return Ok(0);
        };
        if !batch
            .actions
            .iter()
            .any(runtime_agent_action_has_runtime_visible_effect)
        {
            return Ok(0);
        }

        let mut emitted = 0usize;
        for action in &batch.actions {
            if let AgentActionPayload::Say {
                status,
                text,
                content_type,
            } = &action.payload
            {
                if *status == SayStatus::Progress || text.trim().is_empty() {
                    continue;
                }
                self.append_agent_assistant_content_to_terminal_buffer(
                    pane_id,
                    text,
                    content_type,
                )?;
                emitted = emitted.saturating_add(1);
            }
        }
        Ok(emitted)
    }

    /// Presents runtime-gated action outcomes that otherwise would not have a
    /// natural command, tool, or assistant-output line in the pane buffer.
    pub(in crate::runtime) fn present_agent_action_outcomes_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return Ok(());
        };
        let mut aggregated_result_ids = BTreeSet::new();
        for (code, label) in [
            (
                "shell_dispatch_limit_exceeded",
                runtime_loop_guard_failure_label("shell_dispatch_limit_exceeded")
                    .unwrap_or("shell dispatch"),
            ),
            (
                "network_action_limit_exceeded",
                runtime_loop_guard_failure_label("network_action_limit_exceeded")
                    .unwrap_or("network action"),
            ),
        ] {
            let matching_results = execution
                .action_results
                .iter()
                .filter(|result| {
                    result.is_error && runtime_action_result_has_error_code(result, code)
                })
                .collect::<Vec<_>>();
            if matching_results.is_empty() {
                continue;
            }
            let message = matching_results
                .iter()
                .find_map(|result| result.error.as_ref().map(|error| error.message.as_str()))
                .unwrap_or("runtime loop guard suppressed this action batch");
            self.append_agent_error_text_to_terminal_buffer(
                pane_id,
                &runtime_loop_guard_failure_summary_line(label, matching_results.len(), message),
            )?;
            aggregated_result_ids.extend(
                matching_results
                    .iter()
                    .map(|result| result.action_id.clone()),
            );
        }
        for result in &execution.action_results {
            if aggregated_result_ids.contains(&result.action_id) {
                continue;
            }
            let Some(action) = batch
                .actions
                .iter()
                .find(|action| action.id == result.action_id)
            else {
                continue;
            };
            let Some((is_error, line)) = runtime_agent_action_outcome_line(
                action,
                result,
                self.agent_verbose_enabled(pane_id) || self.agent_trace_enabled(pane_id),
            ) else {
                continue;
            };
            if is_error {
                self.append_agent_error_text_to_terminal_buffer(pane_id, &line)?;
            } else {
                self.append_agent_status_text_to_terminal_buffer(pane_id, &line)?;
            }
        }
        Ok(())
    }

    /// Presents bounded failure details when the runtime is ending a failed
    /// turn instead of giving the model another recovery attempt.
    pub(in crate::runtime) fn present_unrecovered_agent_failure_diagnostics_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        reason: &str,
    ) -> Result<()> {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return Ok(());
        };
        let mut aggregated_result_ids = BTreeSet::new();
        for (code, label) in [
            (
                "shell_dispatch_limit_exceeded",
                runtime_loop_guard_failure_label("shell_dispatch_limit_exceeded")
                    .unwrap_or("shell dispatch"),
            ),
            (
                "network_action_limit_exceeded",
                runtime_loop_guard_failure_label("network_action_limit_exceeded")
                    .unwrap_or("network action"),
            ),
        ] {
            let matching_results = execution
                .action_results
                .iter()
                .filter(|result| {
                    runtime_action_result_is_terminal_failure(result)
                        && runtime_action_result_has_error_code(result, code)
                })
                .collect::<Vec<_>>();
            if matching_results.is_empty() {
                continue;
            }
            let message = matching_results
                .iter()
                .find_map(|result| result.error.as_ref().map(|error| error.message.as_str()))
                .unwrap_or("runtime loop guard suppressed this action batch");
            self.append_agent_error_text_to_terminal_buffer(
                pane_id,
                &format!(
                    "{}; {reason}",
                    runtime_loop_guard_failure_summary_line(label, matching_results.len(), message)
                ),
            )?;
            aggregated_result_ids.extend(
                matching_results
                    .iter()
                    .map(|result| result.action_id.clone()),
            );
        }
        for result in execution
            .action_results
            .iter()
            .filter(|result| runtime_action_result_is_terminal_failure(result))
        {
            if aggregated_result_ids.contains(&result.action_id) {
                continue;
            }
            let Some(action) = batch
                .actions
                .iter()
                .find(|action| action.id == result.action_id)
            else {
                continue;
            };
            let label = runtime_agent_action_summary(action)
                .unwrap_or_else(|| format!("{} action {}", result.action_type, result.action_id));
            let detail = runtime_agent_action_error_suffix(result);
            let mut message = format!("agent: {label} failed; {reason}{detail}");
            if let Some(output) = runtime_unrecovered_action_failure_output(result) {
                let lines = runtime_unrecovered_failure_output_lines(action, &output);
                if !lines.is_empty() {
                    message.push('\n');
                    message.push_str(&lines.join("\n"));
                }
            }
            self.append_agent_error_text_to_terminal_buffer(pane_id, &message)?;
        }
        Ok(())
    }
}
