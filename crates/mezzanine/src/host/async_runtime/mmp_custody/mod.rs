//! Owned MMP storage attempts independent of provider and socket observers.
//!
//! One root serializes one private session store. It opens and writes only on
//! blocking workers, retaining actual handles and the original plan before any
//! observer can await them. Cancellation of an observer never takes a handle or
//! abandons a linear input. Only an observed actual join permits recovery after
//! input loss; ordinary uncertainty uses the retained input and a fresh nonce.
//! Positive completion stays retained until exact lower/request adoption.
//!
//! Stop closes new sends but preserves accepted-input ACK drains, reconciliation and
//! publication. Backoff is monotonic1,2,4..30 seconds after each observed failure,
//! with one worker per root and no provider permit. The owning actor/supervisor
//! must keep this root until settlement. The raw/ACK actor adapter retains its
//! original root/reply through graceful shutdown; native/bridge sends and control
//! drains, supervisory abort, restart dedup and total quotas remain unfinished.

use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::task::Poll;
use std::time::Duration;

use mez_agent::messaging::{
    Delivery, MessageAcceptanceAttempt, MessageAcceptanceJoin, MessageAcceptanceTicket,
    MessageAcceptanceWrite, MessageAcceptanceWriteOutcome, MessageError, MessageRequests,
    MessageService, MessageStorageScope,
};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::error::{MezError, Result};
use crate::storage::mmp_custody::acceptance::AcceptancePlan;
use crate::storage::mmp_custody::ownership::RootOwnership;
use crate::storage::mmp_custody::{CustodyStoreLimits, MmpCustodyStore};

mod acknowledgement;

/// Retained source chosen by trusted session setup, never by an envelope.
#[derive(Clone)]
struct Destination {
    /// Private session-scoped store directory.
    root: PathBuf,
    /// Trusted original storage owner label.
    owner: String,
    /// Construction-time ceilings; tests may narrow them.
    limits: CustodyStoreLimits,
}

/// Observational state; none of these labels is an acceptance or join proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CustodyWorkerStatus {
    /// An owned actual opening/write worker has not yet been joined.
    Running,
    /// Store is ready for one fresh lower-issued occurrence.
    Idle,
    /// Exact joined positive/refusal awaits canonical actor adoption.
    AwaitingAdoption,
    /// Exact joined qualified noncommit awaits paired lower/request release.
    AwaitingRefusal,
    /// Actual joined ACK commit awaits its original lower cursor publication.
    AcknowledgementAwaitingAdoption,
    /// Original uncertain/lost ACK can retry after this monotonic deadline.
    AcknowledgementRetryAt(Instant),
    /// Original uncertain/lost attempt can retry after this monotonic deadline.
    RetryAt(Instant),
    /// Opening failed; no lower write was issued by this root.
    Unavailable,
}

/// One private store connection and its off-actor-observed settled position.
struct Opened {
    /// Only the root or its one actual worker can own this connection.
    store: MmpCustodyStore,
    /// A fresh plan may use this position only while the root is idle.
    anchor: AcceptancePlan,
}

/// Completed physical write, retained independently of any original waiter.
struct Written {
    /// Joined connection, absent only after actual worker failure.
    store: MmpCustodyStore,
    /// Opaque committed/refused/uncertain original, never replaced by status text.
    outcome: MessageAcceptanceWriteOutcome,
    /// Positive settlement can reuse the actual off-actor-read next position.
    anchor: Result<AcceptancePlan>,
}

/// Immutable original admission plus bounded retry scheduling metadata.
struct Admission {
    /// Original destination/sequence/generation plan, never replanned on failure.
    plan: AcceptancePlan,
    /// Exact lower attempt whose actual worker handle this root owns.
    attempt: MessageAcceptanceAttempt,
    /// Tracked originals must settle their paired request owner too.
    tracked: bool,
    /// Once requested, failed-stage recovery survives every later lost result.
    recovery: bool,
    /// Completed failures, saturating without creating catch-up retries.
    failures: u32,
}

