//! Mezzanine patch planning, parsing, matching, and shell transaction generation.
//!
//! This module owns the deterministic patch pipeline behind the semantic
//! facade: validating model-authored Mezzanine patches, reading remote file
//! snapshots, matching hunks, producing diagnostics, and generating the shell
//! write transaction that applies verified bytes.

use super::{LocalActionKind, LocalActionPlan};
use crate::agent::maap::{is_mez_patch_payload, validate_apply_patch_payload};
use crate::agent::shell::shell_quote;
use crate::error::{MezError, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

mod matcher;
mod parser;
mod snapshot;
mod transaction;

use matcher::apply_patch_hunks_to_file;
pub use parser::try_convert_unified_diff_to_mez_patch;
use parser::{MezPatch, MezPatchOperation, parse_mez_patch};
use snapshot::{
    ApplyPatchFileChange, ApplyPatchOriginalState, ApplyPatchSnapshot, ApplyPatchTextFile,
    ensure_missing_state, ensure_regular_state, parse_apply_patch_snapshot_output,
    snapshot_text_state,
};
pub use transaction::ApplyPatchTransactionPhase;
use transaction::{
    apply_patch_write_change_command, apply_patch_write_command_prelude,
    mez_apply_patch_read_command,
};

/// Timeout for patch application actions.
///
/// Patch actions should either apply quickly or fail with a diagnostic that the
/// model can repair. Keeping them below the turn-wide shell-action timeout avoids
/// making a malformed patch look like an indefinite stalled turn.
pub(in crate::agent) const APPLY_PATCH_TIMEOUT_MS: u64 = 30 * 1000;
/// Marker that identifies the shell-backed read phase for `apply_patch`.
pub(super) const APPLY_PATCH_READ_PHASE_MARKER: &str = "__MEZ_APPLY_PATCH_READ_PHASE__";
/// Marker that identifies the shell-backed write phase for `apply_patch`.
pub(super) const APPLY_PATCH_WRITE_PHASE_MARKER: &str = "__MEZ_APPLY_PATCH_WRITE_PHASE__";
/// Marker that starts one `apply_patch` remote snapshot stream.
const APPLY_PATCH_READ_BEGIN_MARKER: &str = "__MEZ_APPLY_PATCH_READ_BEGIN__";
/// Marker that ends one `apply_patch` remote snapshot stream.
const APPLY_PATCH_READ_END_MARKER: &str = "__MEZ_APPLY_PATCH_READ_END__";
/// Marker that starts one path entry in an `apply_patch` snapshot stream.
const APPLY_PATCH_FILE_BEGIN_MARKER: &str = "__MEZ_APPLY_PATCH_FILE_BEGIN__";
/// Marker that ends one path entry in an `apply_patch` snapshot stream.
const APPLY_PATCH_FILE_END_MARKER: &str = "__MEZ_APPLY_PATCH_FILE_END__";
/// Marker that starts base64 file content in an `apply_patch` snapshot stream.
const APPLY_PATCH_CONTENT_BEGIN_MARKER: &str = "__MEZ_APPLY_PATCH_CONTENT_BEGIN__";
/// Marker that ends base64 file content in an `apply_patch` snapshot stream.
const APPLY_PATCH_CONTENT_END_MARKER: &str = "__MEZ_APPLY_PATCH_CONTENT_END__";

/// Planned per-file patch outcomes after matching hunks against snapshots.
struct ApplyPatchPlan {
    /// Verified file changes that can be applied independently.
    changes: Vec<ApplyPatchFileChange>,
    /// File-specific diagnostics for patch operations that could not be planned.
    errors: Vec<String>,
}

impl ApplyPatchPlan {
    /// Returns true when no file operation failed during planning.
    fn is_success(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Returns the shell transaction phase represented by a generated apply-patch
/// command.
///
/// # Parameters
/// - `command`: The generated shell command being inspected.
pub fn apply_patch_transaction_phase(command: &str) -> Option<ApplyPatchTransactionPhase> {
    if command.contains(APPLY_PATCH_READ_PHASE_MARKER) {
        Some(ApplyPatchTransactionPhase::Read)
    } else if command.contains(APPLY_PATCH_WRITE_PHASE_MARKER) {
        Some(ApplyPatchTransactionPhase::Write)
    } else {
        None
    }
}

/// Builds the write phase for an `apply_patch` action from a remote snapshot.
///
/// # Parameters
/// - `patch`: The model-authored Mezzanine patch block.
/// - `read_output`: The decoded shell output from the read phase.
pub fn apply_patch_write_plan_from_read_output(
    patch: &str,
    read_output: &str,
) -> Result<LocalActionPlan> {
    apply_patch_write_plan_from_read_outputs(patch, std::slice::from_ref(&read_output.to_string()))
}

/// Builds the write phase for an `apply_patch` action from multiple remote
/// snapshot read outputs.
///
/// # Parameters
/// - `patch`: The model-authored Mezzanine patch block.
/// - `read_outputs`: Decoded shell outputs from one or more read phases.
pub fn apply_patch_write_plan_from_read_outputs(
    patch: &str,
    read_outputs: &[String],
) -> Result<LocalActionPlan> {
    let patch = parse_mez_patch(patch)?;
    let mut snapshots = BTreeMap::new();
    for read_output in read_outputs {
        snapshots.extend(parse_apply_patch_snapshot_output(read_output)?);
    }
    let plan = apply_mez_patch_to_snapshots(&patch, &snapshots)?;
    mez_apply_patch_write_plan(plan)
}

/// Applies one model-authored patch directly through the host filesystem.
///
/// This helper reuses the same parser, hunk matcher, path safety checks,
/// and preimage verification as the shell-backed read/write pipeline so
/// semantic patch writes share one implementation.
pub fn apply_patch_natively(
    patch: &str,
    strip: Option<u64>,
    cwd: &Path,
    deadline: Option<Instant>,
) -> Result<()> {
    if strip.is_some() {
        return Err(MezError::invalid_args(
            "apply_patch strip is unsupported for Mezzanine patch blocks",
        ));
    }
    ensure_native_apply_patch_deadline(deadline)?;
    let effective =
        try_convert_unified_diff_to_mez_patch(patch).unwrap_or_else(|| patch.to_string());
    ensure_native_apply_patch_deadline(deadline)?;
    validate_apply_patch_payload(&effective)?;
    debug_assert!(is_mez_patch_payload(&effective));
    let patch = parse_mez_patch(&effective)?;
    ensure_native_apply_patch_deadline(deadline)?;
    let cwd = canonical_apply_patch_cwd(cwd)?;
    ensure_native_apply_patch_deadline(deadline)?;
    let snapshots = native_apply_patch_snapshots(&patch, &cwd, deadline)?;
    ensure_native_apply_patch_deadline(deadline)?;
    let plan = apply_mez_patch_to_snapshots(&patch, &snapshots)?;
    ensure_native_apply_patch_deadline(deadline)?;
    apply_native_patch_changes(&cwd, &plan.changes, deadline)?;
    if plan.is_success() {
        Ok(())
    } else {
        Err(apply_patch_planned_failure(&plan))
    }
}

fn apply_patch_planned_failure(plan: &ApplyPatchPlan) -> MezError {
    let mut lines = Vec::new();
    if !plan.changes.is_empty() {
        let paths = plan
            .changes
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("apply_patch: applied path(s): {paths}"));
    }
    lines.extend(plan.errors.iter().cloned());
    MezError::invalid_args(lines.join("\n"))
}

fn apply_patch_planned_failure_shell_lines(plan: &ApplyPatchPlan) -> String {
    let mut lines = Vec::new();
    if !plan.changes.is_empty() {
        let paths = plan
            .changes
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("apply_patch: applied path(s): {paths}"));
    }
    lines.extend(plan.errors.iter().cloned());
    let mut command = String::new();
    for line in lines {
        command.push_str("printf '%s\\n' ");
        command.push_str(&shell_quote(&line));
        command.push_str(" >&2\n");
    }
    command.push_str("exit 1\n");
    command
}

fn ensure_native_apply_patch_deadline(deadline: Option<Instant>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(MezError::invalid_state("apply_patch timed out"));
    }
    Ok(())
}

