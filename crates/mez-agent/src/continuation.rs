//! Provider-independent capability-continuation decisions.
//!
//! This module decides how an agent action surface changes after a model
//! requests a coarse capability. Product adapters retain request rendering,
//! configuration lookup, and provider transport ownership.

use crate::{AgentCapability, AllowedActionSet, ModelInteractionKind};

/// One model-requested coarse capability and its task-specific reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRequest {
    /// Capability requested for the next provider interaction.
    pub capability: AgentCapability,
    /// Model-authored reason for requesting the capability.
    pub reason: String,
}

/// Runtime facts used to make deterministic capability-surface decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityAvailability {
    /// Whether at least one MCP tool is available to the model.
    pub mcp_available: bool,
    /// Whether persistent memory actions are enabled.
    pub memory_enabled: bool,
    /// Whether local issue actions are enabled.
    pub issues_enabled: bool,
}

/// The action-surface outcome of one capability request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityDecision {
    /// Capability selected by the model.
    pub capability: AgentCapability,
    /// Whether the capability may be exposed on the next interaction.
    pub granted: bool,
    /// Newly available actions when the capability is granted.
    pub allowed_actions: AllowedActionSet,
    /// Stable controller-visible explanation of the decision.
    pub reason: String,
}

/// Computes deterministic decisions for each requested capability.
pub fn decide_capabilities(
    requests: &[CapabilityRequest],
    availability: CapabilityAvailability,
) -> Vec<CapabilityDecision> {
    requests
        .iter()
        .map(|request| CapabilityDecision {
            capability: request.capability,
            ..capability_decision(request.capability, availability)
        })
        .collect()
}

/// Produces the next interaction kind and allowed-action surface after a
/// capability decision phase.
pub fn continuation_surface(
    interaction_kind: ModelInteractionKind,
    current_actions: &AllowedActionSet,
    decisions: &[CapabilityDecision],
) -> (ModelInteractionKind, AllowedActionSet) {
    let carried_execution_surface = interaction_kind == ModelInteractionKind::ActionExecution;
    let mut actions = if carried_execution_surface {
        current_actions.clone()
    } else {
        AllowedActionSet::action_execution_base()
    };
    for decision in decisions {
        if decision.granted {
            actions.extend_set(&decision.allowed_actions);
        }
    }
    if carried_execution_surface || decisions.iter().any(|decision| decision.granted) {
        (ModelInteractionKind::ActionExecution, actions)
    } else {
        (
            ModelInteractionKind::CapabilityDecision,
            AllowedActionSet::capability_decision(),
        )
    }
}

fn capability_decision(
    capability: AgentCapability,
    availability: CapabilityAvailability,
) -> CapabilityDecision {
    let (granted, allowed_actions, reason) = match capability {
        AgentCapability::Mcp if !availability.mcp_available => (
            false,
            AllowedActionSet::capability_decision(),
            "mcp capability requires at least one available MCP tool in runtime context"
                .to_string(),
        ),
        AgentCapability::Memory if !availability.memory_enabled => (
            false,
            AllowedActionSet::capability_decision(),
            "memory capability requires persistent memory to be enabled in runtime config"
                .to_string(),
        ),
        AgentCapability::Issues if !availability.issues_enabled => (
            false,
            AllowedActionSet::capability_decision(),
            "issues capability requires local issue tracking to be enabled in runtime config"
                .to_string(),
        ),
        _ => (
            true,
            AllowedActionSet::for_capability(capability),
            "capability is permitted by deterministic action-surface rules".to_string(),
        ),
    };
    CapabilityDecision {
        capability,
        granted,
        allowed_actions,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CapabilityAvailability, CapabilityRequest, continuation_surface, decide_capabilities,
    };
    use crate::{AgentCapability, AllowedAction, AllowedActionSet, ModelInteractionKind};

    /// Capability decisions deny unavailable stateful integrations while
    /// preserving deterministic reasons for the product continuation adapter.
    #[test]
    fn capability_decisions_reject_unavailable_integrations() {
        let decisions = decide_capabilities(
            &[CapabilityRequest {
                capability: AgentCapability::Mcp,
                reason: "inspect integration state".to_string(),
            }],
            CapabilityAvailability {
                mcp_available: false,
                memory_enabled: true,
                issues_enabled: true,
            },
        );

        assert!(!decisions[0].granted);
        assert_eq!(
            decisions[0].reason,
            "mcp capability requires at least one available MCP tool in runtime context"
        );
    }

    /// A granted capability advances a capability decision into an executable
    /// action surface without requiring product request or provider types.
    #[test]
    fn continuation_surface_exposes_granted_capability_actions() {
        let decisions = decide_capabilities(
            &[CapabilityRequest {
                capability: AgentCapability::Shell,
                reason: "inspect the repository".to_string(),
            }],
            CapabilityAvailability {
                mcp_available: true,
                memory_enabled: true,
                issues_enabled: true,
            },
        );
        let (interaction, actions) = continuation_surface(
            ModelInteractionKind::CapabilityDecision,
            &AllowedActionSet::capability_decision(),
            &decisions,
        );

        assert_eq!(interaction, ModelInteractionKind::ActionExecution);
        assert!(actions.contains(AllowedAction::ShellCommand));
        assert!(actions.contains(AllowedAction::ApplyPatch));
    }
}
