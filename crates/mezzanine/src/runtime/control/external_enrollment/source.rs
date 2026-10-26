//! Native boot/source namespace lookup on the existing enrollment worker.
//!
//! UUID comes from fixed host-kernel interfaces, never environment/payload/PATH.
//! The lookup follows genuine producer/ancestry observation but grants no
//! authority: actor commit still revalidates retained native evidence/deadline.
//! Ledger or boot-evidence failure does not break observational lifecycle and
//! cannot enable usage. Request-bound usage and baseline continuity are
//! independently qualified; finding a namespace alone never enables reporting.

use super::*;
use crate::storage::token_usage::{ExternalSourceKey, NativeSourceKind, NativeSourceWitness};

/// Frozen first-allocation inputs and original preparation/acceptance outcomes.
/// These facts alone are not native authority or a reusable launch credential.
/// Retaining the exact proposal permits later explicit reconciliation rather
/// than regenerating an attempt after an ambiguous write. One bounded recovery
/// CAS requires positive exact-predecessor evidence; unknown writes never replay.
#[derive(Debug)]
pub(super) struct NativeSourcePreparation {
    key: ExternalSourceKey,
    /// Original repository snapshot; retirement never retargets a new store.
    store: crate::storage::token_usage::TokenUsageStore,
    activation: crate::storage::token_usage::ExternalSourceActivation,
    receipt: OnceLock<Result<crate::storage::token_usage::ExternalSourceActivationReceipt>>,
    /// Sealed only after the actor actually registered this original source.
    pub(super) admission: OnceLock<super::publication::SourceActorAdmission>,
    /// Original acceptance outcome, shared by worker clones without replay.
    acceptance: OnceLock<Result<crate::storage::token_usage::ExternalSourceActivationReceipt>>,
    /// Original retirement result, retained without automatic write replay.
    retirement: OnceLock<Result<crate::storage::token_usage::ExternalSourceActivationReceipt>>,
    /// Exact target captured before CAS, or from an already committed tombstone.
    retirement_revision: OnceLock<u64>,
    /// Original predecessor stage, never inferred from a later changed row.
    retirement_state: OnceLock<crate::storage::token_usage::ExternalSourceActivationState>,
    /// At most one recovery write; errors cannot replenish its attempt budget.
    retirement_recovery:
        OnceLock<Result<crate::storage::token_usage::ExternalSourceActivationReceipt>>,
    /// Per-fixture loss AFTER the real tombstone commits, never fake authority.
    #[cfg(test)]
    retirement_ack_lost: std::sync::atomic::AtomicBool,
}

