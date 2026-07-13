//! Subagent spawn validation and role naming helpers.
//!
//! Request validation lives separately from active scope state so spawn checks
//! can run before any registry mutation or pane creation occurs.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use crate::agent::apply_patch_touched_paths;
use crate::error::{MezError, Result};
use crate::permissions::{EffectiveCommandEffects, classify_shell_command};

use super::types::{
    BuiltinSubagentRole, CooperationMode, SubagentProfile, SubagentScopeDeclaration,
    SubagentSpawnRequest,
};

impl SubagentSpawnRequest {
    /// Validates that the request has required identity, placement, task, and
    /// shape for the selected cooperation mode.
    ///
    /// Returns invalid-arguments errors for malformed write declarations and a
    /// forbidden error when unrestricted writes lack explicit user approval.
    pub fn validate(&self) -> Result<()> {
        if self.parent_agent_id.is_empty()
            || self.placement.is_empty()
            || (self.task_prompt.trim().is_empty() && !self.skip_initial_turn)
        {
            return Err(MezError::invalid_args(
                "subagent spawn request identity, placement, and task must not be empty",
            ));
        }
        if self.cooperation_mode == CooperationMode::ExploreOnly && !self.write_scopes.is_empty() {
            return Err(MezError::invalid_args(
                "explore-only subagents must not declare write scopes",
            ));
        }
        if self.cooperation_mode == CooperationMode::Unrestricted && !self.explicit_user_approval {
            return Err(MezError::forbidden(
                "unrestricted subagent writes require explicit user approval",
            ));
        }
        Ok(())
    }

    /// Returns true when the cooperation mode must be backed by explicit user
    /// approval.
    pub fn requires_user_approval(&self) -> bool {
        self.cooperation_mode == CooperationMode::Unrestricted
    }
}

/// Returns the stable role name passed through the subagent harness.
pub fn builtin_role_name(role: BuiltinSubagentRole) -> &'static str {
    match role {
        BuiltinSubagentRole::Default => "default",
        BuiltinSubagentRole::Worker => "worker",
        BuiltinSubagentRole::Explorer => "explorer",
    }
}

/// Returns the baseline built-in subagent profiles.
pub fn builtin_subagent_profiles() -> BTreeMap<String, SubagentProfile> {
    [
        SubagentProfile {
            id: "default".to_string(),
            name: "default".to_string(),
            description: "General-purpose fallback agent.".to_string(),
            developer_instructions: None,
            model_profile: None,
            permission_preset: None,
            mcp_servers: Vec::new(),
            shell_env: BTreeMap::new(),
            default_cooperation_mode: Some(CooperationMode::ExploreOnly),
            default_read_scopes: Vec::new(),
            default_write_scopes: Vec::new(),
        },
        SubagentProfile {
            id: "worker".to_string(),
            name: "worker".to_string(),
            description:
                "Execution-focused agent for implementation, fixing, and production changes."
                    .to_string(),
            developer_instructions: None,
            model_profile: None,
            permission_preset: None,
            mcp_servers: Vec::new(),
            shell_env: BTreeMap::new(),
            default_cooperation_mode: Some(CooperationMode::OwnedWrite),
            default_read_scopes: Vec::new(),
            default_write_scopes: Vec::new(),
        },
        SubagentProfile {
            id: "explorer".to_string(),
            name: "explorer".to_string(),
            description: "Read-heavy codebase exploration agent.".to_string(),
            developer_instructions: None,
            model_profile: None,
            permission_preset: None,
            mcp_servers: Vec::new(),
            shell_env: BTreeMap::new(),
            default_cooperation_mode: Some(CooperationMode::ExploreOnly),
            default_read_scopes: Vec::new(),
            default_write_scopes: Vec::new(),
        },
    ]
    .into_iter()
    .map(|profile| (profile.id.clone(), profile))
    .collect()
}

/// Runs the normalize scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn normalize_scope(scope: &str) -> String {
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

/// Runs the scopes overlap operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn scopes_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|remaining| remaining.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|remaining| remaining.starts_with('/'))
}

