//! Shared ordinary settlement over the existing completion-group owner.
//!
//! Source staging is not candidate acceptance, a pending provider claim, audit
//! success or durability. Retained intent is one metadata slot in its admitted
//! group; payload/read ownership stays in canonical execution and the existing
//! immutable handoff. Recovery only completes remaining publication/settlement
//! phases. It never replays an action, hook, assessment or ambiguous audit write.

use super::*;
use sha2::{Digest, Sha256};

/// Truthful ordinary-settlement result; Retained is never Queued or Declined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionSettlementDisposition {
    /// Canonical source/intent remains owned; no new provider insertion occurred.
    Retained,
    /// The existing queue actually accepted this provider continuation.
    Queued,
    /// Ordinary correction policy declined; terminal settlement may proceed.
    Declined,
}

/// One unresolved continuation obligation, with no copied outcome or retry list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime::agent) enum CompletionSettlementIntent {
    /// Successful or explicitly resumed ordinary continuation.
    Ordinary,
    /// Failure policy has not yet queued correction or declined terminalization.
    Failure,
}

/// Required audit phases distinguish absence of policy from successful append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureAuditPhase {
    /// Actual append was entered; an uncertain result must never be replayed.
    Entered,
    /// The existing append API returned success, not a disk-durability receipt.
    Accepted,
    /// No configured audit/profile required an append at this boundary.
    NotRequired,
}

/// One exact canonical-failure signature in its existing admitted group.
#[derive(Debug)]
pub(super) struct CompletionFailureAudit {
    source: String,
    phase: FailureAuditPhase,
}

impl RuntimeSessionService {
    /// Injects an isolated lost acknowledgement, not a second audit operation.
    #[cfg(test)]
    pub(crate) fn reject_completion_failure_audit_ack_for_tests(&mut self) {
        self.agent
            .completion_settlements
            .reject_failure_audit_ack_for_tests = true;
    }

