//! Original-store facts for native registration continuity.
//!
//! Each bounded record captures an actual registration namespace, shell epoch,
//! immutable identity and interval floor. Updates compare the exact original
//! preimage/position and reconcile only their own last committed batch.
//! Guarded lifetime provenance is retained in the same transaction before latest
//! n1 changes. These facts describe original namespace/epochs, never retirement.
//! All unrelated rows remain bound to the original input through trigger effects and
//! retries; any mutation rolls the attempted write back. A floor
//! reserves allocator gaps atomically without accepting a message. Readers return
//! quarantined observations, never native restore/registration authority. Product
//! lifecycle publication and private-context qualification remain separate gates.

use std::io::{self, Write};
use std::sync::{Arc, OnceLock};

mod lifetime;

use mez_agent::messaging::{
    MessageNativeContinuitySnapshot, MessageNativeContinuityStore, MessageNativeContinuityWrite,
};
use serde::de::DeserializeSeed;
use serde::{Deserialize, Serialize, de};

use super::{integrity::unaffected_rows_digest, *};

/// Leaves room for framing/key/preimage inside one existing bounded batch.
const MAX_RECORD: usize = 240 * 1024;
/// Structural serde preflight bounds decoded containers before retaining them.
const MAX_DECODE_METADATA: usize = 512 * 1024;

/// Exact original position and preimage retained before any native write.
#[derive(Clone)]
pub(crate) struct NativeContinuityPlan {
    store_id: Vec<u8>,
    generation: u64,
    watermark: u64,
    key: String,
    previous: Option<[u8; 32]>,
    /// All non-target rows at the original position, retained across retries.
    unaffected: [u8; 32],
    /// Exact first history targets/input outside physical worker uncertainty.
    lifetime: Arc<OnceLock<lifetime::LifetimePlan>>,
}
impl NativeContinuityPlan {
    /// Rechecks the exact one-batch native post-position after owned I/O.
    /// Later unrelated generations cannot be adopted as this original result.
    pub(crate) fn validate_settled(&self, store: &MmpCustodyStore, floor: u64) -> Result<()> {
        let state = store.current_state()?;
        if self.store_id != store.store_id
            || self.generation.checked_add(1) != Some(state.generation as u64)
            || state.watermark != self.watermark.max(floor)
        {
            return Err(invalid("settled original position changed"));
        }
        Ok(())
    }
}

/// Non-authorizing, versioned record bound to the actual original allocation.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredNativeContinuity {
    format_version: u32,
    store_id: [u8; 16],
    /// Original native publication generation; legacy observations lack it.
    #[serde(default)]
    pub generation: Option<u64>,
    /// Exact observed interval; neither these fields nor matching labels revive it.
    pub snapshot: MessageNativeContinuitySnapshot,
}
impl StoredNativeContinuity {
    /// Returns a guarded publication observation only for the new framing.
    /// The generation still needs actual original-store/private-context proof;
    /// this method alone never returns a native registration or shell token.
    pub(crate) fn publication_generation(&self) -> Option<u64> {
        (self.format_version == 2 && self.snapshot.admission_guarded)
            .then_some(self.generation)
            .flatten()
    }
}

/// Borrowed serializer avoids deep-copying the original captured metadata.
#[derive(Serialize)]
struct RecordRef<'a> {
    format_version: u32,
    store_id: &'a [u8],
    generation: u64,
    snapshot: &'a MessageNativeContinuitySnapshot,
}

/// Borrowed trusted write port; the actual root retains its plan through loss.
pub(crate) struct NativeContinuityPort<'a> {
    store: &'a mut MmpCustodyStore,
    plan: &'a NativeContinuityPlan,
}

