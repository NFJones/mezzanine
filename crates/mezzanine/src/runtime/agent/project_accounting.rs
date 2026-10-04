//! Native project partitions and conservation across runtime accounting restore.
//!
//! Partitions use frozen opaque origin, not current cwd. Legacy aggregate-only
//! metadata is explicitly unattributed. Restore changes views without emitting
//! durable usage events or changing external checkpoints.

use super::{ModelTokenUsage, ModelTokenUsageKey, RuntimeSessionService};
use crate::storage::token_usage::AccountingOrigin;
use mez_agent::ProjectTokenUsage;

/// Immutable issued-request provenance, separate from content acceptance.
#[derive(Debug, Clone)]
pub(super) struct ProviderAccountingOwner {
    /// Original task identity; state is not used to authorize response content.
    pub(super) turn: mez_agent::AgentTurnRecord,
    /// Request-selected profile before routing may choose an execution model.
    pub(super) profile: mez_agent::ModelProfile,
    /// Project captured when dispatch was frozen.
    pub(super) origin: AccountingOrigin,
    /// Kernel-backed pane-root incarnation, when available.
    pub(super) process: Option<crate::runtime::processes::RuntimePaneProcessIdentity>,
    /// Stable namespace retained for every storage observation from this request.
    pub(super) observation_id: String,
}

/// Frozen provenance shared by turn-less auxiliary requests.
#[derive(Debug, Clone)]
pub(super) struct AuxiliaryAccountingOwner {
    /// Original pane and conversation, not the current focus.
    pub(super) pane: String,
    /// Original conversation whose expense was incurred.
    pub(super) conversation: String,
    /// Opaque project captured before dispatch.
    pub(super) origin: AccountingOrigin,
    /// Request-selected model identity.
    pub(super) model: ModelTokenUsageKey,
    /// Root incarnation used only to qualify the pane view.
    pub(super) process: Option<crate::runtime::processes::RuntimePaneProcessIdentity>,
    /// Stable durable event identity.
    pub(super) observation: String,
}

/// Adds a disjoint observation to one normalized project/model partition.
fn add_partition(
    rows: &mut Vec<ProjectTokenUsage>,
    project_id: Option<String>,
    model: &ModelTokenUsageKey,
    usage: ModelTokenUsage,
) {
    if usage.is_zero() {
        return;
    }
    if let Some(row) = rows
        .iter_mut()
        .find(|row| row.project_id == project_id && row.model == *model)
    {
        row.usage.add_assign(usage);
    } else {
        rows.push(ProjectTokenUsage {
            project_id,
            model: model.clone(),
            usage,
        });
        rows.sort_by(|a, b| (&a.project_id, &a.model).cmp(&(&b.project_id, &b.model)));
    }
}

impl RuntimeSessionService {
    /// Refuses new auxiliary requests rather than evicting unsettled expense evidence.
    pub(in crate::runtime) fn require_auxiliary_accounting_capacity(
        &self,
    ) -> crate::error::Result<()> {
        if self.agent.compaction_accounting_owners.len() >= 256
            || self.agent.remember_accounting_owners.len() >= 256
        {
            return Err(super::MezError::invalid_state(
                "issued auxiliary accounting capacity exhausted",
            ));
        }
        Ok(())
    }

    /// Records reported compactor failure expense before exact generation retirement.
    pub(in crate::runtime) fn record_compaction_failure_usage(
        &mut self,
        pane: &str,
        generation: u64,
        usage: ModelTokenUsage,
    ) {
        let Some(task) = self
            .agent
            .compaction_accounting_owners
            .remove(&(pane.to_string(), generation))
        else {
            return;
        };
        self.record_auxiliary_usage(task, usage);
    }

    /// Records reported memory failure expense before exact observation retirement.
    pub(in crate::runtime) fn record_remember_failure_usage(
        &mut self,
        pane: &str,
        observation: &str,
        usage: ModelTokenUsage,
    ) {
        let Some(task) = self
            .agent
            .remember_accounting_owners
            .get(observation)
            .filter(|task| task.pane == pane)
            .cloned()
        else {
            return;
        };
        self.agent.remember_accounting_owners.remove(observation);
        self.record_auxiliary_usage(task, usage);
    }

    /// Settles consumed auxiliary evidence without publishing response content.
    fn record_auxiliary_usage(&mut self, owner: AuxiliaryAccountingOwner, usage: ModelTokenUsage) {
        let current = self
            .agent_shell_store()
            .get(&owner.pane)
            .is_some_and(|session| session.session_id == owner.conversation)
            && owner.process.as_ref().is_some_and(|identity| {
                self.pane_process_identity(&owner.pane)
                    .ok()
                    .is_some_and(|current| identity.same_incarnation(&current))
            });
        self.record_native_usage_observation(
            &owner.conversation,
            current.then_some(owner.pane.as_str()),
            &owner.origin,
            &owner.model,
            usage,
            owner.observation,
        );
    }

