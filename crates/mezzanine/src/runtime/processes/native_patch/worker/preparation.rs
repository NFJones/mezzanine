//! Pre-mutation diff and encoded-fact capacity admission for the complete plan.
//!
//! Every possible positive directory/file fact is sized before the first mkdir
//! or file commit. Diffs have an independent 256 KiB source cap and at most sixfold
//! JSON escaping; authoritative facts fit 1 MiB including semantic intent and a
//! 64 KiB diagnostic/envelope reserve. Nothing is truncated to reconstruct effects.

use super::*;
use crate::security::filesystem::capability::parent::MAX_NATIVE_PARENT_DIRECTORIES;
use mez_agent::semantic_patch_planning::native::{
    NATIVE_PATCH_DIFF_BYTES, NativePatchPlan, native_unified_diff,
};
use std::collections::BTreeSet;

/// Preallocated exact endpoint sources; allocation/scans cannot follow a commit.
pub(super) struct PreparedEndpoint {
    /// File fact, whose actual confirmation ordinal is assigned immediately precommit.
    pub(super) file: NativePatchEndpointReceipt,
    /// First-use missing ancestor facts in creation order, each separately leased.
    pub(super) parents: Vec<NativePatchEndpointReceipt>,
}

/// Builds all accepted endpoint sources and refuses excess facts before mutation.
pub(super) fn prepare(
    plan: &NativePatchPlan,
    owners: &BTreeMap<String, EndpointOwner>,
    work: &PatchWorkControl<'_>,
) -> Result<BTreeMap<(usize, usize), PreparedEndpoint>> {
    let mut facts = 64 * 1024;
    for effect in plan
        .operations()
        .iter()
        .filter(|operation| !operation.changes().is_empty())
        .map(|operation| operation.effect())
    {
        work.check()?;
        facts += effect_json(effect).to_string().len() + 1;
    }
    let mut diff_budget = NATIVE_PATCH_DIFF_BYTES;
    let mut directories = BTreeSet::new();
    let mut confirmation = 0;
    let mut prepared = BTreeMap::new();
    for operation in plan.operations() {
        for (endpoint, change) in operation.changes().iter().enumerate() {
            work.check()?;
            let mut parents = Vec::new();
            if matches!(change.original, ApplyPatchOriginalState::Missing)
                && change.final_bytes.is_some()
            {
                let Some(EndpointOwner::Captured(capability)) = owners.get(&change.path) else {
                    return Err(MezError::invalid_state(
                        "native preparation lost initial capture",
                    ));
                };
                for path in capability.missing_parent_paths()? {
                    work.check()?;
                    if directories.contains(&path) {
                        continue;
                    }
                    let ordinal = directories.len();
                    if ordinal >= MAX_NATIVE_PARENT_DIRECTORIES {
                        return Err(MezError::invalid_args(
                            "native parent count exceeds action budget",
                        ));
                    }
                    let path = path
                        .to_str()
                        .ok_or_else(|| MezError::invalid_args("native parent path is not UTF-8"))?
                        .to_owned();
                    let receipt = NativePatchEndpointReceipt {
                        operation_ordinal: operation.ordinal(),
                        endpoint_ordinal: 2 + ordinal,
                        confirmation_ordinal: confirmation,
                        path: path.clone(),
                        resolved_path: path.clone(),
                        kind: NativePatchEndpointKind::DirectoryCreated,
                        parent_target: Some(NativeParentTarget {
                            path: change.path.clone(),
                            resolved_path: change.resolved_path.clone(),
                        }),
                        diff: String::new(),
                        added_lines: 0,
                        removed_lines: 0,
                        diff_truncated: false,
                    };
                    facts += receipt.metadata(false).to_string().len() + 1;
                    if facts > NATIVE_PATCH_FACT_BYTES {
                        return Err(MezError::invalid_args(
                            "native encoded effect facts exceed result budget",
                        ));
                    }
                    directories.insert(std::path::PathBuf::from(path));
                    parents.push(receipt);
                    confirmation += 1;
                }
            }
            let diff = native_unified_diff(change, diff_budget, work)?;
            diff_budget = diff_budget.saturating_sub(diff.source.len());
            let file = NativePatchEndpointReceipt {
                operation_ordinal: operation.ordinal(),
                endpoint_ordinal: endpoint,
                confirmation_ordinal: confirmation,
                path: change.path.clone(),
                resolved_path: change.resolved_path.clone(),
                kind: if change.final_bytes.is_none() {
                    NativePatchEndpointKind::Deleted
                } else if matches!(change.original, ApplyPatchOriginalState::Missing) {
                    NativePatchEndpointKind::Created
                } else {
                    NativePatchEndpointKind::Updated
                },
                parent_target: None,
                diff: diff.source,
                added_lines: diff.new_lines,
                removed_lines: diff.old_lines,
                diff_truncated: diff.truncated,
            };
            facts += file.metadata(false).to_string().len() + 1;
            if facts > NATIVE_PATCH_FACT_BYTES {
                return Err(MezError::invalid_args(
                    "native encoded effect facts exceed result budget",
                ));
            }
            confirmation += 1;
            prepared.insert(
                (operation.ordinal(), endpoint),
                PreparedEndpoint { file, parents },
            );
        }
    }
    work.check()?;
    Ok(prepared)
}
