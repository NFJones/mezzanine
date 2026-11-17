//! Turn-local refusal when provider failure reporting cannot safely settle.
//!
//! Retain the original causal error and source before any reporting mutation.
//! This is bounded by admitted, fenced pane work, not an executable history or
//! retry queue. Existing physical workers and persistence obligations keep
//! their original owners; no effect or append is inferred complete. A refused
//! pane cannot create more work or discard its source, while other panes can
//! use the released provider capacity and control service.

use super::*;
use crate::runtime::RuntimeAgentProviderPersistenceOutcome;

/// Immutable pre-report source. Copies never acquire execution/publication
/// rights and are never installed as a replacement live context.
pub(super) struct ProviderFailureSource {
    turn: AgentTurnRecord,
    context: Option<AgentContext>,
    execution: Option<AgentTurnExecution>,
    history: Option<std::sync::Arc<crate::runtime::control::RuntimeAgentHistorySource>>,
    imported_high_water: u64,
    // Diagnostic custody only; never restored or submitted for execution.
    persistence_outcome: Option<Box<RuntimeAgentProviderPersistenceOutcome>>,
}

/// Required failure evidence remains available even when audit, transcript or
/// presentation failed. Repeated delivery cannot replace or grow this record.
pub(super) struct ProviderFailureRefusal {
    source: ProviderFailureSource,
    cause: MezError,
    settlement: MezError,
    local_errors: Vec<MezError>,
}

impl std::fmt::Debug for ProviderFailureSource {
    /// Diagnostics describe custody without printing prompts, commands or rows.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderFailureSource")
            .field("turn", &self.turn)
            .field("has_context", &self.context.is_some())
            .field("has_execution", &self.execution.is_some())
            .field("has_history", &self.history.is_some())
            .field("imported_high_water", &self.imported_high_water)
            .field(
                "has_persistence_outcome",
                &self.persistence_outcome.is_some(),
            )
            .finish()
    }
}

impl std::fmt::Debug for ProviderFailureRefusal {
    /// Retained typed errors may include provider text; generic Debug is bounded
    /// to categories and ownership, independent of those diagnostic payloads.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderFailureRefusal")
            .field("source", &self.source)
            .field("cause_kind", &self.cause.kind())
            .field("settlement_kind", &self.settlement.kind())
            .field("local_errors", &self.local_errors.len())
            .finish()
    }
}

impl RuntimeSessionService {
    /// Captures the actual pre-report owner without requesting another history
    /// read, fabricating a request, or mutating pending publication obligations.
    pub(in crate::runtime::agent) fn capture_provider_failure_source(
        &self,
        turn_id: &str,
    ) -> Option<ProviderFailureSource> {
        Some(ProviderFailureSource {
            turn: self.agent_turn_ledger().turn(turn_id)?.clone(),
            context: self.agent_turn_contexts().get(turn_id).cloned(),
            execution: self.agent_turn_executions().get(turn_id).cloned(),
            history: self.agent_turn_history_source(turn_id).cloned(),
            imported_high_water: self.agent_turn_imported_history_sequence_high_water(turn_id),
            persistence_outcome: None,
        })
    }

    /// Keeps already-issued worker results alongside the pre-adoption source.
    /// This is refusal evidence only: neither copy may be installed as a live
    /// execution, used to retry writes, or treated as an audit/publication ACK.
    pub(in crate::runtime::agent) fn capture_provider_persistence_failure_source(
        &self,
        turn: &AgentTurnRecord,
        outcome: &RuntimeAgentProviderPersistenceOutcome,
    ) -> ProviderFailureSource {
        ProviderFailureSource {
            turn: turn.clone(),
            context: self.agent_turn_contexts().get(&turn.turn_id).cloned(),
            execution: self.agent_turn_executions().get(&turn.turn_id).cloned(),
            history: self.agent_turn_history_source(&turn.turn_id).cloned(),
            imported_high_water: self
                .agent_turn_imported_history_sequence_high_water(&turn.turn_id),
            persistence_outcome: Some(Box::new(outcome.clone())),
        }
    }

    /// Contains an accepted outcome's adoption failure without regenerating a
    /// failed execution. An inner stage may already have retained a refusal;
    /// preserve its first source/cause and fill missing outcome custody only.
    pub(in crate::runtime::agent) fn retain_provider_persistence_adoption_refusal(
        &mut self,
        mut source: ProviderFailureSource,
        error: &MezError,
    ) {
        if let Some(refusal) = self
            .agent
            .provider_failure_refusals
            .get_mut(&source.turn.turn_id)
        {
            if refusal.source.turn.agent_id == source.turn.agent_id
                && refusal.source.turn.pane_id == source.turn.pane_id
                && refusal.source.turn.conversation_id == source.turn.conversation_id
                && refusal.source.persistence_outcome.is_none()
            {
                refusal.source.persistence_outcome = source.persistence_outcome.take();
            }
            return;
        }
        self.retain_provider_failure_refusal(
            source,
            error,
            MezError::invalid_state(
                "issued provider persistence results retained after adoption refusal; automatic replay disabled",
            ),
        );
    }

