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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    Registration,
    RevisionExhausted,
    SchedulingExhausted,
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

/// Content-free actor snapshot of optional objective-cache ownership.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RuntimeObjectiveMirrorStatus {
    /// Latest desired keys still awaiting an eligible write.
    pub pending_keys: usize,
    /// All bounded keys, including unchanged-value suppression records.
    pub tracked_keys: usize,
    /// Idle, in_flight, receipt_pending, or observing (short mutex contention).
    /// Receipt custody is not a claim that physical I/O is still running.
    pub slot_phase: &'static str,
    /// Exact current-revision cache confirmations, not transcript receipts.
    pub confirmed: u64,
    /// Nonblocking filesystem lock refusals.
    pub busy: u64,
    /// Terminal I/O or abandoned-work observations.
    pub failed: u64,
    /// Desired updates refused by bounded admission.
    pub refused: u64,
    /// Fixed bounded reason only; never a raw error, path, identifier or title.
    pub last_failure: Option<&'static str>,
}

impl Default for RuntimeObjectiveMirrorStatus {
    /// Represents an unused optional cache lane.
    fn default() -> Self {
        Self {
            pending_keys: 0,
            tracked_keys: 0,
            slot_phase: "idle",
            confirmed: 0,
            busy: 0,
            failed: 0,
            refused: 0,
            last_failure: None,
        }
    }
}