/// State owns every actual handle and never moves one out across an await.
enum State {
    /// Off-actor private opening already registered in root custody.
    Opening(JoinHandle<Result<Opened>>),
    /// Exactly one connection and settled plan available for new admission.
    Idle(Opened),
    /// Original plan/attempt retained before physical execution can be observed.
    Running {
        admission: Admission,
        worker: JoinHandle<Result<Written>>,
    },
    /// Actual joined disposition retained until actor publication or exact retry.
    Joined {
        admission: Admission,
        result: Result<Box<Written>>,
        retry_at: Instant,
    },
    /// Actual ACK worker shares the same exclusive connection/position lane.
    Acknowledging {
        admission: acknowledgement::Admission,
        worker: JoinHandle<Result<acknowledgement::Written>>,
    },
    /// Retained exact ACK disposition after its actual handle was joined.
    Acknowledged {
        admission: acknowledgement::Admission,
        result: Result<Box<acknowledgement::Written>>,
        retry_at: Instant,
    },
    /// Bounded failed-open diagnostic; cannot imply any issued write was unsent.
    Unavailable(MezError),
}

/// Root control and handle ownership shared with abortable observation tasks.
struct Inner {
    /// Short synchronous state fence, never held across I/O or an await.
    state: Mutex<State>,
    /// One observer polls the retained join at a time; cancellation passes the
    /// observation slot without replacing another waiter's registered waker.
    observer: tokio::sync::Mutex<()>,
    /// Trusted immutable store destination for actual lost-worker reopening.
    destination: Destination,
    /// The opening worker installs the actual fence before touching SQLite.
    /// Workers and fallback retirement retain this cell beyond root drop.
    ownership: Arc<Mutex<Option<Arc<RootOwnership>>>>,
    /// Runtime used to install blocking workers and fallback retirement custody.
    runtime: Handle,
    /// Live identity shared only with receipts issued by this exact root.
    storage_scope: MessageStorageScope,
    /// Reduce-only admission closure; issued reconciliation remains permitted.
    stopping: AtomicBool,
    /// Test-only actual blocking gate and postcommit panic injection.
    #[cfg(test)]
    probe: Option<Arc<tests::Probe>>,
    /// ACK-only physical gate/panic probe, independent of acceptance fixtures.
    #[cfg(test)]
    ack_probe: Option<Arc<acknowledgement::tests::Probe>>,
}

/// One session's owned storage lifecycle; observers may clone it safely.
#[derive(Clone)]
pub(crate) struct MmpCustodyRoot(Arc<Inner>);

impl MmpCustodyRoot {
    /// Borrows original live root authority for qualified duplicate lookup.
    /// Equal owner labels or a newly opened root cannot recreate this allocation.
    pub fn storage_scope(&self) -> &MessageStorageScope {
        &self.0.storage_scope
    }

    /// Starts private opening on one owned blocking worker before returning.
    /// Calling setup must use an existing Tokio runtime; no provider slot is used.
    pub fn open(root: PathBuf, owner: String, limits: CustodyStoreLimits, runtime: Handle) -> Self {
        Self::construct(
            Destination {
                root,
                owner,
                limits,
            },
            runtime,
            #[cfg(test)]
            None,
            #[cfg(test)]
            None,
        )
    }

    /// Registers the actual opening handle before observers can access the root.
    fn construct(
        destination: Destination,
        runtime: Handle,
        #[cfg(test)] probe: Option<Arc<tests::Probe>>,
        #[cfg(test)] ack_probe: Option<Arc<acknowledgement::tests::Probe>>,
    ) -> Self {
        let source = destination.clone();
        let ownership = Arc::new(Mutex::new(None));
        let opening_ownership = ownership.clone();
        let worker = runtime.spawn_blocking(move || open_destination(&source, &opening_ownership));
        Self(Arc::new(Inner {
            state: Mutex::new(State::Opening(worker)),
            observer: tokio::sync::Mutex::new(()),
            destination,
            ownership,
            runtime,
            storage_scope: MessageStorageScope::default(),
            stopping: AtomicBool::new(false),
            #[cfg(test)]
            probe,
            #[cfg(test)]
            ack_probe,
        }))
    }

