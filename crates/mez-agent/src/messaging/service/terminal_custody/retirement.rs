//! One exact retired native slot from qualified original lookup/end evidence.
//!
//! Namespace replacement alone never selects this operation. The witness owns
//! an actual source and the trusted port's preceding canonical absence. Existing
//! application anchors, positive ACKs and other terminal operations exclude it.
//! Nothing changes until the common original write and adoption succeed.

use super::*;

impl MessageService {
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
        let lease = checkpoint::reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            bytes,
            self.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )?;
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
            metadata_bytes: bytes,
            decode_payload_bytes: policy.limits.cache_payload_bytes,
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
