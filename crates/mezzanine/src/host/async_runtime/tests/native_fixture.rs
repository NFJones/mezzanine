//! Failure-safe ownership for real-shell async test fixtures.
//!
//! This is test infrastructure, not a production lifecycle API. Three original
//! futures remain concurrently polled after an early error/panic while a single
//! actor-close request is serviced. Completed outputs live outside the timed
//! join, so cancellation cannot discard an already returned service. Only actual
//! completion counts as joined. Final assertions happen after bounded retirement.
//!
//! Storage and an external native-owner reference are preserving guards BEFORE
//! spawn, including post-spawn hook failure and actor construction panic. Unknown
//! settlement intentionally retains that same reference and generated storage
//! until process exit; there is no background reaper, new deadline, destructor
//! logger, or late final-owner best-effort signal. Native waits use one captured
//! pre-spawn token and the existing nonblocking guard-release/drain contracts.
//! Panic hooks may already have printed before catch_unwind; custom compound
//! panic payloads preserve secondary evidence structurally, not a promise that
//! libtest's default hook renders arbitrary payload fields.

use std::any::Any;
use std::cell::RefCell;
use std::future::Future;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use futures_util::FutureExt;
use mez_mux::process::{PaneProcessCleanupAttempt, PaneProcessRetirementOwner};

use super::{AsyncRuntimeActorFixture, MezError, RuntimeLifecycleState, RuntimeSessionService};
use crate::host::async_runtime::{
    AsyncRuntimeActorExit, AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

/// An original stage, never a PID or reconstructed native identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    Client,
    Supervisor,
    Actor,
}

/// Failure ownership is non-Clone, retaining arbitrary original panic payloads.
pub(super) enum Failure {
    Error(MezError),
    Panic(Box<dyn Any + Send>),
    UnexpectedCompletion,
    Deadline,
    Cancelled,
}

impl std::fmt::Debug for Failure {
    /// Keeps arbitrary panic data owned, without guessing a display codec.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error(error) => formatter.debug_tuple("Error").field(error).finish(),
            Self::Panic(payload) => formatter
                .debug_tuple("Panic")
                .field(&payload.as_ref().type_id())
                .finish(),
            Self::UnexpectedCompletion => formatter.write_str("UnexpectedCompletion"),
            Self::Deadline => formatter.write_str("Deadline"),
            Self::Cancelled => formatter.write_str("Cancelled"),
        }
    }
}

/// A real return value may coexist with unexpected-completion failure evidence.
pub(super) struct StageResult<T> {
    pub(super) value: Option<T>,
    pub(super) failure: Option<Failure>,
    pub(super) completed: bool,
}

impl<T> StageResult<T> {
    /// Cancellation is explicitly not an affirmative stage-completion receipt.
    fn cancelled() -> Self {
        Self {
            value: None,
            failure: Some(Failure::Cancelled),
            completed: false,
        }
    }
}

/// Shared one-shot actor-close admission for normal and failure paths alike.
#[derive(Clone, Default)]
pub(super) struct CloseGate(Arc<AtomicBool>);

/// Notification carries completion only; original supervisor data stays owned.
pub(super) struct JoinControl {
    pub(super) stop: Arc<AtomicBool>,
    pub(super) close: CloseGate,
    pub(super) supervisor_done: Option<tokio::sync::oneshot::Sender<()>>,
}

impl CloseGate {
    /// Issues at most one request while the caller keeps the actor polled.
    pub(super) async fn close(
        &self,
        handle: &AsyncRuntimeSessionHandle,
    ) -> Result<RuntimeLifecycleState, MezError> {
        if self.0.swap(true, Ordering::SeqCst) {
            return Err(MezError::invalid_state(
                "fixture actor close was already requested",
            ));
        }
        handle.shutdown().await
    }
}

/// Completed results survive the deadline selecting over the owning join.
pub(super) struct Joined<C, W, A = AsyncRuntimeActorExit> {
    pub(super) client: StageResult<C>,
    pub(super) supervisor: StageResult<W>,
    pub(super) actor: StageResult<A>,
    order: Vec<Stage>,
    pub(super) cleanup: Vec<String>,
    pub(super) timed_out: bool,
    extra: Vec<(Stage, Failure)>,
}

