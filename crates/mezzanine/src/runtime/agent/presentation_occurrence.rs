//! Immutable provenance for the existing validated provider presentation order.
//!
//! Response identities remain agent-owned and use exact chronology. This owner
//! retains one accepted wall-clock basis for static source components lacking
//! provisional evidence. It cannot execute, resubmit, reorder or deduplicate
//! actions; render workers and persistence only consume the captured metadata.

use super::{Result, RuntimeSessionService, provider_execution};
use mez_core::presentation::{PresentationEntry, PresentationOccurrenceBasis};

impl RuntimeSessionService {
    /// Retains local batch acceptance in the existing response-order owner once.
    pub(in crate::runtime) fn retain_provider_presentation_order(
        &mut self,
        pane: &str,
        execution: &mez_agent::AgentTurnExecution,
    ) -> Result<()> {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return Ok(());
        };
        let Some(session) = self.agent_shell_store().get(pane) else {
            return Ok(());
        };
        let group = provider_execution::provider_log_execution_group_id(execution)?;
        let key = (
            pane.to_string(),
            execution.request.turn_id.clone(),
            session.session_id.clone(),
            group,
        );
        if self
            .presentation
            .agent_provider_log_orders
            .contains_key(&key)
        {
            return Ok(());
        }
        let occurrence =
            self.capture_presentation_occurrence(PresentationOccurrenceBasis::LocalAcceptance);
        self.presentation.agent_provider_log_orders.insert(
            key,
            crate::runtime::render::RuntimeProviderPresentationOrder {
                actions: batch.actions.clone(),
                occurrence,
                streamed_entries: Default::default(),
            },
        );
        Ok(())
    }

    /// Returns a component identity and accepted stamp without consulting text,
    /// sampling, or changing chronology. Missing response ownership stays unknown.
    pub(in crate::runtime) fn provider_component_entry(
        &self,
        pane: &str,
        execution: &mez_agent::AgentTurnExecution,
        ordinal: Option<usize>,
        component: &str,
    ) -> Result<PresentationEntry> {
        let conversation = self
            .agent_shell_store()
            .get(pane)
            .map(|session| session.session_id.as_str())
            .unwrap_or_default();
        let group = provider_execution::provider_log_execution_group_id(execution)?;
        let owner = (
            pane.to_string(),
            execution.request.turn_id.clone(),
            conversation.to_string(),
            group.clone(),
        );
        if let Some(entry) = self
            .presentation
            .agent_provider_log_orders
            .get(&owner)
            .and_then(|owner| {
                owner
                    .streamed_entries
                    .get(&(ordinal, component.to_string()))
            })
        {
            return Ok(entry.clone());
        }
        Ok(PresentationEntry {
            id: format!(
                "provider:{conversation}:{}:{}:{component}",
                group.as_str(),
                ordinal.map_or_else(|| "rationale".to_string(), |ordinal| ordinal.to_string())
            ),
            occurrence: self
                .presentation
                .agent_provider_log_orders
                .get(&owner)
                .map(|owner| owner.occurrence)
                .unwrap_or_default(),
            first_row: true,
        })
    }

    /// Returns independently validated stream evidence, if any. Unlike the
    /// static component adapter, absence does not substitute batch acceptance
    /// for a later executor or sender's own local acceptance boundary.
    pub(in crate::runtime) fn retained_provider_component_entry(
        &self,
        pane: &str,
        execution: &mez_agent::AgentTurnExecution,
        ordinal: Option<usize>,
        component: &str,
    ) -> Result<Option<PresentationEntry>> {
        let Some(session) = self.agent_shell_store().get(pane) else {
            return Ok(None);
        };
        let group = provider_execution::provider_log_execution_group_id(execution)?;
        let key = (
            pane.to_string(),
            execution.request.turn_id.clone(),
            session.session_id.clone(),
            group,
        );
        Ok(self
            .presentation
            .agent_provider_log_orders
            .get(&key)
            .and_then(|owner| {
                owner
                    .streamed_entries
                    .get(&(ordinal, component.to_string()))
            })
            .cloned())
    }

    /// Transfers only exact closed source evidence from reconciliation into the
    /// existing accepted response owner. Layout promotion is not required;
    /// callers validate each component before transferring its immutable stamp.
    pub(in crate::runtime) fn retain_validated_stream_entries(
        &mut self,
        pane: &str,
        execution: &mez_agent::AgentTurnExecution,
        entries: Vec<((Option<usize>, String), PresentationEntry)>,
    ) -> Result<()> {
        self.retain_provider_presentation_order(pane, execution)?;
        let Some(session) = self.agent_shell_store().get(pane) else {
            return Ok(());
        };
        let group = provider_execution::provider_log_execution_group_id(execution)?;
        let key = (
            pane.to_string(),
            execution.request.turn_id.clone(),
            session.session_id.clone(),
            group,
        );
        if let Some(owner) = self.presentation.agent_provider_log_orders.get_mut(&key) {
            for (component, entry) in entries {
                owner.streamed_entries.entry(component).or_insert(entry);
            }
        }
        Ok(())
    }
}