fn canonical_apply_patch_cwd(cwd: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(cwd).map_err(|error| {
        MezError::new(
            crate::error::MezErrorKind::Io,
            format!("apply_patch: failed to resolve current working directory: {error}"),
        )
    })?;
    if !canonical.is_dir() {
        return Err(MezError::invalid_args(
            "apply_patch: current working directory is not a directory",
        ));
    }
    Ok(canonical)
}

fn native_apply_patch_snapshots(
    patch: &MezPatch,
    cwd: &Path,
    deadline: Option<Instant>,
) -> Result<BTreeMap<String, ApplyPatchSnapshot>> {
    let mut snapshots = BTreeMap::new();
    for path in patch.touched_paths() {
        ensure_native_apply_patch_deadline(deadline)?;
        let snapshot = native_apply_patch_snapshot(&path, cwd)?;
        snapshots.insert(path, snapshot);
    }
    Ok(snapshots)
}

fn native_apply_patch_snapshot(path: &str, cwd: &Path) -> Result<ApplyPatchSnapshot> {
    let target = cwd.join(path);
    let resolved = native_apply_patch_resolved_path(cwd, Path::new(path))?;
    let resolved_text = resolved.to_string_lossy().into_owned();
    if !path_is_under_cwd(cwd, &resolved) {
        return Ok(ApplyPatchSnapshot::outside_cwd(
            path.to_string(),
            resolved_text,
        ));
    }
    let metadata = match fs::metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ApplyPatchSnapshot::missing(path.to_string(), resolved_text));
        }
        Err(_) => return Ok(ApplyPatchSnapshot::error(path.to_string(), resolved_text)),
    };
    if !metadata.is_file() {
        return Ok(ApplyPatchSnapshot::non_regular(
            path.to_string(),
            resolved_text,
        ));
    }
    let bytes = fs::read(&target).map_err(|error| {
        MezError::new(
            crate::error::MezErrorKind::Io,
            format!("apply_patch: failed to read file: {path}: {error}"),
        )
    })?;
    Ok(ApplyPatchSnapshot::regular(
        path.to_string(),
        resolved_text,
        bytes,
    ))
}