/// Final compound panic owns the original payloads and all secondary evidence.
#[derive(Debug)]
pub(super) struct FixtureFailure {
    pub(super) failures: Vec<(Stage, Failure)>,
    pub(super) cleanup: Vec<String>,
    pub(super) pending: usize,
    pub(super) root: PathBuf,
}

impl<C, W> Joined<C, W> {
    /// Transfers original successful outputs only after the caller retired the
    /// fixture. Failure keeps every original panic payload in one owned report.
    pub(super) fn finish(
        mut self,
        fixture: &NativeFixture,
    ) -> Result<(C, W, AsyncRuntimeActorExit), Box<FixtureFailure>> {
        if fixture.settled
            && !self.timed_out
            && self.cleanup.is_empty()
            && self.client.failure.is_none()
            && self.supervisor.failure.is_none()
            && self.actor.failure.is_none()
        {
            if let (Some(client), Some(worker), Some(actor)) = (
                self.client.value.take(),
                self.supervisor.value.take(),
                self.actor.value.take(),
            ) {
                return Ok((client, worker, actor));
            }
        }
        let mut failures = Vec::new();
        for stage in
            self.order
                .iter()
                .copied()
                .chain([Stage::Client, Stage::Supervisor, Stage::Actor])
        {
            let failure = match stage {
                Stage::Client => self.client.failure.take(),
                Stage::Supervisor => self.supervisor.failure.take(),
                Stage::Actor => self.actor.failure.take(),
            };
            if let Some(failure) = failure {
                failures.push((stage, failure));
            }
        }
        if self.timed_out {
            failures.push((Stage::Client, Failure::Deadline));
        }
        failures.append(&mut self.extra);
        Err(Box::new(FixtureFailure {
            failures,
            cleanup: self.cleanup,
            pending: fixture.owner().outstanding(),
            root: fixture.root.clone(),
        }))
    }
}

/// Records completion before notifying the one failure coordinator.
async fn capture<T>(
    future: impl Future<Output = Result<T, MezError>>,
    stage: Stage,
    slot: Rc<RefCell<Option<StageResult<T>>>>,
    order: Rc<RefCell<Vec<Stage>>>,
    failed: Arc<tokio::sync::Notify>,
    stop: Arc<AtomicBool>,
    completed: Option<tokio::sync::oneshot::Sender<()>>,
) {
    let mut result = match std::panic::AssertUnwindSafe(future).catch_unwind().await {
        Ok(Ok(value)) => StageResult {
            value: Some(value),
            failure: None,
            completed: true,
        },
        Ok(Err(error)) => StageResult {
            value: None,
            failure: Some(Failure::Error(error)),
            completed: true,
        },
        Err(payload) => StageResult {
            value: None,
            failure: Some(Failure::Panic(payload)),
            completed: true,
        },
    };
    if result.failure.is_none() && stage != Stage::Client && !stop.load(Ordering::SeqCst) {
        result.failure = Some(Failure::UnexpectedCompletion);
    }
    let is_failure = result.failure.is_some();
    *slot.borrow_mut() = Some(result);
    // Sending to a dropped receiver cannot discard the retained original error.
    if let Some(completed) = completed {
        let _ = completed.send(());
    }
    if is_failure {
        let first = order.borrow().is_empty();
        order.borrow_mut().push(stage);
        if first {
            failed.notify_one();
        }
    }
}

/// Coordinates original stages without knowing native process or actor types.
/// The one failure callback is polled concurrently with the same stage futures,
/// including the actor needed to answer a close request. No futures are spawned.
async fn join_originals<C, W, A, S: Future<Output = Result<(), MezError>>>(
    client: impl Future<Output = Result<C, MezError>>,
    worker: impl Future<Output = Result<W, MezError>>,
    actor: impl Future<Output = Result<A, MezError>>,
    deadline: tokio::time::Instant,
    stop: Arc<AtomicBool>,
    shutdown: impl FnOnce() -> S,
) -> Joined<C, W, A> {
    join_originals_notified(client, worker, actor, deadline, stop, shutdown, None).await
}

