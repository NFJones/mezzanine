//! Runtime agent terminal presentation helpers.
//!
//! This module owns model-response and action-outcome rendering decisions for
//! pane transcript buffers. It centralizes visible rationale, `say` action,
//! deferred output, and terminal failure diagnostics so execution modules can
//! update state without duplicating display policy.

use super::{
    ActionStatus, AgentActionPayload, AgentTurnExecution, AgentTurnState, BTreeSet, Result,
    RuntimeSessionService, SayStatus, runtime_action_result_has_error_code,
    runtime_action_result_is_terminal_failure, runtime_agent_action_error_suffix,
    runtime_agent_action_has_runtime_visible_effect, runtime_agent_action_outcome_line,
    runtime_agent_action_summary, runtime_agent_batch_rationale_repeats_visible_batch_text,
    runtime_agent_batch_visible_action_texts, runtime_agent_execution_failure_error,
    runtime_agent_turn_state_name, runtime_loop_guard_failure_label,
    runtime_loop_guard_failure_summary_line, runtime_unrecovered_action_failure_output,
    runtime_unrecovered_failure_output_lines,
};

/// Provider-neutral input to the actor-owned MAAP log presenter. Progress is
/// optional and never authoritative; completion and static fallback are admitted
/// only after the provider execution has been validated by its owning caller.
pub(crate) enum RuntimeProviderLogInput<'a> {
    /// A source fragment or lifecycle barrier from the current provider claim.
    Progress(&'a mez_agent::StreamingSayEvent),
    /// Retire provisional source after the owning caller rejects or loses a response.
    DiscardProvisional,
    /// Retire ownership after interruption without rewinding already visible rows.
    Interrupted,
    /// Remove a still-provisional pane projection when the turn settles.
    Terminal,
    /// Reconcile installed provisional source with a validated execution.
    Validated(&'a AgentTurnExecution),
    /// Present any validated component not already promoted by reconciliation.
    Settled(&'a AgentTurnExecution),
}

/// Validated response-local log components in the same order as provider
/// progress: batch rationale precedes action ordinals. This is presentation
/// input only; execution results remain owned by their action workers.
enum RuntimeValidatedLogComponent<'a> {
    Rationale(&'a str),
    Action(usize, &'a AgentActionPayload),
}

/// Adapts a complete validated batch without inventing provisional deltas.
fn validated_log_components(batch: &mez_agent::MaapBatch) -> Vec<RuntimeValidatedLogComponent<'_>> {
    let mut components = Vec::with_capacity(batch.actions.len().saturating_add(1));
    components.push(RuntimeValidatedLogComponent::Rationale(&batch.rationale));
    components.extend(
        batch
            .actions
            .iter()
            .enumerate()
            .map(|(index, action)| RuntimeValidatedLogComponent::Action(index, &action.payload)),
    );
    components
}

/// Whether this action owns a log that must precede a later say component.
fn action_holds_later_log(action: &mez_agent::AgentAction) -> bool {
    runtime_agent_action_has_runtime_visible_effect(action)
        || matches!(
            action.payload,
            AgentActionPayload::ListAgents { .. }
                | AgentActionPayload::Wait
                | AgentActionPayload::CloseAgent { .. }
                | AgentActionPayload::IssueAdd { .. }
                | AgentActionPayload::IssueUpdate { .. }
                | AgentActionPayload::IssueQuery { .. }
                | AgentActionPayload::IssueDelete { .. }
        )
}

impl RuntimeSessionService {
    /// Captures response-wide rationale independently of action components.
    fn append_activity_rationale_for_execution(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        text: &str,
    ) -> Result<()> {
        let source = self.activity_rationale_source(pane_id, execution, text)?;
        self.append_agent_thinking_with_activity(pane_id, text, source)
    }

    /// Returns response-wide accepted identity for both static and promoted
    /// rationale. Absence of a conversation retains the existing no-op path.
    pub(in crate::runtime) fn activity_rationale_source(
        &self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        text: &str,
    ) -> Result<Option<crate::storage::transcript::activity::ActivitySource>> {
        use crate::storage::transcript::activity::{ActivityComponentKind, ActivitySource};
        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Ok(None);
        };
        let source = ActivitySource {
            version: 1,
            conversation_id: session.session_id.clone(),
            turn_id: execution.request.turn_id.clone(),
            response_id: super::provider_execution::provider_log_execution_group_id(execution)?
                .as_str()
                .to_string(),
            action_id: None,
            action_ordinal: None,
            transaction: None,
            kind: ActivityComponentKind::Rationale,
            status: "accepted".to_string(),
            content_type:
                "application/vnd.mezzanine.agent-presentation.thinking+text; charset=utf-8"
                    .to_string(),
            source: text.to_string(),
            preview_source: None,
            intent: Default::default(),
        };
        Ok(Some(source))
    }

    /// Builds an accepted action component from the validated batch ordinal.
    /// Missing ownership remains absent rather than inferred from display text.
    pub(in crate::runtime) fn activity_action_source(
        &self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        ordinal: usize,
        kind: crate::storage::transcript::activity::ActivityComponentKind,
        source: (&str, &str),
    ) -> Result<Option<crate::storage::transcript::activity::ActivitySource>> {
        let Some(action) = execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| batch.actions.get(ordinal))
        else {
            return Ok(None);
        };
        let Some(mut activity) = self.activity_rationale_source(pane_id, execution, source.0)?
        else {
            return Ok(None);
        };
        activity.action_id = Some(action.id.clone());
        activity.action_ordinal = Some(ordinal);
        activity.kind = kind;
        activity.content_type = source.1.to_string();
        activity.intent = self.activity_intent_for_result(pane_id, execution, action);
        Ok(Some(activity))
    }

    /// Captures accepted command intent without claiming executor evidence.
    fn append_activity_command_for_execution(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
        command: &str,
    ) -> Result<()> {
        use crate::storage::transcript::activity::{ActivityComponentKind, ActivitySource};
        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Ok(());
        };
        let Some(ordinal) = execution.response.action_batch.as_ref().and_then(|batch| {
            batch
                .actions
                .iter()
                .position(|candidate| candidate == action)
        }) else {
            return self.append_agent_command_preview_to_terminal_buffer(pane_id, command);
        };
        let activity = ActivitySource {
            version: 1,
            conversation_id: session.session_id.clone(),
            turn_id: execution.request.turn_id.clone(),
            response_id: super::provider_execution::provider_log_execution_group_id(execution)?
                .as_str()
                .to_string(),
            action_id: Some(action.id.clone()),
            action_ordinal: Some(ordinal),
            transaction: None,
            kind: ActivityComponentKind::Command,
            status: "accepted".to_string(),
            content_type: "text/plain".to_string(),
            source: String::new(),
            preview_source: None,
            intent: self.activity_intent_for_result(pane_id, execution, action),
        };
        self.append_agent_command_preview_with_activity(pane_id, command, false, Some(activity))
    }

    /// Binds a visible result preview to its accepted response and explicit
    /// executor transaction. Missing ownership retains the legacy projection.
    pub(crate) fn append_activity_result_for_execution(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
        result: &mez_agent::ActionResult,
        text: &str,
        transaction: Option<&str>,
    ) -> Result<()> {
        let Some(ordinal) = execution.response.action_batch.as_ref().and_then(|batch| {
            batch
                .actions
                .iter()
                .position(|candidate| candidate == action)
        }) else {
            return self
                .append_agent_action_result_text_to_terminal_buffer(pane_id, action, result, text);
        };
        if result.turn_id != execution.request.turn_id {
            return Err(crate::error::MezError::invalid_state(
                "activity result belongs to another execution turn",
            ));
        }
        let group = super::provider_execution::provider_log_execution_group_id(execution)?;
        self.append_ordered_activity_result(
            pane_id,
            (group.as_str(), ordinal, transaction),
            action,
            result,
            text,
            self.activity_intent_for_result(pane_id, execution, action),
        )
    }

    /// Captures accepted visible intent from typed batch/action fields only.
    fn activity_intent_for_result(
        &self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
    ) -> crate::storage::transcript::activity::ActivityIntent {
        use crate::storage::transcript::activity::ActivityIntent;
        let visible = self.agent_thinking_enabled(pane_id);
        let (summary, command) = match &action.payload {
            AgentActionPayload::ShellCommand {
                summary, command, ..
            } => (visible.then(|| summary.clone()), Some(command.clone())),
            _ => (None, None),
        };
        ActivityIntent {
            rationale: visible
                .then(|| {
                    execution
                        .response
                        .action_batch
                        .as_ref()
                        .map(|batch| batch.rationale.clone())
                })
                .flatten(),
            summary,
            command,
            header: crate::runtime::render::agent_action_execution_display_header(action),
        }
    }

    /// Preserves the existing visible outcome while binding disclosure to its
    /// exact response/action owner. No hidden tool output or tracing is added.
    fn append_ordered_activity_outcome(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
        result: &mez_agent::ActionResult,
        is_error: bool,
        line: &str,
    ) -> Result<()> {
        use crate::runtime::render::AgentTerminalPresentationStyle;
        use crate::storage::transcript::activity::{
            ACTIVITY_CONTENT_TYPE, ActivityComponentKind, ActivitySource,
        };
        if result.turn_id != execution.request.turn_id
            || result.action_id != action.id
            || result.action_type != action.action_type()
        {
            return Err(crate::error::MezError::invalid_state(
                "activity outcome differs from execution owner",
            ));
        }
        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Ok(());
        };
        let Some(ordinal) = execution.response.action_batch.as_ref().and_then(|batch| {
            batch
                .actions
                .iter()
                .position(|candidate| candidate == action)
        }) else {
            return Ok(());
        };
        let style = if is_error {
            AgentTerminalPresentationStyle::Error
        } else {
            AgentTerminalPresentationStyle::Status
        };
        let rows = vec![(style, line.to_string())];
        let source =
            serde_json::to_string(&vec![(style.persistence_name(), line)]).map_err(|error| {
                crate::error::MezError::invalid_args(format!(
                    "activity outcome encoding failed: {error}"
                ))
            })?;
        let activity = ActivitySource {
            version: 1,
            conversation_id: session.session_id.clone(),
            turn_id: result.turn_id.clone(),
            response_id: super::provider_execution::provider_log_execution_group_id(execution)?
                .as_str()
                .to_string(),
            action_id: Some(action.id.clone()),
            action_ordinal: Some(ordinal),
            transaction: None,
            kind: ActivityComponentKind::Outcome,
            status: format!("{:?}", result.status).to_ascii_lowercase(),
            content_type:
                "application/vnd.mezzanine.agent-presentation.styled-lines+json; charset=utf-8"
                    .to_string(),
            source,
            preview_source: None,
            intent: self.activity_intent_for_result(pane_id, execution, action),
        };
        match activity.encode() {
            Ok(encoded) => self.append_agent_terminal_styled_lines_with_source(
                pane_id,
                &rows,
                Some((&encoded, ACTIVITY_CONTENT_TYPE)),
            ),
            Err(_) => self.append_agent_terminal_styled_lines_to_buffer(pane_id, &rows),
        }
    }

    /// Routes accepted provider source through the existing presentation owner.
    pub(crate) fn ingest_provider_log(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        input: RuntimeProviderLogInput<'_>,
    ) -> Result<()> {
        match input {
            RuntimeProviderLogInput::Progress(event) => {
                self.apply_agent_streaming_say_event_to_terminal_buffer(pane_id, turn_id, event)
            }
            RuntimeProviderLogInput::DiscardProvisional => {
                self.discard_agent_streaming_say_presentations_for_turn(turn_id)?;
                Ok(())
            }
            RuntimeProviderLogInput::Interrupted => {
                self.finalize_agent_streaming_say_presentation(pane_id, Some(turn_id))?;
                Ok(())
            }
            RuntimeProviderLogInput::Terminal => {
                self.discard_agent_streaming_say_presentation(pane_id, Some(turn_id))?;
                Ok(())
            }
            RuntimeProviderLogInput::Validated(execution) => {
                self.reconcile_agent_streaming_say_completion_with_render_intent(
                    pane_id, turn_id, execution,
                )?;
                Ok(())
            }
            RuntimeProviderLogInput::Settled(execution) => {
                if turn_id != execution.request.turn_id {
                    return Ok(());
                }
                let Some(conversation_id) = self
                    .agent_shell_store()
                    .get(pane_id)
                    .map(|session| session.session_id.clone())
                else {
                    return Ok(());
                };
                let group = super::provider_execution::provider_log_execution_group_id(execution)?;
                let owner = (
                    pane_id.to_string(),
                    turn_id.to_string(),
                    conversation_id,
                    group,
                );
                if self
                    .presentation
                    .agent_settled_provider_log_groups
                    .contains(&owner)
                {
                    return Ok(());
                }
                self.present_agent_response_actions_to_terminal_buffer(pane_id, execution)?;
                self.presentation
                    .agent_settled_provider_log_groups
                    .insert(owner);
                Ok(())
            }
        }
    }

    /// Retires response replay fences after a terminal turn releases ownership.
    pub(crate) fn clear_settled_provider_log_groups_for_turn(&mut self, turn_id: &str) {
        self.presentation
            .agent_settled_provider_log_groups
            .retain(|(_, candidate_turn_id, _, _)| candidate_turn_id != turn_id);
        self.presentation
            .agent_deferred_provider_progress
            .retain(|(_, candidate_turn_id, _, _, _)| candidate_turn_id != turn_id);
        self.presentation
            .agent_retired_provider_says
            .retain(|(_, candidate_turn_id, _, _, _)| candidate_turn_id != turn_id);
        self.presentation
            .agent_queued_provider_outcomes
            .retain(|(_, candidate_turn_id, _, _, _), _| candidate_turn_id != turn_id);
        self.presentation
            .agent_published_provider_outcomes
            .retain(|((_, candidate_turn_id, _, _, _), _, _)| candidate_turn_id != turn_id);
        self.presentation
            .agent_queued_provider_headers
            .retain(|(_, candidate_turn_id, _, _, _), _| candidate_turn_id != turn_id);
        self.presentation
            .agent_queued_provider_commands
            .retain(|(_, candidate_turn_id, _, _, _), _| candidate_turn_id != turn_id);
        self.presentation
            .agent_published_provider_commands
            .retain(|((_, candidate_turn_id, _, _, _), _)| candidate_turn_id != turn_id);
        self.presentation
            .agent_queued_provider_results
            .retain(|(_, candidate_turn_id, _, _, _), _| candidate_turn_id != turn_id);
        self.presentation
            .agent_published_provider_results
            .retain(|((_, candidate_turn_id, _, _, _), _)| candidate_turn_id != turn_id);
        self.presentation
            .agent_provider_log_orders
            .retain(|(_, candidate_turn_id, _, _), _| candidate_turn_id != turn_id);
        self.presentation
            .agent_published_provider_headers
            .retain(|(_, candidate_turn_id, _, _, _)| candidate_turn_id != turn_id);
    }

    /// Captures an executor-approved header without publishing it ahead of earlier ordinals.
    pub(crate) fn queue_ordered_provider_header(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
    ) -> Result<bool> {
        let Some(header) = crate::runtime::render::agent_action_execution_display_header(action)
        else {
            return Ok(false);
        };
        self.queue_ordered_provider_header_with_text(pane_id, execution, action, header)?;
        Ok(true)
    }

    /// Queues an approved transaction-specific header at its validated action ordinal.
    pub(crate) fn queue_ordered_provider_header_with_text(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
        header: String,
    ) -> Result<()> {
        let Some(conversation_id) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|s| s.session_id.clone())
        else {
            return Ok(());
        };
        let group = super::provider_execution::provider_log_execution_group_id(execution)?;
        let Some(index) = execution.response.action_batch.as_ref().and_then(|batch| {
            batch
                .actions
                .iter()
                .position(|candidate| candidate.id == action.id)
        }) else {
            return Ok(());
        };
        let key = (
            pane_id.to_string(),
            execution.request.turn_id.clone(),
            conversation_id,
            group,
            index,
        );
        if self
            .presentation
            .agent_published_provider_headers
            .contains(&key)
        {
            return Ok(());
        }
        self.presentation
            .agent_queued_provider_headers
            .entry(key)
            .or_insert_with(|| (action.clone(), header));
        Ok(())
    }

    /// Queues a validated shell preview for publication at its action ordinal.
    pub(crate) fn queue_ordered_provider_command(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
        command: &str,
    ) -> Result<()> {
        let Some(conversation_id) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
        else {
            return Ok(());
        };
        let group = super::provider_execution::provider_log_execution_group_id(execution)?;
        let Some(index) = execution.response.action_batch.as_ref().and_then(|batch| {
            batch
                .actions
                .iter()
                .position(|candidate| candidate.id == action.id)
        }) else {
            return Ok(());
        };
        let key = (
            pane_id.to_string(),
            execution.request.turn_id.clone(),
            conversation_id,
            group,
            index,
        );
        let source_key = (key.clone(), command.to_string());
        if self
            .presentation
            .agent_published_provider_commands
            .contains(&source_key)
        {
            return Ok(());
        }
        if self
            .presentation
            .agent_published_provider_headers
            .contains(&key)
        {
            self.append_activity_command_for_execution(pane_id, execution, action, command)?;
            self.presentation
                .agent_published_provider_commands
                .insert(source_key);
            return Ok(());
        }
        self.presentation
            .agent_queued_provider_headers
            .entry(key.clone())
            .or_insert_with(|| (action.clone(), String::new()));
        self.presentation
            .agent_queued_provider_commands
            .entry(key)
            .or_insert_with(|| command.to_string());
        self.flush_ordered_provider_headers(pane_id, execution)
    }

    /// Queues a settled verbose result behind its accepted action header.
    pub(crate) fn queue_ordered_provider_result(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
        result: &mez_agent::ActionResult,
    ) -> Result<()> {
        if result.turn_id != execution.request.turn_id
            || result.action_id != action.id
            || result.action_type != action.action_type()
        {
            return Err(crate::error::MezError::invalid_state(
                "activity result differs from execution owner",
            ));
        }
        let Some(conversation_id) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
        else {
            return Ok(());
        };
        let group = super::provider_execution::provider_log_execution_group_id(execution)?;
        let Some(index) = execution.response.action_batch.as_ref().and_then(|batch| {
            batch
                .actions
                .iter()
                .position(|candidate| candidate.id == action.id)
        }) else {
            return Ok(());
        };
        let key = (
            pane_id.to_string(),
            execution.request.turn_id.clone(),
            conversation_id,
            group,
            index,
        );
        let text = result.content_text();
        let source_key = (key.clone(), text.clone());
        if self
            .presentation
            .agent_published_provider_results
            .contains(&source_key)
        {
            return Ok(());
        }
        if self
            .presentation
            .agent_published_provider_headers
            .contains(&key)
        {
            self.append_ordered_activity_result(
                pane_id,
                (key.3.as_str(), key.4, None),
                action,
                result,
                &text,
                self.activity_intent_for_result(pane_id, execution, action),
            )?;
            self.presentation
                .agent_published_provider_results
                .insert(source_key);
            return Ok(());
        }
        self.presentation
            .agent_queued_provider_results
            .entry(key)
            .or_insert_with(|| (action.clone(), result.clone(), text));
        Ok(())
    }

    /// Publishes ready executor headers in response order, without delaying execution.
    pub(crate) fn flush_ordered_provider_headers(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let Some(conversation_id) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|s| s.session_id.clone())
        else {
            return Ok(());
        };
        let group = super::provider_execution::provider_log_execution_group_id(execution)?;
        let owner = (
            pane_id.to_string(),
            execution.request.turn_id.clone(),
            conversation_id,
            group,
        );
        let Some(actions) = self
            .presentation
            .agent_provider_log_orders
            .get(&owner)
            .cloned()
        else {
            return Ok(());
        };
        self.present_deferred_agent_say_actions_to_terminal_buffer(pane_id, execution)?;
        for (index, action) in actions.iter().enumerate() {
            let key = (
                owner.0.clone(),
                owner.1.clone(),
                owner.2.clone(),
                owner.3.clone(),
                index,
            );
            if self
                .presentation
                .agent_queued_provider_headers
                .contains_key(&key)
                && !self
                    .presentation
                    .agent_published_provider_headers
                    .contains(&key)
            {
                if actions[..index]
                    .iter()
                    .enumerate()
                    .any(|(prior_index, prior)| {
                        let prior_key = (
                            owner.0.clone(),
                            owner.1.clone(),
                            owner.2.clone(),
                            owner.3.clone(),
                            prior_index,
                        );
                        if matches!(prior.payload, AgentActionPayload::Say { .. }) {
                            if self
                                .presentation
                                .agent_retired_provider_says
                                .contains(&prior_key)
                            {
                                return false;
                            }
                            // A failed, blocked, or interrupted response cannot
                            // promote this deferred say. Retire its ordering
                            // slot without presenting successful assistant text.
                            if !matches!(
                                execution.terminal_state,
                                AgentTurnState::Running | AgentTurnState::Completed
                            ) {
                                return false;
                            }
                            return !self.agent_streaming_say_action_is_promoted(
                                pane_id,
                                &execution.request.turn_id,
                                prior_index,
                            ) && !self
                                .presentation
                                .agent_deferred_provider_progress
                                .contains(&prior_key);
                        }
                        action_holds_later_log(prior)
                            && !self
                                .presentation
                                .agent_published_provider_headers
                                .contains(&prior_key)
                            && !execution.action_results.iter().any(|result| {
                                result.action_id == prior.id
                                    && result.is_terminal()
                                    && runtime_agent_action_outcome_line(
                                        prior,
                                        result,
                                        self.agent_verbose_enabled(pane_id)
                                            || self.agent_trace_enabled(pane_id),
                                    )
                                    .is_none_or(
                                        |(is_error, line)| {
                                            self.presentation
                                                .agent_published_provider_outcomes
                                                .contains(&(prior_key.clone(), is_error, line))
                                        },
                                    )
                            })
                    })
                {
                    break;
                }
                let Some((approved_action, header)) =
                    self.presentation.agent_queued_provider_headers.remove(&key)
                else {
                    continue;
                };
                if !header.is_empty() {
                    let activity = self.activity_action_source(
                        pane_id, execution, index,
                        crate::storage::transcript::activity::ActivityComponentKind::Header,
                        (&header, "application/vnd.mezzanine.agent-presentation.action-header+text; charset=utf-8"),
                    )?;
                    self.append_agent_action_header_with_activity(
                        pane_id,
                        &approved_action,
                        &header,
                        activity,
                    )?;
                }
                self.presentation
                    .agent_published_provider_headers
                    .insert(key.clone());
                if let Some(command) = self
                    .presentation
                    .agent_queued_provider_commands
                    .remove(&key)
                {
                    self.append_activity_command_for_execution(
                        pane_id, execution, action, &command,
                    )?;
                    self.presentation
                        .agent_published_provider_commands
                        .insert((key.clone(), command));
                }
                if let Some((action, result, text)) =
                    self.presentation.agent_queued_provider_results.remove(&key)
                {
                    self.append_ordered_activity_result(
                        pane_id,
                        (key.3.as_str(), key.4, None),
                        &action,
                        &result,
                        &text,
                        self.activity_intent_for_result(pane_id, execution, &action),
                    )?;
                    self.presentation
                        .agent_published_provider_results
                        .insert((key.clone(), text));
                }
                if let Some(outcomes) = self
                    .presentation
                    .agent_queued_provider_outcomes
                    .remove(&key)
                {
                    for (is_error, line) in outcomes {
                        if let Some(result) = execution
                            .action_results
                            .iter()
                            .find(|result| result.action_id == action.id)
                        {
                            self.append_ordered_activity_outcome(
                                pane_id, execution, action, result, is_error, &line,
                            )?;
                        } else if is_error {
                            self.append_agent_error_text_to_terminal_buffer(pane_id, &line)?;
                        } else {
                            self.append_agent_status_text_to_terminal_buffer(pane_id, &line)?;
                        }
                        self.presentation.agent_published_provider_outcomes.insert((
                            key.clone(),
                            is_error,
                            line,
                        ));
                    }
                }
                self.present_deferred_agent_say_actions_to_terminal_buffer(pane_id, execution)?;
            } else if action_holds_later_log(action)
                && !(matches!(action.payload, AgentActionPayload::ShellCommand { .. })
                    && self
                        .presentation
                        .agent_published_provider_headers
                        .contains(&key))
                && !execution.action_results.iter().any(|result| {
                    result.action_id == action.id
                        && result.is_terminal()
                        && runtime_agent_action_outcome_line(
                            action,
                            result,
                            self.agent_verbose_enabled(pane_id)
                                || self.agent_trace_enabled(pane_id),
                        )
                        .is_none_or(|(is_error, line)| {
                            self.presentation
                                .agent_published_provider_outcomes
                                .contains(&(key.clone(), is_error, line))
                        })
                })
            {
                break;
            }
        }
        self.present_deferred_agent_say_actions_to_terminal_buffer(pane_id, execution)?;
        Ok(())
    }
}

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
    let failed_before_provider_response = execution.terminal_state == AgentTurnState::Failed
        && execution
            .response
            .raw_text
            .trim_start()
            .starts_with("provider_error:");
    if failed_before_provider_response {
        lines.push(format!("agent: provider {provider_id} selected"));
    } else {
        lines.push(format!("agent: provider {provider_id} responded"));
    }
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
            let waiting_for_peer_message = execution.action_results.iter().any(|result| {
                result.action_type == "wait" && result.status == ActionStatus::Running
            });
            lines.push(if waiting_for_peer_message {
                "agent: waiting for MMP peer message".to_string()
            } else {
                "agent: waiting for pane, tool, or provider continuation".to_string()
            });
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

        if let Some(session) = self.agent_shell_store().get(pane_id) {
            let group = super::provider_execution::provider_log_execution_group_id(execution)?;
            self.presentation
                .agent_provider_log_orders
                .entry((
                    pane_id.to_string(),
                    execution.request.turn_id.clone(),
                    session.session_id.clone(),
                    group,
                ))
                .or_insert_with(|| batch.actions.clone());
        }

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
                        self.append_agent_assistant_content_to_terminal_buffer(
                            pane_id,
                            text,
                            content_type,
                        )?;
                        if let Some(session) = self.agent_shell_store().get(pane_id) {
                            let group = super::provider_execution::provider_log_execution_group_id(
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
            let group = super::provider_execution::provider_log_execution_group_id(execution)?;
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
        let group = super::provider_execution::provider_log_execution_group_id(execution)?;

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
                if self.presentation.agent_queued_provider_headers.keys().any(
                    |(queued_pane, queued_turn, queued_conversation, queued_group, ordinal)| {
                        queued_pane == pane_id
                            && queued_turn == &execution.request.turn_id
                            && owner.as_ref() == Some(queued_conversation)
                            && queued_group == &group
                            && *ordinal < action_index
                    },
                ) {
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
                self.append_agent_assistant_content_to_terminal_buffer(
                    pane_id,
                    text,
                    content_type,
                )?;
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
            let owner = self
                .agent_shell_store()
                .get(pane_id)
                .map(|session| session.session_id.clone());
            let group = super::provider_execution::provider_log_execution_group_id(execution)?;
            let index = batch
                .actions
                .iter()
                .position(|candidate| candidate.id == action.id);
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
                let outcome_key = (key.clone(), is_error, line.clone());
                if self
                    .presentation
                    .agent_published_provider_outcomes
                    .contains(&outcome_key)
                {
                    continue;
                }
                if self
                    .presentation
                    .agent_published_provider_headers
                    .contains(&key)
                {
                    self.append_ordered_activity_outcome(
                        pane_id, execution, action, result, is_error, &line,
                    )?;
                    self.presentation
                        .agent_published_provider_outcomes
                        .insert(outcome_key);
                    continue;
                }
                // A rejected action may have no accepted execution header. Its
                // result still occupies the ordinal without claiming execution.
                self.presentation
                    .agent_queued_provider_headers
                    .entry(key.clone())
                    .or_insert_with(|| (action.clone(), String::new()));
                let pending = self
                    .presentation
                    .agent_queued_provider_outcomes
                    .entry(key)
                    .or_default();
                if !pending
                    .iter()
                    .any(|entry| entry == &(is_error, line.clone()))
                {
                    pending.push((is_error, line));
                }
                self.flush_ordered_provider_headers(pane_id, execution)?;
            } else if is_error {
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
                model_capabilities: Default::default(),
                max_input_tokens: None,
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
                messages: Vec::new().into(),
            },
            response: ModelResponse {
                provider: "deepseek".to_string(),
                model: "deepseek-v4-pro".to_string(),
                raw_text: "executing".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: Some(MaapBatch {
                    rationale: "inspect the target files".to_string(),

                    actions: Vec::new(),
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
                permission_evaluation: None,
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
                model_capabilities: Default::default(),
                max_input_tokens: None,
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
                messages: Vec::new().into(),
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

    /// Verifies local provider preflight failures do not claim that the
    /// selected provider returned a response.
    ///
    /// Path-resolution and bootstrap certification can fail before network
    /// dispatch. Their synthetic failed execution begins with
    /// `provider_error:`, which must be presented as provider selection rather
    /// than a provider response.
    #[test]
    fn failed_preflight_execution_does_not_report_provider_response() {
        let execution = AgentTurnExecution {
            request: mez_agent::ModelRequest {
                provider: "openai".to_string(),
                model: "gpt-test".to_string(),
                model_capabilities: Default::default(),
                max_input_tokens: None,
                reasoning_effort: None,
                thinking_enabled: None,
                latency_preference: None,
                prompt_cache_retention: None,
                max_output_tokens: None,
                temperature: None,
                stop: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: "turn-3".to_string(),
                agent_id: "agent-%1".to_string(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: true,
                interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
                allowed_actions: mez_agent::AllowedActionSet::action_execution_base(),
                messages: Vec::new().into(),
            },
            response: ModelResponse {
                provider: "openai".to_string(),
                model: "gpt-test".to_string(),
                raw_text: "provider_error: InvalidState: pane agent-subshell bootstrap certification failed: foreground_process_group_changed"
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

        let lines =
            runtime_agent_execution_prompt_display_lines("turn-3", "openai", &execution, 0, 2);

        assert!(lines.contains(&"agent: provider openai selected".to_string()));
        assert!(
            lines
                .iter()
                .all(|line| line != "agent: provider openai responded")
        );
    }
}
