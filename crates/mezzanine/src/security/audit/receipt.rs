//! Allocation-bound physical audit receipts and immutable ordered write work.
//!
//! A receipt binds one encoded record, destination and retention policy before
//! queue handoff. Only this module's actual I/O path or the writer's synchronous
//! append can finish its private guard. Duplicate deliveries observe the same
//! result and never retry an entered write. Failure/unwind may leave a physical
//! prefix and is permanently non-replayable. Physical acceptance proves the
//! append/sync/permissions/retention boundary, not permanent retention, worker
//! join, filesystem-action settlement or producer retirement.

use super::types::{AuditDeferredWrite, AuditRetentionPolicy};
use crate::error::{MezError, Result};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Actual shared physical state, never supplied by a worker callback's labels.
#[derive(Clone, Debug)]
pub(crate) enum AuditAppendStatus {
    /// Original bytes are retained but no physical writer entered.
    Queued,
    /// A physical writer entered; cancellation cannot prove nonexecution.
    InFlight,
    /// Actual append, sync, permissions and retention returned successfully.
    Accepted,
    /// A failed or unwound entered write is uncertain and cannot replay.
    Failed(MezError),
}

/// Exact immutable effect binding and its one physical result owner.
struct ReceiptInner {
    /// Original configured destination; equality alone is not receipt identity.
    path: PathBuf,
    /// Exact encoded record identity, including its trailing newline.
    encoded: Arc<[u8]>,
    /// Original retention policy is part of the admitted physical operation.
    retention: AuditRetentionPolicy,
    /// No user callback or I/O executes while this short mutex is held.
    status: Mutex<AuditAppendStatus>,
    /// Actual blocking handle survives cancellation of any observing future.
    worker: tokio::sync::Mutex<WorkerState>,
    /// Deferred acceptance requires observing the actual blocking worker join.
    worker_joined: AtomicBool,
    /// Isolated deterministic admission fault; absent from production state.
    #[cfg(test)]
    checkpoint: Mutex<Option<TestCheckpoint>>,
}

/// Test-only boundary control while the genuine physical guard is held.
#[cfg(test)]
#[derive(Clone)]
enum TestCheckpoint {
    /// Actual worker announces entry and waits for its test owner to release.
    Block(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>),
    /// Unwind after entry to verify actual join and non-replayable custody.
    Panic(String),
}

/// Shared actual worker custody, distinct from physical I/O result facts.
enum WorkerState {
    /// No asynchronous worker has started for this queued record.
    Unstarted,
    /// The handle remains here while observers borrow it across awaits.
    Running(tokio::task::JoinHandle<Result<(AuditAppendStatus, bool)>>),
    /// Joined result remains available; duplicates do not enter I/O again.
    Joined(Result<AuditAppendStatus>),
}

/// Opaque shared physical evidence; equal record labels cannot construct it.
#[derive(Clone)]
pub(crate) struct AuditAppendReceipt(Arc<ReceiptInner>);

impl PartialEq for AuditAppendReceipt {
    /// Receipt identity is allocation identity, independent of equal contents.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for AuditAppendReceipt {}

impl fmt::Debug for AuditAppendReceipt {
    /// Never exposes record bytes, paths, digests or diagnostic contents.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuditAppendReceipt")
            .field(
                "state",
                &match self.status() {
                    AuditAppendStatus::Queued => "queued",
                    AuditAppendStatus::InFlight => "in-flight",
                    AuditAppendStatus::Accepted => "accepted",
                    AuditAppendStatus::Failed(_) => "failed",
                },
            )
            .finish()
    }
}

/// Sole entered physical operation; unwinding preserves non-replayable failure.
pub(super) struct AuditAppendGuard {
    /// Original allocation remains retained through the final I/O result.
    receipt: AuditAppendReceipt,
    /// Only explicit result installation acknowledges this entered boundary.
    finished: bool,
}

