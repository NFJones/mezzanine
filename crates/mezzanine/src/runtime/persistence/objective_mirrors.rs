//! Optional objective display-cache ownership, never turn/durability authority.
//!
//! The actor keeps at most 256 captured store/CID keys and one physical worker
//! reservation. Pending publications coalesce to the latest bounded value,
//! including clear. A reservation is not freed by dropping a Tokio JoinHandle:
//! only the actual work's completion/drop publishes a retained receipt, and the
//! actor must consume that receipt before another physical job is issued.
//! Lock contention uses nonblocking admission and a bounded cadence. Filesystem
//! operations themselves can still stall indefinitely; this owner bounds jobs
//! and protects actor/essential persistence progress, not physical I/O latency.
//!
//! Revisions order this runtime only. Different runtimes/processes sharing a
//! store retain cache last-lock-admitted-writer semantics, not a fabricated
//! global order from local revisions or seconds timestamps.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use crate::runtime::RuntimeSessionService;
use crate::session_title::bound_session_title;
use crate::storage::transcript::AgentTranscriptStore;

/// Global per-runtime key bound, including suppression and in-flight records.
const MAX_OBJECTIVE_MIRROR_KEYS: usize = 256;
/// Busy locks never cause immediate retries or occupy an essential worker wait.
const OBJECTIVE_MIRROR_BUSY_DELAY: Duration = Duration::from_millis(250);
/// Exact captured root and conversation, never looked up from a rebound pane.
type MirrorKey = (PathBuf, String);

/// Content-free cache degradation; none of these outcomes fail a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MirrorFailure {
    Io,
    Abandoned,
    Capacity,
    RevisionExhausted,
}

/// Bounded in-memory diagnostics; no title, identifier, raw error or private path.
#[derive(Debug, Default)]
struct MirrorStatus {
    confirmed: u64,
    busy: u64,
    failed: u64,
    refused: u64,
    last_failure: Option<MirrorFailure>,
}

/// One newest desired value and its own exact in-flight revision, if any.
#[derive(Debug)]
struct MirrorEntry {
    store: AgentTranscriptStore,
    desired: Option<String>,
    revision: u64,
    token: Arc<AtomicU64>,
    dirty: bool,
    confirmed: bool,
    in_flight: Option<u64>,
    queued_at: u64,
    eligible_at: Instant,
    updated_at_unix_seconds: u64,
}

/// Worker result without provider/objective/error text.
#[derive(Debug)]
enum MirrorResult {
    Written(bool),
    Busy,
    Failed(MirrorFailure),
}

/// One exact receipt retained even when its launching service was cancelled.
#[derive(Debug)]
struct MirrorReceipt {
    key: MirrorKey,
    revision: u64,
    result: MirrorResult,
}

/// Physical reservation and retained receipt share one slot, avoiding a gap in
/// which a replacement worker could overwrite an unconsumed result.
#[derive(Debug, Default)]
enum PhysicalSlot {
    #[default]
    Idle,
    Reserved(u64),
    Completed(MirrorReceipt),
}

/// Actor-owned coalescing queue plus shared physical worker lifetime evidence.
#[derive(Debug, Default)]
pub(super) struct ObjectiveMirrorQueue {
    entries: BTreeMap<MirrorKey, MirrorEntry>,
    next_revision: u64,
    slot: Arc<Mutex<PhysicalSlot>>,
    status: MirrorStatus,
}

impl ObjectiveMirrorQueue {
    /// Drops pending old-store revisions without resetting the monotonic issue
    /// identity or releasing a physical operation that may already be syncing.
    pub(super) fn invalidate(&mut self) {
        for entry in self.entries.values() {
            entry.token.store(0, Ordering::Release);
        }
        self.entries.clear();
    }

