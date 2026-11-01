//! Descriptor-based patch worker with ordered, positively confirmed effects.
//!
//! This synchronous owner runs outside the actor. A supplied authority callback
//! must obtain current actor grants for preparation and exact short actor-issued
//! leases before each commit. The
//! immutable initial scopes remain an upper bound. Actual runtime dispatch and
//! retirement must retain this worker and its returned evidence; dropping a
//! blocking task cannot certify unexecuted effects. Cancellation is checked
//! between bounded descriptor I/O quanta and operations, not an interruption
//! guarantee for OS syscalls. The shared matcher, snapshots and native diff use
//! that same original fence with a finite CPU work allowance. Missing-parent
//! creation uses separately scoped exact leases and positive directory receipts.
//! Receipt facts are capacity-checked before mutation; directory effects remain
//! after later failure and never become fabricated file writes or rollback.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use mez_agent::native_action::{
    NativeCommitIdentity, NativeEffectCertainty, NativePatchOperation, NativePatchOutcome,
    NativePatchPhase, PatchEffect,
};
use mez_agent::permissions::PathScopes;
use mez_agent::semantic_patch_planning::native::{NATIVE_PATCH_SNAPSHOT_BYTES, plan_native_patch};
use mez_agent::semantic_patch_planning::{
    ApplyPatchFileChange, ApplyPatchOriginalState, ApplyPatchSnapshot, ApplyPatchSnapshotState,
};

use mez_agent::semantic_patch::SemanticPatchPlanningError;
use mez_agent::semantic_patch_planning::work::PatchWorkControl;

use super::commit::NativePatchCommitFence;
use super::lease::{NativePatchCommitLease, NativePatchCommitProposal};
use crate::error::{MezError, Result};
use crate::security::filesystem::capability::parent::NativeCreatedDirectory;
use crate::security::filesystem::capability::{
    NativeFileCapability, NativeFilesystemRoot, NativeStagedFile,
};
use crate::security::filesystem::native_io::NativeIoControl;

/// Immutable action ownership and upper-bound filesystem authority.
pub(crate) struct NativePatchWorkerRequest {
    /// Exact action, generation and semantic payload admitted by the actor.
    pub(crate) identity: NativeCommitIdentity,
    /// Captured live process lifetime, physical cwd and credential identity.
    pub(crate) root: NativeFilesystemRoot,
    /// Maximum grants captured for the action; later grants can only narrow.
    pub(crate) scopes: PathScopes,
    /// Exact action's shared cancellation and finite deadline.
    pub(crate) fence: NativePatchCommitFence,
}

mod parents;
mod preparation;
mod receipt;
pub(crate) use receipt::{
    NATIVE_PATCH_FACT_BYTES, NATIVE_PATCH_RESULT_BYTES, NativeParentTarget,
    NativePatchEndpointKind, NativePatchEndpointReceipt, effect_json,
};

/// Original worker outcome and endpoint custody, without fabricated shell I/O.
pub(crate) struct NativePatchWorkerOutcome {
    /// Full admitted identity and truthful whole-operation certainty.
    pub(crate) result: NativePatchOutcome,
    /// Every confirmed endpoint, including effects before a later error.
    pub(crate) receipts: Vec<NativePatchEndpointReceipt>,
    /// Successfully processed semantic operation occurrences, including no-ops.
    pub(crate) completed_operations: Vec<usize>,
}

/// Current exact object proof for the next operation on one source occurrence.
enum EndpointOwner {
    /// Initial snapshot's physical identity, not a fresh equal-byte substitute.
    Captured(NativeFileCapability),
    /// Exact stage descriptor retained after its positive publication.
    Published {
        /// Exact positively published leaf object and final byte evidence.
        stage: NativeStagedFile,
        /// Original captured authority and complete held parent ancestry.
        predecessor: NativeFileCapability,
    },
    /// Positive deletion with its original held ancestry for later recreation.
    Deleted(NativeFileCapability),
}

