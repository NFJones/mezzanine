//! Bounded worker pool for agent-prompt selector refreshes.
//!
//! Selector refreshes used to spawn one detached OS thread per (client, pane)
//! generation. A burst of invalidations therefore left several full skill
//! catalog walks, issue-database opens, and transcript enumerations running at
//! once, and nothing ever joined or cancelled them. This module owns a
//! per-session pool of bounded workers instead: requests carry their generation,
//! superseded generations are skipped before the expensive walk, a saturated
//! pool reports back so the caller keeps its prior candidates, and teardown
//! stops and joins every worker.

use super::SelectorExtraCandidate;
use mez_core::ids::ClientId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Number of selector-refresh workers owned by one session.
pub(crate) const RUNTIME_AGENT_SELECTOR_REFRESH_WORKERS: usize = 2;

/// Selector refresh key: exact client plus pane owner.
pub(crate) type RuntimeAgentSelectorRefreshKey = (ClientId, String);

/// Owned inputs one worker consumes without touching serialized actor state.
pub(crate) struct RuntimeAgentSelectorRefreshSnapshot {
    /// In-memory candidates already known to the actor.
    pub(crate) candidates: Vec<SelectorExtraCandidate>,
    /// Configured user root whose skill and macro catalogs are discovered.
    pub(crate) user_config_root: Option<std::path::PathBuf>,
    /// Trusted project root whose project catalogs may apply.
    pub(crate) project_root: Option<std::path::PathBuf>,
    /// Issue database the refresh may read, when issue commands are enabled.
    pub(crate) issue_database_path: Option<crate::storage::issues::IssueDatabasePath>,
    /// Transcript store the refresh may enumerate, when one is configured.
    pub(crate) transcript_store: Option<crate::storage::transcript::AgentTranscriptStore>,
    /// Session-title policy applied when the refresh reads saved sessions.
    pub(crate) session_title_policy: crate::session_title::SessionTitlePolicy,
    /// Test-only gate that holds a worker before it starts the expensive walk.
    #[cfg(test)]
    pub(crate) test_gate: Option<Receiver<()>>,
}

/// One queued selector refresh.
pub(crate) struct RuntimeAgentSelectorRefreshRequest {
    /// Exact client and pane owner the request belongs to.
    key: RuntimeAgentSelectorRefreshKey,
    /// Generation stamped by the actor when the request was submitted.
    generation: u64,
    /// Immutable snapshot the worker walks.
    snapshot: RuntimeAgentSelectorRefreshSnapshot,
    /// Result channel the actor polls.
    sender: SyncSender<Vec<SelectorExtraCandidate>>,
}

/// Outcome of submitting one refresh request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeAgentSelectorRefreshOutcome {
    /// The request is queued or already running.
    Queued,
    /// Every worker and the queue are busy; the caller keeps prior candidates.
    Saturated,
    /// The pool is stopped; the caller must settle the refresh itself.
    Stopped,
}

/// Bounded pool of selector-refresh workers owned by one session.
pub(crate) struct RuntimeAgentSelectorRefreshPool {
    /// Queue into the workers; `None` once the pool is stopped.
    sender: Option<SyncSender<RuntimeAgentSelectorRefreshRequest>>,
    /// Latest submitted generation per key, used to skip superseded work.
    latest: Arc<Mutex<HashMap<RuntimeAgentSelectorRefreshKey, u64>>>,
    /// Stop flag checked by every worker before and after dequeueing.
    stop: Arc<AtomicBool>,
    /// Workers started since the pool was created.
    started_workers: Arc<AtomicUsize>,
    /// Workers currently inside the worker loop.
    live_workers: Arc<AtomicUsize>,
    /// Worker handles joined on shutdown.
    handles: Vec<JoinHandle<()>>,
}