    /// Publishes bounded desired work. False means unchanged/refused, not a
    /// claim that the cache is durable. Failed identical values stay retryable.
    fn publish(
        &mut self,
        store: AgentTranscriptStore,
        conversation: &str,
        objective: Option<&str>,
    ) -> bool {
        let key = (store.root().to_path_buf(), conversation.to_string());
        let desired = objective.and_then(bound_session_title);
        if self.entries.get(&key).is_some_and(|entry| {
            entry.desired == desired
                && (entry.dirty || entry.in_flight.is_some() || entry.confirmed)
        }) {
            return false;
        }
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_OBJECTIVE_MIRROR_KEYS {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, entry)| !entry.dirty && entry.in_flight.is_none())
                .min_by_key(|(_, entry)| entry.queued_at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                if let Some(entry) = self.entries.remove(&oldest) {
                    entry.token.store(0, Ordering::Release);
                }
            } else {
                self.status.refused = self.status.refused.saturating_add(1);
                self.status.last_failure = Some(MirrorFailure::Capacity);
                return false;
            }
        }
        let Some(revision) = self.next_revision.checked_add(1) else {
            self.status.refused = self.status.refused.saturating_add(1);
            self.status.last_failure = Some(MirrorFailure::RevisionExhausted);
            return false;
        };
        self.next_revision = revision;
        let now = Instant::now();
        let entry = self.entries.entry(key).or_insert_with(|| MirrorEntry {
            store,
            desired: None,
            revision,
            token: Arc::new(AtomicU64::new(revision)),
            dirty: false,
            confirmed: false,
            in_flight: None,
            queued_at: revision,
            eligible_at: now,
            updated_at_unix_seconds: 0,
        });
        if !entry.dirty {
            entry.queued_at = revision;
        }
        entry.desired = desired;
        entry.revision = revision;
        entry.token.store(revision, Ordering::Release);
        entry.dirty = true;
        entry.confirmed = false;
        entry.eligible_at = now;
        entry.updated_at_unix_seconds = crate::runtime::current_unix_seconds();
        true
    }

    /// Collects a receipt using try_lock only; worker mutexes never span I/O.
    /// Stale receipts release their slot but cannot confirm or erase new work.
    fn collect(&mut self) -> Option<MirrorKey> {
        let receipt = {
            let mut slot = match self.slot.try_lock() {
                Ok(slot) => slot,
                Err(TryLockError::Poisoned(error)) => error.into_inner(),
                Err(TryLockError::WouldBlock) => return None,
            };
            if !matches!(*slot, PhysicalSlot::Completed(_)) {
                return None;
            }
            let PhysicalSlot::Completed(receipt) = std::mem::take(&mut *slot) else {
                return None;
            };
            receipt
        };
        let Some(entry) = self.entries.get_mut(&receipt.key) else {
            return None;
        };
        if entry.in_flight == Some(receipt.revision) {
            entry.in_flight = None;
        }
        if entry.revision != receipt.revision {
            return None;
        }
        match receipt.result {
            MirrorResult::Written(changed) => {
                entry.confirmed = true;
                entry.dirty = false;
                self.status.confirmed = self.status.confirmed.saturating_add(1);
                changed.then_some(receipt.key)
            }
            MirrorResult::Busy => {
                entry.dirty = true;
                entry.queued_at = self.next_revision;
                entry.eligible_at = Instant::now() + OBJECTIVE_MIRROR_BUSY_DELAY;
                self.status.busy = self.status.busy.saturating_add(1);
                None
            }
            MirrorResult::Failed(reason) => {
                // A cancelled handoff/panicking optional job is recoverable by
                // a replacement service without an unrelated publication.
                // Real filesystem failures wait for an explicit new refresh.
                entry.dirty = reason == MirrorFailure::Abandoned;
                if entry.dirty {
                    entry.queued_at = self.next_revision;
                    entry.eligible_at = Instant::now() + OBJECTIVE_MIRROR_BUSY_DELAY;
                }
                entry.confirmed = false;
                self.status.failed = self.status.failed.saturating_add(1);
                self.status.last_failure = Some(reason);
                None
            }
        }
    }

    /// Reserves at most one physical job; all fairness/key work is bounded by
    /// MAX_OBJECTIVE_MIRROR_KEYS. The slot remains busy until receipt collection.
    fn claim(&mut self) -> Option<RuntimeObjectiveMirrorWork> {
        let now = Instant::now();
        let key = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry.dirty && entry.in_flight.is_none() && entry.eligible_at <= now
            })
            .min_by_key(|(_, entry)| entry.queued_at)
            .map(|(key, _)| key.clone())?;
        let mut slot = match self.slot.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        if !matches!(*slot, PhysicalSlot::Idle) {
            return None;
        }
        let entry = self.entries.get_mut(&key)?;
        *slot = PhysicalSlot::Reserved(entry.revision);
        entry.in_flight = Some(entry.revision);
        entry.dirty = false;
        Some(RuntimeObjectiveMirrorWork {
            key,
            store: entry.store.clone(),
            revision: entry.revision,
            objective: entry.desired.clone(),
            updated_at_unix_seconds: entry.updated_at_unix_seconds,
            token: entry.token.clone(),
            slot: self.slot.clone(),
            completed: false,
            #[cfg(test)]
            before_write: None,
        })
    }
}

