//! Bounded lifetime evidence for original storage-backed source allocations.
//!
//! Replay/index eviction does not end an original native or worker capability.
//! Weak references follow the actual shared source through its last owner,
//! without pinning its payload or keeping an unbounded settled history. Only
//! live admission and trusted cold reconstruction insert allocations here.
//! Captures describe absence of live source rights within a frozen allocator
//! range; they do not prove acceptance, durable settlement or authorize deletion.

use std::collections::BTreeMap;
use std::sync::{Mutex, Weak};

use super::*;

mod staging;
pub use staging::MessageSourcePreservationReservation;

/// Includes weak control-block, map bucket and conservative index overhead.
pub(super) const SOURCE_LIFETIME_METADATA: usize = 256;
/// Independent finite allocation index; the combined metadata ceiling can bind
/// earlier. This is not a larger pending-message or recipient admission limit.
const MAX_LIVE_SOURCES: usize = 10_000;

/// Covers container/framing overhead and two conservative sequence projections
/// for the entire finite registry. Reserved independently of validation scratch;
/// optional captures cannot prevent the first absence inspection from starting.
pub(super) const SOURCE_RETIREMENT_METADATA_QUANTUM: usize = 4096 + MAX_LIVE_SOURCES * 16;

/// One fixed recipient-slot accumulator in addition to the existing complete
/// source decoder/scratch/framing allowance. This excess still needs ordinary
/// room; absence inspection alone is not a full reclamation progress reserve.
const INSPECTION_METADATA_BYTES: usize = storage_policy::WORKER_METADATA_QUANTUM + 256 * 1024;

/// Runtime-only allocation coverage; no saved sequence list can populate it.
#[derive(Debug)]
pub(super) struct SourceLifetimes {
    sources: Mutex<BTreeMap<MessageSequence, Weak<QueuedEnvelope>>>,
    limit: usize,
}

impl Default for SourceLifetimes {
    fn default() -> Self {
        Self {
            sources: Mutex::default(),
            limit: MAX_LIVE_SOURCES,
        }
    }
}

impl SourceLifetimes {
    /// Prunes only allocations that can no longer be upgraded. A concurrent
    /// final drop may leave a conservative extra entry until the next scan.
    fn live(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<u64, Weak<QueuedEnvelope>>>> {
        let mut sources = self.sources.lock().map_err(|_| unavailable())?;
        sources.retain(|_, source| source.strong_count() != 0);
        if sources.is_empty() {
            // Drop any retained empty root node as well as the dead weak refs.
            *sources = BTreeMap::new();
        }
        Ok(sources)
    }

    /// Charges the real retained weak-index nodes, independently of row bytes.
    pub(super) fn metadata_bytes(&self) -> Result<usize> {
        Ok(self.live()?.len().saturating_mul(SOURCE_LIFETIME_METADATA))
    }

    /// Reserves one new source node before sequence allocation or recovery
    /// publication. Dead weak entries do not consume a lifetime history slot.
    pub(super) fn preflight(&self) -> Result<usize> {
        let sources = self.live()?;
        if sources.len() >= self.limit {
            return Err(capacity());
        }
        Ok((sources.len() + 1).saturating_mul(SOURCE_LIFETIME_METADATA))
    }

    /// Records the actual original before it can escape to an issued worker.
    /// A different still-live allocation cannot replace an existing sequence.
    pub(super) fn record(&self, source: &Arc<QueuedEnvelope>) -> Result<()> {
        let mut sources = self.live()?;
        if let Some(prior) = sources.get(&source.sequence) {
            return if prior.ptr_eq(&Arc::downgrade(source)) {
                Ok(())
            } else {
                Err(unavailable())
            };
        }
        if sources.len() >= self.limit {
            return Err(capacity());
        }
        sources.insert(source.sequence, Arc::downgrade(source));
        Ok(())
    }
}

/// Immutable absence evidence for one actual service/root and allocator range.
/// It is deliberately neither serializable nor constructible from saved facts.
/// Retaining it pins no original source and does not renew any delivery right.
pub struct MessageSourceRetirementFence {
    runtime: Arc<()>,
    scope: MessageStorageScope,
    through: MessageSequence,
    /// Sorted conservative exclusions, including unpublished original sources.
    live_sources: Vec<MessageSequence>,
    /// Copies and container overhead remain charged through the final owner.
    _lease: MessageStorageSnapshotLease,
}

/// A bounded original-source inspection input, never a cleanup intent or I/O
/// permission. The real worker must retain this input through its actual exit.
/// Optional snapshots cannot consume the fence allowance, but the additional
/// slot accumulator must fit ordinary space before this input can be issued.
pub struct MessageSourceRetirementInspection {
    fence: MessageSourceRetirementFence,
    identity: Arc<()>,
    _scratch: MessageStorageSnapshotLease,
}

impl MessageSourceRetirementInspection {
    /// Checks actual runtime-issued root scope, not a serialized owner label.
    pub fn belongs_to(&self, scope: &MessageStorageScope) -> bool {
        self.fence.belongs_to(scope)
    }

    /// Source absence is only a prerequisite to separately verified settlement.
    pub fn has_no_live_source(&self, sequence: MessageSequence) -> bool {
        self.fence.has_no_live_source(sequence)
    }

