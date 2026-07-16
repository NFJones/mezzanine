//! Macro prompt, request, recipient, and judge parsing helpers.

use super::super::*;
use super::*;

/// Returns the target agent id for a direct agent-recipient string.
pub(super) fn macro_message_recipient_agent_id(recipient: &str) -> Option<String> {
    recipient
        .strip_prefix("agent:")
        .filter(|agent_id| !agent_id.trim().is_empty())
        .map(|id| id.trim().to_owned())
        .or_else(|| {
            recipient
                .starts_with("agent-%")
                .then(|| recipient.to_string())
        })
}

/// Builds the parent model prompt that orchestrates one active macro run.
pub(super) fn runtime_macro_parent_orchestration_prompt(
    definition: &MacroDefinition,
    additional_context: Option<&str>,
    child_agent_id: &str,
) -> String {
    let mut lines = vec![
        format!("Agent macro invocation: #{}", definition.summary.name),
        format!("Description: {}", definition.summary.description),
        format!("Persistent subagent recipient: agent:{child_agent_id}"),
        "".to_string(),
        "Macro execution rules:".to_string(),
        "- Use the same persistent subagent recipient for every step.".to_string(),
        "- Step 1 has already been sent to the persistent subagent by the runtime; wait for that result before judging whether to continue.".to_string(),
        format!("- The runtime submits every later macro step to `agent:{child_agent_id}` after a valid structured judge decision."),
        "- Judge each completed step with one outcome: continue, continue_with_adapted_prompt, stop_failure, or finish_success.".to_string(),
        "- Each step is interpreted as a normal agent-shell prompt in the subagent, so slash commands such as /loop remain valid.".to_string(),
        "- You may adapt a scripted step to the user's stated intent, but preserve the macro purpose and step order.".to_string(),
        "- After each subagent result, judge success against the step intent, user context, and remaining sequence.".to_string(),
        "- On success, choose a continuation outcome; on failure, choose stop_failure with a concise explanation.".to_string(),
        "- Finish successfully only after all required steps complete in order.".to_string(),
        "".to_string(),
    ];
    if let Some(context) = additional_context.filter(|context| !context.trim().is_empty()) {
        lines.push("User additional context:".to_string());
        lines.push(context.trim().to_string());
        lines.push(String::new());
    }
    lines.push("Scripted steps:".to_string());
    lines.extend(
        definition
            .steps
            .iter()
            .map(|step| format!("{}. {}", step.index, step.prompt)),
    );
    lines.join("\n")
}

/// Builds the runtime-owned first macro-step prompt sent to the child agent.
pub(super) fn runtime_macro_initial_step_prompt(
    step_prompt: &str,
    additional_context: Option<&str>,
) -> String {
    let Some(context) = additional_context.filter(|context| !context.trim().is_empty()) else {
        return step_prompt.to_string();
    };
    format!(
        "{step_prompt}\n\nUser additional context for this macro invocation:\n{}",
        context.trim()
    )
}

/// Builds a synthetic request record for the runtime-owned macro first step.
pub(super) fn runtime_owned_macro_step_model_request(
    parent_turn: &AgentTurnRecord,
) -> ModelRequest {
    ModelRequest {
        provider: "runtime".to_string(),
        model: "macro-orchestration".to_string(),
        reasoning_effort: None,
        thinking_enabled: None,
        latency_preference: None,
        prompt_cache_retention: None,
        max_output_tokens: None,
        temperature: None,
        prompt_cache_session_id: None,
        prompt_cache_lineage_id: None,
        turn_id: parent_turn.turn_id.clone(),
        agent_id: parent_turn.agent_id.clone(),
        available_mcp_tools: Vec::new(),
        memory_actions_enabled: false,
        issue_actions_enabled: false,
        interaction_kind: ModelInteractionKind::ActionExecution,
        allowed_actions: AllowedActionSet::for_capability(mez_agent::AgentCapability::Subagent),
        stop: None,
        messages: vec![ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::TranscriptUser,
            content: "runtime-owned macro first step".to_string(),
        }],
    }
}

/// Returns the stable wire value for one macro judge outcome.
pub(super) fn macro_judge_outcome_wire_value(outcome: MacroJudgeOutcome) -> &'static str {
    match outcome {
        MacroJudgeOutcome::Continue => "continue",
        MacroJudgeOutcome::ContinueWithAdaptedPrompt => "continue_with_adapted_prompt",
        MacroJudgeOutcome::RetryCurrentStep => "retry_current_step",
        MacroJudgeOutcome::StopFailure => "stop_failure",
        MacroJudgeOutcome::FinishSuccess => "finish_success",
    }
}

