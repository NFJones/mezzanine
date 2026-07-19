//! Runtime agent terminal presentation helpers.
//!
//! This module owns model-response and action-outcome rendering decisions for
//! pane transcript buffers. It centralizes visible rationale, `say` action,
//! deferred output, and terminal failure diagnostics so execution modules can
//! update state without duplicating display policy.

use super::{
    AgentActionPayload, AgentTurnExecution, AgentTurnState, BTreeSet, Result,
    RuntimeSessionService, SayStatus, normalize_agent_user_visible_text,
    runtime_action_result_has_error_code, runtime_action_result_is_terminal_failure,
    runtime_agent_action_error_suffix, runtime_agent_action_has_runtime_visible_effect,
    runtime_agent_action_outcome_line, runtime_agent_action_rationale_repeats_visible_batch_text,
    runtime_agent_action_rationale_repeats_visible_summary, runtime_agent_action_summary,
    runtime_agent_batch_rationale_repeats_visible_batch_text,
    runtime_agent_batch_visible_action_texts, runtime_agent_execution_failure_error,
    runtime_agent_turn_state_name, runtime_loop_guard_failure_label,
    runtime_loop_guard_failure_summary_line, runtime_unrecovered_action_failure_output,
    runtime_unrecovered_failure_output_lines,
};

/// Formats one last-request context snapshot for pane status.
///
/// The display is a bounded status indicator, so accepted provider responses
/// whose token count exceeds the configured profile window saturate at `100%`
/// instead of rendering impossible percentages above the full window.
pub(super) fn runtime_agent_provider_context_usage_display(
    snapshot: mez_agent::AgentContextUsageSnapshot,
) -> Option<String> {
    if snapshot.input_tokens == 0 || snapshot.context_window_tokens == 0 {
        return None;
    }
    let budget_tokens = snapshot.context_window_tokens;
    let percentage = snapshot
        .input_tokens
        .saturating_mul(100)
        .saturating_add(budget_tokens / 2)
        / budget_tokens;
    Some(format!("{}%", percentage.min(100)))
}

/// Builds the terminal prompt lines that summarize one agent execution.
pub(super) fn runtime_agent_execution_prompt_display_lines(
    turn_id: &str,
    provider_id: &str,
    execution: &AgentTurnExecution,
    dispatched_actions: usize,
    transcript_entries: usize,
) -> Vec<String> {
    let state = runtime_agent_turn_state_name(execution.terminal_state);
    let mut lines = vec![format!("agent: turn {turn_id} {state}")];
    lines.push(format!("agent: provider {provider_id} responded"));
    if dispatched_actions > 0 {
        lines.push(format!("agent: dispatched {dispatched_actions} actions"));
    }
    if transcript_entries > 0 {
        lines.push(format!(
            "agent: recorded {transcript_entries} transcript entries"
        ));
    }
    match execution.terminal_state {
        AgentTurnState::Completed if execution.response.action_batch.is_none() => {
            lines.extend(
                execution
                    .response
                    .raw_text
                    .lines()
                    .take(200)
                    .map(ToOwned::to_owned),
            );
        }
        AgentTurnState::Completed => {}
        AgentTurnState::Failed => {
            lines.extend(runtime_agent_failed_execution_prompt_display_lines(
                execution,
            ));
        }
        AgentTurnState::Blocked => {
            lines.push("agent: blocked pending approval".to_string());
        }
        AgentTurnState::Running => {
            lines.push("agent: waiting for pane, tool, or provider continuation".to_string());
        }
        AgentTurnState::Queued | AgentTurnState::Interrupted => {}
    }
    lines
}

/// Returns prompt display lines for a failed provider execution.
fn runtime_agent_failed_execution_prompt_display_lines(
    execution: &AgentTurnExecution,
) -> Vec<String> {
    let failure = runtime_agent_execution_failure_error(execution);
    let mut lines = vec![format!("agent: failure: {}", failure.message())];
    lines.extend(
        execution
            .response
            .raw_text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter(|line| !runtime_agent_failed_execution_raw_text_is_placeholder(execution, line))
            .take(200)
            .map(ToOwned::to_owned),
    );
    lines
}

/// Returns true when provider raw text is only an internal execution marker.
fn runtime_agent_failed_execution_raw_text_is_placeholder(
    execution: &AgentTurnExecution,
    line: &str,
) -> bool {
    line == "executing"
        && (execution.response.action_batch.is_some()
            || !execution.response.provider_transcript_events.is_empty())
}

