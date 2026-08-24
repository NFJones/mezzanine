//! Concurrent ownership and lifecycle isolation for multiple session runtimes.
//!
//! `SessionSupervisor` sits above `SessionFactory`: it reserves stable session
//! identities before asynchronous construction, runs each ready runtime in an
//! independent Tokio task, and indexes only cloneable actor handles. Map locks
//! are never held across factory, actor, or task awaits. Every entry carries a
//! monotonically increasing generation so completion from an older task cannot
//! remove or overwrite a replacement session with the same identity.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{Notify, watch};

use super::{SessionFactory, SessionFactoryRequest, SessionRuntimeHandle};
use crate::error::{MezError, MezErrorKind, Result};
use crate::runtime::RuntimeLifecycleState;

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
    #[cfg(test)]
    start_reservation_probe: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
}

#[derive(Debug)]
struct SupervisorEntry {
    generation: u64,
    state: SupervisorEntryState,
}

#[derive(Debug)]
enum SupervisorEntryState {
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
                #[cfg(test)]
                start_reservation_probe: Mutex::new(None),
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
        {
            let mut entries = self.inner.entries()?;
            if entries.contains_key(&session_id) {
                return Err(MezError::conflict(format!(
                    "session `{session_id}` is already supervised"
                )));
            }
            entries.insert(
                session_id.clone(),
                SupervisorEntry {
                    generation,
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

        let runtime = match SessionFactory::create(request).await {
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
                entries.remove(&session_id);
                drop(entries);
                drop(runtime);
                self.inner.push_terminal(SessionSupervisorSnapshot {
                    session_id: session_id.clone(),
                    generation,
                    state: SessionSupervisorState::Stopped,
                    runtime_state: None,
                    failure: None,
                })?;
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
        let action = {
            let mut entries = self.inner.entries()?;
            let entry = entries.get_mut(session_id).ok_or_else(|| {
                MezError::new(
                    MezErrorKind::NotFound,
                    format!("session `{session_id}` is not supervised"),
                )
            })?;
            match &mut entry.state {
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

    /// Stops every live runtime and waits boundedly for all matching tasks to settle.
    pub(crate) async fn shutdown_all(&self, force: bool, timeout: Duration) -> Result<()> {
        let mut session_ids = {
            let entries = self.inner.entries()?;
            entries.keys().cloned().collect::<Vec<_>>()
        };
        session_ids.sort();
        for session_id in session_ids {
            self.stop(&session_id, force).await?;
        }
        tokio::time::timeout(timeout, async {
            loop {
                let changed = self.inner.changed.notified();
                if self.inner.entries()?.is_empty() {
                    return Ok(());
                }
                changed.await;
            }
        })
        .await
        .map_err(|_| MezError::invalid_state("session supervisor shutdown timed out"))?
    }

    /// Returns bounded live and recent-terminal status in stable identity order.
    pub(crate) async fn snapshots(&self) -> Result<Vec<SessionSupervisorSnapshot>> {
        let live = {
            let entries = self.inner.entries()?;
            entries
                .iter()
                .map(|(session_id, entry)| {
                    let (state, handle) = match &entry.state {
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
                    (session_id.clone(), entry.generation, state, handle)
                })
                .collect::<Vec<_>>()
        };
        let mut snapshots = Vec::with_capacity(live.len() + self.inner.terminal()?.len());
        for (session_id, generation, state, handle) in live {
            let runtime_state = match handle {
                Some(handle) => handle.lifecycle_state().await.ok(),
                None => None,
            };
            snapshots.push(SessionSupervisorSnapshot {
                session_id,
                generation,
                state,
                runtime_state,
                failure: None,
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
        );
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
