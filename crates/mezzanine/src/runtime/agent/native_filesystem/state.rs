//! Exact filesystem-attempt ownership independent of worker and pane lifetimes.
//!
//! Allocation identity prevents an old actor's numeric attempt from authorizing
//! a replacement actor. Claims transfer work once; observed outcomes remain
//! immutable and retained even after cancellation. This owner does not infer
//! commit permission, producer termination or durable canonical settlement.
//! Shared queue enrollment retains the actual root before first claim; offered
//! tokens own that cell, while producer/cached evidence tokens are non-owning.
//! This preserves original recovery without a source/producer custody cycle.

use crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use mez_agent::native_action::{NativeCommitIdentity, NativePatchOperation};
use mez_agent::permissions::{PathScopes, PermissionEvaluation};
use mez_agent::{AgentAction, AgentTurnRecord, ContextExecutionGroupId};

use crate::error::{MezError, Result};
use crate::runtime::processes::native_cancellation::NativeActionCancellation;
use crate::runtime::processes::native_filesystem_context::NativeFilesystemContext;
use crate::runtime::processes::native_patch::commit::{
    NativePatchCommitFence, NativePatchWriteCoordinator,
};
use crate::runtime::processes::native_patch::worker::NativePatchWorkerOutcome;

/// Maximum admitted active/untransferred owners, not an unbounded result archive.
const MAX_ATTEMPTS: usize = 4;

/// One immutable producer enrollment owned by the actor and offered-source
/// custodians. Producers retain only a weak link back, never their own owner.
type NativeFilesystemEnrollment = Mutex<Option<NativeFilesystemProducer>>;

/// Opaque actor allocation and checked sequence; public labels are not authority.
#[derive(Clone)]
pub(crate) struct NativeFilesystemAttempt {
    /// Actor allocation retained while any issued handle survives.
    realm: Arc<()>,
    /// Checked actor-local sequence, never reused in this allocation.
    sequence: u64,
    /// Reduce-only cancellation reaches queued state before any actor RPC/claim.
    fence: NativePatchCommitFence,
    /// Exact source enrollment; this weak link grants no claim or commit rights.
    enrollment: Weak<NativeFilesystemEnrollment>,
    /// Offered-source lifetime only. Producer/cache observers deliberately omit
    /// this anchor so source -> producer -> source cannot retain itself.
    _custody: Option<Arc<NativeFilesystemEnrollment>>,
}

impl PartialEq for NativeFilesystemAttempt {
    /// Compares actual actor allocation, not equal unit values or label text.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.realm, &other.realm) && self.sequence == other.sequence
    }
}
impl Eq for NativeFilesystemAttempt {}

impl PartialOrd for NativeFilesystemAttempt {
    /// Strong realm allocations make this in-process source ordering ABA-safe.
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for NativeFilesystemAttempt {
    /// Pointer ordering is private source bookkeeping, never serialized authority.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Arc::as_ptr(&self.realm)
            .cmp(&Arc::as_ptr(&other.realm))
            .then(self.sequence.cmp(&other.sequence))
    }
}
impl NativeFilesystemAttempt {
    /// Preserves source identity/cancellation without owning the enrollment cell.
    /// Producer-owned state and cached storage gates must use this form to avoid
    /// keeping their own actual producer alive through a strong custody cycle.
    pub(crate) fn non_owning(&self) -> Self {
        Self {
            realm: self.realm.clone(),
            sequence: self.sequence,
            fence: self.fence.clone(),
            enrollment: self.enrollment.clone(),
            _custody: None,
        }
    }

