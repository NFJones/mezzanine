//! Provider-independent agent macro contracts and parsing.
//!
//! This module owns macro identities, deterministic catalog precedence,
//! model-facing catalog projection, `MACRO.md` parsing, ordered prompt-step
//! validation, and explicit `#macro` invocation syntax. Filesystem discovery,
//! embedded product assets, and product error projection remain in the
//! composition crate.

use serde::Deserialize;
use std::fmt;
use std::path::PathBuf;

use crate::{
    AgentCapability, AgentTurnRecord, AllowedActionSet, ContextSourceKind, ModelInteractionKind,
    ModelMessage, ModelMessageRole, ModelRequest,
};

/// File name that carries one macro's metadata and ordered prompt steps.
pub const MACRO_FILE_NAME: &str = "MACRO.md";
/// Markdown heading that starts the required ordered prompt-step section.
pub const MACRO_STEPS_HEADING: &str = "## Steps";
/// Maximum accepted `MACRO.md` size in bytes.
pub const MAX_MACRO_FILE_BYTES: u64 = 256 * 1024;
/// Maximum accepted prompt steps in one macro definition.
pub const MAX_MACRO_STEPS: usize = 128;

/// Provider-independent macro contract failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroContractError {
    message: String,
}

impl MacroContractError {
    /// Creates a macro contract failure with a stable diagnostic.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the human-readable contract diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for MacroContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MacroContractError {}

/// Source scope for one effective macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroSource {
    /// Macro shipped with Mezzanine.
    Builtin,
    /// Macro from the primary user configuration directory.
    User,
    /// Macro from a trusted project configuration directory.
    Project,
}

impl MacroSource {
    /// Returns the stable model-facing scope name for this source.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User => "user",
            Self::Project => "project",
        }
    }

    fn precedence(self) -> u8 {
        match self {
            Self::Builtin => 0,
            Self::User => 1,
            Self::Project => 2,
        }
    }
}

/// Catalog metadata for one available macro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroSummary {
    /// Macro identifier from `MACRO.md` front matter.
    pub name: String,
    /// Short usage description from `MACRO.md` front matter.
    pub description: String,
    /// Effective source scope for this macro.
    pub source: MacroSource,
    /// Path supplied by the product discovery adapter.
    pub path: PathBuf,
    /// Number of parsed prompt steps.
    pub step_count: usize,
}

impl MacroSummary {
    /// Returns a human-facing source label.
    pub fn attribution_label(&self) -> String {
        self.source.as_str().to_string()
    }
}

/// Non-fatal discovery diagnostic for an invalid or unreadable macro path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroDiagnostic {
    /// Path whose macro metadata could not be used.
    pub path: PathBuf,
    /// Human-readable reason the path was skipped.
    pub message: String,
}

/// Effective macro catalog for one pane/project context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacroCatalog {
    /// Deterministically ordered effective macros.
    pub macros: Vec<MacroSummary>,
    /// Non-fatal discovery diagnostics.
    pub diagnostics: Vec<MacroDiagnostic>,
}

impl MacroCatalog {
    /// Returns a macro summary by exact name.
    pub fn get(&self, name: &str) -> Option<&MacroSummary> {
        self.macros.iter().find(|summary| summary.name == name)
    }

    /// Returns macro names in deterministic catalog order.
    pub fn names(&self) -> Vec<String> {
        self.macros
            .iter()
            .map(|summary| summary.name.clone())
            .collect()
    }

    /// Inserts one discovered summary using deterministic source precedence.
    pub fn insert(&mut self, summary: MacroSummary) {
        if let Some(index) = self
            .macros
            .iter()
            .position(|existing| existing.name == summary.name)
        {
            let existing = &self.macros[index];
            let replace = summary.source.precedence() > existing.source.precedence();
            self.diagnostics.push(MacroDiagnostic {
                path: summary.path.clone(),
                message: if replace {
                    format!(
                        "macro name {:?} from {} overrides existing {} entry",
                        summary.name,
                        summary.attribution_label(),
                        existing.attribution_label()
                    )
                } else {
                    format!(
                        "macro name {:?} from {} ignored because existing {} entry has precedence",
                        summary.name,
                        summary.attribution_label(),
                        existing.attribution_label()
                    )
                },
            });
            if !replace {
                return;
            }
            self.macros[index] = summary;
        } else {
            self.macros.push(summary);
        }
        self.macros
            .sort_by(|left, right| left.name.cmp(&right.name));
    }

