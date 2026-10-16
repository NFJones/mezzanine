//! Private completed-output validation and bounded correction on existing tasks.
//!
//! Invalid text never enters accepted chunk/synthesis state. Source integrity is
//! checked independently before admitting correction; only auxiliary inference
//! is requeued, preserving the logical epoch and frozen current source. Transport
//! retry and exact claim/expense settlement remain owned by the parent subsystem.

use super::*;
use crate::error::{
    CompactionCostUnit, CompactionFailureDisposition, CompactionFailureEvidence,
    CompactionFailurePhase,
};
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
            RuntimeAgentCompactionTarget::Conversation => (
                task.summary_correction.manual_words.ok_or_else(|| {
                    MezError::invalid_state("manual compaction frozen word allowance is missing")
                })?,
                false,
            ),
        };
        let (summary, malformed) = runtime_model_compaction_summary_from_response(
            response,
            &task.request.turn_id,
            &task.request.agent_id,
        )
        .map_or_else(|_| (String::new(), true), |summary| (summary, false));
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
        // A frozen-valid summary MUST reach the existing complete-candidate
        // owner, even above its advisory replan target. Only that owner measures
        // full fit and enforces candidate decrease; diverting here would grant
        // a fresh correction budget to an already non-reducing final candidate.
        if validation.is_ok() {
            // Reset only after bounded output is accepted for this source. Final
            // request progress accounting is separate and is never reset here.
            let mut receipt = compaction_failure_evidence(
                task,
                None,
                CompactionFailurePhase::Summary,
                CompactionFailureDisposition::CandidateRejected,
            );
            receipt.phase = CompactionFailurePhase::CompleteCandidate;
            receipt.source_generation = receipt.source_generation.or(Some(task.task_generation));
            receipt.measured_cost = Some(if tokens {
                mez_agent::provider_text_input_token_estimate(&summary)
            } else {
                model_context_text_word_count(&summary)
            });
            receipt.measured_bytes = Some(summary.len());
            receipt.size_progress = task.summary_correction.last_rejection.map(|previous| {
                (receipt.measured_cost.unwrap_or(0) <= budget
                    || receipt.measured_cost.unwrap_or(0) < previous.cost)
                    && (summary.len() <= SUMMARY_BYTES || summary.len() < previous.bytes)
            });
            task.summary_correction.reset_progress();
            task.summary_correction.accepted_summary = Some(Box::new(receipt));
            let mut messages = task.request.messages.iter().cloned().collect::<Vec<_>>();
            messages.retain(|message| {
                !(message.role == ModelMessageRole::Developer
                    && message.source == ContextSourceKind::Configuration
                    && message.content.starts_with("[private summary correction]"))
            });
            task.request.messages = messages.into();
            let (target, target_tokens) = task
                .summary_correction
                .effective_input_tokens
                .map_or((budget, tokens), |target| (target, true));
            communicate_summary_target(&mut task.request, target, target_tokens);
            return Ok(Some(summary));
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
            RuntimeAgentCompactionTarget::Conversation => (
                task.summary_correction.manual_words.ok_or_else(|| {
                    MezError::invalid_state("manual compaction frozen word allowance is missing")
                })?,
                false,
            ),
        };
        self.correct_rejected_compaction_summary(
            task,
            ModelCompactionSummaryRejection {
                defect: ModelCompactionSummaryDefect::Malformed,
                bytes: 0,
                cost: 0,
                budget,
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
        self.validate_compaction_correction_source(task)
            .map_err(|error| {
                error.with_compaction_failure(compaction_failure_evidence(
                    task,
                    Some(rejected),
                    CompactionFailurePhase::Summary,
                    CompactionFailureDisposition::SourceIntegrity,
                ))
            })?;
        prepare_summary_correction(task, rejected)?;
        self.append_agent_trace_turn_event(
            &task.pane_id,
            task.resume_turn_id.as_deref().unwrap_or("manual"),
            &format!("context_compaction summary_correction reason={:?} summary_bytes={} summary_cost={} budget={} unit={} guidance_target={}",
                rejected.defect, rejected.bytes, rejected.cost, rejected.budget,
                if rejected.estimated_tokens { "estimated_input_tokens" } else { "words" },
                task.summary_correction.guidance_target.unwrap_or(0)),
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
            }
        }
        Ok(())
    }
}

