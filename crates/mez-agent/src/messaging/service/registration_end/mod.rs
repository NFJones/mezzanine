//! Actual registration-end ownership before guarded source/native publication.
//!
//! An actual registration owns a one-way end cutoff. Charged registry entries
//! retain its allocation before I/O, so removal/replacement needs no allocation
//! afterward and cannot lose an end while a worker runs. Never-ended raw source
//! reservations release when their final source guard leaves; native publication
//! and actual ended intents persist until original-root journal adoption. Cold
//! intents come only from the sealed original-store/private-context target.
//! Stored tuples cannot end a live registration, restore it, acknowledge mail
//! or supply a source-slot terminal disposition.

use std::collections::BTreeMap;
use std::sync::{
    Mutex, Weak,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

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
    /// Fingerprints only these bounded descriptive facts, without granting an
    /// end capability. Raw source retirement links this image to actual o1;
    /// native retirement additionally requires its original b1 provenance.
    pub fn fingerprint(&self) -> Result<[u8; 32]> {
        use sha2::Digest;
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| changed())?;
        if bytes.len() > 4096 {
            return Err(changed());
        }
        Ok(sha2::Sha256::digest(bytes).into())
    }

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

/// Allocation retained while source, native publication or end custody needs it.
struct Intent {
    registration: Option<MessageSenderRegistration>,
    agent: AgentId,
    namespace: [u8; 16],
    ended: Arc<std::sync::OnceLock<u64>>,
    floor: u64,
    persistent: AtomicBool,
    source_references: AtomicUsize,
    _lease: MessageStorageSnapshotLease,
}

/// Runtime-only bounded owners, absent from all generic snapshot formats.
#[derive(Default)]
pub(in crate::messaging) struct RegistrationEnds(Arc<Mutex<BTreeMap<[u8; 16], Arc<Intent>>>>);

impl std::fmt::Debug for RegistrationEnds {
    /// Does not expose allocation identities or namespace data in diagnostics.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrationEnds").finish_non_exhaustive()
    }
}

/// Source-owned retention of an actual namespace. A rejected raw reservation
/// must not pin endpoint metadata after its final source disappears. Native
/// publication and already-ended intents remain independently persistent.
pub(in crate::messaging) struct RegistrationSourceRetention {
    entries: Weak<Mutex<BTreeMap<[u8; 16], Arc<Intent>>>>,
    intent: Weak<Intent>,
}
impl std::fmt::Debug for RegistrationSourceRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrationSourceRetention")
            .finish_non_exhaustive()
    }
}
impl Drop for RegistrationSourceRetention {
    /// Removes only this last, never-ended, source-only original. Poison keeps
    /// the bounded reservation rather than mistaking an integrity failure for
    /// ownership loss; no end, publication or physical store fact is inferred.
    fn drop(&mut self) {
        let Some(intent) = self.intent.upgrade() else {
            return;
        };
        let previous = intent.source_references.fetch_sub(1, Ordering::AcqRel);
        let Some(entries) = self.entries.upgrade() else {
            return;
        };
        let Ok(mut entries) = entries.lock() else {
            return;
        };
        if previous == 1
            && !intent.persistent.load(Ordering::Acquire)
            && intent.ended.get().is_none()
            && entries
                .get(&intent.namespace)
                .is_some_and(|actual| Arc::ptr_eq(actual, &intent))
        {
            entries.remove(&intent.namespace);
        }
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
    /// A captured live/cold source keeps its end intent until actual original
    /// journal adoption. Do not let raw terminal I/O occupy the exclusive root
    /// before that prerequisite can commit, including capture-capacity refusal.
    pub(super) fn registration_end_is_recorded(&self, namespace: &[u8; 16]) -> Result<bool> {
        let policy = self.storage_custody.as_ref().ok_or_else(changed)?;
        Ok(!policy
            .registration_ends
            .0
            .lock()
            .map_err(|_| changed())?
            .contains_key(namespace))
    }

    /// Retains one explicit raw namespace on the sealed cold recovery path.
    /// No raw registration is recreated. The caller has qualified the complete
    /// original and knows no endpoint from the prior process can be resumed.
    pub(super) fn track_cold_raw_end(
        &self,
        agent: &AgentId,
        namespace: [u8; 16],
        floor: u64,
        ended: Arc<std::sync::OnceLock<u64>>,
        cutoff: u64,
    ) -> Result<()> {
        let policy = self.storage_custody.as_ref().ok_or_else(changed)?;
        let snapshot = MessageRegistrationEndSnapshot {
            format_version: 1,
            agent_id: agent.to_string(),
            registration_namespace: namespace,
            registered_after_sequence: floor,
            ended_after_sequence: cutoff,
        };
        snapshot.validate()?;
        let mut entries = policy.registration_ends.0.lock().map_err(|_| changed())?;
        if let Some(entry) = entries.get(&namespace) {
            if entry.registration.is_some()
                || entry.agent != *agent
                || entry.floor != floor
                || entry.ended.get() != Some(&cutoff)
            {
                return Err(changed());
            }
            if ended.get_or_init(|| cutoff) != &cutoff {
                return Err(changed());
            }
            return Ok(());
        }
        let lease = checkpoint::reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            INTENT_METADATA.saturating_add(agent.as_str().len() * 4),
            self.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )?;
        if ended.get_or_init(|| cutoff) != &cutoff {
            return Err(changed());
        }
        entries.insert(
            namespace,
            Arc::new(Intent {
                registration: None,
                agent: agent.clone(),
                namespace,
                ended,
                floor,
                persistent: AtomicBool::new(true),
                source_references: AtomicUsize::new(0),
                _lease: lease,
            }),
        );
        Ok(())
    }

