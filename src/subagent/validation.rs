//! Subagent spawn validation and role naming helpers.
//!
//! Request validation lives separately from active scope state so spawn checks
//! can run before any registry mutation or pane creation occurs.

use std::path::{Component, Path, PathBuf};

use crate::agent::apply_patch_touched_paths;
use crate::error::Result;
use crate::permissions::{EffectiveCommandEffects, classify_shell_command};

use mez_agent::{CooperationMode, SubagentScopeDeclaration};

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

/// Product adapter for the agent-owned subagent scope-enforcement port.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProductSubagentScopeEnforcement;

/// Shared stateless scope-enforcement adapter used by agent turn runners.
pub static AGENT_SUBAGENT_SCOPE_ENFORCEMENT: ProductSubagentScopeEnforcement =
    ProductSubagentScopeEnforcement;

impl mez_agent::SubagentScopeEnforcement for ProductSubagentScopeEnforcement {
    fn shell_command_violation(
        &self,
        scope: &SubagentScopeDeclaration,
        command: &str,
    ) -> std::result::Result<Option<String>, String> {
        SubagentScopeEnforcement::shell_command_violation(scope, command)
            .map_err(|error| error.to_string())
    }

    fn apply_patch_violation(
        &self,
        scope: &SubagentScopeDeclaration,
        patch: &str,
    ) -> std::result::Result<Option<String>, String> {
        SubagentScopeEnforcement::apply_patch_violation(scope, patch)
            .map_err(|error| error.to_string())
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
