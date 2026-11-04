//! Actual registration-end intent retained before guarded native publication.
//!
//! An actual registration owns a one-way end cutoff. A charged registry entry
//! retains that allocation before native I/O, so synchronous removal/replacement
//! needs no post-removal allocation and cannot lose an end while a worker runs.
//! Only an original-root journal positive removes the entry. Stored tuples are
//! descriptive: they cannot end a live registration, restore it, acknowledge mail
//! or supply a source-slot terminal disposition.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::*;

/// Covers the retained registration, bounded identifier and registry capacity.
const INTENT_METADATA: usize = 4096;

/// Bounded facts captured from an actually ended registration allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageRegistrationEndSnapshot {
    /// Independent end-intent framing version, currently one.
    pub format_version: u32,
    /// Original endpoint attribution; equal labels are not a live capability.
    pub agent_id: String,
    /// Original registration namespace, covering all its native shell epochs.
    pub registration_namespace: [u8; 16],
    /// Original registration interval floor, never a subscription cursor.
    pub registered_after_sequence: u64,
    /// Last allocated sequence when this actual registration ended.
    pub ended_after_sequence: u64,
}

impl MessageRegistrationEndSnapshot {
    /// Rejects malformed framing, attribution, namespace or regressive cutoff.
    /// Validation never creates an end intent or a live registration token.
    pub fn validate(&self) -> Result<()> {
        parse_opaque_id(&self.agent_id, "MMP registration end agent")?;
        let namespace = uuid::Uuid::from_bytes(self.registration_namespace);
        if self.format_version != 1
            || namespace.get_version_num() != 4
            || namespace.get_variant() != uuid::Variant::RFC4122
            || self.registered_after_sequence > self.ended_after_sequence
        {
            return Err(changed());
        }
        Ok(())
    }
}

/// Actual allocation retained through journal adoption, independently of n1.
struct Intent {
    registration: MessageSenderRegistration,
    floor: u64,
    _lease: MessageStorageSnapshotLease,
}

/// Runtime-only bounded owners, absent from all generic snapshot formats.
#[derive(Default)]
pub(in crate::messaging) struct RegistrationEnds(Mutex<BTreeMap<[u8; 16], Arc<Intent>>>);

impl std::fmt::Debug for RegistrationEnds {
    /// Does not expose allocation identities or namespace data in diagnostics.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrationEnds").finish_non_exhaustive()
    }
}

/// Frozen original end capture; all worker copies share its metadata lease.
struct Capture {
    runtime: Arc<()>,
    scope: MessageStorageScope,
    intent: Arc<Intent>,
    snapshot: MessageRegistrationEndSnapshot,
    _lease: MessageStorageSnapshotLease,
}

/// Original end input retained outside its actual physical worker.
pub struct MessageRegistrationEndRequest(Arc<Capture>);
/// One worker's immutable input, never constructible from serialized facts.
pub struct MessageRegistrationEndWrite(Arc<Capture>);
/// Exact successful trusted-port completion; carries no source-slot authority.
pub struct MessageRegistrationEndVerified(Arc<Capture>);

/// Trusted synchronous end journal, called on the product's owned worker lane.
pub trait MessageRegistrationEndStore {
    /// Persists/reconciles this exact original; failure never asserts noncommit.
    fn commit(&mut self, write: &MessageRegistrationEndWrite) -> Result<()>;
}

impl RegistrationIdentity {
    /// Marks the first actual removal/replacement cutoff, with no allocation.
    /// Temporary native shell displacement never calls this method.
    pub(super) fn end_at(&self, cutoff: u64) {
        // The single MessageService owner serializes lifecycle transitions.
        // Preserve the first cutoff even if cleanup encounters this token again.
        let _ = self.ended_after_sequence.set(cutoff);
    }
}

