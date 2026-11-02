//! Original authorized native message actions across storage capacity and I/O.
//!
//! Semantic sources belong to existing completion groups before lower admission.
//! They retain the original permission/approval disposition and exact action, not
//! a stale sender envelope. Selected admission refreshes telemetry only while the
//! original turn still owns its conversation. Opaque allocation identity fences
//! stale callbacks; original failed backing receives late committed results.
//! No source or capability is serialized, and this is not a restart permission.

use super::*;
use crate::runtime::RuntimePreparedTrackedMessage;
use mez_agent::messaging::{Delivery, MessageAcceptanceTicket, MessageError, MessageScope};
use std::sync::Arc;

mod admission;
mod continuation;
mod projection;

/// Same-runtime source identity; tuple labels alone never authorize completion.
#[derive(Clone)]
pub(crate) struct RuntimeMessageActionSource {
    identity: Arc<()>,
    turn_id: String,
    action_id: String,
}
impl fmt::Debug for RuntimeMessageActionSource {
    /// Reveals only public attribution, never semantic payload or permissions.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeMessageActionSource")
            .field("turn_id", &self.turn_id)
            .field("action_id", &self.action_id)
            .finish_non_exhaustive()
    }
}

/// Exact lower owner selected for the original action, without another payload.
pub(crate) struct RuntimeMessageActionAdmission {
    /// Exact original semantic owner.
    source: RuntimeMessageActionSource,
    /// This preparation allocation, distinct from cancelled earlier selections.
    identity: Arc<()>,
    lower: MessageAdmission,
}
/// Private lower variant cannot be rebound to another public source/ticket pair.
#[derive(Clone)]
enum MessageAdmission {
    Tracked(RuntimePreparedTrackedMessage),
    Untracked(MessageAcceptanceTicket),
}
impl RuntimeMessageActionAdmission {
    /// Original source allocation independent of the lower admission variant.
    pub(crate) fn source(&self) -> &RuntimeMessageActionSource {
        &self.source
    }
}

/// Source stages advance only through actual original acceptance and projection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Authorized semantics only; no lower reservation exists.
    Queued,
    /// Exact never-issued lower owner remains cancellable.
    Prepared,
    Issued,
    Outcome,
    Canonical,
    Context,
    Published,
}

/// Canonical permission remains in original backing; this records its disposition.
#[derive(Clone, PartialEq, Eq)]
enum Authorization {
    Policy,
    Approval(String),
}

/// Additional source text is bounded separately from the charged lower ledger.
const MAX_SOURCES: usize = 64;
const MAX_SOURCE_TEXT: usize = 8 * 1024 * 1024;

/// One original action under its existing bounded completion-group lifetime.
pub(super) struct MessageCompletionReceipt {
    source: RuntimeMessageActionSource,
    sequence: u64,
    group_identity: Arc<()>,
    pub(super) owner: ExternalCompletionOwner,
    turn: AgentTurnRecord,
    registration: mez_agent::messaging::MessageSenderRegistration,
    parent_anchor: crate::runtime::agent::request_waits::ParentAnchor,
    action: AgentAction,
    result_index: usize,
    authorization: Authorization,
    /// Exact preparation revision remains as inert duplicate provenance.
    admission_identity: Option<Arc<()>>,
    /// Payload-free lower cancellation ownership until an outcome transfers.
    prepared_lower: Option<MessageAdmission>,
    text_bytes: usize,
    phase: Phase,
    outcome: Option<std::result::Result<Delivery, MessageError>>,
    result: Option<ActionResult>,
    failure: Option<MezError>,
}
impl fmt::Debug for MessageCompletionReceipt {
    /// Generic diagnostics reveal ownership and stages without source text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessageCompletionReceipt")
            .field("source", &self.source)
            .field("phase", &self.phase)
            .field("failed", &self.failure.is_some())
            .finish()
    }
}

