//! One bounded, unaccepted runtime bridge source awaiting the existing root.
//!
//! New child registration may still need its native-continuity journal. Retain
//! semantics, not a second mailbox or a successful receipt, until actor-selected
//! lower admission freezes the audience. The lower ledger still permits only
//! one unpublished acceptance. Issued custody belongs to the ordinary root.

use super::*;
use mez_agent::messaging::{
    Envelope, MessageAcceptanceTicket, MessageNativeIncarnation, MessageSenderRegistration,
    Recipient,
};

/// A conservative smaller limit for optional runtime bridge reports. The shared
/// native semantic-source pool additionally bounds total retained source text.
const MAX_BRIDGE_SOURCE_BYTES: usize = 512 * 1024;

pub(in crate::runtime::agent) struct BridgeSource {
    envelope: Envelope,
    sender: MessageSenderRegistration,
    recipient: MessageSenderRegistration,
    sender_pane: String,
    sender_conversation: String,
    recipient_pane: Option<String>,
    recipient_conversation: Option<String>,
    sender_incarnation: MessageNativeIncarnation,
    recipient_incarnation: Option<MessageNativeIncarnation>,
    sequence: u64,
    pub(in crate::runtime::agent) text_bytes: usize,
}

/// Measures owned strings/vectors before retaining the code-owned envelope.
/// Opaque IDs are fresh clones of bounded registered identities; the allowance
/// covers their copies, registration handles and conversation attribution too.
fn source_bytes(envelope: &Envelope) -> usize {
    let sender = &envelope.sender;
    let recipient = match &envelope.recipient {
        Recipient::Agent(id) => id.as_str().len(),
        _ => usize::MAX,
    };
    let strings = [
        envelope.id.capacity(),
        envelope.message_type.capacity(),
        envelope.time.capacity(),
        envelope.content_type.capacity(),
        envelope.payload.capacity(),
        envelope.correlation_id.as_ref().map_or(0, String::capacity),
        sender.role.as_ref().map_or(0, String::capacity),
        sender.objective.as_ref().map_or(0, String::capacity),
    ]
    .into_iter()
    .chain(sender.capabilities.iter().map(String::capacity))
    .chain(
        envelope
            .extension_fields
            .iter()
            .flat_map(|(k, v)| [k.capacity(), v.capacity()]),
    )
    .fold(0usize, usize::saturating_add);
    4096usize
        .saturating_add(strings.saturating_mul(2))
        .saturating_add(
            sender
                .capabilities
                .capacity()
                .saturating_mul(std::mem::size_of::<String>()),
        )
        .saturating_add(
            envelope
                .extension_fields
                .capacity()
                .saturating_mul(std::mem::size_of::<(String, String)>()),
        )
        .saturating_add(recipient.saturating_mul(4))
        .saturating_add(sender.agent_id.as_str().len().saturating_mul(4))
        .saturating_add(
            sender
                .pane_id
                .as_ref()
                .map_or(0, |id| id.as_str().len())
                .saturating_mul(4),
        )
        .saturating_add(
            sender
                .window_id
                .as_ref()
                .map_or(0, |id| id.as_str().len())
                .saturating_mul(4),
        )
}

