//! Inert durable facts for one exact native accounting activation attempt.
//!
//! This worker-only ledger supplements the native namespace mapping; it does
//! not grant enrollment, attest a producer, restore credentials or activate a
//! cold service. A future admission owner must independently qualify the exact
//! native source and ancestry, supply a same-boot clock, and fence publication
//! and retirement. Native observation writes only Prepared for a newly allocated
//! ordinary socket source. Actual actor registration can seal a second off-actor
//! acceptance write. Logical source retirement queues an exact tombstone on the
//! existing persistence lane, retaining its original proposal and cached outcome.
//! Lost retirement receipts may reconcile from an existing exact tombstone. One
//! worker-owned recovery CAS requires positive unchanged-predecessor evidence;
//! unknown mutations never replay or create/migrate storage merely on read.
//! A separate explicit server end method requires exact worker proof. The client
//! has finite same-service end-ACK recovery. An inert renewal CAS stores a separate
//! bounded checkpoint without rewriting the original proposal. Native runtime
//! renewal is paced and actor-fenced. Schema10 stores bounded public observer
//! digests initialized only by the sealed acceptance worker; observer takeover
//! admission/publication and typed cold recovery remain unfinished.
//! Exhausted or unknown optional writes remain unavailable.
//! Curated and existing source paths are unchanged. Accepted is historical
//! admission, not a persisted assertion of current liveness or a control right.
//!
//! Each mapped namespace retains at most one bounded record, including its
//! retired tombstone. Preparation is not acceptance. Exact retries reconcile
//! committed facts; changed/stale attempts cannot replace them. Expiry never
//! deletes a record or turns it into a new namespace. No paths, native PIDs,
//! launch tokens, callback bodies or transcripts are stored.

use rusqlite::{Connection, OptionalExtension, params};

use super::{ExternalSourceKey, MezError, Result, TokenUsageStore, sqlite_i64};

mod renewal;
pub(crate) use renewal::ExternalSourceRenewal;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "public observer takeover admission remains unfinished"
    )
)]
mod observers;
pub(crate) use observers::ExternalSourceObserverFence;

/// Same-boot retention window, not a public reconnect or delivery guarantee.
const MAX_ACTIVATION_WINDOW_MS: u64 = 60_000;
/// One fixed-size activation/tombstone per existing bounded source namespace.
const MAX_ACTIVATIONS: i64 = 65_536;

/// Captured server attempt and native ancestry digests plus a same-boot window.
/// Construction checks representation only, never native authority or a clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalSourceActivation {
    attempt: [u8; 32],
    ancestry: [u8; 32],
    prepared_at_boot_ms: u64,
    expires_at_boot_ms: u64,
}

impl ExternalSourceActivation {
    /// Validates fixed-size, nonempty digests and a finite positive boot-clock
    /// window within SQLite's exact range. UTC or serialized Instant is not a
    /// substitute for the caller's independently qualified same-boot clock.
    pub(crate) fn new(
        attempt: [u8; 32],
        ancestry: [u8; 32],
        prepared_at_boot_ms: u64,
        expires_at_boot_ms: u64,
    ) -> Result<Self> {
        if attempt == [0; 32]
            || ancestry == [0; 32]
            || prepared_at_boot_ms == 0
            || expires_at_boot_ms > i64::MAX as u64
            || expires_at_boot_ms
                .checked_sub(prepared_at_boot_ms)
                .is_none_or(|duration| duration == 0 || duration > MAX_ACTIVATION_WINDOW_MS)
        {
            return Err(MezError::invalid_args(
                "external source activation metadata unavailable",
            ));
        }
        Ok(Self {
            attempt,
            ancestry,
            prepared_at_boot_ms,
            expires_at_boot_ms,
        })
    }

    /// Compares only inert native continuity metadata; equality grants no authority.
    pub(crate) fn matches_native_ancestry(&self, digest: &[u8; 32]) -> bool {
        self.ancestry == *digest
    }