/// Builds the system policy for a structured macro judge request.
pub(super) fn runtime_macro_judge_policy() -> String {
    [
        "You are judging one completed Mezzanine agent macro step.",
        "Return only JSON matching the requested macro-judge schema.",
        "Choose continue only when the completed step satisfied its intent and another scripted step remains.",
        "Choose continue_with_adapted_prompt only when another step remains and the next prompt needs bounded adaptation.",
        "Choose retry_current_step when the completed step looks incomplete but recoverable and the same scripted step should be retried, optionally with a bounded adapted prompt.",
        "Choose stop_failure when the completed step did not satisfy its intent or continuation would violate the macro purpose.",
        "Choose finish_success only after the final required step completed successfully.",
    ]
    .join("\n")
}

/// Builds the user task for a structured macro judge request.
pub(super) fn runtime_macro_judge_task(
    run: &MacroRunState,
    step: &MacroRunStep,
    result: &crate::runtime::service_state::MacroStepTaskResult,
    next_step: Option<&MacroRunStep>,
) -> String {
    let mut value = serde_json::json!({
        "macro_name": run.macro_name,
        "macro_description": run.macro_description,
        "invocation_prompt": run.invocation_prompt,
        "invocation_context": run.invocation_context,
        "completed_step": {
            "index": step.index,
            "scripted_prompt": step.scripted_prompt,
            "submitted_prompt": step.submitted_prompt,
            "child_turn_id": step.child_turn_id,
            "task_result": {
                "success": result.success,
                "summary": result.summary,
                "output": result.output,
            }
        },
        "prior_steps": run.steps.iter().filter(|candidate| candidate.index < step.index).map(|candidate| {
            serde_json::json!({
                "index": candidate.index,
                "scripted_prompt": candidate.scripted_prompt,
                "task_result": candidate.task_result.as_ref().map(|task_result| serde_json::json!({
                    "success": task_result.success,
                    "summary": task_result.summary,
                })),
                "judgment": candidate.judgment.as_ref().map(|judgment| serde_json::json!({
                    "outcome": macro_judge_outcome_wire_value(judgment.outcome),
                    "step_success": judgment.step_success,
                    "rationale": judgment.rationale,
                })),
            })
        }).collect::<Vec<_>>(),
        "next_step": next_step.map(|next_step| serde_json::json!({
            "index": next_step.index,
            "scripted_prompt": next_step.scripted_prompt,
        })),
    });
    value["instructions"] = serde_json::json!(
        "Judge whether the completed step satisfies the macro intent and select the next runtime action, including retry_current_step for incomplete but recoverable output."
    );
    value.to_string()
}

/// Parses and validates one structured macro judge response.
pub(super) fn macro_judge_decision_from_text(
    text: &str,
    step_count: usize,
    step_index: usize,
) -> Result<MacroJudgeDecision> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).map_err(|error| {
        MezError::invalid_args(format!(
            "macro judge response invalid after step {}: expected JSON object: {error}",
            step_index.saturating_add(1)
        ))
    })?;
    let object = value.as_object().ok_or_else(|| {
        MezError::invalid_args(format!(
            "macro judge response invalid after step {}: expected JSON object",
            step_index.saturating_add(1)
        ))
    })?;
    let outcome = object
        .get("outcome")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_args("macro judge response missing outcome"))?
        .parse::<MacroJudgeOutcome>()
        .map_err(MezError::invalid_args)?;
    let step_success = object
        .get("step_success")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| MezError::invalid_args("macro judge response missing step_success"))?;
    let rationale = object
        .get("rationale")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| MezError::invalid_args("macro judge response missing rationale"))?
        .to_string();
    let adapted_prompt = object
        .get("adapted_prompt")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let user_message = object
        .get("user_message")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let final_step = step_index.saturating_add(1) >= step_count;
    match outcome {
        MacroJudgeOutcome::Continue if final_step => {
            return Err(MezError::invalid_args(
                "macro judge cannot continue after the final step",
            ));
        }
        MacroJudgeOutcome::ContinueWithAdaptedPrompt if final_step => {
            return Err(MezError::invalid_args(
                "macro judge cannot adapt a next prompt after the final step",
            ));
        }
        MacroJudgeOutcome::ContinueWithAdaptedPrompt if adapted_prompt.is_none() => {
            return Err(MezError::invalid_args(
                "macro judge adapted continuation requires adapted_prompt",
            ));
        }
        MacroJudgeOutcome::StopFailure if user_message.is_none() => {
            return Err(MezError::invalid_args(
                "macro judge stop_failure requires user_message",
            ));
        }
        MacroJudgeOutcome::FinishSuccess if !final_step => {
            return Err(MezError::invalid_args(
                "macro judge cannot finish before the final step",
            ));
        }
        _ => {}
    }
    Ok(MacroJudgeDecision {
        outcome,
        step_success,
        rationale,
        adapted_prompt,
        user_message,
    })
}