impl RuntimeSessionService {
    /// Runs the present agent response actions to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn present_agent_response_actions_to_terminal_buffer(
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
        let batch_rationale_was_presented = !batch.rationale.trim().is_empty()
            && !runtime_agent_batch_rationale_repeats_visible_batch_text(
                batch,
                &visible_action_texts,
            );
        if batch_rationale_was_presented {
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
        if batch_rationale_was_presented {
            emitted_action_rationale_keys
                .insert(normalize_agent_user_visible_text(&batch.rationale));
        }
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
                | AgentActionPayload::ConfigChange { .. }
                | AgentActionPayload::MemorySearch { .. }
                | AgentActionPayload::MemoryStore { .. }
                | AgentActionPayload::IssueAdd { .. }
                | AgentActionPayload::IssueUpdate { .. }
                | AgentActionPayload::IssueQuery { .. }
                | AgentActionPayload::IssueDelete { .. } => {
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
    pub(crate) fn present_deferred_agent_say_actions_to_terminal_buffer(
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
    pub(crate) fn present_agent_action_outcomes_to_terminal_buffer(
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
    pub(crate) fn present_unrecovered_agent_failure_diagnostics_to_terminal_buffer(
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

#[cfg(test)]
mod tests {
    use super::runtime_agent_execution_prompt_display_lines;
    use mez_agent::{
        ActionResult, ActionStatus, AgentTurnExecution, AgentTurnState, MaapBatch, ModelResponse,
    };

    /// Verifies failed DeepSeek tool-call turns display the action failure
    /// diagnostic instead of the provider's `executing` placeholder.
    ///
    /// DeepSeek responses with only tool calls use `executing` as local
    /// fallback raw text. If a later action result fails, the prompt footer must
    /// show the failed action diagnostic so users can see why the turn stopped.
    #[test]
    fn failed_deepseek_execution_prompt_shows_action_error_not_executing_placeholder() {
        let execution = AgentTurnExecution {
            request: mez_agent::ModelRequest {
                provider: "deepseek".to_string(),
                model: "deepseek-v4-pro".to_string(),
                reasoning_effort: Some("high".to_string()),
                thinking_enabled: None,
                latency_preference: None,
                prompt_cache_retention: None,
                max_output_tokens: None,
                temperature: None,
                stop: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: "turn-2".to_string(),
                agent_id: "agent-1".to_string(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: true,
                interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
                allowed_actions: mez_agent::AllowedActionSet::action_execution_base(),
                messages: Vec::new(),
            },
            response: ModelResponse {
                provider: "deepseek".to_string(),
                model: "deepseek-v4-pro".to_string(),
                raw_text: "executing".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: Some(MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "inspect the target files".to_string(),
                    thought: None,
                    turn_id: "turn-2".to_string(),
                    agent_id: "agent-1".to_string(),
                    actions: Vec::new(),
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![ActionResult {
                protocol: "maap/1".to_string(),
                turn_id: "turn-2".to_string(),
                agent_id: "agent-1".to_string(),
                action_id: "a1".to_string(),
                action_type: "shell_command",
                status: ActionStatus::Failed,
                content: vec![mez_agent::ActionContentBlock::text("shell command failed")],
                structured_content_json: None,
                is_error: true,
                error: Some(mez_agent::ActionError {
                    code: "shell_failed".to_string(),
                    message: "command exited with status 1".to_string(),
                    data_json: None,
                }),
            }],
            final_turn: false,
            terminal_state: AgentTurnState::Failed,
        };

        let lines =
            runtime_agent_execution_prompt_display_lines("turn-2", "deepseek", &execution, 0, 5);

        assert!(lines.contains(&"agent: turn turn-2 failed".to_string()));
        assert!(lines.contains(&"agent: provider deepseek responded".to_string()));
        assert!(lines.contains(&"agent: recorded 5 transcript entries".to_string()));
        assert!(lines.contains(
            &"agent: failure: agent action shell_failed: command exited with status 1".to_string(),
        ));
        assert!(!lines.iter().any(|line| line == "executing"));
    }

    /// Verifies failed macro-judge completions display the structured runtime
    /// application error instead of the generic missing-MAAP diagnostic.
    ///
    /// Macro-judge provider responses are intentionally JSON-only and do not
    /// contain MAAP batches. When applying that JSON fails, the failed-turn
    /// prompt must show the embedded provider error so users see the judge
    /// validation problem that actually stopped the macro.
    #[test]
    fn failed_macro_judge_execution_prompt_shows_provider_error_not_missing_batch() {
        let execution = AgentTurnExecution {
            request: mez_agent::ModelRequest {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_effort: None,
                thinking_enabled: None,
                latency_preference: None,
                prompt_cache_retention: None,
                max_output_tokens: None,
                temperature: None,
                stop: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: false,
                interaction_kind: mez_agent::ModelInteractionKind::MacroJudge,
                allowed_actions: mez_agent::AllowedActionSet::for_capability(
                    mez_agent::AgentCapability::RespondOnly,
                ),
                messages: Vec::new(),
            },
            response: ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "{\"outcome\":\"finish_success\",\"step_success\":true,\"rationale\":\"done\",\"adapted_prompt\":null,\"user_message\":null}\nprovider_error: InvalidArgs: macro judge cannot finish before the final step"
                    .to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: Vec::new(),
            final_turn: true,
            terminal_state: AgentTurnState::Failed,
        };

        let lines = runtime_agent_execution_prompt_display_lines(
            "turn-1",
            "runtime-batch",
            &execution,
            0,
            3,
        );

        assert!(
            lines.contains(
                &"agent: failure: InvalidArgs: macro judge cannot finish before the final step"
                    .to_string(),
            )
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.contains("model response did not contain a MAAP action batch"))
        );
    }
}
