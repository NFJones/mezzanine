//! Project root and overlay discovery.
//!
//! Discovery keeps filesystem probing separate from trust-record storage so
//! callers can inspect projects without mutating the trust database.

use super::{BTreeMap, MezError, OVERLAY_FILENAMES, Path, PathBuf, Result};
#[cfg(test)]
use super::{ProjectTrustPrompt, ProjectTrustStore, TrustDecision, fs};

/// Runs the discover project root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn discover_project_root(start: &Path) -> PathBuf {
    let mut current = if start.is_file() {
        start.parent().unwrap_or(start).to_path_buf()
    } else {
        start.to_path_buf()
    };
    let fallback = current.clone();

    loop {
        if current.join(".git").exists() {
            return current;
        }
        if !current.pop() {
            return fallback;
        }
    }
}

/// Runs the default trust database path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn default_trust_database_path(config_root: &Path) -> PathBuf {
    config_root.join("project-trust.tsv")
}

/// Runs the discover overlay candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn discover_overlay_candidates(project_root: &Path, current_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut cursor = current_dir.to_path_buf();
    loop {
        dirs.push(cursor.clone());
        if cursor == project_root || !cursor.pop() {
            break;
        }
    }
    dirs.reverse();

    dirs.into_iter()
        .flat_map(|dir| {
            OVERLAY_FILENAMES
                .iter()
                .map(move |name| dir.join(name))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Runs the select overlay for directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn select_overlay_for_directory(existing_files: &[PathBuf]) -> Result<Option<PathBuf>> {
    let supported = existing_files
        .iter()
        .filter(|path| {
            OVERLAY_FILENAMES
                .iter()
                .any(|suffix| path.ends_with(suffix))
        })
        .cloned()
        .collect::<Vec<_>>();

    match supported.len() {
        0 => Ok(None),
        1 => Ok(supported.into_iter().next()),
        _ => Err(MezError::config(
            "multiple project overlay files found in one directory",
        )),
    }
}

/// Runs the summarize overlay capabilities operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn summarize_overlay_capabilities(overlay_files: &[PathBuf]) -> Result<Vec<String>> {
    let mut capabilities = Vec::new();
    for path in overlay_files {
        if !path.is_file() {
            continue;
        }
        let text = fs::read_to_string(path)?;
        let lower = text.to_ascii_lowercase();
        push_capability_if(
            &mut capabilities,
            lower.contains("[hooks") || lower.contains("hooks:") || lower.contains("\"hooks\""),
            "hooks",
        );
        push_capability_if(
            &mut capabilities,
            lower.contains("[mcp_servers")
                || lower.contains("mcp_servers:")
                || lower.contains("\"mcp_servers\""),
            "mcp_servers",
        );
        push_capability_if(
            &mut capabilities,
            lower.contains("command_rules")
                || lower.contains("global_command_rules")
                || lower.contains("\"command_rules\""),
            "command_rules",
        );
        push_capability_if(
            &mut capabilities,
            lower.contains("[providers")
                || lower.contains("providers:")
                || lower.contains("\"providers\""),
            "providers",
        );
        push_capability_if(
            &mut capabilities,
            lower.contains("[permissions")
                || lower.contains("permissions:")
                || lower.contains("\"permissions\""),
            "permissions",
        );
    }
    capabilities.sort();
    capabilities.dedup();
    Ok(capabilities)
}

/// Runs the discover project trust prompt operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn discover_project_trust_prompt(
    trust_store: &ProjectTrustStore,
    current_dir: &Path,
) -> Result<Option<ProjectTrustPrompt>> {
    let project_root = discover_project_root(current_dir);
    let overlay_files = discover_existing_overlays(&project_root, current_dir)?;
    if overlay_files.is_empty() {
        return Ok(None);
    }
    let git_marker_path = git_marker_path_for_project(&project_root);
    let record = trust_store.get_for_project(&project_root, git_marker_path.as_deref());
    let state = record
        .map(|record| record.state)
        .unwrap_or(TrustDecision::Pending);
    let blocks_until_primary_decision = matches!(state, TrustDecision::Pending);
    Ok(Some(ProjectTrustPrompt {
        project_root,
        state,
        capability_expansion_summary: summarize_overlay_capabilities(&overlay_files)?,
        overlay_files,
        blocks_until_primary_decision,
    }))
}

/// Returns the repository marker path for a discovered project root.
#[cfg(test)]
fn git_marker_path_for_project(project_root: &Path) -> Option<PathBuf> {
    let marker = project_root.join(".git");
    if marker.exists() { Some(marker) } else { None }
}

/// Runs the discover existing overlays operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn discover_existing_overlays(project_root: &Path, current_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut by_directory: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for candidate in discover_overlay_candidates(project_root, current_dir)
        .into_iter()
        .filter(|path| path.is_file())
    {
        if let Some(directory) = candidate.parent() {
            by_directory
                .entry(directory.to_path_buf())
                .or_default()
                .push(candidate);
        }
    }
    let mut selected = Vec::new();
    for files in by_directory.values() {
        if let Some(path) = select_overlay_for_directory(files)? {
            selected.push(path);
        }
    }
    selected.sort();
    Ok(selected)
}

/// Runs the push capability if operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
fn push_capability_if(capabilities: &mut Vec<String>, present: bool, capability: &str) {
    if present {
        capabilities.push(capability.to_string());
    }
}
