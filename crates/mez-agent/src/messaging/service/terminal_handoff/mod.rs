//! Original native-sender terminal facts and trusted canonical qualification.
//!
//! An anchor freezes one actual unreleased terminal slot, its immutable source
//! fingerprint and original native SENDER. Live capture additionally checks the
//! original registered sender; lookup preserves historical attribution without
//! reviving a sender, task or provider. Source metadata and shared slot protection
//! survive through the last anchor/proof owner. A real trusted canonical callback
//! alone can seal commit proof; descriptive rows/labels/snapshots cannot do so.
//! Durable consumption remains a separate terminal transaction after that proof.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::*;

/// Exact source/before-image retained independently of mutable sender discovery.
struct Source {
    runtime: Arc<()>,
    scope: MessageStorageScope,
    row: Arc<QueuedEnvelope>,
    index: usize,
    sender_floor: u64,
    digest: [u8; 32],
    native: MessageNativeOwner,
    before: MessageDeliveryTerminalSnapshot,
    live: bool,
    _protection: Arc<()>,
    _lease: MessageStorageSnapshotLease,
}

/// Actual original sender/terminal source; neither data nor canonical proof.
#[derive(Clone)]
pub struct MessageTerminalHandoffAnchor(Arc<Source>);
impl std::fmt::Debug for MessageTerminalHandoffAnchor {
    /// Redacts authored IDs, outcomes, native attribution and private scope.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageTerminalHandoffAnchor")
            .field("sequence", &self.sequence())
            .field("slot", &self.binding_index())
            .field("live", &self.is_live())
            .finish()
    }
}

/// Descriptive actual canonical position, never a sender/task/consume token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageTerminalHandoffRecord {
    /// Original private allocation, qualified by the actual root port.
    pub store_id: [u8; 16],
    /// Distinct original source/slot/terminal handoff occurrence.
    pub occurrence: [u8; 32],
    /// Exact synced original archive row and its complete encoded digest.
    pub archive_sequence: u64,
    pub canonical_sha256: [u8; 32],
    /// Actual read/sync observation only; serialization discards this fact.
    #[serde(skip)]
    pub archive_observed: bool,
}

/// Trusted synchronous canonical-storage port, run on an owned root worker.
pub trait MessageTerminalHandoffStore {
    /// Returns only actual original canonical evidence. None is qualified
    /// absence and cannot authorize context insertion or consumption by itself.
    fn verify(
        &mut self,
        source: &MessageTerminalHandoffAnchor,
    ) -> Result<Option<MessageTerminalHandoffRecord>>;
}

/// Actual sealed original canonical result, separate from later consumption.
#[derive(Debug, Clone)]
pub struct MessageTerminalHandoffCommit {
    anchor: MessageTerminalHandoffAnchor,
    record: MessageTerminalHandoffRecord,
}
impl MessageTerminalHandoffCommit {
    /// Borrows its original source cap without reconstructing sender rights.
    pub fn anchor(&self) -> &MessageTerminalHandoffAnchor {
        &self.anchor
    }
    /// Describes the committed physical position; not a public proof constructor.
    pub fn record(&self) -> &MessageTerminalHandoffRecord {
        &self.record
    }
}

