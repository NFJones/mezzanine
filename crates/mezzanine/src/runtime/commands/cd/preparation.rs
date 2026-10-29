//! Native-only read preparation for a literal navigation request.
//!
//! This worker uses host filesystem evidence and frozen grant declarations. It
//! selects only already trusted overlays, retains child resolution bases, and
//! bounds instruction discovery without silently dropping scopes. It never
//! changes runtime state, a process directory, project trust or permissions.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::{NativeDirectoryRead, NativeDirectoryWork};
use crate::config::{ConfigFormat, ConfigLayer, ConfigScope, compose_effective_config};
use crate::error::{MezError, Result};
use crate::runtime::config::{
    ShellMode, runtime_configured_permissions_from_config, runtime_effective_config_value,
    runtime_shell_mode_from_config,
};
use crate::security::filesystem::capability::{NativeFileCapability, NativeFilesystemRoot};
use crate::security::filesystem::{
    host_resolved_path_scopes, read_scoped_instruction_file, resolve_host_path,
};
use crate::security::project::{
    ProjectTrustProvenance, discover_existing_overlays, discover_project_root,
    resolve_project_trust_provenance,
};
use mez_agent::instructions::DiscoveredInstructionFile;
use mez_agent::permissions::{PathScopes, ResolvedPathKind, ResolvedPathObjectKind};

/// Finite aggregate resource admission; exceeding it rejects the whole command.
const MAX_INSTRUCTION_FILES: usize = 256;
/// Bounds retained overlay source text independently of instruction content.
const MAX_OVERLAY_BYTES: usize = 8 * 1024 * 1024;

/// Resolves one literal directory using only the captured agent cwd and home.
/// Tilde expansion is limited to `~` and `~/`; all other metacharacters are data.
fn requested_directory(work: &NativeDirectoryWork) -> Result<PathBuf> {
    let home = || {
        work.home
            .as_ref()
            .filter(|home| {
                home.is_absolute()
                    && home
                        .to_str()
                        .is_some_and(|text| !text.is_empty() && !text.chars().any(char::is_control))
            })
            .ok_or_else(|| {
                MezError::invalid_args(
                    "agent home directory is unavailable; supply an explicit directory",
                )
            })
    };
    match work.options.directory.as_deref() {
        None | Some("~") => Ok(home()?.clone()),
        Some(value) if value.starts_with("~/") => Ok(home()?.join(&value[2..])),
        Some(value) if value.starts_with('~') => Err(MezError::invalid_args(
            "named-user home shortcuts are unsupported",
        )),
        Some(value) => Ok(PathBuf::from(value)),
    }
}

/// Resolves target authority without granting trust or rerooting inherited paths.
fn target_scopes(
    work: &NativeDirectoryWork,
    target: &Path,
    structured: &serde_json::Value,
) -> Result<PathScopes> {
    let mut resources = runtime_configured_permissions_from_config(structured)?.resources;
    if let Some(root) = &work.config_root {
        representable(root)?;
        resources.add_managed_catalog_scopes(root);
    }
    let trust = if resources.has_user_path_scope_declarations() {
        None
    } else {
        work.trust
            .as_ref()
            .map(|store| resolve_project_trust_provenance(store, target))
    };
    let status = crate::runtime::agent::RuntimePrimaryPathScopeStatus::from_resources_and_trust(
        &resources, trust,
    );
    let primary =
        host_resolved_path_scopes(target, &status.read_scopes, &status.write_scopes, &[])?;
    let Some(child) = &work.child else {
        return Ok(primary);
    };
    let inherited = host_resolved_path_scopes(
        Path::new(&child.current_directory),
        &child.read_scopes,
        &child.write_scopes,
        &[],
    )?;
    // The target remains the resolution cwd, while authority can only narrow.
    inherited
        .intersection(&primary)
        .map_err(|error| MezError::forbidden(error.message()))
}

/// Returns exact path text, never a lossy spelling that could select another node.
fn representable(path: &Path) -> Result<&str> {
    path.to_str()
        .filter(|value| !value.chars().any(char::is_control))
        .ok_or_else(|| {
            MezError::invalid_args("navigation paths must be representable without controls")
        })
}

/// Prepares target layers using existing discovery and trust precedence. Config
/// reads reuse the descriptor-backed native file capability under daemon read
/// credentials; they are harness reads, not claims about pane-process credentials.
fn target_layers(
    work: &NativeDirectoryWork,
    target: &Path,
    scopes: &PathScopes,
) -> Result<Vec<ConfigLayer>> {
    let mut layers = work
        .layers
        .iter()
        .filter(|layer| layer.scope != ConfigScope::ProjectOverlay)
        .cloned()
        .collect::<Vec<_>>();
    let root = discover_project_root(target);
    let paths = discover_existing_overlays(&root, target)?;
    let reader = NativeFilesystemRoot::capture(std::process::id())?;
    let reader_scopes = host_resolved_path_scopes(reader.cwd(), &scopes.read_scopes, &[], &[])?;
    let mut total = 0usize;
    let count = paths.len();
    for (index, path) in paths.into_iter().enumerate() {
        let trusted = path.parent().is_some_and(|directory| {
            work.trust.as_ref().is_some_and(|store| {
                matches!(
                    resolve_project_trust_provenance(store, directory),
                    ProjectTrustProvenance::TrustedRoot(_)
                )
            })
        });
        // Untrusted overlays are never read or promoted as configuration.
        if !trusted {
            continue;
        }
        let file =
            NativeFileCapability::capture(&reader, &reader_scopes, representable(&path)?, false)?;
        let bytes = file
            .preimage()
            .ok_or_else(|| MezError::conflict("project overlay disappeared during navigation"))?;
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| MezError::invalid_args("project overlay byte budget exceeded"))?;
        if total > MAX_OVERLAY_BYTES {
            return Err(MezError::invalid_args(
                "project overlay byte budget exceeded",
            ));
        }
        let text = String::from_utf8(bytes.to_vec())
            .map_err(|_| MezError::config("project overlay is not UTF-8"))?;
        layers.push(ConfigLayer {
            name: if count == 1 {
                "project".into()
            } else {
                format!("project:{}", index + 1)
            },
            path: Some(path.clone()),
            format: ConfigFormat::from_path(&path)?,
            scope: ConfigScope::ProjectOverlay,
            trusted,
            text,
        });
    }
    Ok(layers)
}

