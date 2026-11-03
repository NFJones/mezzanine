//! Payload residency of the canonical accepted occurrence.
//!
//! Delivery authority, expiry, indexing and accounting use immutable metadata
//! independently of the payload cache. Acquiring bytes is explicit and never
//! performs I/O. A weak reference preserves visibility of external delivery or
//! worker pins after the cache releases its own reference. They can be reused
//! without allocating a second payload. Custody continues to charge the original
//! logical payload length independently of its residency.
//!
//! Production eviction is deliberately unavailable until a store-qualified
//! hydration owner and recovery path can replace the resident bytes. Tests can
//! exercise the absent-payload boundary so consumers cannot fabricate an empty
//! envelope, skip an unread prefix or silently produce an incomplete snapshot.

use std::ops::Deref;
use std::sync::{Arc, Mutex, Weak};

use super::*;

/// Immutable routing and identity fields, containing no payload allocation.
#[derive(Debug)]
pub(in crate::messaging) struct EnvelopeMetadata {
    /// Original validated protocol; metadata access grants no transport rights.
    pub protocol: &'static str,
    /// Original idempotency key, also retained by receipt indexes.
    pub id: String,
    /// Original validated send/response kind.
    pub message_type: String,
    /// Authored timestamp, distinct from the acceptance clock used for expiry.
    pub time: String,
    /// Complete original identity, independent of mutable discovery.
    pub sender: SenderIdentity,
    /// Original selector; frozen recipient bindings remain on the occurrence.
    pub recipient: Recipient,
    /// Original optional request correlation, never a task capability.
    pub correlation_id: Option<String>,
    /// Original TTL duration; absence means no expiry.
    pub ttl_ms: Option<u64>,
    /// Original payload media type.
    pub content_type: String,
    /// Original bounded extension entries, preserved without reinterpretation.
    pub extension_fields: Vec<(String, String)>,
    /// Original UTF-8 byte length, also available while bytes are absent.
    pub payload_bytes: usize,
}

/// One cache owner and the last allocation it handed to a consumer.
#[derive(Debug)]
struct PayloadResidency {
    cached: Option<Arc<Envelope>>,
    allocation: Weak<Envelope>,
}

/// Canonical immutable metadata with explicit, nonblocking payload acquisition.
#[derive(Debug)]
pub(in crate::messaging) struct RetainedEnvelope {
    metadata: EnvelopeMetadata,
    residency: Mutex<PayloadResidency>,
}

impl RetainedEnvelope {
    /// Records immutable metadata without making a second payload allocation.
    pub(in crate::messaging) fn new(envelope: Arc<Envelope>) -> Self {
        Self {
            metadata: EnvelopeMetadata {
                protocol: envelope.protocol,
                id: envelope.id.clone(),
                message_type: envelope.message_type.clone(),
                time: envelope.time.clone(),
                sender: envelope.sender.clone(),
                recipient: envelope.recipient.clone(),
                correlation_id: envelope.correlation_id.clone(),
                ttl_ms: envelope.ttl_ms,
                content_type: envelope.content_type.clone(),
                extension_fields: envelope.extension_fields.clone(),
                payload_bytes: envelope.payload.len(),
            },
            residency: Mutex::new(PayloadResidency {
                allocation: Arc::downgrade(&envelope),
                cached: Some(envelope),
            }),
        }
    }

    /// Acquires existing bytes or returns `InvalidState` requiring hydration.
    /// External pins remain reusable after cache release. Poison fails closed;
    /// this accessor never reads storage or changes delivery/offer state.
    pub(in crate::messaging) fn resident(&self) -> Result<Arc<Envelope>> {
        let state = self
            .residency
            .lock()
            .map_err(|_| MessageError::invalid_state("MMP payload residency is unavailable"))?;
        state
            .cached
            .clone()
            .or_else(|| state.allocation.upgrade())
            .ok_or_else(|| {
                MessageError::invalid_state(
                    "MMP source payload is not resident; hydration required",
                )
            })
    }

    /// Returns logical bytes still pinned by the cache or external consumers.
    /// Poison conservatively retains the full charge instead of understating it.
    #[cfg(test)]
    pub(in crate::messaging) fn resident_bytes(&self) -> usize {
        self.residency.lock().map_or(self.payload_bytes, |state| {
            if state.cached.is_some() || state.allocation.strong_count() > 0 {
                self.payload_bytes
            } else {
                0
            }
        })
    }

    /// Exercises cache release without exposing an unqualified production API.
    #[cfg(test)]
    pub(super) fn release_cache_for_test(&self) {
        self.residency.lock().unwrap().cached = None;
    }
}

impl Deref for RetainedEnvelope {
    type Target = EnvelopeMetadata;

    /// Metadata access cannot acquire payloads, block on storage or fabricate one.
    fn deref(&self) -> &Self::Target {
        &self.metadata
    }
}
