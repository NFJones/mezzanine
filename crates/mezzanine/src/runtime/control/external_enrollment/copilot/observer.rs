//! Finite concrete extension proof beside classic Copilot activity.
//!
//! A nonce narrows an instance only after the common native writer/parent/root
//! pipeline. One retained kernel origin and birth record own its counter. A new
//! connection may continue the same living process; another child cannot borrow
//! its nonce or replace it while alive. Dead owners permit a new first counter,
//! never a new registration or restored source epoch. Replay is time-inert.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// One bounded original extension lifetime; no socket-health inference is used.
#[derive(Debug, Default)]
pub(super) struct CopilotObserver {
    owner: Option<ObserverOwner>,
}

/// Retains a native process anchor independently of short-lived RPC sockets.
#[derive(Debug)]
struct ObserverOwner {
    origin: Arc<UnixOriginProcess>,
    nonce: String,
    sequence: u64,
    observed_at: Instant,
    /// One-way logical lifetime; native process survival cannot revive it.
    valid: Arc<AtomicBool>,
}

impl Drop for ObserverOwner {
    /// Epoch replacement or owner destruction revokes every queued snapshot.
    fn drop(&mut self) {
        self.valid.store(false, Ordering::Release);
    }
}

/// Frozen existing instance selector. Only worker reobservation supplies proof;
/// neither the nonce nor this snapshot authorizes a different native writer.
#[derive(Debug, Clone)]
pub(in crate::runtime::control::external_enrollment) struct CopilotInstance {
    /// Existing kernel lifetime anchor, not a newly claimed process ID.
    origin: Arc<UnixOriginProcess>,
    /// Inert original instance selector, checked independently of native proof.
    nonce: String,
    /// Private code-owned generation witness, independent of transport anchors.
    valid: Arc<AtomicBool>,
}

impl CopilotInstance {
    /// Runs on the bounded native worker before any accounting dispatch.
    pub(in crate::runtime::control::external_enrollment) fn observe(
        &self,
        writer: &Arc<UnixOriginProcess>,
    ) -> Result<()> {
        let retained = self
            .origin
            .reobserve()
            .map_err(|_| MezError::forbidden("Copilot original observer unavailable"))?;
        let actual = writer
            .reobserve()
            .map_err(|_| MezError::forbidden("Copilot reporting writer unavailable"))?;
        if retained != actual
            || self.origin.uid() != writer.uid()
            || !self.origin.writer_confirmed()
            || !writer.writer_confirmed()
            || !self.valid.load(Ordering::Acquire)
        {
            return Err(MezError::forbidden("Copilot reporting instance changed"));
        }
        Ok(())
    }
}

impl CopilotObserver {
    /// Captures only an existing published owner without actor-side native I/O.
    pub(super) fn capture(&self, nonce: &str) -> Result<CopilotInstance> {
        let owner = self
            .owner
            .as_ref()
            .filter(|owner| owner.nonce == nonce && owner.valid.load(Ordering::Acquire))
            .ok_or_else(|| MezError::forbidden("Copilot concrete observer unavailable"))?;
        Ok(CopilotInstance {
            origin: owner.origin.clone(),
            nonce: nonce.into(),
            valid: owner.valid.clone(),
        })
    }

    /// Current native owner must still match after worker publication. Transport
    /// anchors may change on a real same-process reconnect; Arc equality is wrong.
    pub(super) fn matches(&self, instance: &CopilotInstance) -> bool {
        self.owner.as_ref().is_some_and(|owner| {
            owner.nonce == instance.nonce
                && owner.valid.load(Ordering::Acquire)
                && Arc::ptr_eq(&owner.valid, &instance.valid)
                && owner.origin.uid() == instance.origin.uid()
                && owner.origin.identity == instance.origin.identity
                && owner.origin.writer_confirmed()
        })
    }

    /// Revokes queued work before source retirement becomes visible. Native
    /// admission that already passed its final check retains original expense.
    pub(super) fn invalidate(&self) {
        if let Some(owner) = &self.owner {
            owner.valid.store(false, Ordering::Release);
        }
    }
    /// Fixture-only clock control; native owner/counter evidence stays intact.
    #[cfg(test)]
    pub(super) fn age_for_test(&mut self) {
        if let Some(owner) = &mut self.owner {
            owner.observed_at = Instant::now() - Duration::from_secs(31);
        }
    }

    /// Fixture-only observation of the genuine current counter and proof clock.
    #[cfg(test)]
    pub(super) fn snapshot_for_test(&self) -> Option<(u64, Instant)> {
        self.owner
            .as_ref()
            .map(|owner| (owner.sequence, owner.observed_at))
    }
    /// A changed positive proof expires after thirty seconds or native death.
    pub(super) fn is_recent(&self) -> bool {
        self.owner.as_ref().is_some_and(|owner| {
            owner.valid.load(Ordering::Acquire)
                && owner.origin.is_live()
                && owner.origin.writer_confirmed()
                && owner.observed_at.elapsed() < Duration::from_secs(30)
        })
    }

    /// A stale or dead observer is distinct from absence of any initial proof.
    pub(super) fn has_started(&self) -> bool {
        self.owner.is_some()
    }

    /// Publishes only after common actor authorization. Both retained native
    /// origins must still denote the same live PID/birth/parent identity for a
    /// reconnect; Arc equality and payload labels cannot establish that fact.
    /// Duplicate counters do not update the proof clock or any presentation.
    pub(super) fn observe(
        &mut self,
        origin: &Arc<UnixOriginProcess>,
        nonce: &str,
        sequence: u64,
    ) -> Result<String> {
        let current = origin
            .reobserve()
            .map_err(|_| MezError::forbidden("Copilot extension writer unavailable"))?;
        if !origin.writer_confirmed() {
            return Err(MezError::forbidden("Copilot extension writer unconfirmed"));
        }
        if let Some(owner) = &mut self.owner {
            if !owner.valid.load(Ordering::Acquire) {
                return Err(MezError::forbidden("Copilot observer instance revoked"));
            }
            if owner.origin.is_live() {
                if owner.nonce != nonce
                    || owner.origin.uid() != origin.uid()
                    || owner.origin.reobserve().ok() != Some(current)
                {
                    return Err(MezError::forbidden("Copilot extension lifetime changed"));
                }
                if sequence < owner.sequence {
                    return Err(MezError::conflict("Copilot observer counter stale"));
                }
                let changed = sequence > owner.sequence;
                if changed {
                    // Keep the current positively qualified transport anchor
                    // for this same native lifetime. A later poisoned old
                    // connection cannot supply or revoke this new proof.
                    owner.origin = origin.clone();
                    owner.sequence = sequence;
                    owner.observed_at = Instant::now();
                }
                return Ok(
                    serde_json::json!({"observed":true,"sequence":sequence,"changed":changed})
                        .to_string(),
                );
            }
            if owner.nonce == nonce {
                return Err(MezError::forbidden("Copilot prior observer instance ended"));
            }
        }
        if sequence != 1 {
            return Err(MezError::conflict(
                "Copilot observer requires first counter",
            ));
        }
        self.owner = Some(ObserverOwner {
            origin: origin.clone(),
            nonce: nonce.to_owned(),
            sequence,
            observed_at: Instant::now(),
            valid: Arc::new(AtomicBool::new(true)),
        });
        Ok(serde_json::json!({"observed":true,"sequence":sequence,"changed":true}).to_string())
    }
}