/// One newest desired value and its own exact in-flight revision, if any.
#[derive(Debug)]
struct MirrorEntry {
    store: AgentTranscriptStore,
    desired: Option<String>,
    revision: u64,
    token: Arc<AtomicU64>,
    /// Shared-store deletion fence, distinct from local publication revision.
    registration: Arc<AtomicBool>,
    dirty: bool,
    confirmed: bool,
    /// An admitted old write changed disk; refresh only on current confirmation.
    refresh_needed: bool,
    in_flight: Option<u64>,
    queued_at: u128,
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
    registration: Arc<AtomicBool>,
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
    /// Fair attempt ordering is independent from publication revisions.
    next_queue_order: u128,
    slot: Arc<Mutex<PhysicalSlot>>,
    status: MirrorStatus,
    /// One bounded fixture probe, never a production scheduling input.
    #[cfg(test)]
    next_before_write: Option<(
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
        Arc<std::sync::atomic::AtomicBool>,
    )>,
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
        let key = (
            store.objective_mirror_store_root().to_path_buf(),
            conversation.to_string(),
        );
        let desired = objective.and_then(bound_session_title);
        if self.entries.get(&key).is_some_and(|entry| {
            entry.registration.load(Ordering::Acquire)
                && entry.desired == desired
                && (entry.dirty || entry.in_flight.is_some() || entry.confirmed)
        }) {
            return false;
        }
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_OBJECTIVE_MIRROR_KEYS {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, entry)| {
                    !entry.dirty && !entry.refresh_needed && entry.in_flight.is_none()
                })
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
        let registration = self
            .entries
            .get(&key)
            .filter(|entry| entry.registration.load(Ordering::Acquire))
            .map(|entry| entry.registration.clone())
            .or_else(|| store.register_session_objective_mirror_work(conversation));
        let Some(registration) = registration else {
            self.status.refused = self.status.refused.saturating_add(1);
            self.status.last_failure = Some(MirrorFailure::Registration);
            return false;
        };
        let Some(revision) = self.next_revision.checked_add(1) else {
            self.status.refused = self.status.refused.saturating_add(1);
            self.status.last_failure = Some(MirrorFailure::RevisionExhausted);
            return false;
        };
        self.next_revision = revision;
        let Some(queued_at) = self.next_queue_order.checked_add(1) else {
            self.status.refused = self.status.refused.saturating_add(1);
            self.status.last_failure = Some(MirrorFailure::SchedulingExhausted);
            return false;
        };
        self.next_queue_order = queued_at;
        let now = Instant::now();
        let entry = self.entries.entry(key).or_insert_with(|| MirrorEntry {
            store: store.clone(),
            desired: None,
            revision,
            token: Arc::new(AtomicU64::new(revision)),
            registration: registration.clone(),
            dirty: false,
            confirmed: false,
            refresh_needed: false,
            in_flight: None,
            queued_at,
            eligible_at: now,
            updated_at_unix_seconds: 0,
        });
        if !Arc::ptr_eq(&entry.registration, &registration) {
            entry.registration = registration;
            entry.store = store;
            entry.refresh_needed = false;
        }
        if !entry.dirty {
            entry.queued_at = queued_at;
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
        // One physical attempt is reported exactly once, even when a newer
        // desired revision or a deletion has already superseded its receipt.
        // The freshness gates below are allowed to reject confirmation, retry
        // and UI refresh, but a filesystem failure/cancelled job must stay
        // visible as bounded content-free diagnostics.
        if let MirrorResult::Failed(reason) = &receipt.result {
            self.status.failed = self.status.failed.saturating_add(1);
            self.status.last_failure = Some(*reason);
        }
        let entry = self.entries.get_mut(&receipt.key)?;
        if entry.in_flight != Some(receipt.revision) {
            return None;
        }
        entry.in_flight = None;
        if !entry.registration.load(Ordering::Acquire) {
            self.entries.remove(&receipt.key);
            return None;
        }
        if !Arc::ptr_eq(&entry.registration, &receipt.registration) {
            return None;
        }
        if matches!(receipt.result, MirrorResult::Written(true)) {
            entry.refresh_needed = true;
        }
        if entry.revision != receipt.revision {
            return None;
        }
        let retry_order = self.next_queue_order.checked_add(1);
        if let Some(order) = retry_order {
            self.next_queue_order = order;
        }
        match receipt.result {
            MirrorResult::Written(changed) => {
                entry.confirmed = true;
                entry.dirty = false;
                self.status.confirmed = self.status.confirmed.saturating_add(1);
                let refresh = changed || entry.refresh_needed;
                entry.refresh_needed = false;
                refresh.then_some(receipt.key)
            }
            MirrorResult::Busy => {
                entry.dirty = retry_order.is_some();
                if let Some(order) = retry_order {
                    entry.queued_at = order;
                } else {
                    self.status.last_failure = Some(MirrorFailure::SchedulingExhausted);
                }
                entry.eligible_at = Instant::now() + OBJECTIVE_MIRROR_BUSY_DELAY;
                self.status.busy = self.status.busy.saturating_add(1);
                None
            }
            MirrorResult::Failed(reason) => {
                // A cancelled handoff/panicking optional job is recoverable by
                // a replacement service without an unrelated publication.
                // Real filesystem failures wait for an explicit new refresh.
                entry.dirty = reason == MirrorFailure::Abandoned && retry_order.is_some();
                if entry.dirty {
                    entry.queued_at = retry_order.unwrap_or(entry.queued_at);
                    entry.eligible_at = Instant::now() + OBJECTIVE_MIRROR_BUSY_DELAY;
                }
                entry.confirmed = false;
                // This failure was already counted when its receipt was
                // consumed; only retry/confirmation depends on freshness.
                None
            }
        }
    }

    /// Reserves at most one physical job; all fairness/key work is bounded by
    /// MAX_OBJECTIVE_MIRROR_KEYS. The slot remains busy until receipt collection.
    fn claim(&mut self) -> Option<RuntimeObjectiveMirrorWork> {
        // Revocation also retires pending/confirmed suppression records. Keep
        // physical custody until its receipt arrives, even after deletion.
        self.entries.retain(|_, entry| {
            entry.registration.load(Ordering::Acquire) || entry.in_flight.is_some()
        });
        let now = Instant::now();
        let key = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry.registration.load(Ordering::Acquire)
                    && entry.dirty
                    && entry.in_flight.is_none()
                    && entry.eligible_at <= now
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
            registration: entry.registration.clone(),
            slot: self.slot.clone(),
            completed: false,
            #[cfg(test)]
            before_write: self.next_before_write.take(),
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
    /// Remains retained through actual completion, including launcher cancel.
    registration: Arc<AtomicBool>,
    slot: Arc<Mutex<PhysicalSlot>>,
    completed: bool,
    /// Bounded fixture gate after the last revision observation, before write.
    /// It represents the unavoidable preemption/sync window, not a timeout or
    /// alternate production storage policy.
    #[cfg(test)]
    before_write: Option<(
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
        Arc<std::sync::atomic::AtomicBool>,
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
                let current = self.token.load(Ordering::Acquire) == self.revision
                    && self.registration.load(Ordering::Acquire);
                #[cfg(test)]
                if observations.replace(observations.get() + 1) == 1
                    && current
                    && let Some((entered, release, released)) = &self.before_write
                {
                    let _ = entered.try_send(());
                    let _ = release.recv_timeout(Duration::from_secs(3));
                    released.store(true, Ordering::Release);
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
                registration: self.registration.clone(),
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
    /// Returns only bounded memory observations. In particular this must not
    /// call the filesystem-backed transcript-store mirror status accessor.
    pub(crate) fn runtime_objective_mirror_status(&self) -> RuntimeObjectiveMirrorStatus {
        let queue = &self.persistence.objective_mirrors;
        let slot_phase = match queue.slot.try_lock() {
            Ok(slot) => match &*slot {
                PhysicalSlot::Idle => "idle",
                PhysicalSlot::Reserved(_) => "in_flight",
                PhysicalSlot::Completed(_) => "receipt_pending",
            },
            Err(_) => "observing",
        };
        RuntimeObjectiveMirrorStatus {
            pending_keys: queue.entries.values().filter(|entry| entry.dirty).count(),
            tracked_keys: queue.entries.len(),
            slot_phase,
            confirmed: queue.status.confirmed,
            busy: queue.status.busy,
            failed: queue.status.failed,
            refused: queue.status.refused,
            last_failure: queue.status.last_failure.map(|reason| match reason {
                MirrorFailure::Io => "io",
                MirrorFailure::Abandoned => "abandoned",
                MirrorFailure::Capacity => "capacity",
                MirrorFailure::Registration => "registration",
                MirrorFailure::RevisionExhausted => "revision_exhausted",
                MirrorFailure::SchedulingExhausted => "scheduling_exhausted",
            }),
        }
    }

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
                .is_some_and(|store| store.objective_mirror_store_root() == root)
        {
            self.invalidate_agent_prompt_selector_extra_candidates();
            let _ = self.refresh_saved_session_overlay_after_title_change();
        }
        allow_claim
            .then(|| self.persistence.objective_mirrors.claim())
            .flatten()
    }

    /// Explicit non-actor fixture driver; never a synchronous production fallback.
    #[cfg(test)]
    pub(crate) fn flush_objective_mirrors_for_tests(&mut self) {
        for _ in 0..=MAX_OBJECTIVE_MIRROR_KEYS {
            let Some(work) = self.poll_runtime_objective_mirror(true) else {
                break;
            };
            work.execute();
        }
    }

    /// Observes physical receipt/queue quiescence without filesystem access.
    #[cfg(test)]
    pub(crate) fn objective_mirrors_are_idle_for_tests(&self) -> bool {
        let queue = &self.persistence.objective_mirrors;
        let Ok(slot) = queue.slot.try_lock() else {
            return false;
        };
        matches!(*slot, PhysicalSlot::Idle)
            && queue
                .entries
                .values()
                .all(|entry| !entry.dirty && entry.in_flight.is_none())
    }

    /// Installs a one-shot bounded worker probe after final revision observation.
    #[cfg(test)]
    pub(crate) fn gate_objective_mirror_write_for_tests(
        &mut self,
        entered: std::sync::mpsc::SyncSender<()>,
        release: std::sync::mpsc::Receiver<()>,
        released: Arc<std::sync::atomic::AtomicBool>,
    ) {
        self.persistence.objective_mirrors.next_before_write = Some((entered, release, released));
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

    /// Pending, claimed and completed-but-uncollected jobs must not restore a
    /// deleted objective or report it confirmed. Exercise active, archived and
    /// retention deletion with both absent and populated indices; a clone owns
    /// deletion so registration sharing is part of the oracle. A genuinely new
    /// post-delete publication may reuse the CID without reviving old work.
    #[test]
    fn objective_mirror_deletion_fences_pending_claimed_and_completed_work() {
        use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

        for lifecycle in ["active", "archived", "retention"] {
            for phase in ["pending", "claimed", "completed"] {
                for populated in [false, true] {
                    let (mut store, root) = store();
                    store
                        .set_saved_session_retention_policy(
                            crate::storage::transcript::SavedSessionRetentionPolicy {
                                max_active_sessions: 1,
                                retention_days: 1,
                            },
                        )
                        .unwrap();
                    store
                        .append(&TranscriptEntry {
                            conversation_id: "deleted".into(),
                            sequence: 1,
                            created_at_unix_seconds: 1,
                            role: TranscriptRole::User,
                            turn_id: "fixture-turn".into(),
                            agent_id: "agent-%1".into(),
                            pane_id: "%1".into(),
                            content: "fixture prompt".into(),
                        })
                        .unwrap();
                    if lifecycle == "archived" {
                        store.archive_session("deleted", 2).unwrap();
                    }
                    if populated {
                        store
                            .mirror_session_objective("deleted", "seed", 2)
                            .unwrap();
                    }
                    let mut queue = ObjectiveMirrorQueue::default();
                    assert!(queue.publish(store.clone(), "deleted", Some("retained secret")));
                    let claimed = match phase {
                        "pending" => None,
                        "completed" => {
                            queue.claim().unwrap().execute();
                            None
                        }
                        _ => queue.claim(),
                    };
                    let deleting = store.clone();
                    match lifecycle {
                        "archived" => assert!(deleting.delete_archived_session("deleted").unwrap()),
                        "retention" => {
                            let report = deleting
                                .enforce_saved_session_retention(200_000, &Default::default())
                                .unwrap();
                            assert_eq!(report.deleted_conversation_ids, ["deleted"]);
                            assert!(report.failures.is_empty());
                        }
                        _ => assert!(deleting.delete("deleted").unwrap()),
                    }
                    if let Some(work) = claimed.or_else(|| queue.claim()) {
                        work.execute();
                    }
                    assert!(
                        queue.collect().is_none(),
                        "deleted work must not refresh UI"
                    );
                    assert!(queue.claim().is_none(), "deleted work must not retry");
                    assert_eq!(queue.status.confirmed, 0);
                    assert!(store.session_objective_mirror("deleted").unwrap().is_none());
                    assert!(queue.publish(store.clone(), "deleted", Some("retained secret")));
                    settle(&mut queue);
                    assert_eq!(
                        store
                            .session_objective_mirror("deleted")
                            .unwrap()
                            .unwrap()
                            .objective,
                        "retained secret"
                    );
                    std::fs::remove_dir_all(root).unwrap();
                }
            }
        }
    }

    /// Deletion arriving after the writer's final revision check must prune
    /// after that admitted write, not just revoke before an old write completes.
    /// The OS-thread gate has its own release timeout; every assertion follows
    /// explicit gate release and thread joins, so failures do not strand locks.
    #[test]
    fn objective_mirror_deletion_prunes_an_already_admitted_write() {
        let (store, root) = store();
        let mut queue = ObjectiveMirrorQueue::default();
        queue.publish(store.clone(), "deleted", Some("admitted secret"));
        let mut old = queue.claim().unwrap();
        let registration = old.registration.clone();
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, gate) = std::sync::mpsc::sync_channel(1);
        old.before_write = Some((entered, gate, Arc::new(AtomicBool::new(false))));
        let worker = std::thread::spawn(move || old.execute());
        let reached = waiting.recv_timeout(Duration::from_secs(2));
        let deleting = store.clone();
        let deletion = std::thread::spawn(move || deleting.delete("deleted"));
        let deadline = Instant::now() + Duration::from_secs(2);
        while registration.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let revoked_while_write_held = !registration.load(Ordering::Acquire);
        let _ = release.try_send(());
        let written = worker.join();
        let deleted = deletion.join();
        reached.unwrap();
        written.unwrap();
        deleted.unwrap().unwrap();
        assert!(revoked_while_write_held);
        assert!(queue.collect().is_none());
        assert_eq!(queue.status.confirmed, 0);
        assert!(store.session_objective_mirror("deleted").unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A fresh publication after deletion may reuse the CID while its old
    /// receipt still has custody. The old registration cannot acknowledge or
    /// clear the new desired value, even when both objective strings match.
    #[test]
    fn objective_mirror_new_publication_does_not_reactivate_a_deleted_claim() {
        for completed in [false, true] {
            let (store, root) = store();
            let mut queue = ObjectiveMirrorQueue::default();
            queue.publish(store.clone(), "same", Some("same objective"));
            let old = queue.claim().unwrap();
            let old = if completed {
                old.execute();
                None
            } else {
                Some(old)
            };
            store.delete("same").unwrap();
            assert!(queue.publish(store.clone(), "same", Some("same objective")));
            if let Some(old) = old {
                old.execute();
            }
            assert!(queue.collect().is_none());
            assert_eq!(queue.status.confirmed, 0);
            settle(&mut queue);
            assert_eq!(queue.status.confirmed, 1);
            assert_eq!(
                store
                    .session_objective_mirror("same")
                    .unwrap()
                    .unwrap()
                    .objective,
                "same objective"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    /// A shared store refuses new registrations at its hard cap without evicting
    /// live fences or pretending the value was persisted. Dropping one owner
    /// makes an identical explicit publication retryable without filesystem I/O
    /// during admission; deletion through a clone revokes all exact-CID owners.
    #[test]
    fn objective_mirror_registration_refusal_is_bounded_and_retryable() {
        let (store, root) = store();
        let mut registrations = Vec::new();
        for _ in 0..4096 {
            registrations.push(
                store
                    .register_session_objective_mirror_work("same")
                    .unwrap(),
            );
        }
        let mut queue = ObjectiveMirrorQueue::default();
        assert!(!queue.publish(store.clone(), "same", Some("new")));
        assert!(queue.entries.is_empty());
        assert_eq!(queue.status.last_failure, Some(MirrorFailure::Registration));
        assert!(
            registrations
                .iter()
                .all(|token| token.load(Ordering::Acquire))
        );
        registrations.pop();
        assert!(queue.publish(store.clone(), "same", Some("new")));
        settle(&mut queue);
        store.clone().delete("same").unwrap();
        assert!(
            registrations
                .iter()
                .all(|token| !token.load(Ordering::Acquire))
        );
        assert!(queue.claim().is_none());
        std::fs::remove_dir_all(root).unwrap();
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
        old.before_write = Some((
            entered,
            gate,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ));
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

    /// A physical failure stays visible exactly once when a newer desired
    /// revision supersedes its receipt before collection. Freshness gating may
    /// reject the stale result, but it must not hide the I/O degradation behind
    /// a successful newest value or suppress the bounded diagnostic contract.
    #[test]
    fn objective_mirror_superseded_io_failure_remains_visible() {
        let (_, root) = store();
        let blocked = root.join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let store = AgentTranscriptStore::new(blocked.clone());
        let mut queue = ObjectiveMirrorQueue::default();
        assert!(queue.publish(store.clone(), "same", Some("stale private objective")));
        let stale = queue.claim().unwrap();
        // The newer publication arrives while the failing write is in flight.
        assert!(queue.publish(store.clone(), "same", Some("newer objective")));
        stale.execute();
        assert!(
            queue.collect().is_none(),
            "stale receipt cannot confirm newer work"
        );
        assert_eq!(queue.status.failed, 1);
        assert_eq!(queue.status.last_failure, Some(MirrorFailure::Io));
        assert_eq!(queue.status.confirmed, 0);
        assert!(!format!("{:?}", queue.status).contains("private objective"));
        assert!(!format!("{:?}", queue.status).contains(&root.display().to_string()));
        // Repairing the same captured store lets the newer value converge
        // without a second publication or a second failure count.
        std::fs::remove_file(&blocked).unwrap();
        std::fs::create_dir(&blocked).unwrap();
        settle(&mut queue);
        assert_eq!(queue.status.confirmed, 1);
        assert_eq!(queue.status.failed, 1, "one physical failure, counted once");
        assert_eq!(
            store
                .session_objective_mirror("same")
                .unwrap()
                .unwrap()
                .objective,
            "newer objective"
        );
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

    /// Busy attempts advance a scheduling clock rather than reusing publication
    /// revision ties. Even with every busy predecessor immediately eligible,
    /// another captured store's ready key must be serviced within the key bound.
    #[test]
    fn objective_mirror_busy_predecessors_cannot_starve_an_independent_root() {
        let (first, root) = store();
        let (ready, other) = store();
        // Force the old BTree-key tie-break counterexample, not random order.
        let (first, root, ready, other) = if root < other {
            (first, root, ready, other)
        } else {
            (ready, other, first, root)
        };
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(".session-objectives.json.lock"))
            .unwrap();
        flock(&lock, FlockOperation::LockExclusive).unwrap();
        let mut queue = ObjectiveMirrorQueue::default();
        for index in 0..3 {
            queue.publish(first.clone(), &format!("busy-{index}"), Some("busy"));
        }
        queue.publish(ready.clone(), "ready", Some("ready"));
        let mut serviced = false;
        for _ in 0..4 {
            let work = queue.claim().unwrap();
            serviced |= work.key.0 == other;
            work.execute();
            queue.collect();
            // An adversarial eligibility schedule makes the ordering oracle
            // stricter without changing production clocks or retry deadlines.
            for entry in queue.entries.values_mut() {
                entry.eligible_at = Instant::now();
            }
        }
        drop(lock);
        let persisted = ready.session_objective_mirror("ready").unwrap();
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(other).unwrap();
        assert!(serviced, "busy entries starved an independent ready key");
        assert_eq!(persisted.unwrap().objective, "ready");
    }
}
