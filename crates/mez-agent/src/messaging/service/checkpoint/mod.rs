//! Bounded cold checkpoint capture of the existing message owner.
//!
//! Checkpoints keep original store references and native namespace metadata,
//! never copied payloads or substituted empty envelopes. A trusted storage port
//! verifies every referenced original before a binding may be attached. These
//! serializable facts are not live registration, reconnect, native context or
//! restore authority; generic message restoration explicitly refuses schema10.

use super::*;
use serde::{Deserialize, Serialize};

const MAX_CHECKPOINT_BYTES: usize = 8 * 1024 * 1024;

/// Actual original store identity observed by the owned checkpoint worker.
/// No field is a path or reconnect credential, and these bytes alone grant none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageStorageBinding {
    /// Trusted original session owner label, rechecked by storage.
    pub owner: String,
    /// Random persisted original SQLite allocation identity.
    pub store_id: [u8; 16],
    /// Settled transaction generation at capture qualification.
    pub generation: u64,
    /// Persisted sequence watermark; no allocator authority by itself.
    pub sequence_watermark: u64,
    /// Kernel device of the original held directory.
    pub directory_device: u64,
    /// Kernel inode of the original held directory.
    pub directory_inode: u64,
    /// Kernel device of the actual original database.
    pub database_device: u64,
    /// Kernel inode of the actual original database.
    pub database_inode: u64,
}

/// Descriptive native namespace captured under an actual current registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageNativeCheckpoint {
    /// Original native registration attribution, never a reconnect bearer.
    pub agent_id: String,
    /// Descriptive namespace of the captured actual registration allocation.
    pub registration_namespace: [u8; 16],
    /// Descriptive original shell epoch, unusable by generic shell restore.
    pub shell_incarnation: [u8; 16],
    /// Derived conversation/incarnation attribution frozen by this tuple.
    pub owner: MessageNativeOwner,
    /// Whether the captured product shell had a qualified durable parent.
    pub admission_available: bool,
    /// Whether the captured shell was outside a temporary child binding.
    pub delivery_available: bool,
}

/// Original cold source and frozen mutable delivery slots, with no payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageStorageSourceSnapshot {
    /// Original accepted occurrence, strictly below the captured allocator.
    pub sequence: u64,
    /// SHA256 of the complete immutable source, verified from committed chunks.
    pub source_sha256: [u8; 32],
    /// Original UTF-8 body length, without acquiring its allocation.
    pub payload_bytes: usize,
    /// Immutable original acceptance clock, covered by the referenced source digest.
    pub accepted_at_ms: u64,
    /// Original acceptance-relative TTL; absence never permits an expired receipt.
    pub ttl_ms: Option<u64>,
    /// Whether the occurrence remained in disposable replay at capture.
    pub replay: bool,
    /// Whether original recipient/outcome custody remained unsettled.
    pub pending: bool,
    /// Exact original acceptance receipt; never reconstructed from a body.
    pub delivery: MessageDeliverySnapshot,
    /// Sorted frozen recipient intervals in immutable source slot order.
    pub recipient_bindings: Vec<MessageRecipientBindingSnapshot>,
    /// Genuine selection clocks in frozen slot order.
    pub offered_at_ms: Vec<Option<u64>>,
    /// Explicit committed delivery slots, independently of subscription cursors.
    pub acknowledged: Vec<bool>,
    /// Unsatisfied outcome state in frozen slot order; never successful ACK.
    pub terminal: Vec<Option<MessageDeliveryTerminalSnapshot>>,
    /// First ACK-prefix expiry exclusion clock, if one was actually observed.
    pub ack_expiry_observed_at_ms: Option<u64>,
}

/// Versioned schema10 checkpoint body. Binding is absent only in a private draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageStorageSnapshot {
    /// Cold reference framing version, currently one.
    pub format_version: u32,
    /// Verified actual store facts; absent only inside an unpublishable draft.
    pub binding: Option<MessageStorageBinding>,
    /// Sorted current native namespace tuples; raw endpoints are excluded.
    pub native: Vec<MessageNativeCheckpoint>,
    /// Sorted unique original references in the replay/custody union.
    pub sources: Vec<MessageStorageSourceSnapshot>,
}