impl MessageService {
    /// Reserves one original namespace intent before guarded publication or
    /// qualified cold binding publication. Repeated captures share its charge.
    pub(super) fn track_native_registration(
        &self,
        registration: &MessageSenderRegistration,
    ) -> Result<()> {
        if !self.sender_registration_is_current(registration) {
            return Err(changed());
        }
        let policy = self.storage_custody.as_ref().ok_or_else(changed)?;
        if !policy.native_continuity_required {
            return Ok(());
        }
        let mut entries = policy.registration_ends.0.lock().map_err(|_| changed())?;
        let key = *registration.identity.native_namespace.as_bytes();
        if let Some(entry) = entries.get(&key) {
            return if Arc::ptr_eq(&entry.registration.identity, &registration.identity) {
                Ok(())
            } else {
                Err(changed())
            };
        }
        let floor = *self
            .registration_floors
            .get(registration.agent_id())
            .ok_or_else(changed)?;
        let lease = checkpoint::reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            INTENT_METADATA.saturating_add(registration.agent_id().as_str().len() * 4),
            self.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )?;
        entries.insert(
            key,
            Arc::new(Intent {
                registration: registration.clone(),
                floor,
                _lease: lease,
            }),
        );
        Ok(())
    }

    /// Reports actual ended allocations still awaiting journal adoption.
    /// A poisoned registry is an integrity failure, never an empty projection.
    pub fn has_pending_registration_end(&self) -> Result<bool> {
        let Some(policy) = &self.storage_custody else {
            return Ok(false);
        };
        let entries = policy.registration_ends.0.lock().map_err(|_| changed())?;
        Ok(entries.values().any(|entry| {
            entry
                .registration
                .identity
                .ended_after_sequence
                .get()
                .is_some()
        }))
    }

    /// Captures one already-ended allocation under the actual original scope.
    /// Reserves worker scratch before cloning facts; no end is inferred from
    /// current presence, n1, a generic snapshot, or a temporary shell switch.
    pub fn prepare_registration_end(
        &self,
        scope: &MessageStorageScope,
    ) -> Result<Option<MessageRegistrationEndRequest>> {
        self.require_storage_scope(Some(scope))?;
        if self.admission.is_some() || self.acknowledgement.is_some() {
            return Err(changed());
        }
        let policy = self.storage_custody.as_ref().ok_or_else(changed)?;
        let entries = policy.registration_ends.0.lock().map_err(|_| changed())?;
        let Some(intent) = entries.values().find(|entry| {
            entry
                .registration
                .identity
                .ended_after_sequence
                .get()
                .is_some()
        }) else {
            return Ok(None);
        };
        let lease = checkpoint::reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            MESSAGE_STORAGE_RECOVERY_SCRATCH_METADATA + 16 * 1024,
            self.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )?;
        let snapshot = MessageRegistrationEndSnapshot {
            format_version: 1,
            agent_id: intent.registration.agent_id().to_string(),
            registration_namespace: *intent.registration.identity.native_namespace.as_bytes(),
            registered_after_sequence: intent.floor,
            ended_after_sequence: *intent
                .registration
                .identity
                .ended_after_sequence
                .get()
                .ok_or_else(changed)?,
        };
        snapshot.validate()?;
        Ok(Some(MessageRegistrationEndRequest(Arc::new(Capture {
            runtime: self.acceptance_runtime.clone(),
            scope: scope.clone(),
            intent: intent.clone(),
            snapshot,
            _lease: lease,
        }))))
    }
}

impl MessageRegistrationEndRequest {
    /// Verifies exact canonical owner and original root, without consuming input.
    pub fn belongs_to_service(&self, service: &MessageService) -> bool {
        Arc::ptr_eq(&self.0.runtime, &service.acceptance_runtime)
            && service
                .storage_custody
                .as_ref()
                .is_some_and(|p| p.scope == self.0.scope)
    }
    /// Shares only this immutable original input with one physical worker.
    pub fn write(&self) -> MessageRegistrationEndWrite {
        MessageRegistrationEndWrite(self.0.clone())
    }
    /// Removes the retained intent only after this exact positive and actual
    /// worker-copy release. It neither changes source slots nor revives an owner.
    pub fn finish(
        self,
        service: &mut MessageService,
        verified: MessageRegistrationEndVerified,
    ) -> Result<bool> {
        if !self.belongs_to_service(service) || !Arc::ptr_eq(&self.0, &verified.0) {
            return Err(changed());
        }
        drop(verified);
        let capture = Arc::try_unwrap(self.0).map_err(|_| changed())?;
        let policy = service.storage_custody.as_ref().ok_or_else(changed)?;
        let mut entries = policy.registration_ends.0.lock().map_err(|_| changed())?;
        let key = capture.snapshot.registration_namespace;
        let Some(entry) = entries.get(&key) else {
            return Ok(false);
        };
        if !Arc::ptr_eq(entry, &capture.intent) {
            return Err(changed());
        }
        entries.remove(&key);
        Ok(true)
    }
}

impl MessageRegistrationEndWrite {
    /// Requires the original live storage-root allocation, not equal labels.
    pub fn belongs_to(&self, scope: &MessageStorageScope) -> bool {
        self.0.scope == *scope
    }
    /// Borrows bounded actual end facts; serialized copies grant no capability.
    pub fn snapshot(&self) -> &MessageRegistrationEndSnapshot {
        &self.0.snapshot
    }
    /// Produces an exact positive only after the trusted journal returns success.
    pub fn persist(
        self,
        store: &mut impl MessageRegistrationEndStore,
    ) -> Result<MessageRegistrationEndVerified> {
        store.commit(&self)?;
        Ok(MessageRegistrationEndVerified(self.0))
    }
}

/// Content-free diagnostic for a changed owner or invalid end capture.
fn changed() -> MessageError {
    MessageError::invalid_state("MMP original registration end is unavailable or changed")
}

#[cfg(test)]
mod tests;
