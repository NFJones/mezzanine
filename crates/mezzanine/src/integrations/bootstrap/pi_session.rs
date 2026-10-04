//! Bounded Pi session coordination over existing lifecycle and lease owners.
//!
//! The privately authorized launcher drives this future outside extension reload
//! lifetime. Callbacks only try-send typed, epoch-bound observations; no callback
//! waits for IPC or grants authority. Renewal is polled during every await,
//! including presentation delivery. No tasks are spawned or detached. Cancellation,
//! expiry and exchange failure retain the caller's exact pending owner state;
//! remote effects may already have happened and are never automatically replayed.
//! This component does not implement vendor IPC, private launch or installation.

use tokio::sync::{mpsc, oneshot, watch};

use super::pi::Observation;
use super::pi_owner::{LifecycleOwner, Operation};
use super::pi_renewal::{self, ActiveLease};
use super::pi_transport::CapabilityTransport;
use crate::error::{MezError, Result};

/// Finite callback/launcher admission separate from the reducer's report budget.
const CAPACITY: usize = 32;

/// Typed callback facts and explicit same-session replacement requests.
enum Input {
    Observation {
        epoch: u64,
        session: String,
        fact: Observation,
    },
    Attach {
        session: String,
        reply: oneshot::Sender<Result<AttachmentOffer>>,
    },
}

/// Nonblocking callback ingress; possession grants no registration capability.
#[derive(Clone)]
pub(crate) struct Ingress(mpsc::Sender<Input>, watch::Sender<()>);

/// Exclusive worker input; never cloned or reconstructed from presentation.
pub(crate) struct Inputs(mpsc::Receiver<Input>, watch::Receiver<()>);

/// Unconfirmed replacement proposal. Dropping it leaves the observer suspended.
pub(crate) struct AttachmentOffer {
    /// Explicit confirmation is the activation command, not receipt of a proposal.
    confirm: oneshot::Sender<()>,
    /// Worker outcome for that command; losing this reply does not undo acceptance.
    accepted: oneshot::Receiver<Result<u64>>,
}

impl AttachmentOffer {
    /// Explicitly requests activation and waits for the worker's result. Once
    /// confirmation is accepted, dropping the reply waiter cannot undo it.
    pub(crate) async fn confirm(self) -> Result<u64> {
        self.confirm.send(()).map_err(|_| unavailable())?;
        self.accepted.await.map_err(|_| unavailable())?
    }
}

/// Creates finite ingress without starting timers, tasks, transport or vendor work.
pub(crate) fn channel() -> (Ingress, Inputs) {
    let (sender, receiver) = mpsc::channel(CAPACITY);
    let (lifetime, ended) = watch::channel(());
    (Ingress(sender, lifetime), Inputs(receiver, ended))
}

/// Generic failure contains no callback content, credentials or supplied paths.
fn unavailable() -> MezError {
    MezError::invalid_state("Pi session telemetry unavailable")
}

/// Bounds untrusted session spelling before it can occupy the callback queue.
fn session_valid(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

impl Ingress {
    /// Wakes an inherited observer bridge when its session worker releases input.
    pub(super) async fn closed(&self) {
        self.0.closed().await;
    }

    /// Attempts one callback admission without awaiting IPC. Full/closed ingress
    /// loses telemetry explicitly; callers must not alter vendor decisions.
    pub(crate) fn observe(&self, epoch: u64, session: &str, fact: Observation) -> Result<()> {
        if epoch == 0 || !session_valid(session) {
            return Err(unavailable());
        }
        self.0
            .try_send(Input::Observation {
                epoch,
                session: session.into(),
                fact,
            })
            .map_err(|_| unavailable())
    }

    /// Requests an unconfirmed same-session replacement proposal without awaiting
    /// IPC. Receiving or abandoning this proposal cannot activate an observer;
    /// the launcher must explicitly confirm it before using the returned epoch.
    pub(crate) fn attach_after_reload(
        &self,
        session: &str,
    ) -> Result<oneshot::Receiver<Result<AttachmentOffer>>> {
        if !session_valid(session) {
            return Err(unavailable());
        }
        let (reply, receiver) = oneshot::channel();
        self.0
            .try_send(Input::Attach {
                session: session.into(),
                reply,
            })
            .map_err(|_| unavailable())?;
        Ok(receiver)
    }
}

/// Drives callback delivery and idle renewal without detached tasks. The caller
/// retains owner state after return; no failed exchange is automatically replayed.
pub(crate) async fn run(
    owner: &mut LifecycleOwner,
    transport: &CapabilityTransport,
    name: &str,
    inputs: Inputs,
    stop: watch::Receiver<bool>,
) -> Result<()> {
    run_with_clock(owner, transport, name, inputs, stop, || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|time| time.as_secs())
    })
    .await
}