/// Immutable payload-free capture shared only with its actual storage worker.
struct Capture {
    scope: MessageStorageScope,
    identity: Arc<()>,
    snapshot: MessageServiceSnapshot,
    lease: MessageStorageSnapshotLease,
}
/// Original request retained outside the actual worker through cancellation/loss.
pub struct MessageStorageSnapshotRequest(Arc<Capture>);
/// Linear actual verification input, sharing metadata rather than copying it.
pub struct MessageStorageSnapshotWrite(Arc<Capture>);
/// Positive fact for this exact request; contains no copied checkpoint metadata.
pub struct MessageStorageSnapshotVerified {
    identity: Arc<()>,
    binding: MessageStorageBinding,
}
/// Trusted synchronous read port; the product owns physical scope/transaction.
pub trait MessageStorageSnapshotStore {
    /// Reads originals in one owned store snapshot and rejects any changed source.
    fn verify(&mut self, request: &MessageStorageSnapshotWrite) -> Result<MessageStorageBinding>;
}
impl MessageStorageSnapshotRequest {
    /// Shares immutable metadata with one actual root-owned worker.
    pub fn write(&self) -> MessageStorageSnapshotWrite {
        MessageStorageSnapshotWrite(self.0.clone())
    }
    /// Moves the original capture after its actual worker joined and qualified it.
    pub fn finish(
        self,
        verified: MessageStorageSnapshotVerified,
    ) -> Result<(MessageServiceSnapshot, MessageStorageSnapshotLease)> {
        if !Arc::ptr_eq(&self.0.identity, &verified.identity) {
            return Err(changed());
        }
        let mut capture = Arc::try_unwrap(self.0).map_err(|_| changed())?;
        let data = capture
            .snapshot
            .storage_snapshot
            .as_mut()
            .ok_or_else(changed)?;
        data.binding = Some(verified.binding);
        capture.snapshot.validate_storage_snapshot()?;
        Ok((capture.snapshot, capture.lease))
    }
}
impl MessageStorageSnapshotWrite {
    /// Checks the live original root allocation, never equal serialized labels.
    pub fn belongs_to(&self, scope: &MessageStorageScope) -> bool {
        self.0.scope == *scope
    }
    /// Borrows original references without exposing capture/native capabilities.
    pub fn sources(&self) -> &[MessageStorageSourceSnapshot] {
        self.0
            .snapshot
            .storage_snapshot
            .as_ref()
            .map(|s| s.sources.as_slice())
            .unwrap_or(&[])
    }
    /// Returns the captured allocator floor; verified originals must lie below it.
    pub fn next_sequence(&self) -> u64 {
        self.0.snapshot.next_sequence
    }
    /// Completes only after the trusted port returned from its actual read.
    pub fn verify(
        self,
        store: &mut impl MessageStorageSnapshotStore,
    ) -> Result<MessageStorageSnapshotVerified> {
        let binding = store.verify(&self)?;
        Ok(MessageStorageSnapshotVerified {
            identity: self.0.identity.clone(),
            binding,
        })
    }
}