    /// Compares original retained evidence without exposing it or granting replay.
    #[cfg(test)]
    pub(crate) fn retained_persistence_outcome_matches_for_tests(
        &self,
        expected: &RuntimeAgentProviderPersistenceOutcome,
    ) -> bool {
        self.agent
            .provider_failure_refusals
            .get(&expected.turn.turn_id)
            .filter(|refusal| {
                refusal.cause.kind() == crate::error::MezErrorKind::Forbidden
                    && refusal
                        .cause
                        .message()
                        .contains("audit logging is required but disabled")
            })
            .and_then(|refusal| refusal.source.persistence_outcome.as_deref())
            == Some(expected)
    }

    /// Tests only actual retained refusal ownership, never error text.
    pub(in crate::runtime) fn agent_provider_failure_is_refused(&self, turn_id: &str) -> bool {
        self.agent.provider_failure_refusals.contains_key(turn_id)
    }

    /// Borrows the immutable pre-refusal execution solely to retain an already
    /// issued result. This does not restore live delivery, paint, publication,
    /// worker admission or continuation authority to the blocked turn.
    pub(in crate::runtime::agent) fn provider_failure_original_execution(
        &self,
        turn_id: &str,
    ) -> Option<(&AgentTurnRecord, &AgentTurnExecution)> {
        let source = &self.agent.provider_failure_refusals.get(turn_id)?.source;
        Some((&source.turn, source.execution.as_ref()?))
    }

    /// Observes retained custody without granting a restore/retry capability.
    #[cfg(test)]
    pub(crate) fn provider_failure_source_for_tests(
        &self,
        turn: &str,
    ) -> Option<(&AgentContext, Option<&AgentTurnExecution>)> {
        let refusal = self.agent.provider_failure_refusals.get(turn)?;
        Some((
            refusal.source.context.as_ref()?,
            refusal.source.execution.as_ref(),
        ))
    }

    /// Rejects new source consumers and destructive cleanup for the affected
    /// pane. A new CID cannot launder its unresolved original obligations.
    pub(in crate::runtime) fn ensure_provider_failure_pane_open(&self, pane: &str) -> Result<()> {
        if let Some(refusal) = self
            .agent
            .provider_failure_refusals
            .values()
            .find(|refusal| refusal.source.turn.pane_id == pane)
        {
            return Err(refusal.cause.clone().with_context(format!(
                "provider failure settlement refused for {}; source retained, automatic retry disabled; settlement: {}",
                refusal.source.turn.turn_id, refusal.settlement
            )));
        }
        Ok(())
    }

    /// Releases only inference scheduling capacity. All original context,
    /// execution, bookkeeping and issued-effect custody survives refusal.
    /// Optional reporting errors are retained without recursive reporting.
    pub(in crate::runtime::agent) fn retain_provider_failure_refusal(
        &mut self,
        source: ProviderFailureSource,
        cause: &MezError,
        settlement: MezError,
    ) {
        let turn = source.turn.clone();
        if self.agent_provider_failure_is_refused(&turn.turn_id) {
            return;
        }
        let diagnostic = format!(
            "agent: provider failure settlement refused; turn={} source retained; automatic retry disabled; retained_context={} retained_execution={} history_source={} imported_high_water={}; cause={}; settlement={}",
            turn.turn_id,
            source.context.is_some(),
            source.execution.is_some(),
            source.history.is_some(),
            source.imported_high_water,
            cause,
            settlement
        );
        self.agent.provider_failure_refusals.insert(
            turn.turn_id.clone(),
            ProviderFailureRefusal {
                source,
                cause: cause.clone(),
                settlement,
                local_errors: Vec::new(),
            },
        );
        self.agent
            .pending_agent_provider_tasks
            .remove(&turn.turn_id);
        self.agent
            .claimed_agent_provider_tasks
            .remove(&turn.turn_id);
        let mut local_errors = Vec::new();
        if self
            .agent
            .agent_scheduler
            .running_turns()
            .any(|work| work.turn_id == turn.turn_id)
            && let Err(error) = self.agent.agent_scheduler.block_running(&turn.turn_id)
        {
            local_errors.push(error.into());
        }
        if self
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .is_some_and(|current| current.state == AgentTurnState::Running)
            && let Err(error) = self
                .agent_turn_ledger_mut()
                .finish_turn(&turn.turn_id, AgentTurnState::Blocked)
        {
            local_errors.push(error.into());
        }
        self.reconcile_active_turn_sleep_inhibition();
        if let Err(error) =
            self.append_agent_trace_turn_event(&turn.pane_id, &turn.turn_id, &diagnostic)
        {
            local_errors.push(error);
        }
        if let Err(error) =
            self.append_agent_error_text_to_terminal_buffer(&turn.pane_id, &diagnostic)
        {
            local_errors.push(error);
        }
        if let Err(error) = self.start_ready_agent_turns() {
            local_errors.push(error);
        }
        // A fixed number of one-shot local operations; no retries or growing
        // secondary-error ledger on duplicate failure delivery.
        if let Some(refusal) = self.agent.provider_failure_refusals.get_mut(&turn.turn_id) {
            refusal.local_errors = local_errors;
        }
    }
}