impl RuntimeAgentSelectorRefreshPool {
    /// Starts one pool with its bounded channel and configured workers.
    pub(crate) fn new() -> Self {
        Self::with_workers(RUNTIME_AGENT_SELECTOR_REFRESH_WORKERS)
    }

    /// Starts one pool with an explicit worker count.
    fn with_workers(workers: usize) -> Self {
        let (sender, receiver) = std::sync::mpsc::sync_channel(workers.max(1));
        let receiver = Arc::new(Mutex::new(receiver));
        let latest = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let started_workers = Arc::new(AtomicUsize::new(0));
        let live_workers = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for index in 0..workers.max(1) {
            let receiver = Arc::clone(&receiver);
            let latest = Arc::clone(&latest);
            let stop = Arc::clone(&stop);
            let started_workers = Arc::clone(&started_workers);
            let live_workers = Arc::clone(&live_workers);
            let spawned = std::thread::Builder::new()
                .name(format!("mez-selector-refresh-{index}"))
                .spawn(move || {
                    started_workers.fetch_add(1, Ordering::SeqCst);
                    live_workers.fetch_add(1, Ordering::SeqCst);
                    run_selector_refresh_worker(&receiver, &latest, &stop);
                    live_workers.fetch_sub(1, Ordering::SeqCst);
                });
            if let Ok(handle) = spawned {
                handles.push(handle);
            }
        }
        Self {
            sender: Some(sender),
            latest,
            stop,
            started_workers,
            live_workers,
            handles,
        }
    }

    /// Records the newest generation for one key without queueing work.
    ///
    /// A saturated pool still has to know the newest generation: work already
    /// queued for an older generation must be skipped rather than walked.
    pub(crate) fn record_generation(&self, key: &RuntimeAgentSelectorRefreshKey, generation: u64) {
        if let Ok(mut latest) = self.latest.lock() {
            latest.insert(key.clone(), generation);
        }
    }

    /// Submits one refresh request to the pool.
    pub(crate) fn submit(
        &self,
        key: RuntimeAgentSelectorRefreshKey,
        generation: u64,
        snapshot: RuntimeAgentSelectorRefreshSnapshot,
        sender: SyncSender<Vec<SelectorExtraCandidate>>,
    ) -> RuntimeAgentSelectorRefreshOutcome {
        self.record_generation(&key, generation);
        let Some(pool_sender) = self.sender.as_ref() else {
            return RuntimeAgentSelectorRefreshOutcome::Stopped;
        };
        let request = RuntimeAgentSelectorRefreshRequest {
            key,
            generation,
            snapshot,
            sender,
        };
        match pool_sender.try_send(request) {
            Ok(()) => RuntimeAgentSelectorRefreshOutcome::Queued,
            Err(TrySendError::Full(_)) => RuntimeAgentSelectorRefreshOutcome::Saturated,
            Err(TrySendError::Disconnected(_)) => RuntimeAgentSelectorRefreshOutcome::Stopped,
        }
    }

    /// Stops every worker and waits for it to exit.
    pub(crate) fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.sender = None;
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }

    /// Returns how many workers this pool started.
    #[cfg(test)]
    pub(crate) fn started_workers(&self) -> usize {
        self.started_workers.load(Ordering::SeqCst)
    }

    /// Returns how many workers are currently inside the worker loop.
    #[cfg(test)]
    pub(crate) fn live_workers(&self) -> usize {
        self.live_workers.load(Ordering::SeqCst)
    }
}

