//! Owned cold-recovery workers and delayed adoption of original custody.
//!
//! A root retains its exact immutable input, first observed store position and
//! actual worker handle through cancellation, panic and monotonic retry. Opening
//! never creates a missing original. Positive cold service and input reservation
//! stay unpublished until actual join/adoption. The dedicated native path retains
//! actual private context evidence and opaque results for exact runtime install;
//! storage-only recovery leaves endpoint registrations unavailable.

use std::sync::OnceLock;
use std::task::Context;

use crate::runtime::PreparedNativeCustodyContext;
use mez_agent::messaging::{
    MessageRecoveredNativeBinding, MessageServiceSnapshot, MessageStorageLimits,
    MessageStorageSnapshotLease,
};

use super::*;

/// Trusted startup input, consumed so only this root retains its checkpoint.
pub(crate) struct CustodyRecoveryRequest {
    /// Optional original schema10 selected by trusted snapshot setup. Absence
    /// requires an existing physical registration and actual native context.
    pub checkpoint: Option<MessageServiceSnapshot>,
    /// Destination-owned limits, never supplied by the saved checkpoint.
    pub limits: MessageStorageLimits,
    /// Destination-owned disposable replay count and byte bounds.
    pub retention_messages: usize,
    pub retention_bytes: usize,
}

/// Joined original-store candidate retained through whole-session startup.
/// Native tuples remain descriptive until actual context continuity qualifies
/// them; this linear owner keeps their input charged without exposing a clone.
pub(crate) struct RecoveredMessageCustody {
    checkpoint: MessageServiceSnapshot,
    service: MessageService,
    native_bindings: Vec<MessageRecoveredNativeBinding>,
    native_context: Option<Arc<PreparedNativeCustodyContext>>,
    input_lease: MessageStorageSnapshotLease,
    native_lease: Option<MessageStorageSnapshotLease>,
    /// Keeps the actual store fence alive through cancellable startup staging.
    /// Last field: input, service and reservation retire before the root owner.
    owner: MmpCustodyRoot,
}
impl RecoveredMessageCustody {
    /// Borrows original saved facts; these bytes are not live native authority.
    pub fn checkpoint(&self) -> &MessageServiceSnapshot {
        &self.checkpoint
    }
    /// Borrows canonical custody for trusted startup qualification only.
    pub fn service_mut(&mut self) -> &mut MessageService {
        &mut self.service
    }
    /// Installs only the exact private runtime contexts qualified by this worker.
    /// Publication compares the private source under its compound checkpoint lock;
    /// the factory still withholds processes/listeners.
    pub fn install_native_context(
        &self,
        runtime: &mut crate::runtime::RuntimeSessionService,
    ) -> Result<()> {
        self.native_context
            .as_ref()
            .ok_or_else(ownership_error)?
            .install(runtime, &self.native_bindings)
    }
    /// Finishes storage staging after the trusted caller's required session
    /// qualification. Empty endpoint maps remain empty unless separately bound.
    /// Input is discarded before its reservation; no bytes revive an epoch.
    /// The caller retains the corresponding root for the runtime it publishes.
    pub fn into_service(self) -> MessageService {
        let Self {
            checkpoint,
            service,
            native_bindings,
            native_context,
            input_lease,
            native_lease,
            owner,
        } = self;
        drop(checkpoint);
        drop(native_bindings);
        drop(native_context);
        drop(input_lease);
        drop(native_lease);
        drop(owner);
        service
    }
}

/// Immutable original retained outside every actual recovery worker.
struct Input {
    checkpoint: OnceLock<Arc<MessageServiceSnapshot>>,
    /// Requires the published original on every retry, never a fresh baseline.
    from_registration: bool,
    native_context: Option<Arc<PreparedNativeCustodyContext>>,
    limits: MessageStorageLimits,
    retention_messages: usize,
    retention_bytes: usize,
    /// Frozen before reconstruction; worker loss never refreshes this anchor.
    position: OnceLock<AcceptancePlan>,
    identity: Arc<()>,
}

/// Same original plus reduce-only retry scheduling, retained after actual loss.
pub(super) struct Admission {
    input: Arc<Input>,
    failures: u32,
}

/// Actual joined cold result; metadata stays charged until input is discarded.
pub(super) struct Written {
    opened: Opened,
    pub(super) service: MessageService,
    input_lease: MessageStorageSnapshotLease,
    native_lease: Option<MessageStorageSnapshotLease>,
    native_bindings: Vec<MessageRecoveredNativeBinding>,
    identity: Arc<()>,
}

impl MmpCustodyRoot {
    /// Starts recovery of an existing original on an already-owned blocking
    /// lane. Input is consumed, never reconstructed from an observer/status.
    /// Failure preserves original custody; no root/session publication occurs.
    pub fn recover(
        root: PathBuf,
        owner: String,
        limits: CustodyStoreLimits,
        runtime: Handle,
        request: super::CustodyRecoveryRequest,
    ) -> Self {
        Self::construct_recovery(
            Destination {
                root,
                owner,
                limits,
            },
            runtime,
            request,
            #[cfg(test)]
            None,
        )
    }