impl AuditAppendReceipt {
    /// Creates evidence only from the writer's exact original encoded record.
    pub(super) fn new(
        path: &Path,
        bytes: &[u8],
        retention: &AuditRetentionPolicy,
        deferred: bool,
    ) -> Self {
        Self(Arc::new(ReceiptInner {
            path: path.into(),
            encoded: Arc::from(bytes),
            retention: retention.clone(),
            status: Mutex::new(AuditAppendStatus::Queued),
            worker: tokio::sync::Mutex::new(WorkerState::Unstarted),
            worker_joined: AtomicBool::new(!deferred),
            #[cfg(test)]
            checkpoint: Mutex::new(None),
        }))
    }

    /// Reads retained physical evidence; poison is unproven, never acceptance.
    pub(crate) fn status(&self) -> AuditAppendStatus {
        self.0
            .status
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| {
                AuditAppendStatus::Failed(MezError::invalid_state(
                    "audit physical receipt state is unproven; replay prohibited",
                ))
            })
    }

    /// Reports actual worker join observation, or the synchronous no-worker
    /// case selected before append. Physical status alone never supplies this.
    pub(crate) fn worker_joined(&self) -> bool {
        self.0.worker_joined.load(Ordering::Acquire)
    }

    /// Enters once after exact payload validation. Existing results/in-flight
    /// ownership are observed without issuing another physical write.
    pub(super) fn begin(
        &self,
        path: &Path,
        bytes: &[u8],
        retention: &AuditRetentionPolicy,
    ) -> Result<Option<AuditAppendGuard>> {
        if path != self.0.path || bytes != self.0.encoded.as_ref() || retention != &self.0.retention
        {
            return Err(MezError::conflict(
                "audit physical work differs from its original receipt",
            ));
        }
        let mut state = self
            .0
            .status
            .lock()
            .map_err(|_| MezError::invalid_state("audit physical receipt lock is unproven"))?;
        if !matches!(*state, AuditAppendStatus::Queued) {
            return Ok(None);
        }
        *state = AuditAppendStatus::InFlight;
        Ok(Some(AuditAppendGuard {
            receipt: self.clone(),
            finished: false,
        }))
    }
}

impl AuditAppendGuard {
    /// Records the actual I/O result after the whole qualified boundary. This
    /// method is private to the audit subsystem, never a callback capability.
    pub(super) fn finish(mut self, result: Result<()>) {
        if let Ok(mut state) = self.receipt.0.status.lock() {
            *state = match result {
                Ok(()) => AuditAppendStatus::Accepted,
                Err(error) => AuditAppendStatus::Failed(error),
            };
        }
        self.finished = true;
    }
}

impl Drop for AuditAppendGuard {
    /// Retains an uncertain entered write on panic/drop, with no I/O or retry.
    fn drop(&mut self) {
        if !self.finished
            && let Ok(mut state) = self.receipt.0.status.lock()
        {
            *state = AuditAppendStatus::Failed(MezError::invalid_state(
                "audit physical writer exited without proof; partial write possible, replay prohibited",
            ));
        }
    }
}

/// Immutable payload and receipt crossing the existing ordered persistence lane.
#[derive(Clone)]
pub(crate) struct AuditAppendWork(Arc<AuditDeferredWrite>);

impl PartialEq for AuditAppendWork {
    /// Clones name the same original work, not another equal-looking write.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for AuditAppendWork {}
impl fmt::Debug for AuditAppendWork {
    /// Reports ownership/size only, excluding audit content and destination.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuditAppendWork")
            .field("bytes", &self.0.bytes.len())
            .finish()
    }
}

impl AuditAppendWork {
    /// Wraps a writer-produced tracked payload without exposing a mutable alias.
    pub(super) fn new(write: AuditDeferredWrite) -> Self {
        Self(Arc::new(write))
    }

    /// Returns the actual opaque source, or an invariant error before I/O.
    pub(crate) fn receipt(&self) -> Result<AuditAppendReceipt> {
        self.0
            .receipt
            .clone()
            .ok_or_else(|| MezError::invalid_state("tracked audit work has no original receipt"))
    }

