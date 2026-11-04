//! Immutable guarded native lifetime provenance, separate from latest presence.
//!
//! Latest `n1` may switch to an ephemeral child and back. Losing its previous
//! namespace/epoch would make later explicit registration retirement impossible
//! to qualify against old source owner hashes. Bounded `b1` facts preserve only
//! the tuple and its first actual guarded publication, never availability,
//! retirement, a live shell token or permission to restore a task. The original
//! n1 transaction freezes all history preimages and preserves the same batch
//! through joined loss. Missing overwritten legacy history is not invented.

use super::*;
use mez_agent::messaging::{MessageIdentitySnapshot, MessageNativeCheckpoint};

/// Two such records plus the existing 240KiB n1 fit one 256KiB write batch.
const MAX_LIFETIME_RECORD: usize = 4096;

/// Descriptive namespace provenance; construction supplies no live capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredNativeLifetime {
    format_version: u32,
    store_id: [u8; 16],
    /// First guarded publication known from an actual n1 input or prior n1 row.
    pub publication_generation: u64,
    /// Original registration interval floor, never a delivery or ACK cursor.
    pub registered_after_sequence: u64,
    /// Availability is normalized false; neither presence nor end is recorded.
    pub native: MessageNativeCheckpoint,
}

impl StoredNativeLifetime {
    /// Copies only a validated guarded publication, omitting all identity text
    /// except its bounded agent attribution. Unguarded legacy facts cannot enter.
    fn capture(
        store_id: [u8; 16],
        snapshot: &MessageNativeContinuitySnapshot,
        generation: u64,
    ) -> Result<Self> {
        snapshot.validate().map_err(|_| invalid("lifetime tuple"))?;
        if snapshot.format_version != 2 || !snapshot.admission_guarded || generation == 0 {
            return Err(invalid("lifetime is not a guarded publication"));
        }
        let mut native = snapshot.native.clone();
        native.admission_available = false;
        native.delivery_available = false;
        Ok(Self {
            format_version: 1,
            store_id,
            publication_generation: generation,
            registered_after_sequence: snapshot.registered_after_sequence,
            native,
        })
    }

    /// Equal lifetime tuples keep their earliest qualified publication even as
    /// n1 availability or later publication generations change.
    fn same_tuple(&self, other: &Self) -> bool {
        self.format_version == other.format_version
            && self.store_id == other.store_id
            && self.registered_after_sequence == other.registered_after_sequence
            && self.native == other.native
    }

    /// Uses the existing tuple validator without granting sparse identity data
    /// any registration authority or reconstructing an opaque shell incarnation.
    fn validate(&self) -> Result<()> {
        if self.format_version != 1
            || self.native.admission_available
            || self.native.delivery_available
        {
            return Err(invalid("lifetime framing or availability"));
        }
        MessageNativeContinuitySnapshot {
            format_version: 2,
            admission_guarded: true,
            native: self.native.clone(),
            identity: MessageIdentitySnapshot {
                agent_id: self.native.agent_id.clone(),
                pane_id: None,
                window_id: None,
                role: None,
                capabilities: Vec::new(),
                objective: None,
                project_scope: None,
            },
            registered_after_sequence: self.registered_after_sequence,
        }
        .validate()
        .map_err(|_| invalid("lifetime native derivation"))
    }
}

/// One immutable first history write, including an already existing equal row.
struct LifetimeWrite {
    key: String,
    previous: Option<[u8; 32]>,
    bytes: Vec<u8>,
}

/// Original current input, all permitted history targets and non-target image.
/// This cell remains outside workers; a retry cannot capture a new history plan.
pub(super) struct LifetimePlan {
    input_sha256: [u8; 32],
    writes: Vec<LifetimeWrite>,
    unaffected: [u8; 32],
}