impl MessageService {
    /// Captures metadata and references without acquiring any retained payload.
    /// Unsettled writes/ACKs, foreign scopes or over-budget metadata refuse capture.
    pub fn prepare_storage_snapshot(
        &self,
        scope: &MessageStorageScope,
    ) -> Result<MessageStorageSnapshotRequest> {
        self.require_storage_scope(Some(scope))?;
        if self.storage_custody.is_none()
            || self.admission.is_some()
            || self.acknowledgement.is_some()
        {
            return Err(changed());
        }
        // Preflight while fields remain borrowed. This intentionally reserves
        // conservative headroom for both registry copies and mutable slot arrays.
        let mut bytes = 4096usize;
        for (id, binding) in &self.native_bindings {
            if !self.sender_registration_is_current(binding.registration()) {
                continue;
            }
            let identity = self.registered.get(id).ok_or_else(changed)?;
            bytes = bytes.saturating_add(identity_charge(identity));
            bytes = bytes
                .saturating_add(2048)
                .saturating_add(id.as_str().len().saturating_mul(8));
        }
        for row in self.queued_by_sequence.values() {
            bytes = bytes
                .saturating_add(512)
                .saturating_add(row.envelope.id.len().saturating_mul(4));
            for (id, _) in &row.recipient_bindings {
                bytes = bytes
                    .saturating_add(512)
                    .saturating_add(id.as_str().len().saturating_mul(4));
            }
        }
        if bytes > MAX_CHECKPOINT_BYTES {
            return Err(capacity());
        }
        // Eight conservative projections cover the owned capture, repository
        // clone, JSON and line escaping/format temporaries, and final output.
        // Original source/cache and worker headroom remain independently charged.
        let policy = self.storage_custody.as_ref().ok_or_else(changed)?;
        let charge = bytes.checked_mul(8).ok_or_else(capacity)?;
        let lease = reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            charge,
            self.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )?;
        let native = self.capture_native_checkpoint();
        let ids: HashSet<&str> = native.iter().map(|n| n.agent_id.as_str()).collect();
        let mut registered_agents: Vec<_> = self
            .registered
            .values()
            .filter(|r| ids.contains(r.agent_id.as_str()))
            .map(identity_snapshot)
            .collect();
        registered_agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        let mut presence: Vec<_> = self
            .presence
            .values()
            .filter(|r| ids.contains(r.identity.agent_id.as_str()))
            .map(presence_snapshot)
            .collect();
        presence.sort_by(|a, b| a.identity.agent_id.cmp(&b.identity.agent_id));
        let mut subscriptions: Vec<_> = self
            .subscriptions
            .values()
            .filter(|r| ids.contains(r.recipient.as_str()))
            .map(cursor_snapshot)
            .collect();
        subscriptions.sort_by(|a, b| a.recipient.cmp(&b.recipient));
        let mut floors: Vec<_> = self
            .registration_floors
            .iter()
            .filter(|(id, _)| ids.contains(id.as_str()))
            .map(|(id, f)| MessageRecipientBindingSnapshot {
                recipient: id.to_string(),
                registered_after_sequence: *f,
            })
            .collect();
        floors.sort_by(|a, b| a.recipient.cmp(&b.recipient));
        let replay: HashSet<_> = self.queue.iter().map(|r| r.sequence).collect();
        let mut sources = Vec::new();
        for row in self.queued_by_sequence.values() {
            let receipt = self
                .accepted_messages
                .get(&row.envelope.id)
                .ok_or_else(changed)?;
            sources.push(MessageStorageSourceSnapshot {
                sequence: row.sequence,
                source_sha256: row.envelope.storage_digest(scope)?,
                payload_bytes: row.envelope.payload_bytes,
                accepted_at_ms: row.accepted_at_ms,
                ttl_ms: row.envelope.ttl_ms,
                replay: replay.contains(&row.sequence),
                pending: self.pending.contains_key(&row.sequence),
                delivery: MessageDeliverySnapshot {
                    accepted: receipt.delivery.accepted,
                    message_id: receipt.delivery.message_id.clone(),
                    sequence: receipt.delivery.sequence,
                    queued_recipients: receipt.delivery.queued_recipients,
                    status: super::snapshot_codec::delivery_status_name(receipt.delivery.status)
                        .into(),
                },
                recipient_bindings: row
                    .recipient_bindings
                    .iter()
                    .map(|(id, f)| MessageRecipientBindingSnapshot {
                        recipient: id.to_string(),
                        registered_after_sequence: *f,
                    })
                    .collect(),
                offered_at_ms: row.offered_at_ms.lock().map_err(|_| changed())?.clone(),
                acknowledged: row.acknowledged.lock().map_err(|_| changed())?.clone(),
                terminal: row.terminal.lock().map_err(|_| changed())?.clone(),
                ack_expiry_observed_at_ms: *row
                    .ack_expiry_observed_at_ms
                    .lock()
                    .map_err(|_| changed())?,
            });
        }
        let snapshot = MessageServiceSnapshot {
            protocol: MMP_PROTOCOL.into(),
            schema_version: 10,
            next_sequence: self.next_sequence,
            retention_messages: self.retention_messages,
            retention_bytes: self.retention_bytes,
            registered_agents,
            registration_floors: Some(floors),
            presence,
            subscriptions,
            retired_delivery_floors: Vec::new(),
            sender_provenance: Some(Vec::new()),
            retained_messages: Vec::new(),
            custody_messages: Some(Vec::new()),
            accepted_messages: Vec::new(),
            storage_snapshot: Some(MessageStorageSnapshot {
                format_version: 1,
                binding: None,
                native,
                sources,
            }),
        };
        validate(&snapshot, false)?;
        Ok(MessageStorageSnapshotRequest(Arc::new(Capture {
            scope: scope.clone(),
            identity: Arc::new(()),
            lease,
            snapshot,
        })))
    }
}
impl MessageServiceSnapshot {
    /// Validates schema10 data structurally without creating any live owner.
    /// Actual source/store and native restoration qualification remain required.
    pub fn validate_storage_snapshot(&self) -> Result<()> {
        validate(self, true)
    }
}
/// Rejects cross-record contradictions before a capture is published or persisted.
fn validate(s: &MessageServiceSnapshot, bound: bool) -> Result<()> {
    validate_encoded_size(s)?;
    if s.schema_version != 10
        || s.protocol != MMP_PROTOCOL
        || s.next_sequence == 0
        || s.retention_messages == 0
        || s.retention_bytes == 0
        || !s.retained_messages.is_empty()
        || s.custody_messages.as_ref().is_none_or(|v| !v.is_empty())
        || !s.accepted_messages.is_empty()
        || !s.retired_delivery_floors.is_empty()
        || s.sender_provenance.as_ref().is_none_or(|v| !v.is_empty())
    {
        return Err(changed());
    }
    let data = s.storage_snapshot.as_ref().ok_or_else(changed)?;
    if data.format_version != 1 || (bound && data.binding.is_none()) {
        return Err(changed());
    }
    if data.binding.as_ref().is_some_and(|b| {
        b.owner.is_empty()
            || b.owner.len() > 256
            || b.owner.chars().any(char::is_control)
            || b.directory_inode == 0
            || b.database_inode == 0
            || b.sequence_watermark >= s.next_sequence
            || b.store_id.iter().all(|b| *b == 0)
            || b.generation > i64::MAX as u64
            || b.sequence_watermark > i64::MAX as u64
    }) {
        return Err(changed());
    }
    let mut previous = 0;
    let mut payload = 0usize;
    let mut obligations = 0usize;
    let mut message_ids = HashSet::new();
    let mut per_recipient = HashMap::new();
    for row in &data.sources {
        let count = row.recipient_bindings.len();
        if row.sequence <= previous
            || row.sequence >= s.next_sequence
            || (!row.replay && !row.pending)
            || row.payload_bytes > 1024 * 1024
            || count == 0
            || row.offered_at_ms.len() != count
            || row.acknowledged.len() != count
            || row.terminal.len() != count
            || !row.delivery.accepted
            || row.delivery.sequence != row.sequence
            || row.delivery.message_id.is_empty()
            || !message_ids.insert(row.delivery.message_id.as_str())
            || row.ttl_ms == Some(0)
            || !valid_receipt_status(row)
            || data
                .binding
                .as_ref()
                .is_some_and(|b| row.sequence > b.sequence_watermark)
            || row.delivery.queued_recipients != count
            || row.pending
                != row
                    .acknowledged
                    .iter()
                    .zip(&row.terminal)
                    .any(|(ack, terminal)| !*ack && terminal.is_none_or(|t| !t.released))
        {
            return Err(changed());
        }
        previous = row.sequence;
        payload = payload.saturating_add(row.payload_bytes);
        obligations = obligations.saturating_add(count);
        let mut id = None;
        for (i, b) in row.recipient_bindings.iter().enumerate() {
            if b.recipient.is_empty()
                || b.recipient.chars().any(char::is_control)
                || b.registered_after_sequence >= row.sequence
                || id.is_some_and(|p: &str| p >= b.recipient.as_str())
                || (row.acknowledged[i]
                    && (row.offered_at_ms[i].is_none() || row.terminal[i].is_some()))
            {
                return Err(changed());
            }
            parse_opaque_id(&b.recipient, "MMP checkpoint recipient")?;
            if row.pending {
                let entry = per_recipient
                    .entry((b.recipient.as_str(), b.registered_after_sequence))
                    .or_insert((0usize, 0usize));
                entry.0 = entry.0.saturating_add(1);
                entry.1 = entry.1.saturating_add(row.payload_bytes);
                if entry.0 > 1024 || entry.1 > 8 * 1024 * 1024 {
                    return Err(capacity());
                }
            }
            id = Some(b.recipient.as_str());
        }
    }
    if data.sources.len() > 10_000 || payload > 64 * 1024 * 1024 || obligations > 40_000 {
        return Err(capacity());
    }
    super::native_binding::validate_native_checkpoint(&data.native)?;
    let ids: HashSet<_> = data.native.iter().map(|n| n.agent_id.as_str()).collect();
    if ids.len() != s.registered_agents.len()
        || s.presence.len() != ids.len()
        || s.registration_floors
            .as_ref()
            .is_none_or(|f| f.len() != ids.len())
    {
        return Err(changed());
    }
    let mut registered = HashMap::new();
    for r in &s.registered_agents {
        if !ids.contains(r.agent_id.as_str()) || registered.insert(r.agent_id.as_str(), r).is_some()
        {
            return Err(changed());
        }
        sender_identity_from_snapshot(r, 9)?;
    }
    let mut seen = HashSet::new();
    for presence in &s.presence {
        parse_presence_status(&presence.status)?;
        if registered.get(presence.identity.agent_id.as_str()) != Some(&&presence.identity)
            || !seen.insert(presence.identity.agent_id.as_str())
        {
            return Err(changed());
        }
    }
    seen.clear();
    for floor in s.registration_floors.as_deref().unwrap_or(&[]) {
        if !ids.contains(floor.recipient.as_str())
            || !seen.insert(floor.recipient.as_str())
            || floor.registered_after_sequence >= s.next_sequence
        {
            return Err(changed());
        }
    }
    seen.clear();
    for cursor in &s.subscriptions {
        if !ids.contains(cursor.recipient.as_str())
            || !seen.insert(cursor.recipient.as_str())
            || cursor.last_sequence >= s.next_sequence
        {
            return Err(changed());
        }
    }
    Ok(())
}

