//! Provider-independent subagent cooperation and scope contracts.
//!
//! These values describe authority inherited by a child agent and provide the
//! default deterministic shell/patch scope enforcer. Product adapters remain
//! responsible for concrete shell and patch execution, active process state,
//! control authorization, and spawning the child environment.

use std::collections::BTreeMap;
use std::fmt;

use crate::{AgentAction, AgentActionPayload, PermissionPreset};

mod scope;

pub use scope::{DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT, DefaultSubagentScopeEnforcement};

/// Stable categories for provider-independent subagent contract failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentContractErrorKind {
    /// A spawn or registration input is malformed.
    InvalidArgs,
    /// Requested authority requires approval that was not supplied.
    Forbidden,
    /// Requested write ownership overlaps an incompatible active owner.
    Conflict,
}

/// A provider-independent subagent spawn or scope-registry failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentContractError {
    kind: SubagentContractErrorKind,
    message: String,
}

impl SubagentContractError {
    fn new(kind: SubagentContractErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the stable failure category.
    pub const fn kind(&self) -> SubagentContractErrorKind {
        self.kind
    }

    /// Returns the stable diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SubagentContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SubagentContractError {}

/// Result returned by provider-independent subagent contracts.
pub type SubagentContractResult<T> = Result<T, SubagentContractError>;

/// Product adapter used to enforce one active subagent scope.
///
/// The agent harness owns when scope checks run, while the composition crate
/// owns shell-effect classification, semantic-patch parsing, and filesystem
/// policy. Implementations return a bounded diagnostic string when product
/// classification itself fails.
pub trait SubagentScopeEnforcement: Send + Sync {
    /// Returns a user-facing violation for one shell-backed local action.
    fn shell_command_violation(
        &self,
        scope: &SubagentScopeDeclaration,
        command: &str,
    ) -> Result<Option<String>, String>;

    /// Returns a user-facing violation for one semantic patch action.
    fn apply_patch_violation(
        &self,
        scope: &SubagentScopeDeclaration,
        patch: &str,
    ) -> Result<Option<String>, String>;
}

/// Routes one local action through canonical delegated-scope enforcement.
///
/// Semantic patches are checked as patches; every other shell-backed local
/// action is checked through its already-lowered policy command. Classification
/// failures are returned unchanged for the product error adapter to project.
pub fn subagent_action_scope_violation(
    enforcement: &dyn SubagentScopeEnforcement,
    scope: &SubagentScopeDeclaration,
    action: &AgentAction,
    policy_command: &str,
) -> Result<Option<String>, String> {
    match &action.payload {
        AgentActionPayload::ApplyPatch { patch, .. } => {
            enforcement.apply_patch_violation(scope, patch)
        }
        _ => enforcement.shell_command_violation(scope, policy_command),
    }
}

#[cfg(test)]
mod scope_tests;

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

/// Normalizes safe descriptive read-only roles onto the built-in explorer.
///
/// Configured roles remain exact. Aliasing occurs only for explore-only
/// requests without write scopes so a provider's descriptive role cannot
/// accidentally gain authority.
pub fn normalize_subagent_spawn_role(
    role: &str,
    configured_role_exists: bool,
    cooperation_mode: CooperationMode,
    write_scopes: &[String],
) -> String {
    if configured_role_exists {
        return role.to_string();
    }
    if cooperation_mode == CooperationMode::ExploreOnly
        && write_scopes.is_empty()
        && matches!(
            role,
            "repo-searcher"
                | "repository-searcher"
                | "searcher"
                | "researcher"
                | "inspector"
                | "reader"
                | "scanner"
                | "finder"
        )
    {
        return "explorer".to_string();
    }
    role.to_string()
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

/// Built-in subagent roles understood by the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinSubagentRole {
    /// General-purpose role.
    Default,
    /// Write-capable implementation role.
    Worker,
    /// Read-only exploration role.
    Explorer,
}

/// Request to create a child agent with requested task-scope metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentSpawnRequest {
    /// Parent agent requesting the spawn.
    pub parent_agent_id: String,
    /// Built-in role or configured profile requested for the child.
    pub requested_role: String,
    /// Product placement hint for the child execution surface.
    pub placement: String,
    /// Requested cooperation model for shared state.
    pub cooperation_mode: CooperationMode,
    /// Whether profile defaults supplied the cooperation model.
    pub cooperation_mode_defaulted: bool,
    /// Requested child read scopes.
    pub read_scopes: Vec<String>,
    /// Whether profile defaults supplied the read scopes.
    pub read_scopes_defaulted: bool,
    /// Requested child write scopes.
    pub write_scopes: Vec<String>,
    /// Whether profile defaults supplied the write scopes.
    pub write_scopes_defaulted: bool,
    /// Initial task prompt for the child.
    pub task_prompt: String,
    /// Whether product composition should create an idle child session.
    pub skip_initial_turn: bool,
    /// Whether unrestricted writes received explicit user approval.
    pub explicit_user_approval: bool,
}

impl SubagentSpawnRequest {
    /// Validates required identity, placement, task, and authority shape.
    pub fn validate(&self) -> SubagentContractResult<()> {
        if self.parent_agent_id.is_empty()
            || self.placement.is_empty()
            || (self.task_prompt.trim().is_empty() && !self.skip_initial_turn)
        {
            return Err(SubagentContractError::new(
                SubagentContractErrorKind::InvalidArgs,
                "subagent spawn request identity, placement, and task must not be empty",
            ));
        }
        if self.cooperation_mode == CooperationMode::ExploreOnly && !self.write_scopes.is_empty() {
            return Err(SubagentContractError::new(
                SubagentContractErrorKind::InvalidArgs,
                "explore-only subagents must not declare write scopes",
            ));
        }
        if self.cooperation_mode == CooperationMode::Unrestricted && !self.explicit_user_approval {
            return Err(SubagentContractError::new(
                SubagentContractErrorKind::Forbidden,
                "unrestricted subagent writes require explicit user approval",
            ));
        }
        Ok(())
    }