    /// Polls the actual retained handle by mutable borrow, caching its real join.
    /// Dropping this future leaves the original handle/input owned by the root.
    pub async fn observe(&self) -> Result<CustodyWorkerStatus> {
        let _observer = self.0.observer.lock().await;
        poll_fn(|cx| {
            let mut state = match self.0.state.lock() {
                Ok(state) => state,
                Err(_) => return Poll::Ready(Err(ownership_error())),
            };
            if matches!(&*state, State::Acknowledging { .. })
                && acknowledgement::poll_join(&mut state, cx).is_pending()
            {
                return Poll::Pending;
            }
            match &mut *state {
                State::Opening(worker) => match Pin::new(worker).poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(result) => {
                        *state = match result.map_err(|_| worker_error()).and_then(|result| result)
                        {
                            Ok(opened) => State::Idle(opened),
                            Err(error) => State::Unavailable(error),
                        };
                    }
                },
                State::Running { worker, .. } => {
                    let result = match Pin::new(worker).poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(result) => {
                            result.map_err(|_| worker_error()).and_then(|result| result)
                        }
                    };
                    let previous =
                        std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
                    if let State::Running { mut admission, .. } = previous {
                        let failed = !matches!(
                            &result,
                            Ok(Written {
                                outcome: MessageAcceptanceWriteOutcome::Committed(_)
                                    | MessageAcceptanceWriteOutcome::NotCommitted(_),
                                ..
                            })
                        );
                        if failed {
                            admission.failures = admission.failures.saturating_add(1);
                        }
                        let retry_at = Instant::now() + retry_delay(admission.failures);
                        *state = State::Joined {
                            admission,
                            result: result.map(Box::new),
                            retry_at,
                        };
                    }
                }
                _ => (),
            }
            Poll::Ready(Ok(status(&state)))
        })
        .await
    }

    /// Borrows bounded failure context without consuming an original or claiming
    /// noncommit. Opening failures remain distinguishable from issued failures.
    pub fn failure(&self) -> Result<Option<MezError>> {
        let state = self.0.state.lock().map_err(|_| ownership_error())?;
        Ok(match &*state {
            State::Unavailable(error)
            | State::Joined {
                result: Err(error), ..
            } => Some(error.clone()),
            State::Joined {
                result: Ok(written),
                ..
            } => match &written.outcome {
                MessageAcceptanceWriteOutcome::Unknown(unknown) => {
                    Some(lower_error(unknown.error().clone()))
                }
                MessageAcceptanceWriteOutcome::NotCommitted(rejected) => {
                    Some(lower_error(rejected.error().clone()))
                }
                _ => None,
            },
            State::Acknowledged { .. } => acknowledgement::failure(&state),
            _ => None,
        })
    }

    /// Closes new send issuance without aborting or fabricating settlement of a write.
    pub fn request_stop(&self) {
        self.0.stopping.store(true, Ordering::Release);
    }

    /// Issues a lower write only after idle root capacity is already secured.
    /// Busy/stopped/foreign-ticket errors leave lower issuance unchanged. The
    /// caller must have paired tracked-request reservation before this operation.
    pub fn start(
        &self,
        service: &mut MessageService,
        ticket: &MessageAcceptanceTicket,
        requests: Option<&MessageRequests>,
    ) -> Result<()> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        if self.0.stopping.load(Ordering::Acquire) {
            return Err(MezError::invalid_state("MMP custody root stopped"));
        }
        let State::Idle(opened) = &*state else {
            return Err(MezError::conflict(
                "MMP custody worker already owned or unavailable",
            ));
        };
        if ticket.has_reserved_proof() != requests.is_some() {
            return Err(ownership_error());
        }
        if let Some(requests) = requests {
            requests.prepared_origin(ticket).map_err(lower_error)?;
        }
        let tracked = requests.is_some();
        let plan = opened.anchor.for_sequence(ticket.sequence());
        let write = service
            .begin_storage_acceptance_write(ticket, &self.0.storage_scope)
            .map_err(lower_error)?;
        let admission = Admission {
            plan,
            attempt: write.attempt(),
            tracked,
            recovery: false,
            failures: 0,
        };
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let State::Idle(opened) = previous else {
            return Err(ownership_error());
        };
        let worker = self.spawn_write(Some(opened.store), admission.plan.clone(), write, false);
        *state = State::Running { admission, worker };
        Ok(())
    }

    /// Retries a joined original only after its monotonic failure deadline. A
    /// foreign/currently replaced lower owner cannot consume uncertain input.
    /// Stop still allows this settlement; no fresh message identity is allocated.
    pub fn retry(&self, service: &mut MessageService, now: Instant) -> Result<bool> {
        self.retry_with_recovery(service, now, false)
    }

    /// Reconciles a failed original for release before later store control work.
    /// A committed original remains positive; otherwise only exact durable abort
    /// cleanup can qualify noncommit. Recovery intent persists across later loss.
    pub fn recover_failed_stage(&self, service: &mut MessageService, now: Instant) -> Result<bool> {
        self.retry_with_recovery(service, now, true)
    }

    /// Issues one exact joined retry with a monotonic, retain-only recovery mode.
    fn retry_with_recovery(
        &self,
        service: &mut MessageService,
        now: Instant,
        recover: bool,
    ) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Joined {
            admission,
            result,
            retry_at,
        } = &*state
        else {
            return Ok(false);
        };
        if matches!(
            result.as_deref(),
            Ok(Written {
                outcome: MessageAcceptanceWriteOutcome::Committed(_)
                    | MessageAcceptanceWriteOutcome::NotCommitted(_),
                ..
            })
        ) || now < *retry_at
        {
            return Ok(false);
        }
        service
            .validate_acceptance_attempt(&admission.attempt)
            .map_err(lower_error)?;
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let State::Joined {
            mut admission,
            result,
            ..
        } = previous
        else {
            return Err(ownership_error());
        };
        admission.recovery |= recover;
        let (store, recovered) = match result.map(|written| *written) {
            Ok(Written {
                store,
                outcome: MessageAcceptanceWriteOutcome::Unknown(unknown),
                ..
            }) => (Some(store), service.retry_acceptance_write(unknown)),
            Err(_) => {
                let mut joined = ObservedJoin(admission.attempt.clone());
                (
                    None,
                    service.recover_joined_acceptance_write(&admission.attempt, &mut joined),
                )
            }
            _ => return Err(ownership_error()),
        };
        // Validation and lower retry share one exclusive borrow, so failure here
        // is an invariant fault. Even then preserve actual joined escrow instead
        // of converting the original to unsent or losing its recovery identity.
        let write = match recovered {
            Ok(write) => write,
            Err(error) => {
                let error = lower_error(error);
                self.0.runtime.spawn_blocking(move || drop(store));
                let retry_at = Instant::now() + retry_delay(admission.failures);
                *state = State::Joined {
                    admission,
                    result: Err(error.clone()),
                    retry_at,
                };
                return Err(error);
            }
        };
        admission.attempt = write.attempt();
        let worker = self.spawn_write(store, admission.plan.clone(), write, admission.recovery);
        *state = State::Running { admission, worker };
        Ok(true)
    }

    /// Publishes the exact joined committed occurrence through its original lower
    /// owner, including paired request adoption when tracked. Errors retain the
    /// positive fact; successful settlement releases this root for later work.
    pub fn adopt(
        &self,
        service: &mut MessageService,
        requests: Option<&mut MessageRequests>,
    ) -> Result<Delivery> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Joined {
            admission,
            result: Ok(written),
            ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        let MessageAcceptanceWriteOutcome::Committed(commit) = &written.outcome else {
            return Err(ownership_error());
        };
        let delivery = if admission.tracked {
            requests
                .ok_or_else(ownership_error)?
                .publish_acceptance(service, commit)
                .map_err(lower_error)?
                .delivery()
                .clone()
        } else {
            if requests.is_some() {
                return Err(ownership_error());
            }
            service.publish_acceptance(commit).map_err(lower_error)?.0
        };
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Joined {
            result: Ok(written),
            ..
        } = previous
        {
            *state = match written.anchor {
                Ok(anchor) => State::Idle(Opened {
                    store: written.store,
                    anchor,
                }),
                Err(error) => State::Unavailable(error),
            };
        }
        Ok(delivery)
    }

    /// Releases paired ownership only for an actual joined qualified refusal.
    /// This path never accepts generic I/O errors or observed worker failures.
    pub fn release_rejected(
        &self,
        service: &mut MessageService,
        requests: Option<&mut MessageRequests>,
    ) -> Result<()> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Joined {
            admission,
            result: Ok(written),
            ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        let MessageAcceptanceWriteOutcome::NotCommitted(rejected) = &written.outcome else {
            return Err(ownership_error());
        };
        if admission.tracked {
            requests
                .ok_or_else(ownership_error)?
                .release_rejected_acceptance(service, rejected)
                .map_err(lower_error)?;
        } else {
            if requests.is_some() {
                return Err(ownership_error());
            }
            service
                .release_rejected_acceptance(rejected)
                .map_err(lower_error)?;
        }
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Joined {
            result: Ok(written),
            ..
        } = previous
        {
            *state = match written.anchor {
                Ok(anchor) => State::Idle(Opened {
                    store: written.store,
                    anchor,
                }),
                Err(error) => State::Unavailable(error),
            };
        }
        Ok(())
    }

    /// Installs exactly one owned physical attempt, retaining the original plan
    /// outside the closure even when the worker panics or loses its result.
    fn spawn_write(
        &self,
        store: Option<MmpCustodyStore>,
        plan: AcceptancePlan,
        write: MessageAcceptanceWrite,
        recovery: bool,
    ) -> JoinHandle<Result<Written>> {
        let destination = self.0.destination.clone();
        let ownership = self.0.ownership.clone();
        #[cfg(test)]
        let probe = self.0.probe.clone();
        self.0.runtime.spawn_blocking(move || {
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.before_write();
            }
            let fence = ownership
                .lock()
                .map_err(|_| ownership_error())?
                .as_ref()
                .cloned()
                .ok_or_else(ownership_error)?;
            fence.validate_root()?;
            let store = store.filter(|store| plan.matches_store(store));
            let mut store = match store {
                Some(store) => store,
                None => MmpCustodyStore::open_with_limits(
                    &destination.root,
                    &destination.owner,
                    destination.limits,
                )?,
            };
            let outcome = {
                let mut owned = fence.bind_store(&mut store)?;
                if recovery {
                    write.persist(&mut owned.store_mut().acceptance_recovery_port(&plan))
                } else {
                    write.persist(&mut owned.store_mut().acceptance_port(&plan))
                }
            };
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.after_write(&outcome);
            }
            // A changed path after physical I/O cannot publish a result under
            // this root; an observed worker error retains reconciliation duty.
            {
                let _bound = fence.bind_store(&mut store)?;
            }
            let anchor = store.acceptance_plan(0);
            Ok(Written {
                store,
                outcome,
                anchor,
            })
        })
    }
}

