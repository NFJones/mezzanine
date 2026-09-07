//! Mezzanine patch matching and shell transaction planning.
//!
//! This module owns deterministic interpretation of shell-produced snapshots,
//! hunk matching and diagnostics, and shell read/write transaction generation.
//! Product adapters retain pane execution and project error conversion.

use crate::semantic_patch::{
    MezPatch, MezPatchOperation, SemanticPatchPlanningError, SemanticPatchPlanningResult as Result,
    is_mez_patch_payload, parse_mez_patch, try_convert_unified_diff_to_mez_patch,
    validate_apply_patch_payload,
};
use crate::{LocalActionKind, LocalActionPlan, shell_quote};
use base64::Engine;
use std::collections::{BTreeMap, BTreeSet};

mod matcher;
mod path_resolution;
mod snapshot;
#[cfg(test)]
mod tests;
mod transaction;

use matcher::apply_patch_hunks_to_file;
use path_resolution::apply_patch_path_resolution_lines;
use snapshot::{
    ApplyPatchFileChange, ApplyPatchOriginalState, ApplyPatchSnapshot, ApplyPatchTextFile,
    ensure_missing_state, ensure_regular_state, parse_apply_patch_snapshot_output,
    snapshot_text_state,
};
pub use transaction::{
    ApplyPatchConfirmedSection, ApplyPatchProgress, ApplyPatchProgressDecoder,
    ApplyPatchTransactionPhase,
};
use transaction::{
    apply_patch_write_change_command, apply_patch_write_command_prelude, apply_patch_write_sidecar,
    mez_apply_patch_read_command,
};

/// Filesystem boundary enforced by both phases of one semantic patch action.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ApplyPatchPathBoundary {
    /// Restrict every target to the pane's canonical current directory.
    #[default]
    CurrentDirectoryOnly,
    /// Restrict every target to the supplied canonical sandbox write scopes.
    SandboxWriteScopes(Vec<String>),
}

/// Timeout for patch application actions.
///
/// Patch actions should either apply quickly or fail with a diagnostic that the
/// model can repair. Keeping them below the turn-wide shell-action timeout avoids
/// making a malformed patch look like an indefinite stalled turn.
pub const APPLY_PATCH_TIMEOUT_MS: u64 = 30 * 1000;
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
/// Prefix for one machine-readable per-file write outcome.
pub const APPLY_PATCH_RESULT_MARKER: &str = "__MEZ_APPLY_PATCH_RESULT__";
/// Prefix for one length-delimited, still-unconfirmed per-file diff section.
pub const APPLY_PATCH_DIFF_MARKER: &str = "__MEZ_APPLY_PATCH_DIFF__";
/// Maximum bytes retained for either one proposed diff or one framing line.
///
/// This matches the crate's model-facing action-result content ceiling. The
/// decoder retains at most one of each component, keeping total private state
/// below twice this cap without retaining previously consumed source.
pub const APPLY_PATCH_PROGRESS_MAX_RETAINED_BYTES: usize = 256 * 1024;

/// One confirmed per-file outcome emitted by an `apply_patch` write phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyPatchFileOutcome {
    /// The target was written and its unified diff was emitted.
    Applied { path: String },
    /// The target was not written and includes a bounded diagnostic.
    Failed { path: String, diagnostic: String },
}