    /// Dedicated whole-session path retaining actual private context input.
    /// Generic snapshot/recovery construction never obtains this qualification.
    pub fn recover_native(
        root: PathBuf,
        owner: String,
        limits: CustodyStoreLimits,
        runtime: Handle,
        request: CustodyRecoveryRequest,
        context: PreparedNativeCustodyContext,
    ) -> Self {
        Self::construct_recovery_with_native(
            Destination {
                root,
                owner,
                limits,
            },
            runtime,
            request,
            Some(Arc::new(context)),
            #[cfg(test)]
            None,
        )
    }

    /// Registers immutable input and the actual worker before exposing observers.
    pub(super) fn construct_recovery(
        destination: Destination,
        runtime: Handle,
        request: CustodyRecoveryRequest,
        #[cfg(test)] probe: Option<Arc<super::tests::Probe>>,
    ) -> Self {
        Self::construct_recovery_with_native(
            destination,
            runtime,
            request,
            None,
            #[cfg(test)]
            probe,
        )
    }

    /// Retains optional private input alongside the immutable original worker input.
    fn construct_recovery_with_native(
        destination: Destination,
        runtime: Handle,
        request: CustodyRecoveryRequest,
        native_context: Option<Arc<PreparedNativeCustodyContext>>,
        #[cfg(test)] probe: Option<Arc<super::tests::Probe>>,
    ) -> Self {
        let from_registration = request.checkpoint.is_none();
        let checkpoint = request
            .checkpoint
            .map(|s| OnceLock::from(Arc::new(s)))
            .unwrap_or_default();
        let input = Arc::new(Input {
            checkpoint,
            from_registration,
            native_context,
            limits: request.limits,
            retention_messages: request.retention_messages,
            retention_bytes: request.retention_bytes,
            position: OnceLock::new(),
            identity: Arc::new(()),
        });
        let ownership = Arc::new(Mutex::new(None));
        let scope = MessageStorageScope::default();
        let worker = spawn(
            &runtime,
            destination.clone(),
            ownership.clone(),
            scope.clone(),
            input.clone(),
            #[cfg(test)]
            probe.clone(),
        );
        Self(Arc::new(Inner {
            state: Mutex::new(State::Recovering {
                admission: Admission { input, failures: 0 },
                worker,
            }),
            observer: tokio::sync::Mutex::new(()),
            destination,
            ownership,
            runtime,
            storage_scope: scope,
            stopping: AtomicBool::new(false),
            #[cfg(test)]
            probe,
            #[cfg(test)]
            ack_probe: None,
        }))
    }

    /// Retries the exact original only after actual join and monotonic backoff.
    /// A positive cold result cannot be replaced or issued for a second read.
    pub fn retry_recovery(&self, now: Instant) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Recovered {
            result, retry_at, ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        if result.is_ok() {
            return Err(MezError::conflict("MMP recovery awaits original adoption"));
        }
        if now < *retry_at {
            return Ok(false);
        }
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Recovered { admission, .. } = previous {
            let worker = spawn(
                &self.0.runtime,
                self.0.destination.clone(),
                self.0.ownership.clone(),
                self.0.storage_scope.clone(),
                admission.input.clone(),
                #[cfg(test)]
                self.0.probe.clone(),
            );
            *state = State::Recovering { admission, worker };
        }
        Ok(true)
    }

    /// Moves the actually joined canonical cold service once. The root remains
    /// its original storage owner, ready for actual startup/native qualification.
    /// The returned staging owner keeps input and its lease together until the
    /// trusted caller finishes session qualification; no session is published here.
    pub fn adopt_recovery(&self) -> Result<super::RecoveredMessageCustody> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Recovered {
            admission,
            result: Ok(written),
            ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        if !Arc::ptr_eq(&admission.input.identity, &written.identity)
            || Arc::strong_count(&admission.input) != 1
            || admission
                .input
                .checkpoint
                .get()
                .is_none_or(|c| Arc::strong_count(c) != 1)
            || admission
                .input
                .native_context
                .as_ref()
                .is_some_and(|c| Arc::strong_count(c) != 1)
        {
            return Err(ownership_error());
        }
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let State::Recovered {
            admission,
            result: Ok(written),
            ..
        } = previous
        else {
            return Err(ownership_error());
        };
        let Written {
            opened,
            service,
            input_lease,
            native_lease,
            native_bindings,
            ..
        } = *written;
        let input = Arc::try_unwrap(admission.input).map_err(|_| ownership_error())?;
        let checkpoint =
            Arc::try_unwrap(input.checkpoint.into_inner().ok_or_else(ownership_error)?)
                .map_err(|_| ownership_error())?;
        *state = State::Idle(opened);
        Ok(RecoveredMessageCustody {
            checkpoint,
            service,
            native_bindings,
            native_context: input.native_context,
            input_lease,
            native_lease,
            owner: self.clone(),
        })
    }
}

