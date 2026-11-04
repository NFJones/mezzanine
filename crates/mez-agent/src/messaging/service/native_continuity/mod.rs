//! Captured native registration continuity, separate from restored authority.
//!
//! A trusted journal writes a snapshot captured from actual registration and
//! shell tokens. The live marker is published only after that exact input's
//! worker completes, and only while the actual binding still matches. Stored
//! snapshots remain descriptive and cannot construct registration or shell
//! tokens. Source admission/lifecycle barriers and qualified native restoration
//! belong to the product integration and must precede durable activation.

use serde::{Deserialize, Serialize};

use super::*;

/// Versioned nonauthorizing facts for one actual native registration interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageNativeContinuitySnapshot {
    /// Native attestation version, independent of the store row framing.
    pub format_version: u32,
    /// Version2 records whether the actual capturing owner required the native
    /// admission barrier. Legacy absence remains unqualified, never upgraded.
    #[serde(default)]
    pub admission_guarded: bool,
    /// Original native namespace, shell epoch, owner and availability facts.
    pub native: MessageNativeCheckpoint,
    /// Immutable registered identity; mutable objective telemetry is omitted.
    pub identity: MessageIdentitySnapshot,
    /// Original registration interval floor; never a delivery/ACK cursor.
    pub registered_after_sequence: MessageSequence,
}
impl MessageNativeContinuitySnapshot {
    /// Compares private snapshot routing data with actual trusted membership
    /// without exposing or reconstructing a live project authority from bytes.
    pub fn matches_project_scope(&self, scope: &Option<ProjectScopeId>) -> bool {
        match (&self.identity.project_scope, scope) {
            (None, None) => true,
            (Some(saved), Some(actual)) => saved == actual.snapshot_value(),
            _ => false,
        }
    }
    /// Validates descriptive framing and tuple derivation without creating any
    /// live owner. Actual store/context qualification remains a separate gate.
    pub fn validate(&self) -> Result<()> {
        if !matches!(self.format_version, 1 | 2)
            || (self.format_version == 1 && self.admission_guarded)
            || self.native.agent_id != self.identity.agent_id
            || self.identity.objective.is_some()
        {
            return Err(changed());
        }
        native_binding::validate_native_checkpoint(std::slice::from_ref(&self.native))?;
        sender_identity_from_snapshot(&self.identity, 9)?;
        Ok(())
    }
}

/// Shared exact captured input, kept charged outside the actual worker.
struct Capture {
    scope: MessageStorageScope,
    runtime: Arc<()>,
    identity: Arc<()>,
    registration: MessageSenderRegistration,
    incarnation: MessageNativeIncarnation,
    snapshot: MessageNativeContinuitySnapshot,
    lease: MessageStorageSnapshotLease,
}
/// Original actor-owned input, retained independently of its actual worker.
pub struct MessageNativeContinuityRequest(Arc<Capture>);
/// One actual worker's borrowed snapshot, sharing metadata without deep copies.
pub struct MessageNativeContinuityWrite(Arc<Capture>);
/// Exact successful trusted-port result; serialized bytes cannot create one.
pub struct MessageNativeContinuityVerified {
    identity: Arc<()>,
}

/// Trusted synchronous journal port, used only on the product's owned lane.
pub trait MessageNativeContinuityStore {
    /// Persists or reconciles this exact input under actual original ownership.
    /// Failure is not a noncommit claim; the same input must be retained/retried.
    fn commit(&mut self, write: &MessageNativeContinuityWrite) -> Result<()>;
}

impl MessageService {
    /// Enables the one-way product barrier under this actual original root.
    /// Unrecorded current native intervals then withhold all new admissions and
    /// duplicate acceptance replies, and cannot deliver native input. Existing
    /// source-bound ACKs remain settleable. Foreign scope or unsettled lower
    /// reservations refuse without changing policy; saved data cannot enable it.
    /// The first transition invalidates unguarded completions. Repeated calls
    /// preserve already recorded guarded bindings.
    pub fn require_native_continuity(&mut self, scope: &MessageStorageScope) -> Result<()> {
        self.require_storage_scope(Some(scope))?;
        if self.admission.is_some() || self.acknowledgement.is_some() {
            return Err(MessageError::conflict(
                "MMP native barrier requires settled reservations",
            ));
        }
        let policy = self.storage_custody.as_mut().ok_or_else(changed)?;
        if !policy.native_continuity_required {
            policy.native_continuity_required = true;
            for binding in self.native_bindings.values_mut() {
                binding.continuity_recorded = false;
            }
        }
        Ok(())
    }

    /// Borrows one known native endpoint requiring current observation/journaling.
    /// Lexical order is stable without allocating a side list or retaining
    /// identity copies. Primitive storage owners without the product barrier
    /// report none; actual product composition must explicitly enable it.
    pub fn pending_native_continuity(&self) -> Option<&AgentId> {
        if !self
            .storage_custody
            .as_ref()
            .is_some_and(|p| p.native_continuity_required)
        {
            return None;
        }
        self.native_bindings
            .iter()
            .filter(|(_, b)| {
                !self.sender_registration_is_current(b.registration()) || !b.continuity_recorded
            })
            .map(|(id, _)| id)
            .min_by(|left, right| left.as_str().cmp(right.as_str()))
    }

    /// Refuses before sequence/idempotency/reservation mutation. Actor-owned
    /// source queues wait for the separate metadata lane; direct ingress can
    /// return a never-accepted busy response without retaining another payload.
    pub(super) fn require_native_continuity_ready(&self) -> Result<()> {
        if self.pending_native_continuity().is_some() {
            return Err(MessageError::conflict(
                "MMP native registration continuity is pending",
            ));
        }
        Ok(())
    }

