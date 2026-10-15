//! Concurrent ownership and lifecycle isolation for multiple session runtimes.
//!
//! `SessionSupervisor` sits above `SessionFactory`: it reserves stable session
//! identities before asynchronous construction, runs each ready runtime in an
//! independent Tokio task, and indexes only cloneable actor handles. Map locks
//! are never held across factory, actor, or task awaits. Every entry carries a
//! monotonically increasing generation so completion from an older task cannot
//! remove or overwrite a replacement session with the same identity.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use mez_mux::process::PaneProcessRetirementOwner;
use tokio::sync::{Notify, watch};

use super::{SessionFactory, SessionFactoryRequest, SessionRuntimeHandle};
use crate::error::{MezError, MezErrorKind, Result};
use crate::runtime::{RuntimeLifecycleState, RuntimePowerInhibitionStatus};

const DEFAULT_TERMINAL_HISTORY_LIMIT: usize = 64;

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
    /// Secret-free failure diagnostic for terminal failed entries.
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
    /// One bounded private native receipt survives entry/history removal.
    native_failure: Mutex<Option<String>>,
    #[cfg(test)]
    start_reservation_probe: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    #[cfg(test)]
    fail_next_stop: AtomicU64,
}

#[derive(Debug)]
struct SupervisorEntry {
    generation: u64,
    /// Exists before factory startup; never stored in evictable terminal history.
    native: PaneProcessRetirementOwner,
    state: SupervisorEntryState,
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
                fail_next_stop: AtomicU64::new(0),
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

