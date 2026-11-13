//! Retained wire evidence and actor acknowledgment for one provider producer.
//!
//! This is the existing observation journal, not an execution queue or billing
//! ledger. Producers retain content-free evidence before cancellable delivery;
//! monitors acknowledge only successful actor ingress. Journal-owned continuations
//! await their own observation without racing a post-retirement drain. Producer
//! cancellation leaves unacknowledged evidence for its original retirement owner.
//! A staged budgeted entry additionally retains its frozen native receipt before
//! delivery and requires durable checkpointing before that actor handoff. These
//! subtotals neither attest final coverage nor release issued provider exposure.

use super::ProviderWireRequestObservation;
use crate::error::{MezError, Result};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tokio::sync::Notify;

/// A frozen original receipt, never permission to send or release exposure.
/// Implementations retain their actual writer across canceled checkpoint waits.
pub(crate) trait ProviderObservationReceipt: Send + Sync {
    fn checkpoint(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

/// Request-owned pure allocator. Each call captures one distinct original
/// descriptor; the wire plan and journal retain it before any admission await.
/// This port performs no I/O and grants no permission or exposure release.
pub(crate) trait ProviderObservationReceiptSource: Send + Sync {
    fn for_exchange(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceiptFactory>>;
}

/// Pure, bounded capture before channel delivery; no filesystem or provider I/O.
/// Each physical exchange needs its own original registered-attempt descriptor.
pub(crate) trait ProviderObservationReceiptFactory: Send + Sync {
    /// Bind the immutable wire subject without I/O or spend permission. Native
    /// descriptors override this before transport; diagnostic factories need no
    /// additional subject binding beyond the original journal's own plan.
    fn plan(&self, _observation: &ProviderWireRequestObservation) -> Result<()> {
        Ok(())
    }

    /// Optional native admission, after the journal owns the original plan and
    /// immediately before transport polling. Receipt-only diagnostic factories
    /// grant no permission; their existing behavior remains unchanged.
    fn before_send<'a>(
        &'a self,
        _observation: &'a ProviderWireRequestObservation,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    /// Synchronous custody transfer after the exact local transport future has
    /// been destroyed. This is not durable settlement, actor ACK, complete wire
    /// usage or remote billing/completion proof. Never start work from Drop.
    fn transport_retired(
        &self,
        _observation: &ProviderWireRequestObservation,
        _retirement: super::ProviderTransportRetirement,
    ) {
    }

    fn freeze(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>>;
}

#[derive(Clone)]
enum BudgetCheckpoint {
    Planned,
    Unbudgeted,
    Pending(Arc<dyn ProviderObservationReceipt>),
    Committed(Arc<dyn ProviderObservationReceipt>),
    Refused(MezError),
}

struct Entry {
    observation: ProviderWireRequestObservation,
    budget: BudgetCheckpoint,
    capture: Option<Arc<Finalization>>,
    // Even a failed pure capture must retain its original native descriptor.
    _factory: Option<Arc<dyn ProviderObservationReceiptFactory>>,
    conflicted: bool,
}

/// Reserve the first exact final evidence before invoking its pure factory.
/// Competing finalizers share capture without holding the journal map lock.
struct Finalization {
    observation: ProviderWireRequestObservation,
    budget: OnceLock<BudgetCheckpoint>,
}

fn same_plan(
    original: &Entry,
    observation: &ProviderWireRequestObservation,
    factory: Option<&Arc<dyn ProviderObservationReceiptFactory>>,
) -> bool {
    let same_factory = match (original._factory.as_ref(), factory) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    };
    same_factory && super::wire_attempt::same_subject(&original.observation, observation)
}

#[derive(Default)]
struct Journal {
    entries: Mutex<BTreeMap<String, Entry>>,
    budget_required: AtomicBool,
    acknowledged: Notify,
}

/// Shared original wire journal for a producer and its retained monitor owner.
#[derive(Clone, Default)]
pub(crate) struct PendingProviderWireObservations(Arc<Journal>);

impl PendingProviderWireObservations {
    /// Preserves already reported expense even when a prior presentation panic
    /// poisoned the lock; no payload or execution authority lives in this map.
    fn entries(&self) -> MutexGuard<'_, BTreeMap<String, Entry>> {
        self.0
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Retains an immutable receipt before sending through the bounded channel.
    #[cfg(test)]
    pub(crate) fn retain(&self, observation: ProviderWireRequestObservation) {
        self.retain_with_receipt(observation, None);
    }

    /// Records missing factory work as a refusal, never silently unbudgeted.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "physical-attempt assembly remains staged")
    )]
    pub(crate) fn require_budget_receipts(&self) -> Result<()> {
        let entries = self.entries();
        if !entries.is_empty() {
            return Err(MezError::conflict(
                "budget observation journal already contains work",
            ));
        }
        self.0.budget_required.store(true, Ordering::Release);
        Ok(())
    }

    /// Retain a plan before transport or admission polling. It has no usage
    /// receipt yet and cannot be checkpointed or acknowledged as final evidence.
    pub(super) fn plan(
        &self,
        observation: ProviderWireRequestObservation,
        factory: Option<&Arc<dyn ProviderObservationReceiptFactory>>,
    ) -> Result<()> {
        let entries = self.entries();
        if entries.contains_key(&observation.request_id) {
            return Err(MezError::conflict(
                "provider wire identity already retained",
            ));
        }
        if factory.is_none() && self.0.budget_required.load(Ordering::Acquire) {
            return Err(MezError::invalid_state(
                "original budget receipt unavailable before provider send",
            ));
        }
        if factory.is_some() {
            self.0.budget_required.store(true, Ordering::Release);
        }
        // Pure native binding may inspect its own original custody. Never run
        // factory code under the journal mutex, and publish no unfinished plan
        // that another finalizer could adopt before binding succeeds.
        drop(entries);
        if let Some(factory) = factory {
            factory.plan(&observation)?;
        }
        let mut entries = self.entries();
        if entries.contains_key(&observation.request_id) {
            return Err(MezError::conflict(
                "provider wire identity already retained",
            ));
        }
        if factory.is_none() && self.0.budget_required.load(Ordering::Acquire) {
            return Err(MezError::invalid_state(
                "original budget receipt unavailable before provider send",
            ));
        }
        entries.insert(
            observation.request_id.clone(),
            Entry {
                observation,
                budget: BudgetCheckpoint::Planned,
                capture: None,
                _factory: factory.cloned(),
                conflicted: false,
            },
        );
        Ok(())
    }

    /// Finalizes one planned identity, or retains legacy completed evidence.
    /// A receipt binds only final evidence; a plan is not a fabricated receipt.
    pub(crate) fn retain_with_receipt(
        &self,
        observation: ProviderWireRequestObservation,
        factory: Option<&Arc<dyn ProviderObservationReceiptFactory>>,
    ) -> bool {
        let capture = {
            let mut entries = self.entries();
            let original = entries
                .entry(observation.request_id.clone())
                .or_insert_with(|| Entry {
                    observation: observation.clone(),
                    budget: BudgetCheckpoint::Planned,
                    capture: None,
                    _factory: factory.cloned(),
                    conflicted: false,
                });
            // Conflict fences checkpoint/ACK, not the first valid original
            // expense handed back by stack custody after a refused subject.
            if !same_plan(original, &observation, factory) {
                original.conflicted = true;
                return false;
            }
            if !matches!(original.budget, BudgetCheckpoint::Planned) {
                if original.observation != observation {
                    original.conflicted = true;
                }
                return false;
            }
            let capture = original
                .capture
                .get_or_insert_with(|| {
                    Arc::new(Finalization {
                        observation: observation.clone(),
                        budget: OnceLock::new(),
                    })
                })
                .clone();
            if capture.observation != observation {
                original.conflicted = true;
                return false;
            }
            if factory.is_some() {
                self.0.budget_required.store(true, Ordering::Release);
            }
            capture
        };
        let budget = capture
            .budget
            .get_or_init(|| {
                if let Some(factory) = factory {
                    // A factory panic may follow partial immutable capture. Keep
                    // its source and a sticky refusal, never invoke it again or
                    // attach its potentially private payload to retained errors.
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        factory.freeze(&observation)
                    })) {
                        Ok(Ok(receipt)) => BudgetCheckpoint::Pending(receipt),
                        Ok(Err(error)) => BudgetCheckpoint::Refused(error),
                        Err(_) => BudgetCheckpoint::Refused(MezError::invalid_state(
                            "budget receipt capture panicked",
                        )),
                    }
                } else if self.0.budget_required.load(Ordering::Acquire) {
                    BudgetCheckpoint::Refused(MezError::invalid_state(
                        "original budget receipt unavailable",
                    ))
                } else {
                    BudgetCheckpoint::Unbudgeted
                }
            })
            .clone();
        self.finish_capture(&capture, budget)
    }

    /// Publish only into the exact reservation that performed capture. A late
    /// finalizer cannot resurrect an ACKed entry or replace a reused map key.
    fn finish_capture(&self, capture: &Arc<Finalization>, budget: BudgetCheckpoint) -> bool {
        let mut entries = self.entries();
        let Some(original) = entries.get_mut(&capture.observation.request_id) else {
            // An exact competitor already finalized and ACKed. Do not
            // resurrect the entry or emit another auxiliary usage report.
            return false;
        };
        if !matches!(original.budget, BudgetCheckpoint::Planned)
            || !original
                .capture
                .as_ref()
                .is_some_and(|reserved| Arc::ptr_eq(reserved, capture))
        {
            return false;
        }
        // Preserve the first valid captured expense even if conflicting
        // evidence arrived during capture. The conflict still fences ACK.
        original.observation = capture.observation.clone();
        original.budget = budget;
        true
    }

    /// Keep the original final expense capturable after failed evidence
    /// transfer, while refusing checkpoint/ACK. No execution/retry authority.
    pub(super) fn refuse_transport_retirement(&self, observation: &ProviderWireRequestObservation) {
        if let Some(entry) = self.entries().get_mut(&observation.request_id)
            && entry.observation.same_wire_subject(observation)
        {
            entry.conflicted = true;
        }
    }

    /// Borrow only an already retained factory for duplicate evidence. This
    /// never allocates a descriptor or supplies permission to send again.
    pub(super) fn original_factory(
        &self,
        id: &str,
    ) -> Option<Arc<dyn ProviderObservationReceiptFactory>> {
        self.entries()
            .get(id)
            .and_then(|entry| entry._factory.clone())
    }

    /// Runs only original bookkeeping, outside the synchronous journal lock.
    /// The entry retains its actual writer even if this monitor is canceled.
    pub(crate) async fn checkpoint(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<()> {
        let receipt = {
            let entries = self.entries();
            let Some(entry) = entries.get(&observation.request_id) else {
                return if self.0.budget_required.load(Ordering::Acquire) {
                    Err(MezError::conflict(
                        "original budget observation unavailable",
                    ))
                } else {
                    Ok(())
                };
            };
            if entry.conflicted || entry.observation != *observation {
                return Err(MezError::conflict(
                    "provider observation checkpoint changed",
                ));
            }
            match &entry.budget {
                BudgetCheckpoint::Planned => {
                    return Err(MezError::invalid_state(
                        "provider wire attempt still owns unfinished evidence",
                    ));
                }
                BudgetCheckpoint::Unbudgeted | BudgetCheckpoint::Committed(_) => return Ok(()),
                BudgetCheckpoint::Refused(error) => return Err(error.clone()),
                BudgetCheckpoint::Pending(receipt) => receipt.clone(),
            }
        };
        receipt.checkpoint().await?;
        let mut entries = self.entries();
        let entry = entries.get_mut(&observation.request_id).ok_or_else(|| {
            MezError::conflict("budget observation disappeared before checkpoint")
        })?;
        if entry.conflicted {
            return Err(MezError::conflict("retained provider observation changed"));
        }
        match &entry.budget {
            BudgetCheckpoint::Committed(original) if Arc::ptr_eq(original, &receipt) => Ok(()),
            BudgetCheckpoint::Pending(original) if Arc::ptr_eq(original, &receipt) => {
                entry.budget = BudgetCheckpoint::Committed(receipt);
                Ok(())
            }
            _ => Err(MezError::conflict(
                "budget observation checkpoint owner changed",
            )),
        }
    }

    /// Actor ingress cannot remove an uncommitted/refused budget receipt.
    pub(crate) fn acknowledge(&self, id: &str) {
        let mut entries = self.entries();
        let eligible = entries.get(id).is_some_and(|entry| {
            !entry.conflicted
                && matches!(
                    entry.budget,
                    BudgetCheckpoint::Unbudgeted | BudgetCheckpoint::Committed(_)
                )
        });
        let removed = eligible && entries.remove(id).is_some();
        drop(entries);
        if removed {
            self.0.acknowledged.notify_waiters();
        }
    }

    /// Copies retained evidence after the producer is physically retired. A
    /// live producer must use per-receipt acknowledgment instead of this drain.
    pub(crate) fn snapshot(&self) -> Vec<ProviderWireRequestObservation> {
        self.entries()
            .values()
            .map(|entry| entry.observation.clone())
            .collect()
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

/// Drives a real adapter future against a fixture consumer. Verify retention
/// before each exact ACK, then return delivered evidence for expense assertions.
/// This does not pretend that actor ingress is durable budget settlement.
#[cfg(test)]
pub(crate) async fn observe_and_acknowledge_for_test<F: std::future::Future>(
    call: F,
    receiver: &mut tokio::sync::mpsc::Receiver<ProviderWireRequestObservation>,
    pending: &PendingProviderWireObservations,
) -> (F::Output, Vec<ProviderWireRequestObservation>) {
    tokio::pin!(call);
    let mut observations = Vec::new();
    loop {
        tokio::select! {
            biased;
            result = &mut call => return (result, observations),
            observation = receiver.recv() => {
                let observation = observation.expect("fixture producer closed before completion");
                assert!(pending.snapshot().contains(&observation));
                pending.acknowledge(&observation.request_id);
                observations.push(observation);
            }
        }
    }
}
