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

#[cfg(test)]
mod tests {
    use super::{ApprovalPolicy, PermissionPreset};

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
}
