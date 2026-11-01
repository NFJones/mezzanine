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

use super::work::{PATCH_WORK_QUANTUM, PatchWorkControl};
use super::{
    ApplyPatchFileChange, ApplyPatchOriginalState, ApplyPatchSnapshot, ApplyPatchSnapshotState,
    apply_mez_patch_to_snapshots_with_work,
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
    let old_count = line_count(old, work)?;
    let new_count = line_count(new, work)?;
    let old = work.decode_text(old)?;
    let new = work.decode_text(new)?;
    let mut diff = NativeUnifiedDiff {
        source: String::new(),
        truncated: false,
        old_lines: old_count,
        new_lines: new_count,
    };
    if work.equal(&old, &new)
        && matches!(change.original, ApplyPatchOriginalState::Regular(_))
        && change.final_bytes.is_some()
    {
        work.check()?;
        return Ok(diff);
    }
    work.check()?;
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
    for (prefix, text) in [("-", old.as_str()), ("+", new.as_str())] {
        let mut line_start = true;
        let mut last_newline = true;
        // Split at UTF-8 boundaries in <=64KiB pieces, including long lines.
        let mut offset = 0;
        while offset < text.len() && !diff.truncated {
            let mut end = text.len().min(offset + PATCH_WORK_QUANTUM);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if !work.spend(end - offset) {
                work.check()?;
            }
            for fragment in text[offset..end].split_inclusive('\n') {
                if line_start {
                    append(&mut diff, prefix, budget);
                }
                append(&mut diff, fragment, budget);
                last_newline = fragment.ends_with('\n');
                line_start = last_newline;
                if diff.truncated {
                    break;
                }
            }
            offset = end;
        }
        if !last_newline && !diff.truncated {
            append(&mut diff, "\n\\ No newline at end of file\n", budget);
        }
    }
    work.check()?;
    Ok(diff)
}

/// Counts raw newline-delimited lines through bounded actual byte scans.
fn line_count(bytes: &[u8], work: &PatchWorkControl<'_>) -> SemanticPatchPlanningResult<usize> {
    let mut count = 0;
    for chunk in bytes.chunks(PATCH_WORK_QUANTUM) {
        if !work.spend(chunk.len()) {
            work.check()?;
        }
        count += chunk.iter().filter(|byte| **byte == b'\n').count();
    }
    work.check()?;
    Ok(count + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n")))
}