/// An actual observed failed join is the only constructor site for this witness.
struct ObservedJoin(MessageAcceptanceAttempt);
impl MessageAcceptanceJoin for ObservedJoin {
    /// Qualifies only the original allocation/nonce whose real handle was joined.
    fn qualify_joined(
        &mut self,
        attempt: &MessageAcceptanceAttempt,
    ) -> mez_agent::messaging::Result<()> {
        if self.0.same_attempt(attempt) {
            Ok(())
        } else {
            Err(MessageError::invalid_state("MMP original join changed"))
        }
    }
}

impl Drop for Inner {
    /// Last-root loss transfers actual handles to a drain task rather than
    /// detaching blocking I/O. It creates no canonical acknowledgement; orderly
    /// supervision must retain the root until its actor settlement completes.
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = std::mem::replace(state, State::Unavailable(ownership_error()));
        let runtime = self.runtime.clone();
        let ownership = self.ownership.clone();
        self.runtime.spawn(async move {
            let _ownership = ownership;
            match state {
                State::Opening(worker) => {
                    let result = worker.await;
                    let _ = runtime.spawn_blocking(move || drop(result)).await;
                }
                State::Running { worker, .. } => {
                    let result = worker.await;
                    let _ = runtime.spawn_blocking(move || drop(result)).await;
                }
                State::Acknowledging { worker, .. } => {
                    let result = worker.await;
                    let _ = runtime.spawn_blocking(move || drop(result)).await;
                }
                state => {
                    let _ = runtime.spawn_blocking(move || drop(state)).await;
                }
            }
        });
    }
}

