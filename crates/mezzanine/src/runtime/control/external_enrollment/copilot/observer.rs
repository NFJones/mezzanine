//! Finite concrete extension proof beside classic Copilot activity.
//!
//! A nonce narrows an instance only after the common native writer/parent/root
//! pipeline. One retained kernel origin and birth record own its counter. A new
//! connection may continue the same living process; another child cannot borrow
//! its nonce or replace it while alive. Dead owners permit a new first counter,
//! never a new registration or restored source epoch. Replay is time-inert.

use super::*;

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
}

impl CopilotObserver {
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
            owner.origin.is_live()
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
        });
        Ok(serde_json::json!({"observed":true,"sequence":sequence,"changed":true}).to_string())
    }
}
