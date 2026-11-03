//! Acceptance-time native application ownership, separate from public routing.
//!
//! Trusted product code binds an actual live registration to its conversation.
//! A fresh opaque incarnation distinguishes replacement/conversation bindings
//! even when no MMP allocation advanced the public registration floor. Immutable
//! sources freeze these facts before storage; decoded facts are quarantined
//! evidence and cannot recreate live registration or provider authority. Raw
//! endpoints carry explicit absence, never persisted reconnect credentials.

use super::*;
use serde::Serialize;

/// Runtime-owned shell binding identity. Clone preserves an exact rollback;
/// new/independently resumed bindings create fresh values. This is not a bearer
/// and is never reconstructed by the generic message snapshot/source decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageNativeIncarnation(uuid::Uuid);
impl Default for MessageNativeIncarnation {
    fn default() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// Complete trusted product observation for one actual native shell binding.
/// Product code derives this from the current shell, not wire identity labels.
pub struct MessageNativeRegistration {
    /// Exact current MMP registration, fenced against replacement.
    pub registration: MessageSenderRegistration,
    /// Actual native shell binding identity, preserved through exact rollback.
    pub incarnation: MessageNativeIncarnation,
    /// Durable conversation currently owned by this shell binding.
    pub conversation_id: String,
    /// Whether this endpoint has an actual qualified application owner for new mail.
    pub admission_available: bool,
    /// False suspends delivery during a temporary ephemeral child binding.
    pub delivery_available: bool,
}

/// Bounded descriptive native destination, never a resume bearer or live proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MessageNativeOwner {
    pub(super) conversation_id: String,
    pub(super) incarnation: String,
}
impl MessageNativeOwner {
    /// Returns original conversation attribution; equality alone grants no access.
    pub fn conversation_id(&self) -> &str {
        &self.conversation_id
    }
    /// Returns the original opaque incarnation, not a reconnect credential.
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
    /// Rejects unsupported identity sizes before retaining or encoding metadata.
    pub(super) fn validate(&self) -> Result<()> {
        validate_conversation(&self.conversation_id)?;
        if self.incarnation.len() != 64
            || !self
                .incarnation
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.incarnation.bytes().all(|b| b == b'0')
        {
            return Err(MessageError::invalid_args("invalid MMP native incarnation"));
        }
        Ok(())
    }
}

/// One current application binding fenced by its actual registration allocation.
#[derive(Debug)]
pub(in crate::messaging) struct LiveNativeBinding {
    registration: MessageSenderRegistration,
    incarnation: MessageNativeIncarnation,
    admission_available: bool,
    delivery_available: bool,
    owner: MessageNativeOwner,
}

/// Frozen sender and recipient owners in immutable recipient binding order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(in crate::messaging) struct NativeBindings {
    pub(super) sender: Option<MessageNativeOwner>,
    pub(super) recipients: Vec<Option<MessageNativeOwner>>,
}
impl NativeBindings {
    /// Counts wrapper/slot and owned string metadata before admission or decode.
    pub(super) fn metadata_bytes(&self) -> usize {
        self.recipients
            .iter()
            .chain(std::iter::once(&self.sender))
            .fold(128usize, |n, owner| {
                let n = n.saturating_add(64);
                owner.as_ref().map_or(n, |owner| {
                    n.saturating_add(128).saturating_add(
                        4usize.saturating_mul(
                            owner
                                .conversation_id
                                .len()
                                .saturating_add(owner.incarnation.len()),
                        ),
                    )
                })
            })
    }
}

