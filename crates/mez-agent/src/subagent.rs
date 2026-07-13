//! Provider-independent subagent cooperation and scope contracts.
//!
//! These values describe authority inherited by a child agent. Product-owned
//! adapters remain responsible for classifying shell commands and patches,
//! enforcing filesystem scopes, coordinating active writers, and spawning the
//! child execution environment.

use crate::PermissionPreset;

/// Declares how a subagent may interact with shared repository state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CooperationMode {
    /// The subagent may read and inspect but must not write.
    ExploreOnly,
    /// The subagent owns the declared write scopes while it is active.
    OwnedWrite,
    /// The subagent may write after explicit coordination with peers.
    CoordinatedWrite,
    /// The subagent writes under a named serial lock.
    SerialWrite,
    /// The subagent may write without scope restrictions after user approval.
    Unrestricted,
}

impl CooperationMode {
    /// Returns the stable configuration and protocol name for this mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExploreOnly => "explore-only",
            Self::OwnedWrite => "owned-write",
            Self::CoordinatedWrite => "coordinated-write",
            Self::SerialWrite => "serial-write",
            Self::Unrestricted => "unrestricted",
        }
    }

    /// Returns whether this mode requires explicit user approval.
    pub const fn requires_explicit_user_approval(self) -> bool {
        matches!(self, Self::Unrestricted)
    }
}

/// Active scope restrictions inherited from a spawned subagent's parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentScopeDeclaration {
    /// Cooperation mode constraining the child agent.
    pub cooperation_mode: CooperationMode,
    /// Pane-shell current directory used to resolve relative effect paths.
    pub current_directory: String,
    /// Declared read scopes.
    pub read_scopes: Vec<String>,
    /// Declared write scopes.
    pub write_scopes: Vec<String>,
    /// Optional stricter permission preset selected by the profile.
    pub permission_preset: Option<PermissionPreset>,
}

#[cfg(test)]
mod tests {
    use super::{CooperationMode, SubagentScopeDeclaration};
    use crate::PermissionPreset;

    /// Verifies cooperation modes retain the stable names consumed by config,
    /// runtime JSON, and agent-facing diagnostics.
    #[test]
    fn cooperation_modes_have_stable_names() {
        assert_eq!(CooperationMode::ExploreOnly.as_str(), "explore-only");
        assert_eq!(CooperationMode::OwnedWrite.as_str(), "owned-write");
        assert_eq!(
            CooperationMode::CoordinatedWrite.as_str(),
            "coordinated-write"
        );
        assert_eq!(CooperationMode::SerialWrite.as_str(), "serial-write");
        assert_eq!(CooperationMode::Unrestricted.as_str(), "unrestricted");
    }

    /// Verifies unrestricted authority is the only cooperation mode requiring
    /// explicit user approval and scope declarations retain permission policy.
    #[test]
    fn unrestricted_scope_requires_explicit_approval() {
        let scope = SubagentScopeDeclaration {
            cooperation_mode: CooperationMode::Unrestricted,
            current_directory: "/workspace".to_string(),
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            permission_preset: Some(PermissionPreset::ReadOnly),
        };

        assert!(scope.cooperation_mode.requires_explicit_user_approval());
        assert_eq!(scope.permission_preset, Some(PermissionPreset::ReadOnly));
        assert!(!CooperationMode::ExploreOnly.requires_explicit_user_approval());
    }
}