impl MmpCustodyStore {
    /// Captures the exact row preimage under the original settled position.
    /// Corrupt or cross-allocation prior facts cannot be silently overwritten.
    pub(crate) fn native_continuity_plan(&self, agent_id: &str) -> Result<NativeContinuityPlan> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let state = self.checked_state(&transaction)?;
        let key = record_key(agent_id);
        let previous = match self.read(&key)? {
            None => None,
            Some(bytes) => {
                let observation = self.decode_native_record(
                    &key,
                    &bytes,
                    state.watermark,
                    state.generation as u64,
                )?;
                self.validate_native_lifetime_observation(&transaction, &observation)?;
                Some(Sha256::digest(&bytes).into())
            }
        };
        let unaffected = unaffected_rows_digest(&transaction, &[&key])?;
        self.finish_transaction(transaction)?;
        Ok(NativeContinuityPlan {
            store_id: self.store_id.clone(),
            generation: state.generation as u64,
            watermark: state.watermark,
            key,
            previous,
            unaffected,
            lifetime: Arc::new(OnceLock::new()),
        })
    }

    /// Borrows only the supplied immutable original write plan.
    pub(crate) fn native_continuity_port<'a>(
        &'a mut self,
        plan: &'a NativeContinuityPlan,
    ) -> NativeContinuityPort<'a> {
        NativeContinuityPort { store: self, plan }
    }

    /// Reads nonauthorizing facts under one checked original-store snapshot.
    pub(crate) fn read_native_continuity(
        &self,
        agent_id: &str,
    ) -> Result<Option<StoredNativeContinuity>> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let result =
            self.read_native_continuity_in_snapshot(&transaction, &record_key(agent_id))?;
        self.finish_transaction(transaction)?;
        Ok(result)
    }

    /// Shares the caller's complete inventory/recovery read snapshot.
    pub(super) fn read_native_continuity_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
    ) -> Result<Option<StoredNativeContinuity>> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid("snapshot belongs to another connection"));
        }
        let state = self.checked_state(transaction)?;
        let record = self
            .read(key)?
            .map(|bytes| {
                self.decode_native_record(key, &bytes, state.watermark, state.generation as u64)
            })
            .transpose()?;
        if let Some(record) = &record {
            self.validate_native_lifetime_observation(transaction, record)?;
        }
        Ok(record)
    }

    /// Checks record framing, bounded decoded size, original allocation and key
    /// before exposing descriptive fields. No live token constructor is called.
    fn decode_native_record(
        &self,
        key: &str,
        bytes: &[u8],
        watermark: u64,
        generation: u64,
    ) -> Result<StoredNativeContinuity> {
        preflight(bytes)?;
        let record: StoredNativeContinuity =
            serde_json::from_slice(bytes).map_err(|_| invalid("record framing"))?;
        record
            .snapshot
            .validate()
            .map_err(|_| invalid("native tuple or identity"))?;
        if !matches!(record.format_version, 1 | 2)
            || match record.format_version {
                1 => record.generation.is_some() || record.snapshot.format_version != 1,
                2 => {
                    record.generation.is_none_or(|g| g == 0 || g > generation)
                        || record.snapshot.format_version != 2
                }
                _ => true,
            }
            || record.store_id.as_slice() != self.store_id
            || record_key(&record.snapshot.native.agent_id) != key
            || record.snapshot.registered_after_sequence > watermark
        {
            return Err(invalid("allocation, key or allocator floor"));
        }
        Ok(record)
    }
}