impl MessageService {
    /// Records an actual product-owned native conversation under storage policy.
    /// Same registration/conversation refreshes preserve its incarnation. A
    /// replacement token or conversation gets a distinct one; stale callers and
    /// invalid strings fail before mutation. This API is not exposed by MMP.
    pub fn bind_native_conversation(
        &mut self,
        registration: &MessageSenderRegistration,
        conversation: &str,
        incarnation: &MessageNativeIncarnation,
    ) -> Result<MessageNativeOwner> {
        validate_conversation(conversation)?;
        if self.storage_custody.is_none() || !self.sender_registration_is_current(registration) {
            return Err(MessageError::invalid_state(
                "MMP native binding requires the current storage-backed registration",
            ));
        }
        if let Some(binding) = self.native_bindings.get(registration.agent_id())
            && self.sender_registration_is_current(&binding.registration)
            && binding.incarnation == *incarnation
            && binding.owner.conversation_id == conversation
        {
            return Ok(binding.owner.clone());
        }
        // Derivation makes exact rollback reproducible without a growing
        // history map. A fresh actual registration namespace or independent
        // shell binding cannot alias the original, even at an equal MMP floor.
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"mezzanine-mmp-native-owner-v1\0");
        hash.update(registration.identity.native_namespace.as_bytes());
        hash.update(incarnation.0.as_bytes());
        hash.update((conversation.len() as u64).to_le_bytes());
        hash.update(conversation.as_bytes());
        let digest = hash.finalize();
        let mut identity = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write;
            let _ = write!(identity, "{byte:02x}");
        }
        let owner = MessageNativeOwner {
            conversation_id: conversation.into(),
            incarnation: identity,
        };
        self.native_bindings.insert(
            registration.agent_id().clone(),
            LiveNativeBinding {
                registration: registration.clone(),
                incarnation: incarnation.clone(),
                admission_available: true,
                delivery_available: true,
                owner: owner.clone(),
            },
        );
        Ok(owner)
    }

    /// Reconciles a complete actual native shell view atomically after validating
    /// all identities/duplicates. Omitted shells lose native delivery rights;
    /// temporarily suspended shells retain their owner without accepting input.
    pub fn reconcile_native_conversations(
        &mut self,
        candidates: &[MessageNativeRegistration],
    ) -> Result<()> {
        let mut ids = HashSet::new();
        if self.storage_custody.is_none() {
            return Err(MessageError::invalid_state(
                "MMP native bindings require storage custody",
            ));
        }
        for binding in candidates {
            validate_conversation(&binding.conversation_id)?;
            if (binding.delivery_available && !binding.admission_available)
                || !self.sender_registration_is_current(&binding.registration)
                || !ids.insert(binding.registration.agent_id().clone())
            {
                return Err(MessageError::invalid_args(
                    "MMP native binding registration is stale or duplicated",
                ));
            }
        }
        for binding in candidates {
            self.bind_native_conversation(
                &binding.registration,
                &binding.conversation_id,
                &binding.incarnation,
            )?;
            if let Some(live) = self
                .native_bindings
                .get_mut(binding.registration.agent_id())
            {
                live.admission_available = binding.admission_available;
                live.delivery_available = binding.delivery_available;
            }
        }
        self.native_bindings.retain(|id, _| ids.contains(id));
        Ok(())
    }

    /// Suspended native parent mail remains pending while its ephemeral child
    /// uses the pane; suspension is neither retirement nor canonical receive.
    pub(super) fn native_delivery_is_available(&self, id: &AgentId) -> bool {
        self.native_bindings.get(id).is_none_or(|binding| {
            !self.sender_registration_is_current(&binding.registration)
                || binding.delivery_available
        })
    }

    /// A qualified suspended parent can retain incoming mail; an unqualified
    /// first-observed ephemeral endpoint cannot acquire mail for a guessed parent.
    pub(super) fn native_admission_is_available(&self, id: &AgentId) -> bool {
        self.native_bindings.get(id).is_none_or(|binding| {
            !self.sender_registration_is_current(&binding.registration)
                || binding.admission_available
        })
    }

    /// Borrows nonauthorizing native attribution only for both exact registration
    /// and actual shell incarnation. A stale cached parent or transcript seed
    /// cannot describe a newly rebound shell.
    pub fn native_owner_for_registration(
        &self,
        registration: &MessageSenderRegistration,
        incarnation: &MessageNativeIncarnation,
    ) -> Option<&MessageNativeOwner> {
        self.native_bindings
            .get(registration.agent_id())
            .filter(|binding| {
                self.sender_registration_is_current(registration)
                    && self.sender_registration_is_current(&binding.registration)
                    && binding.incarnation == *incarnation
            })
            .map(|binding| &binding.owner)
    }

    /// Borrows an already qualified parent association for this exact shell.
    /// An unavailable first-observed ephemeral binding cannot promote itself
    /// into a durable parent merely because the next refresh sees its cache.
    pub fn native_parent_for_registration(
        &self,
        registration: &MessageSenderRegistration,
        incarnation: &MessageNativeIncarnation,
    ) -> Option<&MessageNativeOwner> {
        self.native_bindings
            .get(registration.agent_id())
            .filter(|binding| binding.admission_available)?;
        self.native_owner_for_registration(registration, incarnation)
    }

    /// Borrows only a still-current native owner; stale facade state grants none.
    fn current_native_owner(&self, id: &AgentId) -> Option<&MessageNativeOwner> {
        self.native_bindings
            .get(id)
            .filter(|binding| self.sender_registration_is_current(&binding.registration))
            .map(|binding| &binding.owner)
    }

    /// Captures immutable native attribution only under the original storage
    /// policy. Absence for a slot means raw/unbound, not legacy reconstructed proof.
    pub(super) fn capture_native_bindings(
        &self,
        envelope: &Envelope,
        bindings: &[(AgentId, MessageSequence)],
    ) -> Result<Option<NativeBindings>> {
        if self.storage_custody.is_none() {
            return Ok(None);
        }
        // Reserve all source metadata while owners are still borrowed, before
        // copying an audience worth of native conversation/incarnation strings.
        let bytes = bindings
            .iter()
            .map(|(id, _)| self.current_native_owner(id))
            .chain(std::iter::once(
                self.current_native_owner(&envelope.sender.agent_id),
            ))
            .fold(128usize, |n, owner| {
                let n = n.saturating_add(64);
                owner.map_or(n, |owner| {
                    n.saturating_add(128).saturating_add(
                        4usize.saturating_mul(
                            owner
                                .conversation_id
                                .len()
                                .saturating_add(owner.incarnation.len()),
                        ),
                    )
                })
            });
        self.reserve_storage_metadata(envelope, bindings, bytes)?;
        Ok(Some(NativeBindings {
            sender: self
                .current_native_owner(&envelope.sender.agent_id)
                .cloned(),
            recipients: bindings
                .iter()
                .map(|(id, _)| self.current_native_owner(id).cloned())
                .collect(),
        }))
    }

    /// Requires the same live native application incarnation for a frozen slot.
    /// Legacy resident sources retain their old routing semantics.
    pub(super) fn native_recipient_is_current(
        &self,
        row: &QueuedEnvelope,
        recipient: &AgentId,
    ) -> bool {
        let Some(native) = &row.native_bindings else {
            return true;
        };
        row.recipient_bindings
            .iter()
            .position(|(id, _)| id == recipient)
            .and_then(|i| native.recipients.get(i))
            .is_some_and(|owner| owner.as_ref() == self.current_native_owner(recipient))
    }

    /// Keeps original sender terminal rights separate from later conversations.
    pub(super) fn native_sender_is_current(&self, row: &QueuedEnvelope) -> bool {
        row.native_bindings.as_ref().is_none_or(|native| {
            native.sender.as_ref() == self.current_native_owner(&row.envelope.sender.agent_id)
        })
    }
}

/// Bounds metadata independently of authored conversation/pane labels.
fn validate_conversation(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(MessageError::invalid_args(
            "MMP native conversation must contain 1..256 non-control UTF-8 bytes",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