/// Bounds validation work and temporary projections before constructing indexes.
fn validate_encoded_size(s: &MessageServiceSnapshot) -> Result<()> {
    // A bounded counting sink avoids a complete encoded metadata buffer.
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(b.len());
            if self.0 > MAX_CHECKPOINT_BYTES {
                return Err(std::io::Error::other("MMP checkpoint byte limit"));
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Count(0), s).map_err(|_| capacity())?;
    Ok(())
}
/// Bounded ownership/framing diagnostic; never includes authored source content.
fn changed() -> MessageError {
    MessageError::invalid_state("MMP storage checkpoint ownership or framing unavailable")
}
/// Reports refusal before allocating beyond the checkpoint metadata policy.
fn capacity() -> MessageError {
    MessageError::new(
        MessageErrorKind::Capacity,
        "MMP storage checkpoint metadata capacity",
    )
}

/// Charges registry/presence copies before any strings or capabilities are cloned.
fn identity_charge(identity: &SenderIdentity) -> usize {
    let fields = [
        identity.agent_id.as_str(),
        identity.pane_id.as_ref().map_or("", PaneId::as_str),
        identity.window_id.as_ref().map_or("", WindowId::as_str),
        identity.role.as_deref().unwrap_or(""),
        identity.objective.as_deref().unwrap_or(""),
        identity
            .project_scope
            .as_ref()
            .map_or("", ProjectScopeId::snapshot_value),
    ];
    fields
        .into_iter()
        .chain(identity.capabilities.iter().map(String::as_str))
        .fold(1024usize, |n, v| {
            n.saturating_add(128)
                .saturating_add(v.len().saturating_mul(16))
        })
}

