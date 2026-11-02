//! Actor ownership of original MMP storage completion and fanout ACK replies.
//!
//! A registered root is observed directly by the actor, independently of socket
//! reply receivers and provider slots. Actual joins precede lower publication;
//! monotonic retry never replays socket input. One outstanding ACK owns bounded
//! metadata and its original reply. Graceful shutdown stops fresh acceptance but
//! retains the actor/service/root until that ACK is canonically settled.
//!
//! Registration is deliberately internal and is not enabled by normal product
//! construction yet: native/raw/bridge acceptance, native input dedup, restore,
//! durable retirement and total quotas must all be integrated before advertising
//! durable mode. This owner handles graceful shutdown, not forced actor abort or
//! runtime destruction. Other root jobs must acquire their own actor continuations
//! before registration can be enabled for general ingress.

use std::future::{Future, pending};

use futures_util::FutureExt;
use mez_agent::messaging::{DeliveryCursor, FanoutBatch};
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::AsyncRuntimeSessionActor;
use crate::host::async_runtime::mmp_custody::{CustodyWorkerStatus, MmpCustodyRoot};
use crate::runtime::RuntimeLifecycleState;
use crate::{MezError, Result};

/// Maximum bounded lower ACK metadata, including its future worker encoding.
const ACK_METADATA_BYTES: usize = 512 * 1024;

/// Next physical observation or original retry; neither is acceptance evidence.
#[derive(Debug, Clone, Copy)]
enum Wake {
    /// Poll the root's original retained actual handle.
    Observe,
    /// Wait until the original monotonic retry is eligible.
    Retry(Instant),
}

/// Content-free callback from the root owned by this actor allocation.
pub(super) enum CustodyEvent {
    /// Actual joined disposition, still requiring canonical lower adoption.
    Observed(Result<CustodyWorkerStatus>),
    /// Original ACK's backoff expired; no new admission may replace it.
    Retry,
}

/// One registered private store with the original pending observer reply.
pub(in crate::host::async_runtime) struct ActorMessageCustody {
    /// Root owns actual blocking handles and immutable original storage plans.
    root: MmpCustodyRoot,
    /// None disables polling an idle/failed root and prevents a busy loop.
    wake: Option<Wake>,
    /// Receiver cancellation cannot remove this issued original continuation.
    acknowledgement: Option<oneshot::Sender<Result<DeliveryCursor>>>,
    /// Shutdown is completed only after actual join and canonical adoption.
    shutdown: Option<oneshot::Sender<Result<RuntimeLifecycleState>>>,
    /// Bounded diagnostic; never a qualification of noncommit or unsent input.
    failure: Option<MezError>,
}

impl std::fmt::Debug for ActorMessageCustody {
    /// Exposes ownership labels without payload, private paths or reply data.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActorMessageCustody")
            .field("wake", &self.wake)
            .field("acknowledgement", &self.acknowledgement.is_some())
            .field("shutdown", &self.shutdown.is_some())
            .field("failed", &self.failure.is_some())
            .finish()
    }
}