/// Parses machine-readable per-file outcomes from one write-phase observation.
///
/// Lines without the runtime-owned marker are ignored. A malformed marked line
/// fails closed so callers can retain their generic completion behavior.
pub fn parse_apply_patch_file_outcomes(output: &str) -> Result<Vec<ApplyPatchFileOutcome>> {
    let uses_confirmed_framing = output.lines().any(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        fields.first().is_some_and(|field| {
            *field == APPLY_PATCH_DIFF_MARKER
                || (*field == APPLY_PATCH_RESULT_MARKER && fields.len() == 5)
        })
    });
    if uses_confirmed_framing {
        let mut decoder = ApplyPatchProgressDecoder::new();
        let mut progress = decoder.push(output.as_bytes())?;
        progress.extend(decoder.finish()?);
        return Ok(progress.outcomes);
    }

    let normalized = output.replace("\r\n", "\n").replace('\r', "\n");
    let mut outcomes = Vec::new();
    let mut seen_paths = BTreeSet::new();
    for line in normalized.lines() {
        let Some(record) = line.strip_prefix(APPLY_PATCH_RESULT_MARKER) else {
            continue;
        };
        let fields = record.split_whitespace().collect::<Vec<_>>();
        let decode = |encoded: &str, field: &str| -> Result<String> {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| {
                    SemanticPatchPlanningError::invalid_args(format!(
                        "apply_patch: malformed per-file result {field}"
                    ))
                })?;
            String::from_utf8(bytes).map_err(|_| {
                SemanticPatchPlanningError::invalid_args(format!(
                    "apply_patch: per-file result {field} is not UTF-8"
                ))
            })
        };
        match fields.as_slice() {
            ["APPLIED", path] => {
                let path = decode(path, "path")?;
                if !seen_paths.insert(path.clone()) {
                    return Err(SemanticPatchPlanningError::invalid_args(
                        "apply_patch: duplicate per-file result record",
                    ));
                }
                outcomes.push(ApplyPatchFileOutcome::Applied { path });
            }
            ["FAILED", path, diagnostic] => {
                let path = decode(path, "path")?;
                if !seen_paths.insert(path.clone()) {
                    return Err(SemanticPatchPlanningError::invalid_args(
                        "apply_patch: duplicate per-file result record",
                    ));
                }
                outcomes.push(ApplyPatchFileOutcome::Failed {
                    path,
                    diagnostic: decode(diagnostic, "diagnostic")?,
                });
            }
            _ => {
                return Err(SemanticPatchPlanningError::invalid_args(
                    "apply_patch: malformed per-file result record",
                ));
            }
        }
    }
    Ok(outcomes)
}

/// Planned per-file patch outcomes after matching hunks against snapshots.
struct ApplyPatchPlan {
    /// Verified file changes that can be applied independently.
    changes: Vec<ApplyPatchFileChange>,
    /// File-specific diagnostics for patch operations that could not be planned.
    errors: Vec<(String, String)>,
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
    apply_patch_write_plan_from_read_outputs_with_boundary(
        patch,
        std::slice::from_ref(&read_output.to_string()),
        &ApplyPatchPathBoundary::CurrentDirectoryOnly,
    )
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
    apply_patch_write_plan_from_read_outputs_with_boundary(
        patch,
        read_outputs,
        &ApplyPatchPathBoundary::CurrentDirectoryOnly,
    )
}

/// Builds a write phase that rechecks the same filesystem boundary used by
/// the corresponding read phases.
pub fn apply_patch_write_plan_from_read_outputs_with_boundary(
    patch: &str,
    read_outputs: &[String],
    boundary: &ApplyPatchPathBoundary,
) -> Result<LocalActionPlan> {
    let patch = parse_mez_patch(patch)?;
    let mut snapshots = BTreeMap::new();
    for read_output in read_outputs {
        snapshots.extend(parse_apply_patch_snapshot_output(read_output)?);
    }
    let plan = apply_mez_patch_to_snapshots(&patch, &snapshots)?;
    mez_apply_patch_write_plan(plan, boundary)
}

fn apply_patch_planned_failure(plan: &ApplyPatchPlan) -> SemanticPatchPlanningError {
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
    lines.extend(plan.errors.iter().map(|(_, message)| message.clone()));
    SemanticPatchPlanningError::invalid_args(lines.join("\n"))
}

fn apply_patch_planned_failure_shell_lines(
    plan: &ApplyPatchPlan,
    starting_ordinal: usize,
) -> String {
    let mut command = String::new();
    for (offset, (path, line)) in plan.errors.iter().enumerate() {
        command.push_str("printf '%s\\n' ");
        command.push_str(&shell_quote(line));
        command.push_str(" >&2\n");
        command.push_str("printf '%s %s %s %s %s\\n' ");
        command.push_str(&shell_quote(APPLY_PATCH_RESULT_MARKER));
        command.push_str(" FAILED ");
        command.push_str(&(starting_ordinal + offset).to_string());
        command.push(' ');
        command.push_str(&shell_quote(
            &base64::engine::general_purpose::STANDARD.encode(path.as_bytes()),
        ));
        command.push(' ');
        command.push_str(&shell_quote(
            &base64::engine::general_purpose::STANDARD.encode(line.as_bytes()),
        ));
        command.push('\n');
    }
    command.push_str("MEZ_APPLY_FAILED=1\n");
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
    apply_patch_read_plan_for_paths_with_boundary(
        paths,
        &ApplyPatchPathBoundary::CurrentDirectoryOnly,
    )
}

