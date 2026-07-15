//! Agent-facing permission identity contracts.
//!
//! This module owns the stable permission preset and approval-policy values
//! used by agent planning and status presentation. Product command rules,
//! path scopes, approval persistence, and enforcement remain in Mezzanine.

/// Selects the baseline permission rule set exposed to an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionPreset {
    /// Restricts actions to the read-only baseline.
    ReadOnly,
    /// Uses the product's automatic command classification policy.
    Auto,
}

impl PermissionPreset {
    /// Returns the stable configuration and presentation name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Auto => "auto",
        }
    }
}

/// Selects how fresh approval prompts are handled for an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Requires an explicit decision for actions that prompt.
    Ask,
    /// Allows eligible prompting actions to proceed automatically.
    AutoAllow,
    /// Treats prompting actions as allowed without interaction.
    FullAccess,
}

impl ApprovalPolicy {
    /// Returns the stable configuration and presentation name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::AutoAllow => "auto-allow",
            Self::FullAccess => "full-access",
        }
    }
}

/// Describes the ordered result of product permission enforcement.
///
/// Variant order is significant: combining decisions with `min` preserves
/// the most restrictive result (`Forbid`, then `Prompt`, then `Allow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuleDecision {
    /// The action must not proceed.
    Forbid,
    /// The action requires an approval decision.
    Prompt,
    /// The action may proceed without a fresh approval.
    Allow,
}

/// Bounded policy view used while planning permission-sensitive agent actions.
///
/// The agent harness owns when permission decisions are requested and how the
/// resulting approval mode affects action metadata. Implementations bind a
/// canonical permission policy to the approval and path-scope state active for
/// one planning request.
pub trait PermissionPlanning: Send + Sync {
    /// Returns the effective decision for one shell-shaped policy command.
    fn evaluate_command(&self, command: &str) -> RuleDecision;

    /// Returns the active approval policy used for prompt-gate behavior.
    fn approval_policy(&self) -> ApprovalPolicy;

    /// Returns whether the product currently bypasses fresh approvals.
    fn approval_bypass(&self) -> bool;
}

/// Bounded permission state shown by agent-shell status commands.
///
/// Product command rules, path scopes, approval persistence, and enforcement
/// remain outside the agent harness. This summary contains only the scalar
/// values needed for user-visible permission and approval displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentShellPermissionSummary {
    /// Active baseline permission preset.
    pub preset: PermissionPreset,
    /// Active approval policy.
    pub approval_policy: ApprovalPolicy,
    /// Whether product policy currently bypasses fresh approval prompts.
    pub approval_bypass: bool,
    /// Number of configured product command rules.
    pub command_rule_count: usize,
}

#[cfg(test)]
mod tests {
    use super::{AgentShellPermissionSummary, ApprovalPolicy, PermissionPreset, RuleDecision};

    /// Verifies permission identities retain the stable names consumed by
    /// configuration, agent-shell status, and model-facing diagnostics.
    #[test]
    fn permission_identities_have_stable_names() {
        assert_eq!(PermissionPreset::ReadOnly.as_str(), "read-only");
        assert_eq!(PermissionPreset::Auto.as_str(), "auto");
        assert_eq!(ApprovalPolicy::Ask.as_str(), "ask");
        assert_eq!(ApprovalPolicy::AutoAllow.as_str(), "auto-allow");
        assert_eq!(ApprovalPolicy::FullAccess.as_str(), "full-access");
    }

    /// Verifies permission decisions retain their restrictive ordering because
    /// product policy aggregation selects the minimum matching decision.
    #[test]
    fn permission_decisions_are_ordered_most_restrictive_first() {
        assert!(RuleDecision::Forbid < RuleDecision::Prompt);
        assert!(RuleDecision::Prompt < RuleDecision::Allow);
        assert_eq!(
            RuleDecision::Allow.min(RuleDecision::Prompt),
            RuleDecision::Prompt
        );
        assert_eq!(
            RuleDecision::Prompt.min(RuleDecision::Forbid),
            RuleDecision::Forbid
        );
    }

    #[test]
    /// Verifies the shell summary carries only bounded display state rather
    /// than product command rules, scopes, stores, or enforcement services.
    fn agent_shell_permission_summary_preserves_display_fields() {
        let summary = AgentShellPermissionSummary {
            preset: PermissionPreset::Auto,
            approval_policy: ApprovalPolicy::AutoAllow,
            approval_bypass: true,
            command_rule_count: 7,
        };

        assert_eq!(summary.preset.as_str(), "auto");
        assert_eq!(summary.approval_policy.as_str(), "auto-allow");
        assert!(summary.approval_bypass);
        assert_eq!(summary.command_rule_count, 7);
    }
}