/// Immutable optional cache work; execute only on the existing blocking pool.
/// Its guard publishes a typed abandoned receipt on queued cancellation/panic,
/// never releases another generation and never pretends sync was cancelled.
pub(crate) struct RuntimeObjectiveMirrorWork {
    key: MirrorKey,
    store: AgentTranscriptStore,
    revision: u64,
    objective: Option<String>,
    updated_at_unix_seconds: u64,
    token: Arc<AtomicU64>,
    slot: Arc<Mutex<PhysicalSlot>>,
    completed: bool,
    /// Bounded fixture gate after the last revision observation, before write.
    /// It represents the unavoidable preemption/sync window, not a timeout or
    /// alternate production storage policy.
    #[cfg(test)]
    before_write: Option<(
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
}

impl RuntimeObjectiveMirrorWork {
    /// Performs finite lock admission and the unchanged atomic write owner.
    /// Physical filesystem latency is deliberately not described as bounded.
    pub(crate) fn execute(mut self) {
        #[cfg(test)]
        let observations = std::cell::Cell::new(0usize);
        let result = self.store.try_mirror_session_objective_if_current(
            &self.key.1,
            self.objective.as_deref().unwrap_or_default(),
            self.updated_at_unix_seconds,
            || {
                let current = self.token.load(Ordering::Acquire) == self.revision;
                #[cfg(test)]
                if observations.replace(observations.get() + 1) == 1
                    && current
                    && let Some((entered, release)) = &self.before_write
                {
                    let _ = entered.try_send(());
                    let _ = release.recv_timeout(Duration::from_secs(3));
                }
                current
            },
        );
        self.complete(match result {
            Ok(Some(changed)) => MirrorResult::Written(changed),
            Ok(None) => MirrorResult::Busy,
            Err(_) => MirrorResult::Failed(MirrorFailure::Io),
        });
    }

    /// Worker-side short receipt publication; no I/O or actor wait under mutex.
    fn complete(&mut self, result: MirrorResult) {
        let mut slot = self.slot.lock().unwrap_or_else(|error| error.into_inner());
        if matches!(*slot, PhysicalSlot::Reserved(revision) if revision == self.revision) {
            *slot = PhysicalSlot::Completed(MirrorReceipt {
                key: self.key.clone(),
                revision: self.revision,
                result,
            });
        }
        self.completed = true;
    }
}

impl Drop for RuntimeObjectiveMirrorWork {
    /// A not-started claim can be dropped by reply cancellation. Reservation
    /// construction releases its mutex before handoff, and there is no other
    /// worker for this slot; completion never overwrites another receipt.
    fn drop(&mut self) {
        if !self.completed {
            self.complete(MirrorResult::Failed(MirrorFailure::Abandoned));
        }
    }
}

impl RuntimeSessionService {
    /// Queues optional mirror work without filesystem access or durable claims.
    pub(crate) fn queue_runtime_objective_mirror(
        &mut self,
        store: AgentTranscriptStore,
        conversation: &str,
        objective: Option<&str>,
    ) -> bool {
        self.persistence
            .objective_mirrors
            .publish(store, conversation, objective)
    }