/// Builds a read phase that authorizes targets against an explicit boundary.
pub fn apply_patch_read_plan_for_paths_with_boundary(
    paths: &BTreeSet<String>,
    boundary: &ApplyPatchPathBoundary,
) -> LocalActionPlan {
    LocalActionPlan {
        kind: LocalActionKind::ApplyPatch,
        program_dialect: crate::LocalProgramDialect::PosixSh,
        summary: "I’ll apply a patch.".to_string(),
        command: mez_apply_patch_read_command(paths, boundary),
        input_sidecar: None,
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
        program_dialect: crate::LocalProgramDialect::PosixSh,
        summary: "I’ll apply a patch.".to_string(),
        command: format!(
            "# {APPLY_PATCH_WRITE_PHASE_MARKER}\nprintf '%s\\n' {} >&2\nexit 1",
            shell_quote(&format!("apply_patch: {message}"))
        ),
        input_sidecar: None,
        policy_command: "apply_patch".to_string(),
        interactive: false,
        stateful: false,
        timeout_ms: Some(APPLY_PATCH_TIMEOUT_MS),
        display_output_after_completion: true,
    }
}

pub fn apply_patch_plan(patch: &str, strip: Option<u64>) -> Result<LocalActionPlan> {
    let effective =
        try_convert_unified_diff_to_mez_patch(patch).unwrap_or_else(|| patch.to_string());
    validate_apply_patch_payload(&effective)
        .map_err(|error| SemanticPatchPlanningError::invalid_args(error.message()))?;
    debug_assert!(is_mez_patch_payload(&effective));
    mez_apply_patch_read_plan(&effective, strip)
}

fn mez_apply_patch_read_plan(patch: &str, strip: Option<u64>) -> Result<LocalActionPlan> {
    if strip.is_some() {
        return Err(SemanticPatchPlanningError::invalid_args(
            "apply_patch strip is unsupported for Mezzanine patch blocks",
        ));
    }
    let patch = parse_mez_patch(patch)?;
    Ok(apply_patch_read_plan_for_paths(&patch.touched_paths()))
}

fn mez_apply_patch_write_plan(
    plan: ApplyPatchPlan,
    boundary: &ApplyPatchPathBoundary,
) -> Result<LocalActionPlan> {
    if plan.changes.is_empty() && !plan.errors.is_empty() {
        return Err(apply_patch_planned_failure(&plan));
    }
    let mut command = String::from("# ");
    command.push_str(APPLY_PATCH_WRITE_PHASE_MARKER);
    command.push('\n');
    command.push_str(&apply_patch_write_command_prelude(boundary));
    for (index, change) in plan.changes.iter().enumerate() {
        command.push_str(&apply_patch_write_change_command(index, change));
    }
    if !plan.errors.is_empty() {
        command.push_str(&apply_patch_planned_failure_shell_lines(
            &plan,
            plan.changes.len(),
        ));
    }
    command.push_str("if [ \"${MEZ_APPLY_FAILED:-0}\" = 1 ]; then exit 1; fi\n");
    let input_sidecar = apply_patch_write_sidecar(&plan.changes);
    Ok(LocalActionPlan {
        kind: LocalActionKind::ApplyPatch,
        program_dialect: crate::LocalProgramDialect::PosixSh,
        summary: "I’ll apply a patch.".to_string(),
        command,
        input_sidecar,
        policy_command: "apply_patch".to_string(),
        interactive: false,
        stateful: false,
        timeout_ms: Some(APPLY_PATCH_TIMEOUT_MS),
        display_output_after_completion: true,
    })
}

fn apply_patch_parse_error<T>(message: &str) -> Result<T> {
    Err(SemanticPatchPlanningError::invalid_args(format!(
        "apply_patch: {message}"
    )))
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
            let path = match operation {
                MezPatchOperation::Add { path, .. }
                | MezPatchOperation::Delete { path }
                | MezPatchOperation::Update { path, .. } => path.clone(),
            };
            errors.push((path, error.message().to_string()));
        }
    }
    let mut changes = Vec::new();
    for path in patch.touched_paths() {
        let snapshot = snapshots.get(&path).ok_or_else(|| {
            SemanticPatchPlanningError::invalid_args(format!(
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
