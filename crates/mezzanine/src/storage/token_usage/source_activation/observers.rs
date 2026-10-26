//! Bounded public observer replay fences in the existing activation ledger.
//!
//! Digests bind only public instance/operation identifiers, never a launch token,
//! source attestation or general capability. The independent observer generation
//! does not alter native lifetime revisions/checkpoints. Native proof, actual
//! admission and final publication remain separate runtime responsibilities.
//! Legacy NULL is unknown, not an initial fence inferred from namespace ownership.

use super::*;
use sha2::{Digest, Sha256};

const MAX_INSTANCES: usize = 128;
const HEADER: usize = 75;
const MAX_BYTES: usize = HEADER + MAX_INSTANCES * 32;

/// Inert current public identity plus absorbing seen-instance tombstones.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ExternalSourceObserverFence {
    generation: u64,
    current: [u8; 32],
    operation: [u8; 32],
    seen: Vec<[u8; 32]>,
}

impl std::fmt::Debug for ExternalSourceObserverFence {
    /// No public selector/digest, private credential or native identifier is echoed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalSourceObserverFence")
            .field("generation", &self.generation)
            .field("instances", &self.seen.len())
            .finish_non_exhaustive()
    }
}

/// One immutable expected-public-fence proposal; construction grants no rights.
#[derive(Debug, Clone)]
pub(crate) struct ExternalSourceObserverChange {
    previous: ExternalSourceObserverFence,
    next: ExternalSourceObserverFence,
}

impl ExternalSourceObserverChange {
    /// Pure expected result of this exact proposal, not proof it committed.
    pub(crate) fn next_fence(&self) -> &ExternalSourceObserverFence {
        &self.next
    }
}

impl ExternalSourceObserverFence {
    /// Hashes bounded public IDs with separate domains; does not attest a source.
    pub(crate) fn initial(instance: &str, operation: [u8; 32]) -> Result<Self> {
        let current = instance_digest(instance)?;
        let operation = operation_digest(operation)?;
        Ok(Self {
            generation: 1,
            current,
            operation,
            seen: vec![current],
        })
    }

    /// Initial acceptance cannot seed an invented historical generation/set.
    pub(super) fn is_initial(&self) -> bool {
        self.generation == 1 && self.seen == [self.current]
    }

    /// Public counter only, never a private launch/control generation.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Bounded public history membership only, never permission to admit an ID.
    pub(crate) fn contains_instance(&self, instance: &str) -> Result<bool> {
        Ok(self.seen.binary_search(&instance_digest(instance)?).is_ok())
    }

    /// Complete actor-owned history may include IDs whose earlier optional
    /// publication failed. Merge all of them into one bounded immutable CAS;
    /// never infer that publishing just the current ID repairs an old gap.
    pub(crate) fn propose_with_history(
        &self,
        predecessor: u64,
        instance: &str,
        operation: [u8; 32],
        instances: &std::collections::BTreeSet<String>,
    ) -> Result<ExternalSourceObserverChange> {
        let mut change = self.propose(predecessor, instance, operation)?;
        for instance in instances {
            let digest = instance_digest(instance)?;
            if let Err(index) = change.next.seen.binary_search(&digest) {
                if change.next.seen.len() >= MAX_INSTANCES {
                    return Err(MezError::conflict(
                        "external source observer history capacity exhausted",
                    ));
                }
                change.next.seen.insert(index, digest);
            }
        }
        // The v1 codec requires the counter to cover its distinct seen IDs.
        // Repair may cover multiple skipped admissions in this single CAS.
        change.next.generation = change.next.generation.max(change.next.seen.len() as u64);
        Ok(change)
    }

    /// Exact original-current replay is inert; any changed intent must name the
    /// exact predecessor. Seen noncurrent IDs never return, even at capacity.
    pub(crate) fn propose(
        &self,
        predecessor: u64,
        instance: &str,
        operation: [u8; 32],
    ) -> Result<ExternalSourceObserverChange> {
        let current = instance_digest(instance)?;
        let operation = operation_digest(operation)?;
        if current == self.current && operation == self.operation {
            return Ok(ExternalSourceObserverChange {
                previous: self.clone(),
                next: self.clone(),
            });
        }
        if predecessor != self.generation
            || operation == self.operation
            || (current != self.current && self.seen.binary_search(&current).is_ok())
        {
            return Err(MezError::conflict(
                "external source observer predecessor changed",
            ));
        }
        let generation = self
            .generation
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| {
                MezError::invalid_state("external source observer generation exhausted")
            })?;
        let mut seen = self.seen.clone();
        if let Err(index) = seen.binary_search(&current) {
            if seen.len() >= MAX_INSTANCES {
                return Err(MezError::new(
                    crate::error::MezErrorKind::RateLimited,
                    "external source observer capacity unavailable",
                ));
            }
            seen.insert(index, current);
        }
        Ok(ExternalSourceObserverChange {
            previous: self.clone(),
            next: Self {
                generation,
                current,
                operation,
                seen,
            },
        })
    }

    /// Closed version1 codec: fixed header and sorted unique bounded digests.
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER + self.seen.len() * 32);
        bytes.push(1);
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        bytes.extend_from_slice(&self.current);
        bytes.extend_from_slice(&self.operation);
        bytes.extend_from_slice(&(self.seen.len() as u16).to_be_bytes());
        for digest in &self.seen {
            bytes.extend_from_slice(digest);
        }
        bytes
    }

    /// Rejects future/partial/noncanonical fields even with disabled SQL checks.
    pub(super) fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER + 32 || bytes.len() > MAX_BYTES || bytes[0] != 1 {
            return Err(invalid_stored_activation());
        }
        let generation = u64::from_be_bytes(
            bytes[1..9]
                .try_into()
                .map_err(|_| invalid_stored_activation())?,
        );
        let current: [u8; 32] = bytes[9..41]
            .try_into()
            .map_err(|_| invalid_stored_activation())?;
        let operation: [u8; 32] = bytes[41..73]
            .try_into()
            .map_err(|_| invalid_stored_activation())?;
        let count = u16::from_be_bytes(
            bytes[73..75]
                .try_into()
                .map_err(|_| invalid_stored_activation())?,
        ) as usize;
        if count == 0
            || count > MAX_INSTANCES
            || bytes.len() != HEADER + count * 32
            || generation == 0
            || generation > i64::MAX as u64
            || generation < count as u64
            || current == [0; 32]
            || operation == [0; 32]
        {
            return Err(invalid_stored_activation());
        }
        let seen = bytes[HEADER..].as_chunks::<32>().0.to_vec();
        if seen.contains(&[0; 32])
            || seen.windows(2).any(|pair| pair[0] >= pair[1])
            || seen.binary_search(&current).is_err()
        {
            return Err(invalid_stored_activation());
        }
        Ok(Self {
            generation,
            current,
            operation,
            seen,
        })
    }
}