/// Tightens advisory targets without changing semantic acceptance or provider
/// allocation. Repeated invalid dimensions and invalid output after the issued
/// nonzero guidance floor fail finitely. Valid output is admitted before this
/// policy runs, even when it exceeds guidance. Empty output has its own budget.
fn prepare_summary_correction(
    task: &mut RuntimeAgentCompactionTask,
    rejected: ModelCompactionSummaryRejection,
) -> Result<()> {
    let next =
        next_summary_target(&mut task.summary_correction, rejected).map_err(|disposition| {
            MezError::invalid_state(disposition.message()).with_compaction_failure(
                compaction_failure_evidence(
                    task,
                    Some(rejected),
                    CompactionFailurePhase::Summary,
                    disposition,
                ),
            )
        })?;
    let state = &mut task.summary_correction;
    state.source_generation.get_or_insert(task.task_generation);
    state.corrections_admitted = state.corrections_admitted.saturating_add(1);
    if matches!(
        rejected.defect,
        ModelCompactionSummaryDefect::Bytes | ModelCompactionSummaryDefect::Budget
    ) {
        state.last_rejection = Some(rejected);
    }
    state.guidance_target = Some(next);
    state.guidance_is_input_tokens = rejected.estimated_tokens;
    // Reuse the frozen current request: it already owns the exact redacted
    // chunk/synthesis source. Only bounded developer guidance changes. Keep
    // configured generation allocation for framing/reasoning on providers that
    // actually support it; OpenAI's canonical adapter omits the wire field.
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
        content: format!("[private summary correction]\nThe previous completed output was rejected ({:?}). Generate a fresh nonempty concise summary of the same frozen source, not a repair of quoted rejected text. Return exactly one final say. Aim for at most {next} {}; this is advisory guidance, not a provider-enforced output cap. UTF-8 summary bytes must not exceed {SUMMARY_BYTES}. The authoritative semantic allowance remains {} in that unit. No logs, repeated history or unsupported claims.", rejected.defect,
            if rejected.estimated_tokens { "estimated input tokens (conservative UTF-8 byte estimate, about four bytes per token)" } else { "whitespace-delimited words" }, rejected.budget),
    });
    task.request.messages = messages.into();
    Ok(())
}

/// Extracts only the completed summary payload. Compaction adapters intentionally
/// allow raw text without a MAAP wire schema; when the model follows the local
/// final-say instructions, decode that optional frame here without changing the
/// provider API or wire controls. Ordinary JSON/code summaries remain text unless
/// they identify themselves as a MAAP envelope. Malformed frames consume the
/// same finite correction budget as already-parsed malformed batches.
pub(super) fn completed_summary(
    response: &ModelResponse,
    turn_id: &str,
    agent_id: &str,
) -> std::result::Result<String, ()> {
    let raw = response.raw_text.trim();
    let decoded = if response.action_batch.is_none() {
        let envelope = match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(value) => value
                .as_object()
                .is_some_and(|object| object.contains_key("actions")),
            Err(_) => {
                raw.starts_with('{')
                    && (raw.contains("\"actions\"") || raw.contains("\"rationale\""))
            }
        };
        if envelope {
            Some(
                mez_agent::parse_maap_action_batch_json_for_turn(raw, turn_id, agent_id)
                    .map_err(|_| ())?,
            )
        } else {
            mez_agent::parse_fenced_maap_action_batch_for_turn(raw, turn_id, agent_id)
                .map_err(|_| ())?
        }
    } else {
        None
    };
    match response.action_batch.as_ref().or(decoded.as_ref()) {
        Some(batch) => match batch.actions.as_slice() {
            [action] => match &action.payload {
                AgentActionPayload::Say {
                    status: mez_agent::SayStatus::Final,
                    text,
                    ..
                } => Ok(text.trim().to_string()),
                _ => Err(()),
            },
            _ => Err(()),
        },
        None => Ok(raw.to_string()),
    }
}

