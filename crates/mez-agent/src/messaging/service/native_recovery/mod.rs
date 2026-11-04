//! Qualified native endpoints on a one-use unpublished cold custody owner.
//!
//! Only the trusted product port can combine original-store publication facts
//! with private current session/context evidence. Generic snapshots and decoded
//! UUIDs cannot obtain this target. A refused endpoint poisons the whole result;
//! no partial registry, raw reconnect identity or provider work is published.
//! Returned opaque epochs let the product install only the context it qualified.

use super::*;

/// Qualified descriptive record supplied only by the original-store/context port.
pub struct MessageNativeRecoveryRecord {
    /// Version2 actual guarded registration observation, not authority alone.
    pub snapshot: MessageNativeContinuitySnapshot,
    /// Actual current private context availability; never imported from a DTO.
    pub admission_available: bool,
    /// Whether the actual current context is outside an ephemeral child.
    pub delivery_available: bool,
    /// Current observation time, not a renewed message acceptance/TTL clock.
    pub observed_at_ms: u64,
}

/// Trusted synchronous product qualification, executed on its owned root lane.
pub trait MessageNativeRecoveryStore {
    /// Checks original allocation/publication and private current context in one
    /// qualified operation before supplying endpoints. Error withholds all state.
    fn recover(&mut self, target: &mut MessageNativeRecovery) -> Result<()>;
}

/// Opaque original epoch and public attribution, charged until product adoption.
/// No decoder or public UUID constructor can create this qualified result.
pub struct MessageRecoveredNativeBinding {
    id: AgentId,
    incarnation: MessageNativeIncarnation,
    _lease: MessageStorageSnapshotLease,
}
impl MessageRecoveredNativeBinding {
    /// Borrows the exact endpoint qualified by the original private context.
    pub fn agent_id(&self) -> &AgentId {
        &self.id
    }
    /// Borrows the original opaque epoch for the product's matching shell only.
    pub fn incarnation(&self) -> &MessageNativeIncarnation {
        &self.incarnation
    }
}

/// Unpublished native registry construction, unavailable to generic codecs.
pub struct MessageNativeRecovery {
    service: MessageService,
    bindings: Vec<MessageRecoveredNativeBinding>,
    failed: bool,
    _scratch: MessageStorageSnapshotLease,
    _payload: super::payload_budget::PayloadReservation,
}

impl MessageService {
    /// Consumes the one-use cold candidate and qualifies native endpoints through
    /// its trusted original-store/private-context port. Fresh/generic/live owners,
    /// foreign scope, occupied endpoints or any refused record return no result.
    /// The caller retains the actual root/input and must install matching opaque
    /// epochs before session publication; this invokes no task/provider/effect.
    pub fn recover_native_context(
        mut self,
        scope: &MessageStorageScope,
        store: &mut impl MessageNativeRecoveryStore,
    ) -> Result<(Self, Vec<MessageRecoveredNativeBinding>)> {
        self.require_storage_scope(Some(scope))?;
        if !self.native_recovery_available
            || !self.registered.is_empty()
            || !self.registration_tokens.is_empty()
            || !self.native_bindings.is_empty()
            || !self.subscriptions.is_empty()
        {
            return Err(unavailable());
        }
        self.native_recovery_available = false;
        self.require_native_continuity(scope)?;
        let policy = self.storage_custody.as_ref().ok_or_else(unavailable)?;
        let scratch = checkpoint::reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            MESSAGE_STORAGE_RECOVERY_SCRATCH_METADATA,
            self.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )?;
        let payload = policy
            .cache
            .reserve(0, policy.limits.cache_payload_bytes, 0)?;
        let mut target = MessageNativeRecovery {
            service: self,
            bindings: Vec::new(),
            failed: false,
            _scratch: scratch,
            _payload: payload,
        };
        store.recover(&mut target)?;
        if target.failed {
            return Err(unavailable());
        }
        target.service.retain_cold_raw_registration_ends()?;
        let MessageNativeRecovery {
            mut service,
            bindings,
            _scratch,
            _payload,
            ..
        } = target;
        service.ids.observe_existing_ids(service.registered.keys());
        drop(_payload);
        drop(_scratch);
        Ok((service, bindings))
    }
}

