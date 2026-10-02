//! Shell-free adapter around the shared snapshot matcher.
//!
//! Product code supplies already-authorized physical snapshots. This module
//! neither touches a filesystem nor launches a process. Each accepted operation
//! retains its ordinal and move endpoint dependency, including intermediate
//! states when several operations touch one file. Errors do not erase earlier
//! plans. Exact raw preimages remain distinct from normalized matching text.
//! Diffs are bounded linear full-file unified hunks, not minimal edit scripts:
//! this avoids quadratic diff work without an additional dependency. Display
//! truncation never changes authoritative plans or final bytes.

use std::collections::BTreeMap;

use super::{
    ApplyPatchFileChange, ApplyPatchOriginalState, ApplyPatchSnapshot, ApplyPatchSnapshotState,
    apply_mez_patch_to_snapshots,
};
use crate::native_action::{NativePatchOperation, PatchEffect};
use crate::semantic_patch::{MezPatch, SemanticPatchPlanningError, SemanticPatchPlanningResult};

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
/// Rejects bounded-resource violations before returning any oversized plan.
pub fn plan_native_patch(
    operation: &NativePatchOperation,
    snapshots: &BTreeMap<String, ApplyPatchSnapshot>,
) -> SemanticPatchPlanningResult<NativePatchPlan> {
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
    let mut current = snapshots.clone();
    let mut retained = 0usize;
    let mut plan = NativePatchPlan {
        operation: operation.clone(),
        operations: Vec::new(),
        errors: Vec::new(),
    };
    for (ordinal, parsed) in operation.patch.operations.iter().enumerate() {
        let one = MezPatch {
            operations: vec![parsed.clone()],
        };
        let touched = one.touched_paths();
        let subset = touched
            .iter()
            .filter_map(|path| {
                current
                    .get(path)
                    .map(|snapshot| (path.clone(), snapshot.clone()))
            })
            .collect();
        let mut matched = match apply_mez_patch_to_snapshots(&one, &subset) {
            Ok(matched) => matched,
            Err(error) => {
                plan.errors.push((
                    ordinal,
                    touched.iter().next().cloned().unwrap_or_default(),
                    error.message().to_string(),
                ));
                continue;
            }
        };
        let effect = operation
            .effects
            .get(ordinal)
            .ok_or_else(|| {
                SemanticPatchPlanningError::invalid_args(
                    "apply_patch: inconsistent semantic operation identity",
                )
            })?
            .clone();
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
                Some(bytes) => ApplyPatchSnapshotState::Regular(bytes.clone()),
                None => ApplyPatchSnapshotState::Missing,
            };
        }
        plan.operations.push(NativePlannedOperation {
            ordinal,
            effect,
            changes: matched.changes,
        });
    }
    Ok(plan)
}

/// Bounded native display source; truncation never changes the semantic plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeUnifiedDiff {
    /// Deterministic unified diff source, bounded at a UTF-8 boundary.
    pub source: String,
    /// True when source exceeded its display budget.
    pub truncated: bool,
}

/// Writes one fragment into the bounded UTF-8 source buffer.
fn append(diff: &mut NativeUnifiedDiff, fragment: &str, budget: usize) {
    if diff.truncated {
        return;
    }
    let available = budget.saturating_sub(diff.source.len());
    let mut end = fragment.len().min(available);
    while !fragment.is_char_boundary(end) {
        end -= 1;
    }
    diff.source.push_str(&fragment[..end]);
    diff.truncated = end < fragment.len();
}

/// Generates a linear full-file unified hunk without external diff utilities.
/// Labels are rendered inertly; oversized/invalid text returns a diagnostic.
/// The caller caps aggregate diff retention and publishes only after commit.
pub fn native_unified_diff(
    change: &ApplyPatchFileChange,
    requested_budget: usize,
) -> SemanticPatchPlanningResult<NativeUnifiedDiff> {
    let old = match &change.original {
        ApplyPatchOriginalState::Regular(bytes) => bytes.as_slice(),
        ApplyPatchOriginalState::Missing => &[],
    };
    let new = change.final_bytes.as_deref().unwrap_or_default();
    if old.len() > NATIVE_PATCH_FILE_BYTES || new.len() > NATIVE_PATCH_FILE_BYTES {
        return Err(SemanticPatchPlanningError::invalid_args(
            "apply_patch: native diff input budget exceeded",
        ));
    }
    let decode = |bytes| {
        std::str::from_utf8(bytes).map_err(|_| {
            SemanticPatchPlanningError::invalid_args("apply_patch: native diff requires UTF-8")
        })
    };
    let old = decode(old)?;
    let new = decode(new)?;
    let mut diff = NativeUnifiedDiff {
        source: String::new(),
        truncated: false,
    };
    if old == new
        && matches!(change.original, ApplyPatchOriginalState::Regular(_))
        && change.final_bytes.is_some()
    {
        return Ok(diff);
    }
    let budget = requested_budget.min(NATIVE_PATCH_DIFF_BYTES);
    let label = change.path.escape_default().to_string();
    let old_label = if matches!(change.original, ApplyPatchOriginalState::Missing) {
        "/dev/null".to_string()
    } else {
        format!("a/{label}")
    };
    let new_label = if change.final_bytes.is_none() {
        "/dev/null".to_string()
    } else {
        format!("b/{label}")
    };
    append(
        &mut diff,
        &format!("diff -- {label}\n--- {old_label}\n+++ {new_label}\n"),
        budget,
    );
    let old_count = old.split_inclusive('\n').count();
    let new_count = new.split_inclusive('\n').count();
    append(
        &mut diff,
        &format!(
            "@@ -{},{} +{},{} @@\n",
            usize::from(old_count > 0),
            old_count,
            usize::from(new_count > 0),
            new_count
        ),
        budget,
    );
    for (prefix, text) in [("-", old), ("+", new)] {
        for line in text.split_inclusive('\n') {
            append(&mut diff, prefix, budget);
            append(&mut diff, line, budget);
            if !line.ends_with('\n') {
                append(&mut diff, "\n\\ No newline at end of file\n", budget);
            }
            if diff.truncated {
                break;
            }
        }
    }
    Ok(diff)
}
