//! Concurrent ownership and lifecycle isolation for multiple session runtimes.
//!
//! `SessionSupervisor` sits above `SessionFactory`: it reserves stable session
//! identities before asynchronous construction, runs each ready runtime in an
//! independent Tokio task, and retains actor handles, exact native ownership and
//! non-evictable completion receipts. Entries remain discoverable through
//! callback/terminal publication, and failures latch before final removal. Map
//! locks are never held across callbacks, factory, actor, or task awaits. Every
//! entry carries a
//! monotonically increasing generation so completion from an older task cannot
//! remove or overwrite a replacement session with the same identity.
//! Shutdown keeps one captured deadline through native effects and publication,
//! preserving the first error with bounded diagnostics from every later stage.
//! A whole-runtime unwind discards the compromised actor and services before
//! publishing a fixed panic cause. The cancellation guard remains independent;
//! neither outcome substitutes task completion for exact native retirement.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::FutureExt;
use mez_mux::process::PaneProcessRetirementOwner;
use tokio::sync::{Notify, watch};

use super::{SessionFactory, SessionFactoryRequest, SessionRuntimeHandle};
use crate::error::{MezError, MezErrorKind, Result, TeardownErrors};
use crate::runtime::{RuntimeLifecycleState, RuntimePowerInhibitionStatus};

const DEFAULT_TERMINAL_HISTORY_LIMIT: usize = 64;

mod native_attempts;
pub(crate) use native_attempts::{NativeContinuationScope, PreparedCliNativeRetry};

/// Host-visible lifecycle state for one supervised session runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionSupervisorState {
    /// The stable identity is reserved while construction is in progress.
    Starting,
    /// The runtime is ready and independently supervised.
    Running,
    /// Shutdown has been requested and completion is pending.
    Stopping,
    /// The runtime completed without a reported failure.
    Stopped,
    /// Construction or runtime supervision failed.
    Failed,
}

/// Immutable bounded status projection for host policy and administration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSupervisorSnapshot {
    /// Stable session identity.
    pub(crate) session_id: String,
    /// Supervisor generation fencing callbacks for this identity.
    pub(crate) generation: u64,
    /// Current supervisor-owned lifecycle state.
    pub(crate) state: SessionSupervisorState,
    /// Actor lifecycle when a live handle is available.
    pub(crate) runtime_state: Option<RuntimeLifecycleState>,
    /// Actor-owned power status while a live runtime handle is available.
    pub(crate) power_inhibition: Option<RuntimePowerInhibitionStatus>,
    /// Private operator failure diagnostic; public/model status projections
    /// must omit raw details rather than assuming arbitrary failures are safe.
    pub(crate) failure: Option<String>,
}

type RuntimeCompletionCallback =
    dyn Fn(&SessionSupervisorSnapshot) -> Result<()> + Send + Sync + 'static;

#[derive(Clone)]
struct RuntimeCompletionHandler(Arc<RuntimeCompletionCallback>);

impl std::fmt::Debug for RuntimeCompletionHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeCompletionHandler")
            .finish_non_exhaustive()
    }
}

/// Concurrent owner for independently running session runtimes.
#[derive(Debug, Clone)]
pub(crate) struct SessionSupervisor {
    inner: Arc<SessionSupervisorInner>,
}

#[derive(Debug)]
struct SessionSupervisorInner {
    entries: Mutex<HashMap<String, SupervisorEntry>>,
    terminal: Mutex<VecDeque<SessionSupervisorSnapshot>>,
    next_generation: AtomicU64,
    changed: Notify,
    terminal_history_limit: usize,
    runtime_completion_handler: Option<RuntimeCompletionHandler>,
    /// Final CLI/host cleanup closes admission before collecting native owners.
    native_cleanup_sealed: AtomicBool,
    /// One bounded private native/completion-cleanup receipt survives removal.
    native_failure: Mutex<Option<String>>,
    #[cfg(test)]
    start_reservation_probe: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    #[cfg(test)]
    native_startup_probe: Mutex<Option<(String, u64, super::NativePaneStartupProbe)>>,
    #[cfg(test)]
    fail_next_stop: AtomicU64,
    /// Test-local observation gate after real runtime/native completion, before
    /// callback/entry publication. It never replaces child or lifecycle I/O.
    #[cfg(test)]
    runtime_completion_probe: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
}

#[derive(Debug)]
struct SupervisorEntry {
    generation: u64,
    /// Exists before factory startup; never stored in evictable terminal history.
    native: PaneProcessRetirementOwner,
    /// Exact non-evictable receipt retained by in-flight shutdown snapshots.
    completion: watch::Sender<ExactCompletionState>,
    /// Serializes matching finishers without holding the map during callbacks.
    completion_claimed: bool,
    state: SupervisorEntryState,
}

/// Private monotonic completion evidence, never a public raw error/handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExactCompletionState {
    /// Native/callback/entry publication is not yet proved complete.
    Pending,
    /// Matching runtime, callback and native settlement succeeded.
    Stopped,
    /// Completion failed; closure and zero cannot erase it.
    Failed,
}

/// Generation/owner/receipt captured after host admission has been sealed.
struct ExactShutdownTarget {
    generation: u64,
    native: PaneProcessRetirementOwner,
    completion: watch::Receiver<ExactCompletionState>,
    /// Captured exact handle; no later ID lookup can target a replacement.
    request: Option<(SessionRuntimeHandle, watch::Sender<bool>)>,
}

#[derive(Debug)]
enum SupervisorEntryState {
    /// The actor may be gone, but this exact generation still owns cleanup.
    CleanupPending {
        failure: String,
    },
    Starting {
        cancelled: bool,
    },
    Running {
        handle: SessionRuntimeHandle,
        cancel: watch::Sender<bool>,
    },
    Stopping {
        handle: SessionRuntimeHandle,
        cancel: watch::Sender<bool>,
    },
}

/// Removes the exact startup generation if its construction future is
/// cancelled before ownership transfers to the runtime supervision task.
struct StartupReservationGuard {
    inner: Arc<SessionSupervisorInner>,
    session_id: String,
    generation: u64,
    armed: bool,
}

impl StartupReservationGuard {
    fn new(inner: Arc<SessionSupervisorInner>, session_id: String, generation: u64) -> Self {
        Self {
            inner,
            session_id,
            generation,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StartupReservationGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.inner.finish_start_failure(
                &self.session_id,
                self.generation,
                "session startup future was cancelled".to_string(),
            );
        }
    }
}

/// Marks exact runtime task abandonment even when it is never first-polled.
struct NativeRuntimeCompletionGuard {
    inner: Arc<SessionSupervisorInner>,
    session_id: String,
    generation: u64,
    armed: bool,
}

impl NativeRuntimeCompletionGuard {
    /// Uses the whole guard so async disjoint-field capture cannot leave its
    /// Drop outside the spawned future while capturing only `armed`.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for NativeRuntimeCompletionGuard {
    /// Retains pending cleanup instead of silently leaving a dead Running entry.
    fn drop(&mut self) {
        if self.armed {
            let _ = self.inner.finish_runtime(
                &self.session_id,
                self.generation,
                SessionSupervisorState::Failed,
                None,
                Some("session runtime task was cancelled before completion".to_string()),
            );
        }
    }
}

impl Default for SessionSupervisor {
    fn default() -> Self {
        Self::new(DEFAULT_TERMINAL_HISTORY_LIMIT)
    }
}

impl SessionSupervisor {
    /// Creates a supervisor with a bounded terminal-status history.
    pub(crate) fn new(terminal_history_limit: usize) -> Self {
        Self::new_with_runtime_completion_handler(terminal_history_limit, None)
    }

    /// Creates a supervisor that reports accepted current-generation runtime completions.
    pub(crate) fn with_runtime_completion_handler(
        handler: impl Fn(&SessionSupervisorSnapshot) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self::new_with_runtime_completion_handler(
            DEFAULT_TERMINAL_HISTORY_LIMIT,
            Some(RuntimeCompletionHandler(Arc::new(handler))),
        )
    }

    fn new_with_runtime_completion_handler(
        terminal_history_limit: usize,
        runtime_completion_handler: Option<RuntimeCompletionHandler>,
    ) -> Self {
        Self {
            inner: Arc::new(SessionSupervisorInner {
                entries: Mutex::new(HashMap::new()),
                terminal: Mutex::new(VecDeque::with_capacity(terminal_history_limit)),
                next_generation: AtomicU64::new(1),
                changed: Notify::new(),
                terminal_history_limit,
                runtime_completion_handler,
                native_cleanup_sealed: AtomicBool::new(false),
                native_failure: Mutex::new(None),
                #[cfg(test)]
                start_reservation_probe: Mutex::new(None),
                #[cfg(test)]
                native_startup_probe: Mutex::new(None),
                #[cfg(test)]
                fail_next_stop: AtomicU64::new(0),
                #[cfg(test)]
                runtime_completion_probe: Mutex::new(None),
            }),
        }
    }