    /// Builds compact model-facing catalog text.
    pub fn model_catalog_text(&self) -> String {
        let mut lines = Vec::new();
        if self.macros.is_empty() {
            lines.push("No macros are currently available.".to_string());
        } else {
            lines.push("Available macros:".to_string());
            lines.extend(self.macros.iter().map(|summary| {
                format!(
                    "- {} ({}, {} steps) - {}",
                    summary.name,
                    summary.source.as_str(),
                    summary.step_count,
                    summary.description
                )
            }));
        }
        if !self.diagnostics.is_empty() {
            lines.push(String::new());
            lines.push("Skipped macro diagnostics:".to_string());
            lines.extend(self.diagnostics.iter().map(|diagnostic| {
                format!("- {} - {}", diagnostic.path.display(), diagnostic.message)
            }));
        }
        lines.join("\n")
    }

    /// Builds structured JSON for action-result metadata.
    pub fn structured_json(&self) -> String {
        serde_json::json!({
            "macros": self.macros.iter().map(|summary| {
                serde_json::json!({
                    "name": summary.name,
                    "description": summary.description,
                    "source": summary.source.as_str(),
                    "path": summary.path.to_string_lossy(),
                    "step_count": summary.step_count,
                })
            }).collect::<Vec<_>>(),
            "diagnostics": self.diagnostics.iter().map(|diagnostic| {
                serde_json::json!({
                    "path": diagnostic.path.to_string_lossy(),
                    "message": diagnostic.message,
                })
            }).collect::<Vec<_>>(),
        })
        .to_string()
    }
}

/// Full macro document loaded for an explicit invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroDefinition {
    /// Catalog metadata for the loaded macro.
    pub summary: MacroSummary,
    /// Complete raw `MACRO.md` text.
    pub text: String,
    /// Parsed ordered prompt steps.
    pub steps: Vec<MacroStep>,
}

/// One parsed macro prompt step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroStep {
    /// One-based step index from the ordered list.
    pub index: usize,
    /// Prompt text submitted to the macro subagent.
    pub prompt: String,
}

/// Parsed explicit `#<macro-name>` agent prompt invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroPromptInvocation {
    /// Macro name after the leading `#`.
    pub name: String,
    /// Optional trailing user-stated invocation context.
    pub additional_context: Option<String>,
}

/// Parsed dependency-neutral `MACRO.md` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMacroDocument {
    /// Validated front-matter macro name.
    pub name: String,
    /// Trimmed non-empty front-matter description.
    pub description: String,
    /// Validated ordered prompt steps.
    pub steps: Vec<MacroStep>,
}

/// Tracks one macro-managed persistent child and the parent run that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroManagedSubagent {
    /// Parent macro orchestration turn allowed to send step prompts to this child.
    pub parent_turn_id: String,
    /// Parent agent that owns the macro run.
    pub parent_agent_id: String,
    /// Macro name used for diagnostics and traceability.
    pub macro_name: String,
}

/// Describes the current harness-owned phase for one active macro run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacroRunPhase {
    /// The harness is preparing or retrying one step submission.
    DispatchingStep {
        /// Zero-based index of the step being dispatched.
        step_index: usize,
    },
    /// The harness is waiting for the submitted child turn to finish.
    WaitingForStep {
        /// Zero-based index of the submitted step.
        step_index: usize,
        /// Child turn currently executing the step prompt.
        child_turn_id: String,
    },
    /// The harness is asking the parent model to judge a completed step.
    WaitingForJudge {
        /// Zero-based index of the step being judged.
        step_index: usize,
    },
}

/// Stores the terminal task result for one completed macro child step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroStepTaskResult {
    /// Whether the child step completed successfully.
    pub success: bool,
    /// Child task summary supplied through the subagent task result.
    pub summary: String,
    /// Child task output supplied through the subagent task result.
    pub output: String,
}

