//! Instruction discovery command planning.
//!
//! Planning validates absolute paths and safe file names, then emits one shell
//! command that discovers project instructions from the task directory upward.

use std::path::{Path, PathBuf};

use crate::shell::shell_quote;

use super::error::{InstructionDiscoveryError, InstructionDiscoveryResult};
use super::types::{InstructionDiscoveryConfig, InstructionDiscoveryPlan};

/// Builds a shell execution plan for discovering instruction files.
///
/// Returns invalid-arguments errors when paths are not absolute, the task path
/// is outside the project root, no file names are configured, the byte limit is
/// zero, or any configured instruction filename is not a plain filename.
pub fn plan_instruction_discovery(
    project_root: impl Into<PathBuf>,
    task_path: impl Into<PathBuf>,
    config: &InstructionDiscoveryConfig,
) -> InstructionDiscoveryResult<InstructionDiscoveryPlan> {
    if config.project_filenames.is_empty() {
        return Err(InstructionDiscoveryError::invalid_args(
            "at least one project instruction filename is required",
        ));
    }
    if config.max_bytes == 0 {
        return Err(InstructionDiscoveryError::invalid_args(
            "instruction discovery max bytes must be greater than zero",
        ));
    }
    for filename in &config.project_filenames {
        validate_filename(filename)?;
    }
    let project_root = project_root.into();
    let task_path = task_path.into();
    if !project_root.is_absolute() || !task_path.is_absolute() {
        return Err(InstructionDiscoveryError::invalid_args(
            "instruction discovery paths must be absolute",
        ));
    }
    if !task_path.starts_with(&project_root) {
        return Err(InstructionDiscoveryError::invalid_args(
            "instruction discovery task path must be inside the project root",
        ));
    }
    let relative_task_dir = task_directory(&project_root, &task_path)?;
    let filenames = config
        .project_filenames
        .iter()
        .map(|name| shell_quote(name))
        .collect::<Vec<String>>()
        .join(" ");
    let hidden_guard = if config.include_hidden_directories {
        "case \"$dir\" in */.*) : ;; esac"
    } else {
        "check_dir=${dir#./}; case \"$check_dir\" in .|\"\") ;; .*|*/.*) dir=\"${dir%/*}\"; [ -z \"$dir\" ] && dir=.; continue ;; esac"
    };
    let shell_command = format!(
        "cd {root} && dir={task_dir}; while :; do {hidden_guard}; for name in {filenames}; do file=\"$dir/$name\"; if [ -f \"$file\" ]; then bytes=$(wc -c < \"$file\" | tr -d ' '); printf 'path=%s\\tscope=%s\\tbytes=%s\\ttruncated=%s\\tcontent=' \"$file\" \"$dir\" \"$bytes\" \"$( [ \"$bytes\" -gt {max_bytes} ] && printf true || printf false )\"; head -c {max_bytes} \"$file\" | sed 's/\\\\/\\\\\\\\/g; s/\\t/\\\\t/g; s/\\r/\\\\r/g; s/$/\\\\n/' | tr -d '\\n'; printf '\\n'; break; fi; done; [ \"$dir\" = \".\" ] && break; dir=\"${{dir%/*}}\"; [ -z \"$dir\" ] && dir=.; done",
        root = shell_quote(&project_root.to_string_lossy()),
        task_dir = shell_quote(&relative_task_dir),
        hidden_guard = hidden_guard,
        filenames = filenames,
        max_bytes = config.max_bytes
    );
    Ok(InstructionDiscoveryPlan {
        project_root,
        task_path,
        shell_command,
        max_bytes: config.max_bytes,
    })
}

/// Runs the task directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn task_directory(project_root: &Path, task_path: &Path) -> InstructionDiscoveryResult<String> {
    let relative = task_path.strip_prefix(project_root).map_err(|_| {
        InstructionDiscoveryError::invalid_args("task path must be inside project root")
    })?;
    let dir = if task_path.extension().is_some() {
        relative.parent().unwrap_or_else(|| Path::new(""))
    } else {
        relative
    };
    if dir.as_os_str().is_empty() {
        Ok(".".to_string())
    } else {
        Ok(format!("./{}", dir.to_string_lossy()))
    }
}

/// Runs the validate filename operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_filename(filename: &str) -> InstructionDiscoveryResult<()> {
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(InstructionDiscoveryError::invalid_args(
            "instruction filename must be a plain file name",
        ));
    }
    Ok(())
}
