//! Private completed-output validation and bounded correction on existing tasks.
//!
//! Invalid text never enters accepted chunk/synthesis state. Source integrity is
//! checked independently before admitting correction; only auxiliary inference
//! is requeued, preserving the logical epoch and frozen current source. Transport
//! retry and exact claim/expense settlement remain owned by the parent subsystem.

use super::*;
use mez_agent::context_compaction::{
    ModelCompactionSummaryDefect, ModelCompactionSummaryRejection,
    validate_model_compaction_summary,
};

/// Finite corrections for output without measurable semantic-size progress.
const EMPTY_CORRECTIONS: u8 = 2;
/// Canonical compaction block byte bound, also enforced in the lower validator.
const SUMMARY_BYTES: usize = 128 * 1024;

impl RuntimeSessionService {
    /// Returns accepted text, or admits another exact auxiliary generation and
    /// returns None. Integrity/exhaustion errors use normal truthful settlement.
    pub(super) fn validate_or_correct_compaction_summary(
        &mut self,
        task: &mut RuntimeAgentCompactionTask,
        response: &ModelResponse,
    ) -> Result<Option<String>> {
        let (budget, tokens) = match &task.target {
            RuntimeAgentCompactionTarget::ActiveTurn { plan, .. } => {
                (plan.summary_budget_words(), plan.planning_budget().1)
            }
            RuntimeAgentCompactionTarget::Conversation => {
                (task.request.max_output_tokens.unwrap_or(usize::MAX), false)
            }
        };
        let budget = task
            .summary_correction
            .output_ceiling
            .map_or(budget, |ceiling| ceiling.min(budget));
        let (summary, malformed) = match response.action_batch.as_ref() {
            Some(batch) => match batch.actions.as_slice() {
                [action] => match &action.payload {
                    AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text,
                        ..
                    } => (text.trim().to_string(), false),
                    _ => (String::new(), true),
                },
                _ => (String::new(), true),
            },
            None => (response.raw_text.trim().to_string(), false),
        };
        let validation = if malformed {
            Err(ModelCompactionSummaryRejection {
                defect: ModelCompactionSummaryDefect::Malformed,
                bytes: response.raw_text.len(),
                cost: 0,
                budget,
                estimated_tokens: tokens,
            })
        } else {
            validate_model_compaction_summary(&summary, budget, tokens)
        };
        if validation.is_ok() {
            // Reset only after bounded output is accepted for this source. Final
            // request progress accounting is separate and is never reset here.
            task.summary_correction = Default::default();
            return runtime_model_compaction_summary_from_response(response).map(Some);
        }
        let rejected = validation
            .err()
            .ok_or_else(|| MezError::invalid_state("summary validation outcome disappeared"))?;
        self.correct_rejected_compaction_summary(task, rejected)?;
        Ok(None)
    }

    /// Admits private correction of a typed malformed completed wire response.
    /// Only the existing crate-authored structured diagnostic tag selects this
    /// path; generic InvalidState, raw output and message substrings never do.
    pub(super) fn correct_malformed_compaction_response(
        &mut self,
        task: &mut RuntimeAgentCompactionTask,
    ) -> Result<()> {
        let (budget, tokens) = match &task.target {
            RuntimeAgentCompactionTarget::ActiveTurn { plan, .. } => {
                (plan.summary_budget_words(), plan.planning_budget().1)
            }
            RuntimeAgentCompactionTarget::Conversation => {
                (task.request.max_output_tokens.unwrap_or(usize::MAX), false)
            }
        };
        self.correct_rejected_compaction_summary(
            task,
            ModelCompactionSummaryRejection {
                defect: ModelCompactionSummaryDefect::Malformed,
                bytes: 0,
                cost: 0,
                budget: task
                    .summary_correction
                    .output_ceiling
                    .unwrap_or(budget)
                    .min(budget),
                estimated_tokens: tokens,
            },
        )
    }

    /// Shares source proof, finite/progress policy and exact fresh-generation
    /// admission across completed values and typed malformed wire failures.
    fn correct_rejected_compaction_summary(
        &mut self,
        task: &mut RuntimeAgentCompactionTask,
        rejected: ModelCompactionSummaryRejection,
    ) -> Result<()> {
        self.validate_compaction_correction_source(task)?;
        prepare_summary_correction(task, rejected)?;
        self.append_agent_trace_turn_event(
            &task.pane_id,
            task.resume_turn_id.as_deref().unwrap_or("manual"),
            &format!("context_compaction summary_correction reason={:?} summary_bytes={} summary_cost={} budget={} unit={} output_ceiling={}",
                rejected.defect, rejected.bytes, rejected.cost, rejected.budget,
                if rejected.estimated_tokens { "estimated_input_tokens" } else { "words" },
                task.request.max_output_tokens.unwrap_or(0)),
        )?;
        self.queue_agent_compaction_task(task.clone());
        Ok(())
    }

    /// Proves the frozen source remains authoritative before treating output as
    /// correctable. A one-character private witness exercises existing strict
    /// application/mapping validators but is never stored, published or sent.
    fn validate_compaction_correction_source(
        &self,
        task: &RuntimeAgentCompactionTask,
    ) -> Result<()> {
        match &task.target {
            RuntimeAgentCompactionTarget::ActiveTurn {
                turn_id,
                plan,
                staged,
                ..
            } => {
                let turn = self.agent_turn_ledger().turn(turn_id).ok_or_else(|| {
                    MezError::invalid_state("summary correction turn disappeared")
                })?;
                if turn.state != AgentTurnState::Running {
                    return Err(MezError::invalid_state(
                        "summary correction turn is terminal",
                    ));
                }
                let live = self.agent_turn_contexts().get(turn_id).ok_or_else(|| {
                    MezError::invalid_state("summary correction context disappeared")
                })?;
                let context = if let Some(staged) = staged {
                    let mut context = staged.context.clone();
                    context
                        .rebase_chronology_suffix(&staged.source_chronology, live)
                        .map_err(|error| MezError::invalid_state(error.message()))?;
                    context
                } else {
                    live.clone()
                };
                apply_model_context_compaction_plan(context.clone(), plan, "x")
                    .map_err(|error| MezError::invalid_state(error.message()))?;
                // This also validates archive/epoch/frozen-row evidence; a
                // legitimate turn-local mapping remains eligible, not corruption.
                self.prospective_observed_compaction_epoch(task, &context, plan, "x")?;
            }
            RuntimeAgentCompactionTarget::Conversation => {
                self.manual_compaction_candidate_context(task, "x")?;
                let store = self.persistence.cloned_transcript_store().ok_or_else(|| {
                    MezError::invalid_state("summary correction source store disappeared")
                })?;
                let pending = self
                    .persistence
                    .pending_transcript_entries(&task.conversation_id);
                let view = store.conversation_transcript_view(
                    &task.conversation_id,
                    crate::storage::transcript::ConversationTranscriptRead::All,
                    task.transcript_entries > pending.len() as u64,
                    &pending,
                )?;
                if task
                    .frozen_compaction_rows
                    .iter()
                    .any(|row| !view.logical.contains(row))
                {
                    return Err(MezError::conflict(
                        "summary correction frozen manual source changed",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Tightens targets without changing current source, accepted staging, ordinary
/// retry state, provider profile or authoritative context. Repeated oversize
/// output and an exhausted measurable ceiling fail; improving work has no total
/// response-count cap. Empty output consumes its own finite correction budget.
fn prepare_summary_correction(
    task: &mut RuntimeAgentCompactionTask,
    rejected: ModelCompactionSummaryRejection,
) -> Result<()> {
    let state = &mut task.summary_correction;
    let ceiling = task
        .request
        .max_output_tokens
        .unwrap_or(rejected.budget)
        .min(rejected.budget);
    let next = if matches!(
        rejected.defect,
        ModelCompactionSummaryDefect::Empty | ModelCompactionSummaryDefect::Malformed
    ) {
        if state.empty_attempts >= EMPTY_CORRECTIONS {
            return Err(MezError::invalid_state(
                "compaction empty/malformed-summary correction budget exhausted",
            ));
        }
        state.empty_attempts += 1;
        ceiling.max(1)
    } else {
        if state.last_size.is_some_and(|(cost, bytes)| {
            (rejected.cost > rejected.budget && rejected.cost >= cost)
                || (rejected.bytes > SUMMARY_BYTES && rejected.bytes >= bytes)
        }) {
            return Err(MezError::invalid_state(
                "compaction invalid-summary correction made no size progress",
            ));
        }
        let next = ceiling / 2;
        if next == 0 {
            return Err(MezError::invalid_state(
                "compaction invalid-summary correction output ceiling exhausted",
            ));
        }
        state.last_size = Some((rejected.cost, rejected.bytes));
        next
    };
    state.output_ceiling = Some(next);
    // Reuse the frozen current request: it already owns the exact redacted
    // chunk/synthesis source. Only bounded developer guidance and output control
    // change. Fresh generation admission remains the existing queue owner.
    task.request.max_output_tokens = Some(next);
    task.preserve_summary_output_budget = true;
    communicate_summary_target(&mut task.request, next, rejected.estimated_tokens);
    let mut messages = task.request.messages.iter().cloned().collect::<Vec<_>>();
    messages.retain(|message| {
        !(message.role == ModelMessageRole::Developer
            && message.source == ContextSourceKind::Configuration
            && message.content.starts_with("[private summary correction]"))
    });
    messages.insert(2.min(messages.len()), ModelMessage {
        role: ModelMessageRole::Developer, source: ContextSourceKind::Configuration,
        placement: mez_agent::ContextPlacement::StablePrefix,
        content: format!("[private summary correction]\nThe previous completed output was rejected ({:?}). Generate a fresh nonempty concise summary of the same frozen source, not a repair of quoted rejected text. Return exactly one final say. Local summary target is at most {next} {}; UTF-8 summary bytes must not exceed {SUMMARY_BYTES}. The provider output-token ceiling is also {next}; leave room for action framing. No logs, repeated history or unsupported claims.", rejected.defect,
            if rejected.estimated_tokens { "estimated input tokens (conservative UTF-8 byte estimate, about four bytes per token)" } else { "whitespace-delimited words" }),
    });
    task.request.messages = messages.into();
    Ok(())
}

/// Publishes the actual local target unit to the compactor, separately from its
/// output-token control. Replaces bounded guidance rather than accumulating it.
pub(super) fn communicate_summary_target(request: &mut ModelRequest, budget: usize, tokens: bool) {
    let mut messages = request.messages.iter().cloned().collect::<Vec<_>>();
    messages.retain(|message| {
        !(message.role == ModelMessageRole::Developer
            && message.source == ContextSourceKind::Configuration
            && message.content.starts_with("[compaction summary target]"))
    });
    messages.insert(2.min(messages.len()), ModelMessage {
        role: ModelMessageRole::Developer, source: ContextSourceKind::Configuration,
        placement: mez_agent::ContextPlacement::StablePrefix,
        content: format!("[compaction summary target]\nKeep the nonempty summary within {budget} {} and {SUMMARY_BYTES} UTF-8 bytes. Provider max_output_tokens is a separate token ceiling including response framing; reserve room for the final say wrapper.", if tokens { "estimated input tokens (approximately four UTF-8 bytes per token)" } else { "whitespace-delimited words" }),
    });
    request.messages = messages.into();
}
