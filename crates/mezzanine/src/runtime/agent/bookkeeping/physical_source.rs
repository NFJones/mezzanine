//! Actual owned workers for checked original archive prefix synchronization.
//!
//! Immutable work contains only the original store and bounded prefix metadata.
//! A shared receipt retains its actual blocking handle before yielding. Dropped
//! observers cannot detach it; worker closures never retain their own receipt.
//! Only a joined genuine store seal can verify physical source. Waiting reads
//! may be explicitly rechecked, but success/error is cached and never replayed.
//! Closing admission fences stale starts before an unstarted owner is replaced.

use super::*;
use crate::storage::transcript::{TranscriptPrefixExpectation, TranscriptPrefixSeal};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// No actor state or caller-supplied success value crosses into this worker.
#[derive(Clone)]
struct PhysicalWork {
    /// Original target retained independently of later repository attachment.
    store: crate::storage::transcript::AgentTranscriptStore,
    /// Minimal required original prefix, not all later archive activity.
    source: TranscriptPrefixExpectation,
    /// Original logical target used to qualify the returned seal.
    path: std::path::PathBuf,
    /// Deterministic worker control remains local to isolated tests.
    #[cfg(test)]
    checkpoint: Option<PhysicalCheckpoint>,
}

/// Only controls the real verification worker, never its returned physical fact.
#[cfg(test)]
#[derive(Clone)]
enum PhysicalCheckpoint {
    /// Gates actual worker entry for observer-cancellation/ownership controls.
    Block(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>),
    /// Qualifies bounded join diagnostics without printing private panic data.
    Panic(String),
}

/// Exact handle custody; no observing future consumes this enum over await.
enum PhysicalWorker {
    /// No blocking read/sync was started.
    Unstarted,
    /// Immutable work executes while this actual handle remains owned here.
    Running(tokio::task::JoinHandle<Result<Option<TranscriptPrefixSeal>>>),
    /// Result is retained after actual join, including pending dependencies.
    Joined(Result<Option<TranscriptPrefixSeal>>),
}

/// Short synchronous start/replacement fence, never held over I/O or await.
struct PhysicalAdmission {
    closed: bool,
    started: bool,
}

/// Shared original work and actual worker lifetime, without a self-owned task.
struct PhysicalInner {
    work: Mutex<PhysicalWork>,
    admission: Mutex<PhysicalAdmission>,
    worker: tokio::sync::Mutex<PhysicalWorker>,
    joined: AtomicBool,
}

/// Opaque allocation-bound physical source owner; matching data cannot replace it.
#[derive(Clone)]
pub(crate) struct RuntimeTranscriptPrefixReceipt(Arc<PhysicalInner>);

impl std::fmt::Debug for RuntimeTranscriptPrefixReceipt {
    /// No store, row, digest, private diagnostic or pointer address is exposed.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RuntimeTranscriptPrefixReceipt")
    }
}
impl PartialEq for RuntimeTranscriptPrefixReceipt {
    /// Strong original allocations prevent label and allocation-ABA aliases.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for RuntimeTranscriptPrefixReceipt {}

impl RuntimeTranscriptPrefixReceipt {
    /// Creates pending work only, after actual checked source projection. The
    /// original actor and producer must retain it before polling observe/recheck.
    pub(super) fn new(
        store: crate::storage::transcript::AgentTranscriptStore,
        source: TranscriptPrefixExpectation,
        path: std::path::PathBuf,
    ) -> Self {
        Self(Arc::new(PhysicalInner {
            work: Mutex::new(PhysicalWork {
                store,
                source,
                path,
                #[cfg(test)]
                checkpoint: None,
            }),
            admission: Mutex::new(PhysicalAdmission {
                closed: false,
                started: false,
            }),
            worker: tokio::sync::Mutex::new(PhysicalWorker::Unstarted),
            joined: AtomicBool::new(false),
        }))
    }

    /// Observes one original worker. A completed Waiting read remains cached;
    /// use explicit recheck only after its physical dependencies may have changed.
    pub(crate) async fn observe(&self) -> Result<Option<TranscriptPrefixSeal>> {
        self.run(false).await
    }

