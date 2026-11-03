//! Actor ownership of original MMP storage completion and raw transport replies.
//!
//! A registered root is observed directly by the actor, independently of socket
//! reply receivers and provider slots. Actual joins precede lower publication;
//! monotonic retry never replays socket input. One outstanding ACK owns bounded
//! metadata and its original reply. Graceful shutdown stops fresh acceptance but
//! retains the actor/service/root until that ACK is canonically settled.
//!
//! Registration is deliberately internal and is not enabled by normal product
//! construction yet: native/bridge acceptance, native input dedup, restore,
//! durable retirement and total quotas must all be integrated before advertising
//! durable mode. This owner handles graceful shutdown, not forced actor abort or
//! runtime destruction. Native root jobs must acquire their own actor continuations
//! before registration can be enabled for general ingress.

use std::future::{Future, pending};

use futures_util::FutureExt;
use mez_agent::messaging::{DeliveryCursor, FanoutBatch, MessageError};

mod actions;
mod native;
mod raw;
use crate::runtime::{RuntimeMessageActionAdmission, RuntimePreparedTrackedMessage};
use raw::RawCompletion;
pub(super) use raw::RawMessageContinuation;
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
    /// Bounded source-admission wake; no physical or canonical completion proof.
    AdmissionReady,
    /// Actual joined disposition, still requiring canonical lower adoption.
    Observed(Result<CustodyWorkerStatus>),
    /// Original job's backoff expired; no new admission may replace it.
    Retry,
}

/// Original actor continuation paired with the root's one exclusive job.
enum Pending {
    /// Original native action permission/group and exact lower admission.
    Action(RuntimeMessageActionAdmission),
    /// Full raw body is already in the charged lower reservation, not this owner.
    Acceptance(RawMessageContinuation),
    /// Original tracked native metadata remains in the runtime request owner.
    Native {
        ticket: RuntimePreparedTrackedMessage,
        reply: oneshot::Sender<Result<mez_agent::messaging::Delivery>>,
    },
    /// Actual application/transport commit preceded this original ACK request.
    Acknowledgement(AcknowledgementReply),
}

/// Exact original observer flavor, neither can cancel physical/canonical custody.
enum AcknowledgementReply {
    /// Successful raw peer batch write awaiting canonical transport cursor.
    Fanout(oneshot::Sender<Result<DeliveryCursor>>),
    /// Explicit raw ACK frame awaiting its original connection cursor result.
    Raw(RawMessageContinuation),
}

/// One registered private store with the original pending observer reply.
pub(in crate::host::async_runtime) struct ActorMessageCustody {
    /// Root owns actual blocking handles and immutable original storage plans.
    root: MmpCustodyRoot,
    /// None disables polling an idle/failed root and prevents a busy loop.
    wake: Option<Wake>,
    /// Receiver cancellation cannot remove this issued original continuation.
    pending: Option<Pending>,
    /// ACK allowed while an exact, never-written acceptance remains retained.
    parked_ack: Option<AcknowledgementReply>,
    /// Original acceptance retry deadline, restored after the guarded ACK.
    parked_retry_at: Option<Instant>,
    /// Shutdown is completed only after actual join and canonical settlement.
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
            .field("pending", &self.pending.is_some())
            .field("parked_ack", &self.parked_ack.is_some())
            .field("shutdown", &self.shutdown.is_some())
            .field("failed", &self.failure.is_some())
            .finish()
    }
}

impl ActorMessageCustody {
    /// Allows one ACK only while the actual joined original remains retained
    /// and the root has observed an untouched store position.
    fn can_park_ack(&self) -> bool {
        matches!(self.wake, Some(Wake::Retry(_)))
            && matches!(
                &self.pending,
                Some(Pending::Action(_) | Pending::Acceptance(_) | Pending::Native { .. })
            )
            && self.parked_ack.is_none()
            && self.failure.is_none()
            && self.shutdown.is_none()
            && self.root.can_yield_acknowledgement().unwrap_or(false)
    }