/// Discovers one configured instruction file per eligible ancestor directory.
/// The task is known to be a directory, including names containing a dot.
fn discover_instructions(
    work: &NativeDirectoryWork,
    target: &Path,
    structured: &serde_json::Value,
) -> Result<Vec<DiscoveredInstructionFile>> {
    let root = discover_project_root(target);
    let scopes = target_scopes(work, target, structured)?;
    let project =
        host_resolved_path_scopes(target, &[representable(&root)?.to_string()], &[], &[])?;
    let scopes = scopes
        .intersection(&project)
        .map_err(|error| MezError::forbidden(error.message()))?;
    let names = match structured.pointer("/instructions/project_filenames") {
        None => vec!["AGENTS.md".to_string()],
        Some(value) => value
            .as_array()
            .ok_or_else(|| MezError::config("instructions.project_filenames must be an array"))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| MezError::config("instruction filenames must be strings"))
            })
            .collect::<Result<Vec<_>>>()?,
    };
    if names.is_empty()
        || names.iter().any(|name| {
            name.is_empty() || name.contains(['/', '\\']) || name.chars().any(char::is_control)
        })
    {
        return Err(MezError::config(
            "instruction filenames must be nonempty plain names without controls",
        ));
    }
    let hidden = structured
        .pointer("/instructions/include_hidden_directories")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mut directory = target.to_path_buf();
    let mut files = Vec::new();
    loop {
        let relative = directory
            .strip_prefix(&root)
            .map_err(|_| MezError::invalid_state("instruction walk escaped project root"))?;
        let eligible = hidden
            || relative
                .components()
                .all(|part| !part.as_os_str().as_encoded_bytes().starts_with(b"."));
        if eligible {
            for name in &names {
                let path = directory.join(name);
                if let Some(file) =
                    read_scoped_instruction_file(&scopes, representable(&path)?, &directory)?
                {
                    files.push(file);
                    if files.len() > MAX_INSTRUCTION_FILES {
                        return Err(MezError::invalid_args(
                            "instruction discovery exceeds the finite file budget",
                        ));
                    }
                    break;
                }
            }
        }
        if directory == root {
            break;
        }
        directory = directory
            .parent()
            .ok_or_else(|| MezError::invalid_state("instruction walk lost project root"))?
            .to_path_buf();
    }
    files.sort_by(|left, right| {
        left.scope_root
            .cmp(&right.scope_root)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(files)
}

/// Builds one complete prepared result or fails without changing any owner.
pub(super) fn prepare(work: &NativeDirectoryWork) -> Result<NativeDirectoryRead> {
    let requested = requested_directory(work)?;
    let evidence = resolve_host_path(&work.source_directory, representable(&requested)?)?;
    if evidence.kind != ResolvedPathKind::Existing
        || evidence.object_kind != ResolvedPathObjectKind::Directory
    {
        return Err(MezError::invalid_args(
            "/cd target must be an existing directory",
        ));
    }
    let directory = PathBuf::from(evidence.canonical_path);
    representable(&directory)?;
    rustix::fs::accessat(
        rustix::fs::CWD,
        &directory,
        rustix::fs::Access::READ_OK | rustix::fs::Access::EXEC_OK,
        rustix::fs::AtFlags::EACCESS,
    )
    .map_err(|error| {
        MezError::forbidden(format!("/cd target is not readable/searchable: {error}"))
    })?;
    let target_metadata = std::fs::symlink_metadata(&directory)?;
    let target_identity = (target_metadata.dev(), target_metadata.ino());
    if !target_metadata.is_dir() {
        return Err(MezError::conflict(
            "navigation target changed before preparation",
        ));
    }
    let base_layers = work
        .layers
        .iter()
        .filter(|layer| layer.scope != ConfigScope::ProjectOverlay)
        .cloned()
        .collect::<Vec<_>>();
    let preliminary = runtime_effective_config_value(&base_layers)?;
    let scopes = target_scopes(work, &directory, &preliminary)?;
    let layers = target_layers(work, &directory, &scopes)?;
    let structured = runtime_effective_config_value(&layers)?;
    if work
        .mode_override
        .unwrap_or(runtime_shell_mode_from_config(&structured)?)
        != ShellMode::Native
        || structured
            .pointer("/agents/enabled")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
    {
        return Err(MezError::config(
            "target configuration would disable native agent navigation",
        ));
    }
    crate::runtime::config::instruction_context_max_bytes_from_config(&structured)?;
    let effective = compose_effective_config(&layers)?;
    let instructions = discover_instructions(work, &directory, &structured)?;
    let metadata = std::fs::symlink_metadata(&directory)?;
    if !metadata.is_dir()
        || (metadata.dev(), metadata.ino()) != target_identity
        || resolve_host_path(&work.source_directory, representable(&directory)?)?.canonical_path
            != representable(&directory)?
    {
        return Err(MezError::conflict(
            "navigation target changed during preparation",
        ));
    }
    Ok(NativeDirectoryRead {
        directory,
        instructions,
        layers,
        effective,
        structured,
        source: work.clone(),
        target_identity,
    })
}