    /// Reserves, constructs, and starts one independently supervised runtime.
    ///
    /// Duplicate identities fail before construction. Failed construction
    /// removes only the matching generation reservation and records a bounded
    /// failed snapshot so a later retry can safely reuse the identity.
    pub(crate) async fn start(
        &self,
        request: SessionFactoryRequest,
    ) -> Result<SessionRuntimeHandle> {
        let session_id = request.session.id.to_string();
        let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
        let native = PaneProcessRetirementOwner::default();
        {
            let mut entries = self.inner.entries()?;
            if self.inner.native_cleanup_sealed.load(Ordering::Acquire) {
                return Err(MezError::invalid_state(
                    "host native process admission is sealed",
                ));
            }
            if entries.contains_key(&session_id) {
                return Err(MezError::conflict(format!(
                    "session `{session_id}` is already supervised"
                )));
            }
            entries.insert(
                session_id.clone(),
                SupervisorEntry {
                    generation,
                    native: native.clone(),
                    completion: watch::channel(ExactCompletionState::Pending).0,
                    completion_claimed: false,
                    state: SupervisorEntryState::Starting { cancelled: false },
                },
            );
        }
        self.inner.changed.notify_waiters();

        let mut reservation =
            StartupReservationGuard::new(self.inner.clone(), session_id.clone(), generation);

        #[cfg(test)]
        let start_reservation_probe = self
            .inner
            .start_reservation_probe
            .lock()
            .map_err(|_| MezError::invalid_state("session startup probe lock was poisoned"))?
            .clone();
        #[cfg(test)]
        if let Some((started, release)) = start_reservation_probe {
            started.notify_waiters();
            release.notified().await;
        }

        #[cfg(test)]
        let native_probe = {
            let mut probe =
                self.inner.native_startup_probe.lock().map_err(|_| {
                    MezError::invalid_state("native startup probe lock was poisoned")
                })?;
            if probe
                .as_ref()
                .is_some_and(|(id, epoch, _)| id == &session_id && *epoch == generation)
            {
                probe.take().map(|(_, _, probe)| probe)
            } else {
                None
            }
        };
        let runtime = match SessionFactory::create_with_native_owner(
            request,
            native.clone(),
            #[cfg(test)]
            native_probe,
        )
        .await
        {
            Ok(runtime) => runtime,
            Err(error) => {
                self.inner
                    .finish_start_failure(&session_id, generation, error.to_string())?;
                reservation.disarm();
                return Err(error);
            }
        };
        let handle = runtime.handle();
        let (cancel, mut cancellation) = watch::channel(false);
        {
            let mut entries = self.inner.entries()?;
            let Some(entry) = entries.get_mut(&session_id) else {
                drop(runtime);
                return Err(MezError::invalid_state(format!(
                    "session `{session_id}` startup reservation disappeared"
                )));
            };
            if entry.generation != generation {
                drop(runtime);
                return Err(MezError::conflict(format!(
                    "session `{session_id}` startup reservation was superseded"
                )));
            }
            if matches!(
                entry.state,
                SupervisorEntryState::Starting { cancelled: true }
            ) {
                drop(entries);
                drop(runtime);
                self.inner.finish_start_failure(
                    &session_id,
                    generation,
                    "session startup was cancelled".to_string(),
                )?;
                reservation.disarm();
                self.inner.changed.notify_waiters();
                return Err(MezError::invalid_state(format!(
                    "session `{session_id}` startup was cancelled"
                )));
            }
            if !matches!(
                entry.state,
                SupervisorEntryState::Starting { cancelled: false }
            ) {
                drop(runtime);
                return Err(MezError::conflict(format!(
                    "session `{session_id}` startup reservation was superseded"
                )));
            }
            entry.state = SupervisorEntryState::Running {
                handle: handle.clone(),
                cancel,
            };
        }
        reservation.disarm();
        self.inner.changed.notify_waiters();

        let inner = self.inner.clone();
        let task_session_id = session_id.clone();
        let mut completion_guard = NativeRuntimeCompletionGuard {
            inner: inner.clone(),
            session_id: session_id.clone(),
            generation,
            armed: true,
        };
        tokio::spawn(async move {
            let cancellation = async move {
                if *cancellation.borrow() {
                    return;
                }
                while cancellation.changed().await.is_ok() {
                    if *cancellation.borrow() {
                        return;
                    }
                }
            };
            // This is the disposable session-runtime boundary, not a recovery
            // point inside serialized mutations. An unwind destroys the entire
            // runtime future; its actor is never polled again. Retained native
            // custody remains with this exact supervisor generation and the
            // existing completion path requires cleanup before publication.
            // Tokio's outer panic catch would otherwise dispose this future as
            // abandonment and the cancellation guard would mislabel the cause.
            // Keep the ordinary panic hook, but never copy its payload to status.
            let completion = std::panic::AssertUnwindSafe(runtime.run(Vec::new(), cancellation))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err(MezError::invalid_state(
                        "session runtime panicked before completion",
                    ))
                });
            #[cfg(test)]
            let publication_probe = { inner.runtime_completion_probe.lock().unwrap().take() };
            #[cfg(test)]
            if let Some((arrived, release)) = publication_probe {
                let _ = arrived.send(());
                let _ = release.await;
            }
            let (state, runtime_state, failure) = match completion {
                Ok(completion) => {
                    let runtime_state = completion.service.lifecycle_state();
                    if runtime_state == RuntimeLifecycleState::Failed {
                        (
                            SessionSupervisorState::Failed,
                            Some(runtime_state),
                            Some("session runtime entered failed state".to_string()),
                        )
                    } else {
                        (SessionSupervisorState::Stopped, Some(runtime_state), None)
                    }
                }
                Err(error) => (
                    SessionSupervisorState::Failed,
                    None,
                    Some(error.to_string()),
                ),
            };
            let _ =
                inner.finish_runtime(&task_session_id, generation, state, runtime_state, failure);
            completion_guard.disarm();
        });

        Ok(handle)
    }

    /// Returns the live actor handle for an exactly running session.
    pub(crate) fn lookup(&self, session_id: &str) -> Result<SessionRuntimeHandle> {
        let entries = self.inner.entries()?;
        let entry = entries.get(session_id).ok_or_else(|| {
            MezError::new(
                MezErrorKind::NotFound,
                format!("session `{session_id}` is not supervised"),
            )
        })?;
        match &entry.state {
            SupervisorEntryState::Running { handle, .. } => Ok(handle.clone()),
            SupervisorEntryState::CleanupPending { .. } => Err(MezError::invalid_state(format!(
                "session `{session_id}` has unproven native cleanup"
            ))),
            SupervisorEntryState::Starting { .. } => Err(MezError::invalid_state(format!(
                "session `{session_id}` is still starting"
            ))),
            SupervisorEntryState::Stopping { .. } => Err(MezError::invalid_state(format!(
                "session `{session_id}` is stopping"
            ))),
        }
    }

    /// Returns whether this supervisor still owns any generation for one identity.
    pub(crate) fn contains(&self, session_id: &str) -> Result<bool> {
        Ok(self.inner.entries()?.contains_key(session_id))
    }

    /// Requests graceful or forced teardown without holding the map lock across actor awaits.
    pub(crate) fn stop<'a>(
        &'a self,
        session_id: &'a str,
        force: bool,
    ) -> impl std::future::Future<Output = Result<()>> + 'a {
        self.stop_with_native_deadline(
            session_id,
            force,
            tokio::time::Instant::now() + super::NATIVE_PANE_RETIREMENT_TIMEOUT,
        )
    }

    /// Shares the enclosing shutdown budget with exact pending-child retry.
    fn stop_with_native_deadline<'a>(
        &'a self,
        session_id: &'a str,
        force: bool,
        deadline: tokio::time::Instant,
    ) -> impl std::future::Future<Output = Result<()>> + 'a {
        let captured = self.inner.entries().and_then(|entries| {
            let entry = entries.get(session_id).ok_or_else(|| {
                MezError::new(
                    MezErrorKind::NotFound,
                    format!("session `{session_id}` is not supervised"),
                )
            })?;
            Ok((
                entry.generation,
                entry.native.clone(),
                entry.completion.subscribe(),
                entry
                    .native
                    .capture_cleanup_attempt(deadline.into_std())
                    .map_err(MezError::from),
            ))
        });
        async move {
            #[cfg(test)]
            if self.inner.fail_next_stop.swap(0, Ordering::SeqCst) != 0 {
                return Err(MezError::invalid_state(
                    "injected session supervisor stop failure",
                ));
            }
            let (generation, native, completion, attempt) = captured?;
            let present = {
                let entries = self.inner.entries()?;
                match entries.get(session_id) {
                    Some(entry) if entry.generation != generation => {
                        return Err(MezError::conflict("session stop generation was replaced"));
                    }
                    Some(_) => true,
                    None => false,
                }
            };
            if !present {
                return self
                    .await_exact_completion(
                        session_id,
                        ExactShutdownTarget {
                            generation,
                            native,
                            completion,
                            request: None,
                        },
                        deadline,
                    )
                    .await;
            }
            let attempt = attempt?.bind_first_source(&native)?;
            attempt.validate(&native)?;
            if tokio::time::Instant::now() >= deadline {
                return Err(MezError::invalid_state(
                    "session stop deadline expired before admission",
                ));
            }
            /// Exact lookup disposition, never inferred from actor error text.
            enum Action {
                Pending(String),
                Startup,
                Runtime(SessionRuntimeHandle, watch::Sender<bool>),
                Removed,
            }
            let action = {
                let mut entries = self.inner.entries()?;
                match entries.get_mut(session_id) {
                    None => Action::Removed,
                    Some(entry) if entry.generation != generation => {
                        return Err(MezError::conflict("session stop generation was replaced"));
                    }
                    Some(entry) => match &mut entry.state {
                        SupervisorEntryState::CleanupPending { failure } => {
                            attempt.validate(&native)?;
                            native.request_shutdown(force);
                            native.seal();
                            Action::Pending(failure.clone())
                        }
                        SupervisorEntryState::Starting { cancelled } => {
                            *cancelled = true;
                            native.seal();
                            Action::Startup
                        }
                        SupervisorEntryState::Running { handle, cancel } => {
                            let handle = handle.clone();
                            let cancel = cancel.clone();
                            entry.state = SupervisorEntryState::Stopping {
                                handle: handle.clone(),
                                cancel: cancel.clone(),
                            };
                            Action::Runtime(handle, cancel)
                        }
                        SupervisorEntryState::Stopping { handle, cancel } => {
                            Action::Runtime(handle.clone(), cancel.clone())
                        }
                    },
                }
            };
            self.inner.changed.notify_waiters();
            let (handle, cancel) = match action {
                Action::Pending(failure) => {
                    let cleanup =
                        crate::host::async_runtime::drain_native_pane_retirement_for_attempt(
                            &native, &attempt, deadline,
                        )
                        .await;
                    return self.finish_native_pending_result(
                        session_id, generation, &native, failure, cleanup,
                    );
                }
                Action::Startup => return Ok(()),
                Action::Removed => {
                    return self
                        .await_exact_completion(
                            session_id,
                            ExactShutdownTarget {
                                generation,
                                native,
                                completion,
                                request: None,
                            },
                            deadline,
                        )
                        .await;
                }
                Action::Runtime(handle, cancel) => (handle, cancel),
            };
            let shutdown = if force {
                handle
                    .force_shutdown(format!("session supervisor stopped `{session_id}`"))
                    .await
            } else {
                handle
                    .graceful_shutdown(format!("session supervisor stopped `{session_id}`"))
                    .await
            };
            let _ = cancel.send(true);
            shutdown
        }
    }

    /// Applies a native cleanup verdict independently of exact resource count.
    /// A retained failure cannot pin an already-reaped generation forever.
    fn finish_native_pending_result(
        &self,
        id: &str,
        generation: u64,
        native: &PaneProcessRetirementOwner,
        failure: String,
        cleanup: Result<()>,
    ) -> Result<()> {
        let disposition = if native.outstanding() == 0 {
            self.inner.finish_runtime(
                id,
                generation,
                SessionSupervisorState::Failed,
                None,
                Some(failure),
            )
        } else {
            Ok(())
        };
        match (cleanup, disposition) {
            (Ok(()), result) | (result, Ok(())) => result,
            (Err(primary), Err(disposition)) => Err(MezError::new(
                primary.kind(),
                format!(
                    "{}; native entry disposition failed: {disposition}",
                    primary.message()
                ),
            )),
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_next_stop(&self) {
        self.inner.fail_next_stop.store(1, Ordering::SeqCst);
    }

    /// Shared test-only receipt injection, never a child/reap proof substitute.
    #[cfg(test)]
    pub(crate) fn retain_native_failure_for_test(&self) {
        *self.inner.native_failure.lock().unwrap() =
            Some("injected native signal failure receipt".to_string());
    }

    /// Shared diagnostic-only fixture for private/public formatting tests.
    /// It creates no native process and must never be used as reap evidence.
    #[cfg(test)]
    pub(crate) fn private_diagnostic_fixture_for_test() -> Self {
        let supervisor = Self::new(1);
        *supervisor.inner.native_failure.lock().unwrap() =
            Some("PRIVATE_NATIVE_HOST_RECEIPT_CANARY /private/host-receipt".to_string());
        supervisor.inner.entries().unwrap().insert(
            "$PRIVATE_NATIVE_ENTRY_CANARY".to_string(),
            SupervisorEntry {
                generation: 1,
                native: PaneProcessRetirementOwner::default(),
                completion: watch::channel(ExactCompletionState::Pending).0,
                completion_claimed: false,
                state: SupervisorEntryState::CleanupPending {
                    failure: "PRIVATE_NATIVE_HOST_ENTRY_CANARY /private/host-entry".to_string(),
                },
            },
        );
        supervisor
            .inner
            .push_terminal(SessionSupervisorSnapshot {
                session_id: "$PRIVATE_NATIVE_TERMINAL_CANARY".to_string(),
                generation: 0,
                state: SessionSupervisorState::Failed,
                runtime_state: Some(RuntimeLifecycleState::Failed),
                power_inhibition: None,
                failure: Some(
                    "PRIVATE_NATIVE_HOST_TERMINAL_CANARY /private/host-terminal".to_string(),
                ),
            })
            .unwrap();
        supervisor.seal_native_admission().unwrap();
        supervisor
    }

    /// Shared test-only status fault used by owner and public-outcome tests.
    /// No native resource or production authority predicate is changed.
    #[cfg(test)]
    pub(crate) fn poison_native_status_for_test(&self) {
        let inner = self.inner.clone();
        let poisoned = std::thread::spawn(move || {
            let _guard = inner.entries.lock().unwrap();
            panic!("test-only native status lock poison");
        })
        .join();
        assert!(poisoned.is_err());
    }

    /// Stops every live runtime and waits boundedly for all matching tasks to settle.
    pub(crate) fn shutdown_all(
        &self,
        force: bool,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<()>> + '_ {
        self.shutdown_all_with_receipt_probe(
            force,
            timeout,
            #[cfg(test)]
            None,
        )
    }

    /// Retains the actual shutdown algorithm while allowing a test-only receipt
    /// publication point after native completion and before entry settlement.
    fn shutdown_all_with_receipt_probe(
        &self,
        force: bool,
        timeout: Duration,
        #[cfg(test)] after_native: Option<fn(&Self, bool)>,
    ) -> impl std::future::Future<Output = Result<()>> + '_ {
        let deadline = tokio::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| MezError::invalid_args("session shutdown budget is not representable"));
        self.shutdown_all_until_with_receipt_probe(
            force,
            deadline,
            #[cfg(test)]
            after_native,
        )
    }

    /// Stops captured exact runtimes within an existing caller deadline, without
    /// refreshing native cleanup attempts or extending an expired budget.
    pub(crate) fn shutdown_all_until(
        &self,
        force: bool,
        deadline: tokio::time::Instant,
    ) -> impl std::future::Future<Output = Result<()>> + '_ {
        self.shutdown_all_until_with_receipt_probe(
            force,
            Ok(deadline),
            #[cfg(test)]
            None,
        )
    }

    /// Shares exact capture and completion settlement between duration and
    /// already-deadlined callers; the test probe grants no cleanup authority.
    fn shutdown_all_until_with_receipt_probe(
        &self,
        force: bool,
        deadline: Result<tokio::time::Instant>,
        #[cfg(test)] after_native: Option<fn(&Self, bool)>,
    ) -> impl std::future::Future<Output = Result<()>> + '_ {
        let preparation = deadline.and_then(|deadline| {
            self.capture_native_continuation_scope(deadline)
                .map(|scope| (deadline, scope))
        });
        async move {
            use futures_util::StreamExt;
            let (deadline, mut scope) = preparation?;
            self.seal_native_scope(&mut scope, deadline, false)?;
            let mut targets = {
                let mut entries = self.inner.entries()?;
                entries
                    .iter_mut()
                    .map(|(id, entry)| {
                        let request = if scope.permits(id, entry, deadline) {
                            entry.native.request_shutdown(force);
                            match &mut entry.state {
                                SupervisorEntryState::Starting { cancelled } => {
                                    *cancelled = true;
                                    None
                                }
                                SupervisorEntryState::CleanupPending { .. } => None,
                                SupervisorEntryState::Running { handle, cancel }
                                | SupervisorEntryState::Stopping { handle, cancel } => {
                                    Some((handle.clone(), cancel.clone()))
                                }
                            }
                        } else {
                            None
                        };
                        if let Some((handle, cancel)) = &request {
                            entry.state = SupervisorEntryState::Stopping {
                                handle: handle.clone(),
                                cancel: cancel.clone(),
                            };
                        }
                        let effect_deadline = scope.effect_deadline(id, entry.generation, deadline);
                        (
                            id.clone(),
                            ExactShutdownTarget {
                                generation: entry.generation,
                                native: entry.native.clone(),
                                completion: entry.completion.subscribe(),
                                request,
                            },
                            effect_deadline,
                        )
                    })
                    .collect::<Vec<_>>()
            };
            targets.sort_by(|left, right| left.0.cmp(&right.0));
            // One future per already-admitted session, not detached tasks. Poll all
            // stop admissions rather than blocking the seventeenth behind stalled
            // actors; native work itself remains fixed-quanta and nonblocking.
            let mut stops: futures_util::stream::FuturesUnordered<_> = targets
                .into_iter()
                .map(|(session_id, target, effect_deadline)| async move {
                    let result = tokio::time::timeout_at(
                        deadline,
                        self.stop_exact_shutdown_target_until(
                            &session_id,
                            target,
                            force,
                            deadline,
                            effect_deadline,
                        ),
                    )
                    .await
                    .map_err(|_| {
                        MezError::invalid_state("session supervisor stop deadline expired")
                    })
                    .and_then(|result| result);
                    (session_id, result)
                })
                .collect();
            let requests = async {
                let mut failures = TeardownErrors::default();
                while let Some((session_id, result)) = stops.next().await {
                    if let Err(error) = result {
                        failures.record(&format!("session {session_id} shutdown"), error);
                    }
                }
                failures
            };
            let (mut failures, native) = tokio::join!(
                requests,
                self.drain_native_captured_scope_with_owner_receipts(
                    scope,
                    deadline,
                    PaneProcessRetirementOwner::cleanup_failure
                )
            );
            #[cfg(test)]
            if let Some(after_native) = after_native {
                after_native(self, native.is_ok());
            }
            if let Err(error) = native {
                failures.record("native cleanup", error);
            }
            let settlement = tokio::time::timeout_at(deadline, async {
                loop {
                    let changed = self.inner.changed.notified();
                    if self.inner.entries()?.is_empty() {
                        return Ok(());
                    }
                    changed.await;
                }
            })
            .await
            .map_err(|_| MezError::invalid_state("session supervisor shutdown timed out"))
            .and_then(|result| result);
            // Entry settlement may run a completion callback after the native
            // future finished. Its monotonic receipt must affect this verdict too.
            match self.native_cleanup_failure() {
                Ok(Some(failure)) => {
                    failures.record(
                        "native completion publication",
                        MezError::invalid_state(failure),
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    failures.record("native completion publication", error);
                }
            }
            if let Err(error) = settlement {
                failures.record("remaining session cleanup", error);
            }
            failures.finish()
        }
    }

    /// Uses only a sealed snapshot's exact handle, never a later ID lookup.
    /// Removal before admission skips repeated effects but still awaits the
    /// exact completion receipt. A replacement generation remains an error.
    #[cfg(test)]
    async fn stop_exact_shutdown_target(
        &self,
        session_id: &str,
        target: ExactShutdownTarget,
        force: bool,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        self.stop_exact_shutdown_target_until(session_id, target, force, deadline, deadline)
            .await
    }

    /// Separates captured effect admission from the outer publication wait.
    async fn stop_exact_shutdown_target_until(
        &self,
        session_id: &str,
        mut target: ExactShutdownTarget,
        force: bool,
        deadline: tokio::time::Instant,
        effect_deadline: tokio::time::Instant,
    ) -> Result<()> {
        let present = {
            let entries = self.inner.entries()?;
            match entries.get(session_id) {
                Some(entry) if entry.generation != target.generation => {
                    return Err(MezError::conflict(
                        "shutdown target generation was replaced",
                    ));
                }
                Some(_) => true,
                None => false,
            }
        };
        if present && let Some((handle, cancel)) = target.request.take() {
            if tokio::time::Instant::now() >= effect_deadline {
                return Err(MezError::invalid_state(
                    "exact shutdown admission deadline expired",
                ));
            }
            let admission = if force {
                handle
                    .force_shutdown(format!("session supervisor stopped `{session_id}`"))
                    .await
            } else {
                handle
                    .graceful_shutdown(format!("session supervisor stopped `{session_id}`"))
                    .await
            };
            if tokio::time::Instant::now() < effect_deadline {
                let _ = cancel.send(true);
            }
            admission?;
        }
        self.await_exact_completion(session_id, target, deadline)
            .await
    }

    /// Waits the matching generation's actual publication, including its
    /// remove-to-publication window. Lost Pending publishers fail closed.
    /// Stopped still requires sealed native zero and a fresh failure sample;
    /// actor intent and evictable history are never native completion proof.
    async fn await_exact_completion(
        &self,
        session_id: &str,
        mut target: ExactShutdownTarget,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        loop {
            let state = *target.completion.borrow_and_update();
            match state {
                ExactCompletionState::Pending => {
                    tokio::time::timeout_at(deadline, target.completion.changed())
                        .await
                        .map_err(|_| {
                            MezError::invalid_state("exact runtime completion deadline expired")
                        })?
                        .map_err(|_| {
                            MezError::invalid_state("exact runtime completion publisher was lost")
                        })?;
                }
                ExactCompletionState::Failed => {
                    return Err(MezError::invalid_state("exact runtime completion failed"));
                }
                ExactCompletionState::Stopped => {
                    if let Some(entry) = self.inner.entries()?.get(session_id) {
                        return Err(if entry.generation != target.generation {
                            MezError::conflict("shutdown target generation was replaced")
                        } else {
                            MezError::invalid_state("exact completion precedes entry settlement")
                        });
                    }
                    if !target.native.is_sealed() || target.native.outstanding() != 0 {
                        return Err(MezError::invalid_state(
                            "exact runtime native completion remains unproven",
                        ));
                    }
                    if let Some(failure) = target.native.cleanup_failure() {
                        return Err(MezError::invalid_state(failure));
                    }
                    return Ok(());
                }
            }
        }
    }

    /// Seals admission before any final native-owner snapshot or awaited drain.
    pub(crate) fn seal_native_admission(&self) -> Result<()> {
        self.fence_native_admission(true)
    }

    /// Stop requests preserve actor lifecycle authority while independent
    /// returned children progress. Final CLI cleanup may also cancel runtimes.
    fn fence_native_admission(&self, cancel_running: bool) -> Result<()> {
        self.inner
            .native_cleanup_sealed
            .store(true, Ordering::Release);
        for entry in self.inner.entries()?.values_mut() {
            entry.native.seal();
            match &mut entry.state {
                SupervisorEntryState::Starting { cancelled } => *cancelled = true,
                SupervisorEntryState::Running { cancel, .. }
                | SupervisorEntryState::Stopping { cancel, .. } => {
                    if cancel_running {
                        let _ = cancel.send(true);
                    }
                }
                SupervisorEntryState::CleanupPending { .. } => {}
            }
        }
        Ok(())
    }

    /// Reports monotonic closure without consulting actor or native handles.
    pub(crate) fn native_admission_is_sealed(&self) -> bool {
        self.inner.native_cleanup_sealed.load(Ordering::Acquire)
    }

    /// Closes host admission and drains native resources independently of actors.
    /// The same absolute deadline covers every session, including cancelled
    /// startup and late worker drops. Pending owners remain in exact entries.
    #[cfg(test)]
    pub(crate) fn drain_native_retirement(
        &self,
        deadline: tokio::time::Instant,
    ) -> impl std::future::Future<Output = Result<()>> + '_ {
        let scope = self.capture_native_continuation_scope(deadline);
        async move {
            let mut scope = scope?;
            self.seal_native_scope(&mut scope, deadline, true)?;
            self.drain_native_captured_scope_with_owner_receipts(
                scope,
                deadline,
                PaneProcessRetirementOwner::cleanup_failure,
            )
            .await
        }
    }

    /// Final host/CLI cleanup includes runtime callback/publication settlement.
    /// Internal native-only progress remains separate to avoid waiting on a
    /// callback from inside its own resource drain. One outer deadline covers
    /// both barriers; failure is sampled again only after publication settles.
    #[cfg(test)]
    pub(crate) fn finish_native_retirement(
        &self,
        deadline: tokio::time::Instant,
    ) -> impl std::future::Future<Output = Result<()>> + '_ {
        let scope = self.capture_native_continuation_scope(deadline);
        self.finish_native_retirement_captured(scope, deadline)
    }

    /// Finishes only a factory-captured scope. Initial CLI finalization uses
    /// this continuation path and cannot reach explicit retry admission.
    pub(crate) async fn finish_native_retirement_captured(
        &self,
        scope: Result<NativeContinuationScope>,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        let mut scope = scope?;
        self.seal_native_scope(&mut scope, deadline, true)?;
        let first = self
            .drain_native_captured_scope_with_owner_receipts(
                scope,
                deadline,
                PaneProcessRetirementOwner::cleanup_failure,
            )
            .await
            .err();
        self.finish_native_publication_barrier(deadline, first)
            .await
    }

    /// Sole host-native mint consumer: reached only by explicit CliExit retry
    /// with preparation captured before that future was first polled.
    pub(crate) async fn finish_cli_native_retry(
        &self,
        prepared: Result<PreparedCliNativeRetry>,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        let scope = self.admit_cli_native_retry(prepared?)?;
        self.finish_native_retirement_captured(Ok(scope), deadline)
            .await
    }

    /// Completes publication independently of native-error success and samples
    /// the sticky receipt only after the registered publication barrier settles.
    async fn finish_native_publication_barrier(
        &self,
        deadline: tokio::time::Instant,
        mut first: Option<MezError>,
    ) -> Result<()> {
        let publication = tokio::time::timeout_at(deadline, async {
            loop {
                let changed = self.inner.changed.notified();
                if !self.has_pending_runtime_completion()? {
                    return Ok(());
                }
                changed.await;
            }
        })
        .await
        .map_err(|_| {
            MezError::invalid_state("host runtime completion publication deadline expired")
        })
        .and_then(|result| result);
        if let Err(error) = publication {
            first.get_or_insert(error);
        }
        match self.native_cleanup_failure() {
            Ok(Some(failure)) => {
                first.get_or_insert_with(|| MezError::invalid_state(failure));
            }
            Ok(None) => {}
            Err(error) => {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Coarse final-publication status, distinct from actual native child count.
    /// Entries stay registered through callback/publication; poisoned status
    /// remains unknown rather than a fabricated settled result.
    pub(crate) fn has_pending_runtime_completion(&self) -> Result<bool> {
        Ok(!self.inner.entries()?.is_empty())
    }

    /// Production reads only the retained owners. The private per-call reader
    /// permits deterministic diagnostic-publication tests without a global hook
    /// or any mutation of child handles, counters or native completion proof.
    #[cfg(test)]
    fn drain_native_with_owner_receipts<'a>(
        &'a self,
        deadline: tokio::time::Instant,
        read_owner_failure: impl FnMut(&PaneProcessRetirementOwner) -> Option<String> + 'a,
    ) -> impl std::future::Future<Output = Result<()>> + 'a {
        let scope = self.capture_native_continuation_scope(deadline);
        async move {
            self.drain_native_captured_scope_with_owner_receipts(
                scope?,
                deadline,
                read_owner_failure,
            )
            .await
        }
    }

    /// Progresses only captured tokens; one rejected owner never skips eligible
    /// siblings, and no per-quantum current-attempt recapture is permitted.
    async fn drain_native_captured_scope_with_owner_receipts(
        &self,
        mut scope: NativeContinuationScope,
        deadline: tokio::time::Instant,
        mut read_owner_failure: impl FnMut(&PaneProcessRetirementOwner) -> Option<String>,
    ) -> Result<()> {
        self.seal_native_scope(&mut scope, deadline, false)?;
        let owners = &mut scope.owners;
        let mut first_error = self.native_cleanup_failure()?.map(MezError::invalid_state);
        for owner in owners.iter_mut() {
            if let Some(failure) = read_owner_failure(&owner.native) {
                first_error.get_or_insert_with(|| MezError::invalid_state(failure));
            }
            if let Some(error) = owner.error.take() {
                first_error.get_or_insert(error);
            }
        }
        loop {
            for owner in owners.iter() {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                if let Some(attempt) = &owner.attempt
                    && let Err(error) =
                        crate::host::async_runtime::advance_native_pane_retirement_for_attempt(
                            &owner.native,
                            attempt,
                            deadline,
                        )
                {
                    first_error.get_or_insert(error);
                }
                tokio::task::yield_now().await;
            }
            self.finalize_settled_native_entries()?;
            let pending: usize = owners.iter().map(|owner| owner.native.outstanding()).sum();
            if pending == 0 {
                // A worker may have published its receipt and reaped while the
                // completion callback is still gated. Zero comes first; read
                // every retained owner's monotonic receipt before success.
                for owner in owners.iter() {
                    if let Some(failure) = read_owner_failure(&owner.native) {
                        first_error.get_or_insert_with(|| MezError::invalid_state(failure));
                    }
                }
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                let pending = MezError::invalid_state(format!(
                    "host native retirement is unproven for {pending} exact owned children"
                ));
                return Err(match first_error {
                    Some(primary) => {
                        MezError::new(primary.kind(), format!("{}; {pending}", primary.message()))
                    }
                    None => pending,
                });
            }
            tokio::time::sleep(
                Duration::from_millis(10)
                    .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
        }
        if let Some(failure) = self.native_cleanup_failure()? {
            first_error.get_or_insert_with(|| MezError::invalid_state(failure));
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Retires each proved-empty pending generation even if a sibling remains
    /// unproven. Failure receipts are preserved separately from resource slots.
    fn finalize_settled_native_entries(&self) -> Result<()> {
        let settled = self
            .inner
            .entries()?
            .iter()
            .filter_map(|(id, entry)| {
                if let SupervisorEntryState::CleanupPending { failure } = &entry.state {
                    (entry.native.outstanding() == 0)
                        .then(|| (id.clone(), entry.generation, failure.clone()))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for (id, generation, failure) in settled {
            self.inner.finish_runtime(
                &id,
                generation,
                SessionSupervisorState::Failed,
                None,
                Some(failure),
            )?;
        }
        Ok(())
    }

    /// Returns private native/completion cleanup evidence independently of history.
    pub(crate) fn native_cleanup_failure(&self) -> Result<Option<String>> {
        self.inner
            .native_failure
            .lock()
            .map(|failure| failure.clone())
            .map_err(|_| MezError::invalid_state("native cleanup failure status is unavailable"))
    }

    /// Counts native obligations even after actor closure or runtime failure.
    pub(crate) fn native_pending_count(&self) -> Result<usize> {
        Ok(self
            .inner
            .entries()?
            .values()
            .map(|entry| entry.native.outstanding())
            .sum())
    }

    /// Returns bounded live and recent-terminal status in stable identity order.
    pub(crate) async fn snapshots(&self) -> Result<Vec<SessionSupervisorSnapshot>> {
        let live = {
            let entries = self.inner.entries()?;
            entries
                .iter()
                .map(|(session_id, entry)| {
                    let (state, handle) = match &entry.state {
                        SupervisorEntryState::CleanupPending { .. } => {
                            (SessionSupervisorState::Stopping, None)
                        }
                        SupervisorEntryState::Starting { .. } => {
                            (SessionSupervisorState::Starting, None)
                        }
                        SupervisorEntryState::Running { handle, .. } => {
                            (SessionSupervisorState::Running, Some(handle.clone()))
                        }
                        SupervisorEntryState::Stopping { handle, cancel } => {
                            let _ = cancel.borrow();
                            (SessionSupervisorState::Stopping, Some(handle.clone()))
                        }
                    };
                    let failure = match &entry.state {
                        SupervisorEntryState::CleanupPending { failure } => Some(failure.clone()),
                        _ => None,
                    };
                    (session_id.clone(), entry.generation, state, handle, failure)
                })
                .collect::<Vec<_>>()
        };
        let mut snapshots = Vec::with_capacity(live.len() + self.inner.terminal()?.len());
        for (session_id, generation, state, handle, failure) in live {
            let (runtime_state, power_inhibition) = match handle {
                Some(handle) => (
                    handle.lifecycle_state().await.ok(),
                    handle.power_inhibition_status().await.ok().flatten(),
                ),
                None => (None, None),
            };
            snapshots.push(SessionSupervisorSnapshot {
                session_id,
                generation,
                state,
                runtime_state,
                power_inhibition,
                failure,
            });
        }
        snapshots.extend(self.inner.terminal()?.iter().cloned());
        snapshots.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then(left.generation.cmp(&right.generation))
        });
        Ok(snapshots)
    }

    /// Removes retained terminal history for one identity without affecting live runtimes.
    pub(crate) fn remove_terminal(&self, session_id: &str) -> Result<usize> {
        let mut terminal = self.inner.terminal()?;
        let before = terminal.len();
        terminal.retain(|snapshot| snapshot.session_id != session_id);
        Ok(before.saturating_sub(terminal.len()))
    }
}

impl SessionSupervisorInner {
    /// Seals an exact generation before deciding whether its entry may retire.
    /// Pending children stay outside terminal history and identity reuse.
    fn retain_native_pending(
        &self,
        session_id: &str,
        generation: u64,
        failure: String,
    ) -> Result<bool> {
        self.retain_native_with_owner_receipt(
            session_id,
            generation,
            failure,
            PaneProcessRetirementOwner::cleanup_failure,
        )
    }

    /// Samples pending before receipt: publication precedes final slot release,
    /// so a zero observation cannot remove a generation with an unseen error.
    /// A prior nonzero snapshot conservatively retains it for finalization.
    fn retain_native_with_owner_receipt(
        &self,
        session_id: &str,
        generation: u64,
        failure: String,
        mut read_owner_failure: impl FnMut(&PaneProcessRetirementOwner) -> Option<String>,
    ) -> Result<bool> {
        let mut entries = self.entries()?;
        if let Some(entry) = entries.get_mut(session_id)
            && entry.generation == generation
        {
            entry.native.seal();
            let pending = entry.native.outstanding();
            if let Some(native_failure) = read_owner_failure(&entry.native) {
                let mut first = self.native_failure.lock().map_err(|_| {
                    MezError::invalid_state("native cleanup failure status is unavailable")
                })?;
                first.get_or_insert(native_failure);
            }
            if pending != 0 {
                entry.state = SupervisorEntryState::CleanupPending { failure };
                self.changed.notify_waiters();
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn entries(&self) -> Result<MutexGuard<'_, HashMap<String, SupervisorEntry>>> {
        self.entries
            .lock()
            .map_err(|_| MezError::invalid_state("session supervisor entries lock was poisoned"))
    }

    fn terminal(&self) -> Result<MutexGuard<'_, VecDeque<SessionSupervisorSnapshot>>> {
        self.terminal.lock().map_err(|_| {
            MezError::invalid_state("session supervisor terminal history lock was poisoned")
        })
    }

    fn finish_start_failure(
        &self,
        session_id: &str,
        generation: u64,
        failure: String,
    ) -> Result<()> {
        if self.retain_native_pending(session_id, generation, failure.clone())? {
            return Ok(());
        }
        let removed = {
            let mut entries = self.entries()?;
            if entries
                .get(session_id)
                .is_some_and(|entry| entry.generation == generation)
            {
                if let Some(entry) = entries.remove(session_id) {
                    entry.completion.send_replace(ExactCompletionState::Failed);
                }
                true
            } else {
                false
            }
        };
        if removed {
            self.push_terminal(SessionSupervisorSnapshot {
                session_id: session_id.to_string(),
                generation,
                state: SessionSupervisorState::Failed,
                runtime_state: None,
                power_inhibition: None,
                failure: Some(failure),
            })?;
            self.changed.notify_waiters();
        }
        Ok(())
    }

    fn finish_runtime(
        &self,
        session_id: &str,
        generation: u64,
        state: SessionSupervisorState,
        runtime_state: Option<RuntimeLifecycleState>,
        failure: Option<String>,
    ) -> Result<()> {
        if self.retain_native_pending(
            session_id,
            generation,
            failure
                .clone()
                .unwrap_or_else(|| "session native retirement remains unproven".to_string()),
        )? {
            return Ok(());
        }
        let accepted = {
            let mut entries = self.entries()?;
            match entries.get_mut(session_id) {
                Some(entry) if entry.generation == generation && !entry.completion_claimed => {
                    entry.completion_claimed = true;
                    true
                }
                _ => false,
            }
        };
        if !accepted {
            return Ok(());
        }
        let mut snapshot = SessionSupervisorSnapshot {
            session_id: session_id.to_string(),
            generation,
            state,
            runtime_state,
            power_inhibition: None,
            failure,
        };
        let callback = self
            .runtime_completion_handler
            .as_ref()
            .map_or(Ok(()), |handler| (handler.0)(&snapshot));
        if let Err(error) = &callback {
            snapshot.state = SessionSupervisorState::Failed;
            snapshot.failure.get_or_insert_with(|| {
                format!("runtime completion callback failed: {error}")
                    .chars()
                    .take(4096)
                    .collect()
            });
        }
        let healthy = state == SessionSupervisorState::Stopped
            && snapshot.failure.is_none()
            && callback.is_ok();
        // Keep the claimed entry in the shutdown barrier throughout terminal
        // publication. A snapshot starting now must see Pending, not absence.
        let publication = self.push_terminal(snapshot);
        if let Some(error) = callback
            .as_ref()
            .err()
            .or_else(|| publication.as_ref().err())
        {
            let mut first = self.native_failure.lock().map_err(|_| {
                MezError::invalid_state("native/completion cleanup failure receipt is unavailable")
            })?;
            first.get_or_insert_with(|| {
                format!("runtime completion cleanup failed: {error}")
                    .chars()
                    .take(4096)
                    .collect()
            });
        }
        let removed = {
            let mut entries = self.entries()?;
            if entries
                .get(session_id)
                .is_some_and(|entry| entry.generation == generation && entry.completion_claimed)
            {
                entries.remove(session_id)
            } else {
                None
            }
        };
        if let Some(entry) = removed {
            let healthy = healthy
                && publication.is_ok()
                && entry.native.is_sealed()
                && entry.native.outstanding() == 0
                && !entry.native.has_cleanup_failure();
            entry.completion.send_replace(if healthy {
                ExactCompletionState::Stopped
            } else {
                ExactCompletionState::Failed
            });
            self.changed.notify_waiters();
        }
        callback.and(publication)
    }

    fn push_terminal(&self, snapshot: SessionSupervisorSnapshot) -> Result<()> {
        if self.terminal_history_limit == 0 {
            return Ok(());
        }
        let mut terminal = self.terminal()?;
        while terminal.len() >= self.terminal_history_limit {
            terminal.pop_front();
        }
        terminal.push_back(snapshot);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use mez_core::ids::SessionId;
    use mez_mux::layout::Size;
    use mez_mux::session::Session;

    use super::*;
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    use crate::host::session::{
        SessionRuntimeConfig, SessionRuntimeLimits, SessionRuntimeStartup, SessionSocketPublication,
    };
    use crate::host::shell::{ResolvedShell, ShellSource};

    mod native_attempt_scopes;
    mod receipt_ordering;
    mod runtime_panic;
    mod stop_completion;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(10_000);

    /// The canary fixture actually retains private entry/terminal/receipt data.
    /// Public CliExit formatting tests must redact it, not use an empty fixture.
    #[test]
    fn private_diagnostic_fixture_contains_only_synthetic_redaction_canaries() {
        let supervisor = SessionSupervisor::private_diagnostic_fixture_for_test();
        let shown = format!("{supervisor:?}");
        for canary in [
            "PRIVATE_NATIVE_HOST_RECEIPT_CANARY",
            "PRIVATE_NATIVE_HOST_ENTRY_CANARY",
            "PRIVATE_NATIVE_HOST_TERMINAL_CANARY",
        ] {
            assert!(shown.contains(canary));
        }
        assert_eq!(supervisor.native_pending_count().unwrap(), 0);
    }

    /// A real native drain supplies the kernel proof; an injected retained error
    /// then exercises disposition of that same now-empty exact generation. The
    /// error is not cleared or called healthy merely to reclaim session capacity.
    #[tokio::test]
    async fn native_failure_verdict_does_not_pin_reaped_generation_or_erase_receipt() {
        let supervisor = SessionSupervisor::new(0);
        let (pid, _) = pending_native_fixture(&supervisor, "$failed-but-reaped", false);
        let native = supervisor
            .inner
            .entries()
            .unwrap()
            .get("$failed-but-reaped")
            .unwrap()
            .native
            .clone();
        super::super::drain_native_pane_retirement(
            &native,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
        supervisor.retain_native_failure_for_test();
        let verdict = supervisor.finish_native_pending_result(
            "$failed-but-reaped",
            1,
            &native,
            "original failure".to_string(),
            Err(MezError::invalid_state(
                "injected native signal failure receipt",
            )),
        );
        assert!(verdict.is_err());
        assert!(!supervisor.contains("$failed-but-reaped").unwrap());
        assert_eq!(supervisor.native_pending_count().unwrap(), 0);
        assert!(supervisor.native_cleanup_failure().unwrap().is_some());
    }

    /// Unavailable status remains an error even when the fixture has no child;
    /// sealing must not manufacture a successful zero observation.
    #[test]
    fn native_status_fault_is_not_reported_as_empty_after_seal() {
        let supervisor = SessionSupervisor::new(0);
        supervisor.poison_native_status_for_test();
        assert!(supervisor.seal_native_admission().is_err());
        assert!(supervisor.native_pending_count().is_err());
    }

    /// Creates a new fixture root without removing or adopting an old path.
    fn native_factory_request(name: &str) -> SessionFactoryRequest {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("mnf-{}-{id}-{nonce:x}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        test_request_at_root(name, id, root)
    }

    /// A post-spawn gate observes the actual factory child without taking it.
    /// Cancellation and injected registry-boundary failure must both leave the
    /// same generation reserved until production stop proves exact native reap.
    #[tokio::test(flavor = "current_thread")]
    async fn factory_postspawn_cancel_and_error_retain_exact_generation_until_reap() {
        for cancel in [true, false] {
            let supervisor = SessionSupervisor::new(0);
            let request = native_factory_request("native-factory");
            let id_value = request.session.id.clone();
            let id = id_value.to_string();
            let root = request.config.root.clone();
            let (spawned, witness) = tokio::sync::oneshot::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            *supervisor.inner.native_startup_probe.lock().unwrap() = Some((
                id.clone(),
                1,
                super::super::NativePaneStartupProbe {
                    spawned,
                    release: released,
                },
            ));
            let starting = supervisor.clone();
            let task = tokio::spawn(async move { starting.start(request).await });
            let witnesses = tokio::time::timeout(Duration::from_secs(10), witness)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(witnesses.len(), 1);
            let (native_pid, start_token) = witnesses[0];
            let pid = rustix::process::Pid::from_raw(i32::try_from(native_pid).unwrap()).unwrap();
            if cancel {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                release.send(true).unwrap();
                let error = task.await.unwrap().unwrap_err();
                assert!(
                    error
                        .message()
                        .contains("injected post-spawn registry-boundary failure")
                );
            }
            assert!(supervisor.contains(&id).unwrap());
            assert_eq!(
                supervisor.native_pending_count().unwrap(),
                1,
                "{native_pid} {start_token:?}"
            );
            let mut duplicate = test_request_at_root("duplicate", 0, root.clone());
            duplicate.session.id = id_value;
            assert!(
                supervisor
                    .start(duplicate)
                    .await
                    .unwrap_err()
                    .message()
                    .contains("already supervised")
            );
            let snapshot = supervisor.snapshots().await.unwrap().pop().unwrap();
            assert_eq!(snapshot.state, SessionSupervisorState::Stopping);
            if !cancel {
                assert!(
                    snapshot
                        .failure
                        .unwrap()
                        .contains("injected post-spawn registry-boundary failure"),
                    "Drop must not replace the original startup cause"
                );
            }
            supervisor.stop(&id, true).await.unwrap();
            assert!(!supervisor.contains(&id).unwrap());
            assert_eq!(
                rustix::process::test_kill_process(pid),
                Err(rustix::io::Errno::SRCH),
                "completed native stop left original child {native_pid} {start_token:?}"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    /// Uses the actual start/spawn closure, not a custom guard future. The
    /// returned handle must remain Running immediately after start; destroying
    /// that Tokio runtime must return the native child to the external host
    /// owner, where a new caller runtime can settle it without an actor.
    #[test]
    fn actual_runtime_spawn_guard_survives_start_return_and_runtime_destruction() {
        let supervisor = SessionSupervisor::new(0);
        let request = native_factory_request("native-runtime-drop");
        let id = request.session.id.to_string();
        let root = request.config.root.clone();
        let (spawned, witness) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        *supervisor.inner.native_startup_probe.lock().unwrap() = Some((
            id.clone(),
            1,
            super::super::NativePaneStartupProbe {
                spawned,
                release: released,
            },
        ));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (handle, witnesses) = runtime.block_on(async {
            let producer = async {
                let witnesses = witness.await.unwrap();
                release.send(false).unwrap();
                witnesses
            };
            let (handle, witnesses) = tokio::join!(supervisor.start(request), producer);
            (handle.unwrap(), witnesses)
        });
        assert!(
            supervisor.lookup(&id).is_ok(),
            "whole completion guard was dropped at start return"
        );
        assert_eq!(witnesses.len(), 1);
        let pid = rustix::process::Pid::from_raw(i32::try_from(witnesses[0].0).unwrap()).unwrap();
        drop(runtime);
        assert!(supervisor.contains(&id).unwrap());
        assert_eq!(supervisor.native_pending_count().unwrap(), 1);
        let retry_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        retry_runtime.block_on(supervisor.stop(&id, true)).unwrap();
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
        assert!(!supervisor.contains(&id).unwrap());
        drop(handle);
        fs::remove_dir_all(root).unwrap();
    }

    /// Sixteen blocked startup reservations must not occupy a whole-operation
    /// prefix that starves the seventeenth exact native owner or its identity.
    #[tokio::test]
    async fn native_round_robin_reaps_owner_seventeen_before_blocked_prefix_settles() {
        let supervisor = SessionSupervisor::new(0);
        let mut gates = Vec::new();
        let mut roots = Vec::new();
        for index in 0..16 {
            let (pid, gate) =
                pending_native_fixture(&supervisor, &format!("$blocked-{index:02}"), true);
            roots.push(pid);
            gates.push(gate);
        }
        let (later, _) = pending_native_fixture(&supervisor, "$later-cleanable", false);
        assert!(
            supervisor
                .shutdown_all(true, Duration::from_millis(150))
                .await
                .is_err()
        );
        assert_eq!(
            rustix::process::test_kill_process(later),
            Err(rustix::io::Errno::SRCH)
        );
        assert!(
            !supervisor.contains("$later-cleanable").unwrap(),
            "settled sibling identity was held by another owner"
        );
        drop(gates);
        supervisor
            .drain_native_retirement(tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        for pid in roots {
            assert_eq!(
                rustix::process::test_kill_process(pid),
                Err(rustix::io::Errno::SRCH)
            );
        }
        assert_eq!(supervisor.native_pending_count().unwrap(), 0);
    }

    /// Spawns the exact fixture child without actor/filesystem setup. The
    /// returned duplicated master is a custody decision for the caller: holding
    /// it excludes kernel PTY hangup as an alternate cause while the exact child
    /// is observed, while dropping it preserves the original handle-return shape
    /// of fixtures that need only slot evidence.
    fn spawn_pending_native_child(
        gate: bool,
    ) -> (
        PaneProcessRetirementOwner,
        rustix::process::Pid,
        Option<mez_mux::process::PaneProcessChildReservation>,
        std::os::fd::OwnedFd,
    ) {
        let native = PaneProcessRetirementOwner::default();
        let gate = gate.then(|| native.reserve().unwrap());
        let process = mez_mux::process::spawn_retained_argv_pty_process(
            std::path::Path::new("/bin/cat"),
            &[],
            &[],
            Size::new(80, 24).unwrap(),
            None,
            native.reserve().unwrap(),
        )
        .unwrap();
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
        let master = process.duplicate_master_fd().unwrap();
        drop(process);
        (native, pid, gate, master)
    }

    /// Inserts the closed-actor generation that owns only the fixture's exact
    /// native obligation. The real production stop/drain path supplies proof.
    fn insert_pending_native_entry(
        supervisor: &SessionSupervisor,
        id: &str,
        native: PaneProcessRetirementOwner,
    ) {
        supervisor.inner.entries().unwrap().insert(
            id.to_string(),
            SupervisorEntry {
                generation: 1,
                native,
                completion: watch::channel(ExactCompletionState::Pending).0,
                completion_claimed: false,
                state: SupervisorEntryState::CleanupPending {
                    failure: "original native fixture failure".to_string(),
                },
            },
        );
    }

    /// Installs only an exact native cleanup obligation, without actor/filesystem
    /// setup, and releases the duplicated master unheld. Callers that need PTY
    /// custody use `pending_native_fixture_with_custody` instead.
    fn pending_native_fixture(
        supervisor: &SessionSupervisor,
        id: &str,
        gate: bool,
    ) -> (
        rustix::process::Pid,
        Option<mez_mux::process::PaneProcessChildReservation>,
    ) {
        let (native, pid, gate, master) = spawn_pending_native_child(gate);
        insert_pending_native_entry(supervisor, id, native);
        drop(master);
        (pid, gate)
    }

    /// Custody variant: the caller keeps the duplicated PTY master so kernel
    /// hangup cannot retire the exact child behind the test's observations.
    fn pending_native_fixture_with_custody(
        supervisor: &SessionSupervisor,
        id: &str,
        gate: bool,
    ) -> (
        rustix::process::Pid,
        Option<mez_mux::process::PaneProcessChildReservation>,
        PaneProcessRetirementOwner,
        std::os::fd::OwnedFd,
    ) {
        let (native, pid, gate, master) = spawn_pending_native_child(gate);
        insert_pending_native_entry(supervisor, id, native.clone());
        (pid, gate, native, master)
    }

    /// Cleanup-pending ownership cannot be evicted by zero terminal history or
    /// removed on a bounded unproven error. Retry uses the same closed-actor
    /// generation and preserves the original failure diagnostic until settlement.
    ///
    /// A real drain lease, not the reservation gate (which is cardinality only),
    /// is the effect barrier here: it deterministically excludes every child
    /// claim during the bounded stop, so the fixture cannot leave the exact child
    /// holding a retained expired effect plan that an ordinary continuation is
    /// contract-forbidden to renew.
    #[tokio::test]
    async fn native_cleanup_pending_survives_history_and_bounded_retry_error() {
        let supervisor = SessionSupervisor::new(0);
        let (pid, gate, native, master) =
            pending_native_fixture_with_custody(&supervisor, "$native-pending", true);
        let barrier = native.begin_drain().unwrap();
        let bounded = supervisor
            .stop_with_native_deadline(
                "$native-pending",
                true,
                tokio::time::Instant::now() + Duration::from_millis(30),
            )
            .await
            .expect_err("unproven obligation must bound the stop");
        assert!(
            bounded
                .message()
                .contains("unproven for 2 exact owned children"),
            "bounded stop must fail on the unproven reservation, not a child deadline: {bounded}"
        );
        // Sanitized phase receipt: while the blocked prefix was held, the drain
        // claimed no child, reaped none, and latched no native failure.
        assert_eq!(native.outstanding(), 2);
        assert_eq!(native.returned_count(), 1);
        assert!(native.cleanup_failure().is_none());
        assert!(supervisor.contains("$native-pending").unwrap());
        assert_eq!(supervisor.remove_terminal("$native-pending").unwrap(), 0);
        let snapshot = supervisor.snapshots().await.unwrap().pop().unwrap();
        assert_eq!(snapshot.state, SessionSupervisorState::Stopping);
        assert_eq!(
            snapshot.failure.as_deref(),
            Some("original native fixture failure")
        );
        drop(barrier);
        drop(gate);
        supervisor.stop("$native-pending", true).await.unwrap();
        assert!(!supervisor.contains("$native-pending").unwrap());
        assert_eq!(native.outstanding(), 0);
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
        drop(master);
    }

    /// Pins the retained initial0 effect-plan contract: once a claimed exact
    /// child's only initial authority has expired, an ordinary continuation may
    /// observe status only and must retain ownership; only a separately prepared
    /// and admitted explicit retry may advance the epoch (SPEC 2267-2306).
    ///
    /// The pre-effect claim is controlled here on purpose, so the retained count
    /// at the failing ordinary stop is this fixture's evidence. It does not
    /// identify the historical 6fe run's child PID, signal, or reap state.
    #[tokio::test]
    async fn native_cleanup_pending_expired_initial_plan_needs_explicit_retry() {
        let supervisor = SessionSupervisor::new(0);
        let (pid, _, native, master) =
            pending_native_fixture_with_custody(&supervisor, "$native-expired", false);
        let attempt = native
            .capture_cleanup_attempt(std::time::Instant::now() + Duration::from_millis(30))
            .unwrap();
        let mut claim = native
            .try_begin_drain_for_attempt(&attempt)
            .unwrap()
            .expect("fixture drain claimed");
        let child = claim.take_returned().expect("exact returned child claimed");
        drop(child);
        drop(claim);
        let retained_deadline = attempt.deadline();
        tokio::time::sleep_until(tokio::time::Instant::from_std(
            retained_deadline + Duration::from_millis(20),
        ))
        .await;
        let primary = supervisor
            .stop_with_native_deadline(
                "$native-expired",
                true,
                tokio::time::Instant::now() + Duration::from_millis(200),
            )
            .await
            .expect_err("an ordinary continuation cannot renew expired authority");
        assert!(
            primary
                .message()
                .contains("native termination deadline expired; exact ownership retained"),
            "ordinary continuation must retain the expired exact plan: {primary}"
        );
        assert_eq!(native.outstanding(), 1);
        assert_eq!(native.returned_count(), 1);
        assert!(native.has_cleanup_failure());
        let retained = supervisor.snapshots().await.unwrap().pop().unwrap();
        assert_eq!(
            retained.failure.as_deref(),
            Some("original native fixture failure")
        );
        // Fixture-only independent cleanup under a separate prepared/admitted
        // token. This is lower-layer test custody, not new automatic host retry
        // authority. Sticky receipts may accompany a successful exact wait.
        let retry = native
            .prepare_explicit_cleanup_retry(std::time::Instant::now() + Duration::from_secs(5))
            .unwrap()
            .admit()
            .expect("expired initial plan authorizes explicit retry");
        assert!(
            attempt.identity().validate(&native).is_err(),
            "stale initial scope must stay stale after explicit retry admission"
        );
        let retry_deadline = tokio::time::Instant::from_std(retry.deadline());
        while native.outstanding() != 0 {
            assert!(
                tokio::time::Instant::now() < retry_deadline,
                "explicit fixture cleanup exceeded its admitted token budget"
            );
            let _ = crate::host::async_runtime::advance_native_pane_retirement_for_attempt(
                &native,
                &retry,
                retry_deadline,
            );
            if native.outstanding() != 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
        // The sticky receipt outlives the reap: the ordinary stop retires the
        // entry but still reports the retained native failure. Neither is erased.
        let sticky = supervisor
            .stop("$native-expired", true)
            .await
            .expect_err("sticky native receipt must not be erased by settlement");
        assert!(
            sticky.message().contains("retained native cleanup failure"),
            "settlement must report the retained native receipt: {sticky}"
        );
        assert!(!supervisor.contains("$native-expired").unwrap());
        drop(master);
    }

    /// A stuck independent cleanup owner cannot prevent a sibling's exact child
    /// from being stopped/reaped within the same enclosing shutdown budget. The
    /// blocked prefix holds a real drain lease (the reservation gate alone is
    /// cardinality only), while the sibling stays fully drainable.
    #[tokio::test]
    async fn native_shutdown_budget_attempts_siblings_after_one_unproven_owner() {
        let supervisor = SessionSupervisor::new(0);
        let (first, gate, blocked, master) =
            pending_native_fixture_with_custody(&supervisor, "$a-pending", true);
        let (sibling, _) = pending_native_fixture(&supervisor, "$b-sibling", false);
        let barrier = blocked.begin_drain().unwrap();
        let start = tokio::time::Instant::now();
        assert!(
            supervisor
                .shutdown_all(true, Duration::from_millis(100))
                .await
                .is_err()
        );
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "shutdown restarted the budget per session"
        );
        assert_eq!(blocked.outstanding(), 2);
        assert!(blocked.cleanup_failure().is_none());
        assert_eq!(
            rustix::process::test_kill_process(sibling),
            Err(rustix::io::Errno::SRCH)
        );
        assert!(supervisor.contains("$a-pending").unwrap());
        drop(barrier);
        drop(gate);
        supervisor.stop("$a-pending", true).await.unwrap();
        assert_eq!(
            rustix::process::test_kill_process(first),
            Err(rustix::io::Errno::SRCH)
        );
        drop(master);
    }

    /// Captured completion guards run even when a runtime task never receives
    /// its first poll. Startup-native resources return into the already retained
    /// host generation rather than disappearing with the dropped future.
    #[tokio::test]
    async fn unpolled_runtime_completion_retains_preexisting_native_owner() {
        let supervisor = SessionSupervisor::new(0);
        let native = PaneProcessRetirementOwner::default();
        let process = mez_mux::process::spawn_retained_argv_pty_process(
            std::path::Path::new("/bin/cat"),
            &[],
            &[],
            Size::new(80, 24).unwrap(),
            None,
            native.reserve().unwrap(),
        )
        .unwrap();
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
        supervisor.inner.entries().unwrap().insert(
            "$unpolled".to_string(),
            SupervisorEntry {
                generation: 7,
                native: native.clone(),
                completion: watch::channel(ExactCompletionState::Pending).0,
                completion_claimed: false,
                state: SupervisorEntryState::Starting { cancelled: false },
            },
        );
        let completion = NativeRuntimeCompletionGuard {
            inner: supervisor.inner.clone(),
            session_id: "$unpolled".to_string(),
            generation: 7,
            armed: true,
        };
        let task = async move {
            let _process = process;
            let _completion = completion;
            std::future::pending::<()>().await;
        };
        drop(task);
        assert!(supervisor.contains("$unpolled").unwrap());
        assert!(
            native.reserve().is_err(),
            "abandoned generation must be sealed before retry"
        );
        assert_eq!(native.returned_count(), 1);
        supervisor.stop("$unpolled", true).await.unwrap();
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
    }

    /// Duplicate concurrent identities are rejected before a second runtime is constructed.
    #[tokio::test(flavor = "current_thread")]
    async fn supervisor_rejects_duplicate_session_identity() {
        let supervisor = SessionSupervisor::default();
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let first = supervisor.start(test_request("first", id)).await.unwrap();
        let error = supervisor
            .start(test_request("duplicate", id))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), MezErrorKind::Conflict);
        assert_eq!(
            supervisor.lookup(first.session_id()).unwrap().session_id(),
            first.session_id()
        );
        supervisor
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
    }

    /// One runtime completion removes only its entry and leaves a sibling actor usable.
    #[tokio::test(flavor = "current_thread")]
    async fn supervisor_isolates_runtime_completion_from_siblings() {
        let supervisor = SessionSupervisor::default();
        let first = supervisor
            .start(test_request(
                "first",
                NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed),
            ))
            .await
            .unwrap();
        let second = supervisor
            .start(test_request(
                "second",
                NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed),
            ))
            .await
            .unwrap();

        first
            .graceful_shutdown("test independent completion")
            .await
            .unwrap();
        wait_until_absent(&supervisor, first.session_id()).await;
        assert_eq!(
            supervisor
                .lookup(second.session_id())
                .unwrap()
                .lifecycle_state()
                .await
                .unwrap(),
            RuntimeLifecycleState::Running
        );
        supervisor
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
    }

    /// Failed factory construction releases the reservation and records a bounded failure.
    #[tokio::test(flavor = "current_thread")]
    async fn supervisor_releases_failed_start_reservation() {
        let supervisor = SessionSupervisor::new(4);
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let mut request = test_request("failed", id);
        request.sockets.publish_control = true;
        request.sockets.message_path = Some(PathBuf::from("relative.sock"));
        assert!(supervisor.start(request).await.is_err());

        let snapshots = supervisor.snapshots().await.unwrap();
        assert!(snapshots.iter().any(|snapshot| {
            snapshot.session_id == SessionId::new('$', id).to_string()
                && snapshot.state == SessionSupervisorState::Failed
        }));
        let retry = supervisor.start(test_request("retry", id)).await.unwrap();
        assert_eq!(retry.session_id(), SessionId::new('$', id).to_string());
        supervisor
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
    }

    /// Cancelling the start future after identity reservation must release the
    /// exact generation so a retry can reuse the stable session identity.
    #[tokio::test(flavor = "current_thread")]
    async fn supervisor_cancellation_releases_start_reservation() {
        let supervisor = SessionSupervisor::new(4);
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let session_id = SessionId::new('$', id).to_string();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        *supervisor.inner.start_reservation_probe.lock().unwrap() =
            Some((started.clone(), release));
        let started_wait = started.notified();
        let starting_supervisor = supervisor.clone();
        let task = tokio::spawn(async move {
            starting_supervisor
                .start(test_request("cancelled", id))
                .await
        });

        started_wait.await;
        assert!(supervisor.contains(&session_id).unwrap());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!supervisor.contains(&session_id).unwrap());
        assert!(
            supervisor
                .snapshots()
                .await
                .unwrap()
                .iter()
                .any(|snapshot| {
                    snapshot.session_id == session_id
                        && snapshot.state == SessionSupervisorState::Failed
                        && snapshot
                            .failure
                            .as_deref()
                            .is_some_and(|failure| failure.contains("future was cancelled"))
                })
        );

        *supervisor.inner.start_reservation_probe.lock().unwrap() = None;
        let retry = supervisor
            .start(test_request("cancelled-retry", id))
            .await
            .unwrap();
        assert_eq!(retry.session_id(), session_id);
        supervisor
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
    }

    /// Whole-supervisor shutdown settles every runtime and retains bounded terminal status.
    #[tokio::test(flavor = "current_thread")]
    async fn supervisor_shutdown_all_is_bounded_and_deterministic() {
        let supervisor = SessionSupervisor::new(2);
        for name in ["one", "two", "three"] {
            supervisor
                .start(test_request(
                    name,
                    NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed),
                ))
                .await
                .unwrap();
        }
        supervisor
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        let snapshots = supervisor.snapshots().await.unwrap();
        assert_eq!(snapshots.len(), 2);
        assert!(snapshots.iter().all(|snapshot| matches!(
            snapshot.state,
            SessionSupervisorState::Stopped | SessionSupervisorState::Failed
        )));
    }

    /// A stale completion callback must not remove the current generation for
    /// an identity that has already been replaced.
    #[tokio::test(flavor = "current_thread")]
    async fn supervisor_ignores_stale_generation_completion() {
        let supervisor = SessionSupervisor::default();
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let handle = supervisor.start(test_request("current", id)).await.unwrap();
        let current = supervisor
            .snapshots()
            .await
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.session_id == handle.session_id())
            .unwrap();

        supervisor
            .inner
            .finish_runtime(
                handle.session_id(),
                current.generation.saturating_sub(1),
                SessionSupervisorState::Failed,
                Some(RuntimeLifecycleState::Failed),
                Some("stale callback".to_string()),
            )
            .unwrap();

        assert!(supervisor.lookup(handle.session_id()).is_ok());
        supervisor
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
    }

    async fn wait_until_absent(supervisor: &SessionSupervisor, session_id: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if supervisor.lookup(session_id).is_err() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    fn test_request(name: &str, id: u64) -> SessionFactoryRequest {
        let root = test_root(name, id);
        test_request_at_root(name, id, root)
    }

    /// Shares request construction without adopting or deleting fixture roots.
    fn test_request_at_root(name: &str, id: u64, root: PathBuf) -> SessionFactoryRequest {
        let mut session = Session::new_default(
            ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
            Size::new(80, 24).unwrap(),
        )
        .unwrap();
        session.id = SessionId::new('$', id);
        session.name = name.to_string();
        SessionFactoryRequest {
            session,
            owner_uid: crate::runtime::current_effective_uid(),
            created_at_unix_seconds: 100,
            config: SessionRuntimeConfig {
                layers: vec![ConfigLayer {
                    name: "session-supervisor-test".to_string(),
                    path: None,
                    format: ConfigFormat::Toml,
                    scope: ConfigScope::Primary,
                    trusted: true,
                    text: "[agents]\nshell_mode = \"pane\"\n[permissions]\nsandbox = \"policy-only\"\n"
                        .to_string(),
                }],
                root: root.clone(),
            },
            sockets: SessionSocketPublication {
                control_path: root.join("control.sock"),
                publish_control: false,
                message_path: None,
                event_path: None,
                publish_registry: false,
            },
            limits: SessionRuntimeLimits::default(),
            startup: SessionRuntimeStartup::Initial {
                explicit_command: Some("cat >/dev/null".to_string()),
                start_directory: None,
                environment: None,
            },
        }
    }

    fn test_root(name: &str, id: u64) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mez-session-supervisor-{}-{name}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
}