    /// Retains the original retry deadline while the guarded ACK owns its reply.
    fn park_ack(&mut self, reply: AcknowledgementReply) {
        self.parked_retry_at = match self.wake {
            Some(Wake::Retry(at)) => Some(at),
            _ => None,
        };
        self.parked_ack = Some(reply);
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
        self.service.enable_stored_message_actions()?;
        self.message_custody = Some(Box::new(ActorMessageCustody {
            root,
            wake: Some(Wake::Observe),
            pending: None,
            parked_ack: None,
            parked_retry_at: None,
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
        let sources_ready = self.message_custody.as_ref().is_some_and(|owner| {
            owner.pending.is_none()
                && owner.wake.is_none()
                && owner.failure.is_none()
                && owner.shutdown.is_none()
        }) && !self.service.pending_stored_message_actions().is_empty();
        let continuation_wait = self.service.stored_message_continuation_needs_wake();
        async move {
            match next {
                Some((root, Wake::Observe)) => CustodyEvent::Observed(root.observe().await),
                Some((_, Wake::Retry(at))) => {
                    tokio::time::sleep_until(at).await;
                    CustodyEvent::Retry
                }
                None if sources_ready => CustodyEvent::AdmissionReady,
                None if continuation_wait => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    CustodyEvent::AdmissionReady
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
        self.pump_stored_message_actions();
        let ready = self.message_custody.as_ref().is_some_and(|owner| {
            owner.shutdown.is_some()
                && owner.pending.is_none()
                && owner.parked_ack.is_none()
                && owner.wake.is_none()
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
        let mut completion = None;
        let mut native_adopted = false;
        let result = match event {
            CustodyEvent::AdmissionReady => Ok(()),
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::Idle)) => {
                if let Some(Pending::Action(admission)) = &owner.pending {
                    let source = admission.source().clone();
                    self.service
                        .adopt_stored_message_action(admission, |_, _| {
                            Err(MezError::invalid_state(
                                "original message has no published acceptance",
                            ))
                        })
                        .and_then(|_| self.service.project_stored_message_action(&source))
                        .map(|_| {
                            owner.pending.take();
                            owner.failure = None;
                            owner.wake = None;
                            native_adopted = true;
                        })
                } else if let Some(Pending::Native { ticket, .. }) = &owner.pending {
                    let ticket = ticket.clone();
                    self.service
                        .adopt_prepared_tracked_runtime_message(&ticket, |_, _| {
                            Err(MezError::invalid_state(
                                "MMP native original has no published receipt",
                            ))
                        })
                        .map(|delivery| {
                            if let Some(Pending::Native { reply, .. }) = owner.pending.take() {
                                let _ = reply.send(Ok(delivery));
                                native_adopted = true;
                            }
                            owner.failure = None;
                            owner.wake = None;
                        })
                } else {
                    owner.wake = None;
                    Ok(())
                }
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::Unavailable)) => {
                owner.failure =
                    owner.root.failure().ok().flatten().or_else(|| {
                        Some(MezError::invalid_state("MMP custody opening unavailable"))
                    });
                owner.wake = None;
                Ok(())
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::AwaitingAdoption)) => {
                if let Some(Pending::Action(admission)) = &owner.pending {
                    let source = admission.source().clone();
                    self.service
                        .adopt_stored_message_action(admission, |messages, requests| {
                            owner.root.adopt(messages, requests)
                        })
                        .and_then(|_| self.service.project_stored_message_action(&source))
                        .map(|_| {
                            owner.pending.take();
                            owner.failure = None;
                            owner.wake = None;
                            native_adopted = true;
                        })
                } else if matches!(&owner.pending, Some(Pending::Acceptance(_))) {
                    owner
                        .root
                        .adopt(self.service.message_service_mut(), None)
                        .map(|delivery| {
                            if let Some(Pending::Acceptance(raw)) = owner.pending.take() {
                                completion = Some((raw, RawCompletion::Acceptance(delivery)));
                            }
                            owner.failure = None;
                            owner.wake = Some(Wake::Observe);
                        })
                } else if let Some(Pending::Native { ticket, .. }) = &owner.pending {
                    let ticket = ticket.clone();
                    self.service
                        .adopt_prepared_tracked_runtime_message(&ticket, |messages, requests| {
                            owner.root.adopt(messages, Some(requests))
                        })
                        .map(|delivery| {
                            if let Some(Pending::Native { reply, .. }) = owner.pending.take() {
                                let _ = reply.send(Ok(delivery));
                                native_adopted = true;
                            }
                            owner.failure = None;
                            owner.wake = Some(Wake::Observe);
                        })
                } else {
                    Err(MezError::invalid_state(
                        "MMP acceptance has no original actor continuation",
                    ))
                }
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::AwaitingRefusal)) => {
                if let Some(Pending::Action(admission)) = &owner.pending {
                    let source = admission.source().clone();
                    self.service
                        .refuse_stored_message_action(admission, |messages, requests| {
                            owner.root.release_rejected(messages, requests)
                        })
                        .and_then(|()| self.service.project_stored_message_action(&source))
                        .map(|_| {
                            owner.pending.take();
                            owner.failure = None;
                            owner.wake = None;
                        })
                } else if matches!(&owner.pending, Some(Pending::Acceptance(_))) {
                    owner
                        .root
                        .release_rejected(self.service.message_service_mut(), None)
                        .map(|()| {
                            if let Some(Pending::Acceptance(raw)) = owner.pending.take() {
                                completion = Some((
                                    raw,
                                    RawCompletion::Rejected(MessageError::invalid_state(
                                        "MMP original storage admission did not commit",
                                    )),
                                ));
                            }
                            owner.failure = None;
                            owner.wake = Some(Wake::Observe);
                        })
                } else if let Some(Pending::Native { ticket, .. }) = &owner.pending {
                    let ticket = ticket.clone();
                    self.service
                        .release_rejected_tracked_runtime_message(&ticket, |messages, requests| {
                            owner.root.release_rejected(messages, Some(requests))
                        })
                        .map(|()| {
                            if let Some(Pending::Native { reply, .. }) = owner.pending.take() {
                                let _ = reply.send(Err(MezError::invalid_state(
                                    "original tracked MMP admission did not commit",
                                )));
                            }
                            owner.failure = None;
                            owner.wake = Some(Wake::Observe);
                        })
                } else {
                    Err(MezError::invalid_state(
                        "MMP refusal has no original actor continuation",
                    ))
                }
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::AcknowledgementAwaitingAdoption)) => {
                if owner.parked_ack.is_some() {
                    owner
                        .root
                        .adopt_acknowledgement(self.service.message_service_mut())
                        .map(|cursor| {
                            match owner.parked_ack.take() {
                                Some(AcknowledgementReply::Fanout(reply)) => {
                                    let _ = reply.send(Ok(cursor));
                                }
                                Some(AcknowledgementReply::Raw(raw)) => {
                                    completion =
                                        Some((raw, RawCompletion::Acknowledgement(cursor)));
                                }
                                None => (),
                            }
                            owner.failure = None;
                            owner.wake = owner.parked_retry_at.take().map(Wake::Retry);
                        })
                } else if matches!(&owner.pending, Some(Pending::Acknowledgement(_))) {
                    owner
                        .root
                        .adopt_acknowledgement(self.service.message_service_mut())
                        .map(|cursor| {
                            match owner.pending.take() {
                                Some(Pending::Acknowledgement(AcknowledgementReply::Fanout(
                                    reply,
                                ))) => {
                                    let _ = reply.send(Ok(cursor));
                                }
                                Some(Pending::Acknowledgement(AcknowledgementReply::Raw(raw))) => {
                                    completion =
                                        Some((raw, RawCompletion::Acknowledgement(cursor)));
                                }
                                _ => {}
                            }
                            owner.failure = None;
                            owner.wake = Some(Wake::Observe);
                        })
                } else {
                    Err(MezError::invalid_state(
                        "MMP ACK has no original actor continuation",
                    ))
                }
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::RetryAt(at)))
                if matches!(
                    &owner.pending,
                    Some(Pending::Acceptance(_) | Pending::Native { .. } | Pending::Action(_))
                ) =>
            {
                owner.wake = Some(Wake::Retry(at));
                Ok(())
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::AcknowledgementRetryAt(at)))
                if owner.parked_ack.is_some()
                    || matches!(&owner.pending, Some(Pending::Acknowledgement(_))) =>
            {
                owner.wake = Some(Wake::Retry(at));
                Ok(())
            }
            CustodyEvent::Observed(Ok(
                CustodyWorkerStatus::RetryAt(_) | CustodyWorkerStatus::AcknowledgementRetryAt(_),
            )) => Err(MezError::invalid_state(
                "MMP retry has no matching original actor continuation",
            )),
            CustodyEvent::Retry if owner.parked_ack.is_some() => owner
                .root
                .retry_acknowledgement(self.service.message_service_mut(), Instant::now())
                .map(|_| owner.wake = Some(Wake::Observe)),
            CustodyEvent::Retry => match owner.pending {
                Some(Pending::Action(ref admission)) => self
                    .service
                    .retry_stored_message_action(admission, |messages| {
                        if owner.shutdown.is_some() {
                            owner.root.recover_failed_stage(messages, Instant::now())
                        } else {
                            owner.root.retry(messages, Instant::now())
                        }
                    })
                    .map(|_| owner.wake = Some(Wake::Observe)),
                Some(Pending::Native { ref ticket, .. }) => self
                    .service
                    .validate_prepared_tracked_runtime_message(ticket)
                    .and_then(|()| {
                        let messages = self.service.message_service_mut();
                        if owner.shutdown.is_some() {
                            owner.root.recover_failed_stage(messages, Instant::now())
                        } else {
                            owner.root.retry(messages, Instant::now())
                        }
                    })
                    .map(|_| owner.wake = Some(Wake::Observe)),
                Some(Pending::Acceptance(_)) => {
                    let service = self.service.message_service_mut();
                    let retried = if owner.shutdown.is_some() {
                        owner.root.recover_failed_stage(service, Instant::now())
                    } else {
                        owner.root.retry(service, Instant::now())
                    };
                    retried.map(|_| owner.wake = Some(Wake::Observe))
                }
                _ => owner
                    .root
                    .retry_acknowledgement(self.service.message_service_mut(), Instant::now())
                    .map(|_| owner.wake = Some(Wake::Observe)),
            },
            CustodyEvent::Observed(Err(error)) => Err(error),
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::Running)) => {
                owner.wake = Some(Wake::Observe);
                Ok(())
            }
        };
        if let Err(error) = result {
            let source_owned = match &owner.pending {
                Some(Pending::Action(admission)) => self
                    .service
                    .stored_message_action_outcome_is_owned(admission.source()),
                _ => false,
            };
            owner.failure = Some(error);
            if source_owned {
                // Actual root settlement already transferred outcome custody to
                // the original completion owner. A required projection failure
                // is returned at shutdown, never retried as physical storage.
                owner.pending.take();
                owner.wake = None;
            } else {
                owner.wake = Some(Wake::Retry(
                    Instant::now() + std::time::Duration::from_secs(1),
                ));
            }
        }
        if native_adopted {
            self.deliver_stored_message_action_input();
            self.notify_message_delivery();
        }
        if let Some((raw, result)) = completion {
            self.complete_stored_raw_message(raw, result);
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
        let yielding = owner.can_park_ack();
        if !yielding && (owner.wake.is_some() || owner.pending.is_some() || owner.failure.is_some())
        {
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
                    if yielding {
                        owner.park_ack(AcknowledgementReply::Fanout(reply));
                    } else {
                        owner.pending = Some(Pending::Acknowledgement(
                            AcknowledgementReply::Fanout(reply),
                        ));
                    }
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

    /// Fences fresh sends and retains original acceptance/ACK/open until settlement.
    /// The ordinary ephemeral actor retains its existing immediate shutdown path.
    pub(super) fn begin_message_custody_shutdown(
        &mut self,
        reply: oneshot::Sender<Result<RuntimeLifecycleState>>,
    ) -> bool {
        let message_stop = self.service.stop_stored_message_actions();
        if let Some(owner) = self.message_custody.as_mut() {
            if let Err(error) = message_stop {
                owner.failure.get_or_insert(error);
            }
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