impl MessageNativeRecovery {
    /// Retains private input/qualification indexes under the same metadata ceiling.
    pub fn hold_metadata(&self, bytes: usize) -> Result<MessageStorageSnapshotLease> {
        let policy = self
            .service
            .storage_custody
            .as_ref()
            .ok_or_else(unavailable)?;
        checkpoint::reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            bytes,
            self.service.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )
    }
    /// Installs one qualified endpoint, preserving original namespace and floor.
    /// A failed call permanently withholds the entire candidate, even if a port
    /// mishandles its immediate error. Stored availability hints are ignored.
    pub fn restore_binding(&mut self, record: MessageNativeRecoveryRecord) -> Result<()> {
        if self.failed {
            return Err(unavailable());
        }
        let result = self.restore_binding_inner(record);
        self.failed |= result.is_err();
        result
    }
    /// Precharges copied identity state before constructing the unpublished binding.
    fn restore_binding_inner(&mut self, record: MessageNativeRecoveryRecord) -> Result<()> {
        let MessageNativeRecoveryRecord {
            snapshot,
            admission_available,
            delivery_available,
            observed_at_ms,
        } = record;
        let temporary = self.hold_metadata(snapshot_charge(&snapshot))?;
        snapshot.validate()?;
        if snapshot.format_version != 2
            || !snapshot.admission_guarded
            || (delivery_available && !admission_available)
            || snapshot.registered_after_sequence >= self.service.next_sequence
        {
            return Err(unavailable());
        }
        let identity = sender_identity_from_snapshot(&snapshot.identity, 9)?;
        let id = identity.agent_id.clone();
        if self.service.registered.contains_key(&id) {
            return Err(unavailable());
        }
        let result_lease =
            self.hold_metadata(256usize.saturating_add(id.as_str().len().saturating_mul(4)))?;
        let incarnation =
            MessageNativeIncarnation::from_qualified_recovery(snapshot.native.shell_incarnation);
        self.service.registration_tokens.insert(
            id.clone(),
            Arc::new(RegistrationIdentity {
                native_namespace: uuid::Uuid::from_bytes(snapshot.native.registration_namespace),
                ended_after_sequence: Default::default(),
            }),
        );
        self.service
            .registration_floors
            .insert(id.clone(), snapshot.registered_after_sequence);
        self.service.registered.insert(id.clone(), identity.clone());
        self.service.presence.insert(
            id.clone(),
            PresenceRecord {
                identity,
                status: AgentPresenceStatus::Available,
                updated_at_ms: observed_at_ms,
            },
        );
        let registration = self
            .service
            .capture_sender_registration(&id)
            .ok_or_else(unavailable)?;
        self.service.reconcile_recovered_native(
            &registration,
            &incarnation,
            &snapshot.native.owner,
            admission_available,
            delivery_available,
        )?;
        self.service.track_registration(&registration)?;
        self.service.subscribe_from_retained_start(&id)?;
        self.bindings.try_reserve(1).map_err(|_| unavailable())?;
        self.bindings.push(MessageRecoveredNativeBinding {
            id,
            incarnation,
            _lease: result_lease,
        });
        drop(registration);
        drop(snapshot);
        drop(temporary);
        Ok(())
    }
}

/// Charges copied identity/container metadata before validating or cloning any
/// strings. The port's original record/input and decoder scratch stay separate.
fn snapshot_charge(snapshot: &MessageNativeContinuitySnapshot) -> usize {
    let identity = &snapshot.identity;
    [
        identity.agent_id.as_str(),
        identity.pane_id.as_deref().unwrap_or(""),
        identity.window_id.as_deref().unwrap_or(""),
        identity.role.as_deref().unwrap_or(""),
        identity.objective.as_deref().unwrap_or(""),
        identity.project_scope.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .chain(identity.capabilities.iter().map(String::as_str))
    .fold(8192usize, |n, value| {
        n.saturating_add(128)
            .saturating_add(value.len().saturating_mul(16))
    })
}

/// Content-free failure; it never exposes native UUIDs or private context data.
fn unavailable() -> MessageError {
    MessageError::invalid_state("MMP native context recovery unavailable")
}

#[cfg(test)]
mod tests;
