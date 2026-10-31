//! Process-free blocking execution with bounded actor authority requests.
//!
//! The root service must retain a producer before polling claim and keep it
//! through actual join and original canonical/durable settlement. This bridge
//! supplies current preparation scopes and short endpoint leases over the actor
//! channel; it never runs actor state inside a blocking worker or renews the
//! original action budget. Dropped RPC replies cannot lose claimed dispatch or
//! authorize replay. Raw outcome custody acknowledgement is not durable transfer.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use mez_agent::native_action::{
    NativeCommitIdentity, NativeEffectCertainty, NativePatchOutcome, NativePatchPhase,
};
use mez_agent::permissions::PathScopes;

use super::AsyncRuntimeSessionHandle;
use crate::error::{MezError, Result};
use crate::runtime::native_filesystem::NativeFilesystemProducer;
use crate::runtime::native_filesystem::NativePatchCommitFence;
use crate::runtime::native_filesystem::{NativeFilesystemAttempt, RuntimeNativeFilesystemDispatch};
use crate::runtime::native_filesystem::{
    NativePatchAuthority, NativePatchWorkerOutcome, NativePatchWorkerRequest, execute_native_patch,
};
use crate::runtime::native_filesystem::{NativePatchCommitLease, NativePatchCommitProposal};

/// Claims and starts using an owner the caller already retained in its root
/// registry. Cancelling this future requires closing/retiring that same owner;
/// no reply is the sole custodian of a dispatch or producer outcome.
pub(crate) async fn claim_and_start_native_filesystem_producer(
    handle: AsyncRuntimeSessionHandle,
    producer: &NativeFilesystemProducer,
) -> Result<bool> {
    if !handle
        .claim_native_filesystem_producer(producer.clone())
        .await?
    {
        return Ok(false);
    }
    start_native_filesystem_producer(handle, producer).await
}

/// Observes the actual join and retains the original raw Arc in actor custody.
/// The caller must keep its producer until canonical/durable transfer succeeds;
/// this raw acknowledgement never frees either original settlement owner.
pub(crate) async fn retain_joined_native_filesystem_outcome(
    handle: &AsyncRuntimeSessionHandle,
    producer: &NativeFilesystemProducer,
) -> Result<Option<Arc<NativePatchWorkerOutcome>>> {
    let Some(outcome) = producer.observe().await? else {
        return Ok(None);
    };
    handle
        .retain_native_filesystem_outcome(producer.attempt().clone(), Arc::clone(&outcome))
        .await?;
    Ok(Some(outcome))
}

/// Observes original joined facts and projects their canonical/context result.
/// Root custody must remain retained: audit, publication and archive ownership
/// are separate required gates and this helper issues no settlement receipt.
pub(crate) async fn project_joined_native_filesystem_facts(
    handle: &AsyncRuntimeSessionHandle,
    producer: &NativeFilesystemProducer,
) -> Result<Option<Arc<NativePatchWorkerOutcome>>> {
    let Some(outcome) = retain_joined_native_filesystem_outcome(handle, producer).await? else {
        return Ok(None);
    };
    handle
        .project_native_filesystem_completion(producer.attempt().clone())
        .await?;
    Ok(Some(outcome))
}

/// Starts a claimed producer once, retaining its actual blocking handle first.
/// No subprocess, shell adapter, environment discovery or PTY input is used.
pub(crate) async fn start_native_filesystem_producer(
    handle: AsyncRuntimeSessionHandle,
    producer: &NativeFilesystemProducer,
) -> Result<bool> {
    let runtime = tokio::runtime::Handle::current();
    producer
        .start(move |dispatch| execute(dispatch, handle, runtime))
        .await
}

/// Executes actual descriptor work with the original admitted action budget.
fn execute(
    dispatch: RuntimeNativeFilesystemDispatch,
    handle: AsyncRuntimeSessionHandle,
    runtime: tokio::runtime::Handle,
) -> NativePatchWorkerOutcome {
    let identity = Arc::new(dispatch.work.identity.clone());
    let root = match dispatch
        .fence
        .check()
        .and_then(|()| dispatch.work.source.capture_root())
    {
        Ok(root) => root,
        Err(error) => {
            return NativePatchWorkerOutcome {
                result: NativePatchOutcome {
                    identity: (*identity).clone(),
                    phase: NativePatchPhase::Snapshot,
                    certainty: NativeEffectCertainty::Unexecuted,
                    confirmed_effects: Vec::new(),
                    diagnostic: Some(error.message().chars().take(8192).collect()),
                },
                receipts: Vec::new(),
                completed_operations: Vec::new(),
            };
        }
    };
    let request = NativePatchWorkerRequest {
        identity: (*identity).clone(),
        root,
        scopes: dispatch.scopes,
        fence: dispatch.fence.clone(),
    };
    let authority = ActorAuthority {
        handle,
        runtime,
        attempt: dispatch.attempt,
        identity,
        fence: dispatch.fence,
    };
    execute_native_patch(request, authority, |_| {})
}

/// Blocking-side authority client; concrete mutations still need actor leases.
struct ActorAuthority {
    /// Original serialized actor destination retained by the request owner.
    handle: AsyncRuntimeSessionHandle,
    /// Owning runtime used only from its blocking task context.
    runtime: tokio::runtime::Handle,
    /// Exact admitted attempt, never reconstructed labels.
    attempt: NativeFilesystemAttempt,
    /// Immutable full semantic identity shared across actor requests.
    identity: Arc<NativeCommitIdentity>,
    /// Exact finite action deadline and cooperative cancellation owner.
    fence: NativePatchCommitFence,
}

impl ActorAuthority {
    /// Waits within the remaining original budget and polls cancellation while
    /// queued. Dropping the RPC releases any undelivered lease without replay.
    fn request<T>(
        &self,
        identity: &NativeCommitIdentity,
        request: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        if identity != self.identity.as_ref() {
            return Err(MezError::conflict(
                "native filesystem RPC semantic identity changed",
            ));
        }
        let remaining = self.fence.remaining()?;
        self.runtime.block_on(async {
            tokio::pin!(request);
            let deadline = tokio::time::sleep(remaining);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    result = &mut request => {
                        self.fence.check()?;
                        return result;
                    }
                    _ = &mut deadline => return Err(MezError::conflict("native filesystem actor request deadline elapsed")),
                    _ = tokio::time::sleep(Duration::from_millis(10)) => self.fence.check()?,
                }
            }
        })
    }
}

impl NativePatchAuthority for ActorAuthority {
    /// Obtains bounded typed preparation authority for original touched paths.
    fn scopes(
        &mut self,
        identity: &NativeCommitIdentity,
        phase: NativePatchPhase,
        path: &str,
    ) -> Result<PathScopes> {
        if path.len() > 4096 {
            return Err(MezError::invalid_args(
                "native filesystem RPC path budget exceeded",
            ));
        }
        self.request(
            identity,
            self.handle.native_filesystem_scopes(
                self.attempt.clone(),
                Arc::clone(&self.identity),
                phase,
                path.to_owned(),
            ),
        )
    }

    /// Obtains one exact actor-issued endpoint lease, never a scope-only grant.
    fn commit(
        &mut self,
        identity: &NativeCommitIdentity,
        proposal: &NativePatchCommitProposal,
    ) -> Result<NativePatchCommitLease> {
        self.request(
            identity,
            self.handle.native_filesystem_commit(
                self.attempt.clone(),
                Arc::clone(&self.identity),
                Arc::new(proposal.clone()),
            ),
        )
    }
}