impl LifetimePlan {
    /// Selects at most the actual previous/current guarded tuples in the first
    /// original transaction. An absent old n1 is absence, never guessed history.
    pub(super) fn capture(
        store: &MmpCustodyStore,
        transaction: &rusqlite::Transaction<'_>,
        plan: &NativeContinuityPlan,
        snapshot: &MessageNativeContinuitySnapshot,
        input: &[u8],
    ) -> Result<Self> {
        let state = store.checked_state(transaction)?;
        if state.generation as u64 != plan.generation
            || unaffected_rows_digest(transaction, &[&plan.key])? != plan.unaffected
        {
            return Err(invalid("lifetime original position changed"));
        }
        let store_id = store
            .store_id
            .as_slice()
            .try_into()
            .map_err(|_| invalid("lifetime allocation"))?;
        let mut candidates = std::collections::BTreeMap::new();
        if let Some(previous) = store.read_native_continuity_in_snapshot(transaction, &plan.key)?
            && let Some(generation) = previous.publication_generation()
        {
            let candidate =
                StoredNativeLifetime::capture(store_id, &previous.snapshot, generation)?;
            candidates.insert(record_key(&candidate.native), candidate);
        }
        if snapshot.format_version == 2 && snapshot.admission_guarded {
            let candidate = StoredNativeLifetime::capture(
                store_id,
                snapshot,
                plan.generation
                    .checked_add(1)
                    .ok_or_else(|| invalid("lifetime generation overflow"))?,
            )?;
            let key = record_key(&candidate.native);
            if let Some(previous) = candidates.get(&key) {
                if !candidate.same_tuple(previous) {
                    return Err(invalid("lifetime tuple changed within one namespace"));
                }
            } else {
                candidates.insert(key, candidate);
            }
        }
        let mut writes = Vec::with_capacity(candidates.len());
        for (key, candidate) in candidates {
            let before = store.read(&key)?;
            let kept = if let Some(bytes) = &before {
                let existing = store.decode_native_lifetime(
                    &key,
                    bytes,
                    state.watermark,
                    state.generation as u64,
                )?;
                if !existing.same_tuple(&candidate)
                    || existing.publication_generation > candidate.publication_generation
                {
                    return Err(invalid("lifetime immutable provenance changed"));
                }
                bytes.clone()
            } else {
                let bytes =
                    serde_json::to_vec(&candidate).map_err(|_| invalid("lifetime encoding"))?;
                if bytes.len() > MAX_LIFETIME_RECORD {
                    return Err(invalid("lifetime record capacity"));
                }
                bytes
            };
            writes.push(LifetimeWrite {
                key,
                previous: before.as_ref().map(|b| Sha256::digest(b).into()),
                bytes: kept,
            });
        }
        let mut targets = vec![plan.key.as_str()];
        targets.extend(writes.iter().map(|w| w.key.as_str()));
        Ok(Self {
            input_sha256: Sha256::digest(input).into(),
            unaffected: unaffected_rows_digest(transaction, &targets)?,
            writes,
        })
    }

    /// Checks the same captured n1 bytes before any retry can use this plan.
    pub(super) fn matches_input(&self, bytes: &[u8]) -> bool {
        self.input_sha256 == <[u8; 32]>::from(Sha256::digest(bytes))
    }

    /// Borrows fixed first-plan writes, without allocating another record image.
    pub(super) fn writes(&self) -> impl Iterator<Item = CustodyWrite<'_>> {
        self.writes.iter().map(|w| CustodyWrite {
            key: &w.key,
            previous: w.previous.as_ref().map(|p| p.as_slice()),
            value: Some(&w.bytes),
        })
    }

    /// All history targets are exact after SQL effects; every other row remains
    /// bound to its first image. A trigger cannot manufacture different history.
    pub(super) fn revalidate(
        &self,
        store: &MmpCustodyStore,
        transaction: &rusqlite::Transaction<'_>,
        native_key: &str,
        after: bool,
    ) -> Result<()> {
        let mut targets = vec![native_key];
        targets.extend(self.writes.iter().map(|w| w.key.as_str()));
        if unaffected_rows_digest(transaction, &targets)? != self.unaffected {
            return Err(invalid("lifetime write changed unrelated rows"));
        }
        if after {
            for write in &self.writes {
                if store.read(&write.key)?.as_deref() != Some(write.bytes.as_slice()) {
                    return Err(invalid("lifetime target changed during SQL effects"));
                }
            }
        }
        Ok(())
    }
}

