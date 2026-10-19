//! Finite per-source ingress order for native callback work through actor commit.
//!
//! Workers may wait only until the existing admission deadline. Actor completion
//! releases the ticket after effects/rejection, not after native observation.
//! Last-lease drop closes cancellation holes; rejected later tickets are skipped
//! only after their predecessors finish. No actor waits or callback effects occur
//! under this lock. Pending ticket memory is bounded by native admission capacity.

use super::*;
use std::sync::{
    Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};

/// Shared original-source queue, independent of observer or presentation IDs.
#[derive(Debug, Default)]
pub(super) struct CodexOrder {
    state: Mutex<State>,
    wake: Condvar,
}

/// Monotonic issued/serving tickets and bounded out-of-order cancellation holes.
#[derive(Debug, Default)]
struct State {
    issued: u64,
    serving: u64,
    /// Coalesced cancellation intervals; only outstanding work can split them.
    finished: std::collections::BTreeMap<u64, u64>,
}

/// One work-owned ticket; clones share its terminal release, never another turn.
#[derive(Debug)]
pub(super) struct CodexOrderLease {
    queue: Arc<CodexOrder>,
    ticket: u64,
    finished: AtomicBool,
    #[cfg(all(test, target_os = "linux"))]
    waiting: OnceLock<Arc<tokio::sync::Notify>>,
}

impl CodexOrder {
    /// Assigns ingress order under actor admission. Poison recovery is safe:
    /// this lock contains only checked arithmetic/set operations, no callbacks.
    pub(super) fn reserve(self: &Arc<Self>) -> Result<Arc<CodexOrderLease>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ticket = state.issued;
        state.issued = state
            .issued
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("Codex callback order exhausted"))?;
        Ok(Arc::new(CodexOrderLease {
            queue: self.clone(),
            ticket,
            finished: AtomicBool::new(false),
            #[cfg(all(test, target_os = "linux"))]
            waiting: OnceLock::new(),
        }))
    }
}

impl CodexOrderLease {
    /// Waits off actor for predecessors' full settlement, within native budget.
    pub(super) fn wait(&self, deadline: Instant) -> Result<()> {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if self.finished.load(Ordering::Acquire) || state.serving > self.ticket {
                return Err(MezError::conflict("Codex callback order retired"));
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| MezError::conflict("Codex callback order expired"))?;
            if state.serving == self.ticket {
                return Ok(());
            }
            #[cfg(all(test, target_os = "linux"))]
            if let Some(waiting) = self.waiting.get() {
                waiting.notify_one();
            }
            let (next, _) = self
                .queue
                .wake
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
        }
    }

    /// Nonwaiting actor check: only the current uncompleted ticket may commit.
    pub(super) fn is_serving(&self) -> bool {
        !self.finished.load(Ordering::Acquire)
            && self
                .queue
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .serving
                == self.ticket
    }

    /// Releases once after actor settlement or final work cancellation. A later
    /// rejected ticket cannot unblock itself ahead of the current predecessor.
    fn finish(&self) {
        if self.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut start = self.ticket;
        let mut end = self.ticket;
        if let Some((&left, &right)) = state.finished.range(..=start).next_back()
            && right.saturating_add(1) >= start
        {
            start = left;
            end = end.max(right);
            state.finished.remove(&left);
        }
        if let Some((&left, &right)) = state.finished.range(start..).next()
            && left <= end.saturating_add(1)
        {
            end = end.max(right);
            state.finished.remove(&left);
        }
        state.finished.insert(start, end);
        if let Some((&left, &right)) = state.finished.first_key_value()
            && left == state.serving
        {
            state.finished.remove(&left);
            state.serving = right + 1; // Reserved tickets are strictly below u64::MAX.
        }
        self.queue.wake.notify_all();
    }

    /// Deterministic test notification when a later worker actually blocks.
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn waiting_for_tests(&self) -> Arc<tokio::sync::Notify> {
        self.waiting
            .get_or_init(|| Arc::new(tokio::sync::Notify::new()))
            .clone()
    }
}

impl Drop for CodexOrderLease {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Scope guard keeps ordering held through every actor result/early return.
pub(super) struct CodexOrderSettlement(pub(super) Option<Arc<CodexOrderLease>>);
impl Drop for CodexOrderSettlement {
    fn drop(&mut self) {
        if let Some(lease) = &self.0 {
            lease.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A stalled original predecessor cannot make later cancellation memory
    /// grow with request volume. Coalesced holes remain bounded and release only
    /// after predecessor settlement; cancelled clones cannot observe afterward.
    #[test]
    fn external_codex_order_cancellation_holes_remain_bounded() {
        let queue = Arc::new(CodexOrder::default());
        let first = queue.reserve().unwrap();
        for _ in 0..10_000 {
            drop(queue.reserve().unwrap());
        }
        assert_eq!(queue.state.lock().unwrap().finished.len(), 1);
        let last = queue.reserve().unwrap();
        assert!(last.wait(Instant::now()).is_err());
        drop(CodexOrderSettlement(Some(first.clone())));
        assert!(first.wait(Instant::now() + Duration::from_secs(1)).is_err());
        assert!(last.wait(Instant::now() + Duration::from_secs(1)).is_ok());
        assert!(queue.state.lock().unwrap().finished.is_empty());
    }

    /// A later ticket times out while its predecessor remains live without
    /// becoming current. Dropping that failed work skips its hole only after
    /// original settlement, allowing the following ticket to proceed normally.
    #[test]
    fn external_codex_order_wait_timeout_does_not_overtake_predecessor() {
        let queue = Arc::new(CodexOrder::default());
        let first = queue.reserve().unwrap();
        let expired = queue.reserve().unwrap();
        assert!(
            expired
                .wait(Instant::now() + Duration::from_millis(1))
                .is_err()
        );
        assert!(first.is_serving());
        assert!(!expired.is_serving());
        drop(expired);
        let next = queue.reserve().unwrap();
        assert!(!next.is_serving());
        drop(CodexOrderSettlement(Some(first)));
        assert!(next.wait(Instant::now() + Duration::from_secs(1)).is_ok());
    }

    /// Cancelled intervals on both sides of a live middle ticket must merge
    /// when that ticket is dropped, without advancing past the still-live head.
    /// Final head settlement releases the exact next ticket and clears holes.
    #[test]
    fn external_codex_order_multiple_cancellation_intervals_merge() {
        let queue = Arc::new(CodexOrder::default());
        let first = queue.reserve().unwrap();
        drop(queue.reserve().unwrap());
        let middle = queue.reserve().unwrap();
        drop(queue.reserve().unwrap());
        let last = queue.reserve().unwrap();
        assert_eq!(queue.state.lock().unwrap().finished.len(), 2);
        drop(middle);
        assert_eq!(queue.state.lock().unwrap().finished.len(), 1);
        assert!(!last.is_serving());
        drop(CodexOrderSettlement(Some(first)));
        assert!(last.is_serving());
        assert!(queue.state.lock().unwrap().finished.is_empty());
    }
}