    /// Consumes one issued request's reported usage independently of content freshness.
    /// Unknown or mismatched generations cannot consume another request's evidence.
    pub(crate) fn settle_provider_request_usage(
        &mut self,
        agent: &mez_core::ids::AgentId,
        turn_id: &str,
        generation: u64,
        usage: &std::collections::BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) -> bool {
        let identity = (turn_id.to_string(), generation);
        let Some(owner) = self.agent.provider_accounting_owners.get(&identity) else {
            return false;
        };
        if owner.turn.agent_id != agent.as_str() {
            return false;
        }
        let Some(owner) = self.agent.provider_accounting_owners.remove(&identity) else {
            return false;
        };
        let pane = self
            .agent_shell_store()
            .get(&owner.turn.pane_id)
            .is_some_and(|session| session.session_id == owner.turn.conversation_id)
            && owner.process.as_ref().is_some_and(|identity| {
                self.pane_process_identity(&owner.turn.pane_id)
                    .ok()
                    .is_some_and(|current| identity.same_incarnation(&current))
            });
        for (index, (model, counters)) in usage.iter().enumerate() {
            self.record_native_usage_observation(
                &owner.turn.conversation_id,
                pane.then_some(owner.turn.pane_id.as_str()),
                &owner.origin,
                model,
                *counters,
                format!("provider:{}:{index}", owner.observation_id),
            );
        }
        true
    }

    /// Validates completion identity and charges ordinary plus routing observations once.
    pub(crate) fn settle_provider_execution_usage(
        &mut self,
        agent: &mez_core::ids::AgentId,
        turn_id: &str,
        generation: u64,
        execution: &mez_agent::AgentTurnExecution,
    ) -> bool {
        let Some(owner) = self
            .agent
            .provider_accounting_owners
            .get(&(turn_id.to_string(), generation))
        else {
            return false;
        };
        if mez_agent::outcome::runtime_validate_provider_completion_identity(
            &owner.turn,
            agent.as_str(),
            turn_id,
            execution,
        )
        .is_err()
        {
            return false;
        }
        let profile = mez_agent::apply_auto_sizing_execution_profile(
            owner.profile.clone(),
            &execution.request,
        );
        let mut usage = execution.routing_token_usage_by_model.clone();
        usage
            .entry(ModelTokenUsageKey::new(&profile.provider, &profile.model))
            .or_default()
            .add_assign(execution.response.usage);
        self.settle_provider_request_usage(agent, turn_id, generation, &usage)
    }

    /// Charges reported cutoff expense using the immutable issued profile.
    pub(crate) fn settle_provider_cutoff_usage(
        &mut self,
        agent: &mez_core::ids::AgentId,
        turn_id: &str,
        generation: u64,
        state: Option<&mez_agent::ProviderOutputLimitState>,
    ) -> bool {
        let Some(owner) = self
            .agent
            .provider_accounting_owners
            .get(&(turn_id.to_string(), generation))
        else {
            return false;
        };
        let usage = state
            .map(|state| {
                std::collections::BTreeMap::from([(
                    ModelTokenUsageKey::new(&owner.profile.provider, &owner.profile.model),
                    state.usage,
                )])
            })
            .unwrap_or_default();
        self.settle_provider_request_usage(agent, turn_id, generation, &usage)
    }

    /// Records one already-deduplicated incurred observation using frozen provenance.
    /// Pane attribution is caller-qualified; storage failures never rerun provider work.
    pub(in crate::runtime) fn record_native_usage_observation(
        &mut self,
        conversation: &str,
        pane: Option<&str>,
        origin: &AccountingOrigin,
        model: &ModelTokenUsageKey,
        usage: ModelTokenUsage,
        observation_id: String,
    ) {
        if usage.is_zero() {
            return;
        }
        // Seed aggregate-only legacy metadata before adding the new observation.
        if !self
            .agent
            .project_usage_by_conversation
            .contains_key(conversation)
        {
            let legacy = self.project_usage_for_conversation(conversation);
            self.agent
                .project_usage_by_conversation
                .insert(conversation.to_string(), legacy);
        }
        if let Some(pane) = pane
            && !self.agent.project_usage_by_pane.contains_key(pane)
        {
            let legacy = self
                .agent_token_usage_for_pane(pane)
                .into_iter()
                .map(|(model, usage)| ProjectTokenUsage {
                    project_id: None,
                    model,
                    usage,
                })
                .collect();
            self.agent
                .project_usage_by_pane
                .insert(pane.to_string(), legacy);
        }
        self.record_project_usage(conversation, pane, origin, model, usage);
        self.agent
            .agent_token_usage_by_conversation
            .entry(conversation.to_string())
            .or_default()
            .entry(model.clone())
            .or_default()
            .add_assign(usage);
        self.agent
            .agent_instance_token_usage_by_model
            .entry(model.clone())
            .or_default()
            .add_assign(usage);
        if let Some(pane) = pane {
            self.agent
                .agent_token_usage_by_pane
                .entry(pane.to_string())
                .or_default()
                .entry(model.clone())
                .or_default()
                .add_assign(usage);
        }
        if let Some(store) = self.persistence.cloned_token_usage_store() {
            let event = crate::storage::token_usage::TokenUsageEvent {
                id: observation_id,
                project: origin.project_id().cloned(),
                observed_at_unix_seconds: self.persistence.token_usage_time(),
                model: model.clone(),
                usage,
            };
            if self.persistence.token_usage_uses_adapter() {
                self.persistence.queue_token_usage(
                    crate::runtime::RuntimeSideEffect::PersistTokenUsage { store, event },
                );
            } else if store.append(&event).is_err() {
                self.persistence.record_token_usage_write_gap();
            }
        }
        let _ = self.checkpoint_agent_session_metadata();
    }