impl EndpointOwner {
    /// Transfers exact original or positively evolved ownership into a commit.
    fn capture(
        self,
        request: &NativePatchWorkerRequest,
        scopes: &PathScopes,
        created: &[NativeCreatedDirectory],
    ) -> Result<NativeFileCapability> {
        match self {
            Self::Captured(capability) => {
                if capability.preimage().is_none() && !created.is_empty() {
                    capability.recapture_with_created_parents(&request.root, scopes, created)
                } else {
                    capability.revalidate(&request.root, scopes)?;
                    Ok(capability)
                }
            }
            Self::Published { stage, predecessor } => {
                predecessor
                    .recapture_published(&request.root, scopes, &stage, || request.fence.check())
            }
            Self::Deleted(capability) => {
                capability.recapture_missing(&request.root, scopes, || request.fence.check())
            }
        }
    }
}

/// Authority bridge implemented by an actor or its asynchronous request client.
/// Returning scopes permits preparation only; every mutation needs an exact
/// actor-issued lease that already owns in-flight commit scheduling custody.
pub(crate) trait NativePatchAuthority {
    /// Resolves current clipped grants for bounded snapshot/stage/no-op work.
    fn scopes(
        &mut self,
        identity: &NativeCommitIdentity,
        phase: NativePatchPhase,
        path: &str,
    ) -> Result<PathScopes>;
    /// Admits one exact proposed endpoint under current actor authority.
    fn commit(
        &mut self,
        identity: &NativeCommitIdentity,
        proposal: &NativePatchCommitProposal,
    ) -> Result<NativePatchCommitLease>;
}

/// Executes bounded native planning and actual descriptor-relative publication.
/// The callback must validate exact current actor ownership before returning
/// scopes; this worker never converts paths or presentation into permission.
/// A panic retains earlier receipts and marks an interrupted commit unknown.
pub(crate) fn execute_native_patch(
    request: NativePatchWorkerRequest,
    mut authority: impl NativePatchAuthority,
    mut progress: impl FnMut(&NativePatchEndpointReceipt),
) -> NativePatchWorkerOutcome {
    let mut outcome = NativePatchWorkerOutcome {
        result: NativePatchOutcome {
            identity: request.identity.clone(),
            phase: NativePatchPhase::Snapshot,
            certainty: NativeEffectCertainty::Unexecuted,
            confirmed_effects: Vec::new(),
            diagnostic: None,
        },
        receipts: Vec::new(),
        completed_operations: Vec::new(),
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        execute(&request, &mut outcome, &mut authority, &mut progress)
    }));
    match result {
        Ok(Ok(())) => outcome.result.certainty = NativeEffectCertainty::Confirmed,
        failure => {
            let uncertain_failure = match &failure {
                Err(_) => true,
                Ok(Err(error)) => error.kind() == crate::error::MezErrorKind::Io,
                _ => false,
            };
            let unknown = uncertain_failure && outcome.result.phase == NativePatchPhase::Commit;
            outcome.result.certainty = if unknown {
                NativeEffectCertainty::Unknown
            } else if outcome.receipts.is_empty() {
                NativeEffectCertainty::Unexecuted
            } else {
                NativeEffectCertainty::Partial
            };
            let diagnostic = match failure {
                Ok(Err(error)) => error.message().to_string(),
                _ => "native patch worker failed; no mutation replay is authorized".to_string(),
            };
            outcome.result.diagnostic = Some(bounded_diagnostic(diagnostic));
        }
    }
    outcome
}

