//! Exact-root reload of an already accepted canonical payload.
//!
//! This is a read of an original occurrence, never admission or a second inbox.
//! Storage adoption supplies its nonserialized root and original fingerprint;
//! a missing payload can then acquire one linear owned load. Workers verify the
//! decoded source against immutable fields and fingerprint before returning a
//! private allocation. Publication preserves every offer, recipient commit,
//! TTL basis and receipt. Only an actual joined attempt can be reissued.
//!
//! Normal product eviction and larger disk admission remain separate integration
//! work. Existing logical custody limits continue to charge absent bytes; these
//! APIs do not enlarge the cache or permit unqualified snapshot restoration.

use std::sync::Weak;

use super::*;

/// Immutable identity of one issued load; contains no strong source/payload pin.
struct Identity {
    owner: Arc<()>,
    scope: MessageStorageScope,
    source: Weak<QueuedEnvelope>,
    sequence: MessageSequence,
    digest: [u8; 32],
    nonce: Arc<()>,
    limits: MessageAcceptanceDecodeLimits,
}

/// Payload-free escrow identifying the exact actual worker that must be joined.
#[derive(Clone)]
pub struct MessagePayloadLoadAttempt(Arc<Identity>);

impl MessagePayloadLoadAttempt {
    /// Compares private original attempt allocations, not matching source IDs.
    pub fn same_attempt(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Linear input to a trusted synchronous read port on the owned worker.
pub struct MessagePayloadLoadWrite {
    identity: Arc<Identity>,
    source: Arc<QueuedEnvelope>,
}

/// Joined result retained until exact publication or reissue. Diagnostics redact
/// all source content. Successful publication removes every strong source pin.
pub struct MessagePayloadLoadResult {
    identity: Arc<Identity>,
    source: Option<Arc<QueuedEnvelope>>,
    envelope: Option<Arc<Envelope>>,
    error: Option<MessageError>,
    published: Option<bool>,
}

/// Trusted original-root reader. It must verify complete committed framing and
/// return only after synchronous I/O finishes. Record data alone is not authority.
pub trait MessagePayloadLoadStore {
    /// Loads the original committed source under this request's exact bounds.
    fn load(&mut self, request: &MessagePayloadLoadWrite) -> Result<MessageAcceptanceSource>;
}

/// Nonblocking actual join witness; timeouts/status text cannot release a load.
pub trait MessagePayloadLoadJoin {
    /// Qualifies the retained handle's observed termination for this attempt.
    fn qualify_joined(&mut self, attempt: &MessagePayloadLoadAttempt) -> Result<()>;
}

impl MessagePayloadLoadWrite {
    /// Captures payload-free identity before moving the original into a worker.
    pub fn attempt(&self) -> MessagePayloadLoadAttempt {
        MessagePayloadLoadAttempt(self.identity.clone())
    }

    /// Returns the original occurrence, never a fresh admission allocation.
    pub fn sequence(&self) -> MessageSequence {
        self.identity.sequence
    }

    /// Supplies the exact original content checksum to the trusted storage port.
    pub fn source_digest(&self) -> [u8; 32] {
        self.identity.digest
    }

    /// Supplies bounded original decoding policy, not policy chosen by disk data.
    pub fn decode_limits(&self) -> MessageAcceptanceDecodeLimits {
        self.identity.limits
    }

    /// Executes one read and validates source identity without publishing bytes.
    /// Payload strings move from decoder to the result without a second copy.
    pub fn load(self, store: &mut impl MessagePayloadLoadStore) -> MessagePayloadLoadResult {
        let loaded = store
            .load(&self)
            .and_then(|source| self.validate(source))
            .and_then(|envelope| {
                let envelope = Arc::new(envelope);
                self.source
                    .envelope
                    .register_loaded(&self.identity.nonce, &envelope)?;
                Ok(envelope)
            });
        let (envelope, error) = match loaded {
            Ok(envelope) => (Some(envelope), None),
            Err(error) => (None, Some(error)),
        };
        MessagePayloadLoadResult {
            identity: self.identity,
            source: Some(self.source),
            envelope,
            error,
            published: None,
        }
    }

    /// Checks every frozen field before using the original canonical fingerprint.
    fn validate(&self, source: MessageAcceptanceSource) -> Result<Envelope> {
        let row = &self.source;
        let audience_matches = match &row.audience {
            ResolvedMessageAudience::Session => {
                source.audience().kind == "session" && source.audience().project_scope.is_none()
            }
            ResolvedMessageAudience::Project(scope) => {
                source.audience().kind == "project"
                    && source.audience().project_scope.as_deref() == Some(scope.snapshot_value())
            }
        };
        if source.sequence() != row.sequence
            || source.accepted_at_ms() != row.accepted_at_ms
            || Some(source.sender_registered_after_sequence())
                != row.sender_registered_after_sequence
            || source.envelope().payload.len() != row.envelope.payload_bytes
            || !audience_matches
            || source.recipient_bindings().len() != row.recipient_bindings.len()
            || !source
                .recipient_bindings()
                .iter()
                .zip(&row.recipient_bindings)
                .all(|(actual, (id, floor))| {
                    actual.recipient == id.as_str() && actual.registered_after_sequence == *floor
                })
        {
            return Err(changed());
        }
        let envelope = source.into_envelope()?;
        let digest =
            admission::source_digest_with_envelope(row, &envelope).map_err(|_| changed())?;
        if digest != self.identity.digest {
            return Err(changed());
        }
        Ok(envelope)
    }
}

impl MessagePayloadLoadResult {
    /// Borrows a bounded storage/validation diagnostic, without source content.
    pub fn error(&self) -> Option<&MessageError> {
        self.error.as_ref()
    }
}

impl MessageService {
    /// Releases an exact storage-backed cache pin, preserving canonical facts.
    /// Rejects memory-only/restored/foreign-root sources and active ACKs/loads.
    /// Existing external envelope pins remain alive and reusable; callers must
    /// count them until drop rather than treating this operation as freed bytes.
    pub fn release_stored_payload(
        &mut self,
        sequence: MessageSequence,
        scope: &MessageStorageScope,
    ) -> Result<()> {
        if self.acknowledgement.is_some() {
            return Err(MessageError::conflict(
                "MMP active ACK retains source payloads",
            ));
        }
        self.queued_by_sequence
            .get(&sequence)
            .ok_or_else(changed)?
            .envelope
            .release_stored(scope)
    }

    /// Issues one original-root read for a missing canonical payload. `None`
    /// means an existing cache/external allocation already supplies those bytes.
    /// No I/O, new acceptance, recipient resolution or offer occurs here.
    pub fn begin_payload_load(
        &mut self,
        sequence: MessageSequence,
        scope: &MessageStorageScope,
    ) -> Result<Option<MessagePayloadLoadWrite>> {
        let row = self.queued_by_sequence.get(&sequence).ok_or_else(changed)?;
        let digest = row.envelope.storage_digest(scope)?;
        let nonce = Arc::new(());
        let identity = Arc::new(Identity {
            owner: self.acceptance_runtime.clone(),
            scope: scope.clone(),
            source: Arc::downgrade(row),
            sequence,
            digest,
            nonce,
            limits: MessageAcceptanceDecodeLimits {
                payload_bytes: row.envelope.payload_bytes,
                auxiliary_bytes: auxiliary::retained_envelope_bytes(&row.envelope),
                recipient_count: row.recipient_bindings.len(),
                recipient_bytes: row
                    .recipient_bindings
                    .iter()
                    .fold(0usize, |bytes, (id, _)| {
                        bytes.saturating_add(192).saturating_add(id.as_str().len())
                    }),
            },
        });
        if !row.envelope.begin_reload(&identity.nonce)? {
            return Ok(None);
        }
        Ok(Some(MessagePayloadLoadWrite {
            identity,
            source: row.clone(),
        }))
    }

    /// Validates the original runtime even when a settled read is obsolete.
    /// This grants no source publication or join authority.
    pub fn validate_payload_load_owner(&self, attempt: &MessagePayloadLoadAttempt) -> Result<()> {
        if Arc::ptr_eq(&self.acceptance_runtime, &attempt.0.owner) {
            Ok(())
        } else {
            Err(changed())
        }
    }

    /// Reissues an exact joined load. Obsolete source returns `None`; it cannot
    /// install bytes into another occurrence, service or registration interval.
    pub fn recover_payload_load(
        &mut self,
        attempt: &MessagePayloadLoadAttempt,
        joined: &mut impl MessagePayloadLoadJoin,
    ) -> Result<Option<MessagePayloadLoadWrite>> {
        if !Arc::ptr_eq(&self.acceptance_runtime, &attempt.0.owner) {
            return Err(changed());
        }
        joined.qualify_joined(attempt)?;
        let Some(row) = attempt.0.source.upgrade() else {
            return Ok(None);
        };
        if !self
            .queued_by_sequence
            .get(&attempt.0.sequence)
            .is_some_and(|live| Arc::ptr_eq(live, &row))
        {
            row.envelope.finish_reload(&attempt.0.nonce, &mut None)?;
            return Ok(None);
        }
        if row.envelope.storage_digest(&attempt.0.scope)? != attempt.0.digest {
            return Err(changed());
        }
        let nonce = Arc::new(());
        let identity = Arc::new(Identity {
            owner: attempt.0.owner.clone(),
            scope: attempt.0.scope.clone(),
            source: Arc::downgrade(&row),
            sequence: attempt.0.sequence,
            digest: attempt.0.digest,
            nonce,
            limits: attempt.0.limits,
        });
        row.envelope
            .replace_reload(&attempt.0.nonce, &identity.nonce)?;
        Ok(Some(MessagePayloadLoadWrite {
            identity,
            source: row,
        }))
    }

    /// Adopts validated bytes without changing any delivery/receipt fact. Returns
    /// false when the source is no longer retained. On error the result retains
    /// its original bytes; a foreign/stale actor cannot consume or publish them.
    pub fn publish_payload_load(&mut self, result: &mut MessagePayloadLoadResult) -> Result<bool> {
        if !Arc::ptr_eq(&self.acceptance_runtime, &result.identity.owner) {
            return Err(changed());
        }
        if let Some(published) = result.published {
            return Ok(published);
        }
        if let Some(error) = &result.error {
            return Err(error.clone());
        }
        let row = result.source.as_ref().ok_or_else(changed)?;
        if row.envelope.storage_digest(&result.identity.scope)? != result.identity.digest {
            return Err(changed());
        }
        let retained = self
            .queued_by_sequence
            .get(&result.identity.sequence)
            .is_some_and(|live| Arc::ptr_eq(live, row));
        if retained {
            row.envelope
                .finish_reload(&result.identity.nonce, &mut result.envelope)?;
        } else {
            row.envelope
                .finish_reload(&result.identity.nonce, &mut None)?;
            result.envelope.take();
        }
        result.source.take();
        result.published = Some(retained);
        Ok(retained)
    }
}

/// Content-free ownership/fingerprint mismatch rather than guessed empty state.
fn changed() -> MessageError {
    MessageError::invalid_state("MMP original payload load changed")
}

#[cfg(test)]
mod tests;
