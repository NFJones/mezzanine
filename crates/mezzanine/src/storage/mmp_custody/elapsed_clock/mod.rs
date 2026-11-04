//! Original-store elapsed clock anchor and transactional progress.
//!
//! The fixed checksummed state lives in the existing singleton, under the same
//! transaction as custody writes. Actual native qualification is required before
//! clocked writes or delivery. Same-boot restarts include suspend and daemon
//! downtime. A newly qualified boot starts at committed elapsed progress plus
//! its native uptime: unknown old-boot tail and powered-off time are excluded,
//! never guessed from UTC. Original acceptance and terminal deadlines do not
//! change. Saved boot UUIDs are descriptive, never native/registration rights.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use super::*;
use crate::host::native_clock::NativeBootClock;
use mez_agent::messaging::MessageStorageClock;

pub(super) const ENCODED_BYTES: usize = 116;
const MAGIC: &[u8; 8] = b"MMPCLK01";
const MAX_MILLIS: u64 = (i64::MAX as u64) - 60_000;

/// Trusted native-clock dependency, selected by host construction, never saved data.
#[derive(Clone, Default)]
pub(crate) enum ClockSource {
    #[default]
    Native,
    #[cfg(test)]
    Controlled(Arc<ControlledClock>),
}
impl ClockSource {
    pub(crate) fn capture(&self) -> Result<Arc<dyn NativeClock>> {
        match self {
            Self::Native => NativeBootClock::capture()
                .map(|clock| Arc::new(clock) as Arc<dyn NativeClock>)
                .ok_or_else(changed),
            #[cfg(test)]
            Self::Controlled(clock) => Ok(clock.clone()),
        }
    }
    /// Test-only elapsed source for generated crash fixtures, independent of UTC.
    #[cfg(test)]
    pub(crate) fn controlled_for_tests(native_ms: u64) -> Self {
        Self::Controlled(Arc::new(ControlledClock(AtomicU64::new(native_ms))))
    }
}
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ControlledClock(AtomicU64);
#[cfg(test)]
impl NativeClock for ControlledClock {
    fn boot_uuid(&self) -> &str {
        "01234567-89ab-cdef-0123-456789abcdef"
    }
    fn millis(&self) -> Result<u64> {
        let now = self.0.load(Ordering::Acquire);
        (now > 0).then_some(now).ok_or_else(changed)
    }
    fn invalidate_for_tests(&self) {
        self.0.store(0, Ordering::Release);
    }
    fn advance_for_tests(&self, now: u64) -> Result<()> {
        let previous = self.0.load(Ordering::Acquire);
        if previous == 0 || now < previous {
            return Err(changed());
        }
        self.0
            .compare_exchange(previous, now, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| changed())
    }
}

/// Actual boot-qualified clock input, injectable only by trusted host/test setup.
pub(crate) trait NativeClock: std::fmt::Debug + Send + Sync {
    fn boot_uuid(&self) -> &str;
    fn millis(&self) -> Result<u64>;
    #[cfg(test)]
    fn invalidate_for_tests(&self);
    #[cfg(test)]
    fn advance_for_tests(&self, _now: u64) -> Result<()> {
        Err(changed())
    }
}
impl NativeClock for NativeBootClock {
    fn boot_uuid(&self) -> &str {
        self.boot_uuid()
    }
    fn millis(&self) -> Result<u64> {
        self.millis().ok_or_else(changed)
    }
    #[cfg(test)]
    fn invalidate_for_tests(&self) {
        NativeBootClock::invalidate_for_tests(self);
    }
}

/// Descriptive anchor; immutable throughout one actual owned store opening.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClockState {
    boot: [u8; 36],
    native_origin: u64,
    logical_origin: u64,
    pub(super) progress: u64,
}
impl ClockState {
    /// Decodes exact bounded bytes tied to the existing physical allocation.
    pub(super) fn decode(bytes: &[u8], store: &[u8]) -> Result<Self> {
        if bytes.len() != ENCODED_BYTES
            || &bytes[..8] != MAGIC
            || &bytes[8..24] != store
            || Sha256::digest(&bytes[..84]).as_slice() != &bytes[84..]
        {
            return Err(changed());
        }
        let boot: [u8; 36] = bytes[24..60].try_into().map_err(|_| changed())?;
        canonical_boot(&boot)?;
        let read = |start| -> Result<u64> {
            Ok(u64::from_le_bytes(
                bytes[start..start + 8].try_into().map_err(|_| changed())?,
            ))
        };
        let state = Self {
            boot,
            native_origin: read(60)?,
            logical_origin: read(68)?,
            progress: read(76)?,
        };
        if state.native_origin > MAX_MILLIS
            || state.logical_origin == 0
            || state.logical_origin > state.progress
            || state.progress > MAX_MILLIS
        {
            return Err(changed());
        }
        Ok(state)
    }