/// Product-owned enforcement for agent-owned subagent scope declarations.
///
/// Command and patch classification remain in Mezzanine because they depend
/// on product permission rules and semantic-patch parsing.
pub trait SubagentScopeEnforcement {
    /// Returns a user-facing violation message when a shell command is outside
    /// the child agent's declared subagent scope.
    ///
    /// Scope enforcement runs before normal permission approval so a human
    /// approval for a command prefix cannot silently expand a subagent's
    /// authority. Commands whose effects cannot be classified fail closed and
    /// must be returned to the parent or retried after an explicit scope
    /// expansion.
    fn shell_command_violation(&self, command: &str) -> Result<Option<String>>;

    /// Returns a user-facing violation when an `apply_patch` action touches
    /// paths outside the child agent's declared write scope.
    fn apply_patch_violation(&self, patch: &str) -> Result<Option<String>>;
}

impl SubagentScopeEnforcement for SubagentScopeDeclaration {
    fn shell_command_violation(&self, command: &str) -> Result<Option<String>> {
        if self.cooperation_mode == CooperationMode::Unrestricted {
            return Ok(None);
        }
        let effects = classify_shell_command(command, None)?;
        for effect in effects {
            if let Some(message) = effect_violation(self, &effect) {
                return Ok(Some(message));
            }
        }
        Ok(None)
    }

    fn apply_patch_violation(&self, patch: &str) -> Result<Option<String>> {
        if self.cooperation_mode == CooperationMode::Unrestricted {
            return Ok(None);
        }
        for path in apply_patch_touched_paths(patch)? {
            if self.cooperation_mode == CooperationMode::ExploreOnly {
                return Ok(Some(format!(
                    "explore-only subagent cannot write path `{path}`"
                )));
            }
            if !path_in_declared_scopes(&self.current_directory, &path, &self.write_scopes) {
                return Ok(Some(format!(
                    "subagent write path `{path}` is outside declared write scopes"
                )));
            }
        }
        Ok(None)
    }
}

/// Returns a scope violation for one product-classified command effect.
fn effect_violation(
    scope: &SubagentScopeDeclaration,
    effects: &EffectiveCommandEffects,
) -> Option<String> {
    if effects.network || effects.credentials || effects.process_control || effects.privilege_change
    {
        return Some(
                "subagent command effects are outside declared scope or cannot be classified; request scope expansion or parent execution".to_string(),
            );
    }
    for path in &effects.reads {
        if !path_in_declared_scopes(&scope.current_directory, path, &scope.read_scopes) {
            return Some(format!(
                "subagent read path `{path}` is outside declared read scopes"
            ));
        }
    }
    let write_paths = effects
        .writes
        .iter()
        .chain(&effects.creates)
        .chain(&effects.deletes)
        .chain(&effects.touches);
    for path in write_paths {
        if scope.cooperation_mode == CooperationMode::ExploreOnly {
            return Some(format!("explore-only subagent cannot write path `{path}`"));
        }
        if !path_in_declared_scopes(&scope.current_directory, path, &scope.write_scopes) {
            return Some(format!(
                "subagent write path `{path}` is outside declared write scopes"
            ));
        }
    }
    if effects.destructive && scope.cooperation_mode == CooperationMode::ExploreOnly {
        return Some("explore-only subagent cannot perform destructive actions".to_string());
    }
    None
}

/// Runs the path in declared scopes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn path_in_declared_scopes(current_directory: &str, path: &str, scopes: &[String]) -> bool {
    if scopes.is_empty() || path == "<unknown>" {
        return false;
    }
    let normalized = normalize_path(current_directory, path);
    scopes
        .iter()
        .map(|scope| normalize_path(current_directory, scope))
        .any(|scope| path_has_prefix(&normalized, &scope))
}

/// Runs the normalize path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn normalize_path(current_directory: &str, path: &str) -> String {
    let input = Path::new(path);
    let combined = if input.is_absolute() {
        input.to_path_buf()
    } else {
        PathBuf::from(current_directory).join(input)
    };

    let mut normalized = PathBuf::new();
    for component in combined.components() {
        match component {
            Component::RootDir => normalized.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::Prefix(_) => {}
        }
    }
    if normalized.as_os_str().is_empty() {
        "/".to_string()
    } else {
        normalized.to_string_lossy().into_owned()
    }
}

/// Runs the path has prefix operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn path_has_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}