/// Runtime-validated outcome returned by the macro judge model request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroJudgeOutcome {
    /// Continue with the next scripted prompt unchanged.
    Continue,
    /// Continue with a validated adapted prompt for the next step.
    ContinueWithAdaptedPrompt,
    /// Retry the current step, optionally with a bounded adapted prompt.
    RetryCurrentStep,
    /// Stop the macro as failed with a user-visible explanation.
    StopFailure,
    /// Complete the macro successfully after the final required step.
    FinishSuccess,
}

impl MacroJudgeOutcome {
    /// Returns the stable wire value used in judge requests and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::ContinueWithAdaptedPrompt => "continue_with_adapted_prompt",
            Self::RetryCurrentStep => "retry_current_step",
            Self::StopFailure => "stop_failure",
            Self::FinishSuccess => "finish_success",
        }
    }
}

impl std::str::FromStr for MacroJudgeOutcome {
    type Err = MacroContractError;

    /// Parses the stable wire value returned by a structured macro judge.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "continue" => Ok(Self::Continue),
            "continue_with_adapted_prompt" => Ok(Self::ContinueWithAdaptedPrompt),
            "retry_current_step" => Ok(Self::RetryCurrentStep),
            "stop_failure" => Ok(Self::StopFailure),
            "finish_success" => Ok(Self::FinishSuccess),
            _ => Err(MacroContractError::new("unsupported macro judge outcome")),
        }
    }
}

/// Stores one validated macro judge decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroJudgeDecision {
    /// Outcome selected by the judge.
    pub outcome: MacroJudgeOutcome,
    /// Whether the judge accepted the completed step as successful.
    pub step_success: bool,
    /// Short model-supplied rationale retained for diagnostics.
    pub rationale: String,
    /// Optional adapted prompt used by an adapted continuation or retry.
    pub adapted_prompt: Option<String>,
    /// Optional user-visible failure message for `StopFailure`.
    pub user_message: Option<String>,
}

/// Stores one scripted macro step and its harness submission metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroRunStep {
    /// Zero-based index copied from the loaded macro definition.
    pub index: usize,
    /// Number of times this step has been submitted to the persistent worker.
    pub attempts: usize,
    /// Scripted prompt copied at run start so in-flight runs are stable.
    pub scripted_prompt: String,
    /// Prompt text submitted to the child, including invocation context.
    pub submitted_prompt: Option<String>,
    /// Child turn created for the submitted step, when one exists.
    pub child_turn_id: Option<String>,
    /// Terminal task result returned by the child step.
    pub task_result: Option<MacroStepTaskResult>,
    /// Harness-validated judge decision for the completed step.
    pub judgment: Option<MacroJudgeDecision>,
}

/// Tracks explicit provider-independent state for a harness-owned macro run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroRunState {
    /// Stable macro run identifier; currently equal to the parent turn id.
    pub run_id: String,
    /// Parent macro orchestration turn id.
    pub parent_turn_id: String,
    /// Parent agent that owns the macro run.
    pub parent_agent_id: String,
    /// Parent pane where the macro was invoked.
    pub parent_pane_id: String,
    /// Persistent child agent used for all macro steps.
    pub child_agent_id: String,
    /// Macro name copied from the loaded definition.
    pub macro_name: String,
    /// Macro description copied from the loaded definition.
    pub macro_description: String,
    /// Original user prompt that invoked the macro.
    pub invocation_prompt: String,
    /// User-supplied context after the macro token, if any.
    pub invocation_context: Option<String>,
    /// Ordered steps copied from the loaded definition at run start.
    pub steps: Vec<MacroRunStep>,
    /// Zero-based index of the current step.
    pub current_step: usize,
    /// Current harness-owned macro run phase.
    pub phase: MacroRunPhase,
}

#[derive(Debug, Deserialize)]
struct MacroFrontMatter {
    name: String,
    description: String,
}

/// Parses and validates one complete `MACRO.md` document.
pub fn parse_macro_document(text: &str) -> Result<ParsedMacroDocument, MacroContractError> {
    let (front_matter, body) = split_macro_front_matter(text)?;
    let front_matter: MacroFrontMatter = serde_norway::from_str(front_matter).map_err(|error| {
        MacroContractError::new(format!("failed to parse MACRO.md front matter: {error}"))
    })?;
    if !is_valid_macro_name(&front_matter.name) {
        return Err(MacroContractError::new(format!(
            "macro name {:?} is invalid",
            front_matter.name
        )));
    }
    if front_matter.description.trim().is_empty() {
        return Err(MacroContractError::new(
            "macro description must not be empty",
        ));
    }
    Ok(ParsedMacroDocument {
        name: front_matter.name,
        description: front_matter.description.trim().to_string(),
        steps: parse_macro_steps(body)?,
    })
}