#[cfg(test)]
mod tests;

/// Reservation for all copies of a checkpoint during verification/publication.
/// The trusted product must retain this until its actual snapshot writer returns,
/// including when the administrative observer leaves. This is no restore right.
pub struct MessageStorageSnapshotLease {
    budget: Arc<std::sync::atomic::AtomicUsize>,
    bytes: usize,
}
impl Drop for MessageStorageSnapshotLease {
    /// Releases only this capture's reservation after its last actual owner exits.
    fn drop(&mut self) {
        self.budget
            .fetch_sub(self.bytes, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Atomically reserves capture/encoding headroom before cloning any source facts.
/// Concurrent captures share this one finite budget, independent of root clones.
fn reserve_checkpoint(
    budget: Arc<std::sync::atomic::AtomicUsize>,
    bytes: usize,
    base: usize,
    limit: usize,
) -> Result<MessageStorageSnapshotLease> {
    use std::sync::atomic::Ordering;
    let mut held = budget.load(Ordering::Acquire);
    loop {
        let next = held
            .checked_add(bytes)
            .filter(|n| base.saturating_add(*n) <= limit)
            .ok_or_else(capacity)?;
        match budget.compare_exchange_weak(held, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Ok(MessageStorageSnapshotLease { budget, bytes }),
            Err(current) => held = current,
        }
    }
}

/// Duplicate lookup may retain an expired receipt without retiring its original.
/// The status cannot be guessed from the capture clock (which may roll back),
/// but expiry must be possible under the frozen original finite TTL basis.
fn valid_receipt_status(row: &MessageStorageSourceSnapshot) -> bool {
    match row.delivery.status.as_str() {
        "accepted" => true,
        "expired" => row
            .ttl_ms
            .is_some_and(|ttl| ttl > 0 && row.accepted_at_ms.saturating_add(ttl) < u64::MAX),
        _ => false,
    }
}