    /// Original admission's finite source quantum; no payload is materialized
    /// by a metadata-only inspection even though its original length is checked.
    pub fn decode_limits(&self) -> MessageAcceptanceDecodeLimits {
        MessageAcceptanceDecodeLimits {
            payload_bytes: 1024 * 1024,
            auxiliary_bytes: 512 * 1024,
            recipient_count: 4096,
            recipient_bytes: 512 * 1024,
        }
    }
}

impl std::fmt::Debug for MessageSourceRetirementFence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageSourceRetirementFence")
            .field("excluded_sources", &self.live_sources.len())
            .finish_non_exhaustive()
    }
}

impl MessageSourceRetirementFence {
    /// Checks the captured actual storage scope, not equal owner labels.
    pub fn belongs_to(&self, scope: &MessageStorageScope) -> bool {
        self.scope == *scope
    }

    /// Reports that no live source allocation existed for this sequence at
    /// capture. Zero, future allocations and every then-live source refuse.
    /// Cancelled allocator gaps can satisfy this predicate: the trusted store
    /// must separately prove an actual original and every durable obligation's
    /// settlement, preserve dependency closure, and fence the current service
    /// before using this evidence. This method alone grants no deletion right.
    pub fn has_no_live_source(&self, sequence: MessageSequence) -> bool {
        sequence != 0
            && sequence <= self.through
            && self.live_sources.binary_search(&sequence).is_err()
    }
}

impl MessageService {
    /// Reserves all inspection scratch before any source/journal scan. Failure
    /// retains no input or loan and changes no sequence, custody or offers.
    pub fn prepare_source_retirement_inspection(
        &self,
        scope: &MessageStorageScope,
    ) -> Result<MessageSourceRetirementInspection> {
        let fence = self.capture_source_retirement_fence(scope)?;
        let scratch = self.reserve_storage_worker_metadata(INSPECTION_METADATA_BYTES)?;
        Ok(MessageSourceRetirementInspection {
            fence,
            identity: Arc::new(()),
            _scratch: scratch,
        })
    }

    /// Revalidate this exact original at work admission/adoption. A matching
    /// saved store, scope or allocator range cannot replace the live service.
    pub fn source_retirement_inspection_is_original(
        &self,
        inspection: &MessageSourceRetirementInspection,
    ) -> bool {
        self.source_retirement_fence_is_original(&inspection.fence)
    }

    /// Captures bounded source-lifetime exclusions without reading payloads,
    /// selecting recipients, acknowledging mail or retiring native capabilities.
    /// All originals are recorded before admission/worker exposure and during
    /// trusted reconstruction. Weak-dead allocations cannot later be upgraded;
    /// fresh sequences lie beyond this fence. A replacement service must not
    /// inherit it even if its saved metadata and storage scope are identical.
    pub fn capture_source_retirement_fence(
        &self,
        scope: &MessageStorageScope,
    ) -> Result<MessageSourceRetirementFence> {
        self.require_storage_scope(Some(scope))?;
        let policy = self.storage_custody.as_ref().ok_or_else(unavailable)?;
        let sources = policy.source_lifetimes.live()?;
        let bytes = 4096usize.saturating_add(sources.len().saturating_mul(16));
        // Do not call base usage with the registry mutex held: it measures the
        // same weak index. Shared service access cannot admit another source,
        // and concurrent final drops only make this capture conservative.
        let mut live_sources = Vec::new();
        drop(sources);
        let lease = checkpoint::reserve_source_retirement_metadata(
            policy.checkpoint_bytes.clone(),
            policy.source_retirement_metadata_loan.clone(),
            bytes,
            self.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )?;
        let sources = policy.source_lifetimes.live()?;
        live_sources
            .try_reserve_exact(sources.len())
            .map_err(|_| capacity())?;
        live_sources.extend(sources.keys().copied());
        live_sources.sort_unstable();
        Ok(MessageSourceRetirementFence {
            runtime: self.acceptance_runtime.clone(),
            scope: scope.clone(),
            through: self.next_sequence.saturating_sub(1),
            live_sources,
            _lease: lease,
        })
    }

    /// Validates this exact service allocation as well as its actual root. A
    /// caller must check this at work admission/adoption, not infer freshness
    /// from the descriptive allocator range or from a successful saved restore.
    pub fn source_retirement_fence_is_original(
        &self,
        fence: &MessageSourceRetirementFence,
    ) -> bool {
        Arc::ptr_eq(&self.acceptance_runtime, &fence.runtime)
            && self
                .storage_custody
                .as_ref()
                .is_some_and(|p| p.scope == fence.scope)
    }

    /// Tracks only code-owned originals, before any external source capability
    /// can escape. Unconfigured ephemeral operation has no storage source right.
    pub(super) fn record_storage_source_lifetime(&self, row: &Arc<QueuedEnvelope>) -> Result<()> {
        if let Some(policy) = &self.storage_custody {
            policy.source_lifetimes.record(row)?;
        }
        Ok(())
    }
}

fn capacity() -> MessageError {
    MessageError::new(
        MessageErrorKind::Capacity,
        "MMP source lifetime metadata capacity",
    )
}

fn unavailable() -> MessageError {
    MessageError::invalid_state("MMP original source lifetime evidence unavailable")
}

#[cfg(test)]
mod tests;