    /// Checks the captured half-open window without refreshing or extending it.
    fn contains_boot_time(&self, now_boot_ms: u64) -> bool {
        self.prepared_at_boot_ms <= now_boot_ms && now_boot_ms < self.expires_at_boot_ms
    }
}

/// Stored stage facts, never sufficient evidence of current native authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalSourceActivationState {
    /// Native publication has not been durably accepted.
    Prepared,
    /// An exact caller-owned acceptance transition committed.
    Accepted,
    /// Absorbing tombstone; no reprepare or accept transition is allowed.
    Retired,
}

impl ExternalSourceActivationState {
    /// Encodes the closed storage grammar without a generic serialization layer.
    fn stored(self) -> i64 {
        match self {
            Self::Prepared => 0,
            Self::Accepted => 1,
            Self::Retired => 2,
        }
    }

    /// Rejects unknown stages even when a damaged database bypasses SQL checks.
    fn from_stored(value: i64) -> Result<Self> {
        match value {
            0 => Ok(Self::Prepared),
            1 => Ok(Self::Accepted),
            2 => Ok(Self::Retired),
            _ => Err(invalid_stored_activation()),
        }
    }
}

/// Immutable transaction result. It cannot restore native proof or credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalSourceActivationReceipt {
    /// Original immutable attempt, ancestry and clock window.
    pub(crate) activation: ExternalSourceActivation,
    /// Exact compare-and-set revision, never a client permission or generation.
    pub(crate) revision: u64,
    /// Current durable stage; preparation and retirement are not acceptance.
    pub(crate) state: ExternalSourceActivationState,
    /// Optional latest inert lease checkpoint; original proposal stays exact.
    pub(crate) renewal: Option<ExternalSourceRenewal>,
    /// Optional public replay fence, never native proof or a bearer capability.
    pub(crate) observer: Option<ExternalSourceObserverFence>,
}

impl ExternalSourceActivationReceipt {
    /// Tests inert acceptance/clock facts only. The caller must still establish
    /// same-boot native ownership, ancestry and current actor authorization.
    pub(crate) fn is_accepted_at(&self, now_boot_ms: u64) -> bool {
        self.state == ExternalSourceActivationState::Accepted
            && self.renewal.as_ref().map_or_else(
                || self.activation.contains_boot_time(now_boot_ms),
                |renewal| renewal.contains_boot_time(now_boot_ms),
            )
    }
}

/// Uses fixed diagnostics rather than echoing stored identifiers or metadata.
fn invalid_stored_activation() -> MezError {
    MezError::invalid_state("stored external source activation is invalid")
}

