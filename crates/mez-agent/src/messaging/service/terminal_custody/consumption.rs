//! One canonical-proof-only sender consumption on the common terminal lane.
//!
//! Canonical proof retains its original source/before-image and protects that
//! slot through actual write, retry and adoption. Historical proof may consume
//! only its original outcome without current sender/task rights. This release
//! is not ACK or elapsed-horizon evidence; its first clock is never changed.

use super::*;

/// One fixed original source/slot/first-terminal and canonical-position frame.
pub(super) const ENCODED_BYTES: usize = 133;

impl MessageService {
    /// Reserves exactly one original unreleased outcome from actual canonical
    /// proof. Caller retains its proof across pre-issue capacity refusal. The
    /// reservation owns a clone through actual uncertainty and final adoption.
    pub fn prepare_storage_sender_consumption(
        &mut self,
        scope: &MessageStorageScope,
        proof: &MessageTerminalHandoffCommit,
        metadata_budget: usize,
    ) -> Result<MessageTerminalTicket> {
        let source = proof.anchor();
        if !source.belongs_to(scope) || !self.terminal_handoff_anchor_is_original(source) {
            return Err(changed());
        }
        let policy = self
            .storage_custody
            .as_ref()
            .filter(|p| p.scope == *scope && p.native_continuity_required)
            .ok_or_else(changed)?;
        if self.terminal_commit.is_some() {
            return Err(capacity());
        }
        let row = source.original_row();
        let index = source.binding_index();
        let before = source.terminal();
        if before.released
            || row.acknowledged.lock().map_err(|_| changed())?.get(index) != Some(&false)
            || row.terminal.lock().map_err(|_| changed())?.get(index) != Some(&Some(before))
        {
            return Err(changed());
        }
        let bytes = super::super::storage_policy::source_metadata_bytes(row)
            .saturating_add(MESSAGE_STORAGE_RECOVERY_SCRATCH_METADATA)
            .saturating_add(1024);
        if bytes > metadata_budget.min(MAX_METADATA) {
            return Err(capacity());
        }
        let lease = super::super::checkpoint::reserve_checkpoint(
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
            observed_at_ms: before.observed_at_ms,
            kind: MessageTerminalKind::SenderConsumption,
            indices: vec![u32::try_from(index).map_err(|_| changed())?],
            before: vec![Some(before)],
            handoff: Some(proof.clone()),
            retirement: None,
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
        Ok(MessageTerminalTicket { identity })
    }
}
impl MessageTerminalWrite {
    /// Encodes descriptive consumption data; only this source-owned write can
    /// carry the canonical proof that qualified selection and actual storage.
    pub(super) fn write_consumption_record(&self, writer: &mut impl Write) -> io::Result<()> {
        let proof = self
            .canonical_handoff()
            .ok_or_else(|| io::Error::other("MMP original canonical proof missing"))?;
        let source = proof.anchor();
        let record = proof.record();
        writer.write_all(b"MMPCON01")?;
        writer.write_all(&self.sequence().to_le_bytes())?;
        writer.write_all(&self.identity.digest)?;
        writer.write_all(&(source.binding_index() as u32).to_le_bytes())?;
        writer.write_all(&source.terminal().observed_at_ms.to_le_bytes())?;
        writer.write_all(&[match source.terminal().reason {
            DeliveryTerminalReason::Expired => 1,
            DeliveryTerminalReason::RecipientRetired => 2,
        }])?;
        writer.write_all(&record.occurrence)?;
        writer.write_all(&record.archive_sequence.to_le_bytes())?;
        writer.write_all(&record.canonical_sha256)
    }
}