/// Runs one worker until the pool stops or every sender disappears.
fn run_selector_refresh_worker(
    receiver: &Arc<Mutex<Receiver<RuntimeAgentSelectorRefreshRequest>>>,
    latest: &Arc<Mutex<HashMap<RuntimeAgentSelectorRefreshKey, u64>>>,
    stop: &Arc<AtomicBool>,
) {
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let request = match receiver.lock() {
            Ok(receiver) => match receiver.recv() {
                Ok(request) => request,
                Err(_) => return,
            },
            Err(_) => return,
        };
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if selector_refresh_is_superseded(latest, &request.key, request.generation) {
            continue;
        }
        #[cfg(test)]
        if let Some(gate) = request.snapshot.test_gate.as_ref() {
            let _ = gate.recv();
        }
        let candidates = super::input::runtime_agent_selector_extra_candidates_from_snapshot(
            request.snapshot.candidates,
            request.snapshot.user_config_root,
            request.snapshot.project_root,
            request.snapshot.issue_database_path,
            request.snapshot.transcript_store,
            request.snapshot.session_title_policy,
        );
        let _ = request.sender.send(candidates);
    }
}

/// Reports whether a newer generation replaced one queued request.
fn selector_refresh_is_superseded(
    latest: &Arc<Mutex<HashMap<RuntimeAgentSelectorRefreshKey, u64>>>,
    key: &RuntimeAgentSelectorRefreshKey,
    generation: u64,
) -> bool {
    latest
        .lock()
        .map(|latest| latest.get(key).is_some_and(|current| *current > generation))
        .unwrap_or(false)
}