/// Resolves only an existing exact native-key/harness mapping. Read a bounded
/// BYTE prefix and validate the stored type: SQLite TEXT length/substr stops at
/// NUL and must never turn a malformed owner into another source's valid UUID.
/// No namespace allocation or foreign/malformed mapping repair occurs here.
fn mapped_owner(connection: &Connection, key: &ExternalSourceKey) -> Result<String> {
    let owner: Option<(Vec<u8>, String)> = connection
        .query_row(
            "SELECT substr(CAST(owner AS BLOB),1,37),typeof(owner)
             FROM external_usage_sources WHERE witness=?1 AND harness=?2",
            params![key.digest, key.harness],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (bytes, kind) = owner.ok_or_else(|| {
        MezError::forbidden("external source activation requires an exact mapped source")
    })?;
    if kind != "text" {
        return Err(invalid_stored_activation());
    }
    let owner = String::from_utf8(bytes).map_err(|_| invalid_stored_activation())?;
    if !super::external_sources::valid_uuid(&owner) {
        return Err(invalid_stored_activation());
    }
    Ok(owner)
}

/// Bounds byte decoding before allocating vectors and validates every stage,
/// integer and immutable window on read, not only at initial SQL insertion.
fn load_receipt(
    connection: &Connection,
    owner: &str,
) -> Result<Option<ExternalSourceActivationReceipt>> {
    let row = connection
        .query_row(
            "SELECT substr(attempt,1,33),substr(ancestry,1,33),prepared_at_boot_ms,
             expires_at_boot_ms,revision,state,renewed_at_boot_ms,renewed_until_boot_ms,
             substr(CAST(observer_fence AS BLOB),1,4172),typeof(observer_fence)
             FROM external_usage_source_activations WHERE owner=?1",
            [owner],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    super::store::row_u64(row, 2)?,
                    super::store::row_u64(row, 3)?,
                    super::store::row_u64(row, 4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<Vec<u8>>>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        attempt,
        ancestry,
        prepared,
        expires,
        revision,
        state,
        renewed_at,
        renewed_until,
        observer_bytes,
        observer_kind,
    )) = row
    else {
        return Ok(None);
    };
    if revision == 0 {
        return Err(invalid_stored_activation());
    }
    let activation = ExternalSourceActivation::new(
        attempt
            .try_into()
            .map_err(|_| invalid_stored_activation())?,
        ancestry
            .try_into()
            .map_err(|_| invalid_stored_activation())?,
        prepared,
        expires,
    )
    .map_err(|_| invalid_stored_activation())?;
    let state = ExternalSourceActivationState::from_stored(state)?;
    let renewal = match (renewed_at, renewed_until) {
        (None, None) => {
            let valid = match state {
                ExternalSourceActivationState::Prepared => revision == 1,
                ExternalSourceActivationState::Accepted => revision == 2,
                ExternalSourceActivationState::Retired => matches!(revision, 2 | 3),
            };
            if !valid {
                return Err(invalid_stored_activation());
            }
            None
        }
        (Some(start), Some(end)) => {
            let renewal = ExternalSourceRenewal::new(
                u64::try_from(start).map_err(|_| invalid_stored_activation())?,
                u64::try_from(end).map_err(|_| invalid_stored_activation())?,
            )
            .map_err(|_| invalid_stored_activation())?;
            if state == ExternalSourceActivationState::Prepared
                || revision < 3
                || (state == ExternalSourceActivationState::Retired && revision < 4)
                || renewal.renewed_at_boot_ms < prepared
                || renewal.expires_at_boot_ms <= expires
            {
                return Err(invalid_stored_activation());
            }
            Some(renewal)
        }
        _ => return Err(invalid_stored_activation()),
    };
    let observer = match (observer_bytes, observer_kind.as_str()) {
        (None, "null") => None,
        (Some(bytes), "blob")
            if state != ExternalSourceActivationState::Prepared
                && !(state == ExternalSourceActivationState::Retired && revision == 2) =>
        {
            Some(ExternalSourceObserverFence::decode(&bytes)?)
        }
        _ => return Err(invalid_stored_activation()),
    };
    Ok(Some(ExternalSourceActivationReceipt {
        activation,
        revision,
        state,
        renewal,
        observer,
    }))
}

impl TokenUsageStore {
    /// Reads an existing current-schema store under one coherent read-only
    /// SQLite snapshot. Never creates/migrates storage, changes journal mode or
    /// upgrades a legacy mapping into evidence. Missing/unknown storage is an
    /// error, not an empty valid ledger. Grants no native/reporting authority.
    /// SQLite may use its normal coordination sidecars; no data is mutated.
    /// Call only on an off-actor worker.
    pub(crate) fn external_source_activation(
        &self,
        key: &ExternalSourceKey,
    ) -> Result<Option<ExternalSourceActivationReceipt>> {
        let mut connection =
            crate::storage::shared_sqlite::open_shared_database_read_only(self.path())?
                .ok_or_else(|| {
                    MezError::invalid_state("external source activation store unavailable")
                })?;
        connection.busy_timeout(std::time::Duration::from_millis(250))?;
        let tx = connection.transaction()?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != super::store::SCHEMA_VERSION {
            return Err(MezError::invalid_state(
                "external source activation schema unavailable",
            ));
        }
        let owner = mapped_owner(&tx, key)?;
        load_receipt(&tx, &owner)
    }

    /// Creates one inert prepared record. Exact replay is idempotent, but an
    /// abandoned/retired or different attempt is never replaced or evicted.
    /// Call only off actor after actual native qualification by the caller.
    pub(crate) fn prepare_external_source_activation(
        &self,
        key: &ExternalSourceKey,
        activation: &ExternalSourceActivation,
    ) -> Result<ExternalSourceActivationReceipt> {
        self.prepare_source_activation_with_limit(key, activation, MAX_ACTIVATIONS)
    }

    /// Owns bounded insertion and exact retry lookup in one immediate writer.
    fn prepare_source_activation_with_limit(
        &self,
        key: &ExternalSourceKey,
        activation: &ExternalSourceActivation,
        limit: i64,
    ) -> Result<ExternalSourceActivationReceipt> {
        let mut connection = self.open()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let owner = mapped_owner(&tx, key)?;
        if let Some(previous) = load_receipt(&tx, &owner)? {
            if previous.activation == *activation
                && previous.state != ExternalSourceActivationState::Retired
            {
                return Ok(previous);
            }
            return Err(MezError::conflict(
                "external source activation attempt changed or retired",
            ));
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM external_usage_source_activations",
            [],
            |row| row.get(0),
        )?;
        if count >= limit {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "durable external source activation capacity unavailable",
            ));
        }
        tx.execute(
            "INSERT INTO external_usage_source_activations
             (owner,attempt,ancestry,prepared_at_boot_ms,expires_at_boot_ms,revision,state)
             VALUES(?1,?2,?3,?4,?5,1,0)",
            params![
                owner,
                activation.attempt.as_slice(),
                activation.ancestry.as_slice(),
                sqlite_i64(activation.prepared_at_boot_ms, "activation clock")?,
                sqlite_i64(activation.expires_at_boot_ms, "activation expiry")?
            ],
        )?;
        tx.commit()?;
        Ok(ExternalSourceActivationReceipt {
            activation: activation.clone(),
            revision: 1,
            state: ExternalSourceActivationState::Prepared,
            renewal: None,
            observer: None,
        })
    }

    /// Commits exact prepared acceptance only within its original same-boot
    /// window. Exact committed retry reconciles the existing revision without
    /// another write. This fact alone cannot activate or attest a native source.
    #[cfg(test)]
    pub(crate) fn accept_external_source_activation(
        &self,
        key: &ExternalSourceKey,
        activation: &ExternalSourceActivation,
        expected_revision: u64,
        now_boot_ms: u64,
    ) -> Result<ExternalSourceActivationReceipt> {
        self.accept_external_source_activation_with_observer(
            key,
            activation,
            expected_revision,
            now_boot_ms,
            None,
        )
    }

    /// Initializes only an inert public fence in the same acceptance transaction.
    /// Native/actor proof remains the caller's responsibility, never these bytes.
    pub(crate) fn accept_external_source_activation_with_observer(
        &self,
        key: &ExternalSourceKey,
        activation: &ExternalSourceActivation,
        expected_revision: u64,
        now_boot_ms: u64,
        observer: Option<&ExternalSourceObserverFence>,
    ) -> Result<ExternalSourceActivationReceipt> {
        if observer.is_some_and(|observer| !observer.is_initial()) {
            return Err(invalid_stored_activation());
        }
        if !activation.contains_boot_time(now_boot_ms) {
            return Err(MezError::conflict(
                "external source activation clock unavailable or expired",
            ));
        }
        self.transition_source_activation(
            key,
            activation,
            expected_revision,
            ExternalSourceActivationState::Accepted,
            observer,
        )
    }

    /// Tombstones an exact prepared/accepted attempt, including after expiry.
    /// Old or changed work cannot retire another attempt/revision. Call off actor;
    /// the native owner must fence retirement before dropping its live proof.
    #[cfg(test)]
    pub(crate) fn retire_external_source_activation(
        &self,
        key: &ExternalSourceKey,
        activation: &ExternalSourceActivation,
        expected_revision: u64,
    ) -> Result<ExternalSourceActivationReceipt> {
        self.retire_external_source_activation_with_observer(
            key,
            activation,
            expected_revision,
            None,
        )
    }

    /// Tombstones only the exact captured public observer as well as lifetime
    /// revision. None means an exact unknown/Prepared fence, NEVER a wildcard.
    /// The comparison also fences already-committed retry acknowledgment.
    pub(crate) fn retire_external_source_activation_with_observer(
        &self,
        key: &ExternalSourceKey,
        activation: &ExternalSourceActivation,
        expected_revision: u64,
        observer: Option<&ExternalSourceObserverFence>,
    ) -> Result<ExternalSourceActivationReceipt> {
        self.transition_source_activation(
            key,
            activation,
            expected_revision,
            ExternalSourceActivationState::Retired,
            observer,
        )
    }

    /// Performs one revision-fenced transition under immediate writer ownership.
    /// Missing preparation, revision exhaustion and changed payload fail closed.
    fn transition_source_activation(
        &self,
        key: &ExternalSourceKey,
        activation: &ExternalSourceActivation,
        expected_revision: u64,
        target: ExternalSourceActivationState,
        observer: Option<&ExternalSourceObserverFence>,
    ) -> Result<ExternalSourceActivationReceipt> {
        if expected_revision == 0 {
            return Err(MezError::invalid_args(
                "external source activation revision unavailable",
            ));
        }
        let revision = expected_revision
            .checked_add(1)
            .filter(|revision| *revision <= i64::MAX as u64)
            .ok_or_else(|| {
                MezError::invalid_state("external source activation revision exhausted")
            })?;
        let mut connection = self.open()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let owner = mapped_owner(&tx, key)?;
        let previous = load_receipt(&tx, &owner)?
            .ok_or_else(|| MezError::forbidden("external source activation was not prepared"))?;
        if previous.activation != *activation {
            return Err(MezError::conflict(
                "external source activation attempt changed",
            ));
        }
        if target == ExternalSourceActivationState::Retired
            && previous.observer.as_ref() != observer
        {
            return Err(MezError::conflict(
                "external source retirement observer changed",
            ));
        }
        if previous.state == target && previous.revision == revision {
            if target == ExternalSourceActivationState::Accepted
                && previous.observer.as_ref() != observer
            {
                return Err(MezError::conflict(
                    "external source observer acceptance changed",
                ));
            }
            return Ok(previous);
        }
        let allowed = matches!(
            (previous.state, target),
            (
                ExternalSourceActivationState::Prepared,
                ExternalSourceActivationState::Accepted | ExternalSourceActivationState::Retired
            ) | (
                ExternalSourceActivationState::Accepted,
                ExternalSourceActivationState::Retired
            )
        );
        if previous.revision != expected_revision || !allowed {
            return Err(MezError::conflict(
                "external source activation is stale or retired",
            ));
        }
        let observer = if target == ExternalSourceActivationState::Accepted {
            observer.cloned()
        } else {
            previous.observer
        };
        if tx.execute(
            "UPDATE external_usage_source_activations SET revision=?1,state=?2,observer_fence=?4 WHERE owner=?3",
            params![
                sqlite_i64(revision, "activation revision")?,
                target.stored(),
                owner,
                observer.as_ref().map(ExternalSourceObserverFence::encode)
            ],
        )? != 1
        {
            return Err(invalid_stored_activation());
        }
        tx.commit()?;
        Ok(ExternalSourceActivationReceipt {
            activation: activation.clone(),
            revision,
            state: target,
            renewal: previous.renewal,
            observer,
        })
    }

    /// Injects a tiny count ceiling in only this generated-store test; production
    /// keeps its fixed cap and never evicts an accepted or retired record.
    #[cfg(test)]
    pub(super) fn prepare_external_source_activation_with_limit_for_tests(
        &self,
        key: &ExternalSourceKey,
        activation: &ExternalSourceActivation,
        limit: i64,
    ) -> Result<ExternalSourceActivationReceipt> {
        self.prepare_source_activation_with_limit(key, activation, limit)
    }
}
