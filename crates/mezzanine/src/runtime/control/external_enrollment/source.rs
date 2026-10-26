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

/// Frozen initial/recovered activation and original native/acceptance outcomes.
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
    /// Public instance/operation digests only; never a private launch credential.
    observer: crate::storage::token_usage::ExternalSourceObserverFence,
    /// Exact current operation scope, shared with the anchor and retirement.
    observer_scope: Arc<super::source_observer::ObserverScope>,
    /// Freshly native-qualified explicit recovery of an existing activation.
    cold: Option<super::source_observer::ObserverMutation>,
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
    /// Frozen observer at the original attempt, never learned from a successor.
    retirement_observer: OnceLock<Option<crate::storage::token_usage::ExternalSourceObserverFence>>,
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
        let observer = crate::storage::token_usage::ExternalSourceObserverFence::initial(
            &work.observer_instance,
            attempt,
        )
        .ok()?;
        Some(Self {
            key,
            store: work.source_store.as_ref()?.clone(),
            activation,
            observer_scope: super::source_observer::ObserverScope::new(Arc::new(observer.clone())),
            observer,
            cold: None,
            receipt: OnceLock::new(),
            admission: OnceLock::new(),
            acceptance: OnceLock::new(),
            retirement: OnceLock::new(),
            retirement_revision: OnceLock::new(),
            retirement_state: OnceLock::new(),
            retirement_observer: OnceLock::new(),
            retirement_recovery: OnceLock::new(),
            #[cfg(test)]
            retirement_ack_lost: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Reproves exact original-kind native continuity and same-boot eligibility
    /// BEFORE any grant. No namespace or activation write occurs in this phase.
    fn capture_cold(
        work: &ExternalEnrollmentWork,
        producer: &ProducerEvidence,
        key: ExternalSourceKey,
    ) -> Option<Self> {
        let selector = work.cold_resume.as_ref()?;
        let before = super::source_clock::native_boot_millis()?;
        let stored = work
            .source_store
            .as_ref()?
            .external_source_activation(&key)
            .ok()??;
        if !work
            .source_store
            .as_ref()?
            .external_source_history_complete(&key)
            .ok()?
        {
            return None;
        }
        if !stored.is_accepted_at(before) || !selector.matches_activation(&stored.activation) {
            return None;
        }
        let mut preparation = Self::capture(work, producer, key)?;
        let after = super::source_clock::native_boot_millis()?;
        if after < before
            || !stored.is_accepted_at(after)
            || !stored
                .activation
                .same_native_ancestry(&preparation.activation)
        {
            return None;
        }
        let observer = stored.observer.as_ref()?;
        let mutation = selector.mutation(observer, &work.observer_instance).ok()?;
        preparation.activation = stored.activation.clone();
        preparation.observer = observer.clone();
        preparation.observer_scope = mutation.scope.clone();
        preparation.cold = Some(mutation);
        preparation.receipt.set(Ok(stored)).ok()?;
        Some(preparation)
    }

    /// Private evidence of this worker's native-qualified recovery mode.
    pub(super) fn is_cold(&self) -> bool {
        self.cold.is_some()
    }

    /// Retain predecessor seen-ID digests across control replacement without
    /// reconstructing raw historical labels or dropping their absorbing fence.
    pub(super) fn inherited_instance_fence(
        &self,
    ) -> Option<crate::storage::token_usage::ExternalSourceObserverFence> {
        self.is_cold().then(|| self.observer.clone())
    }

    /// Cached native-phase eligibility, rechecked against the constant-time
    /// suspend-aware boot clock before an exact already-published cold retry.
    pub(super) fn cold_observation_is_accepted_at(&self, now: u64) -> bool {
        self.is_cold()
            && self.receipt.get().is_some_and(|result| {
                result
                    .as_ref()
                    .is_ok_and(|receipt| receipt.is_accepted_at(now))
            })
    }

    /// Required cold publication executes only AFTER actual sealed actor grant,
    /// under the slot I/O lock. Shared native metadata qualification seals the
    /// exact commit into this scope before postchecks. Cache failures/ambiguity;
    /// reread only the exact own result, never adopt an unrelated newer fence.
    pub(super) fn accept_cold(
        &self,
        origin: &Arc<UnixOriginProcess>,
        ancestry: &UnixAncestryWitness,
        deadline: Instant,
    ) -> Result<()> {
        self.acceptance
            .get_or_init(|| {
                let mutation = self
                    .cold
                    .as_ref()
                    .ok_or_else(|| MezError::conflict("durable source claim unavailable"))?;
                self.update_native_metadata(origin, ancestry, deadline, Some(mutation))?;
                let receipt = self
                    .store
                    .external_source_activation(&self.key)?
                    .ok_or_else(|| MezError::conflict("durable source claim disappeared"))?;
                let now = super::source_clock::native_boot_millis()
                    .ok_or_else(|| MezError::conflict("durable source clock unavailable"))?;
                if receipt.activation != self.activation
                    || !receipt.is_accepted_at(now)
                    || receipt.observer.as_ref() != Some(mutation.scope.current().as_ref())
                {
                    return Err(MezError::conflict("durable source claim changed"));
                }
                Ok(receipt)
            })
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone)
    }

    /// Performs at most one prepared-only write on the original native worker.
    /// Cache both success and error with the immutable proposal. Acceptance
    /// requires a separately sealed actual actor admission and worker phase.
    fn prepare(&self, store: &crate::storage::token_usage::TokenUsageStore) {
        let _ = self
            .receipt
            .get_or_init(|| store.prepare_external_source_activation(&self.key, &self.activation));
    }

    /// Pure phase eligibility: actual Prepared initial work or freshly native-
    /// qualified Accepted cold predecessor. Neither is the cold claim itself.
    pub(super) fn outcome_is_prepared(&self) -> bool {
        self.receipt.get().is_some_and(|result| {
            result.as_ref().is_ok_and(|receipt| {
                (receipt.state
                    == crate::storage::token_usage::ExternalSourceActivationState::Prepared
                    && receipt.revision == 1)
                    || (self.is_cold()
                        && receipt.state
                            == crate::storage::token_usage::ExternalSourceActivationState::Accepted)
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
                store.accept_external_source_activation_with_observer(
                    &self.key,
                    &self.activation,
                    prepared.revision,
                    now,
                    Some(&self.observer),
                )
            })
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone)
    }

    /// Exposes only the original worker's successful actor-qualified acceptance
    /// cache. Injected completion, preparation and failed writes yield no receipt;
    /// this historical fact is not fresh native proof or cold admission.
    pub(super) fn accepted_receipt(
        &self,
    ) -> Option<&crate::storage::token_usage::ExternalSourceActivationReceipt> {
        self.admission.get()?;
        self.acceptance.get()?.as_ref().ok().filter(|receipt| {
            receipt.state == crate::storage::token_usage::ExternalSourceActivationState::Accepted
        })
    }

    /// Reprove the original native source before recording history uncertainty.
    /// This neither admits an observer nor extends a lifetime. Prepared rows
    /// retain the marker through their independent later initial acceptance.
    pub(super) fn mark_observer_history(
        &self,
        origin: &Arc<UnixOriginProcess>,
        ancestry: &UnixAncestryWitness,
        deadline: Instant,
        previous: &crate::storage::token_usage::ExternalSourceObserverFence,
        nonce: [u8; 32],
    ) -> Result<()> {
        let unavailable = || MezError::conflict("source history native proof unavailable");
        self.admission.get().ok_or_else(unavailable)?;
        let before = super::source_clock::native_boot_millis().ok_or_else(unavailable)?;
        let native = origin.reobserve().map_err(|_| unavailable())?;
        let digest = ancestry
            .continuity_digest(origin.uid(), native)
            .map_err(|_| unavailable())?;
        if !origin.writer_confirmed()
            || !self.activation.matches_native_ancestry(&digest)
            || origin.reobserve().map_err(|_| unavailable())? != native
            || !ancestry.is_live()
            || Instant::now() >= deadline
        {
            return Err(unavailable());
        }
        let after = super::source_clock::native_boot_millis().ok_or_else(unavailable)?;
        if after < before {
            return Err(unavailable());
        }
        self.store.mark_external_source_history_gap(
            &self.key,
            &self.activation,
            previous,
            nonce,
        )?;
        if Instant::now() >= deadline
            || !ancestry.is_live()
            || origin.reobserve().map_err(|_| unavailable())? != native
        {
            return Err(unavailable());
        }
        Ok(())
    }

    /// Requalifies the exact original native source and bracketed boot clock on
    /// a blocking worker, then extends only its current accepted store revision.
    /// The immutable original proposal/store never changes. This is inert metadata,
    /// not observer admission, public publication or cold recovery authority.
    pub(super) fn renew_native(
        &self,
        origin: &Arc<UnixOriginProcess>,
        ancestry: &UnixAncestryWitness,
        deadline: Instant,
    ) -> Result<()> {
        self.update_native_metadata(origin, ancestry, deadline, None)
    }

    /// Shares exact original-kind native/clock qualification with lifetime
    /// renewal. Observer updates preserve the existing lifetime window and seal
    /// their own commit receipt before any fallible postcommit proof checks.
    pub(super) fn update_native_metadata(
        &self,
        origin: &Arc<UnixOriginProcess>,
        ancestry: &UnixAncestryWitness,
        deadline: Instant,
        observer: Option<&super::source_observer::ObserverMutation>,
    ) -> Result<()> {
        let unavailable = || MezError::conflict("external source renewal proof unavailable");
        self.admission.get().ok_or_else(unavailable)?;
        let previous = self
            .store
            .external_source_activation(&self.key)?
            .filter(|receipt| receipt.activation == self.activation)
            .ok_or_else(unavailable)?;
        let before = super::source_clock::native_boot_millis().ok_or_else(unavailable)?;
        if !origin.writer_confirmed() || !origin.observer_connected() {
            return Err(unavailable());
        }
        let native = origin.reobserve().map_err(|_| unavailable())?;
        let digest = ancestry
            .continuity_digest(origin.uid(), native)
            .map_err(|_| unavailable())?;
        if !self.activation.matches_native_ancestry(&digest)
            || origin.reobserve().map_err(|_| unavailable())? != native
            || !origin.observer_connected()
            || !ancestry.is_live()
            || Instant::now() >= deadline
        {
            return Err(unavailable());
        }
        let after = super::source_clock::native_boot_millis().ok_or_else(unavailable)?;
        if after < before || !previous.is_accepted_at(after) || Instant::now() >= deadline {
            return Err(unavailable());
        }
        let receipt = if let Some(observer) = observer {
            let receipt = if observer.history_nonce.is_some() {
                self.store.advance_external_source_observer_history(
                    &self.key,
                    &self.activation,
                    previous.revision,
                    &observer.change,
                    after,
                    observer.history_nonce,
                )?
            } else {
                self.store.advance_external_source_observer(
                    &self.key,
                    &self.activation,
                    previous.revision,
                    &observer.change,
                    after,
                )?
            };
            observer
                .scope
                .committed(receipt.observer.clone().ok_or_else(unavailable)?)?;
            receipt
        } else {
            let expires = before.checked_add(60_000).ok_or_else(unavailable)?;
            let renewal = crate::storage::token_usage::ExternalSourceRenewal::new(before, expires)?;
            self.store.renew_external_source_activation(
                &self.key,
                &self.activation,
                previous.revision,
                &renewal,
                after,
            )?
        };
        if Instant::now() >= deadline
            || !origin.observer_connected()
            || !ancestry.is_live()
            || origin.reobserve().map_err(|_| unavailable())? != native
        {
            return Err(unavailable());
        }
        let completed = super::source_clock::native_boot_millis().ok_or_else(unavailable)?;
        if completed < after || !receipt.is_accepted_at(completed) || Instant::now() >= deadline {
            return Err(unavailable());
        }
        Ok(())
    }

    /// Returns the originally captured store, not a current configuration lookup.
    pub(super) fn original_store(&self) -> &crate::storage::token_usage::TokenUsageStore {
        &self.store
    }

    /// Exact initial/cold operation scope; these bytes are not native authority.
    pub(super) fn initial_observer_scope(&self) -> Arc<super::source_observer::ObserverScope> {
        self.observer_scope.clone()
    }

    /// Returns only this genuine fixture's captured selectors/public fence for
    /// storage-race controls; never constructs native proof or a launch token.
    #[cfg(test)]
    pub(super) fn observer_scope_for_tests(
        &self,
    ) -> (
        crate::storage::token_usage::ExternalSourceKey,
        crate::storage::token_usage::ExternalSourceActivation,
        crate::storage::token_usage::ExternalSourceObserverFence,
    ) {
        (
            self.key.clone(),
            self.activation.clone(),
            self.observer.clone(),
        )
    }

    /// Reconciles only the exact immutable proposal and OUR captured observer,
    /// then tombstones its actual revision. A newer stored fence is not adopted.
    /// Called under the slot's worker-only lock after logical retire.
    /// A cached failure is retained, not silently retried or called success.
    #[cfg(test)]
    pub(super) fn retire(&self) -> Result<()> {
        self.retire_observer(&self.observer)
    }

    /// The actor's immutable operation scope is resolved under original source
    /// I/O ownership; never substitute the latest stored observer as a target.
    pub(super) fn retire_observer(
        &self,
        expected: &crate::storage::token_usage::ExternalSourceObserverFence,
    ) -> Result<()> {
        self.retirement
            .get_or_init(|| {
                let receipt = self
                    .store
                    .external_source_activation(&self.key)?
                    .filter(|receipt| receipt.activation == self.activation)
                    .ok_or_else(|| {
                        MezError::conflict("external source retirement proposal changed")
                    })?;
                // Prepared (or its Retired2 tombstone) has no accepted observer.
                // Every accepted source must still carry OUR original fence;
                // reading a newer row cannot transfer retirement authority.
                let observer = if receipt.state
                    == crate::storage::token_usage::ExternalSourceActivationState::Prepared
                    || (receipt.state
                        == crate::storage::token_usage::ExternalSourceActivationState::Retired
                        && receipt.revision == 2)
                {
                    None
                } else {
                    Some(expected.clone())
                };
                if receipt.observer != observer {
                    return Err(MezError::conflict(
                        "external source retirement observer changed",
                    ));
                }
                let _ = self.retirement_observer.set(observer.clone());
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
                let retired = self.store.retire_external_source_activation_with_observer(
                    &self.key,
                    &self.activation,
                    receipt.revision,
                    observer.as_ref(),
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
    /// A prepared/accepted/changed/missing receipt or another observer's equal-
    /// revision tombstone cannot be reported retired by this original attempt.
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
                    && self.retirement_observer.get() == Some(&receipt.observer)
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
            .filter(|receipt| {
                receipt.activation == self.activation
                    && self.retirement_observer.get() == Some(&receipt.observer)
            })
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
            let observer = self.retirement_observer.get().ok_or_else(|| {
                MezError::conflict("external source retirement observer unavailable")
            })?;
            let retired = self.store.retire_external_source_activation_with_observer(
                &self.key,
                &self.activation,
                receipt.revision,
                observer.as_ref(),
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
            if self.cold_resume.is_some() {
                let owner = store.find_external_source_owner(&key).ok()??;
                let preparation = NativeSourcePreparation::capture_cold(self, producer, key)?;
                if !self.source_preparation.set(preparation) {
                    return None;
                }
                let _ = self.source_created.set(false);
                return Some(owner);
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
