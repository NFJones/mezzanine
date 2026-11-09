//! Shell-free adapter around the shared snapshot matcher.
//!
//! Product code supplies already-authorized physical snapshots. This module
//! neither touches a filesystem nor launches a process. Each accepted operation
//! retains its ordinal and move endpoint dependency, including intermediate
//! states when several operations touch one file. Errors do not erase earlier
//! plans. Exact raw preimages remain distinct from normalized matching text.
//! Focused previews and exact changed-line statistics share one bounded raw-line
//! edit script. Display truncation never changes plans, counts or final bytes.

use std::collections::BTreeMap;

use super::work::PatchWorkControl;
use super::{
    ApplyPatchFileChange, ApplyPatchOriginalState, ApplyPatchSnapshot, ApplyPatchSnapshotState,
    apply_mez_patch_to_snapshots_with_work,
};
use crate::native_action::{NativePatchOperation, PatchEffect};
use crate::semantic_patch::{MezPatch, SemanticPatchPlanningError, SemanticPatchPlanningResult};

mod line_delta;

/// Maximum total captured bytes accepted by one pure planning invocation.
pub const NATIVE_PATCH_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
/// Maximum bytes in a single file or a proposed final file.
pub const NATIVE_PATCH_FILE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum source payload accepted before parsing/matching work.
pub const NATIVE_PATCH_SOURCE_BYTES: usize = 1024 * 1024;
/// Maximum operations retained by one ordered plan.
pub const NATIVE_PATCH_OPERATIONS: usize = 1024;
/// Maximum source bytes retained for a native unified diff.
pub const NATIVE_PATCH_DIFF_BYTES: usize = 256 * 1024;
/// Maximum retained intermediate preimage/final bytes across ordered operations.
pub const NATIVE_PATCH_PLAN_BYTES: usize = 64 * 1024 * 1024;

/// One ordered operation with dependent move endpoints kept together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePlannedOperation {
    /// Zero-based parser operation identity, not a sorted path index.
    ordinal: usize,
    /// Original semantic intent, even when the operation is a no-op.
    effect: PatchEffect,
    /// Actual changed endpoints; a move destination precedes source deletion.
    changes: Vec<ApplyPatchFileChange>,
}

impl NativePlannedOperation {
    /// Returns the immutable authored operation identity.
    pub fn ordinal(&self) -> usize {
        self.ordinal
    }
    /// Returns the accepted semantic intent without mutable access.
    pub fn effect(&self) -> &PatchEffect {
        &self.effect
    }
    /// Returns immutable ordered endpoint changes, including exact preimages.
    pub fn changes(&self) -> &[ApplyPatchFileChange] {
        &self.changes
    }
}

/// Pure plan, never execution evidence or a claim that effects were committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePatchPlan {
    /// Exact semantic source/effect identity from which this plan was derived.
    operation: NativePatchOperation,
    /// Accepted operations in authored order, including semantic no-ops.
    operations: Vec<NativePlannedOperation>,
    /// Operation-specific failures; earlier plans remain usable independently.
    errors: Vec<(usize, String, String)>,
}

impl NativePatchPlan {
    /// Returns exact accepted semantics for result/commit ownership validation.
    pub fn operation(&self) -> &NativePatchOperation {
        &self.operation
    }
    /// Returns read-only accepted operations; display/execution copies cannot
    /// prune the authoritative proof of required effects.
    pub fn operations(&self) -> &[NativePlannedOperation] {
        &self.operations
    }
    /// Returns read-only planning failures, independently of presentation.
    pub fn errors(&self) -> &[(usize, String, String)] {
        &self.errors
    }
}