/// Both real adapters and dropped-receiver regressions use this capture path.
async fn join_originals_notified<C, W, A, S: Future<Output = Result<(), MezError>>>(
    client: impl Future<Output = Result<C, MezError>>,
    worker: impl Future<Output = Result<W, MezError>>,
    actor: impl Future<Output = Result<A, MezError>>,
    deadline: tokio::time::Instant,
    stop: Arc<AtomicBool>,
    shutdown: impl FnOnce() -> S,
    supervisor_done: Option<tokio::sync::oneshot::Sender<()>>,
) -> Joined<C, W, A> {
    if tokio::time::Instant::now() >= deadline {
        return Joined {
            client: StageResult::cancelled(),
            supervisor: StageResult::cancelled(),
            actor: StageResult::cancelled(),
            order: Vec::new(),
            cleanup: vec!["stage polling skipped: original deadline exhausted".into()],
            timed_out: true,
            extra: Vec::new(),
        };
    }
    let client_slot = Rc::new(RefCell::new(None));
    let worker_slot = Rc::new(RefCell::new(None));
    let actor_slot = Rc::new(RefCell::new(None));
    let order = Rc::new(RefCell::new(Vec::new()));
    let failed = Arc::new(tokio::sync::Notify::new());
    let stages = async {
        tokio::join!(
            capture(
                client,
                Stage::Client,
                client_slot.clone(),
                order.clone(),
                failed.clone(),
                stop.clone(),
                None
            ),
            capture(
                worker,
                Stage::Supervisor,
                worker_slot.clone(),
                order.clone(),
                failed.clone(),
                stop.clone(),
                supervisor_done
            ),
            capture(
                actor,
                Stage::Actor,
                actor_slot.clone(),
                order.clone(),
                failed.clone(),
                stop.clone(),
                None
            )
        );
    };
    let mut cleanup = Vec::new();
    let mut extra = Vec::new();
    let timed_out;
    {
        tokio::pin!(stages);
        let running = async {
            tokio::select! {
                biased;
                () = &mut stages => {},
                () = failed.notified() => {
                    stop.store(true, Ordering::SeqCst);
                    if tokio::time::Instant::now() < deadline {
                        let close = async {
                            match std::panic::AssertUnwindSafe(async { shutdown().await }).catch_unwind().await {
                                Ok(Ok(())) => {},
                                Ok(Err(error)) => {
                                    cleanup.push(format!("failure-triggered actor close: {error}"));
                                    extra.push((Stage::Actor, Failure::Error(error)));
                                },
                                Err(payload) => {
                                    cleanup.push("failure-triggered actor close panicked".into());
                                    extra.push((Stage::Actor, Failure::Panic(payload)));
                                }
                            }
                        };
                        tokio::join!(stages, close);
                    } else {
                        cleanup.push("failure-triggered close skipped: original deadline exhausted".into());
                    }
                }
            }
        };
        tokio::pin!(running);
        timed_out = tokio::select! {
            biased;
            () = tokio::time::sleep_until(deadline) => true,
            () = &mut running => false,
        };
    }
    let client = client_slot
        .borrow_mut()
        .take()
        .unwrap_or_else(StageResult::cancelled);
    let supervisor = worker_slot
        .borrow_mut()
        .take()
        .unwrap_or_else(StageResult::cancelled);
    let actor = actor_slot
        .borrow_mut()
        .take()
        .unwrap_or_else(StageResult::cancelled);
    let order = std::mem::take(&mut *order.borrow_mut());
    Joined {
        client,
        supervisor,
        actor,
        order,
        cleanup,
        timed_out: timed_out || tokio::time::Instant::now() >= deadline,
        extra,
    }
}