fn native_apply_patch_resolved_path(cwd: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    }) {
        return Err(MezError::invalid_args(format!(
            "apply_patch: unsafe patch path: {}",
            relative.display()
        )));
    }
    let target = cwd.join(relative);
    if target.exists() || fs::symlink_metadata(&target).is_ok_and(|metadata| metadata.is_symlink())
    {
        return fs::canonicalize(&target).map_err(|_| {
            MezError::invalid_args(format!(
                "apply_patch: failed to resolve path: {}",
                relative.display()
            ))
        });
    }
    let mut existing_parent = target.as_path();
    let mut missing_components = Vec::new();
    while !existing_parent.exists() {
        let Some(name) = existing_parent.file_name() else {
            break;
        };
        missing_components.push(name.to_os_string());
        let Some(parent) = existing_parent.parent() else {
            break;
        };
        existing_parent = parent;
    }
    let mut resolved = fs::canonicalize(existing_parent).map_err(|_| {
        MezError::invalid_args(format!(
            "apply_patch: failed to resolve path: {}",
            relative.display()
        ))
    })?;
    for component in missing_components.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn path_is_under_cwd(cwd: &Path, path: &Path) -> bool {
    path == cwd || path.starts_with(cwd)
}

fn apply_native_patch_changes(
    cwd: &Path,
    changes: &[ApplyPatchFileChange],
    deadline: Option<Instant>,
) -> Result<()> {
    let mut applied_paths = Vec::new();
    for change in changes {
        ensure_native_apply_patch_deadline(deadline)?;
        let prepared = prepare_native_patch_change(cwd, change)
            .map_err(|error| native_partial_apply_error(&applied_paths, error))?;
        ensure_native_apply_patch_deadline(deadline)?;
        verify_native_patch_preimage(prepared.change, &prepared.resolved)
            .and_then(|()| apply_prepared_native_patch_change(&prepared))
            .map_err(|error| native_partial_apply_error(&applied_paths, error))?;
        applied_paths.push(change.path.clone());
    }
    Ok(())
}

fn native_partial_apply_error(applied_paths: &[String], error: MezError) -> MezError {
    if applied_paths.is_empty() {
        return error;
    }
    let paths = applied_paths.join(", ");
    MezError::new(
        error.kind(),
        format!(
            "apply_patch: applied path(s): {paths}
{}",
            error.message()
        ),
    )
}

/// Resolved and pre-validated native patch change ready for mutation.
struct NativePatchPreparedChange<'a> {
    /// Original semantic change produced by the patch matcher.
    change: &'a ApplyPatchFileChange,
    /// Canonical filesystem target verified against the earlier snapshot.
    resolved: PathBuf,
}

/// Resolves and verifies one native patch target against its snapshot.
fn prepare_native_patch_change<'a>(
    cwd: &Path,
    change: &'a ApplyPatchFileChange,
) -> Result<NativePatchPreparedChange<'a>> {
    let resolved = native_apply_patch_resolved_path(cwd, Path::new(&change.path))?;
    let expected = PathBuf::from(&change.resolved_path);
    if !path_is_under_cwd(cwd, &resolved) {
        return Err(MezError::invalid_args(format!(
            "apply_patch: resolved path is outside current working directory: {}",
            change.path
        )));
    }
    if resolved != expected {
        return Err(MezError::invalid_args(format!(
            "apply_patch: resolved path changed before apply: {}",
            change.path
        )));
    }
    verify_native_patch_preimage(change, &resolved)?;
    Ok(NativePatchPreparedChange { change, resolved })
}