/// Returns bounded model-safe failure text without slicing through UTF-8.
fn bounded_diagnostic(mut text: String) -> String {
    let mut end = text.len().min(8192);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

/// Intersects each fresh actor grant with the immutable captured upper bound.
fn authorize(
    request: &NativePatchWorkerRequest,
    authority: &mut impl NativePatchAuthority,
    phase: NativePatchPhase,
    path: &str,
) -> Result<PathScopes> {
    request.fence.check()?;
    let current = authority.scopes(&request.identity, phase, path)?;
    request.fence.check()?;
    request
        .scopes
        .intersection(&current)
        .map_err(|error| MezError::forbidden(error.message()))
}

/// Captures exact physical snapshots, then executes authored operations in order.
fn execute(
    request: &NativePatchWorkerRequest,
    outcome: &mut NativePatchWorkerOutcome,
    authority: &mut impl NativePatchAuthority,
    progress: &mut impl FnMut(&NativePatchEndpointReceipt),
) -> Result<()> {
    request.fence.check()?;
    if NativePatchOperation::parse(&request.identity.operation.exact_patch, None)?
        != request.identity.operation
    {
        return Err(MezError::invalid_args(
            "native patch semantic identity changed",
        ));
    }
    let checkpoint = || {
        request
            .fence
            .check()
            .map_err(|error| SemanticPatchPlanningError::invalid_args(error.message()))
    };
    let work = PatchWorkControl::native(&checkpoint);
    let original_fence = request.fence.clone();
    let io_control = NativeIoControl::new(move || original_fence.check());
    let mut owners = BTreeMap::new();
    let mut snapshots = BTreeMap::new();
    let mut snapshot_bytes = 0usize;
    for path in request.identity.operation.patch.touched_paths() {
        let scopes = authorize(request, authority, NativePatchPhase::Snapshot, &path)?;
        let capability = NativeFileCapability::capture_with_io_control(
            &request.root,
            &scopes,
            &path,
            true,
            io_control.clone(),
        )?;
        snapshot_bytes =
            snapshot_bytes.saturating_add(capability.preimage().map_or(0, <[u8]>::len));
        if snapshot_bytes > NATIVE_PATCH_SNAPSHOT_BYTES {
            return Err(MezError::invalid_args(
                "native patch snapshot budget exceeded",
            ));
        }
        let resolved_path = capability
            .path()
            .to_str()
            .ok_or_else(|| MezError::invalid_args("native patch physical path is not UTF-8"))?
            .to_string();
        let state = match capability.preimage() {
            Some(bytes) => ApplyPatchSnapshotState::Regular(work.copy_bytes(bytes)?),
            None => ApplyPatchSnapshotState::Missing,
        };
        snapshots.insert(
            path.clone(),
            ApplyPatchSnapshot {
                path: path.clone(),
                resolved_path,
                state,
            },
        );
        owners.insert(path, EndpointOwner::Captured(capability));
    }
    outcome.result.phase = NativePatchPhase::Plan;
    request.fence.check()?;
    let plan = plan_native_patch(&request.identity.operation, &snapshots, &work)?;
    request.fence.check()?;
    let mut prepared = preparation::prepare(&plan, &owners, &work)?;
    let maximum_receipts = prepared
        .values()
        .map(|endpoint| 1 + endpoint.parents.len())
        .sum();
    outcome
        .receipts
        .try_reserve_exact(maximum_receipts)
        .map_err(|error| {
            MezError::invalid_state(format!("native receipt capacity unavailable: {error}"))
        })?;
    outcome
        .completed_operations
        .try_reserve_exact(plan.operations().len())
        .map_err(|error| {
            MezError::invalid_state(format!("native completion capacity unavailable: {error}"))
        })?;
    outcome
        .result
        .confirmed_effects
        .try_reserve_exact(plan.operations().len())
        .map_err(|error| {
            MezError::invalid_state(format!("native effect capacity unavailable: {error}"))
        })?;
    let mut created = Vec::new();
    let maximum_parents = prepared
        .values()
        .map(|endpoint| endpoint.parents.len())
        .sum();
    created
        .try_reserve_exact(maximum_parents)
        .map_err(|error| {
            MezError::invalid_state(format!(
                "native directory owner capacity unavailable: {error}"
            ))
        })?;
    for operation in plan.operations() {
        request.fence.check()?;
        if operation.changes().is_empty() {
            let paths: Vec<&str> = match operation.effect() {
                PatchEffect::Create(path)
                | PatchEffect::Update(path)
                | PatchEffect::Delete(path) => vec![path],
                PatchEffect::Move {
                    source,
                    destination,
                } => vec![source, destination],
            };
            for path in paths {
                let scopes = authorize(request, authority, NativePatchPhase::Commit, path)?;
                let owner = owners
                    .remove(path)
                    .ok_or_else(|| MezError::invalid_state("native no-op lost source ownership"))?;
                let capability = owner.capture(request, &scopes, &created)?;
                owners.insert(path.to_string(), EndpointOwner::Captured(capability));
            }
        }
        for (endpoint_ordinal, change) in operation.changes().iter().enumerate() {
            publish_endpoint(
                request,
                outcome,
                authority,
                progress,
                &mut owners,
                operation.ordinal(),
                endpoint_ordinal,
                change,
                &mut created,
                prepared
                    .remove(&(operation.ordinal(), endpoint_ordinal))
                    .ok_or_else(|| {
                        MezError::invalid_state("native endpoint preparation missing")
                    })?,
            )?;
        }
        outcome.completed_operations.push(operation.ordinal());
        if !operation.changes().is_empty() {
            outcome
                .result
                .confirmed_effects
                .push(operation.effect().clone());
        }
    }
    if let Some((_, _, diagnostic)) = plan.errors().first() {
        return Err(MezError::invalid_args(diagnostic));
    }
    outcome.result.phase = NativePatchPhase::Settle;
    Ok(())
}

/// Publishes one endpoint, records its receipt, then notifies presentation.
#[allow(
    clippy::too_many_arguments,
    reason = "explicit exact request, authority and ordered receipt owners"
)]
fn publish_endpoint(
    request: &NativePatchWorkerRequest,
    outcome: &mut NativePatchWorkerOutcome,
    authority: &mut impl NativePatchAuthority,
    progress: &mut impl FnMut(&NativePatchEndpointReceipt),
    owners: &mut BTreeMap<String, EndpointOwner>,
    operation_ordinal: usize,
    endpoint_ordinal: usize,
    change: &ApplyPatchFileChange,
    created: &mut Vec<NativeCreatedDirectory>,
    prepared: preparation::PreparedEndpoint,
) -> Result<()> {
    outcome.result.phase = NativePatchPhase::Stage;
    let scopes = authorize(request, authority, NativePatchPhase::Stage, &change.path)?;
    let owner = owners
        .remove(&change.path)
        .ok_or_else(|| MezError::invalid_state("native patch endpoint lost source ownership"))?;
    let capability = owner.capture(request, &scopes, created)?;
    let expected = match &change.original {
        ApplyPatchOriginalState::Missing => None,
        ApplyPatchOriginalState::Regular(bytes) => Some(bytes.as_slice()),
    };
    if !capability.preimage_matches(expected)?
        || capability.path().to_str() != Some(change.resolved_path.as_str())
    {
        return Err(MezError::conflict(
            "native patch exact preimage or resolution changed",
        ));
    }
    let capability = parents::create_missing_parents(
        request,
        outcome,
        authority,
        progress,
        capability,
        change,
        operation_ordinal,
        created,
        prepared.parents,
    )?;
    let scopes = authorize(request, authority, NativePatchPhase::Stage, &change.path)?;
    let mut receipt = prepared.file;
    receipt.confirmation_ordinal = outcome.receipts.len();
    request.fence.check()?;
    let stage = change
        .final_bytes
        .as_ref()
        .map(|bytes| capability.stage(&request.root, &scopes, bytes))
        .transpose()?;
    let proposal = NativePatchCommitProposal::capture(
        &capability,
        change,
        operation_ordinal,
        endpoint_ordinal,
    )?;
    request.fence.check()?;
    let lease = authority.commit(&request.identity, &proposal)?;
    let (scopes, guard) = lease.enter(&proposal, &request.fence)?;
    outcome.result.phase = NativePatchPhase::Commit;
    let (kind, next) = if let Some(mut stage) = stage {
        capability.publish(&request.root, &scopes, &mut stage)?;
        let kind = if expected.is_none() {
            NativePatchEndpointKind::Created
        } else {
            NativePatchEndpointKind::Updated
        };
        (
            kind,
            EndpointOwner::Published {
                stage,
                predecessor: capability,
            },
        )
    } else {
        capability.delete(&request.root, &scopes)?;
        (
            NativePatchEndpointKind::Deleted,
            EndpointOwner::Deleted(capability),
        )
    };
    // All receipt allocations and scans preceded mutation. Only a scalar kind
    // assignment follows its positive syscall before retaining that evidence.
    receipt.kind = kind;
    outcome.receipts.push(receipt);
    outcome.result.phase = NativePatchPhase::Settle;
    owners.insert(change.path.clone(), next);
    drop(guard);
    progress(
        outcome
            .receipts
            .last()
            .ok_or_else(|| MezError::invalid_state("native patch lost commit receipt"))?,
    );
    Ok(())
}

#[cfg(test)]
mod tests;