impl MessageNativeContinuityStore for NativeContinuityPort<'_> {
    /// Persists one captured registration and allocator floor atomically. The
    /// exact last batch is retryable after a lost positive; stale plans refuse.
    fn commit(&mut self, write: &MessageNativeContinuityWrite) -> mez_agent::messaging::Result<()> {
        self.persist(write).map_err(|e| {
            mez_agent::messaging::MessageError::invalid_state(format!(
                "MMP native continuity storage: {e}"
            ))
        })
    }
}
impl NativeContinuityPort<'_> {
    /// Rechecks the original allocation/position/preimage inside its actual
    /// transaction before applying or reconciling the original bounded batch.
    fn persist(&mut self, write: &MessageNativeContinuityWrite) -> Result<()> {
        let snapshot = write.snapshot();
        snapshot.validate().map_err(|_| invalid("captured tuple"))?;
        if self.plan.store_id != self.store.store_id
            || record_key(&snapshot.native.agent_id) != self.plan.key
        {
            return Err(invalid("original plan changed"));
        }
        let record = RecordRef {
            format_version: 2,
            store_id: &self.plan.store_id,
            generation: self
                .plan
                .generation
                .checked_add(1)
                .ok_or_else(|| invalid("generation overflow"))?,
            snapshot,
        };
        let mut size = ByteCount(0);
        serde_json::to_writer(&mut size, &record).map_err(|_| invalid("record size"))?;
        if size.0 > MAX_RECORD {
            return Err(invalid("record capacity"));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size.0)
            .map_err(|_| invalid("record allocation"))?;
        serde_json::to_writer(&mut bytes, &record).map_err(|_| invalid("record encoding"))?;
        preflight(&bytes)?;
        let transaction = self
            .store
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let state = self.store.checked_state(&transaction)?;
        if self.store.native_registration_has_ended(
            &transaction,
            &snapshot.native,
            snapshot.registered_after_sequence,
            None,
        )? {
            return Err(invalid("captured native registration has ended"));
        }
        if self.plan.lifetime.get().is_none() {
            let captured = lifetime::LifetimePlan::capture(
                self.store,
                &transaction,
                self.plan,
                snapshot,
                &bytes,
            )?;
            self.plan
                .lifetime
                .set(captured)
                .map_err(|_| invalid("lifetime plan already selected"))?;
        }
        let lifetime = self
            .plan
            .lifetime
            .get()
            .ok_or_else(|| invalid("lifetime plan missing"))?;
        if !lifetime.matches_input(&bytes) {
            return Err(invalid("lifetime original input changed"));
        }
        lifetime.revalidate(self.store, &transaction, &self.plan.key, false)?;
        if state.watermark < self.plan.watermark {
            return Err(invalid("watermark rollback"));
        }
        let mut writes = vec![CustodyWrite {
            key: &self.plan.key,
            previous: self.plan.previous.as_ref().map(|s| s.as_slice()),
            value: Some(&bytes),
        }];
        writes.extend(lifetime.writes());
        self.store.commit_in_transaction_with_floor(
            &transaction,
            self.plan.generation,
            &writes,
            snapshot.registered_after_sequence,
        )?;
        if self.store.read(&self.plan.key)?.as_deref() != Some(bytes.as_slice()) {
            return Err(invalid("native target changed during SQL effects"));
        }
        lifetime.revalidate(self.store, &transaction, &self.plan.key, true)?;
        self.store.finish_transaction(transaction)
    }
}

/// Exact content-addressed agent key, never a pathname or reconnect credential.
pub(super) fn record_key(agent_id: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut key = String::with_capacity(67);
    key.push_str("n1:");
    for byte in Sha256::digest(agent_id.as_bytes()) {
        key.push(HEX[(byte >> 4) as usize] as char);
        key.push(HEX[(byte & 15) as usize] as char);
    }
    key
}

/// Counts structural metadata before serde allocates its owned DTO containers.
fn preflight(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_RECORD {
        return Err(invalid("record capacity"));
    }
    let mut budget = MAX_DECODE_METADATA;
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    Count(&mut budget)
        .deserialize(&mut decoder)
        .map_err(|_| invalid("decoded metadata capacity or framing"))?;
    decoder.end().map_err(|_| invalid("trailing record bytes"))
}
/// Nonretaining JSON visitor with conservative per-node/container/string charge.
struct Count<'a>(&'a mut usize);
impl<'de> de::DeserializeSeed<'de> for Count<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        decoder: D,
    ) -> std::result::Result<(), D::Error> {
        *self.0 = self
            .0
            .checked_sub(128)
            .ok_or_else(|| de::Error::custom("native metadata capacity"))?;
        decoder.deserialize_any(self)
    }
}
impl<'de> de::Visitor<'de> for Count<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded native continuity JSON")
    }
    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        while map.next_key_seed(Count(self.0))?.is_some() {
            map.next_value_seed(Count(self.0))?;
        }
        Ok(())
    }
    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
        while seq.next_element_seed(Count(self.0))?.is_some() {}
        Ok(())
    }
    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<(), E> {
        *self.0 = self
            .0
            .checked_sub(value.len().saturating_mul(4))
            .ok_or_else(|| E::custom("native string capacity"))?;
        Ok(())
    }
    fn visit_string<E: de::Error>(self, value: String) -> std::result::Result<(), E> {
        self.visit_str(&value)
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<(), E> {
        Ok(())
    }
}
/// Counts encoding bytes without allocating another metadata projection.
struct ByteCount(usize);
impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("native record size"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Content-free integrity/capacity diagnostic.
fn invalid(detail: &str) -> MezError {
    MezError::invalid_state(format!("MMP native continuity {detail}"))
}

#[cfg(test)]
mod tests;