/// Applies one prepared native patch change.
fn apply_prepared_native_patch_change(change: &NativePatchPreparedChange<'_>) -> Result<()> {
    match &change.change.final_bytes {
        Some(bytes) => write_native_patch_bytes(&change.resolved, bytes),
        None => fs::remove_file(&change.resolved).map_err(|error| {
            MezError::new(
                crate::error::MezErrorKind::Io,
                format!(
                    "apply_patch: failed to delete file: {}: {error}",
                    change.change.path
                ),
            )
        }),
    }
}

fn verify_native_patch_preimage(change: &ApplyPatchFileChange, resolved: &Path) -> Result<()> {
    match &change.original {
        ApplyPatchOriginalState::Regular(bytes) => {
            let metadata = fs::metadata(resolved).map_err(|_| {
                MezError::invalid_args(format!(
                    "apply_patch: refusing to patch non-regular file: {}",
                    change.path
                ))
            })?;
            if !metadata.is_file() {
                return Err(MezError::invalid_args(format!(
                    "apply_patch: refusing to patch non-regular file: {}",
                    change.path
                )));
            }
            let current = fs::read(resolved).map_err(|error| {
                MezError::new(
                    crate::error::MezErrorKind::Io,
                    format!("apply_patch: failed to read file: {}: {error}", change.path),
                )
            })?;
            if current != *bytes {
                return Err(MezError::invalid_args(format!(
                    "apply_patch: file changed before apply: {}",
                    change.path
                )));
            }
            Ok(())
        }
        ApplyPatchOriginalState::Missing => {
            if resolved.exists() || fs::symlink_metadata(resolved).is_ok() {
                return Err(MezError::invalid_args(format!(
                    "apply_patch: refusing to add existing path: {}",
                    change.path
                )));
            }
            Ok(())
        }
    }
}

fn write_native_patch_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = native_patch_temp_path(path);
    fs::write(&temp, bytes)?;
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        error.into()
    })
}

fn native_patch_temp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "patch".to_string());
    path.with_file_name(format!(".{name}.mez-{nanos}.tmp"))
}

/// Returns the sorted relative paths touched by one Mezzanine patch.
///
/// # Parameters
/// - `patch`: The model-authored Mezzanine patch block to inspect.
pub fn apply_patch_touched_paths(patch: &str) -> Result<Vec<String>> {
    Ok(parse_mez_patch(patch)?
        .touched_paths()
        .into_iter()
        .collect())
}

/// Builds a read-phase shell action that snapshots only the provided paths.
///
/// # Parameters
/// - `paths`: Relative paths from the parsed patch to snapshot in one shell
///   transaction.
pub fn apply_patch_read_plan_for_paths(paths: &BTreeSet<String>) -> LocalActionPlan {
    LocalActionPlan {
        kind: LocalActionKind::ApplyPatch,
        summary: "I’ll apply a patch.".to_string(),
        command: mez_apply_patch_read_command(paths),
        policy_command: "apply_patch".to_string(),
        interactive: false,
        stateful: false,
        timeout_ms: Some(APPLY_PATCH_TIMEOUT_MS),
        display_output_after_completion: true,
    }
}

/// Builds a write-phase shell action that reports one deterministic
/// `apply_patch` error.
///
/// # Parameters
/// - `message`: The diagnostic to show to the model and user.
pub fn apply_patch_error_plan(message: &str) -> LocalActionPlan {
    let message = message.strip_prefix("apply_patch: ").unwrap_or(message);
    LocalActionPlan {
        kind: LocalActionKind::ApplyPatch,
        summary: "I’ll apply a patch.".to_string(),
        command: format!(
            "# {APPLY_PATCH_WRITE_PHASE_MARKER}\nprintf '%s\\n' {} >&2\nexit 1",
            shell_quote(&format!("apply_patch: {message}"))
        ),
        policy_command: "apply_patch".to_string(),
        interactive: false,
        stateful: false,
        timeout_ms: Some(APPLY_PATCH_TIMEOUT_MS),
        display_output_after_completion: true,
    }
}

pub(super) fn apply_patch_plan(patch: &str, strip: Option<u64>) -> Result<LocalActionPlan> {
    let effective =
        try_convert_unified_diff_to_mez_patch(patch).unwrap_or_else(|| patch.to_string());
    validate_apply_patch_payload(&effective)?;
    debug_assert!(is_mez_patch_payload(&effective));
    mez_apply_patch_read_plan(&effective, strip)
}

