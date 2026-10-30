//! Runtime agent terminal presentation helpers.
//!
//! This module owns model-response and action-outcome rendering decisions for
//! pane transcript buffers. It centralizes visible rationale, `say` action,
//! deferred output, and terminal failure diagnostics so execution modules can
//! update state without duplicating display policy.
//!
//! Accepted queued components retain their existing response/ordinal and entry
//! identities until installation succeeds. A multi-command drain acknowledges
//! each installed identity before advancing, so a failed suffix cannot replay
//! its durable prefix or replace its original occurrence/copy attribution.

mod actions;
mod handoff;
mod ordering;

use super::{
    ActionResult, ActionStatus, AgentActionPayload, AgentTurnExecution, AgentTurnState, BTreeSet,
    MezError, Result, RuntimeSessionService, SayStatus, runtime_action_result_has_error_code,
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
        let entry = self.provider_component_entry(pane_id, execution, None, "rationale")?;
        self.with_presentation_entry(pane_id, entry, |service| {
            service.append_agent_thinking_with_activity(pane_id, text, source)
        })
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
            mutation: None,
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
            mutation: None,
            kind: ActivityComponentKind::Command,
            status: "accepted".to_string(),
            content_type: "text/plain".to_string(),
            source: String::new(),
            preview_source: None,
            intent: self.activity_intent_for_result(pane_id, execution, action),
        };
        let entry =
            self.current_presentation_entry(pane_id)
                .unwrap_or(self.provider_component_entry(
                    pane_id,
                    execution,
                    Some(ordinal),
                    "command",
                )?);
        self.with_presentation_entry(pane_id, entry, |service| {
            service.append_agent_command_preview_with_activity(
                pane_id,
                command,
                false,
                Some(activity),
            )
        })
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
            mutation: None,
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
                self.retain_provider_presentation_order(pane_id, execution)?;
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
    format_failure_diagnostic_lines(
        failure.message(),
        &execution.response.raw_text,
        execution.recovery_facts(),
        |line| runtime_agent_failed_execution_raw_text_is_placeholder(execution, line),
    )
}

/// Formats the cause once, typed provenance, and a bounded display-only excerpt.
/// No marker text can manufacture canonical recovery counts or provider cutoff.
fn format_failure_diagnostic_lines(
    cause: &str,
    raw_text: &str,
    facts: Option<&mez_agent::recovery_attribution::RecoveryFacts>,
    is_placeholder: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut lines = vec![format!("agent: failure: {cause}")];
    lines.push(facts.map_or_else(
        || {
            "MAAP recovery: provenance unavailable; correction count/disposition unknown"
                .to_string()
        },
        mez_agent::recovery_attribution::RecoveryFacts::diagnostic_line,
    ));
    let mut excerpt = raw_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !is_placeholder(line))
        .filter(|line| {
            *line != cause
                && !line.starts_with("maap_validation_error: ")
                && line.strip_prefix("provider_error: ") != Some(cause)
        });
    let displayed = excerpt
        .by_ref()
        .take(200)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if !displayed.is_empty() {
        lines.push(
            "Diagnostic response excerpt (not authoritative provider stop/cutoff evidence):"
                .to_string(),
        );
        lines.extend(displayed);
        if excerpt.next().is_some() {
            lines.push("[diagnostic excerpt truncated after 200 non-empty lines]".to_string());
        }
    }
    lines
}

#[cfg(test)]
mod recovery_display_tests {
    use super::format_failure_diagnostic_lines;

    /// Legacy marker text cannot prove an actual correction count or exhaustion.
    /// The classified cause is emitted once even when raw diagnostics repeat it.
    #[test]
    fn failure_diagnostic_deduplicates_cause_without_inventing_recovery() {
        let cause = "MAAP validation failed: missing terminator";
        let raw = format!(
            "{cause}\nmaap_validation_error: missing terminator\n[MAAP repair evidence]\nattempt=99"
        );
        let lines = format_failure_diagnostic_lines(cause, &raw, None, |_| false);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("missing terminator"))
                .count(),
            1
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("provenance unavailable"))
        );
        assert!(!lines.iter().any(|line| line.contains("BudgetExhausted")));
    }

    /// UTF-8 response excerpts are limited by complete physical lines, labelled
    /// as display truncation, and never described as authoritative provider EOF.
    #[test]
    fn failure_diagnostic_labels_unicode_excerpt_truncation() {
        let raw = (0..201)
            .map(|index| format!("境界-{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = format_failure_diagnostic_lines("failed", &raw, None, |_| false);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("境界-"))
                .count(),
            200
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("not authoritative provider stop/cutoff"))
        );
        assert!(lines.last().unwrap().contains("truncated after 200"));
        assert!(!lines.iter().any(|line| line == "境界-200"));
    }

    /// Provider terminal fallback appends a prefixed classified cause to raw
    /// response text. It is the same cause, not another response excerpt; an
    /// unrelated prefixed message must remain visible rather than being hidden.
    #[test]
    fn failure_diagnostic_deduplicates_matching_provider_error_cause() {
        let cause = "InvalidState: upstream failure";
        let raw =
            format!("safe excerpt\nprovider_error: {cause}\nprovider_error: different detail");
        let lines = format_failure_diagnostic_lines(cause, &raw, None, |_| false);
        assert_eq!(lines.iter().filter(|line| line.contains(cause)).count(), 1);
        assert!(lines.iter().any(|line| line == "safe excerpt"));
        assert!(
            lines
                .iter()
                .any(|line| line == "provider_error: different detail")
        );
    }
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