impl MmpCustodyStore {
    /// Owns the exact read snapshot for host-boundary tests without exporting
    /// this store's private SQLite handle or manufacturing a native capability.
    #[cfg(test)]
    pub(crate) fn read_native_lifetime(&self, key: &str) -> Result<Option<StoredNativeLifetime>> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let result = self.read_native_lifetime_in_snapshot(&transaction, key);
        self.finish_transaction(transaction)?;
        result
    }

    /// Reads only in this connection's actual snapshot; returned tuples remain
    /// descriptive and cannot create availability, retirement or live tokens.
    pub(crate) fn read_native_lifetime_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
    ) -> Result<Option<StoredNativeLifetime>> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid("lifetime snapshot belongs to another connection"));
        }
        let state = self.checked_state(transaction)?;
        self.read(key)?
            .map(|bytes| {
                let lifetime = self.decode_native_lifetime(
                    key,
                    &bytes,
                    state.watermark,
                    state.generation as u64,
                )?;
                self.native_registration_has_ended(
                    transaction,
                    &lifetime.native,
                    lifetime.registered_after_sequence,
                    Some(lifetime.publication_generation),
                )?;
                Ok(lifetime)
            })
            .transpose()
    }

    /// Existing history must agree with a currently stored guarded observation.
    /// Missing legacy history remains unknown; it is not manufactured by read.
    pub(super) fn validate_native_lifetime_observation(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        observation: &StoredNativeContinuity,
    ) -> Result<()> {
        self.native_registration_has_ended(
            transaction,
            &observation.snapshot.native,
            observation.snapshot.registered_after_sequence,
            observation.generation,
        )?;
        let Some(generation) = observation.publication_generation() else {
            return Ok(());
        };
        let store_id = self
            .store_id
            .as_slice()
            .try_into()
            .map_err(|_| invalid("lifetime allocation"))?;
        let expected = StoredNativeLifetime::capture(store_id, &observation.snapshot, generation)?;
        if let Some(history) =
            self.read_native_lifetime_in_snapshot(transaction, &record_key(&expected.native))?
            && (!history.same_tuple(&expected) || history.publication_generation > generation)
        {
            return Err(invalid(
                "lifetime disagrees with original native observation",
            ));
        }
        Ok(())
    }

    /// Bounds fixed-shape decoding and validates its allocation, first guarded
    /// publication, tuple-derived key and persistent allocator floor.
    fn decode_native_lifetime(
        &self,
        key: &str,
        bytes: &[u8],
        watermark: u64,
        generation: u64,
    ) -> Result<StoredNativeLifetime> {
        if bytes.len() > MAX_LIFETIME_RECORD {
            return Err(invalid("lifetime record capacity"));
        }
        let record: StoredNativeLifetime =
            serde_json::from_slice(bytes).map_err(|_| invalid("lifetime framing"))?;
        record.validate()?;
        if record.store_id.as_slice() != self.store_id
            || record_key(&record.native) != key
            || record.publication_generation == 0
            || record.publication_generation > generation
            || record.registered_after_sequence > watermark
        {
            return Err(invalid("lifetime allocation, key, publication or floor"));
        }
        Ok(record)
    }
}

/// Derived owner hash is attribution, never a filesystem path or live token.
fn record_key(native: &MessageNativeCheckpoint) -> String {
    format!("b1:{}", native.owner.incarnation())
}