    /// Records a terminal worker-delivery failure only before any physical
    /// writer entered. An in-flight operation retains its own actual result.
    pub(crate) fn fail_unstarted_delivery(&self, error: MezError) {
        if let Some(receipt) = &self.0.receipt
            && let Ok(mut state) = receipt.0.status.lock()
            && matches!(*state, AuditAppendStatus::Queued)
        {
            *state = AuditAppendStatus::Failed(error);
        }
    }

    /// Reads immutable queued bytes/destination for original-source test checks.
    #[cfg(test)]
    pub(crate) fn encoded_record_for_tests(&self) -> (&Path, &[u8]) {
        (&self.0.path, &self.0.bytes)
    }

    /// Returns immutable payload size for actual performed-write accounting.
    pub(crate) fn byte_count(&self) -> usize {
        self.0.bytes.len()
    }

    /// Starts once and observes the actual blocking worker through retained
    /// handle custody. Cancellation drops only an observer's borrow. Another
    /// observer joins the same worker; a cached joined result cannot replay I/O.
    /// Requires the host's Tokio runtime; handle installation precedes its first
    /// pending join await. No observer moves the handle out of shared custody.
    pub(crate) async fn execute_owned(&self) -> Result<(AuditAppendStatus, bool)> {
        let receipt = self.receipt()?;
        let mut state = receipt.0.worker.lock().await;
        if let WorkerState::Joined(result) = &*state {
            return result.clone().map(|status| (status, false));
        }
        if matches!(*state, WorkerState::Unstarted) {
            let owned = self.clone();
            let handle = tokio::task::spawn_blocking(move || owned.execute());
            *state = WorkerState::Running(handle);
        }
        let result = match &mut *state {
            WorkerState::Running(handle) => match handle.await {
                Ok(result) => result,
                Err(error) => {
                    // JoinError formatting contains task identity and arbitrary
                    // panic payloads. Only fixed bounded categories may cross
                    // this ownership boundary into returned/cached diagnostics.
                    let message = if error.is_panic() {
                        "audit physical worker panicked; effects unproven, replay prohibited"
                    } else if error.is_cancelled() {
                        "audit physical worker cancelled before join; effects unproven, replay prohibited"
                    } else {
                        "audit physical worker join unproven; replay prohibited"
                    };
                    let error = MezError::invalid_state(message);
                    self.fail_unstarted_delivery(error.clone());
                    Err(error)
                }
            },
            _ => Err(MezError::invalid_state(
                "audit blocking handle ownership changed",
            )),
        };
        receipt.0.worker_joined.store(true, Ordering::Release);
        *state = WorkerState::Joined(result.clone().map(|(status, _)| status));
        result
    }

    /// Reports whether a performed write includes an enabled retention scan.
    pub(crate) fn has_retention_policy(&self) -> bool {
        self.0.retention.max_age_days.is_some()
            || self.0.retention.max_records.is_some()
            || self.0.retention.max_bytes.is_some()
    }

    /// Executes at most one exact physical operation. The bool reports whether
    /// this call entered I/O; duplicates only observe the retained result. This
    /// blocking method must run off the actor. It does not prove worker join.
    fn execute(&self) -> Result<(AuditAppendStatus, bool)> {
        self.execute_with_checkpoint(|| {
            #[cfg(test)]
            if let Ok(receipt) = self.receipt() {
                let checkpoint = receipt
                    .0
                    .checkpoint
                    .lock()
                    .ok()
                    .and_then(|value| value.clone());
                match checkpoint {
                    Some(TestCheckpoint::Block(entered, release)) => {
                        entered.wait();
                        release.wait();
                    }
                    Some(TestCheckpoint::Panic(payload)) => std::panic::panic_any(payload),
                    None => {}
                }
            }
        })
    }

    /// Runs a test-only admission checkpoint while the physical guard is held;
    /// production always supplies the empty callback and still performs real I/O.
    fn execute_with_checkpoint(
        &self,
        checkpoint: impl FnOnce(),
    ) -> Result<(AuditAppendStatus, bool)> {
        let receipt = self.receipt()?;
        let Some(guard) = receipt.begin(&self.0.path, &self.0.bytes, &self.0.retention)? else {
            return Ok((receipt.status(), false));
        };
        checkpoint();
        let result =
            super::log::persist_audit_record(&self.0.path, &self.0.bytes, &self.0.retention);
        guard.finish(result);
        Ok((receipt.status(), true))
    }
}

