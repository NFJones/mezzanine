//! Default deterministic enforcement for declared subagent scopes.
//!
//! Shell effects use the canonical permission classifier and semantic patches
//! use the canonical patch parser. Unknown or privileged effects fail closed.

use std::path::{Component, Path, PathBuf};

use crate::permissions::{EffectiveCommandEffects, classify_shell_command};
use crate::semantic_patch_planning::apply_patch_touched_paths;

use super::{CooperationMode, SubagentScopeDeclaration, SubagentScopeEnforcement};

/// Canonical stateless enforcer for declared subagent scope.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultSubagentScopeEnforcement;

/// Shared stateless scope enforcer used by agent turn runners.
pub static DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT: DefaultSubagentScopeEnforcement =
    DefaultSubagentScopeEnforcement;

impl SubagentScopeEnforcement for DefaultSubagentScopeEnforcement {
    fn shell_command_violation(
        &self,
        scope: &SubagentScopeDeclaration,
        command: &str,
    ) -> std::result::Result<Option<String>, String> {
        if scope.cooperation_mode == CooperationMode::Unrestricted {
            return Ok(None);
        }
        let effects = classify_shell_command(command, None).map_err(|error| error.to_string())?;
        Ok(effects
            .iter()
            .find_map(|effects| effect_violation(scope, effects)))
    }

    fn apply_patch_violation(
        &self,
        scope: &SubagentScopeDeclaration,
        patch: &str,
    ) -> std::result::Result<Option<String>, String> {
        if scope.cooperation_mode == CooperationMode::Unrestricted {
            return Ok(None);
        }
        for path in apply_patch_touched_paths(patch).map_err(|error| error.to_string())? {
            if scope.cooperation_mode == CooperationMode::ExploreOnly {
                return Ok(Some(format!(
                    "explore-only subagent cannot write path `{path}`"
                )));
            }
            if !path_in_declared_scopes(&scope.current_directory, &path, &scope.write_scopes) {
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
