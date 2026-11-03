//! Payload residency of the canonical accepted occurrence.
//!
//! Delivery authority, expiry, indexing and accounting use immutable metadata
//! independently of the payload cache. Acquiring bytes is explicit and never
//! performs I/O. A weak reference preserves visibility of external delivery or
//! worker pins after the cache releases its own reference. They can be reused
//! without allocating a second payload. Custody continues to charge the original
//! logical payload length independently of its residency.
//!
//! Trusted callers may release only an original storage-qualified cache pin.
//! Reloads reserve one exact nonce and preserve every delivery fact. Automatic
//! product eviction, global cache accounting and cold recovery are separate
//! integration work; current logical custody limits still charge absent bytes.
//! Consumers cannot fabricate an empty envelope, skip an unread prefix or
//! silently produce an incomplete snapshot.

use std::ops::Deref;
use std::sync::{Arc, Mutex, OnceLock, Weak};

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
    /// Exact issued reload nonce, retained until actual worker settlement.
    loading: Option<Arc<()>>,
    /// Unpublished worker output remains charged until publication or drop.
    loaded: Weak<Envelope>,
}

/// Canonical immutable metadata with explicit, nonblocking payload acquisition.
#[derive(Debug)]
pub(in crate::messaging) struct RetainedEnvelope {
    metadata: EnvelopeMetadata,
    /// Positive original storage qualification, never restored from text.
    backing: OnceLock<(MessageStorageScope, [u8; 32])>,
    residency: Mutex<PayloadResidency>,
}

impl RetainedEnvelope {
    /// Records immutable metadata without making a second payload allocation.
    pub(in crate::messaging) fn new(envelope: Arc<Envelope>) -> Self {
        Self {
            backing: OnceLock::new(),
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
                loading: None,
                loaded: Weak::new(),
            }),
        }
    }

    /// Attaches the exact successful original root/fingerprint once at adoption.
    pub(super) fn bind_storage(&self, scope: &MessageStorageScope, digest: [u8; 32]) -> Result<()> {
        let backing = self.backing.get_or_init(|| (scope.clone(), digest));
        if backing.0 != *scope || backing.1 != digest {
            return Err(MessageError::invalid_state(
                "MMP canonical storage qualification changed",
            ));
        }
        Ok(())
    }

    /// Requires live original root identity; matching source labels are insufficient.
    pub(super) fn storage_digest(&self, scope: &MessageStorageScope) -> Result<[u8; 32]> {
        self.backing
            .get()
            .filter(|(original, _)| original == scope)
            .map(|(_, digest)| *digest)
            .ok_or_else(|| {
                MessageError::invalid_state("MMP source lacks original storage qualification")
            })
    }

    /// Releases only the cache pin, retaining metadata and any external pins.
    pub(super) fn release_stored(&self, scope: &MessageStorageScope) -> Result<()> {
        self.storage_digest(scope)?;
        let mut state = self.residency.lock().map_err(|_| unavailable())?;
        if state.loading.is_some() {
            return Err(MessageError::conflict(
                "MMP source reload is already issued",
            ));
        }
        state.cached = None;
        Ok(())
    }

    /// Reserves one missing allocation without I/O or an implicit payload clone.
    pub(super) fn begin_reload(&self, nonce: &Arc<()>) -> Result<bool> {
        let mut state = self.residency.lock().map_err(|_| unavailable())?;
        if state.loading.is_some() {
            return Err(MessageError::conflict(
                "MMP source reload is already issued",
            ));
        }
        if state.cached.is_some() || state.allocation.upgrade().is_some() {
            return Ok(false);
        }
        state.loading = Some(nonce.clone());
        Ok(true)
    }

    /// Reissues only a joined original, preserving its exact source and budget.
    pub(super) fn replace_reload(&self, previous: &Arc<()>, nonce: &Arc<()>) -> Result<()> {
        let mut state = self.residency.lock().map_err(|_| unavailable())?;
        if !state
            .loading
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, previous))
        {
            return Err(unavailable());
        }
        if state.loaded.strong_count() > 0 {
            return Err(MessageError::conflict(
                "MMP original payload read still owns bytes",
            ));
        }
        state.loaded = Weak::new();
        state.loading = Some(nonce.clone());
        Ok(())
    }

    /// Records one validated worker allocation before it can escape to a result.
    /// Retained positive output forbids a second allocation for the same source.
    pub(super) fn register_loaded(&self, nonce: &Arc<()>, envelope: &Arc<Envelope>) -> Result<()> {
        let mut state = self.residency.lock().map_err(|_| unavailable())?;
        if !state
            .loading
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, nonce))
            || state.loaded.strong_count() > 0
        {
            return Err(unavailable());
        }
        state.loaded = Arc::downgrade(envelope);
        Ok(())
    }

    /// Installs only the exact validated allocation, or settles an obsolete read.
    /// The caller retains bytes on error; success consumes its cache ownership.
    pub(super) fn finish_reload(
        &self,
        nonce: &Arc<()>,
        bytes: &mut Option<Arc<Envelope>>,
    ) -> Result<()> {
        let mut state = self.residency.lock().map_err(|_| unavailable())?;
        if !state
            .loading
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, nonce))
        {
            return Err(unavailable());
        }
        if let Some(envelope) = bytes.as_ref()
            && !state
                .loaded
                .upgrade()
                .is_some_and(|original| Arc::ptr_eq(&original, envelope))
        {
            return Err(unavailable());
        }
        if let Some(envelope) = bytes.take() {
            state.allocation = Arc::downgrade(&envelope);
            state.cached = Some(envelope);
        }
        state.loading = None;
        state.loaded = Weak::new();
        Ok(())
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

    /// Returns logical bytes pinned by cache/consumers or reserved by a read.
    /// Poison conservatively retains the full charge instead of understating it.
    #[cfg(test)]
    pub(in crate::messaging) fn resident_bytes(&self) -> usize {
        self.residency.lock().map_or(self.payload_bytes, |state| {
            if state.cached.is_some()
                || state.allocation.strong_count() > 0
                || state.loading.is_some()
                || state.loaded.strong_count() > 0
            {
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

/// Bounded content-free diagnostic for poisoned or changed load ownership.
fn unavailable() -> MessageError {
    MessageError::invalid_state("MMP source reload ownership unavailable")
}