/// Parses explicit `#<macro-name>` agent prompt syntax.
pub fn parse_macro_prompt_invocation(input: &str) -> Option<MacroPromptInvocation> {
    let trimmed = input.trim_start();
    let remainder = trimmed.strip_prefix('#')?;
    let name_end = remainder
        .char_indices()
        .find_map(|(index, character)| character.is_whitespace().then_some(index))
        .unwrap_or(remainder.len());
    let name = remainder[..name_end].trim();
    if name.is_empty() {
        return None;
    }
    let argument = remainder[name_end..].trim();
    Some(MacroPromptInvocation {
        name: name.to_string(),
        additional_context: (!argument.is_empty()).then(|| argument.to_string()),
    })
}

/// Returns the target agent id for a direct agent-recipient string.
pub fn macro_message_recipient_agent_id(recipient: &str) -> Option<String> {
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

/// Builds the parent model prompt that describes one active macro run.
pub fn macro_parent_orchestration_prompt(
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

/// Builds the harness-owned first macro-step prompt sent to the child agent.
pub fn macro_initial_step_prompt(step_prompt: &str, additional_context: Option<&str>) -> String {
    let Some(context) = additional_context.filter(|context| !context.trim().is_empty()) else {
        return step_prompt.to_string();
    };
    format!(
        "{step_prompt}\n\nUser additional context for this macro invocation:\n{}",
        context.trim()
    )
}

/// Builds a synthetic request record for a harness-owned macro step.
pub fn macro_step_model_request(parent_turn: &AgentTurnRecord) -> ModelRequest {
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
        allowed_actions: AllowedActionSet::for_capability(AgentCapability::Subagent),
        stop: None,
        messages: vec![ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::TranscriptUser,
            content: "runtime-owned macro first step".to_string(),
        }],
    }
}