/// Opens and captures the actual settled position exclusively off the actor.
fn open_destination(
    source: &Destination,
    ownership: &Mutex<Option<Arc<RootOwnership>>>,
) -> Result<Opened> {
    let fence = Arc::new(RootOwnership::acquire(&source.root)?);
    fence.validate_root()?;
    let mut store = MmpCustodyStore::open_with_limits(&source.root, &source.owner, source.limits)?;
    {
        let _bound = fence.bind_store(&mut store)?;
    }
    *ownership.lock().map_err(|_| ownership_error())? = Some(fence);
    let anchor = store.acceptance_plan(0)?;
    Ok(Opened { store, anchor })
}

/// Provides bounded scheduling state without transferring original facts.
fn status(state: &State) -> CustodyWorkerStatus {
    match state {
        State::Opening(_) | State::Running { .. } => CustodyWorkerStatus::Running,
        State::Idle(_) => CustodyWorkerStatus::Idle,
        State::Joined {
            result, retry_at, ..
        } => match result.as_deref() {
            Ok(Written {
                outcome: MessageAcceptanceWriteOutcome::Committed(_),
                ..
            }) => CustodyWorkerStatus::AwaitingAdoption,
            Ok(Written {
                outcome: MessageAcceptanceWriteOutcome::NotCommitted(_),
                ..
            }) => CustodyWorkerStatus::AwaitingRefusal,
            _ => CustodyWorkerStatus::RetryAt(*retry_at),
        },
        State::Acknowledging { .. } => CustodyWorkerStatus::Running,
        State::Acknowledged {
            result, retry_at, ..
        } => match result.as_deref() {
            Ok(acknowledgement::Written {
                outcome: mez_agent::messaging::MessageAcknowledgementWriteOutcome::Committed(_),
                ..
            }) => CustodyWorkerStatus::AcknowledgementAwaitingAdoption,
            _ => CustodyWorkerStatus::AcknowledgementRetryAt(*retry_at),
        },
        State::Unavailable(_) => CustodyWorkerStatus::Unavailable,
    }
}

/// Spacing starts after observed failure and saturates without catch-up bursts.
fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs((1u64 << failures.saturating_sub(1).min(5)).min(30))
}
/// Keeps bounded original-ownership diagnostics free of message bodies and IDs.
fn ownership_error() -> MezError {
    MezError::invalid_state("MMP custody original ownership unavailable")
}
/// Actual worker failure is uncertain, never proof of noncommit.
fn worker_error() -> MezError {
    MezError::invalid_state("MMP custody actual worker join failed")
}
/// Preserves lower failure context without inferring acceptance or nonexecution.
fn lower_error(error: MessageError) -> MezError {
    MezError::invalid_state(format!("MMP custody lower owner: {error}"))
}

#[cfg(test)]
mod tests;