    /// Observes allocation liveness only for custody-cycle regression controls.
    /// This is not worker termination, source acceptance or permission evidence.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn enrollment_alive_for_tests(&self) -> bool {
        self.enrollment.upgrade().is_some()
    }

    /// Recovers or creates the actual original root before its first claim poll.
    /// Only enrollment changes: scope/worker/commit admission remain actor-owned.
    /// Expired/poisoned source custody fails without manufacturing a replacement.
    pub(crate) fn root_producer(&self) -> Result<NativeFilesystemProducer> {
        let enrollment = self
            .enrollment
            .upgrade()
            .ok_or_else(|| MezError::conflict("native filesystem original enrollment ended"))?;
        let mut current = enrollment.lock().map_err(|_| {
            MezError::invalid_state("native filesystem enrollment custody poisoned")
        })?;
        if let Some(original) = &*current {
            return Ok(original.clone());
        }
        let producer = NativeFilesystemProducer::new(self.non_owning());
        *current = Some(producer.clone());
        Ok(producer)
    }

    /// Registers exactly this allocation before releasing a host root claim gate.
    /// Equal labels cannot replace an enrolled producer; no scope is granted.
    pub(crate) fn register_root(&self, producer: &NativeFilesystemProducer) -> Result<()> {
        if producer.attempt() != self
            || !Weak::ptr_eq(&self.enrollment, &producer.attempt().enrollment)
        {
            return Err(MezError::conflict(
                "native filesystem enrollment source changed",
            ));
        }
        let enrollment = self
            .enrollment
            .upgrade()
            .ok_or_else(|| MezError::conflict("native filesystem original enrollment ended"))?;
        let mut current = enrollment.lock().map_err(|_| {
            MezError::invalid_state("native filesystem enrollment custody poisoned")
        })?;
        if current
            .as_ref()
            .is_some_and(|original| !original.same(producer))
        {
            return Err(MezError::conflict(
                "native filesystem producer allocation changed",
            ));
        }
        *current = Some(producer.clone());
        Ok(())
    }

    /// Monotonically fences the original queued or issued work, granting no rights.
    pub(crate) fn cancel(&self) {
        self.fence.cancel();
    }
}

impl std::fmt::Debug for NativeFilesystemAttempt {
    /// Prints only a numeric identifier, without addresses or patch payloads.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeFilesystemAttempt")
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

/// Original canonical slot, not the currently displayed provider response.
pub(crate) struct NativeFilesystemOwner {
    /// Frozen original logical turn and conversation attribution.
    pub(crate) turn: AgentTurnRecord,
    /// Exact model-authored action accepted in that original group.
    pub(crate) action: AgentAction,
    /// Original canonical execution group.
    pub(crate) group: ContextExecutionGroupId,
    /// Original action occurrence within the group.
    pub(crate) ordinal: usize,
    /// Immutable original policy evidence; never reconstructed from a summary.
    pub(crate) permission: Option<Box<PermissionEvaluation>>,
    /// Original configured sandbox intent, never a claim of OS confinement.
    pub(crate) sandbox_intent: &'static str,
    /// Private proof of the actual original approved request, if one was needed.
    pub(in crate::runtime::agent) approval: Option<super::authorization::NativeFilesystemApproval>,
    /// The controller planned this exact accepted action under auto-allow policy.
    pub(in crate::runtime::agent) auto_allowed: bool,
}

/// Immutable admitted inputs shared with exactly one claimed producer.
pub(crate) struct NativeFilesystemWork {
    /// Exact original canonical slot and permission evidence.
    pub(crate) owner: NativeFilesystemOwner,
    /// Exact transaction generation and semantic operation.
    pub(crate) identity: NativeCommitIdentity,
    /// Queued source data; not a write or worker-admission capability.
    pub(crate) source: NativeFilesystemContext,
}

/// Opaque evidence that this actor still retains the accepted original slot.
/// Only the registry constructs it; copied source data or CID labels cannot.
pub(crate) struct NativeFilesystemRegisteredSource {
    /// Exact actor allocation/sequence whose registration must remain live.
    attempt: NativeFilesystemAttempt,
    /// Original immutable slot whose allocation must match the registry.
    work: Arc<NativeFilesystemWork>,
}

impl NativeFilesystemRegisteredSource {
    /// Returns the registered turn, never the currently displayed response.
    pub(crate) fn turn(&self) -> &AgentTurnRecord {
        &self.work.owner.turn
    }
    /// Returns source data only in combination with actual registry evidence.
    pub(crate) fn source(&self) -> &NativeFilesystemContext {
        &self.work.source
    }
}

/// Work claimed once, with a scope ceiling but no actor-issued commit lease.
pub(crate) struct RuntimeNativeFilesystemDispatch {
    /// Exact actor-attempt handle used for subsequent messages.
    pub(crate) attempt: NativeFilesystemAttempt,
    /// Original admitted immutable metadata, independent of pane projections.
    pub(crate) work: Arc<NativeFilesystemWork>,
    /// Current captured maximum scope; later grants may only narrow.
    pub(crate) scopes: PathScopes,
    /// Exact action cancellation and finite admission deadline.
    pub(crate) fence: NativePatchCommitFence,
}

/// Ownership stages; retaining an outcome is not producer-join or durability proof.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Queued,
    Issued,
    Rejected,
    Retained,
    /// Positive unissued source, never native dispatch or joined execution.
    RefusalRetained,
}

