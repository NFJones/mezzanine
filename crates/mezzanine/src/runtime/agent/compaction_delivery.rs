//! Exact custody of a claimed dispatch before producer registration.
//!
//! The last dispatch-value copy can disappear inside a successful unread reply.
//! Its drop proves no producer only until an original worker owner is registered.
//! A registration reservation gates the monitor's first poll; unwind before
//! registration revokes it. Numeric expense custody retains only the observer,
//! so logical cancellation cannot hide an abandoned delivery or retire live cost.
//! This receipt grants no source, execution, publication or wire-send authority.

use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

const OFFERED: u8 = 0;
const RESERVED: u8 = 1;
const REGISTERED: u8 = 2;
const ABANDONED: u8 = 3;

#[derive(Debug, Clone)]
pub(crate) struct RuntimeCompactionDeliveryObserver(Arc<AtomicU8>);

/// Only actual dispatch-value copies keep this drop witness alive. Observer
/// copies in accounting/worker metadata never impersonate an unread reply.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeCompactionDispatchDelivery(Arc<DeliveryValue>);

#[derive(Debug)]
struct DeliveryValue {
    observer: RuntimeCompactionDeliveryObserver,
}

/// Unique pre-poll registration reservation. A duplicate cannot start a second
/// monitor or submit a fabricated retirement over the first producer's expense.
pub(crate) struct RuntimeCompactionProducerRegistration {
    observer: RuntimeCompactionDeliveryObserver,
    committed: bool,
}

impl RuntimeCompactionDispatchDelivery {
    pub(crate) fn new() -> Self {
        Self(Arc::new(DeliveryValue {
            observer: RuntimeCompactionDeliveryObserver(Arc::new(AtomicU8::new(OFFERED))),
        }))
    }

    pub(crate) fn observer(&self) -> RuntimeCompactionDeliveryObserver {
        self.0.observer.clone()
    }

    pub(crate) fn producer_registered(&self) -> bool {
        self.0.observer.0.load(Ordering::Acquire) == REGISTERED
    }
}

impl RuntimeCompactionDeliveryObserver {
    pub(crate) fn abandoned(&self) -> bool {
        self.0.load(Ordering::Acquire) == ABANDONED
    }

    /// Reserves the sole producer admission before spawning its gated monitor.
    /// The caller must retain the actual worker owner before committing it.
    pub(crate) fn reserve_producer(&self) -> Option<RuntimeCompactionProducerRegistration> {
        self.0
            .compare_exchange(OFFERED, RESERVED, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(RuntimeCompactionProducerRegistration {
            observer: self.clone(),
            committed: false,
        })
    }
}

impl RuntimeCompactionProducerRegistration {
    /// Called only after the original worker identity enters its retained map,
    /// before the monitor gate opens. Only this reservation can change RESERVED.
    pub(crate) fn commit(mut self) {
        self.observer.0.store(REGISTERED, Ordering::Release);
        self.committed = true;
    }
}

impl Drop for RuntimeCompactionProducerRegistration {
    fn drop(&mut self) {
        if !self.committed {
            self.observer.0.store(ABANDONED, Ordering::Release);
        }
    }
}

impl Drop for DeliveryValue {
    fn drop(&mut self) {
        let _ = self.observer.0.compare_exchange(
            OFFERED,
            ABANDONED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}
