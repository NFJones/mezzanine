//! Runtime lifecycle for ambiguous Bubblewrap payload-failure assessment.
//!
//! The provider-independent request and response contract lives in
//! `mez-agent`. This module owns only actor-scoped pending state and the
//! transition between shell settlement, provider dispatch, and an eventual
//! approval or ordinary command failure.

use crate::runtime::{
    AgentTurnRecord, MezError, ModelRequest, ModelResponse, PaneReadinessState, Result,
    RunningShellTransactionRef, RuntimeSandboxFailureAssessment, RuntimeSessionService,
};

impl RuntimeSessionService {
    /// Queues one bounded internal model assessment after Bubblewrap proves
    /// that a prompt-classified payload executed and then exited non-zero.
    pub(crate) fn queue_sandbox_failure_assessment(
        &mut self,
        turn: &AgentTurnRecord,
        action_id: &str,
        marker: &str,
        transaction: RunningShellTransactionRef,
        exit_code: i32,
    ) -> Result<bool> {
        if exit_code == 0
            || self
                .agent
                .sandbox_failure_assessments
                .contains_key(&turn.turn_id)
        {
            return Ok(false);
        }
        let execution = self
            .agent_turn_executions()
            .get(&turn.turn_id)
            .ok_or_else(|| {
                MezError::invalid_state("sandbox assessment execution is unavailable")
            })?;
        let batch = execution.response.action_batch.as_ref().ok_or_else(|| {
            MezError::invalid_state("sandbox assessment execution has no action batch")
        })?;
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .ok_or_else(|| MezError::invalid_state("sandbox assessment action is unavailable"))?;
        let evaluation = execution
            .action_results
            .iter()
            .find(|result| result.action_id == action_id)
            .and_then(|result| result.permission_evaluation.as_deref())
            .ok_or_else(|| {
                MezError::invalid_state("sandbox assessment permission evaluation is unavailable")
            })?;
        if evaluation.decision != mez_agent::permissions::RuleDecision::Prompt {
            return Ok(false);
        }
        let model_profile = self
            .agent
            .agent_turn_model_profiles
            .get(&turn.turn_id)
            .ok_or_else(|| {
                MezError::invalid_state("sandbox assessment model profile is unavailable")
            })?;
        let decoded = mez_agent::decode_shell_output_transport_with_diagnostics(
            &transaction.observed_output_preview,
        );
        let output_preview = if decoded.diagnostics.saw_begin_marker {
            decoded.output
        } else {
            transaction.observed_output_preview.clone()
        };
        let write_effects = evaluation
            .effects
            .writes
            .iter()
            .chain(&evaluation.effects.creates)
            .chain(&evaluation.effects.deletes)
            .chain(&evaluation.effects.touches)
            .cloned()
            .collect();
        let evidence = mez_agent::SandboxFailureAssessmentEvidence {
            action_kind: action.action_type().to_string(),
            permission_decision: "prompt".to_string(),
            matched_rule_ids: evaluation.matched_rule_ids.clone(),
            read_effects: evaluation.effects.reads.clone(),
            write_effects,
            effect_completeness: match evaluation.completeness {
                mez_agent::permissions::EffectCompleteness::Unknown => "unknown",
                mez_agent::permissions::EffectCompleteness::Complete => "complete",
            }
            .to_string(),
            exit_code,
            output_preview,
            output_truncated: transaction.observed_output_truncated
                || decoded.diagnostics.output_truncated()
                || decoded.diagnostics.transport_incomplete(),
            sandbox_restrictions: vec![
                "isolated network namespace".to_string(),
                "minimal cleared environment".to_string(),
                "policy-derived read-only and read-write mounts".to_string(),
                "no host credentials, process control, or privilege changes".to_string(),
            ],
        };
        let request = mez_agent::sandbox_failure_assessment_request(turn, model_profile, &evidence)
            .map_err(|error| MezError::invalid_state(error.message()))?;
        self.agent.sandbox_failure_assessments.insert(
            turn.turn_id.clone(),
            RuntimeSandboxFailureAssessment {
                action_id: action_id.to_string(),
                marker: marker.to_string(),
                transaction,
                exit_code,
                request,
            },
        );
        self.agent
            .pending_agent_provider_tasks
            .insert(turn.turn_id.clone());
        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Ready);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "sandbox_failure_assessment queued action={} exit_code={} partial_effect_warning=true",
                action_id, exit_code
            ),
        )?;
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            "agent: assessing whether Bubblewrap caused the command failure",
        )?;
        Ok(true)
    }

    /// Returns the dedicated structured request retained for one pending
    /// Bubblewrap failure assessment.
    pub(super) fn sandbox_failure_assessment_request_for_turn(
        &self,
        turn_id: &str,
    ) -> Option<ModelRequest> {
        self.agent
            .sandbox_failure_assessments
            .get(turn_id)
            .map(|pending| pending.request.clone())
    }

    /// Returns a pending assessment request to crate-local regression tests.
    #[cfg(test)]
    pub(crate) fn sandbox_failure_assessment_request_for_tests(
        &self,
        turn_id: &str,
    ) -> Option<ModelRequest> {
        self.sandbox_failure_assessment_request_for_turn(turn_id)
    }

    /// Applies one structured assessment response. Only an explicit validated
    /// sandbox-failure attribution may create an approval; every other result
    /// settles the original command failure normally.
    pub(crate) fn apply_sandbox_failure_assessment_provider_response(
        &mut self,
        turn: &AgentTurnRecord,
        response: &ModelResponse,
    ) -> Result<()> {
        let Some(pending) = self.agent.sandbox_failure_assessments.remove(&turn.turn_id) else {
            return Err(MezError::invalid_state(
                "sandbox assessment response has no pending failure",
            ));
        };
        self.agent
            .pending_agent_provider_tasks
            .remove(&turn.turn_id);
        self.agent
            .claimed_agent_provider_tasks
            .remove(&turn.turn_id);
        let assessment = mez_agent::sandbox_failure_assessment_from_text(&response.raw_text);
        if let Ok(assessment) = &assessment
            && assessment.class == mez_agent::SandboxFailureAssessmentClass::SandboxFailure
            && assessment.retry_requested
        {
            let proof = format!(
                "model confidence={:.3}: {}",
                assessment.confidence, assessment.rationale
            );
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "sandbox_failure_assessment applied class={} confidence={:.3} retry_requested=true",
                    assessment.class.as_str(), assessment.confidence
                ),
            )?;
            if self.offer_sandbox_fallback_approval(
                &pending.marker,
                &turn.turn_id,
                &pending.action_id,
                "model_assessed_sandbox_failure",
                &proof,
                true,
            )? {
                return Ok(());
            }
        }
        let reason = assessment
            .as_ref()
            .map(|assessment| assessment.class.as_str())
            .unwrap_or("invalid_assessment");
        self.settle_sandbox_failure_assessment_as_command_failure(pending, reason)
    }

    /// Settles a pending assessment as the original command failure when the
    /// provider request fails, times out, or returns invalid/uncertain output.
    pub(crate) fn settle_pending_sandbox_failure_assessment(
        &mut self,
        turn_id: &str,
        reason: &str,
    ) -> Result<bool> {
        let Some(pending) = self.agent.sandbox_failure_assessments.remove(turn_id) else {
            return Ok(false);
        };
        self.agent.pending_agent_provider_tasks.remove(turn_id);
        self.agent.claimed_agent_provider_tasks.remove(turn_id);
        self.settle_sandbox_failure_assessment_as_command_failure(pending, reason)?;
        Ok(true)
    }
}