/// One bounded actual owner with its original observed result, if available.
struct Attempt {
    /// Immutable original execution/action/source metadata.
    work: Arc<NativeFilesystemWork>,
    /// Monotonic cancellation shared with the actual claimed worker.
    fence: NativePatchCommitFence,
    /// Current sole ownership stage.
    phase: Phase,
    /// Captured scope ceiling remains available throughout claimed execution.
    scopes: Option<PathScopes>,
    /// Original immutable result; retries never clone or replay its effects.
    outcome: Option<Arc<NativePatchWorkerOutcome>>,
    /// Actual original producer's immutable no-dispatch proof.
    refusal: Option<crate::runtime::processes::native_filesystem_producer::NativeFilesystemRefusal>,
    /// Shared producer registration survives lost claim replies and monitors.
    enrollment: Arc<NativeFilesystemEnrollment>,
    /// Issued endpoint admissions are never replayed after delivery uncertainty.
    commits: BTreeSet<(usize, usize)>,
    /// Failed preparation or outcome validation, without losing result custody.
    failure: Option<Box<MezError>>,
}

/// Actor-owned bounded live filesystem work, separate from spawned-shell state.
pub(in crate::runtime::agent) struct NativeFilesystemActions {
    /// Exact owner allocation, invalidating all handles from replaced actors.
    realm: Arc<()>,
    /// Checked next identity; exhaustion fails instead of aliasing older work.
    next: u64,
    /// Live/untransferred owners only, bounded before admission.
    attempts: BTreeMap<u64, Attempt>,
    /// One explicitly shared writer exclusion per actor instance.
    coordinator: NativePatchWriteCoordinator,
    /// Bounded semantic history exists only for actual provider-selected turns.
    pub(in crate::runtime::agent) selection: super::selection::NativeFilesystemSelection,
    /// Exact approved actions waiting for ordinary ordered selector admission.
    pub(super) approvals:
        BTreeMap<(String, String), super::authorization::NativeFilesystemApproval>,
}

impl Default for NativeFilesystemActions {
    /// Creates a fresh actor allocation without counters or locks shared globally.
    fn default() -> Self {
        Self {
            realm: Arc::new(()),
            next: 1,
            attempts: BTreeMap::new(),
            coordinator: NativePatchWriteCoordinator::default(),
            selection: super::selection::NativeFilesystemSelection::default(),
            approvals: BTreeMap::new(),
        }
    }
}
impl std::fmt::Debug for NativeFilesystemActions {
    /// Keeps exact actions, credentials, source data and outcome bodies private.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeFilesystemActions")
            .field("owners", &self.attempts.len())
            .finish_non_exhaustive()
    }
}