impl NativeSourcePreparation {
    /// Captures actual retained native continuity and a bracketed same-boot
    /// window after mapping allocation, before any receipt write. Missing clock,
    /// rollback, stale source/dead chain or arithmetic ambiguity refuses capture.
    fn capture(
        work: &ExternalEnrollmentWork,
        producer: &ProducerEvidence,
        key: ExternalSourceKey,
    ) -> Option<Self> {
        let before = super::source_clock::native_boot_millis()?;
        let original = producer.reobserve().ok()?;
        let ancestry = work.ancestry.get()?;
        let digest = ancestry.continuity_digest(producer.uid(), original).ok()?;
        if producer.reobserve().ok()? != original || Instant::now() >= work.deadline {
            return None;
        }
        let after = super::source_clock::native_boot_millis()?;
        let expires = before.checked_add(60_000)?;
        if after < before || after >= expires {
            return None;
        }
        let mut attempt = [0; 32];
        rand::rng().fill_bytes(&mut attempt);
        let activation = crate::storage::token_usage::ExternalSourceActivation::new(
            attempt, digest, before, expires,
        )
        .ok()?;
        Some(Self {
            key,
            store: work.source_store.as_ref()?.clone(),
            activation,
            receipt: OnceLock::new(),
            admission: OnceLock::new(),
            acceptance: OnceLock::new(),
            retirement: OnceLock::new(),
            retirement_revision: OnceLock::new(),
            retirement_state: OnceLock::new(),
            retirement_recovery: OnceLock::new(),
            #[cfg(test)]
            retirement_ack_lost: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Performs at most one prepared-only write on the original native worker.
    /// Cache both success and error with the immutable proposal. Acceptance
    /// requires a separately sealed actual actor admission and worker phase.
    fn prepare(&self, store: &crate::storage::token_usage::TokenUsageStore) {
        let _ = self
            .receipt
            .get_or_init(|| store.prepare_external_source_activation(&self.key, &self.activation));
    }

    /// Pure inspection used by the actor; no storage read or new attempt occurs.
    pub(super) fn outcome_is_prepared(&self) -> bool {
        self.receipt.get().is_some_and(|result| {
            result.as_ref().is_ok_and(|receipt| {
                receipt.state
                    == crate::storage::token_usage::ExternalSourceActivationState::Prepared
                    && receipt.revision == 1
            })
        })
    }

    /// Requires an actual actor grant and original successful preparation, then
    /// performs at most one exact acceptance write on the existing worker. A
    /// failed or ambiguous result is immutable; it is not silently retried.
    pub(super) fn accept(
        &self,
        store: &crate::storage::token_usage::TokenUsageStore,
    ) -> Result<()> {
        self.admission
            .get()
            .ok_or_else(|| MezError::forbidden("external source actor admission unavailable"))?;
        let prepared = self
            .receipt
            .get()
            .and_then(|result| result.as_ref().ok())
            .ok_or_else(|| MezError::conflict("external source preparation unavailable"))?;
        self.acceptance
            .get_or_init(|| {
                let now = super::source_clock::native_boot_millis().ok_or_else(|| {
                    MezError::conflict("external source activation clock unavailable")
                })?;
                store.accept_external_source_activation(
                    &self.key,
                    &self.activation,
                    prepared.revision,
                    now,
                )
            })
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone)
    }

    /// Returns the originally captured store, not a current configuration lookup.
    pub(super) fn original_store(&self) -> &crate::storage::token_usage::TokenUsageStore {
        &self.store
    }

    /// Reconciles only the exact immutable proposal, then tombstones its actual
    /// revision. Called under the slot's worker-only lock after logical retire.
    /// A cached failure is retained, not silently retried or called success.
    pub(super) fn retire(&self) -> Result<()> {
        self.retirement
            .get_or_init(|| {
                let receipt = self
                    .store
                    .external_source_activation(&self.key)?
                    .filter(|receipt| receipt.activation == self.activation)
                    .ok_or_else(|| {
                        MezError::conflict("external source retirement proposal changed")
                    })?;
                if receipt.state
                    == crate::storage::token_usage::ExternalSourceActivationState::Retired
                {
                    let _ = self.retirement_revision.set(receipt.revision);
                    return Ok(receipt);
                }
                let revision = receipt
                    .revision
                    .checked_add(1)
                    .filter(|revision| *revision <= i64::MAX as u64)
                    .ok_or_else(|| {
                        MezError::invalid_state("external source retirement revision exhausted")
                    })?;
                let _ = self.retirement_revision.set(revision);
                let _ = self.retirement_state.set(receipt.state);
                let retired = self.store.retire_external_source_activation(
                    &self.key,
                    &self.activation,
                    receipt.revision,
                )?;
                #[cfg(test)]
                self.maybe_lose_retirement_ack_for_tests()?;
                Ok(retired)
            })
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone)
    }

    /// Reconciles a previously attempted retirement by read-only exact facts.
    /// No failed write is repeated and the original cached outcome is retained.
    /// A prepared/accepted/changed/missing receipt cannot be reported retired.
    pub(super) fn reconcile_retirement(&self) -> Result<()> {
        if self.admission.get().is_none() || self.retirement.get().is_none() {
            return Err(MezError::invalid_state(
                "external source retirement evidence unavailable",
            ));
        }
        let revision = self.retirement_revision.get().ok_or_else(|| {
            MezError::invalid_state("external source retirement target unavailable")
        })?;
        if self
            .store
            .external_source_activation(&self.key)?
            .is_some_and(|receipt| {
                receipt.activation == self.activation
                    && receipt.revision == *revision
                    && receipt.state
                        == crate::storage::token_usage::ExternalSourceActivationState::Retired
            })
        {
            Ok(())
        } else {
            Err(MezError::conflict(
                "external source retirement evidence unavailable",
            ))
        }
    }

    /// Allows one persistence-only recovery after a read proves that the exact
    /// original predecessor is still uncommitted. The ordinary store CAS then
    /// rechecks proposal/revision under writer ownership. A changed/unknown row
    /// never authorizes a retry; neither cached result is replaced. Call only
    /// under the original slot's blocking-worker mutex after logical closure.
    pub(super) fn recover_retirement(&self) -> Result<()> {
        use crate::storage::token_usage::ExternalSourceActivationState;
        if self.admission.get().is_none() || self.retirement.get().is_none() {
            return Err(MezError::invalid_state(
                "external source retirement evidence unavailable",
            ));
        }
        let target = self.retirement_revision.get().copied().ok_or_else(|| {
            MezError::invalid_state("external source retirement target unavailable")
        })?;
        let receipt = self
            .store
            .external_source_activation(&self.key)?
            .filter(|receipt| receipt.activation == self.activation)
            .ok_or_else(|| MezError::conflict("external source retirement proposal changed"))?;
        if receipt.state == ExternalSourceActivationState::Retired && receipt.revision == target {
            return Ok(());
        }
        if self.retirement_state.get() != Some(&receipt.state)
            || !matches!(
                receipt.state,
                ExternalSourceActivationState::Prepared | ExternalSourceActivationState::Accepted
            )
            || receipt.revision.checked_add(1) != Some(target)
        {
            return Err(MezError::conflict(
                "external source retirement predecessor changed",
            ));
        }
        let _ = self.retirement_recovery.get_or_init(|| {
            let retired = self.store.retire_external_source_activation(
                &self.key,
                &self.activation,
                receipt.revision,
            )?;
            #[cfg(test)]
            self.maybe_lose_retirement_ack_for_tests()?;
            Ok(retired)
        });
        // Neither a lost receipt nor cached success may override fresh
        // contradictory evidence. This read cannot replenish the one CAS.
        self.reconcile_retirement()
    }

    /// Suppresses only a fixture's receipt AFTER either real CAS commits.
    #[cfg(test)]
    fn maybe_lose_retirement_ack_for_tests(&self) -> Result<()> {
        if self
            .retirement_ack_lost
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            Err(MezError::invalid_state(
                "fixture retirement acknowledgment lost",
            ))
        } else {
            Ok(())
        }
    }