    /// Returns whether explicit user approval must back this request.
    pub fn requires_user_approval(&self) -> bool {
        self.cooperation_mode.requires_explicit_user_approval()
    }
}

/// Configured subagent profile metadata and defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentProfile {
    /// Stable profile identifier.
    pub id: String,
    /// User-visible profile name.
    pub name: String,
    /// User-visible profile description.
    pub description: String,
    /// Optional developer instructions appended to the child prompt.
    pub developer_instructions: Option<String>,
    /// Optional child model-profile override.
    pub model_profile: Option<String>,
    /// Optional stricter permission preset.
    pub permission_preset: Option<PermissionPreset>,
    /// MCP servers selected for the child.
    pub mcp_servers: Vec<String>,
    /// Extra shell environment requested by the profile.
    pub shell_env: BTreeMap<String, String>,
    /// Default cooperation mode.
    pub default_cooperation_mode: Option<CooperationMode>,
    /// Default requested read scopes.
    pub default_read_scopes: Vec<String>,
    /// Default requested write scopes.
    pub default_write_scopes: Vec<String>,
}

/// Registered active write ownership for one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWriteScope {
    /// Agent holding the registration.
    pub agent_id: String,
    /// Cooperation mode used by the registration.
    pub mode: CooperationMode,
    /// Normalized path-like scope.
    pub scope: String,
    /// Optional serial lock shared by compatible writers.
    pub serial_lock: Option<String>,
}

/// Describes a requested write scope that overlaps an active registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeConflict {
    /// Agent currently holding the overlapping scope.
    pub existing_agent_id: String,
    /// Existing normalized scope.
    pub existing_scope: String,
    /// Requested normalized scope.
    pub requested_scope: String,
}

/// Registry of active write-scope ownership by agent id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeRegistry {
    active: BTreeMap<String, Vec<ActiveWriteScope>>,
}

/// Returns the stable role name passed through the subagent harness.
pub const fn builtin_role_name(role: BuiltinSubagentRole) -> &'static str {
    match role {
        BuiltinSubagentRole::Default => "default",
        BuiltinSubagentRole::Worker => "worker",
        BuiltinSubagentRole::Explorer => "explorer",
    }
}