impl Attempt {
    /// Conserves the actual allocation even if a control lock was poisoned.
    /// This read grants no new enrollment, permission, join or storage evidence;
    /// mutating enrollment still fails closed on poison.
    fn producer(&self) -> Option<NativeFilesystemProducer> {
        match self.enrollment.lock() {
            Ok(current) => current.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl NativeFilesystemActions {
    /// Drops only volatile selected-turn history and unused approval proofs.
    /// Actual attempt/worker/original storage custody remains independently held.
    pub(in crate::runtime::agent) fn forget_selected_turn(&mut self, turn: &str) {
        self.selection.forget(turn);
        self.approvals.retain(|(owner, _), _| owner != turn);
    }

    /// Retains the original native domain for deferred selection or approval.
    /// This routing fact grants no permission: fresh source/decision validation
    /// remains mandatory even when the current pane mode chooses another adapter.
    pub(in crate::runtime::agent) fn requires_native_adapter(
        &self,
        turn: &str,
        action: &str,
    ) -> bool {
        self.selection.waits_for(turn, action)
            || self
                .approvals
                .contains_key(&(turn.to_string(), action.to_string()))
    }

    /// Capacity is backpressure before selection, never a mutation failure.
    pub(super) fn has_capacity(&self) -> bool {
        self.attempts.len() < MAX_ATTEMPTS
    }
    /// Reserves a bounded exact original slot, without dispatching a producer.
    pub(super) fn queue(
        &mut self,
        owner: NativeFilesystemOwner,
        source: NativeFilesystemContext,
        operation: NativePatchOperation,
        generation: u64,
        deadline: Option<Instant>,
    ) -> Result<(NativeFilesystemAttempt, Arc<NativeFilesystemWork>)> {
        if self.attempts.len() >= MAX_ATTEMPTS {
            return Err(MezError::conflict(
                "native filesystem attempt capacity exhausted",
            ));
        }
        if self.attempts.values().any(|entry| {
            entry.work.owner.turn.turn_id == owner.turn.turn_id
                && entry.work.owner.group == owner.group
                && entry.work.owner.action.id == owner.action.id
        }) {
            return Err(MezError::conflict(
                "native filesystem original slot already owns an attempt",
            ));
        }
        let next = self.next.checked_add(1).ok_or_else(|| {
            MezError::invalid_state("native filesystem attempt identity exhausted")
        })?;
        let fence = NativePatchCommitFence::new(NativeActionCancellation::new(), deadline)?;
        fence.check()?;
        let identity = NativeCommitIdentity {
            turn_id: owner.turn.turn_id.clone(),
            action_id: owner.action.id.clone(),
            transaction: format!("filesystem-{}", self.next),
            authority_generation: generation,
            operation,
        };
        let enrollment = Arc::new(Mutex::new(None));
        let token = NativeFilesystemAttempt {
            realm: Arc::clone(&self.realm),
            sequence: self.next,
            fence: fence.clone(),
            enrollment: Arc::downgrade(&enrollment),
            _custody: Some(enrollment.clone()),
        };
        let work = Arc::new(NativeFilesystemWork {
            owner,
            identity,
            source,
        });
        self.attempts.insert(
            self.next,
            Attempt {
                work: Arc::clone(&work),
                fence,
                phase: Phase::Queued,
                scopes: None,
                outcome: None,
                refusal: None,
                enrollment,
                commits: BTreeSet::new(),
                failure: None,
            },
        );
        self.next = next;
        Ok((token, work))
    }

    /// Offers original unclaimed sources, including fenced refusal metadata.
    /// Claim revalidates the fence; this list grants no execution rights. Paused owners
    /// already have root custody and must not produce duplicate producer offers.
    pub(super) fn pending(&self) -> Vec<NativeFilesystemAttempt> {
        self.attempts
            .iter()
            .filter(|(_, entry)| entry.phase == Phase::Queued && entry.producer().is_none())
            .map(|(sequence, entry)| NativeFilesystemAttempt {
                realm: self.realm.clone(),
                sequence: *sequence,
                fence: entry.fence.clone(),
                enrollment: Arc::downgrade(&entry.enrollment),
                _custody: Some(entry.enrollment.clone()),
            })
            .collect()
    }

    /// Recovers original untransferred sources and actual registered producers.
    /// This metadata snapshot cannot grant scopes or reconstruct a worker.
    pub(super) fn recoverable(
        &self,
    ) -> Vec<(
        NativeFilesystemAttempt,
        Option<crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer>,
    )> {
        self.attempts
            .iter()
            .map(|(sequence, entry)| {
                (
                    NativeFilesystemAttempt {
                        realm: self.realm.clone(),
                        sequence: *sequence,
                        fence: entry.fence.clone(),
                        enrollment: Arc::downgrade(&entry.enrollment),
                        _custody: Some(entry.enrollment.clone()),
                    },
                    entry.producer(),
                )
            })
            .collect()
    }

    /// Resolves only handles belonging to this actual actor allocation.
    fn get(&self, token: &NativeFilesystemAttempt) -> Option<&Attempt> {
        Arc::ptr_eq(&self.realm, &token.realm)
            .then(|| self.attempts.get(&token.sequence))
            .flatten()
    }

    /// Resolves mutable state without accepting a matching public sequence alone.
    fn get_mut(&mut self, token: &NativeFilesystemAttempt) -> Option<&mut Attempt> {
        Arc::ptr_eq(&self.realm, &token.realm)
            .then(|| self.attempts.get_mut(&token.sequence))
            .flatten()
    }

    /// Returns immutable queued inputs for actor-side current-source validation.
    pub(super) fn queued(
        &self,
        token: &NativeFilesystemAttempt,
    ) -> Option<Arc<NativeFilesystemWork>> {
        self.get(token)
            .filter(|entry| entry.phase == Phase::Queued)
            .map(|entry| Arc::clone(&entry.work))
    }

    /// Captures registration evidence for an accepted queued or issued owner.
    pub(super) fn registered_source(
        &self,
        token: &NativeFilesystemAttempt,
    ) -> Result<NativeFilesystemRegisteredSource> {
        let entry = self
            .get(token)
            .filter(|entry| matches!(entry.phase, Phase::Queued | Phase::Issued))
            .ok_or_else(|| {
                MezError::conflict("native filesystem original owner is not registered")
            })?;
        entry.fence.check()?;
        Ok(NativeFilesystemRegisteredSource {
            attempt: token.non_owning(),
            work: Arc::clone(&entry.work),
        })
    }

    /// Rechecks allocation/phase/cancellation instead of trusting an old proof.
    pub(super) fn registered_source_current(
        &self,
        source: &NativeFilesystemRegisteredSource,
    ) -> bool {
        self.get(&source.attempt).is_some_and(|entry| {
            matches!(entry.phase, Phase::Queued | Phase::Issued)
                && Arc::ptr_eq(&entry.work, &source.work)
                && entry.fence.check().is_ok()
        })
    }

    /// Records a preparation refusal with positive no-producer ownership intact.
    pub(super) fn reject_queued(&mut self, token: &NativeFilesystemAttempt, error: MezError) {
        if let Some(entry) = self
            .get_mut(token)
            .filter(|entry| entry.phase == Phase::Queued)
        {
            entry.fence.cancel();
            entry.failure = Some(Box::new(error));
            entry.phase = Phase::Rejected;
        }
    }

    /// Issues work once after validation; a repeated claim never starts work again.
    pub(super) fn claim(
        &mut self,
        token: &NativeFilesystemAttempt,
        scopes: PathScopes,
    ) -> Result<Option<RuntimeNativeFilesystemDispatch>> {
        let Some(entry) = self
            .get_mut(token)
            .filter(|entry| entry.phase == Phase::Queued)
        else {
            return Ok(None);
        };
        entry.fence.check()?;
        entry.scopes = Some(scopes.clone());
        entry.phase = Phase::Issued;
        Ok(Some(RuntimeNativeFilesystemDispatch {
            attempt: token.non_owning(),
            work: Arc::clone(&entry.work),
            scopes,
            fence: entry.fence.clone(),
        }))
    }

    /// Reads an exact issued scope ceiling; this is not an atomic commit lease.
    pub(super) fn issued(
        &self,
        token: &NativeFilesystemAttempt,
    ) -> Result<(Arc<NativeFilesystemWork>, PathScopes)> {
        let entry = self
            .get(token)
            .filter(|entry| entry.phase == Phase::Issued)
            .ok_or_else(|| MezError::conflict("native filesystem attempt is not issued"))?;
        entry.fence.check()?;
        let scopes = entry
            .scopes
            .clone()
            .ok_or_else(|| MezError::invalid_state("native filesystem claim lost scope ceiling"))?;
        Ok((Arc::clone(&entry.work), scopes))
    }

    /// Retains the actual root registration before issuing work. Replacing it
    /// with a same-label producer is forbidden; the same allocation may retry
    /// while a queued claim remains paused or otherwise temporarily unavailable.
    pub(super) fn register_producer(
        &mut self,
        token: &NativeFilesystemAttempt,
        producer: crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer,
    ) -> Result<()> {
        let entry = self
            .get_mut(token)
            .filter(|entry| entry.phase == Phase::Queued)
            .ok_or_else(|| {
                MezError::conflict("native filesystem producer registration is not queued")
            })?;
        if !Weak::ptr_eq(&token.enrollment, &Arc::downgrade(&entry.enrollment)) {
            return Err(MezError::conflict(
                "native filesystem enrollment allocation changed",
            ));
        }
        token.register_root(&producer)?;
        Ok(())
    }

    /// Recovers actual producer custody after a lost reply/monitor, without
    /// reconstructing a producer from equal action or attempt labels.
    pub(super) fn producer(
        &self,
        token: &NativeFilesystemAttempt,
    ) -> Option<crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer>
    {
        self.get(token).and_then(Attempt::producer)
    }

    /// Matches live display data only to a registered issued/retained producer.
    /// This predicate carries no scope, lease or mutation authority.
    pub(super) fn progress_is_registered(
        &self,
        progress: &mez_agent::ActionPresentationProgress,
    ) -> bool {
        self.attempts.values().any(|entry| {
            matches!(entry.phase, Phase::Issued | Phase::Retained)
                && entry.work.owner.turn.turn_id == progress.turn_id
                && entry.work.owner.action.id == progress.action_id
                && entry
                    .producer()
                    .as_ref()
                    .is_some_and(|producer| producer.matches_progress(progress))
        })
    }

    /// Acquires commit scheduling in the serialized actor after policy checks.
    /// A granted occurrence remains spent even if delivery or lease entry fails:
    /// neither uncertainty nor a later display retry authorizes mutation replay.
    pub(super) fn issue_commit(
        &mut self,
        token: &NativeFilesystemAttempt,
        proposal: &crate::runtime::processes::native_patch::lease::NativePatchCommitProposal,
        scopes: PathScopes,
    ) -> Result<crate::runtime::processes::native_patch::lease::NativePatchCommitLease> {
        let coordinator = self.coordinator.clone();
        let entry = self
            .get_mut(token)
            .filter(|entry| entry.phase == Phase::Issued)
            .ok_or_else(|| MezError::conflict("native filesystem commit owner is not issued"))?;
        proposal.authorize(&entry.work.identity, &scopes)?;
        if entry.commits.contains(&proposal.key()) {
            return Err(MezError::conflict(
                "native filesystem endpoint already admitted",
            ));
        }
        // Parent keys occupy a separate namespace from file endpoints 0/1.
        // Count admitted attempts, including failed/unknown ones: neither lost
        // delivery nor a later operation can renew the action's parent budget.
        if proposal.key().1 >= 2
            && entry
                .commits
                .iter()
                .filter(|(_, endpoint)| *endpoint >= 2)
                .count()
                >= crate::security::filesystem::capability::parent::MAX_NATIVE_PARENT_DIRECTORIES
        {
            return Err(MezError::conflict(
                "native parent admission count exceeds action budget",
            ));
        }
        let guard = entry.fence.try_begin(&coordinator)?;
        let lease = crate::runtime::processes::native_patch::lease::NativePatchCommitLease::issue(
            proposal.clone(),
            scopes,
            guard,
        )?;
        entry.commits.insert(proposal.key());
        Ok(lease)
    }

    /// Retains the original immutable producer result before any projection.
    /// Cancellation or changed presentation does not discard known effects.
    /// Even a mismatched outcome is retained and fenced for bounded diagnosis.
    pub(super) fn retain(
        &mut self,
        token: &NativeFilesystemAttempt,
        outcome: Arc<NativePatchWorkerOutcome>,
    ) -> Result<bool> {
        let entry = self.get_mut(token).ok_or_else(|| {
            MezError::conflict("native filesystem outcome has no issued actor owner")
        })?;
        if let Some(original) = &entry.outcome {
            return if Arc::ptr_eq(original, &outcome) {
                if let Some(error) = &entry.failure {
                    Err((**error).clone())
                } else {
                    Ok(false)
                }
            } else {
                Err(MezError::conflict(
                    "native filesystem attempt returned a different second outcome",
                ))
            };
        }
        if entry.phase != Phase::Issued {
            return Err(MezError::conflict(
                "native filesystem outcome was never issued",
            ));
        }
        entry.fence.cancel();
        entry.phase = Phase::Retained;
        let matches = outcome.result.identity == entry.work.identity;
        entry.outcome = Some(outcome);
        if !matches {
            let error = MezError::conflict(
                "native filesystem outcome differs from admitted semantic identity",
            );
            entry.failure = Some(Box::new(error.clone()));
            return Err(error);
        }
        Ok(true)
    }

    /// Returns retained immutable evidence for its original settlement owner.
    pub(super) fn retained(
        &self,
        token: &NativeFilesystemAttempt,
    ) -> Option<Arc<NativePatchWorkerOutcome>> {
        self.get(token)
            .and_then(|entry| entry.outcome.as_ref())
            .map(Arc::clone)
    }

    /// Reports preparation/validation failure without consuming original evidence.
    pub(super) fn failure(&self, token: &NativeFilesystemAttempt) -> Option<&MezError> {
        self.get(token).and_then(|entry| entry.failure.as_deref())
    }

    /// Monotonically fences all exact attempts of the requested original turn.
    pub(in crate::runtime::agent) fn cancel_turn(&self, turn: &str) {
        for entry in self
            .attempts
            .values()
            .filter(|entry| entry.work.owner.turn.turn_id == turn)
        {
            entry.fence.cancel();
        }
    }

    /// Fences one exact invalidated claim without cancelling unrelated siblings.
    pub(super) fn cancel_attempt(&self, token: &NativeFilesystemAttempt) {
        if let Some(entry) = self.get(token) {
            entry.fence.cancel();
        }
    }

    /// Retained original work is a recoverable progress owner until transfer,
    /// including queued refusal and checked-storage obligations after native join.
    pub(super) fn owns_turn(&self, turn: &str) -> bool {
        self.attempts
            .values()
            .any(|entry| entry.work.owner.turn.turn_id == turn)
    }

    /// Protects queued/issued native action ownership from shell redispatch or
    /// false unstarted-sibling classification. This grants no mutation or join proof.
    pub(in crate::runtime::agent) fn owns_action(&self, turn: &str, action: &str) -> bool {
        self.attempts.values().any(|entry| {
            entry.work.owner.turn.turn_id == turn && entry.work.owner.action.id == action
        })
    }

    /// Keeps navigation blocked while any original filesystem owner is pending.
    pub(in crate::runtime::agent) fn pending_for_pane(&self, pane: &str) -> bool {
        self.attempts
            .values()
            .any(|entry| entry.work.owner.turn.pane_id == pane)
    }

    /// Verifies original actor allocation even after its active entry was removed.
    pub(in crate::runtime::agent) fn owns_realm(&self, token: &NativeFilesystemAttempt) -> bool {
        Arc::ptr_eq(&self.realm, &token.realm)
    }

    /// Removes one exact observed original owner after retaining its opaque ack.
    /// All checks precede the only fallible callback; successful retention is
    /// followed solely by infallible removal in this serialized actor operation.
    pub(in crate::runtime::agent) fn transfer_joined(
        &mut self,
        token: &NativeFilesystemAttempt,
        work: &Arc<NativeFilesystemWork>,
        producer: &crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer,
        raw: &Arc<NativePatchWorkerOutcome>,
        retain_acknowledgement: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let entry = self
            .get(token)
            .ok_or_else(|| MezError::conflict("filesystem transfer registry owner unavailable"))?;
        if let Some(error) = &entry.failure {
            return Err((**error).clone());
        }
        if entry.phase != Phase::Retained
            || !Arc::ptr_eq(&entry.work, work)
            || !entry
                .producer()
                .as_ref()
                .is_some_and(|original| original.same(producer))
            || !entry
                .outcome
                .as_ref()
                .is_some_and(|original| Arc::ptr_eq(original, raw))
        {
            return Err(MezError::conflict(
                "filesystem transfer original work or observed result changed",
            ));
        }
        retain_acknowledgement()?;
        // The exact validated entry cannot disappear: no callback has access to
        // actor state and no await/reentry occurs after acknowledgement retention.
        self.attempts.remove(&token.sequence);
        Ok(())
    }

    /// Validates original queued/rejected source without creating dispatch rights.
    pub(in crate::runtime::agent) fn unissued_source(
        &self,
        token: &NativeFilesystemAttempt,
        producer: &NativeFilesystemProducer,
    ) -> Result<(Arc<NativeFilesystemWork>, Option<MezError>)> {
        let entry = self
            .get(token)
            .ok_or_else(|| MezError::conflict("filesystem refusal source unavailable"))?;
        if !matches!(entry.phase, Phase::Queued | Phase::Rejected)
            || entry.scopes.is_some()
            || !entry.commits.is_empty()
            || entry.outcome.is_some()
            || !entry
                .producer()
                .is_some_and(|original| original.same(producer))
        {
            return Err(MezError::conflict(
                "filesystem refusal is not the original unissued producer",
            ));
        }
        Ok((entry.work.clone(), entry.failure.as_deref().cloned()))
    }

    /// Retains positive original no-dispatch facts independently of worker joins.
    pub(in crate::runtime::agent) fn retain_refusal(
        &mut self,
        token: &NativeFilesystemAttempt,
        refusal: crate::runtime::processes::native_filesystem_producer::NativeFilesystemRefusal,
    ) -> Result<()> {
        let entry = self
            .get_mut(token)
            .ok_or_else(|| MezError::conflict("filesystem refusal source unavailable"))?;
        if let Some(original) = &entry.refusal {
            return if original.same(&refusal) {
                Ok(())
            } else {
                Err(MezError::conflict("filesystem refusal proof changed"))
            };
        }
        if !matches!(entry.phase, Phase::Queued | Phase::Rejected)
            || !Arc::ptr_eq(&entry.work, refusal.work())
            || !entry
                .producer()
                .is_some_and(|producer| refusal.belongs_to(&producer))
            || !entry.commits.is_empty()
            || entry.scopes.is_some()
            || entry.outcome.is_some()
        {
            return Err(MezError::conflict(
                "filesystem refusal original source changed",
            ));
        }
        entry.fence.cancel();
        entry.outcome = Some(refusal.outcome().clone());
        entry.refusal = Some(refusal);
        entry.phase = Phase::RefusalRetained;
        Ok(())
    }

    /// Returns only actual retained no-dispatch provenance.
    pub(in crate::runtime::agent) fn refusal(
        &self,
        token: &NativeFilesystemAttempt,
    ) -> Option<crate::runtime::processes::native_filesystem_producer::NativeFilesystemRefusal>
    {
        self.get(token).and_then(|entry| entry.refusal.clone())
    }

    /// Removes only exact refused source after its opaque acknowledgement is stored.
    pub(in crate::runtime::agent) fn transfer_refused(
        &mut self,
        token: &NativeFilesystemAttempt,
        producer: &NativeFilesystemProducer,
        refusal: &crate::runtime::processes::native_filesystem_producer::NativeFilesystemRefusal,
        retain_ack: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let entry = self
            .get(token)
            .ok_or_else(|| MezError::conflict("filesystem refusal source unavailable"))?;
        if entry.phase != Phase::RefusalRetained
            || !Arc::ptr_eq(&entry.work, refusal.work())
            || !entry
                .refusal
                .as_ref()
                .is_some_and(|original| original.same(refusal))
            || !entry
                .outcome
                .as_ref()
                .is_some_and(|raw| Arc::ptr_eq(raw, refusal.outcome()))
            || !entry
                .producer()
                .is_some_and(|original| original.same(producer))
            || !refusal.belongs_to(producer)
            || !entry.commits.is_empty()
        {
            return Err(MezError::conflict(
                "filesystem refusal handoff original source changed",
            ));
        }
        retain_ack()?;
        self.attempts.remove(&token.sequence);
        Ok(())
    }

    /// Transfers metadata only before any producer enrollment. Registered root
    /// custody requires exact joined or refusal acceptance, never queue removal.
    pub(super) fn take_unissued(
        &mut self,
        token: &NativeFilesystemAttempt,
    ) -> Result<Arc<NativeFilesystemWork>> {
        let entry = self
            .get(token)
            .ok_or_else(|| MezError::conflict("native filesystem attempt owner unavailable"))?;
        if !matches!(entry.phase, Phase::Queued | Phase::Rejected) || entry.producer().is_some() {
            return Err(MezError::conflict(
                "native filesystem producer outcome still requires settlement",
            ));
        }
        entry.fence.cancel();
        self.attempts
            .remove(&token.sequence)
            .map(|entry| entry.work)
            .ok_or_else(|| MezError::invalid_state("native filesystem unissued owner disappeared"))
    }
}
