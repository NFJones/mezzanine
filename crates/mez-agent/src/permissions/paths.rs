//! Permissions Paths implementation.
//!
//! This module owns the permissions paths boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{Component, EffectiveCommandEffects, Path, PathBuf, PathResolutionStatus, PathScopes};

// Read/write scope and path normalization helpers.

/// Runs the writes escape scopes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn writes_escape_scopes(
    effects: &EffectiveCommandEffects,
    scopes: Option<&PathScopes>,
) -> bool {
    let Some(scopes) = scopes else {
        return false;
    };
    effects
        .writes
        .iter()
        .chain(&effects.creates)
        .chain(&effects.deletes)
        .chain(&effects.touches)
        .any(|path| !path_in_write_scope(path, scopes))
}

/// Runs the path in read scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn path_in_read_scope(path: &str, scopes: Option<&PathScopes>) -> bool {
    let Some(scopes) = scopes else {
        return true;
    };
    path_in_scopes(path, scopes, &scopes.read_scopes)
}

/// Runs the path in write scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn path_in_write_scope(path: &str, scopes: &PathScopes) -> bool {
    path_in_scopes(path, scopes, &scopes.write_scopes)
}

/// Runs the path in scopes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn path_in_scopes(path: &str, context: &PathScopes, scopes: &[String]) -> bool {
    let Some(normalized) = shell_resolved_path(path, context) else {
        return false;
    };
    scopes
        .iter()
        .map(|scope| normalize_path(&context.current_directory, scope))
        .any(|scope| path_has_prefix(&normalized, &scope))
}

/// Runs the resolved read path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolved_read_path(path: &str, scopes: Option<&PathScopes>) -> Option<String> {
    match scopes {
        Some(scopes) if path_in_read_scope(path, Some(scopes)) => shell_resolved_path(path, scopes),
        Some(_) => None,
        None => Some(path.to_string()),
    }
}

/// Runs the shell resolved path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn shell_resolved_path(path: &str, scopes: &PathScopes) -> Option<String> {
    if path.starts_with('~')
        || path.is_empty()
        || scopes.resolution_status != PathResolutionStatus::ShellResolved
    {
        return None;
    }
    let normalized = normalize_path(&scopes.current_directory, path);
    scopes
        .canonical_paths
        .get(path)
        .or_else(|| scopes.canonical_paths.get(&normalized))
        .cloned()
        .or_else(|| {
            scopes
                .canonical_paths
                .values()
                .any(|canonical| canonical == &normalized)
                .then_some(normalized)
        })
}

/// Runs the normalize path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn normalize_path(current_directory: &str, path: &str) -> String {
    let input = Path::new(path);
    let mut combined = if input.is_absolute() {
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
        combined = PathBuf::from("/");
        combined.to_string_lossy().into_owned()
    } else {
        normalized.to_string_lossy().into_owned()
    }
}

/// Runs the path has prefix operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn path_has_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}
