//! Parent-only Pi session replacement over authenticated private observation IPC.
//!
//! Child session/epoch facts are proposals. FIFO retirement of the previous
//! immutable binding precedes fresh primary issuance with its exact-root witness.
//! Reload only confirms a same-session observer epoch. Failed framing, retirement
//! or reauthorization ends telemetry, never restarts the vendor or reuses retired
//! credentials. One coordinator is polled at a time; no tasks are detached.

use super::*;
use crate::integrations::bootstrap::{
    pi::Observation,
    pi_binding::{Frame, Reader},
};

/// Exact parent-owned authority and immutable session state across transitions.
/// No Debug/serialization; credentials never reach child observers.
pub(super) struct Context {
    socket: PathBuf,
    pane: String,
    version: String,
    owner: pi_owner::LifecycleOwner,
    transport: pi_transport::CapabilityTransport,
    generation: u64,
    activated: bool,
}

impl Context {
    /// Captures explicit launch authority without registering a vendor session.
    pub(super) fn new(
        socket: PathBuf,
        pane: String,
        version: String,
        session: &str,
        grant: Grant,
    ) -> Result<Self> {
        let owner = pi_owner::LifecycleOwner::new(session)?;
        let generation = grant.generation;
        let transport = pi_transport::CapabilityTransport::new(
            &socket,
            grant.launch_token,
            generation,
            &owner,
        )?;
        Ok(Self {
            socket,
            pane,
            version,
            owner,
            transport,
            generation,
            activated: false,
        })
    }

    /// Best-effort exact cleanup only after matched start activation. This cannot
    /// imply acknowledgment and is never retried after an ambiguous exchange.
    pub(super) async fn retire(&self) {
        if self.activated {
            let _ = self.transport.retire().await;
        }
    }

    /// Drives initial activation, reload and separately authorized replacements.
    /// Silent/disabled extensions produce no registration, renewal or retirement.
    pub(super) async fn run(
        &mut self,
        stream: tokio::net::UnixStream,
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        let mut reader = Reader::new(stream)?;
        let mut start = tokio::select! {
            biased;
            () = crate::integrations::bootstrap::pi_renewal::cancelled(&mut stop) => return Ok(()),
            frame = reader.next() => frame?.ok_or_else(unavailable)?,
        };
        if start.session != self.owner.transport_binding().0
            || start.epoch != 1
            || start.fact != (Observation::SessionStarted { reason: "startup" })
        {
            return Err(unavailable());
        }
        loop {
            let (reason, epoch) = self.active(&mut reader, start, stop.clone()).await?;
            let Some(reason) = reason else {
                return Ok(());
            };
            start = tokio::select! {
                biased;
                () = crate::integrations::bootstrap::pi_renewal::cancelled(&mut stop) => return Ok(()),
                frame = reader.next() => frame?.ok_or_else(unavailable)?,
            };
            if start.epoch != epoch.checked_add(1).ok_or_else(unavailable)?
                || start.fact != (Observation::SessionStarted { reason })
            {
                return Err(unavailable());
            }
            // Same spelling is legitimate when explicitly resuming that session,
            // but always creates a fresh generation/owner after retirement.
            let grant = tokio::select! {
                biased;
                () = crate::integrations::bootstrap::pi_renewal::cancelled(&mut stop) => return Ok(()),
                grant = authorize_root(&self.socket, &self.pane, &self.version, Some(self.generation)) => grant?,
            };
            self.owner = pi_owner::LifecycleOwner::new(&start.session)?;
            self.generation = grant.generation;
            self.transport = pi_transport::CapabilityTransport::new(
                &self.socket,
                grant.launch_token,
                grant.generation,
                &self.owner,
            )?;
            self.activated = false;
        }
    }

    /// Polls one immutable lifecycle/renewal worker alongside its sole reader.
    /// New-session shutdown waits for acknowledged retirement before returning
    /// its transition reason; data following it remains in the owned stream.
    async fn active(
        &mut self,
        reader: &mut Reader,
        start: Frame,
        stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(Option<&'static str>, u64)> {
        let session = start.session.clone();
        let mut child_epoch = start.epoch;
        let mut parent_epoch = self.owner.observer_epoch();
        let (ingress, inputs) = pi_session::channel();
        ingress.observe(parent_epoch, &session, start.fact)?;
        self.activated = true;
        let worker = pi_session::run(&mut self.owner, &self.transport, "pi", inputs, stop);
        tokio::pin!(worker);
        let mut reload = false;
        loop {
            let frame = tokio::select! {
                result = &mut worker => { result?; return Ok((None, child_epoch)); }
                frame = reader.next() => frame?,
            };
            let Some(frame) = frame else {
                let ended = ingress.observer_ended();
                tokio::pin!(ended);
                tokio::select! {
                    result = &mut worker => { result?; }
                    result = &mut ended => { result?; worker.await?; }
                }
                return Ok((None, child_epoch));
            };
            if reload {
                if frame.epoch == child_epoch {
                    continue;
                } // old observer cannot revive
                if frame.session != session
                    || frame.epoch != child_epoch.checked_add(1).ok_or_else(unavailable)?
                    || frame.fact != (Observation::SessionStarted { reason: "reload" })
                {
                    return Err(unavailable());
                }
                let proposal = ingress.attach_after_reload(&session)?;
                let confirm = async { proposal.await.map_err(|_| unavailable())??.confirm().await };
                tokio::pin!(confirm);
                parent_epoch = tokio::select! {
                    result = &mut worker => { result?; return Err(unavailable()); }
                    result = &mut confirm => result?,
                };
                child_epoch = frame.epoch;
                reload = false;
            } else if frame.epoch < child_epoch {
                continue;
            } else if frame.session != session
                || frame.epoch != child_epoch
                || matches!(frame.fact, Observation::SessionStarted { .. })
            {
                return Err(unavailable());
            }
            let shutdown = match frame.fact {
                Observation::SessionShutdown { reason } => Some(reason),
                _ => None,
            };
            ingress.observe(parent_epoch, &session, frame.fact)?;
            match shutdown {
                Some("reload") => reload = true,
                Some(reason @ ("new" | "resume" | "fork")) => {
                    self.activated = false; // this worker owns the sole retirement attempt
                    worker.await?; // exact old retirement must acknowledge
                    return Ok((Some(reason), child_epoch));
                }
                Some("quit") => {
                    self.activated = false;
                    worker.await?;
                    return Ok((None, child_epoch));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests;