impl MessageTerminalHandoffAnchor {
    /// Requires the actual live storage scope, not equal source/session labels.
    pub fn belongs_to(&self, scope: &MessageStorageScope) -> bool {
        self.0.scope == *scope
    }
    /// True only when captured through current original-sender authentication.
    /// It is not canonical commit and must not survive descriptive serialization.
    pub fn is_live(&self) -> bool {
        self.0.live
    }
    /// Returns the original accepted occurrence and exact frozen recipient slot.
    pub fn sequence(&self) -> u64 {
        self.0.row.sequence
    }
    pub fn binding_index(&self) -> usize {
        self.0.index
    }
    /// Borrows original sender identity, not a reconnect credential.
    pub fn sender(&self) -> &AgentId {
        &self.0.row.envelope.sender.agent_id
    }
    /// Returns the actual captured original sender registration interval.
    pub fn sender_registered_after_sequence(&self) -> u64 {
        self.0.sender_floor
    }
    /// Borrows the recipient whose original terminal outcome is being handed off.
    pub fn recipient(&self) -> &AgentId {
        &self.0.row.recipient_bindings[self.0.index].0
    }
    pub fn registered_after_sequence(&self) -> u64 {
        self.0.row.recipient_bindings[self.0.index].1
    }
    /// Borrows frozen native SENDER attribution without creating a live epoch.
    pub fn native_owner(&self) -> &MessageNativeOwner {
        &self.0.native
    }
    /// Returns the complete original accepted-source fingerprint.
    pub fn source_sha256(&self) -> [u8; 32] {
        self.0.digest
    }
    /// Returns its exact first terminal before-image, always unreleased here.
    pub fn terminal(&self) -> MessageDeliveryTerminalSnapshot {
        self.0.before
    }
    /// Copies a bounded outcome view for the actual original owner. These bytes
    /// grant no canonical, sender-consumption or task/provider authority.
    pub fn outcome(&self) -> MessageDeliveryOutcome {
        MessageDeliveryOutcome {
            sequence: self.sequence(),
            message_id: self.0.row.envelope.id.clone(),
            recipient: MessageRecipientBindingSnapshot {
                recipient: self.recipient().to_string(),
                registered_after_sequence: self.registered_after_sequence(),
            },
            reason: self.0.before.reason,
            observed_at_ms: self.0.before.observed_at_ms,
        }
    }
    /// Distinct handoff identity cannot collide with native recipient input.
    pub fn occurrence(&self, store_id: &[u8; 16]) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"mez-terminal-canonical-handoff-v1\0");
        hash.update(store_id);
        hash.update(self.0.digest);
        hash.update((self.0.index as u32).to_le_bytes());
        hash.update(self.0.before.observed_at_ms.to_le_bytes());
        hash.update([match self.0.before.reason {
            DeliveryTerminalReason::Expired => 1,
            DeliveryTerminalReason::RecipientRetired => 2,
        }]);
        hash.finalize().into()
    }
    /// Seals only a trusted callback's actual original result. A lookup and a
    /// live source share proof validation, without granting fresh task rights.
    pub fn verify(
        self,
        store: &mut impl MessageTerminalHandoffStore,
    ) -> Result<Option<MessageTerminalHandoffCommit>> {
        let Some(record) = store.verify(&self)? else {
            return Ok(None);
        };
        if record.store_id == [0; 16]
            || record.canonical_sha256 == [0; 32]
            || record.archive_sequence == 0
            || record.occurrence != self.occurrence(&record.store_id)
        {
            return Err(unavailable());
        }
        Ok(Some(MessageTerminalHandoffCommit {
            anchor: self,
            record,
        }))
    }
}

impl MessageService {
    /// Captures only an authenticated original native sender's exact actual
    /// unconsumed outcome under trusted durable setup. A DTO cannot select a
    /// different source/slot/clock or infer a sender from public identity text.
    pub fn capture_terminal_handoff(
        &self,
        sender: &AgentId,
        outcome: &MessageDeliveryOutcome,
    ) -> Result<MessageTerminalHandoffAnchor> {
        let row = self
            .pending
            .get(&outcome.sequence)
            .ok_or_else(unavailable)?;
        if !self.registered.contains_key(sender)
            || !self.owns_outcomes(row, sender)
            || row.envelope.id != outcome.message_id
        {
            return Err(unavailable());
        }
        let index = row
            .recipient_bindings
            .iter()
            .position(|(id, floor)| {
                id.as_str() == outcome.recipient.recipient
                    && *floor == outcome.recipient.registered_after_sequence
            })
            .ok_or_else(unavailable)?;
        let anchor = self.terminal_handoff_anchor(row, index, true)?;
        if anchor.terminal().reason != outcome.reason
            || anchor.terminal().observed_at_ms != outcome.observed_at_ms
        {
            return Err(unavailable());
        }
        Ok(anchor)
    }

    /// Retains an actual historical terminal/native-sender source for lookup
    /// only, including after the original sender is unavailable or replaced.
    pub fn capture_terminal_handoff_lookup(
        &self,
        sequence: u64,
        index: usize,
    ) -> Result<MessageTerminalHandoffAnchor> {
        self.terminal_handoff_anchor(
            self.pending.get(&sequence).ok_or_else(unavailable)?,
            index,
            false,
        )
    }