/// Builds the system policy for a structured macro judge request.
pub fn macro_judge_policy() -> String {
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
pub fn macro_judge_task(
    run: &MacroRunState,
    step: &MacroRunStep,
    result: &MacroStepTaskResult,
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
                    "outcome": judgment.outcome.as_str(),
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
pub fn macro_judge_decision_from_text(
    text: &str,
    step_count: usize,
    step_index: usize,
) -> Result<MacroJudgeDecision, MacroContractError> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).map_err(|error| {
        MacroContractError::new(format!(
            "macro judge response invalid after step {}: expected JSON object: {error}",
            step_index.saturating_add(1)
        ))
    })?;
    let object = value.as_object().ok_or_else(|| {
        MacroContractError::new(format!(
            "macro judge response invalid after step {}: expected JSON object",
            step_index.saturating_add(1)
        ))
    })?;
    let outcome = object
        .get("outcome")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MacroContractError::new("macro judge response missing outcome"))?
        .parse::<MacroJudgeOutcome>()?;
    let step_success = object
        .get("step_success")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| MacroContractError::new("macro judge response missing step_success"))?;
    let rationale = object
        .get("rationale")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| MacroContractError::new("macro judge response missing rationale"))?
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
            return Err(MacroContractError::new(
                "macro judge cannot continue after the final step",
            ));
        }
        MacroJudgeOutcome::ContinueWithAdaptedPrompt if final_step => {
            return Err(MacroContractError::new(
                "macro judge cannot adapt a next prompt after the final step",
            ));
        }
        MacroJudgeOutcome::ContinueWithAdaptedPrompt if adapted_prompt.is_none() => {
            return Err(MacroContractError::new(
                "macro judge adapted continuation requires adapted_prompt",
            ));
        }
        MacroJudgeOutcome::StopFailure if user_message.is_none() => {
            return Err(MacroContractError::new(
                "macro judge stop_failure requires user_message",
            ));
        }
        MacroJudgeOutcome::FinishSuccess if !final_step => {
            return Err(MacroContractError::new(
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

/// Parses the required ordered prompt list from a macro body.
pub fn parse_macro_steps(body: &str) -> Result<Vec<MacroStep>, MacroContractError> {
    let mut in_steps = false;
    let mut steps = Vec::new();
    let mut current_indent = 0usize;
    let mut current_lines = Vec::<String>::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if !in_steps {
            if trimmed == MACRO_STEPS_HEADING {
                in_steps = true;
            }
            continue;
        }
        if line.starts_with('#') && is_markdown_section_heading(line) {
            break;
        }
        if let Some((indent, content)) = parse_ordered_list_item(line) {
            push_current_macro_step(&mut steps, &mut current_lines)?;
            current_indent = indent;
            current_lines.push(content);
            continue;
        }
        if current_lines.is_empty() {
            if trimmed.is_empty() {
                continue;
            }
            return Err(MacroContractError::new(
                "Steps section must contain ordered-list prompt steps",
            ));
        }
        if trimmed.is_empty() {
            current_lines.push(String::new());
        } else if leading_ascii_whitespace_len(line) > current_indent {
            current_lines.push(trimmed.to_string());
        } else {
            return Err(MacroContractError::new(format!(
                "line {:?} in Steps is not an ordered item or indented continuation",
                trimmed
            )));
        }
    }
    if !in_steps {
        return Err(MacroContractError::new(
            "MACRO.md must contain a ## Steps section",
        ));
    }
    push_current_macro_step(&mut steps, &mut current_lines)?;
    if steps.is_empty() {
        return Err(MacroContractError::new(
            "Steps section must contain at least one prompt step",
        ));
    }
    Ok(steps)
}

/// Returns whether a macro name is a safe lowercase identifier.
pub fn is_valid_macro_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name
            .bytes()
            .any(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn split_macro_front_matter(text: &str) -> Result<(&str, &str), MacroContractError> {
    let normalized = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"));
    let Some(after_open) = normalized else {
        return Err(MacroContractError::new(
            "MACRO.md must start with YAML front matter",
        ));
    };
    for marker in ["\n---\n", "\n---\r\n", "\r\n---\r\n", "\r\n---\n"] {
        if let Some(index) = after_open.find(marker) {
            return Ok((&after_open[..index], &after_open[index + marker.len()..]));
        }
    }
    Err(MacroContractError::new(
        "MACRO.md front matter is not closed",
    ))
}

fn is_markdown_section_heading(line: &str) -> bool {
    let trimmed = line.trim_end();
    let hash_count = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if hash_count == 0 || hash_count > 6 {
        return false;
    }
    let after_hashes = &trimmed[hash_count..];
    after_hashes.is_empty() || after_hashes.starts_with(' ')
}

fn push_current_macro_step(
    steps: &mut Vec<MacroStep>,
    current_lines: &mut Vec<String>,
) -> Result<(), MacroContractError> {
    if current_lines.is_empty() {
        return Ok(());
    }
    if steps.len() == MAX_MACRO_STEPS {
        return Err(MacroContractError::new(format!(
            "macro contains more than {MAX_MACRO_STEPS} prompt steps"
        )));
    }
    let prompt = normalized_step_prompt(current_lines);
    if prompt.trim().is_empty() {
        return Err(MacroContractError::new(format!(
            "macro step {} prompt must not be empty",
            steps.len() + 1
        )));
    }
    steps.push(MacroStep {
        index: steps.len() + 1,
        prompt,
    });
    current_lines.clear();
    Ok(())
}

fn normalized_step_prompt(lines: &[String]) -> String {
    let first = lines.iter().position(|line| !line.trim().is_empty());
    let last = lines.iter().rposition(|line| !line.trim().is_empty());
    match (first, last) {
        (Some(first), Some(last)) => lines[first..=last].join("\n"),
        _ => String::new(),
    }
}

fn parse_ordered_list_item(line: &str) -> Option<(usize, String)> {
    let indent = leading_ascii_whitespace_len(line);
    let rest = &line[indent..];
    let bytes = rest.as_bytes();
    let mut digit_end = 0usize;
    while digit_end < bytes.len() && bytes[digit_end].is_ascii_digit() {
        digit_end += 1;
    }
    if digit_end == 0 || digit_end >= bytes.len() || !matches!(bytes[digit_end], b'.' | b')') {
        return None;
    }
    let content_start = digit_end + 1;
    if content_start < bytes.len() && !bytes[content_start].is_ascii_whitespace() {
        return None;
    }
    Some((indent, rest[content_start..].trim_start().to_string()))
}

fn leading_ascii_whitespace_len(line: &str) -> usize {
    line.bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: &str, source: MacroSource, description: &str) -> MacroSummary {
        MacroSummary {
            name: name.to_string(),
            description: description.to_string(),
            source,
            path: PathBuf::from(format!("/{}/{name}/{MACRO_FILE_NAME}", source.as_str())),
            step_count: 2,
        }
    }

    #[test]
    /// Verifies catalog insertion applies project-over-user-over-builtin
    /// precedence and records the winning collision diagnostic.
    fn macro_catalog_applies_source_precedence() {
        let mut catalog = MacroCatalog::default();
        catalog.insert(summary("ship-it", MacroSource::Builtin, "Built in"));
        catalog.insert(summary("ship-it", MacroSource::User, "User"));
        catalog.insert(summary("ship-it", MacroSource::Project, "Project"));

        assert_eq!(catalog.macros.len(), 1);
        assert_eq!(catalog.get("ship-it").unwrap().description, "Project");
        assert_eq!(catalog.diagnostics.len(), 2);
        assert!(
            catalog
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.message.contains("overrides existing"))
        );
    }

    #[test]
    /// Verifies model-facing and structured catalog representations include
    /// source, description, path, and step-count details.
    fn macro_catalog_formats_text_and_structured_json() {
        let mut catalog = MacroCatalog::default();
        catalog.insert(summary(
            "review-release",
            MacroSource::User,
            "Review a release",
        ));

        let text = catalog.model_catalog_text();
        let json = catalog.structured_json();

        assert!(text.contains("- review-release (user, 2 steps) - Review a release"));
        assert!(json.contains(r#""name":"review-release""#));
        assert!(json.contains(r#""step_count":2"#));
    }

    #[test]
    /// Verifies macro prompt invocation is recognized only at the start and
    /// preserves trailing user-stated context.
    fn macro_prompt_invocation_parses_name_and_context() {
        let invocation = parse_macro_prompt_invocation("  #release-check for v1.2").unwrap();

        assert_eq!(invocation.name, "release-check");
        assert_eq!(invocation.additional_context.as_deref(), Some("for v1.2"));
        assert!(parse_macro_prompt_invocation("hello #release-check").is_none());
        assert!(parse_macro_prompt_invocation("#").is_none());
    }

    #[test]
    /// Verifies macro-name validation accepts safe identifiers and rejects
    /// uppercase or path-like names.
    fn macro_name_validation_rejects_paths_and_uppercase() {
        assert!(is_valid_macro_name("release-check-2"));
        assert!(!is_valid_macro_name("Release"));
        assert!(!is_valid_macro_name("release/check"));
        assert!(!is_valid_macro_name(".."));
        assert!(!is_valid_macro_name("---"));
    }

    #[test]
    /// Verifies hash-prefixed continuation lines remain prompt text rather than
    /// being mistaken for section headings.
    fn macro_steps_accept_hash_prefixed_continuation_lines() {
        let body = "## Steps\n\n1. First step\n   # nested macro call\n2. Second step\n";
        let steps = parse_macro_steps(body).unwrap();

        assert_eq!(steps.len(), 2);
        assert!(steps[0].prompt.contains("# nested macro call"));
        assert_eq!(steps[1].prompt, "Second step");
    }

    #[test]
    /// Verifies an actual Markdown section heading terminates ordered step
    /// parsing before subsequent sections can be ingested.
    fn macro_steps_heading_terminates_parsing() {
        let body = "## Steps\n\n1. First step\n2. Second step\n\n## Next Section\n\n3. Should not appear\n";
        let steps = parse_macro_steps(body).unwrap();

        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].prompt, "First step");
        assert_eq!(steps[1].prompt, "Second step");
    }

    #[test]
    /// Verifies adapted continuation is accepted only before the final step
    /// and only when the judge supplies a non-empty adapted prompt.
    fn macro_judge_decision_validates_adapted_continuation() {
        let decision = macro_judge_decision_from_text(
            r#"{"outcome":"continue_with_adapted_prompt","step_success":true,"rationale":"step passed","adapted_prompt":"Run the next step with the observed id.","user_message":null}"#,
            2,
            0,
        )
        .unwrap();
        assert_eq!(
            decision.outcome,
            MacroJudgeOutcome::ContinueWithAdaptedPrompt
        );
        assert_eq!(
            decision.adapted_prompt.as_deref(),
            Some("Run the next step with the observed id.")
        );

        let missing_prompt = macro_judge_decision_from_text(
            r#"{"outcome":"continue_with_adapted_prompt","step_success":true,"rationale":"step passed","adapted_prompt":null,"user_message":null}"#,
            2,
            0,
        )
        .unwrap_err();
        assert!(
            missing_prompt
                .message()
                .contains("adapted continuation requires adapted_prompt")
        );

        let final_step = macro_judge_decision_from_text(
            r#"{"outcome":"continue_with_adapted_prompt","step_success":true,"rationale":"step passed","adapted_prompt":"extra work","user_message":null}"#,
            1,
            0,
        )
        .unwrap_err();
        assert!(
            final_step
                .message()
                .contains("cannot adapt a next prompt after the final step")
        );
    }

    #[test]
    /// Verifies a recoverable judge decision can retry the current step,
    /// including the final scripted step, without advancing the run.
    fn macro_judge_decision_allows_retry_current_step() {
        let retry = macro_judge_decision_from_text(
            r#"{"outcome":"retry_current_step","step_success":false,"rationale":"recoverable","adapted_prompt":"Inspect directly.","user_message":null}"#,
            2,
            0,
        )
        .unwrap();
        assert_eq!(retry.outcome, MacroJudgeOutcome::RetryCurrentStep);
        assert_eq!(retry.adapted_prompt.as_deref(), Some("Inspect directly."));

        let final_step = macro_judge_decision_from_text(
            r#"{"outcome":"retry_current_step","step_success":false,"rationale":"recoverable","adapted_prompt":null,"user_message":null}"#,
            1,
            0,
        )
        .unwrap();
        assert_eq!(final_step.outcome, MacroJudgeOutcome::RetryCurrentStep);
    }

    #[test]
    /// Verifies terminal decisions are position-sensitive and failures require
    /// a user-visible explanation before the harness accepts them.
    fn macro_judge_decision_validates_terminal_outcomes() {
        let finish = macro_judge_decision_from_text(
            r#"{"outcome":"finish_success","step_success":true,"rationale":"done","adapted_prompt":null,"user_message":null}"#,
            2,
            1,
        )
        .unwrap();
        assert_eq!(finish.outcome, MacroJudgeOutcome::FinishSuccess);

        let early_finish = macro_judge_decision_from_text(
            r#"{"outcome":"finish_success","step_success":true,"rationale":"done","adapted_prompt":null,"user_message":null}"#,
            2,
            0,
        )
        .unwrap_err();
        assert!(
            early_finish
                .message()
                .contains("cannot finish before the final step")
        );

        let missing_message = macro_judge_decision_from_text(
            r#"{"outcome":"stop_failure","step_success":false,"rationale":"failed","adapted_prompt":null,"user_message":null}"#,
            2,
            0,
        )
        .unwrap_err();
        assert!(
            missing_message
                .message()
                .contains("stop_failure requires user_message")
        );
    }

    #[test]
    /// Verifies direct macro recipients trim whitespace while empty and
    /// unrelated recipient forms remain unavailable to the macro bridge.
    fn macro_recipient_trims_whitespace_after_agent_prefix() {
        assert_eq!(
            macro_message_recipient_agent_id("agent: agent-%5"),
            Some("agent-%5".to_string())
        );
        assert_eq!(
            macro_message_recipient_agent_id("agent:agent-%7 "),
            Some("agent-%7".to_string())
        );
        assert_eq!(macro_message_recipient_agent_id("agent:   "), None);
        assert_eq!(
            macro_message_recipient_agent_id("agent-%12"),
            Some("agent-%12".to_string())
        );
        assert_eq!(macro_message_recipient_agent_id("pane:%1"), None);
    }
}