/// Returns the baseline built-in subagent profiles.
pub fn builtin_subagent_profiles() -> BTreeMap<String, SubagentProfile> {
    [
        (
            "default",
            "General-purpose fallback agent.",
            CooperationMode::ExploreOnly,
        ),
        (
            "worker",
            "Execution-focused agent for implementation, fixing, and production changes.",
            CooperationMode::OwnedWrite,
        ),
        (
            "explorer",
            "Read-heavy codebase exploration agent.",
            CooperationMode::ExploreOnly,
        ),
    ]
    .into_iter()
    .map(|(id, description, mode)| {
        let profile = SubagentProfile {
            id: id.to_string(),
            name: id.to_string(),
            description: description.to_string(),
            developer_instructions: None,
            model_profile: None,
            permission_preset: None,
            mcp_servers: Vec::new(),
            shell_env: BTreeMap::new(),
            default_cooperation_mode: Some(mode),
            default_read_scopes: Vec::new(),
            default_write_scopes: Vec::new(),
        };
        (profile.id.clone(), profile)
    })
    .collect()
}

impl ScopeRegistry {
    /// Creates an empty active scope registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers normalized write scopes or rejects an overlap.
    pub fn register(
        &mut self,
        agent_id: impl Into<String>,
        mode: CooperationMode,
        write_scopes: &[String],
        serial_lock: Option<String>,
    ) -> SubagentContractResult<()> {
        let agent_id = agent_id.into();
        if agent_id.is_empty() {
            return Err(SubagentContractError::new(
                SubagentContractErrorKind::InvalidArgs,
                "subagent id must not be empty",
            ));
        }
        if let Some(conflict) = self
            .conflicts(mode, write_scopes, serial_lock.as_deref())
            .first()
        {
            return Err(SubagentContractError::new(
                SubagentContractErrorKind::Conflict,
                format!(
                    "write scope `{}` overlaps active scope `{}` owned by {}",
                    conflict.requested_scope, conflict.existing_scope, conflict.existing_agent_id
                ),
            ));
        }
        self.active.insert(
            agent_id.clone(),
            write_scopes
                .iter()
                .map(|scope| ActiveWriteScope {
                    agent_id: agent_id.clone(),
                    mode,
                    scope: normalize_scope(scope),
                    serial_lock: serial_lock.clone(),
                })
                .collect(),
        );
        Ok(())
    }

    /// Removes all active scopes for an agent.
    pub fn unregister(&mut self, agent_id: &str) -> bool {
        self.active.remove(agent_id).is_some()
    }

    /// Returns active scopes registered for one agent.
    pub fn active_write_scopes_for(&self, agent_id: &str) -> Vec<ActiveWriteScope> {
        self.active.get(agent_id).cloned().unwrap_or_default()
    }

    /// Returns every active write scope in deterministic agent order.
    pub fn active_write_scopes(&self) -> Vec<ActiveWriteScope> {
        self.active.values().flatten().cloned().collect()
    }

    /// Returns the number of active scope registrations.
    pub fn active_write_scope_count(&self) -> usize {
        self.active.values().map(Vec::len).sum()
    }

    /// Returns incompatible overlaps without mutating the registry.
    pub fn conflicts(
        &self,
        requested_mode: CooperationMode,
        requested_scopes: &[String],
        requested_serial_lock: Option<&str>,
    ) -> Vec<ScopeConflict> {
        if requested_mode == CooperationMode::ExploreOnly {
            return Vec::new();
        }
        let mut conflicts = Vec::new();
        for requested_scope in requested_scopes.iter().map(|scope| normalize_scope(scope)) {
            for active in self.active.values().flatten() {
                if !scopes_overlap(&requested_scope, &active.scope) {
                    continue;
                }
                if requested_mode == CooperationMode::SerialWrite
                    && active.mode == CooperationMode::SerialWrite
                    && active.serial_lock.as_deref() == requested_serial_lock
                    && requested_serial_lock.is_some()
                {
                    continue;
                }
                conflicts.push(ScopeConflict {
                    existing_agent_id: active.agent_id.clone(),
                    existing_scope: active.scope.clone(),
                    requested_scope: requested_scope.clone(),
                });
            }
        }
        conflicts
    }
}

fn normalize_scope(scope: &str) -> String {
    let mut normalized = Vec::new();
    for segment in scope.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                let _ = normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    if scope.starts_with('/') {
        format!("/{}", normalized.join("/"))
    } else if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized.join("/")
    }
}