/// Owns preserving storage/custody before any fallible native startup action.
/// Retains setup primary ownership before polling independently fallible cleanup.
async fn compound_setup_failure(
    primary: Failure,
    cleanup: impl Future<Output = Result<(), MezError>>,
) -> Vec<(Stage, Failure)> {
    let mut failures = vec![(Stage::Actor, primary)];
    match std::panic::AssertUnwindSafe(cleanup).catch_unwind().await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => failures.push((Stage::Actor, Failure::Error(error))),
        Err(payload) => failures.push((Stage::Actor, Failure::Panic(payload))),
    }
    failures
}

/// Owns preserving storage/custody before any fallible native startup action.
pub(super) struct NativeFixture {
    root: PathBuf,
    environment: Vec<(String, String)>,
    pub(super) deadline: tokio::time::Instant,
    native: Option<ManuallyDrop<PaneProcessRetirementOwner>>,
    attempt: Option<PaneProcessCleanupAttempt>,
    settled: bool,
    delete_storage: bool,
    retired: bool,
}

impl NativeFixture {
    /// Creates private normal-startup files without touching ambient user state.
    pub(super) fn new(label: &str, budget: std::time::Duration) -> Self {
        let deadline = tokio::time::Instant::now() + budget;
        let unique = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("mez-owned-{label}-{}-{unique}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = root.join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            home.join(".zshrc"),
            "# Owned inert fixture startup; no first-use wizard.\n",
        )
        .unwrap();
        std::fs::set_permissions(home.join(".zshrc"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let mut environment = vec![
            ("HOME".into(), home.to_string_lossy().into_owned()),
            ("ZDOTDIR".into(), home.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("TMPDIR".into(), root.to_string_lossy().into_owned()),
        ];
        for key in [
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
        ] {
            let path = home.join(key);
            std::fs::create_dir(&path).unwrap();
            environment.push((key.into(), path.to_string_lossy().into_owned()));
        }
        Self {
            root,
            environment,
            deadline,
            native: None,
            attempt: None,
            settled: false,
            delete_storage: false,
            retired: false,
        }
    }

    /// Returns generated fixture storage; it stays preserved on unknown cleanup.
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// Shares the exact originally installed native owner, not a PID lookup.
    pub(super) fn owner(&self) -> &PaneProcessRetirementOwner {
        self.native
            .as_ref()
            .expect("native custody installed before startup")
    }

    /// Installs preservation and the original token BEFORE startup can spawn or
    /// fail post-spawn hooks. Actor construction is caught in the same scope.
    pub(super) fn build(
        &mut self,
        mut service: RuntimeSessionService,
    ) -> Result<
        (
            crate::runtime::PaneProcessStart,
            AsyncRuntimeSessionHandle,
            AsyncRuntimeSessionActor,
        ),
        Failure,
    > {
        let native = service.native_pane_retirement_owner();
        self.native = Some(ManuallyDrop::new(native));
        self.attempt = Some(
            self.owner()
                .capture_cleanup_attempt(self.deadline.into_std())
                .map_err(|error| Failure::Error(error.into()))?,
        );
        if tokio::time::Instant::now() >= self.deadline {
            return Err(Failure::Error(MezError::invalid_state(
                "fixture startup original deadline exhausted",
            )));
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let started = service.start_initial_pane_process_with_launch_context(
                None,
                Some(&self.root),
                Some(&self.environment),
            )?;
            let (handle, actor) = AsyncRuntimeActorFixture::from_service(service).build()?;
            Ok((started, handle, actor))
        })) {
            Ok(result) => result.map_err(Failure::Error),
            Err(payload) => Err(Failure::Panic(payload)),
        }
    }

    /// Polls original futures and one failure-triggered close concurrently.
    /// Stored receipts survive cancellation at the unchanged absolute deadline.
    pub(super) async fn join<C, W>(
        &self,
        client: impl Future<Output = Result<C, MezError>>,
        worker: impl Future<Output = Result<W, MezError>>,
        actor: AsyncRuntimeSessionActor,
        handle: &AsyncRuntimeSessionHandle,
        control: JoinControl,
    ) -> Joined<C, W> {
        let JoinControl {
            stop,
            close: gate,
            supervisor_done,
        } = control;
        join_originals_notified(
            client,
            worker,
            async { Ok(actor.run().await) },
            self.deadline,
            stop,
            || async {
                self.owner().request_shutdown(true);
                if gate.0.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    gate.close(handle).await.map(|_| ())
                }
            },
            supervisor_done,
        )
        .await
    }

