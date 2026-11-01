//! Exact separately leased parent commits with positive evidence before probes.
//!
//! Directory grants remain distinct from file-only grants. Each successful mkdir
//! appends its preallocated fact before notification, cancellation or observation;
//! subsequent failures preserve that fact and never roll back directories. Held
//! observations remain owned for later original snapshots sharing those parents.

use super::*;
use crate::security::filesystem::capability::parent::NativeParentCapability;

/// Creates only the exact prepared first missing parent under fresh actor leases.
#[allow(
    clippy::too_many_arguments,
    reason = "original request, source, authority and positive effect owners are explicit"
)]
pub(super) fn create_missing_parents(
    request: &NativePatchWorkerRequest,
    outcome: &mut NativePatchWorkerOutcome,
    authority: &mut impl NativePatchAuthority,
    progress: &mut impl FnMut(&NativePatchEndpointReceipt),
    mut capability: NativeFileCapability,
    change: &ApplyPatchFileChange,
    operation: usize,
    created: &mut Vec<NativeCreatedDirectory>,
    parents: Vec<NativePatchEndpointReceipt>,
) -> Result<NativeFileCapability> {
    for mut receipt in parents {
        let scopes = authorize(request, authority, NativePatchPhase::Stage, &change.path)?;
        capability.revalidate(&request.root, &scopes)?;
        let next = capability.missing_parent_paths()?;
        if next.first().and_then(|path| path.to_str()) != Some(receipt.path.as_str()) {
            return Err(MezError::conflict(
                "native missing parent differs from prepared effect",
            ));
        }
        let fence = request.fence.clone();
        let control = NativeIoControl::new(move || fence.check());
        let parent =
            NativeParentCapability::capture(&request.root, &scopes, &receipt.path, control)?;
        let proposal = NativePatchCommitProposal::capture_parent(
            &parent,
            &capability,
            change,
            operation,
            receipt.endpoint_ordinal - 2,
        )?;
        let lease = authority.commit(&request.identity, &proposal)?;
        let (scopes, guard) = lease.enter(&proposal, &request.fence)?;
        receipt.confirmation_ordinal = outcome.receipts.len();
        outcome.result.phase = NativePatchPhase::Commit;
        let mut directory = parent.commit(&request.root, &scopes)?;
        outcome.receipts.push(receipt);
        outcome.result.phase = NativePatchPhase::Settle;
        drop(guard);
        progress(
            outcome
                .receipts
                .last()
                .ok_or_else(|| MezError::invalid_state("native directory receipt missing"))?,
        );
        directory.observe(&request.root, &scopes)?;
        created.push(directory);
        capability = capability.recapture_with_created_parents(&request.root, &scopes, created)?;
    }
    Ok(capability)
}