fn mez_apply_patch_read_plan(patch: &str, strip: Option<u64>) -> Result<LocalActionPlan> {
    if strip.is_some() {
        return Err(MezError::invalid_args(
            "apply_patch strip is unsupported for Mezzanine patch blocks",
        ));
    }
    let patch = parse_mez_patch(patch)?;
    Ok(apply_patch_read_plan_for_paths(&patch.touched_paths()))
}

fn mez_apply_patch_write_plan(plan: ApplyPatchPlan) -> Result<LocalActionPlan> {
    if plan.changes.is_empty() && !plan.errors.is_empty() {
        return Err(apply_patch_planned_failure(&plan));
    }
    let mut command = String::from("# ");
    command.push_str(APPLY_PATCH_WRITE_PHASE_MARKER);
    command.push('\n');
    command.push_str(&apply_patch_write_command_prelude());
    for (index, change) in plan.changes.iter().enumerate() {
        command.push_str(&apply_patch_write_change_command(index, change));
    }
    if !plan.errors.is_empty() {
        command.push_str(&apply_patch_planned_failure_shell_lines(&plan));
    }
    Ok(LocalActionPlan {
        kind: LocalActionKind::ApplyPatch,
        summary: "I’ll apply a patch.".to_string(),
        command,
        policy_command: "apply_patch".to_string(),
        interactive: false,
        stateful: false,
        timeout_ms: Some(APPLY_PATCH_TIMEOUT_MS),
        display_output_after_completion: true,
    })
}

fn apply_patch_parse_error<T>(message: &str) -> Result<T> {
    Err(MezError::invalid_args(format!("apply_patch: {message}")))
}

fn apply_mez_patch_to_snapshots(
    patch: &MezPatch,
    snapshots: &BTreeMap<String, ApplyPatchSnapshot>,
) -> Result<ApplyPatchPlan> {
    let mut current = BTreeMap::new();
    let mut original = BTreeMap::new();
    for (path, snapshot) in snapshots {
        let state = snapshot_text_state(snapshot)?;
        original.insert(path.clone(), state.clone());
        current.insert(path.clone(), state);
    }
    let mut errors = Vec::new();
    for operation in &patch.operations {
        if let Err(error) = apply_mez_patch_operation_to_current(operation, &mut current) {
            errors.push(error.message().to_string());
        }
    }
    let mut changes = Vec::new();
    for path in patch.touched_paths() {
        let snapshot = snapshots.get(&path).ok_or_else(|| {
            MezError::invalid_args(format!(
                "apply_patch: missing remote snapshot for path: {path}"
            ))
        })?;
        let original_state = match original.get(&path).cloned().flatten() {
            Some(file) => ApplyPatchOriginalState::Regular(file.into_bytes()),
            None => ApplyPatchOriginalState::Missing,
        };
        let final_bytes = current
            .get(&path)
            .cloned()
            .flatten()
            .map(|file| file.into_bytes());
        let unchanged = match (&original_state, &final_bytes) {
            (ApplyPatchOriginalState::Regular(original), Some(final_bytes)) => {
                original == final_bytes
            }
            (ApplyPatchOriginalState::Missing, None) => true,
            _ => false,
        };
        if unchanged {
            continue;
        }
        changes.push(ApplyPatchFileChange {
            path,
            resolved_path: snapshot.resolved_path.clone(),
            original: original_state,
            final_bytes,
        });
    }
    Ok(ApplyPatchPlan { changes, errors })
}

fn apply_mez_patch_operation_to_current(
    operation: &MezPatchOperation,
    current: &mut BTreeMap<String, Option<ApplyPatchTextFile>>,
) -> Result<()> {
    match operation {
        MezPatchOperation::Add { path, content } => {
            ensure_missing_state(path, current.get(path))?;
            current.insert(
                path.clone(),
                Some(ApplyPatchTextFile::from_lines(content.clone(), true)),
            );
        }
        MezPatchOperation::Delete { path } => {
            ensure_regular_state(path, current.get(path))?;
            current.insert(path.clone(), None);
        }
        MezPatchOperation::Update {
            path,
            move_to,
            hunks,
            trailing_newline,
        } => {
            let mut file = ensure_regular_state(path, current.get(path))?.clone();
            if let Some(value) = trailing_newline {
                file.trailing_newline = *value;
            }
            file = apply_patch_hunks_to_file(path, file, hunks)?;
            if let Some(target) = move_to {
                ensure_missing_state(target, current.get(target))?;
                current.insert(path.clone(), None);
                current.insert(target.clone(), Some(file));
            } else {
                current.insert(path.clone(), Some(file));
            }
        }
    }
    Ok(())
}