    /// Encodes one fixed-size state, including allocation identity and checksum.
    pub(super) fn encode(&self, store: &[u8]) -> Result<Vec<u8>> {
        if store.len() != 16 {
            return Err(changed());
        }
        let mut bytes = Vec::with_capacity(ENCODED_BYTES);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(store);
        bytes.extend_from_slice(&self.boot);
        bytes.extend_from_slice(&self.native_origin.to_le_bytes());
        bytes.extend_from_slice(&self.logical_origin.to_le_bytes());
        bytes.extend_from_slice(&self.progress.to_le_bytes());
        bytes.extend_from_slice(&Sha256::digest(&bytes));
        Ok(bytes)
    }

    /// Progress may advance in custody transactions; anchor replacement may not.
    pub(super) fn same_anchor(&self, other: &Self) -> bool {
        self.boot == other.boot
            && self.native_origin == other.native_origin
            && self.logical_origin == other.logical_origin
    }

    fn at_native(&self, native: u64) -> Result<u64> {
        native
            .checked_sub(self.native_origin)
            .and_then(|delta| self.logical_origin.checked_add(delta))
            .filter(|logical| *logical >= self.progress && *logical <= MAX_MILLIS)
            .ok_or_else(changed)
    }
}

/// Nonserializable root capability shared by the worker and the lower owner.
#[derive(Debug)]
pub(crate) struct CustodyElapsedClock {
    domain: [u8; 16],
    observed: AtomicU64,
    anchor: ClockState,
    native: Arc<dyn NativeClock>,
    committed: AtomicU64,
}
impl MessageStorageClock for CustodyElapsedClock {
    fn millis(&self) -> mez_agent::messaging::Result<u64> {
        self.read().map_err(|error| {
            mez_agent::messaging::MessageError::invalid_state(format!("MMP elapsed clock: {error}"))
        })
    }
}
impl CustodyElapsedClock {
    pub(super) fn read(&self) -> Result<u64> {
        if self.native.boot_uuid().as_bytes() != self.anchor.boot {
            return Err(changed());
        }
        self.anchor
            .at_native(self.native.millis()?)
            .and_then(|now| {
                if now < self.committed.load(Ordering::Acquire) {
                    return Err(changed());
                }
                Ok(self.observed.fetch_max(now, Ordering::AcqRel).max(now))
            })
    }
    pub(super) fn observe_commit(&self, progress: u64) {
        self.committed.fetch_max(progress, Ordering::AcqRel);
        self.observed.fetch_max(progress, Ordering::AcqRel);
    }

    /// Advances only an explicitly controlled fixture dependency, never the OS.
    #[cfg(test)]
    fn advance_for_tests(&self, logical: u64) -> Result<()> {
        let native = logical
            .checked_sub(self.anchor.logical_origin)
            .and_then(|delta| delta.checked_add(self.anchor.native_origin))
            .ok_or_else(changed)?;
        self.native.advance_for_tests(native)?;
        self.read().map(|_| ())
    }

    #[cfg(test)]
    pub(crate) fn invalidate_for_tests(&self) {
        self.native.invalidate_for_tests();
    }

    /// Already-issued immutable custody may finish after native clock failure.
    /// Only actual last-good original observations can advance that commit; no
    /// new timing decision or fresh admission may use this settlement fallback.
    fn checkpoint_progress(&self) -> u64 {
        let _ = self.read();
        self.observed.load(Ordering::Acquire)
    }
}