/// Runs exclusively on the root's blocking lane. Its ownership cell outlives
/// actual worker loss and final-root retirement until the retained handle joins.
fn spawn(
    runtime: &Handle,
    destination: Destination,
    ownership: Arc<Mutex<Option<Arc<RootOwnership>>>>,
    scope: MessageStorageScope,
    input: Arc<Input>,
    #[cfg(test)] probe: Option<Arc<super::tests::Probe>>,
) -> JoinHandle<Result<Written>> {
    runtime.spawn_blocking(move || {
        if input.from_registration && input.native_context.is_none() {
            return Err(ownership_error());
        }
        let existing = ownership
            .lock()
            .map_err(|_| ownership_error())?
            .as_ref()
            .cloned();
        let fence = match existing {
            Some(fence) => fence,
            None => {
                let fence = Arc::new(RootOwnership::acquire_existing(&destination.root)?);
                *ownership.lock().map_err(|_| ownership_error())? = Some(fence.clone());
                fence
            }
        };
        fence.validate_root()?;
        let mut store = MmpCustodyStore::open_existing(
            &destination.root,
            &destination.owner,
            destination.limits,
        )?;
        let (service, input_lease, native_lease, native_bindings, anchor) = {
            let mut owned = fence.bind_store(&mut store)?;
            let observed = owned.store_mut().acceptance_plan(0)?;
            let anchor = input.position.get_or_init(|| observed.clone());
            if !observed.same_position(anchor) {
                return Err(ownership_error());
            }
            if input.from_registration {
                let observed = owned.registered_recovery_checkpoint(
                    input.retention_messages,
                    input.retention_bytes,
                )?;
                let captured = input.checkpoint.get_or_init(|| Arc::new(observed.clone()));
                if captured
                    .storage_snapshot
                    .as_ref()
                    .and_then(|s| s.binding.as_ref())
                    != observed
                        .storage_snapshot
                        .as_ref()
                        .and_then(|s| s.binding.as_ref())
                {
                    return Err(ownership_error());
                }
            }
            let checkpoint = input.checkpoint.get().ok_or_else(ownership_error)?;
            if checkpoint
                .storage_snapshot
                .as_ref()
                .and_then(|s| s.binding.as_ref())
                .is_none_or(|b| b.owner != destination.owner)
            {
                return Err(ownership_error());
            }
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.before_write();
            }
            let (service, lease, native_lease, native_bindings) =
                if let Some(context) = &input.native_context {
                    let recovered = owned.reconstruct_native_service(
                        checkpoint.clone(),
                        &scope,
                        input.limits,
                        input.retention_messages,
                        input.retention_bytes,
                        context,
                    )?;
                    (
                        recovered.service,
                        recovered.input_lease,
                        recovered.native_lease,
                        recovered.bindings,
                    )
                } else {
                    let (service, lease) = owned.reconstruct_cold_service(
                        checkpoint.clone(),
                        &scope,
                        input.limits,
                        input.retention_messages,
                        input.retention_bytes,
                    )?;
                    (service, lease, None, Vec::new())
                };
            owned.revalidate()?;
            if !owned.store_mut().acceptance_plan(0)?.same_position(anchor) {
                return Err(ownership_error());
            }
            (
                service,
                lease,
                native_lease,
                native_bindings,
                anchor.clone(),
            )
        };
        #[cfg(test)]
        if let Some(probe) = &probe {
            probe.after_payload_read();
        }
        {
            let _bound = fence.bind_store(&mut store)?;
        }
        if !store.acceptance_plan(0)?.same_position(&anchor) {
            return Err(ownership_error());
        }
        Ok(Written {
            opened: Opened { store, anchor },
            service,
            input_lease,
            native_lease,
            native_bindings,
            identity: input.identity.clone(),
        })
    })
}

/// Polls only the retained actual handle. Cancellation never takes its input.
pub(super) fn poll_join(state: &mut State, cx: &mut Context<'_>) -> Poll<()> {
    let State::Recovering { worker, .. } = state else {
        return Poll::Ready(());
    };
    let result = match Pin::new(worker).poll(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(result) => result.map_err(|_| worker_error()).and_then(|r| r),
    };
    let previous = std::mem::replace(state, State::Unavailable(ownership_error()));
    if let State::Recovering { mut admission, .. } = previous {
        if result.is_err() {
            admission.failures = admission.failures.saturating_add(1);
        }
        let retry_at = Instant::now() + retry_delay(admission.failures);
        *state = State::Recovered {
            admission,
            result: result.map(Box::new),
            retry_at,
        };
    }
    Poll::Ready(())
}
