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
mod bridge;
mod checkpoint;
mod native;
mod native_continuity;
mod native_input;
mod terminal_custody;
mod terminal_handoff;
use native_input::NativeInputResult;
mod payload_reads;
use payload_reads::PayloadRead;
mod raw;
mod registration_end;
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
    /// No physical input was issued; try current native/end metadata after delay.
    MetadataCaptureRetry(Instant),
}

/// Content-free callback from the root owned by this actor allocation.
pub(super) enum CustodyEvent {
    /// Bounded source-admission wake; no physical or canonical completion proof.
    AdmissionReady,
    /// A cache-capacity poll expired; no physical input has been issued yet.
    PayloadCapacityReady,
    /// A parked native capacity refusal may recheck the original source.
    NativeReceiveRetry,
    /// Local application/admission backoff is due; no physical retry is implied.
    NativeApplicationReady,
    /// Idle expiry deadline/capture retry; canonical probes still precede selection.
    TerminalMaintenanceReady,
    /// Actual joined disposition, still requiring canonical lower adoption.
    Observed(Result<CustodyWorkerStatus>),
    /// Original job's backoff expired; no new admission may replace it.
    Retry,
    /// A pre-issuance metadata refusal can recheck current bounded state.
    MetadataCaptureRetry,
}

/// Original actor continuation paired with the root's one exclusive job.
enum Pending {
    /// The root owns exact original expiry source/slot/clock and worker escrow.
    TerminalWrite,
    /// Exact canonical sender proof remains in the runtime through lower adoption.
    TerminalConsumption,
    /// Original sender lookup/write, with no reconstructed application owner.
    TerminalHandoff {
        source: mez_agent::messaging::MessageTerminalHandoffAnchor,
        live: bool,
    },
    /// The root owns the exact native metadata input; no message is reserved.
    NativeContinuity,
    /// Pre-reserved original ended allocation retained by the lower owner/root.
    RegistrationEnd,
    /// Exact original canonical probe/write; receipt absence means lookup only.
    NativeInput {
        source: mez_agent::messaging::MessageNativeInputAnchor,
        receipt: Option<String>,
    },
    /// Original qualified-absent native source awaiting its actual cold reload.
    NativePayload(mez_agent::messaging::MessageNativeInputAnchor),
    /// Original native action permission/group and exact lower admission.
    Action(RuntimeMessageActionAdmission),
    /// Original untracked bridge acceptance; payload remains lower-owned.
    Bridge,
    /// Full raw body is already in the charged lower reservation, not this owner.
    Acceptance(RawMessageContinuation),
    /// Original tracked native metadata remains in the runtime request owner.
    Native {
        ticket: RuntimePreparedTrackedMessage,
        reply: oneshot::Sender<Result<mez_agent::messaging::Delivery>>,
    },
    /// Same-point session capture awaiting actual original-store verification.
    Checkpoint(Box<checkpoint::Checkpoint>),
    /// Original accepted source whose payload needs an owned reload.
    PayloadLoad(PayloadRead),
    /// Actual application/transport commit preceded this original ACK request.
    Acknowledgement(AcknowledgementReply),
}

/// Exact original observer flavor, neither can cancel physical/canonical custody.
enum AcknowledgementReply {
    /// Original sealed native commit, never a task/replay continuation.
    Native {
        receipt: Option<String>,
        _proof: mez_agent::messaging::MessageNativeInputCommit,
    },
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
    /// Independent capacity poll; it never blocks unrelated fresh admission.
    payload_retry_at: Option<Instant>,
    /// Independent idle expiry deadline or pre-issuance capacity backoff.
    terminal_retry_at: Option<Instant>,
    /// Bounded payload-free requests, each fenced to its original registration.
    payload_reads: std::collections::VecDeque<PayloadRead>,
    /// Payload-free bounded scan progress, reset by actual delivery notification.
    native_scan_after: Option<(u64, usize)>,
    native_scan_pending: bool,
    /// Coalesces real notifications behind an active cursor into one more pass.
    native_scan_again: bool,
    native_scan_watermark: u64,
    /// One payload-free source retained through pre-issuance capacity backoff.
    native_retry: Option<(Instant, mez_agent::messaging::MessageNativeInputAnchor)>,
    /// One joined original reply before application/ACK handoff completes.
    native_result: Option<NativeInputResult>,
    /// Payload-free stable scan over original terminal slots; real changes
    /// coalesce into one further pass without discarding an active cursor.
    handoff_scan_after: Option<(u64, usize)>,
    handoff_scan_pending: bool,
    handoff_scan_again: bool,
    /// Unissued lookup/capture refusal, independent of physical root retries.
    handoff_retry: Option<(
        Instant,
        Option<mez_agent::messaging::MessageTerminalHandoffAnchor>,
    )>,
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
            .field("native_result", &self.native_result.is_some())
            .field("parked_ack", &self.parked_ack.is_some())
            .field("shutdown", &self.shutdown.is_some())
            .field("failed", &self.failure.is_some())
            .finish()
    }
}

