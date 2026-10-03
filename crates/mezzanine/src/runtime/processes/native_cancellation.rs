//! Exact-owned cooperative cancellation for native action workers.
//!
//! The actor and its immutable dispatch share one fence. Cancellation is
//! monotonic: retirement or worker-owner drop requests termination, never
//! resets the fence, and never controls a replacement dispatch. A request is
//! not proof of settlement; the worker must still reap its owned processes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Identity-bearing cancellation fence for one native dispatch.
#[derive(Debug, Clone)]
pub(crate) struct NativeActionCancellation(Arc<AtomicBool>);

impl PartialEq for NativeActionCancellation {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for NativeActionCancellation {}

impl NativeActionCancellation {
    /// Creates an uncancelled fence for a new exact dispatch.
    pub(crate) fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// Requests cancellation without claiming that effects have settled.
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Shares the monotonic flag with process and probe wait loops.
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }

    /// Waits without holding the actor; the bounded poll also handles cancellation
    /// admitted before the async owner starts awaiting this fence.
    pub(crate) async fn cancelled(&self) {
        while !self.0.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Creates an owner guard that requests cancellation on every exit path.
    pub(crate) fn cancel_on_drop(&self) -> NativeActionCancellationGuard {
        NativeActionCancellationGuard(self.clone())
    }
}

/// Async-owner guard; dropping a blocking JoinHandle alone cannot stop work.
pub(crate) struct NativeActionCancellationGuard(NativeActionCancellation);

impl Drop for NativeActionCancellationGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
