//! Native request custody independent of the asynchronously cancelled monitor.
//!
//! A per-request async mutex owns the actual blocking JoinHandle. The monitor
//! borrows it while awaiting; dropping that borrow cannot detach the worker.
//! Observed completion transfers the original outcome into the same owner until
//! authoritative adoption. Retirement cancels queued/cooperative work, observes
//! its actual join, and consumes one result. No command or effect is replayed.

use super::{RuntimeNativeShellDispatch, RuntimeNativeShellOutcome, native_worker_join_failure};
use crate::error::{MezError, Result};
use crate::runtime::{NativeActionCancellation, RuntimeNativeShellFailure};
use std::sync::Arc;
use tokio::sync::{Mutex, watch};

/// Original completion identity retained before the monitor's first poll.
struct NativeIdentity {
    /// Actual issued turn, action and marker; never reconstructed generations.
    turn_id: String,
    /// Actual action within the admitted turn.
    action_id: String,
    /// Exact transaction incarnation used by actor completion fencing.
    marker: String,
    /// Original audit command; never formatted into cleanup diagnostics.
    command: String,
    /// Original dispatch timestamp, not retirement wall-clock time.
    started_at_unix_ms: u64,
    /// Original issued phase; a probe failure cannot impersonate a workload.
    capability_probe_only: bool,
}

/// One observed producer completion, kept outside the monitor until adoption.
enum NativeCompletion {
    /// The original executor result, including output and lifecycle evidence.
    Outcome(Box<RuntimeNativeShellOutcome>),
    /// A bounded observed join/retirement cause without invented native facts.
    Lost(&'static str),
}

/// Exclusive native producer custody; a consumed slot rejects duplicate takes.
enum NativeState {
    /// The admitted monitor has not started its blocking request.
    Unstarted,
    /// The actual handle remains here even while a monitor awaits it.
    Running(tokio::task::JoinHandle<RuntimeNativeShellOutcome>),
    /// Joined outcome; None means its sole adoption owner already consumed it.
    Completed(Option<NativeCompletion>),
}

/// Root-service native ownership, independent of the monitor future's lifetime.
#[derive(Clone)]
pub(super) struct NativeWorkerOwner {
    /// Small immutable identity shared with the registered service owner.
    identity: Arc<NativeIdentity>,
    /// Signals the exact queued or executing native dispatch only.
    cancellation: NativeActionCancellation,
    /// Per-request custody; no actor or global mutex is held while joining.
    state: Arc<Mutex<NativeState>>,
}

impl std::fmt::Debug for NativeWorkerOwner {
    /// Keeps commands and native output out of accidental owner diagnostics.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeWorkerOwner")
            .finish_non_exhaustive()
    }
}

impl NativeWorkerOwner {
    /// Captures genuine admitted dispatch identity before spawning its monitor.
    pub(super) fn new(dispatch: &RuntimeNativeShellDispatch) -> Self {
        Self {
            identity: Arc::new(NativeIdentity {
                turn_id: dispatch.turn_id.clone(),
                action_id: dispatch.action_id.clone(),
                marker: dispatch.marker.clone(),
                command: dispatch.request.transaction.command.clone(),
                started_at_unix_ms: dispatch.started_at_unix_ms,
                capability_probe_only: dispatch.capability_probe_only,
            }),
            cancellation: dispatch.cancellation.clone(),
            state: Arc::new(Mutex::new(NativeState::Unstarted)),
        }
    }

    /// Starts request-local execution once, retaining its actual handle before
    /// yielding. Cancellation while awaiting this lock leaves Unstarted custody.
    pub(super) async fn start<Executor>(
        &self,
        dispatch: RuntimeNativeShellDispatch,
        progress: watch::Sender<Option<crate::runtime::RuntimeNativeShellWorkerProgress>>,
        execute: Executor,
    ) -> Result<()>
    where
        Executor: FnOnce(
                RuntimeNativeShellDispatch,
                watch::Sender<Option<crate::runtime::RuntimeNativeShellWorkerProgress>>,
            ) -> RuntimeNativeShellOutcome
            + Send
            + 'static,
    {
        let mut state = self.state.lock().await;
        if !matches!(*state, NativeState::Unstarted) {
            return Err(MezError::invalid_state(
                "native worker already has producer custody",
            ));
        }
        *state = NativeState::Running(tokio::task::spawn_blocking(move || {
            execute(dispatch, progress)
        }));
        Ok(())
    }

    /// Borrows the handle across its observed join, then transfers the original
    /// result into shared custody without another await. Monitor cancellation
    /// drops only this mutex borrow, leaving the handle available to retirement.
    pub(super) async fn join(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        observe_native_join(&mut state).await
    }

    /// Takes one joined result after final progress handling. No normal outcome
    /// is cloned, overwritten by cancellation or made available for replay.
    pub(super) async fn take_completed(&self) -> Result<RuntimeNativeShellOutcome> {
        let mut state = self.state.lock().await;
        self.take_locked(&mut state)
    }

    /// Signals cancellation, observes actual blocking termination and adopts
    /// its original result. Tokio abort only cancels queued blocking work; a
    /// started executor must return through its cooperative native cancellation.
    pub(super) async fn retire(&self, cause: &'static str) -> Result<RuntimeNativeShellOutcome> {
        self.cancellation.cancel();
        let mut state = self.state.lock().await;
        match &*state {
            NativeState::Unstarted => {
                *state = NativeState::Completed(Some(NativeCompletion::Lost(cause)));
            }
            NativeState::Running(worker) => {
                worker.abort();
                observe_native_join(&mut state).await?;
            }
            NativeState::Completed(_) => {}
        }
        self.take_locked(&mut state)
    }

    /// Consumes an observed result exactly once; missing custody remains fatal.
    fn take_locked(&self, state: &mut NativeState) -> Result<RuntimeNativeShellOutcome> {
        let NativeState::Completed(completion) = state else {
            return Err(MezError::invalid_state("native worker has not been joined"));
        };
        match completion.take().ok_or_else(|| {
            MezError::invalid_state("native worker completion has already been consumed")
        })? {
            NativeCompletion::Outcome(outcome) => Ok(*outcome),
            NativeCompletion::Lost(message) => Ok(RuntimeNativeShellOutcome {
                turn_id: self.identity.turn_id.clone(),
                action_id: self.identity.action_id.clone(),
                marker: self.identity.marker.clone(),
                command: self.identity.command.clone(),
                started_at_unix_ms: self.identity.started_at_unix_ms,
                sandbox_capability: None,
                capability_probe_only: self.identity.capability_probe_only,
                launch_counts: None,
                result: Err(RuntimeNativeShellFailure {
                    kind: "invalid_state".into(),
                    message: message.into(),
                    lifecycle: None,
                }),
            }),
        }
    }
}

/// Observes a retained handle without moving it out before awaiting. Even if
/// the awaiting future is cancelled, shared custody still contains that handle.
/// Join failures retain only their bounded category, not arbitrary panic text.
async fn observe_native_join(state: &mut NativeState) -> Result<()> {
    let NativeState::Running(worker) = state else {
        return Err(MezError::invalid_state(
            "native worker has no running join owner",
        ));
    };
    let completion = match worker.await {
        Ok(outcome) => NativeCompletion::Outcome(Box::new(outcome)),
        Err(error) => NativeCompletion::Lost(native_worker_join_failure(&error)),
    };
    *state = NativeState::Completed(Some(completion));
    Ok(())
}