impl RuntimeSessionService {
    /// True means actual same-runtime acceptance; false means bounded unaccepted
    /// source retention only. No caller may turn false into a delivery receipt.
    /// Only code-owned runtime producers call this; envelope extension fields do
    /// not grant raw clients or model-authored messages this ingress authority.
    pub(crate) fn accept_runtime_bridge_message(
        &mut self,
        turn: &AgentTurnRecord,
        envelope: Envelope,
        now_ms: u64,
    ) -> Result<bool> {
        if !self.stored_message_actions_enabled() {
            let sender = envelope.sender.agent_id.clone();
            self.message_service_mut().accept_at_with_scope(
                &sender,
                envelope,
                mez_agent::messaging::MessageScope::Session,
                now_ms,
            )?;
            return Ok(true);
        }
        if self.agent.completion_settlements.message_storage_stopping
            || self.message_restore_pending()
            || self.agent.completion_settlements.bridge_source.is_some()
        {
            return Err(MezError::conflict(
                "runtime bridge source unavailable before acceptance",
            ));
        }
        if !crate::runtime::control::runtime_owned_bridge_message(&envelope)
            || envelope.sender.agent_id.as_str() != turn.agent_id
            || envelope.ttl_ms.is_some()
        {
            return Err(MezError::invalid_args(
                "runtime bridge source attribution invalid",
            ));
        }
        let Recipient::Agent(recipient_id) = &envelope.recipient else {
            return Err(MezError::invalid_args(
                "runtime bridge requires an exact recipient",
            ));
        };
        let recipient_pane = recipient_id
            .as_str()
            .strip_prefix("agent-")
            .map(str::to_owned);
        let recipient_conversation = recipient_pane
            .as_deref()
            .and_then(|pane| self.agent_shell_store().get(pane))
            .map(|shell| shell.session_id.clone());
        let text_bytes = source_bytes(&envelope)
            .saturating_add(turn.pane_id.len().saturating_mul(4))
            .saturating_add(turn.conversation_id.len().saturating_mul(4))
            .saturating_add(
                recipient_conversation
                    .as_ref()
                    .map_or(0, String::len)
                    .saturating_mul(4),
            );
        if text_bytes > MAX_BRIDGE_SOURCE_BYTES
            || !self.message_source_capacity_available(text_bytes)
        {
            return Err(MezError::conflict(
                "runtime bridge source capacity; message not accepted",
            ));
        }
        let sender = self
            .message_service()
            .capture_sender_registration(&envelope.sender.agent_id)
            .ok_or_else(|| MezError::conflict("runtime bridge sender registration unavailable"))?;
        let recipient = self
            .message_service()
            .capture_sender_registration(recipient_id)
            .ok_or_else(|| {
                MezError::conflict("runtime bridge recipient registration unavailable")
            })?;
        let sender_incarnation = self
            .agent_shell_store()
            .get(&turn.pane_id)
            .filter(|shell| shell.session_id == turn.conversation_id)
            .ok_or_else(|| MezError::conflict("runtime bridge sender conversation unavailable"))?
            .native_message_incarnation
            .clone();
        let recipient_incarnation = recipient_pane
            .as_deref()
            .and_then(|pane| self.agent_shell_store().get(pane))
            .map(|shell| shell.native_message_incarnation.clone());
        let sequence = self.agent.completion_settlements.next_message_source;
        self.agent.completion_settlements.next_message_source = sequence
            .checked_add(1)
            .ok_or_else(|| MezError::conflict("message source generation exhausted"))?;
        self.agent.completion_settlements.bridge_source = Some(Box::new(BridgeSource {
            envelope,
            sender,
            recipient,
            sender_pane: turn.pane_id.clone(),
            sender_conversation: turn.conversation_id.clone(),
            recipient_pane,
            recipient_conversation,
            sender_incarnation,
            recipient_incarnation,
            sequence,
            text_bytes,
        }));
        Ok(false)
    }

    pub(crate) fn has_pending_stored_bridge_message(&self) -> bool {
        self.agent.completion_settlements.bridge_source.is_some()
    }

    /// Uses the existing FIFO source allocator; bridge traffic cannot leapfrog
    /// an earlier eligible native action, nor can later actions starve a bridge.
    pub(crate) fn stored_bridge_message_has_priority(&self) -> bool {
        self.agent
            .completion_settlements
            .bridge_source
            .as_ref()
            .is_some_and(|bridge| {
                self.pending_stored_message_actions()
                    .first()
                    .and_then(|source| self.message_source(source).ok())
                    .is_none_or(|action| action.sequence > bridge.sequence)
            })
    }

    /// Selected only at idle root capacity, after native continuity is settled.
    /// The source moves directly into the lower reservation; failed pre-admission
    /// is explicitly unaccepted and cannot be replayed as a task or model action.
    pub(crate) fn prepare_stored_bridge_message(
        &mut self,
        now_ms: u64,
    ) -> Result<Option<MessageAcceptanceTicket>> {
        let Some(source) = self.agent.completion_settlements.bridge_source.take() else {
            return Ok(None);
        };
        let current_recipient = source
            .recipient_pane
            .as_deref()
            .and_then(|pane| self.agent_shell_store().get(pane))
            .map(|shell| &shell.session_id);
        if self.agent.completion_settlements.message_storage_stopping
            || self.message_restore_pending()
            || !self
                .message_service()
                .sender_registration_is_current(&source.sender)
            || !self
                .message_service()
                .sender_registration_is_current(&source.recipient)
            || self
                .agent_shell_store()
                .get(&source.sender_pane)
                .is_none_or(|shell| {
                    shell.session_id != source.sender_conversation
                        || shell.native_message_incarnation != source.sender_incarnation
                })
            || current_recipient != source.recipient_conversation.as_ref()
            || source
                .recipient_pane
                .as_deref()
                .and_then(|pane| self.agent_shell_store().get(pane))
                .map(|shell| &shell.native_message_incarnation)
                != source.recipient_incarnation.as_ref()
            || self.subagent_descendant_is_fenced(source.sender.agent_id().as_str())
        {
            return Err(MezError::conflict(
                "runtime bridge original source retired before acceptance",
            ));
        }
        let mut envelope = source.envelope;
        envelope.sender = self
            .message_service()
            .registered_identity(source.sender.agent_id())
            .cloned()
            .ok_or_else(|| MezError::conflict("runtime bridge original sender unavailable"))?;
        self.message_service_mut()
            .prepare_acceptance(
                source.sender.agent_id(),
                envelope,
                mez_agent::messaging::MessageScope::Session,
                now_ms,
                4096,
                None,
            )
            .map(Some)
            .map_err(Into::into)
    }
}