    /// Records partitions in lockstep with a cumulative accounting observation.
    pub(super) fn record_project_usage(
        &mut self,
        conversation: &str,
        pane: Option<&str>,
        origin: &AccountingOrigin,
        model: &ModelTokenUsageKey,
        usage: ModelTokenUsage,
    ) {
        let project = origin.project_id().map(|id| id.as_str().to_string());
        add_partition(
            self.agent
                .project_usage_by_conversation
                .entry(conversation.to_string())
                .or_default(),
            project.clone(),
            model,
            usage,
        );
        if let Some(pane) = pane {
            add_partition(
                self.agent
                    .project_usage_by_pane
                    .entry(pane.to_string())
                    .or_default(),
                project,
                model,
                usage,
            );
        }
    }

    /// Returns durable conversation partitions, normalizing legacy totals as unattributed.
    pub(crate) fn project_usage_for_conversation(
        &self,
        conversation: &str,
    ) -> Vec<ProjectTokenUsage> {
        self.agent
            .project_usage_by_conversation
            .get(conversation)
            .cloned()
            .unwrap_or_else(|| {
                self.agent_token_usage_for_conversation(conversation)
                    .into_iter()
                    .map(|(model, usage)| ProjectTokenUsage {
                        project_id: None,
                        model,
                        usage,
                    })
                    .collect()
            })
    }

    /// Returns pane-view partitions without database or project discovery.
    /// Returns native runtime-instance partitions independently of pane resets.
    pub(crate) fn project_usage_for_instance(&self) -> Vec<ProjectTokenUsage> {
        let mut rows = Vec::new();
        for conversation in self.agent.agent_token_usage_by_conversation.keys() {
            for row in self.project_usage_for_conversation(conversation) {
                add_partition(&mut rows, row.project_id, &row.model, row.usage);
            }
        }
        rows
    }

    /// Returns pane-view partitions without database or project discovery.
    pub(crate) fn project_usage_for_pane(&self, pane: &str) -> Vec<ProjectTokenUsage> {
        self.agent
            .project_usage_by_pane
            .get(pane)
            .cloned()
            .unwrap_or_else(|| {
                self.agent_token_usage_for_pane(pane)
                    .into_iter()
                    .map(|(model, usage)| ProjectTokenUsage {
                        project_id: None,
                        model,
                        usage,
                    })
                    .collect()
            })
    }

    /// Restores an exact pane-view snapshot after failed resume, without expense.
    pub(crate) fn restore_project_usage_for_pane(
        &mut self,
        pane: &str,
        rows: Vec<ProjectTokenUsage>,
    ) {
        self.agent
            .project_usage_by_pane
            .insert(pane.to_string(), rows);
    }

    /// Replaces restored partition state without charging inherited expense.
    pub(crate) fn restore_project_usage(
        &mut self,
        conversation: &str,
        pane: &str,
        rows: Vec<ProjectTokenUsage>,
        merge_pane: bool,
    ) {
        let rows = if rows.is_empty() {
            self.agent_token_usage_for_conversation(conversation)
                .into_iter()
                .map(|(model, usage)| ProjectTokenUsage {
                    project_id: None,
                    model,
                    usage,
                })
                .collect()
        } else {
            rows
        };
        self.agent
            .project_usage_by_conversation
            .insert(conversation.to_string(), rows.clone());
        if !merge_pane {
            self.agent
                .project_usage_by_pane
                .insert(pane.to_string(), rows);
        } else {
            let pane_rows = self
                .agent
                .project_usage_by_pane
                .entry(pane.to_string())
                .or_default();
            for row in rows {
                add_partition(pane_rows, row.project_id, &row.model, row.usage);
            }
        }
    }
}