/// Computes finite advisory progress using measured invalid dimensions, never
/// the previous guidance as a new semantic threshold. Unit changes do not make
/// incomparable costs evidence of size progress; the positive floor still
/// bounds mixed-defect sequences independently of transport retry policy.
fn next_summary_target(
    state: &mut crate::runtime::agent_state::RuntimeSummaryCorrection,
    rejected: ModelCompactionSummaryRejection,
) -> std::result::Result<usize, CompactionFailureDisposition> {
    let ceiling = state.guidance_target.unwrap_or(rejected.budget).max(1);
    let next = if matches!(
        rejected.defect,
        ModelCompactionSummaryDefect::Empty | ModelCompactionSummaryDefect::Malformed
    ) {
        if state.empty_attempts >= EMPTY_CORRECTIONS {
            return Err(CompactionFailureDisposition::EmptyOrMalformedExhausted);
        }
        state.empty_attempts += 1;
        ceiling.max(1)
    } else {
        if state.last_rejection.is_some_and(|previous| {
            (rejected.cost > rejected.budget
                && rejected.estimated_tokens == previous.estimated_tokens
                && rejected.budget == previous.budget
                && rejected.cost >= previous.cost)
                || (rejected.bytes > SUMMARY_BYTES && rejected.bytes >= previous.bytes)
        }) {
            return Err(CompactionFailureDisposition::NoProgress);
        }
        if state.guidance_target == Some(1) {
            return Err(CompactionFailureDisposition::GuidanceFloor);
        }
        (ceiling / 2).max(1)
    };
    Ok(next)
}

/// Captures only locally authoritative measurements. A resolved Responses
/// shape establishes canonical omission, not an internal request allocation.
pub(super) fn compaction_failure_evidence(
    task: &RuntimeAgentCompactionTask,
    rejected: Option<ModelCompactionSummaryRejection>,
    phase: CompactionFailurePhase,
    disposition: CompactionFailureDisposition,
) -> CompactionFailureEvidence {
    if phase == CompactionFailurePhase::CompleteCandidate
        && let Some(receipt) = task.summary_correction.accepted_summary.as_deref()
    {
        let mut facts = receipt.clone();
        facts.phase = phase;
        facts.disposition = disposition;
        return facts;
    }
    let rejected = if phase == CompactionFailurePhase::ProviderInference {
        rejected.or(task.summary_correction.last_rejection)
    } else {
        rejected
    };
    let (frozen_allowance, tokens, synthesis) = match &task.target {
        RuntimeAgentCompactionTarget::ActiveTurn {
            plan,
            synthesis_source_bytes,
            ..
        } => (
            Some(plan.summary_budget_words()),
            plan.planning_budget().1,
            synthesis_source_bytes.is_some(),
        ),
        RuntimeAgentCompactionTarget::Conversation => (
            task.summary_correction.manual_words,
            false,
            task.conversation_chunks
                .as_ref()
                .is_some_and(|chunks| chunks.synthesis_source_bytes.is_some()),
        ),
    };
    let unit = |tokens| {
        if tokens {
            CompactionCostUnit::EstimatedInputTokens
        } else {
            CompactionCostUnit::Words
        }
    };
    CompactionFailureEvidence {
        phase: if phase == CompactionFailurePhase::Summary && synthesis {
            CompactionFailurePhase::Synthesis
        } else {
            phase
        },
        source_generation: task.summary_correction.source_generation.or_else(|| {
            task.summary_correction
                .accepted_summary
                .as_ref()
                .and_then(|receipt| receipt.source_generation)
        }),
        task_generation: task.task_generation,
        defect: rejected.map(|value| value.defect),
        frozen_allowance,
        frozen_unit: unit(tokens),
        effective_input_tokens: task.summary_correction.effective_input_tokens,
        measured_cost: rejected.map(|value| value.cost),
        measured_unit: unit(rejected.map_or(tokens, |value| value.estimated_tokens)),
        measured_bytes: rejected.map(|value| value.bytes),
        byte_bound: SUMMARY_BYTES,
        guidance_target: task.summary_correction.guidance_target,
        guidance_unit: unit(task.summary_correction.guidance_is_input_tokens),
        size_progress: rejected
            .zip(task.summary_correction.last_rejection)
            .and_then(|(now, prior)| {
                if now.estimated_tokens != prior.estimated_tokens || now.budget != prior.budget {
                    None
                } else {
                    Some(
                        (now.cost <= now.budget || now.cost < prior.cost)
                            && (now.bytes <= SUMMARY_BYTES || now.bytes < prior.bytes),
                    )
                }
            }),
        corrections_admitted: task.summary_correction.corrections_admitted.saturating_add(
            task.summary_correction
                .accepted_summary
                .as_ref()
                .map_or(0, |receipt| receipt.corrections_admitted),
        ),
        disposition,
        openai_max_output_tokens_transmitted: task.compaction_request_shape.as_ref().and_then(
            |(api, _, _)| {
                (*api == mez_agent::ProviderApiCompatibility::OpenAiResponses).then_some(false)
            },
        ),
    }
}

