//! Trusted original-root elapsed time over the existing message owner.
//!
//! Saved source clock domains are descriptive. Only an actual host root with
//! the exact live storage scope installs a clock reader; generic restoration
//! cannot select one. All fresh timing decisions use that reader and refuse
//! unavailable time. Historical offers/ACKs/terminal attempts keep their frozen
//! numeric clock and never resample during positive adoption or reconciliation.

use super::*;

/// Suspend-inclusive logical milliseconds supplied by the trusted original store.
/// The host owns persistent boot anchors and fail-closed native qualification.
pub trait MessageStorageClock: std::fmt::Debug + Send + Sync {
    /// Returns original-domain elapsed time, or an error without a UTC fallback.
    fn millis(&self) -> Result<u64>;
}

/// Runtime-only clock capability and the descriptive domain it qualifies.
#[derive(Debug)]
pub(super) struct ElapsedClock {
    pub(super) domain: [u8; 16],
    pub(super) reader: Arc<dyn MessageStorageClock>,
}

/// Conservative immutable domain and per-original bookkeeping charge.
pub(in crate::messaging) const CLOCK_METADATA_BYTES: usize = 128;

impl MessageService {
    /// Installs only on an idle, guarded original owner. Recovered originals must all
    /// name this exact domain; missing legacy timing cannot be silently renewed.
    /// A domain alone never authorizes a clock, and installed readers cannot be
    /// replaced after failure. Returns InvalidState for mismatched or live work.
    pub fn install_storage_clock(
        &mut self,
        scope: &MessageStorageScope,
        domain: [u8; 16],
        reader: Arc<dyn MessageStorageClock>,
    ) -> Result<()> {
        let policy = self.storage_custody.as_ref().ok_or_else(unavailable)?;
        if &policy.scope != scope
            || !policy.native_continuity_required
            || policy.elapsed_clock.is_some()
            || policy
                .decoded_clock
                .is_some_and(|decoded| decoded != domain)
            || self.admission.is_some()
            || self.acknowledgement.is_some()
            || self.terminal_commit.is_some()
            || domain == [0; 16]
            || self
                .queued_by_sequence
                .values()
                .any(|row| row.custody_clock != Some(domain))
        {
            return Err(unavailable());
        }
        valid_millis(reader.millis()?)?;
        self.storage_custody
            .as_mut()
            .ok_or_else(unavailable)?
            .elapsed_clock = Some(ElapsedClock { domain, reader });
        Ok(())
    }

    /// Selects actual trusted elapsed time in durable mode. Inert clocked rows
    /// without a qualified reader fail closed instead of interpreting wall time.
    pub(in crate::messaging) fn custody_now(&self, fallback: u64) -> Result<u64> {
        match self
            .storage_custody
            .as_ref()
            .and_then(|policy| policy.elapsed_clock.as_ref())
        {
            Some(clock) => valid_millis(clock.reader.millis()?),
            None if self
                .storage_custody
                .as_ref()
                .is_some_and(|policy| policy.decoded_clock.is_some()) =>
            {
                Err(unavailable())
            }
            None => Ok(fallback),
        }
    }

    /// Returns the clock domain's current observation for host deadline scheduling.
    /// The original opaque scope must match; no serialized clock selects policy.
    pub fn storage_observation_time(
        &self,
        scope: &MessageStorageScope,
        fallback: u64,
    ) -> Result<u64> {
        if self
            .storage_custody
            .as_ref()
            .is_none_or(|policy| &policy.scope != scope)
        {
            return Err(unavailable());
        }
        self.custody_now(fallback)
    }

    /// Captures descriptive clock attribution only from the live trusted policy.
    pub(in crate::messaging) fn custody_clock_domain(&self) -> Option<[u8; 16]> {
        self.storage_custody
            .as_ref()?
            .elapsed_clock
            .as_ref()
            .map(|clock| clock.domain)
    }
}

/// Keep arithmetic within the SQLite clock range with finite-window headroom.
fn valid_millis(now: u64) -> Result<u64> {
    if now == 0 || now > (i64::MAX as u64) - 60_000 {
        return Err(unavailable());
    }
    Ok(now)
}

fn unavailable() -> MessageError {
    MessageError::invalid_state("MMP original elapsed clock unavailable")
}
