//! Own the exact concrete transport future through local retirement.
//!
//! The inner future is explicitly destroyed before its original factory sees
//! a sealed witness, on Ready and cancellation alike. A panic while destroying
//! the future cannot manufacture retirement. This covers scoped local transport
//! and active callback futures, not independently spawned work or remote billing.
//! Native transports must not detach inference work from this scoped future.
//! Evidence transfer is synchronous and performs no I/O or exposure release.

use super::*;
use std::task::{Context, Poll};

#[cfg(test)]
mod tests;

/// Not constructible from a wire observation, database row, ACK or task join.
/// The private constructor below runs only AFTER the original future is gone.
#[derive(Debug)]
pub(crate) struct ProviderTransportRetirement {
    observation: ProviderWireRequestObservation,
}

impl ProviderTransportRetirement {
    pub(crate) fn matches(&self, observation: &ProviderWireRequestObservation) -> bool {
        self.observation.same_wire_subject(observation)
    }
}

pub(super) struct WireTransportOwner {
    observation: ProviderWireRequestObservation,
    factory: Option<Arc<dyn ProviderObservationReceiptFactory>>,
    pending: Option<PendingProviderWireObservations>,
}

impl ProviderWireObservationContext<'_> {
    /// Capture before constructing/polling transport. This borrows no decoder
    /// or usage state and cannot select a newer factory during later Drop.
    pub(super) fn transport_owner(&self) -> Result<WireTransportOwner> {
        let mut planned = self
            .planned_wire
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let plan = planned
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("transport requires original wire plan"))?;
        if plan.transport_attached {
            return Err(MezError::conflict("original transport already attached"));
        }
        plan.transport_attached = true;
        Ok(WireTransportOwner {
            observation: plan.observation.clone(),
            factory: plan.factory.clone(),
            pending: self.observer.pending.clone(),
        })
    }
}

/// One owner wraps one concrete send. The boxed pinned future has no independent
/// handle that can outlive this wrapper. No async work is launched from Drop.
pub(super) struct WireTransport<'a, T> {
    future: Option<Pin<Box<dyn Future<Output = T> + Send + 'a>>>,
    owner: Option<WireTransportOwner>,
}

impl<'a, T> WireTransport<'a, T> {
    pub(super) fn new(
        future: Pin<Box<dyn Future<Output = T> + Send + 'a>>,
        owner: Option<WireTransportOwner>,
    ) -> Self {
        Self {
            future: Some(future),
            owner,
        }
    }

    fn retire(&mut self) {
        // Do not move notification into a drop guard around this operation:
        // destruction must return normally before a witness can exist.
        // Take notification ownership first: if destruction unwinds through
        // poll, this wrapper's later Drop must not attest the failed attempt.
        let owner = self.owner.take();
        drop(self.future.take());
        if let Some(owner) = owner
            && let Some(factory) = owner.factory
        {
            let retirement = ProviderTransportRetirement {
                observation: owner.observation.clone(),
            };
            // A consumer panic is an owner-local integrity refusal. Preserve
            // evidence and fence ACK; never log its potentially private payload
            // or let a second panic escape a cancellation/unwind destructor.
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                factory.transport_retired(&owner.observation, retirement);
            }))
            .is_err()
                && let Some(pending) = owner.pending
            {
                pending.refuse_transport_retirement(&owner.observation);
            }
        }
    }
}

impl<T> Future for WireTransport<'_, T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let this = self.get_mut();
        let result = match this.future.as_mut() {
            Some(future) => future.as_mut().poll(cx),
            None => panic!("retired provider transport polled again"),
        };
        match result {
            Poll::Ready(value) => {
                this.retire();
                Poll::Ready(value)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> Drop for WireTransport<'_, T> {
    fn drop(&mut self) {
        self.retire();
    }
}