    /// Retains an actual native publication independently of accepted sources.
    pub(super) fn track_registration(
        &self,
        registration: &MessageSenderRegistration,
    ) -> Result<()> {
        if self
            .storage_custody
            .as_ref()
            .is_none_or(|p| !p.native_continuity_required)
        {
            return Ok(());
        }
        self.retain_registration_intent(registration, true)
            .map(|_| ())
    }

    /// Reserves source-only raw/native intent retention before copying a source.
    /// Dropping a failed unpublished source releases newly held raw metadata.
    pub(super) fn track_source_registration(
        &self,
        registration: &MessageSenderRegistration,
    ) -> Result<RegistrationSourceRetention> {
        let intent = self.retain_registration_intent(registration, false)?;
        let policy = self.storage_custody.as_ref().ok_or_else(changed)?;
        intent.source_references.fetch_add(1, Ordering::AcqRel);
        Ok(RegistrationSourceRetention {
            entries: Arc::downgrade(&policy.registration_ends.0),
            intent: Arc::downgrade(&intent),
        })
    }

    /// Shares one actual allocation and charge. Native publication promotes its
    /// retention before I/O; source cleanup can never demote that obligation.
    fn retain_registration_intent(
        &self,
        registration: &MessageSenderRegistration,
        persistent: bool,
    ) -> Result<Arc<Intent>> {
        if !self.sender_registration_is_current(registration) {
            return Err(changed());
        }
        let policy = self
            .storage_custody
            .as_ref()
            .filter(|p| p.native_continuity_required)
            .ok_or_else(changed)?;
        let mut entries = policy.registration_ends.0.lock().map_err(|_| changed())?;
        let key = *registration.identity.native_namespace.as_bytes();
        if let Some(entry) = entries.get(&key) {
            if entry
                .registration
                .as_ref()
                .is_none_or(|actual| !Arc::ptr_eq(&actual.identity, &registration.identity))
            {
                return Err(changed());
            }
            if persistent {
                entry.persistent.store(true, Ordering::Release);
            }
            return Ok(entry.clone());
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
        let intent = Arc::new(Intent {
            registration: Some(registration.clone()),
            agent: registration.agent_id().clone(),
            namespace: key,
            ended: registration.identity.ended_after_sequence.clone(),
            floor,
            persistent: AtomicBool::new(persistent),
            source_references: AtomicUsize::new(0),
            _lease: lease,
        });
        entries.insert(key, intent.clone());
        Ok(intent)
    }

    /// Reports actual ended allocations still awaiting journal adoption.
    /// A poisoned registry is an integrity failure, never an empty projection.
    pub fn has_pending_registration_end(&self) -> Result<bool> {
        let Some(policy) = &self.storage_custody else {
            return Ok(false);
        };
        let entries = policy.registration_ends.0.lock().map_err(|_| changed())?;
        Ok(entries.values().any(|entry| entry.ended.get().is_some()))
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
        let Some(intent) = entries.values().find(|entry| entry.ended.get().is_some()) else {
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
            agent_id: intent.agent.to_string(),
            registration_namespace: intent.namespace,
            registered_after_sequence: intent.floor,
            ended_after_sequence: *intent.ended.get().ok_or_else(changed)?,
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
    /// Checks source/marker/registry ownership without consuming the original
    /// request or positive. The root retains both on refusal, including another
    /// actual worker copy that has not yet released its input.
    pub fn qualify_adoption(
        &self,
        service: &MessageService,
        verified: &MessageRegistrationEndVerified,
    ) -> Result<()> {
        if !self.belongs_to_service(service)
            || !Arc::ptr_eq(&self.0, &verified.0)
            || Arc::strong_count(&self.0) != 2
        {
            return Err(changed());
        }
        service.check_registration_end_sources(&self.0.snapshot)?;
        let policy = service.storage_custody.as_ref().ok_or_else(changed)?;
        let entries = policy.registration_ends.0.lock().map_err(|_| changed())?;
        if entries
            .get(&self.0.snapshot.registration_namespace)
            .is_some_and(|entry| !Arc::ptr_eq(entry, &self.0.intent))
        {
            return Err(changed());
        }
        Ok(())
    }

    /// Removes the retained intent only after this exact positive and actual
    /// worker-copy release. It neither changes source slots nor revives an owner.
    pub fn finish(
        self,
        service: &mut MessageService,
        verified: MessageRegistrationEndVerified,
    ) -> Result<bool> {
        self.qualify_adoption(service, &verified)?;
        drop(verified);
        let capture = Arc::try_unwrap(self.0).map_err(|_| changed())?;
        service.publish_registration_end_to_sources(&capture.snapshot)?;
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