/// Treats identifiers as byte-exact public metadata, not callback content.
fn instance_digest(instance: &str) -> Result<[u8; 32]> {
    if instance.is_empty() || instance.len() > 128 || instance.chars().any(char::is_control) {
        return Err(MezError::invalid_args(
            "external source observer identifier unavailable",
        ));
    }
    let mut hash = Sha256::new();
    hash.update(b"mezzanine/public-source-observer-instance/v1\0");
    hash.update((instance.len() as u64).to_be_bytes());
    hash.update(instance.as_bytes());
    Ok(hash.finalize().into())
}

/// Public operation nonces are domain separated from instances and credentials.
fn operation_digest(operation: [u8; 32]) -> Result<[u8; 32]> {
    if operation == [0; 32] {
        return Err(MezError::invalid_args(
            "external source observer operation unavailable",
        ));
    }
    let mut hash = Sha256::new();
    hash.update(b"mezzanine/public-source-observer-operation/v1\0");
    hash.update(operation);
    Ok(hash.finalize().into())
}

impl TokenUsageStore {
    /// Worker-only inert exact-fence CAS. Never initializes legacy NULL, extends
    /// a clock/window, allocates a namespace or proves native/actor authority.
    /// Exact committed intent retry does no write, even after a native renewal.
    pub(crate) fn advance_external_source_observer(
        &self,
        key: &ExternalSourceKey,
        activation: &ExternalSourceActivation,
        expected_revision: u64,
        change: &ExternalSourceObserverChange,
        now_boot_ms: u64,
    ) -> Result<ExternalSourceActivationReceipt> {
        self.advance_external_source_observer_history(
            key,
            activation,
            expected_revision,
            change,
            now_boot_ms,
            None,
        )
    }

    /// Same exact-fence transaction plus one captured pre-admission gap intent.
    /// A later warm proposal replaces that intent, preventing an older worker
    /// from certifying completeness over a not-yet-captured admission.
    pub(crate) fn advance_external_source_observer_history(
        &self,
        key: &ExternalSourceKey,
        activation: &ExternalSourceActivation,
        expected_revision: u64,
        change: &ExternalSourceObserverChange,
        now_boot_ms: u64,
        history_nonce: Option<[u8; 32]>,
    ) -> Result<ExternalSourceActivationReceipt> {
        if expected_revision < 2 || expected_revision > i64::MAX as u64 {
            return Err(invalid_stored_activation());
        }
        let mut connection = self.open()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let owner = mapped_owner(&tx, key)?;
        let mut previous = load_receipt(&tx, &owner)?.ok_or_else(invalid_stored_activation)?;
        if previous.activation != *activation || !previous.is_accepted_at(now_boot_ms) {
            return Err(MezError::conflict(
                "external source observer activation unavailable",
            ));
        }
        let gap = super::history::read_gap(&tx, &owner)?;
        if gap != history_nonce {
            if gap.is_none() && previous.observer.as_ref() == Some(&change.next) {
                return Ok(previous);
            }
            return Err(MezError::conflict(
                "external source observer history changed",
            ));
        }
        if previous.observer.as_ref() == Some(&change.next) {
            return Ok(previous);
        }
        if previous.revision != expected_revision
            || previous.observer.as_ref() != Some(&change.previous)
        {
            return Err(MezError::conflict("external source observer fence changed"));
        }
        if tx.execute("UPDATE external_usage_source_activations SET observer_fence=?1,observer_history_gap=NULL WHERE owner=?2 AND revision=?3 AND state=1 AND observer_fence=?4 AND observer_history_gap IS ?5",
            params![change.next.encode(), owner, sqlite_i64(expected_revision, "activation revision")?, change.previous.encode(), history_nonce.as_ref().map(|nonce|nonce.as_slice())])? != 1 { return Err(invalid_stored_activation()); }
        tx.commit()?;
        previous.observer = Some(change.next.clone());
        Ok(previous)
    }
}
