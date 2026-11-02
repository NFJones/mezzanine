//! Selected original semantic source to exact lower storage ownership.
//!
//! Sender telemetry is sampled only after actor capacity selection. The immutable
//! semantic action and original turn never change; queued retired sources cannot
//! issue work. Actual root operations are bounded trusted callbacks, and positive
//! acceptance is retained before any fallible result/context publication.

use super::*;
use mez_agent::messaging::{MessageRequests, MessageService};

impl RuntimeSessionService {
    /// Selects current telemetry for an original authorized source and reserves
    /// its exact tracked/untracked lower owner. No provider or storage I/O runs.
    pub(crate) fn prepare_stored_message_action(
        &mut self,
        source: &RuntimeMessageActionSource,
        now_ms: u64,
    ) -> Result<RuntimeMessageActionAdmission> {
        self.ensure_completion_admission_open()?;
        let receipt = self.message_source(source)?;
        if self.agent_turn_is_human_paused(&receipt.turn.turn_id) {
            return Err(MezError::conflict(
                "original message source is human paused",
            ));
        }
        if !self.request_parent_anchor_is_current(&receipt.turn, &receipt.parent_anchor) {
            return Err(MezError::conflict(
                "original message native process incarnation changed",
            ));
        }
        if !self
            .message_service()
            .sender_registration_is_current(&receipt.registration)
        {
            return Err(MezError::conflict(
                "original message sender registration retired",
            ));
        }
        if receipt.phase != Phase::Queued
            || !self
                .agent_turn_ledger()
                .turn(&receipt.turn.turn_id)
                .is_some_and(|turn| {
                    matches!(
                        turn.state,
                        AgentTurnState::Running | AgentTurnState::Blocked
                    )
                })
            || self.subagent_descendant_is_fenced(&receipt.turn.agent_id)
            || self
                .agent_shell_store()
                .get(&receipt.turn.pane_id)
                .is_none_or(|shell| shell.session_id != receipt.turn.conversation_id)
        {
            return Err(MezError::conflict(
                "original message source retired before acceptance",
            ));
        }
        let turn = receipt.turn.clone();
        let action = receipt.action.clone();
        let (_, execution) = self.completion_original_execution(&turn, &receipt.owner.group)?;
        if execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| batch.actions.get(receipt.owner.ordinal))
            != Some(&action)
        {
            return Err(MezError::conflict("original message source action changed"));
        }
        let AgentActionPayload::SendMessage {
            recipient,
            scope,
            content_type,
            payload,
            correlation_id,
            expect_reply,
        } = action.payload
        else {
            return Err(MezError::invalid_args(
                "stored message source is not send_message",
            ));
        };
        let sender = self.runtime_message_sender_identity(&turn)?;
        let envelope = Envelope {
            protocol: "mmp/1",
            id: format!("{}:{}", turn.turn_id, action.id),
            message_type: "send".into(),
            time: format!("runtime:{now_ms}"),
            sender: sender.clone(),
            recipient: runtime_message_recipient(&recipient)?,
            correlation_id: correlation_id.or_else(|| Some(turn.turn_id.clone())),
            ttl_ms: None,
            content_type: runtime_maap_message_content_type(&content_type),
            payload,
            extension_fields: vec![],
        };
        let scope = runtime_message_scope(scope.as_deref())?;
        let lower = if expect_reply {
            MessageAdmission::Tracked(
                self.prepare_tracked_runtime_message(&turn, envelope, scope, now_ms)?,
            )
        } else {
            MessageAdmission::Untracked(self.message_service_mut().prepare_acceptance(
                &sender.agent_id,
                envelope,
                scope,
                now_ms,
                4096,
                None,
            )?)
        };
        let identity = Arc::new(());
        let receipt = self
            .agent
            .completion_settlements
            .messages
            .get_mut(&(source.turn_id.clone(), source.action_id.clone()))
            .ok_or_else(|| MezError::invalid_state("prepared message source disappeared"))?;
        receipt.admission_identity = Some(Arc::clone(&identity));
        receipt.prepared_lower = Some(lower.clone());
        receipt.phase = Phase::Prepared;
        Ok(RuntimeMessageActionAdmission {
            source: source.clone(),
            identity,
            lower,
        })
    }

    /// Checks the exact original source and this preparation allocation.
    fn message_action_admission(
        &self,
        admission: &RuntimeMessageActionAdmission,
    ) -> Result<&MessageCompletionReceipt> {
        let receipt = self.message_source(admission.source())?;
        if !receipt
            .admission_identity
            .as_ref()
            .is_some_and(|identity| Arc::ptr_eq(identity, &admission.identity))
        {
            return Err(MezError::conflict("original message preparation changed"));
        }
        Ok(receipt)
    }

    /// Issues only this exact source/lower pair through the selected idle root.
    /// Failed start remains safely cancellable and never marks actual issuance.
    pub(crate) fn start_stored_message_action(
        &mut self,
        admission: &RuntimeMessageActionAdmission,
        start: impl FnOnce(
            &mut MessageService,
            &MessageAcceptanceTicket,
            Option<&MessageRequests>,
        ) -> Result<()>,
    ) -> Result<()> {
        let receipt = self.message_action_admission(admission)?;
        if receipt.phase != Phase::Prepared {
            return Err(MezError::conflict(
                "message source is not prepared for issuance",
            ));
        }
        match &admission.lower {
            MessageAdmission::Tracked(ticket) => self
                .start_prepared_tracked_runtime_message(ticket, |messages, requests, ticket| {
                    start(messages, ticket, Some(requests))
                })?,
            MessageAdmission::Untracked(ticket) => start(self.message_service_mut(), ticket, None)?,
        }
        let source = admission.source();
        let key = (source.turn_id.clone(), source.action_id.clone());
        let receipt = self
            .agent
            .completion_settlements
            .messages
            .get_mut(&key)
            .ok_or_else(|| MezError::invalid_state("issued message source disappeared"))?;
        receipt.phase = Phase::Issued;
        let attempt = receipt.owner.attempt.clone();
        self.record_completion_issued_attempt(&source.turn_id, &source.action_id, &attempt)
    }

    /// Cancels only a never-issued exact lower pair. It does not remove the
    /// authorized source, permitting capacity re-selection without reapproval.
    pub(crate) fn cancel_stored_message_preparation(
        &mut self,
        admission: &RuntimeMessageActionAdmission,
    ) -> Result<()> {
        if self.message_action_admission(admission)?.phase != Phase::Prepared {
            return Err(MezError::conflict(
                "issued message source cannot be cancelled",
            ));
        }
        match &admission.lower {
            MessageAdmission::Tracked(ticket) => {
                self.cancel_prepared_tracked_runtime_message(ticket)?
            }
            MessageAdmission::Untracked(ticket) => {
                self.message_service_mut().cancel_acceptance(ticket)?
            }
        }
        let source = admission.source();
        let receipt = self
            .agent
            .completion_settlements
            .messages
            .get_mut(&(source.turn_id.clone(), source.action_id.clone()))
            .ok_or_else(|| MezError::invalid_state("cancelled message source disappeared"))?;
        receipt.phase = Phase::Queued;
        receipt.admission_identity = None;
        receipt.prepared_lower = None;
        Ok(())
    }

    /// Owns real positive acceptance before any result projection. Re-observation
    /// returns that same historical fact without repeating root publication.
    pub(crate) fn adopt_stored_message_action(
        &mut self,
        admission: &RuntimeMessageActionAdmission,
        adopt: impl FnOnce(&mut MessageService, Option<&mut MessageRequests>) -> Result<Delivery>,
    ) -> Result<Delivery> {
        let source = admission.source();
        let receipt = self.message_action_admission(admission)?;
        if let Some(Ok(delivery)) = &receipt.outcome {
            return Ok(delivery.clone());
        }
        if receipt.phase != Phase::Issued {
            return Err(MezError::conflict("message source has no issued original"));
        }
        let delivery = match &admission.lower {
            MessageAdmission::Tracked(ticket) => self
                .adopt_prepared_tracked_runtime_message(ticket, |messages, requests| {
                    adopt(messages, Some(requests))
                })?,
            MessageAdmission::Untracked(_) => adopt(self.message_service_mut(), None)?,
        };
        self.retain_message_source_outcome(source, Ok(delivery.clone()))?;
        Ok(delivery)
    }

    /// Releases only actual root-qualified noncommit and its exact lower pair,
    /// then records a rejected original result. Generic I/O cannot call this path.
    pub(crate) fn refuse_stored_message_action(
        &mut self,
        admission: &RuntimeMessageActionAdmission,
        release: impl FnOnce(&mut MessageService, Option<&mut MessageRequests>) -> Result<()>,
    ) -> Result<()> {
        if self.message_action_admission(admission)?.phase != Phase::Issued {
            return Err(MezError::conflict("message refusal has no issued original"));
        }
        match &admission.lower {
            MessageAdmission::Tracked(ticket) => self
                .release_rejected_tracked_runtime_message(ticket, |messages, requests| {
                    release(messages, Some(requests))
                })?,
            MessageAdmission::Untracked(_) => release(self.message_service_mut(), None)?,
        }
        self.retain_message_source_outcome(
            admission.source(),
            Err(MessageError::invalid_state(
                "message storage original did not commit",
            )),
        )
    }

    /// Retains a deterministic pre-issuance refusal without reserving a sequence.
    pub(crate) fn reject_queued_stored_message_action(
        &mut self,
        source: &RuntimeMessageActionSource,
        error: MessageError,
    ) -> Result<()> {
        if self.message_source(source)?.phase != Phase::Queued {
            return Err(MezError::conflict(
                "issued message source is not unaccepted",
            ));
        }
        self.retain_message_source_outcome(source, Err(error))
    }

    /// Installs original outcome in the same private source before later errors.
    fn retain_message_source_outcome(
        &mut self,
        source: &RuntimeMessageActionSource,
        outcome: std::result::Result<Delivery, MessageError>,
    ) -> Result<()> {
        self.message_source(source)?;
        let key = (source.turn_id.clone(), source.action_id.clone());
        let receipt = self
            .agent
            .completion_settlements
            .messages
            .get_mut(&key)
            .ok_or_else(|| MezError::invalid_state("message outcome source unavailable"))?;
        receipt.outcome = Some(outcome);
        receipt.prepared_lower = None;
        receipt.phase = Phase::Outcome;
        if let Some(group) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&source.turn_id)
        {
            group
                .observed
                .insert((source.action_id.clone(), receipt.owner.attempt.clone()));
            group.source_changed();
        }
        Ok(())
    }
}

impl RuntimeSessionService {
    /// Validates original issued source/native metadata before bounded root retry.
    /// Retired senders remain valid historical owners of already-issued work.
    pub(crate) fn retry_stored_message_action(
        &mut self,
        admission: &RuntimeMessageActionAdmission,
        retry: impl FnOnce(&mut MessageService) -> Result<bool>,
    ) -> Result<bool> {
        if self.message_action_admission(admission)?.phase != Phase::Issued {
            return Err(MezError::conflict("message retry has no issued original"));
        }
        if let MessageAdmission::Tracked(ticket) = &admission.lower {
            self.validate_prepared_tracked_runtime_message(ticket)?;
        }
        retry(self.message_service_mut())
    }
}