    /// Inspects the separately cached bounded recovery, never a retry trigger.
    #[cfg(test)]
    pub(super) fn recovery_outcome_for_tests(
        &self,
    ) -> Option<&Result<crate::storage::token_usage::ExternalSourceActivationReceipt>> {
        self.retirement_recovery.get()
    }

    /// Arms a fixture-only lost receipt after an actual successful store commit.
    /// The original native preparation and actor grant must already exist.
    #[cfg(test)]
    pub(super) fn lose_retirement_ack_for_tests(&self) {
        assert!(self.admission.get().is_some());
        assert!(self.retirement.get().is_none());
        self.retirement_ack_lost
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Inspects the original outcome without replacing it by reconciled truth.
    #[cfg(test)]
    pub(super) fn retirement_outcome_for_tests(
        &self,
    ) -> Option<&Result<crate::storage::token_usage::ExternalSourceActivationReceipt>> {
        self.retirement.get()
    }

    /// Exposes this fixture's original cached write result, not a constructor,
    /// retry trigger or substitute for native/actor admission.
    #[cfg(test)]
    pub(super) fn outcome_for_tests(
        &self,
    ) -> Option<&Result<crate::storage::token_usage::ExternalSourceActivationReceipt>> {
        self.receipt.get()
    }
}

impl ExternalEnrollmentWork {
    /// Resolves only the original worker's qualified source lifetime. Helpers
    /// cannot allocate sources; failed/missing evidence retains unavailable
    /// accounting and a mapping row never substitutes for native authorization.
    pub(super) fn observe_source_owner(&self, producer: &ProducerEvidence) {
        let owner = (|| {
            if self.helper.is_some() || Instant::now() >= self.deadline {
                return None;
            }
            let store = self.source_store.as_ref()?;
            let boot = native_boot_uuid()?;
            let identity = producer.reobserve().ok()?;
            let kind = match producer {
                ProducerEvidence::Socket(_) => NativeSourceKind::Socket,
                ProducerEvidence::VerifiedParent(_) => NativeSourceKind::CuratedCreator,
            };
            let key = ExternalSourceKey::from_native(
                &self.harness,
                NativeSourceWitness {
                    boot: &boot,
                    uid: producer.uid(),
                    root: (self.process.process_id, self.process.start_token),
                    producer: (
                        identity.process_id,
                        identity.parent_process_id,
                        identity.start_token,
                    ),
                    kind,
                    session: &self.session_id,
                },
            )
            .ok()?;
            if matches!(producer, ProducerEvidence::VerifiedParent(_)) {
                return store.external_source_owner(&key).ok();
            }
            let resolved = store.resolve_external_source_owner(&key).ok()?;
            let _ = self.source_created.set(resolved.created);
            if resolved.created
                && self.source_preparation.has_reservation()
                && let Some(preparation) = NativeSourcePreparation::capture(self, producer, key)
            {
                // Publish immutable inputs before I/O so a lost/failed write
                // cannot cause this worker to mint a different attempt.
                if self.source_preparation.set(preparation)
                    && let Some(original) = self.source_preparation.get()
                {
                    original.prepare(store);
                }
            }
            // Optional preparation failure cannot remove independently proven
            // namespace identity or weaken current lifecycle/reporting fences.
            // Cold recovery remains unavailable regardless of this new fact.
            Some(resolved.owner)
        })();
        // Native observation already has one-shot publication; a duplicate
        // worker cannot replace its original diagnostic namespace result.
        let _ = self.source_owner.set(owner);
    }
}

/// Fixed Linux kernel boot UUID, bounded even if procfs behaves unexpectedly.
#[cfg(target_os = "linux")]
fn native_boot_uuid() -> Option<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open("/proc/sys/kernel/random/boot_id")
        .ok()?
        .take(65)
        .read_to_end(&mut bytes)
        .ok()?;
    parse_boot_uuid(&bytes)
}