    /// Selects one historical native-sender outcome in stable source/slot order.
    /// Only actual unreleased terminal facts participate; it offers no mail and
    /// cannot recreate the original accepted action or sender/task registration.
    pub fn next_terminal_handoff_lookup(
        &self,
        after: Option<(u64, usize)>,
    ) -> Result<Option<MessageTerminalHandoffAnchor>> {
        for row in self.pending.values() {
            if row
                .native_bindings
                .as_ref()
                .and_then(|n| n.sender.as_ref())
                .is_none()
            {
                continue;
            }
            let count = row.recipient_bindings.len();
            for index in 0..count {
                if after.is_some_and(|last| (row.sequence, index) <= last) {
                    continue;
                }
                let before = row
                    .terminal
                    .lock()
                    .map_err(|_| unavailable())?
                    .get(index)
                    .copied()
                    .flatten();
                if before.is_some_and(|value| !value.released)
                    && !self.expiry_protects(row.sequence, &row.recipient_bindings[index].0)
                {
                    return self.terminal_handoff_anchor(row, index, false).map(Some);
                }
            }
        }
        Ok(None)
    }

    /// Checks original runtime/scope/source allocation before root adoption.
    /// A replacement sender may not inherit live rights but cannot retarget this
    /// exact historical proof. No generic decoded data supplies these Arcs.
    pub fn terminal_handoff_anchor_is_original(
        &self,
        source: &MessageTerminalHandoffAnchor,
    ) -> bool {
        Arc::ptr_eq(&self.acceptance_runtime, &source.0.runtime)
            && self
                .storage_custody
                .as_ref()
                .is_some_and(|p| p.scope == source.0.scope)
            && self
                .pending
                .get(&source.sequence())
                .is_some_and(|row| Arc::ptr_eq(row, &source.0.row))
    }
    /// Reports actual current original sender ownership for optional private
    /// frontier application, never from archived availability or native DTOs.
    pub fn terminal_handoff_owner_is_current(&self, source: &MessageTerminalHandoffAnchor) -> bool {
        self.terminal_handoff_anchor_is_original(source)
            && self.owns_outcomes(&source.0.row, source.sender())
    }

    /// Reserves source/attribution/projection metadata before retaining a shared
    /// protection token. It freezes actual state without modifying the outcome.
    fn terminal_handoff_anchor(
        &self,
        row: &Arc<QueuedEnvelope>,
        index: usize,
        live: bool,
    ) -> Result<MessageTerminalHandoffAnchor> {
        let policy = self
            .storage_custody
            .as_ref()
            .filter(|p| p.native_continuity_required)
            .ok_or_else(unavailable)?;
        let native = row
            .native_bindings
            .as_ref()
            .and_then(|n| n.sender.as_ref())
            .ok_or_else(unavailable)?;
        let sender_floor = row
            .sender_registered_after_sequence
            .ok_or_else(unavailable)?;
        if index >= row.recipient_bindings.len()
            || row
                .acknowledged
                .lock()
                .map_err(|_| unavailable())?
                .get(index)
                != Some(&false)
            || self.expiry_protects(row.sequence, &row.recipient_bindings[index].0)
        {
            return Err(unavailable());
        }
        let before = row
            .terminal
            .lock()
            .map_err(|_| unavailable())?
            .get(index)
            .copied()
            .flatten()
            .filter(|value| !value.released)
            .ok_or_else(unavailable)?;
        let bytes = super::storage_policy::source_metadata_bytes(row)
            .saturating_add(4096)
            .saturating_add(2 * super::auxiliary::OUTCOME_BYTES)
            .saturating_add(
                8 * (native.conversation_id().len()
                    + native.incarnation().len()
                    + row.recipient_bindings[index].0.as_str().len()),
            );
        let lease = checkpoint::reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            bytes,
            self.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )?;
        let digest = row.envelope.storage_digest(&policy.scope)?;
        let protection = super::native_input::protect_slot(row, index)?;
        Ok(MessageTerminalHandoffAnchor(Arc::new(Source {
            runtime: self.acceptance_runtime.clone(),
            scope: policy.scope.clone(),
            row: row.clone(),
            index,
            sender_floor,
            digest,
            native: native.clone(),
            before,
            live,
            _protection: protection,
            _lease: lease,
        })))
    }
}

/// Content-free refusal preserves accepted source and exact original outcome.
fn unavailable() -> MessageError {
    MessageError::invalid_state("MMP original terminal handoff unavailable")
}

#[cfg(test)]
mod tests;
