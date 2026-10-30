//! Accepted response speech, deferred actions and failure diagnostics.
//!
//! This module decides display eligibility from canonical results. It never
//! executes an action or turns failed/interrupted speech into success.

use super::*;

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

        self.retain_provider_presentation_order(pane_id, execution)?;

        let visible_action_texts = runtime_agent_batch_visible_action_texts(batch);
        let streamed_response_was_promoted =
            batch
                .actions
                .iter()
                .enumerate()
                .any(|(action_index, _action)| {
                    self.agent_streaming_say_action_is_promoted(
                        pane_id,
                        &execution.request.turn_id,
                        action_index,
                    )
                });
        let batch_rationale_was_presented = !batch.rationale.trim().is_empty()
            && !streamed_response_was_promoted
            && !self.agent_streaming_rationale_is_promoted(pane_id, &execution.request.turn_id)
            && !runtime_agent_batch_rationale_repeats_visible_batch_text(
                batch,
                &visible_action_texts,
            );
        let mut emitted_user_visible_action = false;
        let mut pending_runtime_visible_action = false;
        let has_runtime_visible_action = batch.actions.iter().any(action_holds_later_log);
        for component in validated_log_components(batch) {
            let (action_index, payload) = match component {
                RuntimeValidatedLogComponent::Rationale(text) => {
                    if batch_rationale_was_presented {
                        self.append_activity_rationale_for_execution(
                            pane_id,
                            execution,
                            text.trim(),
                        )?;
                    }
                    continue;
                }
                RuntimeValidatedLogComponent::Action(index, payload) => (index, payload),
            };
            match payload {
                AgentActionPayload::Say {
                    status,
                    text,
                    content_type,
                } => {
                    if self.agent_streaming_say_action_is_promoted(
                        pane_id,
                        &execution.request.turn_id,
                        action_index,
                    ) {
                        emitted_user_visible_action = true;
                        continue;
                    }
                    if text.trim().is_empty() {
                        continue;
                    }
                    if pending_runtime_visible_action
                        || (has_runtime_visible_action && *status != SayStatus::Progress)
                    {
                        pending_runtime_visible_action = true;
                    } else {
                        emitted_user_visible_action = true;
                        let entry = self.provider_component_entry(
                            pane_id,
                            execution,
                            Some(action_index),
                            "say",
                        )?;
                        self.with_presentation_entry(pane_id, entry, |service| {
                            service.append_agent_assistant_content_to_terminal_buffer(
                                pane_id,
                                text,
                                content_type,
                            )
                        })?;
                        if let Some(session) = self.agent_shell_store().get(pane_id) {
                            let group =
                                super::super::provider_execution::provider_log_execution_group_id(
                                    execution,
                                )?;
                            self.presentation.agent_deferred_provider_progress.insert((
                                pane_id.to_string(),
                                execution.request.turn_id.clone(),
                                session.session_id.clone(),
                                group,
                                action_index,
                            ));
                        }
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
                AgentActionPayload::McpServerSearch { .. }
                | AgentActionPayload::McpServerGet { .. }
                | AgentActionPayload::McpCall { .. }
                | AgentActionPayload::SendMessage { .. }
                | AgentActionPayload::Wait
                | AgentActionPayload::SpawnAgent { .. }
                | AgentActionPayload::CloseAgent { .. }
                | AgentActionPayload::ConfigChange { .. }
                | AgentActionPayload::MemorySearch { .. }
                | AgentActionPayload::MemoryStore { .. }
                | AgentActionPayload::ListAgents { .. }
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
        if !has_runtime_visible_action {
            self.clear_promoted_agent_streaming_say_actions(pane_id, &execution.request.turn_id);
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
        if matches!(
            execution.terminal_state,
            AgentTurnState::Failed | AgentTurnState::Interrupted
        ) && let (Some(batch), Some(conversation_id)) = (
            execution.response.action_batch.as_ref(),
            self.agent_shell_store()
                .get(pane_id)
                .map(|session| session.session_id.clone()),
        ) {
            let group =
                super::super::provider_execution::provider_log_execution_group_id(execution)?;
            for (index, action) in batch.actions.iter().enumerate() {
                if matches!(action.payload, AgentActionPayload::Say { .. })
                    && !self.agent_streaming_say_action_is_promoted(
                        pane_id,
                        &execution.request.turn_id,
                        index,
                    )
                {
                    self.presentation.agent_retired_provider_says.insert((
                        pane_id.to_string(),
                        execution.request.turn_id.clone(),
                        conversation_id.clone(),
                        group.clone(),
                        index,
                    ));
                }
            }
        }
        if execution.terminal_state != AgentTurnState::Running {
            self.settle_pending_final_say_preview(
                pane_id,
                &execution.request.turn_id,
                execution.terminal_state == AgentTurnState::Completed,
            )?;
        }
        if !matches!(
            execution.terminal_state,
            AgentTurnState::Completed | AgentTurnState::Running
        ) {
            return Ok(0);
        }
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return Ok(0);
        };
        if !batch.actions.iter().any(action_holds_later_log) {
            return Ok(0);
        }

        let owner = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone());
        let group = super::super::provider_execution::provider_log_execution_group_id(execution)?;

        let mut emitted = 0usize;
        for (action_index, action) in batch.actions.iter().enumerate() {
            if let AgentActionPayload::Say {
                status,
                text,
                content_type,
            } = &action.payload
            {
                let publication_key = owner.as_ref().map(|conversation_id| {
                    (
                        pane_id.to_string(),
                        execution.request.turn_id.clone(),
                        conversation_id.clone(),
                        group.clone(),
                        action_index,
                    )
                });
                if publication_key
                    .as_ref()
                    .is_some_and(|key| self.presentation.agent_retired_provider_says.contains(key))
                {
                    continue;
                }
                if self.agent_streaming_say_action_is_promoted(
                    pane_id,
                    &execution.request.turn_id,
                    action_index,
                ) {
                    if let Some(key) = publication_key {
                        self.presentation
                            .agent_deferred_provider_progress
                            .insert(key);
                    }
                    continue;
                }
                if text.trim().is_empty() {
                    continue;
                }
                if execution.terminal_state == AgentTurnState::Running
                    && (execution
                        .action_results
                        .iter()
                        .any(|result| result.is_error)
                        || *status != SayStatus::Progress
                        || !batch.actions[..action_index].iter().all(|prior| {
                            execution.action_results.iter().any(|result| {
                                result.action_id == prior.id
                                    && result.status == ActionStatus::Succeeded
                            })
                        }))
                {
                    continue;
                }
                if self
                    .presentation
                    .agent_queued_provider_headers
                    .keys()
                    .chain(self.presentation.agent_queued_provider_commands.keys())
                    .chain(self.presentation.agent_queued_provider_results.keys())
                    .chain(self.presentation.agent_queued_provider_outcomes.keys())
                    .any(
                        |(queued_pane, queued_turn, queued_conversation, queued_group, ordinal)| {
                            queued_pane == pane_id
                                && queued_turn == &execution.request.turn_id
                                && owner.as_ref() == Some(queued_conversation)
                                && queued_group == &group
                                && *ordinal < action_index
                        },
                    )
                {
                    continue;
                }
                if *status == SayStatus::Progress
                    && !batch.actions[..action_index]
                        .iter()
                        .any(action_holds_later_log)
                {
                    continue;
                }
                if publication_key.as_ref().is_some_and(|key| {
                    self.presentation
                        .agent_deferred_provider_progress
                        .contains(key)
                }) {
                    continue;
                }
                let entry =
                    self.provider_component_entry(pane_id, execution, Some(action_index), "say")?;
                self.with_presentation_entry(pane_id, entry, |service| {
                    service.append_agent_assistant_content_to_terminal_buffer(
                        pane_id,
                        text,
                        content_type,
                    )
                })?;
                if let Some(key) = publication_key {
                    self.presentation
                        .agent_deferred_provider_progress
                        .insert(key);
                }
                emitted = emitted.saturating_add(1);
            }
        }
        self.clear_promoted_agent_streaming_say_actions(pane_id, &execution.request.turn_id);
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
        if !matches!(
            execution.terminal_state,
            AgentTurnState::Running | AgentTurnState::Completed
        ) {
            // Record suppressed say ordinals before an outcome can take the
            // already-published-header path and bypass the queued log drain.
            self.present_deferred_agent_say_actions_to_terminal_buffer(pane_id, execution)?;
        }
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
            (
                "network_action_no_progress",
                runtime_loop_guard_failure_label("network_action_no_progress")
                    .unwrap_or("network search progress"),
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
            let Some(representative) = matching_results.first().copied() else {
                continue;
            };
            let Some(action) = batch
                .actions
                .iter()
                .find(|action| action.id == representative.action_id)
            else {
                return Err(MezError::invalid_state(
                    "aggregate outcome has no original action",
                ));
            };
            let line =
                runtime_loop_guard_failure_summary_line(label, matching_results.len(), message);
            if self.completion_owns_transcript_policy(&execution.request.turn_id) {
                self.publish_ordered_runtime_outcome(
                    pane_id,
                    execution,
                    action,
                    representative,
                    (true, line),
                )?;
            } else {
                // Legacy/unowned presentation still needs execution-wide
                // acknowledgment; raw fallback appends have no ordinal owner.
                self.publish_agent_aggregate_outcome_once(pane_id, execution, &line, &line)?;
            }
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
            self.publish_ordered_runtime_outcome(
                pane_id,
                execution,
                action,
                result,
                (is_error, line),
            )?;
        }
        Ok(())
    }

    /// Both aggregate and per-action outcomes retain exact source/result/entry
    /// before immediate append and share the same installed-source ack. This is
    /// the existing ordered owner, not an outcome replay queue or execution grant.
    fn publish_ordered_runtime_outcome(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
        result: &ActionResult,
        source: (bool, String),
    ) -> Result<()> {
        let (is_error, line) = source;
        let completion_owned = self.completion_owns_transcript_policy(&execution.request.turn_id);
        if completion_owned
            && !self.completion_settlement_binding_current(&execution.request.turn_id)
        {
            return Err(MezError::conflict("outcome publication binding is retired"));
        }
        let owner = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone());
        let group = super::super::provider_execution::provider_log_execution_group_id(execution)?;
        let index = execution.response.action_batch.as_ref().and_then(|batch| {
            batch
                .actions
                .iter()
                .position(|candidate| candidate.id == action.id)
        });
        if let (Some(conversation_id), Some(index)) = (owner, index)
            && self.presentation.agent_provider_log_orders.contains_key(&(
                pane_id.to_string(),
                execution.request.turn_id.clone(),
                conversation_id.clone(),
                group.clone(),
            ))
        {
            let key = (
                pane_id.to_string(),
                execution.request.turn_id.clone(),
                conversation_id,
                group,
                index,
            );
            if self
                .presentation
                .agent_published_provider_outcomes
                .contains(&(key.clone(), is_error, line.clone()))
            {
                return Ok(());
            }
            if !self
                .presentation
                .agent_published_provider_headers
                .contains(&key)
            {
                self.presentation
                    .agent_queued_provider_headers
                    .entry(key.clone())
                    .or_insert_with(|| (action.clone(), String::new(), None));
            }
            let already_queued = self
                .presentation
                .agent_queued_provider_outcomes
                .get(&key)
                .is_some_and(|pending| {
                    pending
                        .iter()
                        .any(|entry| entry.0 == is_error && entry.1 == line)
                });
            let entry = (!already_queued).then(|| {
                self.presentation_entry(
                    pane_id,
                    mez_core::presentation::PresentationOccurrenceBasis::LocalAcceptance,
                )
            });
            if let Some(entry) = entry {
                self.presentation
                    .agent_queued_provider_outcomes
                    .entry(key)
                    .or_default()
                    .push((is_error, line, entry, result.clone()));
            }
            return self.flush_ordered_provider_headers(pane_id, execution);
        }
        if completion_owned {
            let error =
                MezError::invalid_state("outcome publication has no original ordered owner");
            self.record_completion_required_failure(
                &execution.request.turn_id,
                "outcome_publication_owner",
                None,
                None,
                &error,
            );
            return Err(error);
        }
        if is_error {
            self.append_agent_error_text_to_terminal_buffer(pane_id, &line)
        } else {
            self.append_agent_status_text_to_terminal_buffer(pane_id, &line)
        }
    }

    /// Publishes one execution-wide aggregate summary at most once per
    /// validated response.
    ///
    /// Loop-guard summaries describe the whole execution rather than one
    /// action, so they bypass the per-action published-outcome keys. Several
    /// settlement families present the same execution as siblings finish, so a
    /// shared aggregate key keeps the summary exactly once regardless of which
    /// family settles next.
    fn publish_agent_aggregate_outcome_once(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        dedupe_key: &str,
        line: &str,
    ) -> Result<()> {
        let owner = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone());
        let group =
            super::super::provider_execution::provider_log_execution_group_id(execution).ok();
        let key = match (owner, group) {
            (Some(conversation_id), Some(group)) => Some((
                pane_id.to_string(),
                execution.request.turn_id.clone(),
                conversation_id,
                group,
                usize::MAX,
            )),
            _ => None,
        };
        let Some(key) = key else {
            return self.append_agent_error_text_to_terminal_buffer(pane_id, line);
        };
        let acknowledged = (key, true, dedupe_key.to_string());
        if self
            .presentation
            .agent_published_provider_outcomes
            .contains(&acknowledged)
        {
            return Ok(());
        }
        // Acknowledge only after the row actually installs: a pre-install
        // presentation failure must leave the summary publishable again instead
        // of permanently suppressing a warning that was never shown.
        self.append_agent_error_text_to_terminal_buffer(pane_id, line)?;
        self.presentation
            .agent_published_provider_outcomes
            .insert(acknowledged);
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
            (
                "network_action_no_progress",
                runtime_loop_guard_failure_label("network_action_no_progress")
                    .unwrap_or("network search progress"),
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
            let summary =
                runtime_loop_guard_failure_summary_line(label, matching_results.len(), message);
            self.publish_agent_aggregate_outcome_once(
                pane_id,
                execution,
                &summary,
                &format!("{summary}; {reason}"),
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
            let show_failure_output =
                !matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
                    || self
                        .agent_shell_store()
                        .get(pane_id)
                        .map(|session| session.log_level != mez_agent::AgentLogLevel::Normal)
                        .unwrap_or(false);
            if show_failure_output
                && let Some(output) = runtime_unrecovered_action_failure_output(result)
            {
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
