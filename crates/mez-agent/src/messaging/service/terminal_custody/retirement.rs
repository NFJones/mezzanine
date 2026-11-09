//! One exact retired native slot from qualified original lookup/end evidence.
//!
//! Namespace replacement alone never selects this operation. The witness owns
//! an actual source and the trusted port's preceding canonical absence. Existing
//! application anchors, positive ACKs and other terminal operations exclude it.
//! Nothing changes until the common original write and adoption succeed.

use super::*;

impl MessageService {
    /// Reserves one raw slot only from the original registration's genuine
    /// one-way end marker. Source namespaces alone and generic restored absence
    /// cannot select this operation. Actual o1/source validation still precedes
    /// storage success, and adoption alone publishes the terminal observation.
    pub fn prepare_storage_raw_retirement(
        &mut self,
        scope: &MessageStorageScope,
        observed_at_ms: u64,
        metadata_budget: usize,
    ) -> Result<Option<MessageTerminalTicket>> {
        let observed_at_ms = self.custody_now(observed_at_ms)?;
        let policy = self
            .storage_custody
            .as_ref()
            .filter(|p| p.scope == *scope && p.native_continuity_required)
            .ok_or_else(changed)?;
        if self.terminal_commit.is_some() || self.acknowledgement.is_some() {
            return Err(capacity());
        }
        for row in self.pending.values() {
            let (Some(registrations), Some(native)) =
                (&row.registration_bindings, &row.native_bindings)
            else {
                continue;
            };
            if registrations.recipients.len() != row.recipient_bindings.len()
                || native.recipients.len() != row.recipient_bindings.len()
            {
                return Err(changed());
            }
            if super::selection::known_expiry(row)?.is_some() {
                continue;
            }
            for (index, ((recipient, floor), registration)) in row
                .recipient_bindings
                .iter()
                .zip(&registrations.recipients)
                .enumerate()
            {
                if native.recipients[index].is_some() {
                    continue;
                }
                let Some(cutoff) = registration.ended.get().copied() else {
                    continue;
                };
                if !self.registration_end_is_recorded(&registration.namespace)? {
                    continue;
                }
                if row.acknowledged.lock().map_err(|_| changed())?[index]
                    || row.terminal.lock().map_err(|_| changed())?[index].is_some()
                    || self.acknowledgement_protects(row.sequence, recipient)
                    || native_input::protects(row, recipient)
                {
                    continue;
                }
                if row.sequence <= *floor || row.sequence > cutoff {
                    return Err(changed());
                }
                let end = MessageRegistrationEndSnapshot {
                    format_version: 1,
                    agent_id: recipient.to_string(),
                    registration_namespace: registration.namespace,
                    registered_after_sequence: *floor,
                    ended_after_sequence: cutoff,
                };
                end.validate()?;
                let bytes = super::super::storage_policy::source_metadata_bytes(row)
                    .saturating_add(MESSAGE_STORAGE_RECOVERY_SCRATCH_METADATA)
                    .saturating_add(4096);
                if bytes > metadata_budget.min(MAX_METADATA) {
                    return Err(capacity());
                }
                let lease = self.reserve_storage_worker_metadata(bytes)?;
                let identity = Arc::new(Identity {
                    owner: self.acceptance_runtime.clone(),
                    scope: scope.clone(),
                    source: row.clone(),
                    digest: row.envelope.storage_digest(scope)?,
                    observed_at_ms: observed_at_ms.max(row.accepted_at_ms),
                    kind: MessageTerminalKind::RecipientRetirement,
                    indices: vec![u32::try_from(index).map_err(|_| changed())?],
                    before: vec![None],
                    handoff: None,
                    retirement: None,
                    raw_retirement: Some(end),
                    metadata_bytes: bytes,
                    payload_validation_limit: policy.limits.cache_payload_bytes,
                    published: AtomicBool::new(false),
                    _lease: lease,
                });
                self.terminal_commit = Some(PendingTerminal {
                    identity: identity.clone(),
                    attempt: None,
                });
                return Ok(Some(MessageTerminalTicket { identity }));
            }
        }
        Ok(None)
    }

    /// Reserves the exact ended recipient only while its lookup exclusively
    /// protects the original slot. Capacity refusal or competing ownership leaves
    /// the witness and accepted source unchanged; no terminal is published here.
    pub fn prepare_storage_native_retirement(
        &mut self,
        scope: &MessageStorageScope,
        witness: &MessageNativeRetirementWitness,
        observed_at_ms: u64,
        metadata_budget: usize,
    ) -> Result<Option<MessageTerminalTicket>> {
        let observed_at_ms = self.custody_now(observed_at_ms)?;
        let source = witness.anchor();
        if !source.belongs_to(scope) || !self.native_input_anchor_is_original(source) {
            return Err(changed());
        }
        let policy = self
            .storage_custody
            .as_ref()
            .filter(|p| p.scope == *scope && p.native_continuity_required)
            .ok_or_else(changed)?;
        if self.terminal_commit.is_some() || self.acknowledgement.is_some() {
            return Err(capacity());
        }
        if !source.retirement_is_exclusive() || self.native_input_owner_is_current(source) {
            return Ok(None);
        }
        let row = &source.0.row;
        if super::selection::known_expiry(row)?.is_some() {
            return Ok(None);
        }
        let index = source.binding_index();
        if row.acknowledged.lock().map_err(|_| changed())?.get(index) != Some(&false)
            || row.terminal.lock().map_err(|_| changed())?.get(index) != Some(&None)
        {
            return Ok(None);
        }
        let bytes = super::super::storage_policy::source_metadata_bytes(row)
            .saturating_add(MESSAGE_STORAGE_RECOVERY_SCRATCH_METADATA)
            .saturating_add(4096);
        if bytes > metadata_budget.min(MAX_METADATA) {
            return Err(capacity());
        }
        let lease = self.reserve_storage_worker_metadata(bytes)?;
        let identity = Arc::new(Identity {
            owner: self.acceptance_runtime.clone(),
            scope: scope.clone(),
            source: row.clone(),
            digest: source.source_sha256(),
            observed_at_ms: observed_at_ms.max(row.accepted_at_ms),
            kind: MessageTerminalKind::RecipientRetirement,
            indices: vec![u32::try_from(index).map_err(|_| changed())?],
            before: vec![None],
            handoff: None,
            retirement: Some(witness.clone()),
            raw_retirement: None,
            metadata_bytes: bytes,
            payload_validation_limit: policy.limits.cache_payload_bytes,
            published: AtomicBool::new(false),
            _lease: lease,
        });
        self.terminal_commit = Some(PendingTerminal {
            identity: identity.clone(),
            attempt: None,
        });
        Ok(Some(MessageTerminalTicket { identity }))
    }
}