impl AuditDeferredWrite {
    /// Moves tracked work into an immutable owner while preserving ordinary
    /// audit writes for their existing coalesced persistence path.
    pub(crate) fn into_tracked(self) -> std::result::Result<AuditAppendWork, Self> {
        if self.receipt.is_some() {
            Ok(AuditAppendWork::new(self))
        } else {
            Err(self)
        }
    }
}

#[cfg(test)]
mod tests {
    //! Real record writes, exact binding and cancellation-safe worker ownership.
    use super::*;
    use crate::security::audit::{AuditActor, AuditConfig, AuditLog, AuditRecord};
    use std::sync::atomic::AtomicU64;

    /// Unique subtree retained until every controlled worker has been observed.
    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        /// Builds a fresh writer-produced encoded payload with one real receipt.
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            Self {
                root: std::env::temp_dir().join(format!(
                    "mez-audit-receipt-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                )),
            }
        }
        /// Retains the original deferred bytes before immutable work conversion.
        fn queued(&self, retention: AuditRetentionPolicy) -> AuditDeferredWrite {
            let mut log = AuditLog::new(AuditConfig {
                enabled: true,
                required: true,
                path: self.root.join("events.jsonl"),
                hash_chain: true,
            })
            .with_retention(retention);
            log.set_defer_writes(true);
            log.append_tracked(AuditRecord::new(
                "session",
                AuditActor {
                    kind: "runtime".into(),
                    id: "physical-test".into(),
                },
                "native_filesystem",
                "apply_patch",
            ))
            .unwrap();
            log.drain_deferred_writes().pop().unwrap()
        }
    }
    impl Drop for Fixture {
        /// Removes only the owned fixture subtree after joined test work.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Equal bytes/destinations are different physical occurrences. Malformed
    /// path, bytes or retention cannot enter the genuine receipt; its original
    /// immutable work remains valid and writes exactly once through a real job.
    #[tokio::test]
    async fn tracked_audit_binding_rejects_modified_work_and_equal_looking_receipts() {
        let fixture = Fixture::new();
        let original = fixture.queued(AuditRetentionPolicy::disabled());
        let receipt = original.receipt.clone().unwrap();
        let other = fixture.queued(AuditRetentionPolicy::disabled());
        assert_ne!(receipt, other.receipt.clone().unwrap());
        for change in 0..3 {
            let mut changed = original.clone();
            match change {
                0 => changed.path = fixture.root.join("other.jsonl"),
                1 => changed.bytes.push(b'x'),
                _ => changed.retention.max_records = Some(1),
            }
            assert!(changed.into_tracked().unwrap().execute().is_err());
            assert!(matches!(receipt.status(), AuditAppendStatus::Queued));
        }
        let work = original.into_tracked().unwrap();
        assert!(matches!(
            work.execute_owned().await.unwrap(),
            (AuditAppendStatus::Accepted, true)
        ));
        assert!(receipt.worker_joined());
        assert!(matches!(
            other.receipt.unwrap().status(),
            AuditAppendStatus::Queued
        ));
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("events.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert!(matches!(
            work.execute_owned().await.unwrap(),
            (AuditAppendStatus::Accepted, false)
        ));
    }