    /// Captures a current native interval from its actual live allocations.
    /// Foreign scope, stale/missing binding or exhausted metadata refuses before
    /// cloning identity fields. Raw endpoints never acquire journal facts.
    pub fn prepare_native_continuity(
        &self,
        scope: &MessageStorageScope,
        id: &AgentId,
    ) -> Result<MessageNativeContinuityRequest> {
        self.require_storage_scope(Some(scope))?;
        if self.admission.is_some() || self.acknowledgement.is_some() {
            return Err(MessageError::conflict(
                "MMP native continuity requires settled admission and ACK reservations",
            ));
        }
        let binding = self
            .native_bindings
            .get(id)
            .filter(|b| self.sender_registration_is_current(b.registration()))
            .ok_or_else(changed)?;
        let identity = self.registered.get(id).ok_or_else(changed)?;
        let policy = self.storage_custody.as_ref().ok_or_else(changed)?;
        let charge = checkpoint::identity_charge(identity)
            // Previous n1 decode, fixed lifetime images, current encoding and
            // exact batch reconciliation remain charged through actual join.
            .saturating_add(super::recovery::MESSAGE_STORAGE_RECOVERY_SCRATCH_METADATA)
            .saturating_add(16 * 1024)
            .saturating_add(id.as_str().len().saturating_mul(8));
        let lease = checkpoint::reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            charge,
            self.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )?;
        let native = binding.continuity_checkpoint();
        let mut identity = identity_snapshot(identity);
        identity.objective = None;
        let snapshot = MessageNativeContinuitySnapshot {
            format_version: 2,
            admission_guarded: policy.native_continuity_required,
            native,
            identity,
            registered_after_sequence: *self.registration_floors.get(id).ok_or_else(changed)?,
        };
        snapshot.validate()?;
        self.track_native_registration(binding.registration())?;
        Ok(MessageNativeContinuityRequest(Arc::new(Capture {
            scope: scope.clone(),
            runtime: self.acceptance_runtime.clone(),
            identity: Arc::new(()),
            registration: binding.registration().clone(),
            incarnation: binding.continuity_incarnation(),
            snapshot,
            lease,
        })))
    }

    /// Reports only actual same-runtime journal completion for the current
    /// binding. Saved facts and a foreign runtime can never set this marker.
    pub fn native_continuity_is_recorded(&self, id: &AgentId) -> bool {
        self.native_bindings.get(id).is_some_and(|binding| {
            self.sender_registration_is_current(binding.registration())
                && binding.continuity_recorded
        })
    }
}
impl MessageNativeContinuityRequest {
    /// Checks the actual canonical runtime/root before consuming a positive
    /// result. A foreign adopter must leave the original request retained.
    pub fn belongs_to_service(&self, service: &MessageService) -> bool {
        Arc::ptr_eq(&self.0.runtime, &service.acceptance_runtime)
            && service
                .storage_custody
                .as_ref()
                .is_some_and(|p| p.scope == self.0.scope)
    }

    /// Shares the exact immutable input with one actual owned worker.
    pub fn write(&self) -> MessageNativeContinuityWrite {
        MessageNativeContinuityWrite(self.0.clone())
    }
    /// Publishes only for this actual input after its worker relinquished it.
    /// A replaced binding returns false without marking its successor or losing
    /// the original durable fact. All captured copies stay charged until drop.
    pub fn finish(
        self,
        service: &mut MessageService,
        verified: MessageNativeContinuityVerified,
    ) -> Result<bool> {
        if !Arc::ptr_eq(&self.0.identity, &verified.identity)
            || !Arc::ptr_eq(&self.0.runtime, &service.acceptance_runtime)
        {
            return Err(changed());
        }
        service.require_storage_scope(Some(&self.0.scope))?;
        let capture = Arc::try_unwrap(self.0).map_err(|_| changed())?;
        let Capture {
            registration,
            incarnation,
            snapshot,
            lease,
            ..
        } = capture;
        let current = (!service
            .storage_custody
            .as_ref()
            .is_some_and(|p| p.native_continuity_required)
            || snapshot.admission_guarded)
            && service.sender_registration_is_current(&registration)
            && service.native_owner_for_registration(&registration, &incarnation)
                == Some(&snapshot.native.owner);
        if current {
            let binding = service
                .native_bindings
                .get_mut(registration.agent_id())
                .ok_or_else(changed)?;
            binding.continuity_recorded = true;
        }
        drop(snapshot);
        drop(lease);
        Ok(current)
    }
}
impl MessageNativeContinuityWrite {
    /// Requires the actual original root scope; equal labels are insufficient.
    pub fn belongs_to(&self, scope: &MessageStorageScope) -> bool {
        self.0.scope == *scope
    }
    /// Borrows nonauthorizing facts captured from actual native tokens.
    pub fn snapshot(&self) -> &MessageNativeContinuitySnapshot {
        &self.0.snapshot
    }
    /// Returns an opaque completion only after the trusted port succeeded.
    pub fn persist(
        self,
        store: &mut impl MessageNativeContinuityStore,
    ) -> Result<MessageNativeContinuityVerified> {
        store.commit(&self)?;
        Ok(MessageNativeContinuityVerified {
            identity: self.0.identity.clone(),
        })
    }
}

/// Bounded, content-free ownership diagnostic.
fn changed() -> MessageError {
    MessageError::invalid_state("MMP native continuity input changed or is unavailable")
}

#[cfg(test)]
mod tests;