    /// Explicitly retries only a joined Waiting read, never a failed or verified
    /// synchronization. One actual worker remains the finite concurrency bound.
    pub(crate) async fn recheck(&self) -> Result<Option<TranscriptPrefixSeal>> {
        self.run(true).await
    }

    /// Closes stale start admission before replacing ownership. False means an
    /// actual started handle still requires observed join and cannot be dropped.
    pub(crate) fn close_for_replacement(&self) -> Result<bool> {
        let mut admission = self
            .0
            .admission
            .lock()
            .map_err(|_| MezError::invalid_state("physical source admission poisoned"))?;
        admission.closed = true;
        Ok(!admission.started || self.0.joined.load(Ordering::Acquire))
    }

    /// Reports only cached actual join results, never raw worker progress.
    pub(crate) fn observed(&self) -> Option<Result<bool>> {
        let worker = self.0.worker.try_lock().ok()?;
        match &*worker {
            PhysicalWorker::Joined(result) => Some(result.clone().and_then(|seal| {
                if let Some(seal) = seal {
                    let work =
                        self.0.work.lock().map_err(|_| {
                            MezError::invalid_state("physical source work poisoned")
                        })?;
                    if !seal.matches(&work.path, &work.source) {
                        return Err(MezError::conflict("physical source seal binding changed"));
                    }
                    Ok(true)
                } else {
                    Ok(false)
                }
            })),
            _ => None,
        }
    }

    /// Installs each actual handle before yielding and borrows it over await.
    async fn run(&self, recheck: bool) -> Result<Option<TranscriptPrefixSeal>> {
        let mut worker = self.0.worker.lock().await;
        let start = matches!(*worker, PhysicalWorker::Unstarted)
            || (recheck && matches!(*worker, PhysicalWorker::Joined(Ok(None))));
        if start {
            let admission = &mut *self
                .0
                .admission
                .lock()
                .map_err(|_| MezError::invalid_state("physical source admission poisoned"))?;
            if admission.closed {
                return match &*worker {
                    PhysicalWorker::Joined(result) => result.clone(),
                    _ => Ok(None),
                };
            }
            let work = self
                .0
                .work
                .lock()
                .map_err(|_| MezError::invalid_state("physical source work poisoned"))?
                .clone();
            admission.started = true;
            self.0.joined.store(false, Ordering::Release);
            *worker = PhysicalWorker::Running(tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                if let Some(checkpoint) = work.checkpoint {
                    match checkpoint {
                        PhysicalCheckpoint::Block(entered, release) => {
                            entered.wait();
                            release.wait();
                        }
                        PhysicalCheckpoint::Panic(message) => panic!("{message}"),
                    }
                }
                work.store
                    .seal_transcript_prefix(&work.source)
                    .map_err(|error| error.with_context("verify original archive RAW prefix"))
            }));
        }
        if let PhysicalWorker::Running(handle) = &mut *worker {
            let joined = handle
                .await
                .map_err(|error| {
                    MezError::invalid_state(if error.is_cancelled() {
                        "physical source verification worker cancelled; no seal accepted"
                    } else {
                        "physical source verification worker failed; no seal accepted"
                    })
                })
                .and_then(|result| result);
            *worker = PhysicalWorker::Joined(joined);
            self.0.joined.store(true, Ordering::Release);
        }
        match &*worker {
            PhysicalWorker::Joined(result) => result.clone(),
            _ => Ok(None),
        }
    }

    /// Gates the genuine worker; no synthetic physical success can be installed.
    #[cfg(test)]
    pub(crate) fn block_for_tests(
        &self,
        entered: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) {
        self.0.work.lock().unwrap().checkpoint = Some(PhysicalCheckpoint::Block(entered, release));
    }

    /// Injects a genuine worker panic to check observed join and safe diagnostics.
    #[cfg(test)]
    pub(crate) fn panic_for_tests(&self, message: String) {
        self.0.work.lock().unwrap().checkpoint = Some(PhysicalCheckpoint::Panic(message));
    }
}