        let runtime = match SessionFactory::create_with_native_owner(request, native.clone()).await
        {
            Ok(runtime) => runtime,
            Err(error) => {
                self.inner
                    .finish_start_failure(&session_id, generation, error.to_string())?;
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
            let completion = runtime.run(Vec::new(), cancellation).await;
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
            completion_guard.armed = false;
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
    pub(crate) async fn stop(&self, session_id: &str, force: bool) -> Result<()> {
        self.stop_with_native_deadline(
            session_id,
            force,
            tokio::time::Instant::now() + super::NATIVE_PANE_RETIREMENT_TIMEOUT,
        )
        .await
    }

    /// Shares the enclosing shutdown budget with exact pending-child retry.
    async fn stop_with_native_deadline(
        &self,
        session_id: &str,
        force: bool,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        #[cfg(test)]
        if self.inner.fail_next_stop.swap(0, Ordering::SeqCst) != 0 {
            return Err(MezError::invalid_state(
                "injected session supervisor stop failure",
            ));
        }
        let pending = {
            let entries = self.inner.entries()?;
            entries.get(session_id).and_then(|entry| {
                if let SupervisorEntryState::CleanupPending { failure } = &entry.state {
                    Some((entry.generation, entry.native.clone(), failure.clone()))
                } else {
                    None
                }
            })
        };
        if let Some((generation, native, failure)) = pending {
            native.request_shutdown(force);
            native.seal();
            super::drain_native_pane_retirement(&native, deadline).await?;
            self.inner.finish_runtime(
                session_id,
                generation,
                SessionSupervisorState::Failed,
                None,
                Some(failure),
            )?;
            return Ok(());
        }
        let action = {
            let mut entries = self.inner.entries()?;
            let entry = entries.get_mut(session_id).ok_or_else(|| {
                MezError::new(
                    MezErrorKind::NotFound,
                    format!("session `{session_id}` is not supervised"),
                )
            })?;
            match &mut entry.state {
                SupervisorEntryState::CleanupPending { .. } => {
                    return Err(MezError::invalid_state(
                        "native cleanup state changed; retry exact session stop",
                    ));
                }
                SupervisorEntryState::Starting { cancelled } => {
                    *cancelled = true;
                    None
                }
                SupervisorEntryState::Running { handle, cancel } => {
                    let handle = handle.clone();
                    let cancel = cancel.clone();
                    entry.state = SupervisorEntryState::Stopping {
                        handle: handle.clone(),
                        cancel: cancel.clone(),
                    };
                    Some((handle, cancel))
                }
                SupervisorEntryState::Stopping { handle, cancel } => {
                    Some((handle.clone(), cancel.clone()))
                }
            }
        };
        self.inner.changed.notify_waiters();
        let Some((handle, cancel)) = action else {
            return Ok(());
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

    #[cfg(test)]
    pub(crate) fn fail_next_stop(&self) {
        self.inner.fail_next_stop.store(1, Ordering::SeqCst);
    }

    /// Stops every live runtime and waits boundedly for all matching tasks to settle.
    pub(crate) async fn shutdown_all(&self, force: bool, timeout: Duration) -> Result<()> {
        use futures_util::StreamExt;
        let deadline = tokio::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| {
                MezError::invalid_args("session shutdown budget is not representable")
            })?;
        let mut session_ids = {
            let entries = self.inner.entries()?;
            entries
                .iter()
                .filter_map(|(id, entry)| {
                    entry.native.request_shutdown(force);
                    // Pending native retries are progressed in fair quanta below,
                    // not held as whole-lifetime futures ahead of owner seventeen.
                    (!matches!(entry.state, SupervisorEntryState::CleanupPending { .. }))
                        .then(|| id.clone())
                })
                .collect::<Vec<_>>()
        };
        session_ids.sort();
        let mut stops =
            futures_util::stream::iter(session_ids.into_iter().map(|session_id| async move {
                tokio::time::timeout_at(
                    deadline,
                    self.stop_with_native_deadline(&session_id, force, deadline),
                )
                .await
                .map_err(|_| MezError::invalid_state("session supervisor stop deadline expired"))?
            }))
            .buffer_unordered(16);
        let mut first_error = None;
        while let Some(result) = stops.next().await {
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = self.drain_native_retirement(deadline).await {
            first_error.get_or_insert(error);
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
        match (first_error, settlement) {
            (None, result) => result,
            (Some(primary), Ok(())) => Err(primary),
            (Some(primary), Err(settlement)) => Err(MezError::new(
                primary.kind(),
                format!(
                    "{}; remaining session cleanup: {settlement}",
                    primary.message()
                ),
            )),
        }
    }

    /// Seals admission before any final native-owner snapshot or awaited drain.
    pub(crate) fn seal_native_admission(&self) -> Result<()> {
        self.inner
            .native_cleanup_sealed
            .store(true, Ordering::Release);
        for entry in self.inner.entries()?.values_mut() {
            entry.native.seal();
            match &mut entry.state {
                SupervisorEntryState::Starting { cancelled } => *cancelled = true,
                SupervisorEntryState::Running { cancel, .. }
                | SupervisorEntryState::Stopping { cancel, .. } => {
                    let _ = cancel.send(true);
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
    pub(crate) async fn drain_native_retirement(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        self.seal_native_admission()?;
        let owners = {
            self.inner
                .entries()?
                .values()
                .map(|entry| entry.native.clone())
                .collect::<Vec<_>>()
        };
        let mut first_error = self.native_cleanup_failure()?.map(MezError::invalid_state);
        loop {
            for owner in &owners {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                if let Err(error) =
                    crate::host::async_runtime::advance_native_pane_retirement(owner, deadline)
                {
                    first_error.get_or_insert(error);
                }
                tokio::task::yield_now().await;
            }
            let pending: usize = owners
                .iter()
                .map(PaneProcessRetirementOwner::outstanding)
                .sum();
            if pending == 0 {
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
        if let Some(failure) = self.native_cleanup_failure()? {
            first_error.get_or_insert_with(|| MezError::invalid_state(failure));
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Returns private native failure evidence independently of terminal history.
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
        let mut entries = self.entries()?;
        if let Some(entry) = entries.get_mut(session_id)
            && entry.generation == generation
        {
            entry.native.seal();
            if let Some(native_failure) = entry.native.cleanup_failure() {
                let mut first = self.native_failure.lock().map_err(|_| {
                    MezError::invalid_state("native cleanup failure status is unavailable")
                })?;
                first.get_or_insert(native_failure);
            }
            if entry.native.outstanding() != 0 {
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
                entries.remove(session_id);
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
        let accepted = self
            .entries()?
            .get(session_id)
            .is_some_and(|entry| entry.generation == generation);
        if !accepted {
            return Ok(());
        }
        let snapshot = SessionSupervisorSnapshot {
            session_id: session_id.to_string(),
            generation,
            state,
            runtime_state,
            power_inhibition: None,
            failure,
        };
        if let Some(handler) = &self.runtime_completion_handler {
            let _ = (handler.0)(&snapshot);
        }
        let removed = {
            let mut entries = self.entries()?;
            if entries
                .get(session_id)
                .is_some_and(|entry| entry.generation == generation)
            {
                entries.remove(session_id);
                true
            } else {
                false
            }
        };
        if removed {
            self.push_terminal(snapshot)?;
            self.changed.notify_waiters();
        }
        Ok(())
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

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(10_000);

    /// Installs only an exact native cleanup obligation, without actor/filesystem
    /// setup. The real production stop/drain path supplies all retirement proof.
    fn pending_native_fixture(
        supervisor: &SessionSupervisor,
        id: &str,
        gate: bool,
    ) -> (
        rustix::process::Pid,
        Option<mez_mux::process::PaneProcessChildReservation>,
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
        supervisor.inner.entries().unwrap().insert(
            id.to_string(),
            SupervisorEntry {
                generation: 1,
                native,
                state: SupervisorEntryState::CleanupPending {
                    failure: "original native fixture failure".to_string(),
                },
            },
        );
        drop(process);
        (pid, gate)
    }

    /// Cleanup-pending ownership cannot be evicted by zero terminal history or
    /// removed on a bounded unproven error. Retry uses the same closed-actor
    /// generation and preserves the original failure diagnostic until settlement.
    #[tokio::test]
    async fn native_cleanup_pending_survives_history_and_bounded_retry_error() {
        let supervisor = SessionSupervisor::new(0);
        let (pid, gate) = pending_native_fixture(&supervisor, "$native-pending", true);
        assert!(
            supervisor
                .stop_with_native_deadline(
                    "$native-pending",
                    true,
                    tokio::time::Instant::now() + Duration::from_millis(30)
                )
                .await
                .is_err()
        );
        assert!(supervisor.contains("$native-pending").unwrap());
        assert_eq!(supervisor.remove_terminal("$native-pending").unwrap(), 0);
        let snapshot = supervisor.snapshots().await.unwrap().pop().unwrap();
        assert_eq!(snapshot.state, SessionSupervisorState::Stopping);
        assert_eq!(
            snapshot.failure.as_deref(),
            Some("original native fixture failure")
        );
        drop(gate);
        supervisor.stop("$native-pending", true).await.unwrap();
        assert!(!supervisor.contains("$native-pending").unwrap());
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
    }

    /// A stuck independent cleanup owner cannot prevent a sibling's exact child
    /// from being stopped/reaped within the same enclosing shutdown budget.
    #[tokio::test]
    async fn native_shutdown_budget_attempts_siblings_after_one_unproven_owner() {
        let supervisor = SessionSupervisor::new(0);
        let (first, gate) = pending_native_fixture(&supervisor, "$a-pending", true);
        let (sibling, _) = pending_native_fixture(&supervisor, "$b-sibling", false);
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
        assert_eq!(
            rustix::process::test_kill_process(sibling),
            Err(rustix::io::Errno::SRCH)
        );
        assert!(supervisor.contains("$a-pending").unwrap());
        drop(gate);
        supervisor.stop("$a-pending", true).await.unwrap();
        assert_eq!(
            rustix::process::test_kill_process(first),
            Err(rustix::io::Errno::SRCH)
        );
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