/// Publishes an advisory target in its actual local unit, independently of
/// provider generation controls. Replaces guidance rather than accumulating it.
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
        content: format!("[compaction summary target]\nAim for a nonempty summary within {budget} {} and {SUMMARY_BYTES} UTF-8 bytes. This is local semantic guidance, not evidence of a transmitted provider output control. Return a complete final say wrapper.", if tokens { "estimated input tokens (approximately four UTF-8 bytes per token)" } else { "whitespace-delimited words" }),
    });
    request.messages = messages.into();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::agent_state::RuntimeSummaryCorrection;

    /// Local targets never become zero for zero/one/two-token allowances. A
    /// still-invalid completion after the issued floor terminates, whereas
    /// semantic validity is independently decided against the frozen allowance.
    #[test]
    fn summary_correction_tiny_targets_have_finite_nonzero_floor() {
        for budget in [0, 1, 2, 128] {
            let mut state = RuntimeSummaryCorrection::default();
            let mut cost = budget + 100;
            for attempt in 0..usize::BITS {
                let rejected = ModelCompactionSummaryRejection {
                    defect: ModelCompactionSummaryDefect::Budget,
                    bytes: cost * 4,
                    cost,
                    budget,
                    estimated_tokens: true,
                };
                let target = next_summary_target(&mut state, rejected);
                if state.guidance_target == Some(1) {
                    assert_eq!(target, Err(CompactionFailureDisposition::GuidanceFloor));
                    break;
                }
                let target = target.unwrap();
                assert!(target > 0);
                state.guidance_target = Some(target);
                state.last_rejection = Some(rejected);
                cost -= 1;
                assert!(
                    attempt < usize::BITS - 1,
                    "floor must bound this exact source"
                );
            }
            assert_eq!(state.guidance_target, Some(1));
        }
        assert!(validate_model_compaction_summary("x", 1, true).is_ok());
        assert!(validate_model_compaction_summary("x", 0, true).is_err());
    }

    /// Fixed byte and semantic dimensions must improve independently. A smaller
    /// byte count does not excuse a repeated invalid semantic cost and vice
    /// versa; the advisory target is not substituted for either fixed bound.
    #[test]
    fn summary_correction_progress_uses_fixed_invalid_dimensions() {
        let prior = ModelCompactionSummaryRejection {
            defect: ModelCompactionSummaryDefect::Bytes,
            bytes: SUMMARY_BYTES + 10,
            cost: 130,
            budget: 128,
            estimated_tokens: true,
        };
        for (cost, bytes) in [(130, SUMMARY_BYTES + 1), (129, SUMMARY_BYTES + 10)] {
            let mut state = RuntimeSummaryCorrection {
                last_rejection: Some(prior),
                guidance_target: Some(64),
                ..Default::default()
            };
            assert_eq!(
                next_summary_target(
                    &mut state,
                    ModelCompactionSummaryRejection {
                        cost,
                        bytes,
                        ..prior
                    }
                ),
                Err(CompactionFailureDisposition::NoProgress)
            );
        }
        let mut state = RuntimeSummaryCorrection {
            last_rejection: Some(prior),
            ..Default::default()
        };
        assert_eq!(
            next_summary_target(
                &mut state,
                ModelCompactionSummaryRejection {
                    cost: 129,
                    bytes: SUMMARY_BYTES + 1,
                    ..prior
                }
            ),
            Ok(64)
        );
    }
}