    /// Independently releases completed service handles and drains the captured
    /// native token. Never calls synchronous termination or creates a new clock.
    pub(super) async fn retire<C, W>(&mut self, joined: &mut Joined<C, W>) {
        if self.retired {
            return;
        }
        self.retired = true;
        if let Some(exit) = joined.actor.value.as_mut() {
            if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                exit.service.release_native_panes_for_retirement();
            })) {
                joined.extra.push((Stage::Actor, Failure::Panic(payload)));
                joined
                    .cleanup
                    .push("manager handle release panicked".into());
            }
        }
        self.owner().seal();
        if tokio::time::Instant::now() < self.deadline {
            let attempt = self
                .attempt
                .as_ref()
                .expect("original native token installed");
            match std::panic::AssertUnwindSafe(
                crate::host::async_runtime::drain_native_pane_retirement_for_attempt(
                    self.owner(),
                    attempt,
                    self.deadline,
                ),
            )
            .catch_unwind()
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    joined.cleanup.push(format!("native retirement: {error}"));
                    joined.extra.push((Stage::Actor, Failure::Error(error)));
                }
                Err(payload) => {
                    joined.cleanup.push("native retirement panicked".into());
                    joined.extra.push((Stage::Actor, Failure::Panic(payload)));
                }
            }
        } else {
            joined
                .cleanup
                .push("native drain skipped: original deadline exhausted".into());
        }
        if self.owner().outstanding() != 0 || self.owner().has_cleanup_failure() {
            joined.cleanup.push(format!(
                "native settlement unconfirmed: pending={} sticky={}",
                self.owner().outstanding(),
                self.owner().has_cleanup_failure()
            ));
        }
        if tokio::time::Instant::now() >= self.deadline {
            joined.timed_out = true;
        }
        self.settled = joined.client.completed
            && joined.supervisor.completed
            && joined.actor.completed
            && joined.cleanup.is_empty()
            && !joined.timed_out;
        self.delete_storage = self.settled
            && joined.client.failure.is_none()
            && joined.supervisor.failure.is_none()
            && joined.actor.failure.is_none()
            && joined.extra.is_empty()
            && joined.client.value.is_some()
            && joined.supervisor.value.is_some()
            && joined.actor.value.is_some();
    }

    /// Settles post-spawn setup failure before publishing its original cause.
    pub(super) async fn fail_setup(&mut self, failure: Failure) -> ! {
        self.retired = true;
        self.owner().seal();
        let failures = compound_setup_failure(failure, async {
            if tokio::time::Instant::now() >= self.deadline {
                return Err(MezError::invalid_state(
                    "setup cleanup skipped: original deadline exhausted",
                ));
            }
            let attempt = self
                .attempt
                .as_ref()
                .ok_or_else(|| MezError::invalid_state("setup cleanup token unavailable"))?;
            crate::host::async_runtime::drain_native_pane_retirement_for_attempt(
                self.owner(),
                attempt,
                self.deadline,
            )
            .await
        })
        .await;
        let cleanup = failures
            .iter()
            .skip(1)
            .map(|(_, failure)| format!("setup cleanup: {failure:?}"))
            .collect();
        std::panic::resume_unwind(Box::new(FixtureFailure {
            failures,
            cleanup,
            pending: self.owner().outstanding(),
            root: self.root.clone(),
        }));
    }
}

impl Drop for NativeFixture {
    /// Unknown custody/storage is deliberately retained, with no external I/O or
    /// late native cleanup. Success may release the already settled owner only.
    fn drop(&mut self) {
        if self.settled {
            self.attempt.take();
            if let Some(native) = self.native.take() {
                drop(ManuallyDrop::into_inner(native));
            }
        }
        if self.delete_storage {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

#[cfg(test)]
mod tests;