/// Darwin native boot-session UUID; unavailable interfaces fail closed rather
/// than substituting wall-clock boot time or a new daemon-lifetime random ID.
#[cfg(target_os = "macos")]
fn native_boot_uuid() -> Option<String> {
    let mut bytes = [0u8; 64];
    let mut len = bytes.len();
    // SAFETY: fixed NUL-terminated sysctl name and bounded writable buffer; the
    // kernel returns actual byte count and no mutation/new process is requested.
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || len > bytes.len() {
        return None;
    }
    parse_boot_uuid(&bytes[..len])
}

/// Unknown platforms cannot manufacture durable native boot identity.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn native_boot_uuid() -> Option<String> {
    None
}

/// Admits only one bounded UUID with a conventional kernel newline/NUL suffix.
/// Arbitrary whitespace/control bytes and malformed native output stay absent.
fn parse_boot_uuid(bytes: &[u8]) -> Option<String> {
    let bytes = bytes
        .strip_suffix(b"\n")
        .or_else(|| bytes.strip_suffix(b"\0"))
        .unwrap_or(bytes);
    if bytes.len() != 36
        || !bytes.iter().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                *b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
    {
        return None;
    }
    Some(std::str::from_utf8(bytes).ok()?.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Native UUID parsing accepts only fixed kernel output shape and preserves
    /// boot identity across case/suffix conventions without an invented fallback.
    #[test]
    fn external_source_boot_uuid_parser_is_bounded_and_exact() {
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        for bytes in [
            uuid.as_bytes().to_vec(),
            format!("{uuid}\n").into_bytes(),
            format!("{}\0", uuid.to_ascii_uppercase()).into_bytes(),
        ] {
            assert_eq!(parse_boot_uuid(&bytes).as_deref(), Some(uuid));
        }
        for bytes in [
            b"".as_slice(),
            b"../private",
            b"01234567-89ab-cdef-0123-456789abcdef\n\0",
            b"01234567-89ab-cdef-0123-456789abcdef extra",
        ] {
            assert!(parse_boot_uuid(bytes).is_none());
        }
    }
}