impl MmpCustodyStore {
    /// Qualifies the original clock under an owned worker, before any new plan.
    /// Legacy nonempty stores refuse rather than acquiring a fresh TTL epoch.
    /// Reopening an already qualified store must keep its exact actual reader.
    pub(crate) fn qualify_elapsed_clock(
        &mut self,
        native: Arc<dyn NativeClock>,
    ) -> Result<Arc<CustodyElapsedClock>> {
        if self.elapsed_clock.is_some() {
            return Err(changed());
        }
        let boot: [u8; 36] = native
            .boot_uuid()
            .as_bytes()
            .try_into()
            .map_err(|_| changed())?;
        canonical_boot(&boot)?;
        let now = native.millis()?;
        if now == 0 || now > MAX_MILLIS {
            return Err(changed());
        }
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)
                .map_err(database_error)?;
        let state = self.checked_state(&transaction)?;
        let fingerprint = integrity::unaffected_rows_digest(&transaction, &[])?;
        let clock = match state.clock.clone() {
            Some(clock) if clock.boot == boot => {
                clock.at_native(now)?;
                clock
            }
            Some(clock) => ClockState {
                boot,
                native_origin: 0,
                logical_origin: clock.progress,
                progress: clock
                    .progress
                    .checked_add(now)
                    .filter(|value| *value <= MAX_MILLIS)
                    .ok_or_else(changed)?,
            },
            None => {
                let rows: i64 = transaction
                    .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
                    .map_err(database_error)?;
                if state.generation != 0
                    || state.watermark != 0
                    || rows != 0
                    || !state.watermark_origin_trusted
                {
                    return Err(changed());
                }
                ClockState {
                    boot,
                    native_origin: 0,
                    logical_origin: 1,
                    progress: now
                        .checked_add(1)
                        .filter(|value| *value <= MAX_MILLIS)
                        .ok_or_else(changed)?,
                }
            }
        };
        let encoded = clock.encode(&self.store_id)?;
        if state.clock.as_ref() != Some(&clock) {
            transaction
                .execute(
                    "UPDATE state SET elapsed_clock=?1 WHERE singleton=1",
                    [&encoded],
                )
                .map_err(database_error)?;
        }
        // Qualifying time must preserve every unrelated row and allocator fact.
        validate_totals(&transaction, self.limits)?;
        let observed = read_clock(&transaction, &self.store_id)?;
        if observed.as_ref() != Some(&clock) {
            return Err(changed());
        }
        let after = self.checked_state_at_clock(&transaction, Some(&clock))?;
        if after.generation != state.generation
            || after.watermark != state.watermark
            || after.batch != state.batch
            || after.watermark_origin_trusted != state.watermark_origin_trusted
            || integrity::unaffected_rows_digest(&transaction, &[])? != fingerprint
        {
            return Err(changed());
        }
        transaction.commit().map_err(database_error)?;
        self.clock_anchor = Some(clock.clone());
        self.committed_elapsed.set(clock.progress);
        let reader = Arc::new(CustodyElapsedClock {
            domain: self.store_id.as_slice().try_into().map_err(|_| changed())?,
            observed: AtomicU64::new(clock.progress),
            committed: AtomicU64::new(clock.progress),
            anchor: clock,
            native,
        });
        self.elapsed_clock = Some(reader.clone());
        reader.read()?;
        Ok(reader)
    }

    #[cfg(test)]
    pub(crate) fn advance_elapsed_clock_for_tests(&self, logical: u64) -> Result<()> {
        self.elapsed_clock
            .as_ref()
            .ok_or_else(changed)?
            .advance_for_tests(logical)
    }

    /// Injects native loss only for this fixture's retained actual clock.
    #[cfg(test)]
    pub(crate) fn invalidate_elapsed_clock_for_tests(&self) {
        self.elapsed_clock
            .as_ref()
            .expect("fixture clock must be qualified")
            .invalidate_for_tests();
    }

    /// Reattaches only the actual original reader retained by the kernel root
    /// after worker loss. Failed native sampling cannot mint another reader.
    pub(crate) fn attach_elapsed_clock(&mut self, reader: Arc<CustodyElapsedClock>) -> Result<()> {
        let state = self.checked_state(&self.connection)?;
        let clock = state.clock.ok_or_else(changed)?;
        if self.store_id.as_slice() != reader.domain || !clock.same_anchor(&reader.anchor) {
            return Err(changed());
        }
        reader.observe_commit(clock.progress);
        self.elapsed_clock = Some(reader);
        Ok(())
    }

    /// Installs actual root time in the canonical lower owner after cold fold or
    /// fresh spill setup. Saved clock domains only validate descriptive lineage.
    pub(crate) fn install_elapsed_clock(
        &self,
        service: &mut mez_agent::messaging::MessageService,
        scope: &mez_agent::messaging::MessageStorageScope,
    ) -> Result<()> {
        if let Some(reader) = &self.elapsed_clock {
            service.require_native_continuity(scope).map_err(|error| {
                MezError::invalid_state(format!("MMP timed custody barrier: {error}"))
            })?;
            service
                .install_storage_clock(scope, reader.domain, reader.clone())
                .map_err(|error| {
                    MezError::invalid_state(format!("MMP elapsed clock install: {error}"))
                })?;
        } else if self.clock_anchor.is_some() {
            return Err(changed());
        }
        Ok(())
    }

    /// Calculates new state only for a genuinely fresh physical transaction.
    pub(super) fn advance_clock(&self, state: &CustodyState) -> Result<Option<ClockState>> {
        match &state.clock {
            None => Ok(None),
            Some(before) => {
                let now = self
                    .elapsed_clock
                    .as_ref()
                    .ok_or_else(changed)?
                    .checkpoint_progress();
                let mut after = before.clone();
                after.progress = now.max(before.progress);
                Ok(Some(after))
            }
        }
    }
}

/// Reads a fixed optional blob without materializing an oversized SQL value.
pub(super) fn read_clock(connection: &Connection, store: &[u8]) -> Result<Option<ClockState>> {
    let (present, bytes): (bool, Option<Vec<u8>>) = connection.query_row(
        "SELECT elapsed_clock IS NOT NULL,CASE WHEN length(elapsed_clock)=116 THEN elapsed_clock ELSE NULL END FROM state WHERE singleton=1",
        [], |row| Ok((row.get(0)?, row.get(1)?))).map_err(database_error)?;
    match (present, bytes) {
        (false, None) => Ok(None),
        (true, Some(bytes)) => ClockState::decode(&bytes, store).map(Some),
        _ => Err(changed()),
    }
}

/// Native boot text must already have the fixed lowercase kernel UUID spelling.
fn canonical_boot(boot: &[u8; 36]) -> Result<()> {
    if boot.iter().enumerate().all(|(index, byte)| {
        if [8, 13, 18, 23].contains(&index) {
            *byte == b'-'
        } else {
            byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
        }
    }) {
        Ok(())
    } else {
        Err(changed())
    }
}
fn changed() -> MezError {
    MezError::invalid_state("MMP original elapsed clock qualification unavailable")
}

#[cfg(test)]
mod tests;