impl AsyncRuntimeSessionActor {
    /// Transfers exclusive scheduling of one root before its first actor claim.
    /// Rejects replacing an existing root, preserving its actual handles/replies.
    /// Trusted setup must not schedule external jobs on a clone after transfer.
    #[allow(
        dead_code,
        reason = "durable ingress activation continues under e048f9a8"
    )]
    pub(in crate::host::async_runtime) fn register_message_custody(
        &mut self,
        root: MmpCustodyRoot,
    ) -> Result<()> {
        if self.message_custody.is_some() {
            return Err(MezError::conflict("MMP custody root already registered"));
        }
        self.message_custody = Some(Box::new(ActorMessageCustody {
            root,
            wake: Some(Wake::Observe),
            acknowledgement: None,
            shutdown: None,
            failure: None,
        }));
        Ok(())
    }

    /// True after graceful shutdown took ownership of its original reply.
    pub(super) fn message_custody_shutdown_pending(&self) -> bool {
        self.message_custody
            .as_ref()
            .is_some_and(|owner| owner.shutdown.is_some())
    }

    /// Builds an abortable observation without borrowing the actor across await.
    /// An idle/absent root is pending forever until another request issues work.
    pub(super) fn message_custody_wakeup(&self) -> impl Future<Output = CustodyEvent> + use<> {
        let next = self
            .message_custody
            .as_ref()
            .and_then(|owner| owner.wake.map(|wake| (owner.root.clone(), wake)));
        async move {
            match next {
                Some((root, Wake::Observe)) => CustodyEvent::Observed(root.observe().await),
                Some((_, Wake::Retry(at))) => {
                    tokio::time::sleep_until(at).await;
                    CustodyEvent::Retry
                }
                None => pending().await,
            }
        }
    }

    /// Gives storage completion a bounded turn even under continuously busy ingress.
    /// Returns true only when an owned shutdown can safely return the service.
    pub(super) fn poll_message_custody(&mut self) -> bool {
        if let Some(event) = self.message_custody_wakeup().now_or_never() {
            self.apply_message_custody_event(event);
        }
        let ready = self.message_custody.as_ref().is_some_and(|owner| {
            owner.shutdown.is_some() && owner.acknowledgement.is_none() && owner.wake.is_none()
        });
        if ready {
            let reply = self
                .message_custody
                .as_mut()
                .and_then(|owner| owner.shutdown.take());
            if let Some(reply) = reply {
                let result = self.finish_message_shutdown();
                let _ = reply.send(result);
            }
        }
        ready
    }

    /// Settles only this registered root's actual result through its original
    /// lower owner. Errors retain pending ACK custody and schedule another bounded
    /// observation; they never publish success or free charged canonical input.
    pub(super) fn apply_message_custody_event(&mut self, event: CustodyEvent) {
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        let result = match event {
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::Idle)) => {
                owner.wake = None;
                Ok(())
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::Unavailable)) => {
                owner.failure =
                    owner.root.failure().ok().flatten().or_else(|| {
                        Some(MezError::invalid_state("MMP custody opening unavailable"))
                    });
                owner.wake = None;
                Ok(())
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::AcknowledgementAwaitingAdoption)) => {
                owner
                    .root
                    .adopt_acknowledgement(self.service.message_service_mut())
                    .map(|cursor| {
                        if let Some(reply) = owner.acknowledgement.take() {
                            let _ = reply.send(Ok(cursor));
                        }
                        owner.failure = None;
                        owner.wake = Some(Wake::Observe);
                    })
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::AcknowledgementRetryAt(at))) => {
                owner.wake = Some(Wake::Retry(at));
                Ok(())
            }
            CustodyEvent::Retry => owner
                .root
                .retry_acknowledgement(self.service.message_service_mut(), Instant::now())
                .map(|_| owner.wake = Some(Wake::Observe)),
            CustodyEvent::Observed(Err(error)) => Err(error),
            // Acceptance continuations are not registered by this ACK adapter.
            // Fail closed if trusted setup violates exclusive scheduling.
            CustodyEvent::Observed(Ok(_)) => Err(MezError::invalid_state(
                "MMP custody result has no actor continuation",
            )),
        };
        if let Err(error) = result {
            owner.failure = Some(error);
            owner.wake = Some(Wake::Retry(
                Instant::now() + std::time::Duration::from_secs(1),
            ));
        }
    }

    /// After the transport confirms its batch write, reserve the original cursor
    /// and defer success until the physical ACK and canonical adoption complete.
    /// Busy/opening errors issue nothing; duplicate ACKs stay inert. No batch
    /// payload is retained by the pending reply or copied into an actor queue.
    pub(super) fn acknowledge_message_fanout(
        &mut self,
        batch: FanoutBatch,
        reply: oneshot::Sender<Result<DeliveryCursor>>,
    ) {
        let Some(owner) = self.message_custody.as_mut() else {
            let result = self
                .service
                .message_service_mut()
                .acknowledge_fanout_batch(&batch)
                .map_err(Into::into);
            let _ = reply.send(result);
            return;
        };
        if owner.wake.is_some() || owner.acknowledgement.is_some() || owner.failure.is_some() {
            let _ = reply.send(Err(MezError::conflict(
                "MMP custody ACK lane busy or unavailable",
            )));
            return;
        }
        let sequence = batch
            .batch
            .messages
            .last()
            .map(|message| message.sequence)
            .unwrap_or(batch.batch.cursor.last_sequence);
        let service = self.service.message_service_mut();
        let prepared =
            service.prepare_acknowledgement(&batch.recipient, sequence, ACK_METADATA_BYTES);
        match prepared {
            Ok(Some(ticket)) => match owner.root.start_acknowledgement(service, &ticket) {
                Ok(()) => {
                    // Registered synchronously before any observer can await or
                    // publish the actual worker's result. Reply loss is inert.
                    owner.acknowledgement = Some(reply);
                    owner.wake = Some(Wake::Observe);
                }
                Err(error) => {
                    // start secures its idle lane before lower issuance. A
                    // failed start leaves this exact ticket safely unissued.
                    let result = service
                        .cancel_acknowledgement(&ticket)
                        .map_err(Into::into)
                        .and(Err(error));
                    let _ = reply.send(result);
                }
            },
            Ok(None) => {
                let cursor = service
                    .subscription(&batch.recipient)
                    .cloned()
                    .ok_or_else(|| MezError::invalid_state("MMP ACK subscription unavailable"));
                let _ = reply.send(cursor);
            }
            Err(error) => {
                let _ = reply.send(Err(error.into()));
            }
        }
    }

    /// Fences fresh root sends and defers exit while an original ACK/open is owned.
    /// The ordinary ephemeral actor retains its existing immediate shutdown path.
    pub(super) fn begin_message_custody_shutdown(
        &mut self,
        reply: oneshot::Sender<Result<RuntimeLifecycleState>>,
    ) -> bool {
        if let Some(owner) = self.message_custody.as_mut() {
            owner.root.request_stop();
            self.sender.close();
            if owner.shutdown.is_some() {
                let _ = reply.send(Err(MezError::conflict(
                    "MMP custody shutdown already pending",
                )));
            } else {
                owner.shutdown = Some(reply);
            }
            false
        } else {
            let result = self.finish_message_shutdown();
            let _ = reply.send(result);
            true
        }
    }

    /// Retires raw leases only after owned ACK settlement; first errors retain
    /// precedence while transport cleanup still runs. Never replays peer input.
    fn finish_message_shutdown(&mut self) -> Result<RuntimeLifecycleState> {
        let storage_error = self
            .message_custody
            .as_ref()
            .and_then(|owner| owner.failure.clone());
        let result = self
            .retire_all_raw_message_identities()
            .map(|()| self.service.lifecycle_state());
        let _ = self.service.clear_runtime_mcp_transports();
        storage_error.map_or(result, Err)
    }
}