    /// Reads the required gate phase without manufacturing acceptance/durability.
    #[cfg(test)]
    pub(crate) fn completion_failure_audit_phase_for_tests(
        &self,
        turn_id: &str,
    ) -> Option<&'static str> {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)?
            .failure_audit
            .as_ref()
            .map(|audit| match audit.phase {
                FailureAuditPhase::Entered => "entered",
                FailureAuditPhase::Accepted => "accepted",
                FailureAuditPhase::NotRequired => "not_required",
            })
    }

    /// Reads canonical live or moved backing in the same original group.
    #[cfg(test)]
    pub(crate) fn retained_completion_execution_for_tests(
        &self,
        turn_id: &str,
    ) -> Option<&AgentTurnExecution> {
        self.agent_turn_executions().get(turn_id).or_else(|| {
            self.agent
                .completion_settlements
                .groups
                .get(turn_id)?
                .failed_backing
                .as_ref()?
                .execution
                .as_ref()
        })
    }

    /// Reads retained immutable source, never an append/producer acknowledgement.
    #[cfg(test)]
    pub(crate) fn retained_completion_snapshot_for_tests(
        &self,
        turn_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .and_then(|group| group.handoff.as_ref())
            .map(|handoff| handoff.candidate.completion_snapshot_bytes())
            .transpose()
    }

    /// Isolates the production publication/staging cut without continuation or
    /// producer acceptance. No receipt phase is fabricated by the fixture.
    #[cfg(test)]
    pub(crate) fn publish_and_stage_completion_sources_for_tests(
        &mut self,
        turn_id: &str,
    ) -> Result<Vec<u8>> {
        let keys = self
            .agent
            .completion_settlements
            .external
            .iter()
            .filter(|(key, receipt)| {
                key.0 == turn_id && receipt.phase == ExternalCompletionPhase::PublicationHeld
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            if !self.publish_retained_external_completion(&key)? {
                return Err(MezError::invalid_state(
                    "fixture source is still publication-held",
                ));
            }
        }
        self.stage_completion_group_transcript(turn_id)?;
        self.retained_completion_snapshot_for_tests(turn_id)?
            .ok_or_else(|| MezError::invalid_state("fixture has no original staged snapshot"))
    }

    /// Exercises the guarded data transition only; it grants no execution or
    /// approval authority and is absent from production action surfaces.
    #[cfg(test)]
    pub(crate) fn adopt_completion_canonical_transition_for_tests(
        &mut self,
        turn: &AgentTurnRecord,
        expected: &AgentTurnExecution,
        proposed: &AgentTurnExecution,
    ) -> Result<()> {
        self.adopt_completion_canonical_transition(turn, expected, proposed)
    }

    /// Validates ORIGINAL canonical source, not replacement pane projections.
    /// A stale supplied value cannot overwrite actor execution or supply C2 data.
    pub(in crate::runtime::agent) fn ensure_completion_settlement_source_current(
        &self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let Some(group) = self.agent.completion_settlements.groups.get(&turn.turn_id) else {
            return Ok(());
        };
        self.ensure_completion_required_gates_ready(&turn.turn_id)?;
        let current = self
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("settlement original turn is unavailable"))?;
        let stored = self
            .agent_turn_executions()
            .get(&turn.turn_id)
            .ok_or_else(|| {
                MezError::invalid_state("settlement canonical execution is unavailable")
            })?;
        if group.turn.turn_id != turn.turn_id
            || group.turn.agent_id != turn.agent_id
            || group.turn.pane_id != turn.pane_id
            || group.turn.conversation_id != turn.conversation_id
            || current.agent_id != group.turn.agent_id
            || current.pane_id != group.turn.pane_id
            || current.conversation_id != group.turn.conversation_id
            || stored.request != execution.request
            || stored.action_results != execution.action_results
            || stored.final_turn != execution.final_turn
            || stored.terminal_state != execution.terminal_state
            || stored.response.action_batch != execution.response.action_batch
            || super::super::provider_execution::provider_log_execution_group_id(stored)?
                != group.group
        {
            return Err(MezError::invalid_state(
                "settlement differs from its original canonical owner",
            ));
        }
        Ok(())
    }

    /// Publishes only a validated same-owner local dispatcher transition before
    /// fallible staging/audit. It cannot replace a changed actor execution or
    /// foreign action batch, and it does not grant dispatch/approval authority.
    pub(in crate::runtime::agent) fn adopt_completion_canonical_transition(
        &mut self,
        turn: &AgentTurnRecord,
        expected: &AgentTurnExecution,
        proposed: &AgentTurnExecution,
    ) -> Result<()> {
        if !self.completion_owns_transcript_policy(&turn.turn_id) {
            return Ok(());
        }
        if self
            .ensure_completion_settlement_source_current(turn, proposed)
            .is_ok()
        {
            self.retain_completion_settlement_intent(
                &turn.turn_id,
                CompletionSettlementIntent::Failure,
            );
            return self
                .stage_completion_group_transcript(&turn.turn_id)
                .map_err(|error| {
                    self.retain_completion_settlement_error(&turn.turn_id, "canonical_stage", error)
                });
        }
        self.ensure_completion_settlement_source_current(turn, expected)?;
        if expected.request != proposed.request
            || expected.response.action_batch != proposed.response.action_batch
            || expected.action_results.len() != proposed.action_results.len()
            || expected
                .action_results
                .iter()
                .zip(&proposed.action_results)
                .any(|(old, new)| {
                    old.action_id != new.action_id
                        || old.action_type != new.action_type
                        || new.turn_id != turn.turn_id
                        || new.agent_id != turn.agent_id
                })
        {
            return Err(MezError::invalid_state(
                "dispatcher transition differs from original completion source",
            ));
        }
        self.agent_turn_executions_mut()
            .insert(turn.turn_id.clone(), proposed.clone());
        self.retain_completion_settlement_intent(
            &turn.turn_id,
            CompletionSettlementIntent::Failure,
        );
        self.stage_completion_group_transcript(&turn.turn_id)
            .map_err(|error| {
                self.retain_completion_settlement_error(&turn.turn_id, "canonical_stage", error)
            })
    }

    /// New model work/paint requires the current binding; original archival and
    /// audit source remain tied to the frozen turn even when paint is revoked.
    pub(in crate::runtime::agent) fn completion_settlement_binding_current(
        &self,
        turn_id: &str,
    ) -> bool {
        let Some(group) = self.agent.completion_settlements.groups.get(turn_id) else {
            return true;
        };
        !group.paint_revoked
            && self.agent_turn_ledger().turn(turn_id).is_some_and(|turn| {
                matches!(
                    turn.state,
                    AgentTurnState::Running | AgentTurnState::Blocked
                ) && turn.agent_id == group.turn.agent_id
                    && turn.pane_id == group.turn.pane_id
                    && turn.conversation_id == group.turn.conversation_id
                    && self
                        .agent_shell_store()
                        .get(&turn.pane_id)
                        .is_some_and(|session| {
                            session.session_id == group.turn.conversation_id
                                && session.running_turn_id.as_deref() == Some(turn_id)
                        })
            })
    }

    /// Finds immutable ordered components belonging to the original group, not
    /// replacement-pane logs or a manufactured installation acknowledgement.
    pub(in crate::runtime::agent) fn completion_settlement_publication_pending(
        &self,
        turn_id: &str,
    ) -> bool {
        let Some(group) = self.agent.completion_settlements.groups.get(turn_id) else {
            return false;
        };
        if group.publication_held {
            return true;
        }
        self.presentation
            .agent_queued_provider_headers
            .keys()
            .chain(self.presentation.agent_queued_provider_commands.keys())
            .chain(self.presentation.agent_queued_provider_results.keys())
            .chain(self.presentation.agent_queued_provider_outcomes.keys())
            .any(|(pane, turn, conversation, owner, _)| {
                pane == &group.turn.pane_id
                    && turn == turn_id
                    && conversation == &group.turn.conversation_id
                    && owner == &group.group
            })
    }

    /// Holds a real body-publication obligation without making optional paint a
    /// required failure or treating a trace-only diagnostic as pending body.
    pub(in crate::runtime::agent) fn hold_completion_settlement_publication(
        &mut self,
        turn_id: &str,
        error: MezError,
    ) {
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id) {
            group.publication_held = true;
            group.publication_failure = Some(Box::new(error));
        }
    }

    /// Registers only an intent in the EXISTING bounded group, never pending work.
    pub(in crate::runtime::agent) fn retain_completion_settlement_intent(
        &mut self,
        turn_id: &str,
        intent: CompletionSettlementIntent,
    ) {
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id) {
            group.pending_settlement = Some(intent);
        }
    }

    /// Only actual queue acceptance or terminal group retirement discharges it.
    pub(in crate::runtime::agent) fn clear_completion_settlement_intent(&mut self, turn_id: &str) {
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id) {
            group.pending_settlement = None;
        }
    }

    /// Runs the unchanged failure-audit operation once for exact group/failure
    /// identity. Required error keeps Entered/unproven and the original backing;
    /// the publication pump must not append again after an ambiguous failure.
    pub(in crate::runtime::agent) fn append_completion_failure_audit_once(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
        append: fn(&mut Self, &AgentTurnRecord, &AgentTurnExecution) -> Result<()>,
    ) -> Result<()> {
        self.ensure_completion_settlement_source_current(turn, execution)?;
        let owns_scope = self.begin_completion_scope(&turn.turn_id, CompletionScope::Callback);
        let result = (|| -> Result<()> {
            self.stage_completion_group_transcript(&turn.turn_id)?;
            self.retain_completion_settlement_intent(
                &turn.turn_id,
                CompletionSettlementIntent::Failure,
            );
            let group = self
                .agent
                .completion_settlements
                .groups
                .get(&turn.turn_id)
                .ok_or_else(|| MezError::invalid_state("failure audit group is unavailable"))?;
            let failure = super::super::outcome::runtime_agent_execution_failure_error(execution);
            let kind = format!("{:?}", failure.kind());
            let bytes = serde_json::to_vec(&(
                group.group.as_str(),
                &group.issued,
                &group.observed,
                kind,
                failure.message(),
                failure.provider_failure_json(),
                failure.provider_raw_text(),
            ))
            .map_err(|_| {
                MezError::invalid_state("failure audit canonical identity cannot be encoded")
            })?;
            let source: String = Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let required = self
                .agent
                .agent_turn_model_profiles
                .contains_key(&turn.turn_id)
                && self.persistence.audit_log().is_some();
            if let Some(audit) = &group.failure_audit {
                if audit.phase == FailureAuditPhase::Entered {
                    return Err(MezError::invalid_state(
                        "failure audit append remains unproven; replay prohibited",
                    ));
                }
                if audit.source == source
                    && (audit.phase == FailureAuditPhase::Accepted || !required)
                {
                    return Ok(());
                }
            }
            let group = self
                .agent
                .completion_settlements
                .groups
                .get_mut(&turn.turn_id)
                .ok_or_else(|| MezError::invalid_state("failure audit owner disappeared"))?;
            group.failure_audit = Some(CompletionFailureAudit {
                source: source.clone(),
                phase: if required {
                    FailureAuditPhase::Entered
                } else {
                    FailureAuditPhase::NotRequired
                },
            });
            if !required {
                return Ok(());
            }
            append(self, turn, execution)?;
            #[cfg(test)]
            if std::mem::take(
                &mut self
                    .agent
                    .completion_settlements
                    .reject_failure_audit_ack_for_tests,
            ) {
                return Err(MezError::invalid_state(
                    "injected failure after audit append before acknowledgement",
                ));
            }
            let group = self
                .agent
                .completion_settlements
                .groups
                .get_mut(&turn.turn_id)
                .ok_or_else(|| {
                    MezError::invalid_state("failure audit owner missing after append")
                })?;
            let audit = group
                .failure_audit
                .as_mut()
                .filter(|audit| audit.source == source && audit.phase == FailureAuditPhase::Entered)
                .ok_or_else(|| {
                    MezError::invalid_state("failure audit owner changed after append")
                })?;
            audit.phase = FailureAuditPhase::Accepted;
            Ok(())
        })();
        let result = result.map_err(|error| {
            self.retain_completion_settlement_error(&turn.turn_id, "failure_audit", error)
        });
        self.end_completion_scope(&turn.turn_id, owns_scope, "failure_audit", false, result)
    }

    /// Reports only a current intent whose remaining settlement can advance.
    /// Failure may reacquire scheduler capacity without yet authorizing ordinary
    /// provider continuation or consuming correction budget.
    pub(crate) fn completion_retained_failure_reacquisition_ready(&self, turn_id: &str) -> bool {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .is_some_and(|group| {
                group.pending_settlement == Some(CompletionSettlementIntent::Failure)
                    && group.primary_failure.is_none()
                    && group.failed_backing.is_none()
                    && self.completion_settlement_binding_current(turn_id)
                    && !self.agent_turn_is_human_paused(turn_id)
                    && self.completion_group_publication_ready(turn_id)
                    && self
                        .agent_scheduler()
                        .waiting_turns()
                        .any(|work| work.turn_id == turn_id)
                    && self
                        .agent_turn_executions()
                        .get(turn_id)
                        .is_some_and(|execution| {
                            execution.terminal_state == AgentTurnState::Failed
                                && !self
                                    .execution_waiting_for_live_joined_subagents(turn_id, execution)
                                && (execution
                                    .action_results
                                    .iter()
                                    .all(ActionResult::is_terminal)
                                    || self.execution_can_feed_failure_to_model(turn_id, execution))
                        })
            })
    }

    /// Shared full dependency-wake predicate, distinct from provider admission.
    pub(crate) fn completion_dependency_reacquisition_ready(&self, turn_id: &str) -> bool {
        (self.completion_provider_continuation_ready(turn_id)
            && self.completion_stored_execution_continuation_ready(turn_id))
            || self.completion_retained_failure_reacquisition_ready(turn_id)
    }

    /// Retained failure stages source only before fair reacquisition; it is not
    /// an archive acceptance or ordinary provider milestone.
    pub(crate) fn prepare_completion_dependency_reacquisition(
        &mut self,
        turn_id: &str,
    ) -> Result<bool> {
        self.ensure_completion_required_gates_ready(turn_id)?;
        if self.completion_retained_failure_reacquisition_ready(turn_id) {
            self.stage_completion_group_transcript(turn_id)?;
            return Ok(true);
        }
        self.prepare_completion_continuation_handoff(turn_id)
    }

    /// Reports only a current intent whose remaining settlement can advance.
    /// Failed/noncurrent owners are retained, not advertised as paint work.
    pub(in crate::runtime::agent) fn retained_completion_settlement_retry_needed(
        &self,
        turn_id: &str,
    ) -> bool {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .is_some_and(|group| {
                group.pending_settlement.is_some()
                    && group.primary_failure.is_none()
                    && group.failed_backing.is_none()
                    && group.scope.is_none()
                    && self.completion_settlement_binding_current(turn_id)
                    && !self.agent_turn_is_human_paused(turn_id)
                    && !self.agent_provider_task_is_owned(turn_id)
                    && self.completion_group_publication_ready(turn_id)
                    && self
                        .agent_turn_ledger()
                        .turn(turn_id)
                        .is_some_and(|turn| turn.state == AgentTurnState::Running)
                    && self
                        .agent_turn_executions()
                        .get(turn_id)
                        .is_some_and(|execution| {
                            !self.execution_waiting_for_live_joined_subagents(turn_id, execution)
                                && (matches!(
                                    execution.terminal_state,
                                    AgentTurnState::Completed
                                        | AgentTurnState::Failed
                                        | AgentTurnState::Interrupted
                                ) || self.completion_provider_continuation_ready(turn_id))
                        })
                    && !self
                        .process
                        .running_shell_transactions
                        .values()
                        .any(|transaction| transaction.turn_id == turn_id)
                    && !self
                        .agent
                        .pending_approved_external_actions
                        .keys()
                        .any(|(turn, _)| turn == turn_id)
                    && !self
                        .agent
                        .pending_native_shell_dispatches
                        .keys()
                        .any(|(turn, _)| turn == turn_id)
                    && !self
                        .agent
                        .claimed_native_shell_dispatches
                        .keys()
                        .any(|(turn, _)| turn == turn_id)
            })
    }

    /// Completes only the stored ordinary settlement suffix from current source.
    /// This never re-enters the managed/native command or assessment handlers.
    pub(in crate::runtime::agent) fn recover_retained_completion_settlement(
        &mut self,
        turn_id: &str,
    ) -> Result<bool> {
        if !self.retained_completion_settlement_retry_needed(turn_id) {
            return Ok(false);
        }
        let owns_scope = self.begin_completion_scope(turn_id, CompletionScope::Callback);
        let result = self.recover_retained_completion_settlement_in_scope(turn_id);
        let result = result.map_err(|error| {
            self.retain_completion_settlement_error(turn_id, "retained_ordinary_settlement", error)
        });
        self.end_completion_scope(
            turn_id,
            owns_scope,
            "retained_ordinary_settlement",
            false,
            result,
        )
    }

    /// Remaining suffix under the same group's lifetime guard, never a command
    /// callback replay. Required failures are latched by the enclosing boundary.
    fn recover_retained_completion_settlement_in_scope(&mut self, turn_id: &str) -> Result<bool> {
        let turn = self
            .agent_turn_ledger()
            .turn(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("retained settlement turn is unavailable"))?;
        let mut execution = self
            .agent_turn_executions()
            .get(turn_id)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("retained settlement execution is unavailable")
            })?;
        self.ensure_completion_settlement_source_current(&turn, &execution)?;
        self.stage_completion_group_transcript(turn_id)?;
        let publication = self
            .present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)
            .and_then(|()| self.flush_ordered_provider_headers(&turn.pane_id, &execution));
        let publication_pending = match publication {
            Ok(()) => {
                if let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id) {
                    group.publication_held = false;
                }
                self.completion_settlement_publication_pending(turn_id)
            }
            Err(error) => {
                self.hold_completion_settlement_publication(turn_id, error);
                true
            }
        };
        if execution.terminal_state == AgentTurnState::Failed {
            self.append_runtime_agent_execution_failure_audit(&turn, &execution)?;
            if publication_pending {
                return Ok(false);
            }
            match self.settle_agent_failure_feedback_for_correction(
                &turn,
                &mut execution,
                "retained_completion_settlement",
            )? {
                CompletionSettlementDisposition::Queued => {
                    self.agent_turn_executions_mut().remove(turn_id);
                    return Ok(true);
                }
                CompletionSettlementDisposition::Retained => return Ok(false),
                CompletionSettlementDisposition::Declined => {}
            }
        } else if execution.terminal_state == AgentTurnState::Running {
            if publication_pending {
                return Ok(false);
            }
            return self.admit_completion_provider_continuation(turn_id);
        }
        if publication_pending {
            return Ok(false);
        }
        if let Err(error) = self.flush_ordered_provider_headers(&turn.pane_id, &execution) {
            self.hold_completion_settlement_publication(turn_id, error);
            return Ok(false);
        }
        self.complete_running_agent_turn_and_start_ready(
            &turn,
            execution.terminal_state,
            "retained_completion_settlement",
        )?;
        Ok(true)
    }
}