impl RuntimeSessionService {
    /// Enables source deferral only for an internally registered actor root.
    pub(crate) fn enable_stored_message_actions(&mut self) -> Result<()> {
        if self.has_pending_stored_message_actions() {
            return Err(MezError::conflict(
                "original message source owner is still pending",
            ));
        }
        self.agent.completion_settlements.message_storage_enabled = true;
        self.agent.completion_settlements.message_storage_stopping = false;
        Ok(())
    }
    /// Reports the internal source route; ordinary product construction is false.
    pub(in crate::runtime::agent) fn stored_message_actions_enabled(&self) -> bool {
        self.agent.completion_settlements.message_storage_enabled
    }
    /// Looks up only the actual source allocation, not matching public IDs.
    fn message_source(
        &self,
        source: &RuntimeMessageActionSource,
    ) -> Result<&MessageCompletionReceipt> {
        self.agent
            .completion_settlements
            .messages
            .get(&(source.turn_id.clone(), source.action_id.clone()))
            .filter(|r| {
                Arc::ptr_eq(&r.source.identity, &source.identity)
                    && self
                        .agent
                        .completion_settlements
                        .groups
                        .get(&r.owner.turn_id)
                        .is_some_and(|group| Arc::ptr_eq(&group.identity, &r.group_identity))
            })
            .map(Box::as_ref)
            .ok_or_else(|| MezError::conflict("original message source unavailable"))
    }
    /// Captures a previously authorized action before any storage-capacity wait.
    /// Approval callers have already validated the original approval digest; this
    /// entry additionally checks its exact retained turn/action reference. Capacity
    /// errors issue no lower reservation and never consume a replacement source.
    pub(in crate::runtime::agent) fn queue_stored_message_action(
        &mut self,
        turn: &AgentTurnRecord,
        result_index: usize,
        action: &AgentAction,
        approval_id: Option<&str>,
    ) -> Result<ActionResult> {
        self.ensure_completion_admission_open()?;
        if self.agent.completion_settlements.message_storage_stopping {
            return Err(MezError::conflict("message source storage is stopping"));
        }
        let execution = self
            .agent_turn_executions()
            .get(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("message source execution unavailable"))?;
        let group_id =
            super::super::provider_execution::provider_log_execution_group_id(execution)?;
        let ordinal = execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| {
                batch
                    .actions
                    .iter()
                    .position(|candidate| candidate == action)
            })
            .ok_or_else(|| MezError::conflict("message source action changed"))?;
        let slot = execution
            .action_results
            .get(result_index)
            .filter(|slot| {
                slot.action_id == action.id
                    && slot.turn_id == turn.turn_id
                    && slot.agent_id == turn.agent_id
                    && slot.action_type == "send_message"
            })
            .ok_or_else(|| MezError::conflict("message source result slot changed"))?;
        let authorization = if let Some(id) = approval_id {
            if slot.status != ActionStatus::Blocked
                || !self
                    .agent
                    .blocked_agent_approval_refs
                    .get(id)
                    .is_some_and(|owner| {
                        owner.turn_id == turn.turn_id && owner.action_id == action.id
                    })
            {
                return Err(MezError::forbidden("message source approval owner changed"));
            }
            Authorization::Approval(id.to_owned())
        } else {
            if slot.status != ActionStatus::Running {
                return Err(MezError::forbidden(
                    "message source is not policy authorized",
                ));
            }
            Authorization::Policy
        };
        let mut running = ActionResult::running(
            turn,
            action,
            vec!["message waiting for delivery storage".into()],
            Some(serde_json::json!({"delivery_status":"pending","accepted":false}).to_string()),
        );
        running.permission_evaluation = slot.permission_evaluation.clone();
        let key = (turn.turn_id.clone(), action.id.clone());
        if let Some(existing) = self.agent.completion_settlements.messages.get(&key) {
            if existing.action != *action
                || existing.owner.group != group_id
                || existing.authorization != authorization
            {
                return Err(MezError::conflict(
                    "message source original authorization changed",
                ));
            }
            return Ok(running);
        }
        let AgentActionPayload::SendMessage {
            recipient,
            scope,
            content_type,
            payload,
            correlation_id,
            expect_reply,
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "message source requires send_message",
            ));
        };
        let text_bytes = [
            recipient.len(),
            scope.as_ref().map_or(0, String::len),
            content_type.len(),
            payload.len(),
            correlation_id.as_ref().map_or(0, String::len),
            action.id.len(),
            turn.turn_id.len(),
            turn.agent_id.len(),
            turn.pane_id.len(),
            turn.conversation_id.len(),
            turn.policy_profile.len(),
            turn.model_profile.len(),
            turn.parent_turn_id.as_ref().map_or(0, String::len),
            turn.cooperation_mode.as_ref().map_or(0, String::len),
            approval_id.map_or(0, str::len),
            1024,
        ]
        .into_iter()
        .try_fold(0usize, usize::checked_add)
        .ok_or_else(|| MezError::conflict("message source text budget overflow"))?;
        let receipts = &self.agent.completion_settlements.messages;
        let used = receipts.values().map(|r| r.text_bytes).sum::<usize>();
        if receipts.len() >= MAX_SOURCES || text_bytes > MAX_SOURCE_TEXT.saturating_sub(used) {
            let code = if *expect_reply {
                "request_tracking_capacity"
            } else {
                "capacity"
            };
            let mut failed = ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                code,
                "message source capacity unavailable",
            )?;
            failed.structured_content_json=Some(serde_json::json!({"recipient":recipient,
                "scope":scope.as_deref().unwrap_or("project"),"message_id":null,"delivery_status":"rejected",
                "accepted":false,"delivery_applied":false,"protocol_error":{"code":code,"message":"message source capacity unavailable"}}).to_string());
            return Ok(failed);
        }
        let sender_id = AgentId::opaque(turn.agent_id.clone())
            .ok_or_else(|| MezError::invalid_state("message source sender ID invalid"))?;
        if sender_id.as_str() == format!("agent-{}", turn.pane_id) {
            // Complete the trusted native pane binding before freezing its
            // registration revision. Ordinary objective publication uses the
            // same reconciliation and must not enrich authority underneath an
            // already-authorized source waiting for storage capacity.
            self.ensure_runtime_message_identity(
                &turn.agent_id,
                None,
                "agent",
                &["agent-harness"],
                current_unix_millis(),
            )?;
        } else if self
            .message_service()
            .capture_sender_registration(&sender_id)
            .is_none()
        {
            // Establish the first native registration before deferral. Existing
            // registrations keep their captured authority; only telemetry is
            // refreshed later at actual selected admission.
            self.runtime_message_sender_identity(turn)?;
        }
        let registration = self
            .message_service()
            .capture_sender_registration(&sender_id)
            .ok_or_else(|| MezError::conflict("message source sender registration unavailable"))?;
        self.reserve_completion_group(&turn.turn_id, &group_id)?;
        let sequence = self.agent.completion_settlements.next_message_source;
        self.agent.completion_settlements.next_message_source = sequence
            .checked_add(1)
            .ok_or_else(|| MezError::conflict("message source generation exhausted"))?;
        let group_identity = Arc::clone(
            &self
                .agent
                .completion_settlements
                .groups
                .get(&turn.turn_id)
                .ok_or_else(|| {
                    MezError::invalid_state("message source original group unavailable")
                })?
                .identity,
        );
        let source = RuntimeMessageActionSource {
            identity: Arc::new(()),
            turn_id: turn.turn_id.clone(),
            action_id: action.id.clone(),
        };
        let owner = ExternalCompletionOwner {
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            conversation_id: turn.conversation_id.clone(),
            group: group_id,
            action_id: action.id.clone(),
            action_type: "send_message".into(),
            ordinal,
            attempt: format!("message-source:{}", action.id),
        };
        let parent_anchor = self.capture_request_parent_anchor(turn);
        self.agent.completion_settlements.messages.insert(
            key,
            Box::new(MessageCompletionReceipt {
                source,
                sequence,
                group_identity,
                owner,
                turn: turn.clone(),
                registration,
                parent_anchor,
                action: action.clone(),
                result_index,
                authorization,
                admission_identity: None,
                prepared_lower: None,
                text_bytes,
                phase: Phase::Queued,
                outcome: None,
                result: None,
                failure: None,
            }),
        );
        Ok(running)
    }
    /// Lists only original queued sources; actor capacity selection precedes preparation.
    pub(crate) fn pending_stored_message_actions(&self) -> Vec<RuntimeMessageActionSource> {
        let mut sources = self
            .agent
            .completion_settlements
            .messages
            .values()
            .filter(|r| {
                r.phase == Phase::Queued
                    && !self.agent_turn_is_human_paused(&r.owner.turn_id)
                    && !self
                        .agent
                        .pending_agent_provider_persistence
                        .contains(&r.owner.turn_id)
            })
            .collect::<Vec<_>>();
        sources.sort_by_key(|r| r.sequence);
        sources.into_iter().map(|r| r.source.clone()).collect()
    }

    /// Observes an already retained actual root outcome, never publication success.
    /// The runtime source remains its owner across required projection failure.
    pub(crate) fn stored_message_action_outcome_is_owned(
        &self,
        source: &RuntimeMessageActionSource,
    ) -> bool {
        self.message_source(source)
            .is_ok_and(|r| r.outcome.is_some())
    }

    /// Refuses snapshot replacement while original semantic/result custody remains.
    pub(crate) fn has_pending_stored_message_actions(&self) -> bool {
        !self
            .agent
            .completion_settlements
            .message_continuations
            .is_empty()
            || self
                .agent
                .completion_settlements
                .messages
                .values()
                .any(|r| r.phase != Phase::Published || r.failure.is_some())
    }

    /// Cancels only unissued semantic sources before forced retirement or shutdown.
    /// Issued originals keep their actual lower/root and late result ownership.
    pub(in crate::runtime) fn cancel_queued_stored_message_actions(
        &mut self,
        turn_id: Option<&str>,
    ) -> Result<()> {
        self.agent
            .completion_settlements
            .message_continuations
            .retain(|id, _| turn_id.is_some_and(|owner| owner != id));
        let sources = self
            .agent
            .completion_settlements
            .messages
            .values()
            .filter(|r| {
                matches!(r.phase, Phase::Queued | Phase::Prepared)
                    && turn_id.is_none_or(|id| r.owner.turn_id == id)
            })
            .map(|r| r.source.clone())
            .collect::<Vec<_>>();
        for source in sources {
            let receipt = self.message_source(&source)?;
            if receipt.phase == Phase::Prepared {
                let admission = RuntimeMessageActionAdmission {
                    source: source.clone(),
                    identity: Arc::clone(receipt.admission_identity.as_ref().ok_or_else(|| {
                        MezError::invalid_state("prepared message identity unavailable")
                    })?),
                    lower: receipt.prepared_lower.clone().ok_or_else(|| {
                        MezError::invalid_state("prepared message cancellation owner unavailable")
                    })?,
                };
                self.cancel_stored_message_preparation(&admission)?;
            }
            self.reject_queued_stored_message_action(
                &source,
                MessageError::invalid_state("original message cancelled before acceptance"),
            )?;
            self.project_stored_message_action(&source)?;
        }
        Ok(())
    }

    /// Stops new source admission and settles only never-issued originals.
    pub(crate) fn stop_stored_message_actions(&mut self) -> Result<()> {
        self.agent.completion_settlements.message_storage_stopping = true;
        self.cancel_queued_stored_message_actions(None)
    }

    /// Keeps original group backing while an action still owns storage/result work.
    pub(in crate::runtime::agent) fn message_completion_pending(&self, turn_id: &str) -> bool {
        self.agent
            .completion_settlements
            .messages
            .values()
            .any(|r| {
                r.owner.turn_id == turn_id && (r.phase != Phase::Published || r.failure.is_some())
            })
    }
    /// Distinguishes actual issuance from a safely cancellable queued original.
    pub(in crate::runtime::agent) fn message_completion_issued_pending(
        &self,
        turn_id: &str,
    ) -> bool {
        self.agent
            .completion_settlements
            .messages
            .values()
            .any(|r| {
                r.owner.turn_id == turn_id
                    && !matches!(r.phase, Phase::Queued | Phase::Prepared | Phase::Published)
            })
    }
}