    /// Simultaneous observers share one actual retained handle and cannot
    /// duplicate the append. Cached result delivery does not count as new I/O.
    #[tokio::test]
    async fn tracked_audit_concurrent_observers_join_one_physical_write() {
        let fixture = Fixture::new();
        let work = fixture
            .queued(AuditRetentionPolicy::disabled())
            .into_tracked()
            .unwrap();
        let (left, right) = tokio::join!(work.execute_owned(), work.execute_owned());
        let left = left.unwrap();
        let right = right.unwrap();
        assert_eq!(usize::from(left.1) + usize::from(right.1), 1);
        assert!(matches!(left.0, AuditAppendStatus::Accepted));
        assert!(matches!(right.0, AuditAppendStatus::Accepted));
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("events.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    /// Retention fails on an existing invalid UTF-8 prefix after the new actual
    /// record has been appended and synced. Failed qualification retains that
    /// prefix and cannot retry the same bytes even after the file is repaired.
    #[tokio::test]
    async fn tracked_audit_failure_after_actual_append_never_replays() {
        let fixture = Fixture::new();
        let queued = fixture.queued(AuditRetentionPolicy {
            max_records: Some(1),
            ..Default::default()
        });
        std::fs::create_dir_all(&fixture.root).unwrap();
        std::fs::write(&queued.path, [0xff, b'\n']).unwrap();
        let encoded = queued.bytes.clone();
        let path = queued.path.clone();
        let work = queued.into_tracked().unwrap();
        assert!(matches!(
            work.execute_owned().await.unwrap(),
            (AuditAppendStatus::Failed(_), true)
        ));
        assert!(std::fs::read(&path).unwrap().ends_with(&encoded));
        std::fs::write(&path, &encoded).unwrap();
        assert!(matches!(
            work.execute_owned().await.unwrap(),
            (AuditAppendStatus::Failed(_), false)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), encoded);
    }

    /// Cancellation of an observer while the real blocking job is admitted
    /// leaves its actual handle retained. A second observer joins that same job;
    /// neither a cancelled future nor physical status alone proves worker exit.
    #[tokio::test]
    async fn tracked_audit_cancelled_observer_cannot_detach_or_reissue_worker() {
        let fixture = Fixture::new();
        let work = fixture
            .queued(AuditRetentionPolicy::disabled())
            .into_tracked()
            .unwrap();
        let receipt = work.receipt().unwrap();
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        *receipt.0.checkpoint.lock().unwrap() =
            Some(TestCheckpoint::Block(entered.clone(), release.clone()));
        let owned = work.clone();
        let observer = tokio::spawn(async move { owned.execute_owned().await });
        tokio::task::spawn_blocking(move || entered.wait())
            .await
            .unwrap();
        /// Releases an already entered fixture job even if an assertion unwinds.
        struct Release(Option<Arc<std::sync::Barrier>>);
        impl Drop for Release {
            /// Prevents a failed assertion from stranding the actual fixture job.
            fn drop(&mut self) {
                if let Some(barrier) = self.0.take() {
                    barrier.wait();
                }
            }
        }
        let mut releasing = Release(Some(release));
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        assert!(matches!(
            *receipt.0.worker.try_lock().unwrap(),
            WorkerState::Running(_)
        ));
        assert!(matches!(receipt.status(), AuditAppendStatus::InFlight));
        assert!(!receipt.worker_joined());
        releasing.0.take().unwrap().wait();
        assert!(matches!(
            work.execute_owned().await.unwrap(),
            (AuditAppendStatus::Accepted, true)
        ));
        assert!(receipt.worker_joined());
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("events.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    /// An actual worker panic after physical admission is joined and retained
    /// as uncertain failure. A second observer cannot start another worker or
    /// turn missing bytes into permission to replay the admitted operation.
    #[tokio::test]
    async fn tracked_audit_worker_unwind_is_joined_and_nonreplayable() {
        let fixture = Fixture::new();
        let work = fixture
            .queued(AuditRetentionPolicy::disabled())
            .into_tracked()
            .unwrap();
        let receipt = work.receipt().unwrap();
        let private_marker = "PRIVATE_AUDIT_PANIC_FIXTURE_";
        *receipt.0.checkpoint.lock().unwrap() =
            Some(TestCheckpoint::Panic(private_marker.repeat(2048)));
        let first = work.execute_owned().await.unwrap_err();
        let cached = work.execute_owned().await.unwrap_err();
        for error in [&first, &cached] {
            assert!(!error.message().contains(private_marker));
            assert!(error.message().len() <= 256);
        }
        assert_eq!(first.message(), cached.message());
        assert!(receipt.worker_joined());
        assert!(matches!(receipt.status(), AuditAppendStatus::Failed(_)));
        assert!(work.execute_owned().await.is_err());
        assert!(!fixture.root.join("events.jsonl").exists());
    }
}
