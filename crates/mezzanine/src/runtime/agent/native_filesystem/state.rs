//! Exact filesystem-attempt ownership independent of worker and pane lifetimes.
//!
//! Allocation identity prevents an old actor's numeric attempt from authorizing
//! a replacement actor. Claims transfer work once; observed outcomes remain
//! immutable and retained even after cancellation. This owner does not infer
//! commit permission, producer termination or durable canonical settlement.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
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

/// Opaque actor allocation and checked sequence; public labels are not authority.
#[derive(Clone)]
pub(crate) struct NativeFilesystemAttempt {
    /// Actor allocation retained while any issued handle survives.
    realm: Arc<()>,
    /// Checked actor-local sequence, never reused in this allocation.
    sequence: u64,
}

impl PartialEq for NativeFilesystemAttempt {
    /// Compares actual actor allocation, not equal unit values or label text.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.realm, &other.realm) && self.sequence == other.sequence
    }
}
impl Eq for NativeFilesystemAttempt {}

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
}

impl Default for NativeFilesystemActions {
    /// Creates a fresh actor allocation without counters or locks shared globally.
    fn default() -> Self {
        Self {
            realm: Arc::new(()),
            next: 1,
            attempts: BTreeMap::new(),
            coordinator: NativePatchWriteCoordinator::default(),
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

impl NativeFilesystemActions {
    /// Reserves a bounded exact original slot, without dispatching a producer.
    pub(super) fn queue(
        &mut self,
        owner: NativeFilesystemOwner,
        source: NativeFilesystemContext,
        operation: NativePatchOperation,
        generation: u64,
        deadline: Option<Instant>,
    ) -> Result<NativeFilesystemAttempt> {
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
        let token = NativeFilesystemAttempt {
            realm: Arc::clone(&self.realm),
            sequence: self.next,
        };
        self.attempts.insert(
            self.next,
            Attempt {
                work: Arc::new(NativeFilesystemWork {
                    owner,
                    identity,
                    source,
                }),
                fence,
                phase: Phase::Queued,
                scopes: None,
                outcome: None,
                commits: BTreeSet::new(),
                failure: None,
            },
        );
        self.next = next;
        Ok(token)
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
            attempt: token.clone(),
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
            attempt: token.clone(),
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

    /// Keeps navigation blocked while any original filesystem owner is pending.
    pub(in crate::runtime::agent) fn pending_for_pane(&self, pane: &str) -> bool {
        self.attempts
            .values()
            .any(|entry| entry.work.owner.turn.pane_id == pane)
    }

    /// Transfers metadata only when no producer was ever admitted. Issued or
    /// retained outcomes cannot be freed using a cancellation observation alone.
    pub(super) fn take_unissued(
        &mut self,
        token: &NativeFilesystemAttempt,
    ) -> Result<Arc<NativeFilesystemWork>> {
        let entry = self
            .get(token)
            .ok_or_else(|| MezError::conflict("native filesystem attempt owner unavailable"))?;
        if !matches!(entry.phase, Phase::Queued | Phase::Rejected) {
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