/// Plans an exact operation against typed snapshots using the shared matcher.
/// Requires the original caller work control and aborts the whole plan on its
/// terminal failure; ordinary independent hunk errors remain in the plan.
pub fn plan_native_patch(
    operation: &NativePatchOperation,
    snapshots: &BTreeMap<String, ApplyPatchSnapshot>,
    work: &PatchWorkControl<'_>,
) -> SemanticPatchPlanningResult<NativePatchPlan> {
    work.check()?;
    let fail = || {
        SemanticPatchPlanningError::invalid_args(
            "apply_patch: native planning resource budget exceeded",
        )
    };
    if operation.exact_patch.len() > NATIVE_PATCH_SOURCE_BYTES
        || operation.patch.operations.len() > NATIVE_PATCH_OPERATIONS
    {
        return Err(fail());
    }
    if NativePatchOperation::parse(&operation.exact_patch, None)
        .map_err(|error| SemanticPatchPlanningError::invalid_args(error.message()))?
        != *operation
    {
        return Err(SemanticPatchPlanningError::invalid_args(
            "apply_patch: semantic descriptor differs from exact payload",
        ));
    }
    let touched_paths = operation.patch.touched_paths();
    if snapshots.len() > NATIVE_PATCH_OPERATIONS * 2
        || snapshots.iter().any(|(path, snapshot)| {
            !touched_paths.contains(path)
                || snapshot.path != *path
                || path.len() > 4096
                || snapshot.resolved_path.len() > 4096
        })
    {
        return Err(fail());
    }
    let mut total = 0usize;
    for snapshot in snapshots.values() {
        if let ApplyPatchSnapshotState::Regular(bytes) = &snapshot.state {
            total = total.saturating_add(bytes.len());
            if bytes.len() > NATIVE_PATCH_FILE_BYTES || total > NATIVE_PATCH_SNAPSHOT_BYTES {
                return Err(fail());
            }
        }
    }
    let mut current = BTreeMap::new();
    for (path, snapshot) in snapshots {
        current.insert(path.clone(), copy_snapshot(snapshot, work)?);
    }
    let mut retained = 0usize;
    let mut plan = NativePatchPlan {
        operation: work.copy_native_operation(operation)?,
        operations: Vec::new(),
        errors: Vec::new(),
    };
    for (ordinal, parsed) in operation.patch.operations.iter().enumerate() {
        work.check()?;
        let one = MezPatch {
            operations: vec![work.copy_parsed_operation(parsed)?],
        };
        let touched = one.touched_paths();
        let mut subset = BTreeMap::new();
        for path in &touched {
            if let Some(snapshot) = current.get(path) {
                subset.insert(path.clone(), copy_snapshot(snapshot, work)?);
            }
        }
        let mut matched = match apply_mez_patch_to_snapshots_with_work(&one, &subset, work) {
            Ok(matched) => matched,
            Err(error) => {
                work.check()?;
                plan.errors.push((
                    ordinal,
                    touched.iter().next().cloned().unwrap_or_default(),
                    error.message().to_string(),
                ));
                continue;
            }
        };
        let effect = work.copy_effect(operation.effects.get(ordinal).ok_or_else(|| {
            SemanticPatchPlanningError::invalid_args(
                "apply_patch: inconsistent semantic operation identity",
            )
        })?)?;
        if !matched.errors.is_empty() {
            plan.errors.extend(
                matched
                    .errors
                    .into_iter()
                    .map(|(path, message)| (ordinal, path, message)),
            );
            continue;
        }
        if matched.changes.iter().any(|change| {
            change
                .final_bytes
                .as_ref()
                .is_some_and(|bytes| bytes.len() > NATIVE_PATCH_FILE_BYTES)
        }) {
            return Err(fail());
        }
        if let PatchEffect::Move { destination, .. } = &effect {
            matched
                .changes
                .sort_by_key(|change| change.path != *destination);
        }
        for change in &matched.changes {
            let original_bytes = match &change.original {
                ApplyPatchOriginalState::Regular(bytes) => bytes.len(),
                ApplyPatchOriginalState::Missing => 0,
            };
            retained = retained
                .saturating_add(original_bytes)
                .saturating_add(change.final_bytes.as_ref().map_or(0, Vec::len));
            if retained > NATIVE_PATCH_PLAN_BYTES {
                return Err(fail());
            }
        }
        for change in &matched.changes {
            let snapshot = current.get_mut(&change.path).ok_or_else(|| {
                SemanticPatchPlanningError::invalid_args("apply_patch: typed snapshot disappeared")
            })?;
            snapshot.state = match &change.final_bytes {
                Some(bytes) => ApplyPatchSnapshotState::Regular(work.copy_bytes(bytes)?),
                None => ApplyPatchSnapshotState::Missing,
            };
        }
        plan.operations.push(NativePlannedOperation {
            ordinal,
            effect,
            changes: matched.changes,
        });
    }
    work.check()?;
    Ok(plan)
}

/// Copies one exact typed snapshot through bounded original work admission.
fn copy_snapshot(
    snapshot: &ApplyPatchSnapshot,
    work: &PatchWorkControl<'_>,
) -> SemanticPatchPlanningResult<ApplyPatchSnapshot> {
    work.check()?;
    let state = match &snapshot.state {
        ApplyPatchSnapshotState::Regular(bytes) => {
            ApplyPatchSnapshotState::Regular(work.copy_bytes(bytes)?)
        }
        state => state.clone(),
    };
    Ok(ApplyPatchSnapshot {
        path: snapshot.path.clone(),
        resolved_path: snapshot.resolved_path.clone(),
        state,
    })
}

/// Bounded native display source; truncation never changes the semantic plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeUnifiedDiff {
    /// Deterministic unified diff source, bounded at a UTF-8 boundary.
    pub source: String,
    /// True when source exceeded its display budget.
    pub truncated: bool,
    /// Full original raw line count, computed cooperatively before publication.
    pub old_lines: usize,
    /// Full final raw line count, independent of display truncation.
    pub new_lines: usize,
    /// Actual inserted raw lines, independent of preview size/truncation.
    pub added_lines: usize,
    /// Actual deleted raw lines, independent of preview size/truncation.
    pub removed_lines: usize,
}

/// Generates exact three-line-context unified hunks without external utilities.
/// Labels are rendered inertly; oversized/invalid text returns a diagnostic.
/// The caller caps aggregate diff retention and publishes only after commit.
pub fn native_unified_diff(
    change: &ApplyPatchFileChange,
    requested_budget: usize,
    work: &PatchWorkControl<'_>,
) -> SemanticPatchPlanningResult<NativeUnifiedDiff> {
    work.check()?;
    let old = match &change.original {
        ApplyPatchOriginalState::Regular(bytes) => bytes.as_slice(),
        ApplyPatchOriginalState::Missing => &[],
    };
    let new = change.final_bytes.as_deref().unwrap_or_default();
    if old.len() > NATIVE_PATCH_FILE_BYTES
        || new.len() > NATIVE_PATCH_FILE_BYTES
        || change.path.len() > 4096
    {
        return Err(SemanticPatchPlanningError::invalid_args(
            "apply_patch: native diff input budget exceeded",
        ));
    }
    line_delta::diff(change, old, new, requested_budget, work)
}