impl std::fmt::Debug for RuntimeAgentSelectorRefreshPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeAgentSelectorRefreshPool")
            .field(
                "started_workers",
                &self.started_workers.load(Ordering::SeqCst),
            )
            .field("live_workers", &self.live_workers.load(Ordering::SeqCst))
            .field("stopped", &self.sender.is_none())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    /// Builds one snapshot whose worker waits until the test releases it.
    ///
    /// Holding a worker inside its walk is what makes pool occupancy
    /// deterministic in these tests; the returned sender releases the gate.
    fn gated_snapshot() -> (RuntimeAgentSelectorRefreshSnapshot, SyncSender<()>) {
        let (release, gate) = std::sync::mpsc::sync_channel(1);
        (
            RuntimeAgentSelectorRefreshSnapshot {
                candidates: Vec::new(),
                user_config_root: None,
                project_root: None,
                issue_database_path: None,
                transcript_store: None,
                session_title_policy: crate::session_title::SessionTitlePolicy::default(),
                test_gate: Some(gate),
            },
            release,
        )
    }

    /// Builds one refresh key for a pane owned by a fixed test client.
    fn refresh_key(pane_id: &str) -> RuntimeAgentSelectorRefreshKey {
        (ClientId::new('c', 1), pane_id.to_string())
    }

    /// Builds one result channel pair for a submitted request.
    fn result_channel() -> (
        SyncSender<Vec<SelectorExtraCandidate>>,
        Receiver<Vec<SelectorExtraCandidate>>,
    ) {
        std::sync::mpsc::sync_channel(1)
    }

    /// Waits until at least one worker has entered its loop.
    fn wait_for_live_worker(pool: &RuntimeAgentSelectorRefreshPool) {
        for _ in 0..1_000 {
            if pool.live_workers() > 0 {
                std::thread::sleep(Duration::from_millis(50));
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("selector refresh workers did not start");
    }

    /// Waits until the pool has started the expected number of workers.
    fn wait_for_started_workers(pool: &RuntimeAgentSelectorRefreshPool, expected: usize) {
        for _ in 0..1_000 {
            if pool.started_workers() == expected {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!(
            "selector refresh pool started {} workers, expected {expected}",
            pool.started_workers()
        );
    }

    /// Verifies the pool starts exactly the configured workers and never more.
    #[test]
    fn selector_refresh_pool_starts_a_bounded_worker_set() {
        let mut pool = RuntimeAgentSelectorRefreshPool::new();
        wait_for_started_workers(&pool, RUNTIME_AGENT_SELECTOR_REFRESH_WORKERS);
        for index in 0..8 {
            let (sender, _receiver) = result_channel();
            let (snapshot, release) = gated_snapshot();
            let _ = pool.submit(refresh_key(&format!("%{index}")), 1, snapshot, sender);
            drop(release);
        }
        assert_eq!(
            pool.started_workers(),
            RUNTIME_AGENT_SELECTOR_REFRESH_WORKERS,
            "a burst of refreshes must not start more workers"
        );
        pool.shutdown();
        assert_eq!(pool.live_workers(), 0);
    }

    /// Verifies a busy pool reports saturation instead of queueing unbounded work.
    #[test]
    fn selector_refresh_pool_reports_saturation_when_busy() {
        let mut pool = RuntimeAgentSelectorRefreshPool::with_workers(1);
        let (first_sender, _first_receiver) = result_channel();
        let (first_snapshot, release_first) = gated_snapshot();
        assert_eq!(
            pool.submit(refresh_key("%1"), 1, first_snapshot, first_sender),
            RuntimeAgentSelectorRefreshOutcome::Queued
        );
        wait_for_live_worker(&pool);
        let (second_sender, _second_receiver) = result_channel();
        let (second_snapshot, release_second) = gated_snapshot();
        assert_eq!(
            pool.submit(refresh_key("%2"), 1, second_snapshot, second_sender),
            RuntimeAgentSelectorRefreshOutcome::Queued
        );
        let (third_sender, _third_receiver) = result_channel();
        let (third_snapshot, release_third) = gated_snapshot();
        assert_eq!(
            pool.submit(refresh_key("%3"), 1, third_snapshot, third_sender),
            RuntimeAgentSelectorRefreshOutcome::Saturated
        );
        drop(release_first);
        drop(release_second);
        drop(release_third);
        pool.shutdown();
    }

    /// Verifies a superseded generation is skipped before the expensive walk.
    #[test]
    fn selector_refresh_pool_skips_a_superseded_request() {
        let mut pool = RuntimeAgentSelectorRefreshPool::with_workers(1);
        let (blocker_sender, _blocker_receiver) = result_channel();
        let (blocker_snapshot, release_blocker) = gated_snapshot();
        assert_eq!(
            pool.submit(refresh_key("%0"), 1, blocker_snapshot, blocker_sender),
            RuntimeAgentSelectorRefreshOutcome::Queued
        );
        wait_for_live_worker(&pool);
        let (stale_sender, stale_receiver) = result_channel();
        let (stale_snapshot, _release_stale) = gated_snapshot();
        assert_eq!(
            pool.submit(refresh_key("%1"), 1, stale_snapshot, stale_sender),
            RuntimeAgentSelectorRefreshOutcome::Queued
        );
        pool.record_generation(&refresh_key("%1"), 2);
        drop(release_blocker);
        assert_eq!(
            stale_receiver.recv_timeout(Duration::from_secs(2)),
            Err(RecvTimeoutError::Disconnected),
            "a superseded generation must be dropped instead of walked"
        );
        pool.shutdown();
    }

    /// Verifies teardown stops and joins every worker and refuses new work.
    #[test]
    fn selector_refresh_pool_shutdown_joins_every_worker() {
        let mut pool = RuntimeAgentSelectorRefreshPool::with_workers(2);
        let (pending_sender, pending_receiver) = result_channel();
        let (pending_snapshot, release_pending) = gated_snapshot();
        assert_eq!(
            pool.submit(refresh_key("%1"), 1, pending_snapshot, pending_sender),
            RuntimeAgentSelectorRefreshOutcome::Queued
        );
        wait_for_live_worker(&pool);
        drop(release_pending);
        pool.shutdown();
        assert_eq!(pool.live_workers(), 0, "teardown joins every worker");
        assert!(
            pending_receiver
                .recv_timeout(Duration::from_secs(1))
                .is_ok()
        );
        let (later_sender, _later_receiver) = result_channel();
        let (later_snapshot, _later_release) = gated_snapshot();
        assert_eq!(
            pool.submit(refresh_key("%2"), 1, later_snapshot, later_sender),
            RuntimeAgentSelectorRefreshOutcome::Stopped
        );
    }
}