impl ActorMessageCustody {
    /// Reopens source observation without discarding bounded pass progress.
    /// A real state change behind the cursor must survive this pass's eventual
    /// exhaustion. Coalescing into one extra pass avoids perpetual idle polls.
    fn request_native_scan(&mut self) {
        self.native_scan_again |= self.native_scan_after.is_some();
        self.native_scan_pending = true;
    }

    /// Reopens terminal lookup while preserving bounded current pass progress.
    fn request_handoff_scan(&mut self) {
        self.handoff_scan_again |= self.handoff_scan_after.is_some();
        self.handoff_scan_pending = true;
    }

    /// Keeps one parked lookup source without replacing its retry deadline.
    /// Later deferred slots remain in lower custody until the due retry reopens
    /// one pass. This avoids an endless immediate scan over two busy owners.
    fn park_handoff_source(&mut self, source: mez_agent::messaging::MessageTerminalHandoffAnchor) {
        if self.handoff_retry.is_none() {
            self.handoff_retry = Some((
                Instant::now() + std::time::Duration::from_secs(1),
                Some(source),
            ));
        }
    }

    /// A refused capture issued no worker/reservation, so a real ACK may use the
    /// idle root to release capacity before the next metadata attempt.
    fn can_interrupt_metadata_capture(&self) -> bool {
        matches!(self.wake, Some(Wake::MetadataCaptureRetry(_)))
            && self.pending.is_none()
            && self.failure.is_none()
            && self.shutdown.is_none()
    }
    /// Allows one ACK only while the actual joined original remains retained
    /// and the root has observed an untouched store position.
    fn can_park_ack(&self) -> bool {
        matches!(self.wake, Some(Wake::Retry(_)))
            && matches!(
                &self.pending,
                Some(
                    Pending::Action(_)
                        | Pending::Bridge
                        | Pending::Acceptance(_)
                        | Pending::Native { .. }
                )
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
    /// Exercises the actual production custody pump without dispatching a live
    /// provider in boundary tests. No status or commit fact is manufactured.
    #[cfg(test)]
    pub(in crate::host) fn poll_message_custody_for_tests(&mut self) {
        self.poll_message_custody();
    }

    /// Polls the real custody wake without applying it. Combined retry controls
    /// use this to detect immediate ready loops without inventing timer status
    /// or waiting for CPU load to make a regression externally observable.
    #[cfg(test)]
    pub(in crate::host) fn message_custody_wake_is_ready_for_tests(&self) -> bool {
        self.message_custody_wakeup().now_or_never().is_some()
    }

    /// Injects a failure at the actual canonical append boundary after original
    /// preparation, without manufacturing receipt, join or commit evidence.
    #[cfg(test)]
    pub(in crate::host) fn fail_next_native_canonical_append_for_tests(&self) {
        self.service
            .agent_transcript_store()
            .unwrap()
            .fail_next_transcript_append();
    }

    /// Interrupts actual canonical publication with real staged bytes on disk,
    /// while the root continues owning its original preparation and lease.
    #[cfg(test)]
    pub(in crate::host) fn fail_custody_receipt_publication_for_tests(
        &self,
        point: crate::storage::transcript::CustodyReceiptPublicationFault,
    ) {
        self.service
            .agent_transcript_store()
            .unwrap()
            .fail_custody_receipt_publication_for_tests(point);
    }

    /// Applies actual live presence and the production delivery notification.
    /// No selection, native binding, worker status or commit is manufactured.
    #[cfg(test)]
    pub(in crate::host) fn native_presence_changed_for_tests(
        &mut self,
        recipient: &mez_core::ids::AgentId,
        status: mez_agent::messaging::AgentPresenceStatus,
    ) -> Result<()> {
        self.service.message_service_mut().update_presence(
            recipient,
            status,
            crate::runtime::current_unix_millis(),
        )?;
        self.notify_message_delivery();
        Ok(())
    }

    /// Observes task records without claiming or scheduling execution. Private
    /// restore may legitimately retain interrupted historical user turns.
    #[cfg(test)]
    pub(in crate::host) fn native_turns_for_tests(&self) -> Vec<mez_agent::AgentTurnRecord> {
        self.service.agent_turn_ledger().turns().to_vec()
    }

    /// Observes startup subscription presence without offering mail, creating a
    /// recipient or manufacturing an application/transport completion.
    #[cfg(test)]
    pub(in crate::host) fn native_subscription_for_tests(
        &self,
        recipient: &mez_core::ids::AgentId,
    ) -> Option<mez_agent::messaging::DeliveryCursor> {
        self.service
            .message_service()
            .subscription(recipient)
            .cloned()
    }

    /// Requests the actual shutdown fence while allowing tests to observe each
    /// retained root operation without dispatching any provider service.
    #[cfg(test)]
    pub(in crate::host) fn begin_message_custody_shutdown_for_tests(
        &mut self,
    ) -> oneshot::Receiver<Result<RuntimeLifecycleState>> {
        let (reply, response) = oneshot::channel();
        self.begin_message_custody_shutdown(reply);
        response
    }

    /// Observes and applies only this actor's actual retained root handle. This
    /// keeps fault-stage tests independent of paid/network provider dispatch.
    #[cfg(test)]
    pub(in crate::host) async fn observe_message_custody_for_tests(
        &mut self,
    ) -> Result<CustodyWorkerStatus> {
        let root = self
            .message_custody
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("test actor custody missing"))?
            .root
            .clone();
        let status = root.observe().await?;
        self.apply_message_custody_event(CustodyEvent::Observed(Ok(status.clone())));
        Ok(status)
    }

    /// Transfers exclusive scheduling of one root before its first actor claim.
    /// Rejects replacing an existing root, preserving its actual handles/replies.
    /// Trusted setup must not schedule external jobs on a clone after transfer.
    #[allow(
        dead_code,
        reason = "durable ingress activation continues under e048f9a8"
    )]
    pub(in crate::host) fn register_message_custody(&mut self, root: MmpCustodyRoot) -> Result<()> {
        if self.message_custody.is_some() {
            return Err(MezError::conflict("MMP custody root already registered"));
        }
        self.service.enable_stored_message_actions()?;
        self.service.enable_terminal_layout_sequences();
        self.message_custody = Some(Box::new(ActorMessageCustody {
            root,
            wake: Some(Wake::Observe),
            payload_retry_at: None,
            terminal_retry_at: None,
            payload_reads: std::collections::VecDeque::new(),
            native_scan_after: None,
            native_scan_pending: true,
            native_scan_again: false,
            native_scan_watermark: 0,
            native_retry: None,
            native_result: None,
            handoff_scan_after: None,
            handoff_scan_pending: true,
            handoff_scan_again: false,
            handoff_retry: None,
            pending: None,
            parked_ack: None,
            parked_retry_at: None,
            shutdown: None,
            failure: None,
        }));
        Ok(())
    }

    /// Enables bounded fresh-store spill after the registered root opened.
    /// Existing/migrated stores require trusted recovery, never a guessed reset.
    #[allow(
        dead_code,
        reason = "fresh-store spill setup remains internal until native recovery and all ingress are qualified"
    )]
    pub(in crate::host::async_runtime) fn enable_message_payload_spill(
        &mut self,
        limits: mez_agent::messaging::MessageStorageLimits,
    ) -> Result<()> {
        let owner = self
            .message_custody
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("MMP storage root is not registered"))?;
        if owner.pending.is_some() || owner.shutdown.is_some() {
            return Err(MezError::invalid_state(
                "MMP storage root is already active",
            ));
        }
        owner
            .root
            .configure_payload_spill(self.service.message_service_mut(), limits)?;
        self.service
            .message_service_mut()
            .require_native_continuity(owner.root.storage_scope())?;
        self.service.refresh_native_message_bindings()
    }

    /// Actual delivery/native lifecycle changes make an exhausted bounded scan
    /// eligible again. Active progress is preserved, with one further pass
    /// retained so changes behind its cursor cannot disappear at exhaustion.
    pub(super) fn reset_native_message_input_scan(&mut self) {
        if let Some(owner) = self.message_custody.as_mut() {
            owner.request_native_scan();
            owner.request_handoff_scan();
            owner.terminal_retry_at = None;
        }
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
        let handoff_owned = self.service.has_pending_terminal_handoff();
        let restoring = self.service.message_restore_pending();
        let next = self
            .message_custody
            .as_ref()
            .and_then(|owner| owner.wake.map(|wake| (owner.root.clone(), wake)));
        let payload_retry = self
            .message_custody
            .as_ref()
            .and_then(|owner| owner.payload_retry_at);
        let native_retry = self.message_custody.as_ref().and_then(|owner| {
            (!restoring
                && owner.shutdown.is_none()
                && owner.pending.is_none()
                && owner.wake.is_none()
                && owner.failure.is_none()
                && !handoff_owned)
                .then(|| owner.native_retry.as_ref().map(|(at, _)| *at))
                .flatten()
        });
        let reads_ready = self.message_custody.as_ref().is_some_and(|owner| {
            owner.pending.is_none()
                && owner.wake.is_none()
                && owner.failure.is_none()
                && owner.shutdown.is_none()
                && !owner.payload_reads.is_empty()
                && owner.payload_retry_at.is_none_or(|at| at <= Instant::now())
        });
        let sources_ready = self.message_custody.as_ref().is_some_and(|owner| {
            owner.pending.is_none()
                && owner.wake.is_none()
                && owner.failure.is_none()
                && owner.shutdown.is_none()
        }) && (self.service.has_pending_stored_bridge_message()
            || !self.service.pending_stored_message_actions().is_empty());
        let native_input_ready = self.service.native_input_storage_required()
            && !handoff_owned
            && self.message_custody.as_ref().is_some_and(|o| {
                o.pending.is_none()
                    && o.wake.is_none()
                    && o.failure.is_none()
                    && ((o.native_result.is_none()
                        && o.native_scan_pending
                        && !restoring
                        && o.shutdown.is_none())
                        || (o.native_result.is_none()
                            && !restoring
                            && o.shutdown.is_none()
                            && o.native_retry
                                .as_ref()
                                .is_some_and(|(at, _)| *at <= Instant::now()))
                        || o.native_result
                            .as_ref()
                            .is_some_and(NativeInputResult::ready_for_ack)
                        || (o.native_result.is_none()
                            && self.service.pending_native_input_work().is_some())
                        || self.service.pending_native_input_ack().is_some())
            });
        let handoff_ready = self.service.native_input_storage_required()
            && self.message_custody.as_ref().is_some_and(|owner| {
                owner.pending.is_none()
                    && owner.wake.is_none()
                    && owner.failure.is_none()
                    && owner.native_result.is_none()
                    && owner
                        .handoff_retry
                        .as_ref()
                        .is_none_or(|(at, source)| source.is_some() || *at <= Instant::now())
                    && (self.service.pending_terminal_handoff_work().is_some()
                        || self
                            .service
                            .pending_terminal_handoff_consumption()
                            .is_some()
                        || (!handoff_owned
                            && !restoring
                            && owner.shutdown.is_none()
                            && owner.handoff_scan_pending))
            });
        let root_idle = self.message_custody.as_ref().is_some_and(|owner| {
            owner.pending.is_none() && owner.wake.is_none() && owner.failure.is_none()
        });
        let application_at = self.message_custody.as_ref().and_then(|owner| {
            self.service
                .native_input_completion_at(root_idle && !handoff_owned)
                .map(Instant::from_std)
                .into_iter()
                .chain(
                    self.service
                        .terminal_handoff_completion_at(root_idle)
                        .map(Instant::from_std),
                )
                .chain(
                    // A parked lookup cannot use the one handoff slot while
                    // application/consumption owns it. Its expired deadline
                    // must not defeat that owner's future admission backoff.
                    // None is the current work's own pre-issue retry and must
                    // still wake; Some retains an independent parked source.
                    owner.handoff_retry.as_ref().and_then(|(at, source)| {
                        (root_idle && (source.is_none() || (!handoff_owned && !restoring)))
                            .then_some(*at)
                    }),
                )
                .chain(
                    owner
                        .native_result
                        .as_ref()
                        .and_then(|result| result.application_at(root_idle)),
                )
                .min()
        });
        let continuation_wait = self.service.stored_message_continuation_needs_wake();
        let native_ready = !restoring
            && self.message_custody.as_ref().is_some_and(|owner| {
                owner.pending.is_none()
                    && owner.wake.is_none()
                    && owner.failure.is_none()
                    && owner.shutdown.is_none()
            })
            && self
                .service
                .message_service()
                .pending_native_continuity()
                .is_some();
        let expiry_at = self.message_custody.as_ref().and_then(|owner| {
            (!restoring
                && owner.pending.is_none()
                && owner.wake.is_none()
                && owner.failure.is_none()
                && owner.shutdown.is_none()
                && owner.native_result.is_none()
                && owner.native_retry.is_none()
                && !handoff_owned)
                .then_some(owner.terminal_retry_at)
                .flatten()
        });
        let registration_end_ready = root_idle
            && self
                .service
                .message_service()
                .has_pending_registration_end()
                .unwrap_or(true);
        let physical = async move {
            match next {
                Some((root, Wake::Observe)) => CustodyEvent::Observed(root.observe().await),
                Some((_, Wake::Retry(at))) => {
                    tokio::time::sleep_until(at).await;
                    CustodyEvent::Retry
                }
                Some((_, Wake::MetadataCaptureRetry(at))) => {
                    tokio::time::sleep_until(at).await;
                    CustodyEvent::MetadataCaptureRetry
                }
                None if registration_end_ready => CustodyEvent::AdmissionReady,
                None if handoff_ready => CustodyEvent::AdmissionReady,
                None if native_input_ready => CustodyEvent::AdmissionReady,
                None if native_ready => CustodyEvent::AdmissionReady,
                None if reads_ready => CustodyEvent::AdmissionReady,
                None if payload_retry.is_some() || native_retry.is_some() => {
                    match (payload_retry, native_retry) {
                        (Some(raw), Some(native)) if raw <= native => {
                            tokio::time::sleep_until(raw).await;
                            CustodyEvent::PayloadCapacityReady
                        }
                        (_, Some(native)) => {
                            tokio::time::sleep_until(native).await;
                            CustodyEvent::NativeReceiveRetry
                        }
                        (Some(raw), None) => {
                            tokio::time::sleep_until(raw).await;
                            CustodyEvent::PayloadCapacityReady
                        }
                        (None, None) => pending().await,
                    }
                }
                None if sources_ready => CustodyEvent::AdmissionReady,
                None if continuation_wait => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    CustodyEvent::AdmissionReady
                }
                None => pending().await,
            }
        };
        let application = async move {
            if let Some(at) = application_at {
                tokio::select! {
                    event = physical => event,
                    _ = tokio::time::sleep_until(at) => CustodyEvent::NativeApplicationReady,
                }
            } else {
                physical.await
            }
        };
        async move {
            tokio::select! {
                event = application => event,
                _ = async move {
                    if let Some(at) = expiry_at { tokio::time::sleep_until(at).await; }
                    else { pending::<()>().await; }
                } => CustodyEvent::TerminalMaintenanceReady,
            }
        }
    }

    /// Original physical and joined application owners must settle before a
    /// live layout restore can retire their registrations. Unissued optional
    /// scans remain dormant and grant no canonical or replacement authority.
    pub(super) fn message_custody_quiescent_for_restore(&self) -> bool {
        self.message_custody.as_ref().is_some_and(|owner| {
            owner.pending.is_none()
                && owner.parked_ack.is_none()
                && owner.wake.is_none()
                && owner.native_result.is_none()
                && owner.failure.is_none()
        })
    }

    /// Gives storage completion a bounded turn even under continuously busy ingress.
    /// Returns true only when an owned shutdown can safely return the service.
    pub(super) fn poll_message_custody(&mut self) -> bool {
        if let Some(event) = self.message_custody_wakeup().now_or_never() {
            self.apply_message_custody_event(event);
        }
        self.pump_native_message_applications();
        self.pump_terminal_handoff_application();
        self.pump_message_registration_end();
        self.pump_native_message_continuity();
        self.pump_terminal_handoff();
        self.pump_native_message_input();
        self.pump_message_terminal();
        self.pump_message_payload_reads();
        self.pump_stored_bridge_message();
        self.pump_stored_message_actions();
        let ready = self.message_custody.as_ref().is_some_and(|owner| {
            owner.shutdown.is_some()
                && owner.pending.is_none()
                && owner.parked_ack.is_none()
                && owner.wake.is_none()
                && owner.native_result.is_none()
                && !self.service.has_pending_native_input()
                && !self.service.has_pending_terminal_handoff()
                && !self
                    .service
                    .message_service()
                    .has_pending_registration_end()
                    .unwrap_or(true)
        });
        if ready {
            // Socket retirement may itself create new end intents. Perform it
            // after original ACK/application settlement, but before consuming
            // the shutdown reply or releasing the root. Stop permits this end
            // drain even though fresh terminal observations are withheld.
            if let Err(error) = self.retire_all_raw_message_identities()
                && let Some(owner) = self.message_custody.as_mut()
            {
                owner.failure.get_or_insert(error);
            }
            if self
                .service
                .message_service()
                .has_pending_registration_end()
                .unwrap_or(true)
            {
                self.pump_message_registration_end();
                return false;
            }
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
        let mut payload_read = None;
        let mut capacity_ready = false;
        let mut native_absence = None;
        let mut native_retirement = None;
        let mut handoff_absence = None;
        let mut native_ack_receipt = None;
        let result = match event {
            CustodyEvent::AdmissionReady
            | CustodyEvent::NativeReceiveRetry
            | CustodyEvent::NativeApplicationReady => Ok(()),
            CustodyEvent::TerminalMaintenanceReady => {
                owner.terminal_retry_at = None;
                owner.request_native_scan();
                owner.request_handoff_scan();
                Ok(())
            }
            CustodyEvent::MetadataCaptureRetry => {
                if owner.pending.is_none()
                    && matches!(owner.wake, Some(Wake::MetadataCaptureRetry(_)))
                {
                    owner.wake = None;
                }
                Ok(())
            }
            CustodyEvent::PayloadCapacityReady => {
                owner.payload_retry_at = None;
                capacity_ready = true;
                Ok(())
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::Idle)) => {
                owner.payload_retry_at = None;
                if matches!(
                    owner.pending,
                    Some(
                        Pending::TerminalWrite
                            | Pending::TerminalConsumption
                            | Pending::TerminalHandoff { .. }
                    )
                ) {
                    Err(MezError::invalid_state(
                        "MMP expiry lost its original worker state",
                    ))
                } else if matches!(
                    owner.pending,
                    Some(Pending::NativeContinuity | Pending::RegistrationEnd | Pending::Bridge)
                ) {
                    Err(MezError::invalid_state(
                        "MMP native journal lost its original worker state",
                    ))
                } else if let Some(Pending::Action(admission)) = &owner.pending {
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
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::SnapshotAwaitingAdoption))
                if matches!(owner.pending, Some(Pending::Checkpoint(_))) =>
            {
                owner.root.adopt_checkpoint().map(|(snapshot, lease)| {
                    if let Some(Pending::Checkpoint(checkpoint)) = owner.pending.take() {
                        checkpoint.persist(snapshot, lease);
                    }
                    owner.failure = None;
                    owner.wake = None;
                })
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::SnapshotRetryAt(at)))
                if matches!(owner.pending, Some(Pending::Checkpoint(_))) =>
            {
                owner.wake = Some(Wake::Retry(at));
                Ok(())
            }
            CustodyEvent::Observed(Ok(
                CustodyWorkerStatus::SnapshotAwaitingAdoption
                | CustodyWorkerStatus::SnapshotRetryAt(_),
            )) => Err(MezError::invalid_state(
                "MMP checkpoint has no matching actor capture",
            )),
            CustodyEvent::Observed(Ok(
                CustodyWorkerStatus::RetirementInspectionAwaitingAdoption,
            )) => Err(MezError::invalid_state(
                "MMP retirement inspection has no matching actor continuation",
            )),
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::PayloadLoadAwaitingAdoption))
                if matches!(owner.pending, Some(Pending::NativePayload(_))) =>
            {
                owner
                    .root
                    .publish_payload_load(self.service.message_service_mut())
                    .map(|_| {
                        if let Some(Pending::NativePayload(source)) = owner.pending.take() {
                            native_absence = Some(source);
                        }
                        owner.wake = None;
                        owner.failure = None;
                    })
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::PayloadLoadAwaitingAdoption))
                if matches!(
                    owner.pending,
                    Some(Pending::PayloadLoad(_) | Pending::NativePayload(_))
                ) =>
            {
                owner
                    .root
                    .publish_payload_load(self.service.message_service_mut())
                    .map(|_| {
                        if let Some(Pending::PayloadLoad(read)) = owner.pending.take() {
                            payload_read = Some(read);
                        }
                        owner.failure = None;
                        owner.wake = None;
                        capacity_ready = true;
                    })
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::PayloadLoadRetryAt(at)))
                if matches!(
                    owner.pending,
                    Some(Pending::PayloadLoad(_) | Pending::NativePayload(_))
                ) =>
            {
                owner.wake = Some(Wake::Retry(at));
                Ok(())
            }
            CustodyEvent::Observed(Ok(
                CustodyWorkerStatus::PayloadLoadAwaitingAdoption
                | CustodyWorkerStatus::PayloadLoadRetryAt(_),
            )) => Err(MezError::invalid_state(
                "MMP payload reload has no matching actor continuation",
            )),
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::Unavailable)) => {
                owner.failure =
                    owner.root.failure().ok().flatten().or_else(|| {
                        Some(MezError::invalid_state("MMP custody opening unavailable"))
                    });
                owner.wake = None;
                Ok(())
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::AwaitingAdoption)) => {
                if matches!(&owner.pending, Some(Pending::Bridge)) {
                    owner
                        .root
                        .adopt(self.service.message_service_mut(), None)
                        .map(|_| {
                            owner.pending.take();
                            owner.failure = None;
                            owner.wake = Some(Wake::Observe);
                            native_adopted = true;
                        })
                } else if let Some(Pending::Action(admission)) = &owner.pending {
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
                if matches!(&owner.pending, Some(Pending::Bridge)) {
                    owner
                        .root
                        .release_rejected(self.service.message_service_mut(), None)
                        .map(|()| {
                            owner.pending.take();
                            owner.failure = None;
                            owner.wake = Some(Wake::Observe);
                            eprintln!(
                                "mez: runtime bridge original storage admission did not commit"
                            );
                        })
                } else if let Some(Pending::Action(admission)) = &owner.pending {
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
                                Some(AcknowledgementReply::Native { receipt, .. }) => {
                                    native_ack_receipt = receipt;
                                }
                                Some(AcknowledgementReply::Raw(raw)) => {
                                    completion =
                                        Some((raw, RawCompletion::Acknowledgement(cursor)));
                                }
                                None => (),
                            }
                            owner.request_native_scan();
                            owner.request_handoff_scan();
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
                                Some(Pending::Acknowledgement(AcknowledgementReply::Native {
                                    receipt,
                                    ..
                                })) => {
                                    native_ack_receipt = receipt;
                                }
                                Some(Pending::Acknowledgement(AcknowledgementReply::Raw(raw))) => {
                                    completion =
                                        Some((raw, RawCompletion::Acknowledgement(cursor)));
                                }
                                _ => {}
                            }
                            owner.request_native_scan();
                            owner.request_handoff_scan();
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
                    Some(
                        Pending::Acceptance(_)
                            | Pending::Native { .. }
                            | Pending::Action(_)
                            | Pending::Bridge
                    )
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
                Some(Pending::TerminalWrite | Pending::TerminalConsumption) => owner
                    .root
                    .retry_terminal(self.service.message_service_mut(), Instant::now())
                    .map(|_| owner.wake = Some(Wake::Observe)),
                Some(Pending::TerminalHandoff { .. }) => owner
                    .root
                    .retry_terminal_handoff(Instant::now())
                    .map(|_| owner.wake = Some(Wake::Observe)),
                Some(Pending::NativeInput { .. }) => owner
                    .root
                    .retry_native_input(Instant::now())
                    .map(|_| owner.wake = Some(Wake::Observe)),
                Some(Pending::NativeContinuity) => owner
                    .root
                    .retry_native_continuity(Instant::now())
                    .map(|_| owner.wake = Some(Wake::Observe)),
                Some(Pending::RegistrationEnd) => owner
                    .root
                    .retry_registration_end(Instant::now())
                    .map(|_| owner.wake = Some(Wake::Observe)),
                Some(Pending::Checkpoint(_)) => owner
                    .root
                    .retry_checkpoint(Instant::now())
                    .map(|_| owner.wake = Some(Wake::Observe)),
                Some(Pending::PayloadLoad(_) | Pending::NativePayload(_)) => owner
                    .root
                    .retry_payload_load(self.service.message_service_mut(), Instant::now())
                    .map(|_| owner.wake = Some(Wake::Observe)),
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
                Some(Pending::Acceptance(_) | Pending::Bridge) => {
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
            CustodyEvent::Observed(Ok(
                CustodyWorkerStatus::RecoveryAwaitingAdoption
                | CustodyWorkerStatus::RecoveryRetryAt(_),
            )) => Err(MezError::invalid_state(
                "MMP recovery must finish before root actor registration",
            )),
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::TerminalAwaitingAdoption))
                if matches!(
                    owner.pending,
                    Some(Pending::TerminalWrite | Pending::TerminalConsumption)
                ) =>
            {
                owner
                    .root
                    .adopt_terminal(self.service.message_service_mut())
                    .map(|_| {
                        if matches!(owner.pending, Some(Pending::TerminalConsumption)) {
                            self.service.complete_terminal_handoff_consumption();
                        }
                        owner.pending = None;
                        owner.wake = None;
                        owner.failure = None;
                        owner.terminal_retry_at = None;
                        owner.request_handoff_scan();
                    })
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::TerminalRetryAt(at)))
                if matches!(
                    owner.pending,
                    Some(Pending::TerminalWrite | Pending::TerminalConsumption)
                ) =>
            {
                owner.wake = Some(Wake::Retry(at));
                Ok(())
            }
            CustodyEvent::Observed(Ok(
                CustodyWorkerStatus::TerminalAwaitingAdoption
                | CustodyWorkerStatus::TerminalRetryAt(_),
            )) => Err(MezError::invalid_state(
                "MMP expiry has no original actor continuation",
            )),
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::RegistrationEndAwaitingAdoption))
                if matches!(owner.pending, Some(Pending::RegistrationEnd)) =>
            {
                owner
                    .root
                    .adopt_registration_end(self.service.message_service_mut())
                    .map(|_| {
                        owner.pending = None;
                        owner.wake = None;
                        owner.failure = None;
                        owner.terminal_retry_at = None;
                        owner.request_native_scan();
                        capacity_ready = true;
                    })
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::RegistrationEndRetryAt(at)))
                if matches!(owner.pending, Some(Pending::RegistrationEnd)) =>
            {
                owner.wake = Some(Wake::Retry(at));
                Ok(())
            }
            CustodyEvent::Observed(Ok(
                CustodyWorkerStatus::RegistrationEndAwaitingAdoption
                | CustodyWorkerStatus::RegistrationEndRetryAt(_),
            )) => Err(MezError::invalid_state(
                "MMP registration end requires its original metadata adopter",
            )),
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::NativeContinuityAwaitingAdoption))
                if matches!(owner.pending, Some(Pending::NativeContinuity)) =>
            {
                owner
                    .root
                    .adopt_native_continuity(self.service.message_service_mut())
                    .map(|_| {
                        owner.pending.take();
                        owner.wake = None;
                        owner.failure = None;
                        capacity_ready = true;
                    })
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::NativeContinuityRetryAt(at)))
                if matches!(owner.pending, Some(Pending::NativeContinuity)) =>
            {
                owner.wake = Some(Wake::Retry(at));
                Ok(())
            }
            CustodyEvent::Observed(Ok(
                CustodyWorkerStatus::NativeContinuityAwaitingAdoption
                | CustodyWorkerStatus::NativeContinuityRetryAt(_),
            )) => Err(MezError::invalid_state(
                "MMP native continuity requires its original metadata adopter",
            )),
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::NativeInputAwaitingAdoption))
                if matches!(owner.pending, Some(Pending::NativeInput { .. })) =>
            {
                owner
                    .root
                    .adopt_native_input(self.service.message_service())
                    .and_then(|observation| {
                        if let Some(Pending::NativeInput { source, receipt }) = owner.pending.take()
                        {
                            use mez_agent::messaging::MessageNativeInputObservation;
                            match (receipt, observation) {
                                (receipt, MessageNativeInputObservation::Committed(proof)) => {
                                    // Retain the actual reply before application
                                    // validation, position or request-reply work.
                                    owner.native_result =
                                        Some(NativeInputResult::new(receipt, proof));
                                }
                                (None, MessageNativeInputObservation::Absent) => {
                                    native_absence = Some(source)
                                }
                                (None, MessageNativeInputObservation::RecipientEnded(witness)) => {
                                    native_retirement = Some(witness)
                                }
                                (
                                    Some(_),
                                    MessageNativeInputObservation::Absent
                                    | MessageNativeInputObservation::RecipientEnded(_),
                                ) => {
                                    return Err(MezError::invalid_state(
                                        "native canonical write returned absence",
                                    ));
                                }
                            }
                        }
                        owner.wake = None;
                        owner.failure = None;
                        Ok(())
                    })
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::NativeInputRetryAt(at)))
                if matches!(owner.pending, Some(Pending::NativeInput { .. })) =>
            {
                owner.wake = Some(Wake::Retry(at));
                Ok(())
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::TerminalHandoffAwaitingAdoption))
                if matches!(owner.pending, Some(Pending::TerminalHandoff { .. })) =>
            {
                owner
                    .root
                    .adopt_terminal_handoff(self.service.message_service())
                    .and_then(|proof| {
                        if let Some(Pending::TerminalHandoff { source, live }) =
                            owner.pending.take()
                        {
                            if let Some(proof) = proof {
                                self.service.retain_terminal_handoff_commit(proof);
                            } else if live {
                                return Err(MezError::invalid_state(
                                    "original terminal canonical write returned absence",
                                ));
                            } else {
                                handoff_absence = Some(source);
                            }
                        }
                        owner.wake = None;
                        owner.failure = None;
                        Ok(())
                    })
            }
            CustodyEvent::Observed(Ok(CustodyWorkerStatus::TerminalHandoffRetryAt(at)))
                if matches!(owner.pending, Some(Pending::TerminalHandoff { .. })) =>
            {
                owner.wake = Some(Wake::Retry(at));
                Ok(())
            }
            CustodyEvent::Observed(Ok(
                CustodyWorkerStatus::NativeInputAwaitingAdoption
                | CustodyWorkerStatus::NativeInputRetryAt(_)
                | CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
                | CustodyWorkerStatus::TerminalHandoffRetryAt(_),
            )) => Err(MezError::invalid_state(
                "native canonical job lacks original actor continuation",
            )),
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
        if let Some(id) = native_ack_receipt {
            self.finish_native_message_ack(&id);
        }
        self.pump_native_message_applications();
        self.pump_terminal_handoff_application();
        if let Some(source) = handoff_absence {
            self.activate_absent_terminal_handoff(source);
        }
        if let Some(source) = native_absence {
            self.activate_absent_native_message(source);
        }
        if let Some(witness) = native_retirement {
            self.activate_retired_native_message(witness);
        }
        if let Some(read) = payload_read {
            self.complete_payload_read(read);
        }
        if capacity_ready {
            self.notify_message_delivery();
        }
        if native_adopted {
            self.deliver_stored_message_action_input();
            self.notify_message_delivery();
        }
        if let Some((raw, result)) = completion {
            self.complete_stored_raw_message(raw, result);
        }
        // Terminal consumption can release the last canonical receipt fencing
        // a loop's next source read. Reconcile only after adoption and custody
        // bookkeeping, never while a provisional proof still owns the source.
        self.dispatch_pending_agent_prompt_history();
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
        if !yielding
            && !owner.can_interrupt_metadata_capture()
            && (owner.wake.is_some() || owner.pending.is_some() || owner.failure.is_some())
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
