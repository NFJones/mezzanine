//! Retained wire evidence and actor acknowledgment for one provider producer.
//!
//! This is the existing observation journal, not an execution queue or billing
//! ledger. Producers retain content-free evidence before cancellable delivery;
//! monitors acknowledge only successful actor ingress. Compactor continuations
//! can await their own receipt without racing a post-retirement drain. Producer
//! cancellation leaves unacknowledged evidence for its original retirement owner.

use super::ProviderWireRequestObservation;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;

#[derive(Default)]
struct Journal {
    entries: Mutex<BTreeMap<String, ProviderWireRequestObservation>>,
    acknowledged: Notify,
}

/// Shared original wire journal for a producer and its retained monitor owner.
#[derive(Clone, Default)]
pub(crate) struct PendingProviderWireObservations(Arc<Journal>);

impl PendingProviderWireObservations {
    /// Preserves already reported expense even when a prior presentation panic
    /// poisoned the lock; no payload or execution authority lives in this map.
    fn entries(&self) -> MutexGuard<'_, BTreeMap<String, ProviderWireRequestObservation>> {
        self.0
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Retains an immutable receipt before sending through the bounded channel.
    pub(crate) fn retain(&self, observation: ProviderWireRequestObservation) {
        self.entries()
            .insert(observation.request_id.clone(), observation);
    }

    /// Removes a receipt only after successful actor ingress and wakes waiters.
    pub(crate) fn acknowledge(&self, id: &str) {
        let removed = self.entries().remove(id).is_some();
        if removed {
            self.0.acknowledged.notify_waiters();
        }
    }

    /// Copies retained evidence after the producer is physically retired. A
    /// live producer must use per-receipt acknowledgment instead of this drain.
    pub(crate) fn snapshot(&self) -> Vec<ProviderWireRequestObservation> {
        self.entries().values().cloned().collect()
    }

    /// Waits for this exact actor acknowledgment. Register before examining the
    /// map so an acknowledgment on either side of the check cannot be lost.
    /// Dropping this future never removes its retained expense evidence.
    pub(crate) async fn wait_for_acknowledgment(&self, id: &str) {
        loop {
            let acknowledged = self.0.acknowledged.notified();
            tokio::pin!(acknowledged);
            acknowledged.as_mut().enable();
            let pending = self.entries().contains_key(id);
            if !pending {
                return;
            }
            acknowledged.await;
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries().len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }
}

#[cfg(test)]
mod tests;
