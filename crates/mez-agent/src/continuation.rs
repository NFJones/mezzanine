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

/// The deterministic acceptance outcome for one provider response.
///
/// Product turn runners retain transport-specific failure summaries and repair
/// request rendering, while this contract keeps their identity and missing
/// batch decisions aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderResponseAcceptance {
    /// Accept the response and retain its request as durable turn context.
    Accept {
        /// Whether the request should replace the durable response request.
        promote_durable_request: bool,
    },
    /// Reject a response emitted by a provider other than the selected one.
    ProviderIdentityMismatch,
    /// Reject a response that omitted the required parsed MAAP action batch.
    MissingActionBatch,
}

impl ProviderResponseAcceptance {
    /// Returns the stable failure-summary stage for a rejected response.
    pub fn rejection_stage(self) -> Option<&'static str> {
        match self {
            Self::ProviderIdentityMismatch => Some("provider_identity"),
            Self::MissingActionBatch => Some("maap_missing_action_batch"),
            Self::Accept { .. } => None,
        }
    }

    /// Returns the model-visible repair diagnostic for a rejected response.
    pub fn rejection_message(self) -> Option<&'static str> {
        match self {
            Self::ProviderIdentityMismatch => {
                Some("model provider response identity does not match the selected provider")
            }
            Self::MissingActionBatch => {
                Some("provider response did not include a parsed MAAP action_batch")
            }
            Self::Accept { .. } => None,
        }
    }
}

/// Classifies the provider-independent acceptance conditions for one response.
pub fn accept_provider_response(
    selected_provider: &str,
    response_provider: &str,
    response_is_repair: bool,
    has_action_batch: bool,
) -> ProviderResponseAcceptance {
    if response_provider != selected_provider {
        return ProviderResponseAcceptance::ProviderIdentityMismatch;
    }
    if !has_action_batch {
        return ProviderResponseAcceptance::MissingActionBatch;
    }
    ProviderResponseAcceptance::Accept {
        promote_durable_request: !response_is_repair,
    }
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
        CapabilityAvailability, CapabilityRequest, ProviderResponseAcceptance,
        accept_provider_response, continuation_surface, decide_capabilities,
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

    /// Response acceptance rejects identity and MAAP-shape failures before
    /// allowing a non-repair response to become durable turn context.
    #[test]
    fn provider_response_acceptance_classifies_shared_turn_runner_guards() {
        assert_eq!(
            accept_provider_response("openai", "anthropic", false, true),
            ProviderResponseAcceptance::ProviderIdentityMismatch
        );
        assert_eq!(
            accept_provider_response("openai", "openai", false, false),
            ProviderResponseAcceptance::MissingActionBatch
        );
        assert_eq!(
            accept_provider_response("openai", "openai", false, true),
            ProviderResponseAcceptance::Accept {
                promote_durable_request: true,
            }
        );
        assert_eq!(
            accept_provider_response("openai", "openai", true, true),
            ProviderResponseAcceptance::Accept {
                promote_durable_request: false,
            }
        );
    }

    /// Rejected responses carry the same repair wording and failure stage for
    /// synchronous and asynchronous product turn-runner adapters.
    #[test]
    fn provider_response_rejections_expose_stable_diagnostics() {
        let identity = ProviderResponseAcceptance::ProviderIdentityMismatch;
        assert_eq!(identity.rejection_stage(), Some("provider_identity"));
        assert_eq!(
            identity.rejection_message(),
            Some("model provider response identity does not match the selected provider")
        );

        let missing = ProviderResponseAcceptance::MissingActionBatch;
        assert_eq!(missing.rejection_stage(), Some("maap_missing_action_batch"));
        assert_eq!(
            missing.rejection_message(),
            Some("provider response did not include a parsed MAAP action_batch")
        );
    }
}