#[cfg(test)]
mod tests {
    //! Actual worker lifetime controls without synthetic seal construction.
    use super::*;
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    /// Owns a unique real store and removes it after joined verification.
    struct Fixture {
        root: std::path::PathBuf,
        store: crate::storage::transcript::AgentTranscriptStore,
        row: TranscriptEntry,
    }

    impl Fixture {
        /// Creates an isolated original target without writing its row yet.
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "mez-physical-worker-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            Self {
                store: crate::storage::transcript::AgentTranscriptStore::new(root.clone()),
                root,
                row: TranscriptEntry {
                    conversation_id: "physical-worker".into(),
                    sequence: 1,
                    created_at_unix_seconds: 1,
                    role: TranscriptRole::User,
                    turn_id: "turn".into(),
                    agent_id: "agent".into(),
                    pane_id: "%1".into(),
                    content: "original source".into(),
                },
            }
        }

        /// Captures only actual checked row metadata, leaving physical work pending.
        fn receipt(&self) -> RuntimeTranscriptPrefixReceipt {
            RuntimeTranscriptPrefixReceipt::new(
                self.store.clone(),
                TranscriptPrefixExpectation::capture(&self.row.conversation_id, [&self.row])
                    .unwrap(),
                self.store
                    .transcript_path(&self.row.conversation_id)
                    .unwrap(),
            )
        }
    }

    impl Drop for Fixture {
        /// Cleanup follows actual worker join in each lifetime test.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Waiting is cached until explicit recheck. A closed stale unstarted owner
    /// cannot begin I/O even if its original physical dependencies later arrive.
    #[tokio::test]
    async fn physical_worker_waiting_recheck_and_stale_start() {
        let fixture = Fixture::new();
        let receipt = fixture.receipt();
        let stale = fixture.receipt();
        assert!(stale.close_for_replacement().unwrap());
        assert!(receipt.observe().await.unwrap().is_none());
        assert!(!receipt.observed().unwrap().unwrap());
        fixture.store.append(&fixture.row).unwrap();
        assert!(receipt.observe().await.unwrap().is_none());
        assert!(receipt.recheck().await.unwrap().is_some());
        assert!(receipt.observed().unwrap().unwrap());
        assert!(stale.observe().await.unwrap().is_none());
        assert!(stale.observed().is_none());
    }

    /// Aborting an observer leaves the genuine blocking handle in the receipt.
    /// Replacement stays closed until another observer actually collects its join.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn physical_worker_survives_observer_cancellation() {
        let fixture = Fixture::new();
        fixture.store.append(&fixture.row).unwrap();
        let receipt = fixture.receipt();
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        receipt.block_for_tests(entered.clone(), release.clone());
        let observer = tokio::spawn({
            let receipt = receipt.clone();
            async move { receipt.observe().await }
        });
        tokio::task::spawn_blocking(move || entered.wait())
            .await
            .unwrap();
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        // Release the real worker before assertions that might unwind.
        let replaceable = receipt.close_for_replacement();
        release.wait();
        assert!(!replaceable.unwrap());
        assert!(receipt.observe().await.unwrap().is_some());
        assert!(receipt.observed().unwrap().unwrap());
        assert!(receipt.close_for_replacement().unwrap());
    }

    /// A genuine worker panic becomes a bounded cached failure after actual join;
    /// private panic text never becomes an archive receipt or returned diagnostic.
    #[tokio::test]
    async fn physical_worker_panic_is_joined_and_bounded() {
        let fixture = Fixture::new();
        let receipt = fixture.receipt();
        receipt.panic_for_tests("private-source-secret".repeat(1024));
        let first = receipt.observe().await.unwrap_err().to_string();
        assert!(first.len() < 200);
        assert!(!first.contains("private-source-secret"));
        assert_eq!(receipt.recheck().await.unwrap_err().to_string(), first);
        assert!(receipt.observed().unwrap().is_err());
        assert!(receipt.close_for_replacement().unwrap());
    }
}