fn scopes_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|remaining| remaining.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|remaining| remaining.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::{
        CooperationMode, ScopeRegistry, SubagentContractErrorKind, SubagentScopeDeclaration,
        SubagentSpawnRequest, builtin_subagent_profiles, normalize_subagent_spawn_role,
    };
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

    /// Verifies only unconfigured, read-only descriptive aliases normalize to
    /// explorer while configured or write-capable roles remain exact.
    #[test]
    fn spawn_role_normalization_preserves_authority_boundaries() {
        assert_eq!(
            normalize_subagent_spawn_role(
                "repo-searcher",
                false,
                CooperationMode::ExploreOnly,
                &[],
            ),
            "explorer"
        );
        assert_eq!(
            normalize_subagent_spawn_role("repo-searcher", true, CooperationMode::ExploreOnly, &[],),
            "repo-searcher"
        );
        assert_eq!(
            normalize_subagent_spawn_role(
                "repo-searcher",
                false,
                CooperationMode::OwnedWrite,
                &["src".to_string()],
            ),
            "repo-searcher"
        );
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

    fn request(mode: CooperationMode) -> SubagentSpawnRequest {
        SubagentSpawnRequest {
            parent_agent_id: "a1".to_string(),
            requested_role: "worker".to_string(),
            placement: "new-pane".to_string(),
            cooperation_mode: mode,
            cooperation_mode_defaulted: false,
            read_scopes: vec!["src".to_string()],
            read_scopes_defaulted: false,
            write_scopes: vec!["src/parser".to_string()],
            write_scopes_defaulted: false,
            task_prompt: "implement parser".to_string(),
            skip_initial_turn: false,
            explicit_user_approval: false,
        }
    }

    /// Verifies that explore-only requests reject write scopes but pass
    /// validation when the write scope list is empty.
    #[test]
    fn explore_only_must_not_write() {
        let mut request = request(CooperationMode::ExploreOnly);
        assert_eq!(
            request.validate().unwrap_err().kind(),
            SubagentContractErrorKind::InvalidArgs
        );
        request.write_scopes.clear();
        request.validate().unwrap();
    }

    /// Verifies unrestricted writes require explicit approval and report that
    /// requirement to callers.
    #[test]
    fn unrestricted_requires_user_approval() {
        let mut request = request(CooperationMode::Unrestricted);
        assert_eq!(
            request.validate().unwrap_err().kind(),
            SubagentContractErrorKind::Forbidden
        );
        request.explicit_user_approval = true;
        request.validate().unwrap();
        assert!(request.requires_user_approval());
    }

    /// Verifies write-capable requests may omit child scopes because product
    /// composition derives enforceable scope from the parent agent.
    #[test]
    fn write_capable_modes_do_not_require_child_scopes() {
        let mut request = request(CooperationMode::OwnedWrite);
        request.read_scopes.clear();
        request.write_scopes.clear();
        request.validate().unwrap();
    }

    /// Verifies overlapping owned-write scopes conflict before the second
    /// writer is registered.
    #[test]
    fn overlapping_owned_write_scopes_conflict() {
        let mut registry = ScopeRegistry::new();
        registry
            .register(
                "a2",
                CooperationMode::OwnedWrite,
                &["src".to_string()],
                None,
            )
            .unwrap();
        let error = registry
            .register(
                "a3",
                CooperationMode::OwnedWrite,
                &["src/parser".to_string()],
                None,
            )
            .unwrap_err();
        assert_eq!(error.kind(), SubagentContractErrorKind::Conflict);
    }

    /// Verifies serial-write registrations sharing the same lock may overlap
    /// because callers opted into serialized mutation.
    #[test]
    fn serial_write_scopes_can_share_same_lock() {
        let mut registry = ScopeRegistry::new();
        registry
            .register(
                "a2",
                CooperationMode::SerialWrite,
                &["src".to_string()],
                Some("lock-1".to_string()),
            )
            .unwrap();
        registry
            .register(
                "a3",
                CooperationMode::SerialWrite,
                &["src/parser".to_string()],
                Some("lock-1".to_string()),
            )
            .unwrap();
    }

    /// Verifies built-in roles keep stable names and baseline profiles.
    #[test]
    fn builtin_roles_have_stable_names() {
        assert_eq!(
            super::builtin_role_name(super::BuiltinSubagentRole::Default),
            "default"
        );
        assert_eq!(
            super::builtin_role_name(super::BuiltinSubagentRole::Worker),
            "worker"
        );
        assert_eq!(
            super::builtin_role_name(super::BuiltinSubagentRole::Explorer),
            "explorer"
        );
        let profiles = builtin_subagent_profiles();
        assert!(profiles.contains_key("default"));
        assert!(profiles.contains_key("worker"));
        assert!(profiles.contains_key("explorer"));
    }
}