    /// Called by the persistence service even during objective-only idle polls.
    /// Browser work is actor-owned and already uses an off-actor refresh claim.
    pub(crate) fn poll_runtime_objective_mirror(
        &mut self,
        allow_claim: bool,
    ) -> Option<RuntimeObjectiveMirrorWork> {
        if let Some((root, _)) = self.persistence.objective_mirrors.collect()
            && self
                .persistence
                .transcript_store()
                .is_some_and(|store| store.root() == root)
        {
            self.invalidate_agent_prompt_selector_extra_candidates();
            let _ = self.refresh_saved_session_overlay_after_title_change();
        }
        allow_claim
            .then(|| self.persistence.objective_mirrors.claim())
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::fs::{FlockOperation, flock};

    /// Creates only an owned file-store fixture, never a native child/provider.
    fn store() -> (AgentTranscriptStore, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("mez-mirror-owner-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&root).unwrap();
        (AgentTranscriptStore::new(root.clone()), root)
    }

    /// Executes one exact claimed job and consumes its receipt explicitly.
    fn settle(queue: &mut ObjectiveMirrorQueue) {
        queue.claim().expect("eligible exact mirror job").execute();
        queue.collect();
    }

    /// A -> B -> A cannot suppress final A because last persisted was A while B
    /// was in flight. Clear is an ordered value and unchanged confirmed values
    /// do not perform additional filesystem reads/writes.
    #[test]
    fn objective_mirror_round_trip_and_clear_follow_current_desired_revision() {
        let (store, root) = store();
        let mut queue = ObjectiveMirrorQueue::default();
        assert!(queue.publish(store.clone(), "same", Some("A")));
        settle(&mut queue);
        assert!(queue.publish(store.clone(), "same", Some("B")));
        let old = queue.claim().unwrap();
        assert!(queue.publish(store.clone(), "same", Some("A")));
        assert!(queue.claim().is_none());
        old.execute();
        assert!(
            queue.claim().is_none(),
            "unconsumed receipt retains physical slot"
        );
        assert!(
            queue.collect().is_none(),
            "stale B cannot confirm current A"
        );
        settle(&mut queue);
        assert_eq!(
            store
                .session_objective_mirror("same")
                .unwrap()
                .unwrap()
                .objective,
            "A"
        );
        let writes = store.session_objective_mirror_status().index_writes;
        assert!(!queue.publish(store.clone(), "same", Some("A")));
        assert!(queue.claim().is_none());
        assert_eq!(store.session_objective_mirror_status().index_writes, writes);
        assert!(queue.publish(store.clone(), "same", None));
        settle(&mut queue);
        assert!(store.session_objective_mirror("same").unwrap().is_none());
        assert!(!queue.publish(store, "same", None));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The physical worker can be past its final revision observation when a
    /// newer clear arrives. Its admitted write is not falsely cancelled; the
    /// sole retained slot prevents a replacement worker, then clear converges.
    #[test]
    fn objective_mirror_clear_converges_after_already_admitted_old_write() {
        let (store, root) = store();
        let mut queue = ObjectiveMirrorQueue::default();
        queue.publish(store.clone(), "same", Some("older"));
        let mut old = queue.claim().unwrap();
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, gate) = std::sync::mpsc::sync_channel(1);
        old.before_write = Some((entered, gate));
        let worker = std::thread::spawn(move || old.execute());
        let reached = waiting.recv_timeout(Duration::from_secs(2));
        let queued = queue.publish(store.clone(), "same", None);
        let replacement = queue.claim();
        let _ = release.try_send(());
        let joined = worker.join();
        // Gate always releases before any assertion; worker also has its own
        // bounded OS-thread timeout if the test unwinds before explicit release.
        reached.unwrap();
        joined.unwrap();
        assert!(queued);
        assert!(replacement.is_none());
        assert!(
            queue.collect().is_none(),
            "old result cannot acknowledge clear"
        );
        assert_eq!(
            store
                .session_objective_mirror("same")
                .unwrap()
                .unwrap()
                .objective,
            "older"
        );
        settle(&mut queue);
        assert!(store.session_objective_mirror("same").unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A failed filesystem update is not remembered as persisted/suppressed.
    /// An explicit identical refresh can retry after storage becomes available;
    /// diagnostics retain only a typed cause, never objective or root content.
    #[test]
    fn objective_mirror_io_failure_keeps_identical_refresh_retryable() {
        let (_, root) = store();
        let child = root.join("blocked");
        std::fs::write(&child, b"not a directory").unwrap();
        let store = AgentTranscriptStore::new(child.clone());
        let mut queue = ObjectiveMirrorQueue::default();
        queue.publish(store.clone(), "same", Some("private objective sentinel"));
        settle(&mut queue);
        assert_eq!(queue.status.failed, 1);
        assert_eq!(queue.status.last_failure, Some(MirrorFailure::Io));
        assert!(!format!("{:?}", queue.status).contains("sentinel"));
        assert!(!format!("{:?}", queue.status).contains(&root.display().to_string()));
        assert!(queue.claim().is_none(), "I/O failure must not spin-retry");
        std::fs::remove_file(&child).unwrap();
        std::fs::create_dir(&child).unwrap();
        assert!(queue.publish(store.clone(), "same", Some("private objective sentinel")));
        settle(&mut queue);
        assert!(store.session_objective_mirror("same").unwrap().is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Store replacement invalidates old tokens without resetting issued IDs.
    /// A late same-root/CID receipt cannot clear or confirm the new owner, and
    /// same CID in two different roots remains separate bounded work.
    #[test]
    fn objective_mirror_store_replacement_and_distinct_roots_have_no_aba() {
        let (first, root) = store();
        let (second, other) = store();
        let mut queue = ObjectiveMirrorQueue::default();
        queue.publish(first.clone(), "same", Some("old"));
        let old = queue.claim().unwrap();
        let old_revision = old.revision;
        queue.invalidate();
        queue.publish(first.clone(), "same", Some("new"));
        queue.publish(second.clone(), "same", Some("other"));
        old.execute();
        assert!(queue.collect().is_none());
        let current = queue.claim().unwrap();
        assert!(current.revision > old_revision);
        current.execute();
        queue.collect();
        settle(&mut queue);
        assert_eq!(
            first
                .session_objective_mirror("same")
                .unwrap()
                .unwrap()
                .objective,
            "new"
        );
        assert_eq!(
            second
                .session_objective_mirror("same")
                .unwrap()
                .unwrap()
                .objective,
            "other"
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(other).unwrap();
    }

    /// Pending keys are globally bounded, not just one slot per unbounded CID.
    /// Full admission remains truthful and does not suppress an identical retry
    /// after a slot becomes clean; existing-key newest coalescing still works.
    #[test]
    fn objective_mirror_capacity_refusal_does_not_claim_convergence() {
        let (store, root) = store();
        let mut queue = ObjectiveMirrorQueue::default();
        for index in 0..MAX_OBJECTIVE_MIRROR_KEYS {
            assert!(queue.publish(store.clone(), &format!("cid-{index}"), Some("first")));
        }
        assert!(!queue.publish(store.clone(), "overflow", Some("latest")));
        assert_eq!(queue.entries.len(), MAX_OBJECTIVE_MIRROR_KEYS);
        assert_eq!(queue.status.refused, 1);
        assert_eq!(queue.status.last_failure, Some(MirrorFailure::Capacity));
        assert!(queue.publish(store.clone(), "cid-0", Some("newest existing")));
        settle(&mut queue);
        assert!(queue.publish(store, "overflow", Some("latest")));
        assert_eq!(queue.entries.len(), MAX_OBJECTIVE_MIRROR_KEYS);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A dropped unstarted handoff publishes exactly one bounded receipt. A
    /// replacement service first consumes it, observes backoff, then can recover
    /// without new publication or release of somebody else's generation.
    #[test]
    fn objective_mirror_abandoned_claim_retains_receipt_until_collection() {
        let (store, root) = store();
        let mut queue = ObjectiveMirrorQueue::default();
        queue.publish(store.clone(), "same", Some("latest"));
        drop(queue.claim().unwrap());
        assert!(queue.claim().is_none());
        queue.collect();
        assert_eq!(queue.status.last_failure, Some(MirrorFailure::Abandoned));
        assert!(
            queue.claim().is_none(),
            "replacement must respect retry cadence"
        );
        queue.entries.values_mut().next().unwrap().eligible_at = Instant::now();
        settle(&mut queue);
        assert_eq!(
            store
                .session_objective_mirror("same")
                .unwrap()
                .unwrap()
                .objective,
            "latest"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Actual lock admission is finite and busy is not successful persistence.
    /// The desired value remains pending with a nonzero retry cadence, and can
    /// later settle after release without a second publication.
    #[test]
    fn objective_mirror_busy_lock_remains_pending_without_a_blocking_wait() {
        let (store, root) = store();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(".session-objectives.json.lock"))
            .unwrap();
        flock(&lock, FlockOperation::LockExclusive).unwrap();
        let mut queue = ObjectiveMirrorQueue::default();
        queue.publish(store.clone(), "same", Some("latest"));
        settle(&mut queue);
        assert_eq!(queue.status.busy, 1);
        assert_eq!(queue.status.confirmed, 0);
        assert!(queue.claim().is_none());
        drop(lock);
        queue.entries.values_mut().next().unwrap().eligible_at = Instant::now();
        settle(&mut queue);
        assert_eq!(
            store
                .session_objective_mirror("same")
                .unwrap()
                .unwrap()
                .objective,
            "latest"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