/// Shares the complete worker with a per-fixture clock, never global mutation.
async fn run_with_clock(
    owner: &mut LifecycleOwner,
    transport: &CapabilityTransport,
    name: &str,
    mut inputs: Inputs,
    mut stop: watch::Receiver<bool>,
    clock: impl Fn() -> Option<u64>,
) -> Result<()> {
    if !transport.belongs_to(owner) {
        return Err(unavailable());
    }
    let (status, mut lease) = watch::channel::<Option<ActiveLease>>(None);
    let renewal = pi_renewal::run_with_clock(transport, name, status, stop.clone(), clock);
    tokio::pin!(renewal);
    // Wait for acknowledged registration, never send presentation before it.
    loop {
        tokio::select! {
            biased;
            () = pi_renewal::cancelled(&mut stop) => return Ok(()),
            _ = inputs.1.changed() => return Ok(()),
            result = &mut renewal => return result,
            changed = lease.changed() => {
                changed.map_err(|_| unavailable())?;
                if lease.borrow_and_update().as_ref().is_some_and(ActiveLease::is_current) { break; }
            }
        }
    }
    loop {
        let active = lease
            .borrow()
            .clone()
            .filter(ActiveLease::is_current)
            .ok_or_else(unavailable)?;
        if let Some(head) = owner.pending() {
            let retirement = head.operation == Operation::Retire;
            tokio::select! {
                biased;
                () = pi_renewal::cancelled(&mut stop) => return Ok(()),
                _ = inputs.1.changed() => return Ok(()),
                result = &mut renewal => return result,
                result = tokio::time::timeout_at(active.deadline(), transport.deliver_next(owner)) => {
                    result.map_err(|_| unavailable())??;
                }
            }
            if retirement {
                return Ok(());
            }
            continue;
        }
        let input = tokio::select! {
            biased;
            () = pi_renewal::cancelled(&mut stop) => return Ok(()),
            _ = inputs.1.changed() => return Ok(()),
            result = &mut renewal => return result,
            () = tokio::time::sleep_until(active.deadline()) => return Err(unavailable()),
            changed = lease.changed() => {
                changed.map_err(|_| unavailable())?;
                lease.borrow_and_update();
                continue;
            }
            input = inputs.0.recv() => input,
        };
        match input {
            None => return Ok(()), // launcher/observer channel lifetime ended
            Some(Input::Observation {
                epoch,
                session,
                fact,
            }) => {
                // Stale facts lose telemetry, not the healthy registration or
                // unrelated current callbacks. No guessed epoch can be promoted.
                if owner.observe(epoch, &session, fact).is_err() {
                    continue;
                }
            }
            Some(Input::Attach { session, reply }) => {
                if reply.is_closed() {
                    continue;
                }
                if !owner.can_attach_after_reload(&session) {
                    let _ = reply.send(Err(unavailable()));
                    continue;
                }
                let (confirm, mut confirmation) = oneshot::channel();
                let (accepted, receipt) = oneshot::channel();
                if reply
                    .send(Ok(AttachmentOffer {
                        confirm,
                        accepted: receipt,
                    }))
                    .is_err()
                {
                    continue;
                }
                let confirmed = loop {
                    let current = lease
                        .borrow()
                        .clone()
                        .filter(ActiveLease::is_current)
                        .ok_or_else(unavailable)?;
                    tokio::select! {
                        biased;
                        () = pi_renewal::cancelled(&mut stop) => return Ok(()),
                        _ = inputs.1.changed() => return Ok(()),
                        result = &mut renewal => return result,
                        changed = lease.changed() => {
                            changed.map_err(|_| unavailable())?;
                            lease.borrow_and_update();
                        }
                        () = tokio::time::sleep_until(current.deadline()) => return Err(unavailable()),
                        result = &mut confirmation => break result.is_ok(),
                    }
                };
                if confirmed {
                    let _ = accepted.send(owner.attach_after_reload(&session));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
