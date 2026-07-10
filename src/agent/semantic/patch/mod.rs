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
