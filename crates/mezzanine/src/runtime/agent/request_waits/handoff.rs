//! Canonical request-outcome staging and exact continuation consumption.
//!
//! Parked waits retain normal fair admission and sibling gates. Already active
//! owners receive only a protected reference; no scheduling transition or old
//! call-result rewrite occurs. A real later provider response may consume that
//! reference only after its claim included it and stale-context checks pass.
//! Source errors retain custody and defer work rather than fabricating progress.

use super::*;

impl RuntimeSessionService {
    /// Settles a selected edge only after canonical evidence and action-result
    /// handoff exist. QueueFull retains the same edge/source for the next sweep.
    /// Unrelated mail uses ordinary wake without consuming this request.
    pub(in crate::runtime::agent) fn resume_targeted_request_wait(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<bool> {
        let Some(edge) = self.agent.request_waits.edges.get(&turn.turn_id).cloned() else {
            return Ok(false);
        };
        let parked = turn.state == AgentTurnState::Blocked;
        if !matches!(
            turn.state,
            AgentTurnState::Blocked | AgentTurnState::Queued | AgentTurnState::Running
        ) || self.agent_turn_is_human_paused(&turn.turn_id)
            || edge.started != turn.started_at_unix_seconds
            || self
                .agent_shell_store()
                .get(&turn.pane_id)
                .is_none_or(|shell| shell.session_id != turn.conversation_id)
        {
            return Ok(false);
        }
        let Some(receipt) = self.agent.request_waits.requests.receipt(edge.generation) else {
            return Ok(false);
        };
        if receipt.origin().agent_id.as_str() != turn.agent_id
            || receipt.origin().conversation_id != turn.conversation_id
        {
            return Ok(false);
        }
        if !self
            .control
            .message_service()
            .acceptance_sender_is_current(receipt.proof())
            || self
                .agent
                .request_waits
                .anchors
                .get(&edge.generation)
                .is_none_or(|anchor| !self.request_parent_anchor_is_current(turn, anchor))
        {
            return Ok(false);
        }
        let outcome = self
            .agent
            .request_waits
            .recipients
            .get(&edge.generation)
            .and_then(|slots| slots.get(edge.recipient))
            .and_then(|slot| slot.outcome.clone());
        let Some(outcome) = outcome else {
            return Ok(false);
        };
        let selected_slot = &self.agent.request_waits.recipients[&edge.generation][edge.recipient];
        let terminal_consumed = selected_slot.terminal_consumed;
        let result = self
            .agent_turn_executions()
            .get(&turn.turn_id)
            .and_then(|execution| {
                if execution.action_results.iter().any(|result| {
                    result.status != ActionStatus::Succeeded
                        && !(result.action_type == "wait" && result.status == ActionStatus::Running)
                }) {
                    return None;
                }
                execution.action_results.iter().find(|result| {
                    result.action_type == "wait" && result.status == ActionStatus::Running
                })
            })
            .cloned();
        if parked && result.is_none() {
            return Ok(false);
        }
        let mut failure_source: Option<(String, [u8; 32])> = None;
        let (reason, kind, source_identity, content, progress) = match outcome {
            Outcome::Reply(sequence) => ("reply", "reply", sequence.to_string(),
                format!("Authenticated matching response committed at message sequence {sequence}. Runtime proves occurrence, not substantive answer correctness."), true),
            Outcome::Terminal { content } => ("peer_terminal", "terminal", String::new(), content, !terminal_consumed),
            Outcome::FailureReference { label, child } => {
                let source = self.agent.owned_child_failures.get(&child);
                if source.is_some_and(|receipt| receipt.committed_turn_id.is_none()) { return Ok(false); }
                if let Some(receipt) = source.filter(|receipt| receipt.committed_turn_id.as_deref() == Some(turn.turn_id.as_str())) {
                    failure_source = Some((receipt.label.clone(), Sha256::digest(receipt.content.as_bytes()).into()));
                }
                ("peer_terminal", "failure-reference", child,
                    format!("Exact captured owned-task failure reference: {label}. Its existing native failure receipt owns diagnostic delivery. This conveys terminal occurrence, not new output, a verdict, approval or permission to replay work."),
                    !terminal_consumed && source.is_some_and(|receipt| !receipt.wake_admitted))
            },
            Outcome::Unavailable => ("peer_unavailable", "unavailable", String::new(),
                "The selected original dependency is unavailable or intentionally retired. Reassess the missing answer and choose authorized independent work. No private peer task or output is disclosed; no request or task is automatically replayed.".into(), false),
        };
        let content = format!(
            "[local selected request outcome state={reason} dependency_generation={}]\n{content}",
            edge.generation
        );
        let label = self.request_handoff_label(turn, &edge, kind, &source_identity)?;
        let handoff_digest = canonical_handoff_digest(&label, &content);
        let retained = self.agent.request_waits.recipients[&edge.generation][edge.recipient]
            .handoff_source
            .clone();
        let Some(context) = self.agent_turn_contexts_mut().get_mut(&turn.turn_id) else {
            return Ok(false);
        };
        let failure_event = match failure_source {
            Some((source_label, expected)) => {
                let Some(source) = context.chronology().iter().find(|event| {
                    event.block().source == mez_agent::ContextSourceKind::PeerMessage
                        && event.block().label == source_label
                }) else {
                    return Ok(false);
                };
                let actual: [u8; 32] = Sha256::digest(source.block().content.as_bytes()).into();
                if actual != expected {
                    return Err(super::super::MezError::conflict(
                        "native failure source identity changed",
                    ));
                }
                Some(source.sequence())
            }
            None => None,
        };
        let event = match retained
            .as_ref()
            .filter(|(owner, _)| owner == &turn.turn_id)
        {
            Some((_, sequence)) => {
                let valid = context.chronology().iter().any(|event| {
                    event.sequence() == *sequence
                        && event.block().source == mez_agent::ContextSourceKind::PeerMessage
                        && event.block().label == label
                        && event.block().content == content
                });
                if !valid {
                    return Err(super::super::MezError::conflict(
                        "request handoff reference identity changed",
                    ));
                }
                *sequence
            }
            None => {
                if context
                    .chronology()
                    .iter()
                    .any(|event| event.block().label == label)
                {
                    return Err(super::super::MezError::conflict(
                        "request handoff label has no live occurrence owner",
                    ));
                }
                context.append_peer_message_event(label, content)?
            }
        };
        if let Some(source) = failure_event {
            context.retain_pending_peer_message(source)?;
        }
        context.retain_pending_peer_message(event)?;
        let mut previous_failure = None;
        if let Some(slot) = self
            .agent
            .request_waits
            .recipients
            .get_mut(&edge.generation)
            .and_then(|slots| slots.get_mut(edge.recipient))
        {
            slot.handoff_source = Some((turn.turn_id.clone(), event));
            slot.handoff_digest = Some(handoff_digest);
            if let Some(source) = failure_event {
                previous_failure = slot.peer_source.replace((turn.turn_id.clone(), source));
            }
        }
        if let Some((owner, sequence)) = previous_failure {
            self.release_request_pin_if_unheld(&owner, sequence);
        }
        if let Some((owner, sequence)) = retained.filter(|(owner, _)| owner != &turn.turn_id) {
            self.release_request_pin_if_unheld(&owner, sequence);
        }
        if !parked {
            // Ordinary mail already resumed this owner. Context staging grants
            // no new scheduler slot, sibling readiness or provider continuation.
            return Ok(false);
        }
        let Some(mut result) = result else {
            return Ok(false);
        };
        result.status = ActionStatus::Succeeded;
        result.content = vec![mez_agent::ActionContentBlock::text(
            "selected request outcome received; reassess coordination",
        )];
        result.structured_content_json = Some(
            serde_json::json!({"state":reason,"dependency_generation":edge.generation,
            "dependency_satisfied":true,"progress_witness":progress})
            .to_string(),
        );
        self.append_action_result_context_if_absent(&turn.turn_id, &result)?;
        match self.agent.agent_scheduler.requeue_waiting(&turn.turn_id) {
            Ok(_) => {}
            Err(error) if error.kind() == mez_agent::SchedulerErrorKind::QueueFull => {
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        }
        if let Some(execution) = self.agent_turn_executions_mut().get_mut(&turn.turn_id) {
            if let Some(wait) = execution
                .action_results
                .iter_mut()
                .find(|wait| wait.action_id == result.action_id)
            {
                *wait = result;
            }
            execution.final_turn = false;
            execution.terminal_state = super::super::runtime_agent_turn_state_from_action_results(
                &execution.action_results,
                false,
            );
        }
        self.consume_request_wait_receipt(turn, &edge, progress);
        self.agent
            .agent_turn_peer_wait_wake_pending
            .remove(&turn.turn_id);
        self.start_ready_agent_turns()?;
        Ok(true)
    }

    /// Names an exact accepted occurrence, frozen recipient, captured task and
    /// outcome within this runtime and waiting task. Historical labels cannot
    /// recreate ownership after generation reset or a recipient pivot.
    fn request_handoff_label(
        &self,
        turn: &AgentTurnRecord,
        edge: &Edge,
        kind: &str,
        source: &str,
    ) -> Result<String> {
        let receipt = self
            .agent
            .request_waits
            .requests
            .receipt(edge.generation)
            .ok_or_else(|| {
                super::super::MezError::invalid_state("request handoff receipt is missing")
            })?;
        let recipient = receipt
            .proof()
            .recipients()
            .nth(edge.recipient)
            .ok_or_else(|| {
                super::super::MezError::invalid_state("request handoff recipient is missing")
            })?;
        let slot = self
            .agent
            .request_waits
            .recipients
            .get(&edge.generation)
            .and_then(|slots| slots.get(edge.recipient))
            .ok_or_else(|| {
                super::super::MezError::invalid_state("request handoff reservation is missing")
            })?;
        let mut digest = Sha256::new();
        digest.update(self.agent.request_waits.label_epoch);
        digest.update(receipt.proof().sequence().to_le_bytes());
        digest.update(turn.started_at_unix_seconds.to_le_bytes());
        for field in [
            receipt.origin().agent_id.as_str(),
            receipt.origin().conversation_id.as_str(),
            receipt.delivery().message_id.as_str(),
            recipient.as_str(),
            turn.turn_id.as_str(),
            kind,
            source,
        ] {
            digest.update((field.len() as u64).to_le_bytes());
            digest.update(field.as_bytes());
        }
        if let Some(task) = &slot.task {
            digest.update(task.started.to_le_bytes());
            for field in [&task.turn, &task.conversation] {
                digest.update((field.len() as u64).to_le_bytes());
                digest.update(field.as_bytes());
            }
        }
        let identity = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(format!(
            "request-handoff:{}:{}:{identity}",
            edge.generation, edge.recipient
        ))
    }

    /// Recovers this active owner's exact outcome before request capture or the
    /// consumed-context check. Returns whether canonical input still blocks
    /// inference, using one native TTL snapshot; commit failures return an error.
    /// It never acquires a second scheduler slot.
    pub(in crate::runtime::agent) fn recover_request_wait_context_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<bool> {
        let now_ms = super::super::current_unix_millis();
        self.prune_request_wait_owners_at(now_ms);
        self.record_request_registration_retirements(now_ms);
        self.resume_targeted_request_wait(turn)?;
        Ok(self.request_wait_context_pending_for_turn_at(turn, now_ms))
    }

    /// Checks pending canonical sources against the same native snapshot used
    /// to recover retirement, without a second TTL boundary crossing.
    pub(super) fn request_wait_context_pending_for_turn_at(
        &self,
        turn: &AgentTurnRecord,
        now_ms: u64,
    ) -> bool {
        let Some(edge) = self.agent.request_waits.edges.get(&turn.turn_id) else {
            return false;
        };
        let Some(slot) = self
            .agent
            .request_waits
            .recipients
            .get(&edge.generation)
            .and_then(|slots| slots.get(edge.recipient))
        else {
            return false;
        };
        if slot.outcome.is_some() {
            return !self.request_handoff_is_committed(turn, slot);
        }
        let Some(receipt) = self.agent.request_waits.requests.receipt(edge.generation) else {
            return false;
        };
        let Some(recipient) = receipt.proof().recipients().nth(edge.recipient) else {
            return false;
        };
        // An accepted matching source is not progress or a committed reply.
        // It keeps an old inference from overtaking canonical delivery, even
        // if the source has not advanced the context frontier yet.
        self.control.message_service().has_pending_acceptance_reply(
            receipt.proof(),
            recipient,
            now_ms,
            |envelope| !crate::runtime::control::runtime_owned_bridge_message(envelope),
        )
    }

    /// Verifies a private canonical occurrence, never a historical label lookup.
    fn request_handoff_is_committed(&self, turn: &AgentTurnRecord, slot: &RecipientState) -> bool {
        let Some((owner, sequence)) = &slot.handoff_source else {
            return false;
        };
        let Some(expected) = slot.handoff_digest else {
            return false;
        };
        owner == &turn.turn_id
            && self
                .agent_turn_contexts()
                .get(owner)
                .is_some_and(|context| {
                    context.chronology().iter().any(|event| {
                        event.sequence() == *sequence
                            && event.block().source == mez_agent::ContextSourceKind::PeerMessage
                            && canonical_handoff_digest(
                                &event.block().label,
                                &event.block().content,
                            ) == expected
                    })
                })
    }

    /// Captures content-free evidence from the real dispatch snapshot. Numeric
    /// event high-water alone cannot prove a special-purpose request included
    /// this reference, and test-installed compatibility claims receive no proof.
    pub(in crate::runtime::agent) fn capture_request_wait_provider_context(
        &self,
        dispatch: &super::super::RuntimeAgentProviderDispatch,
    ) -> Option<(u64, usize, mez_agent::ContextEventSequence, [u8; 32])> {
        if dispatch
            .interaction_kind
            .is_some_and(|kind| kind != mez_agent::ModelInteractionKind::ActionExecution)
        {
            return None;
        }
        let edge = self.agent.request_waits.edges.get(&dispatch.turn.turn_id)?;
        let slot = self
            .agent
            .request_waits
            .recipients
            .get(&edge.generation)?
            .get(edge.recipient)?;
        let (owner, sequence) = slot.handoff_source.as_ref()?;
        let digest = slot.handoff_digest?;
        if owner != &dispatch.turn.turn_id
            || !self.request_handoff_is_committed(&dispatch.turn, slot)
        {
            return None;
        }
        dispatch
            .context
            .durable()
            .chronology()
            .iter()
            .any(|event| {
                event.sequence() == *sequence
                    && event.block().source == mez_agent::ContextSourceKind::PeerMessage
                    && canonical_handoff_digest(&event.block().label, &event.block().content)
                        == digest
            })
            .then_some((edge.generation, edge.recipient, *sequence, digest))
    }

    /// Consumes an active dependency only after the real claimed request read
    /// its exact protected reference and its response passes stale-context
    /// validation. No plain wait result or current sibling execution is rewritten.
    pub(in crate::runtime::agent) fn consume_active_request_wait_context(
        &mut self,
        turn: &AgentTurnRecord,
        claimed_source: Option<(u64, usize, mez_agent::ContextEventSequence, [u8; 32])>,
    ) {
        let Some((generation, recipient, source, digest)) = claimed_source else {
            return;
        };
        let Some(edge) = self.agent.request_waits.edges.get(&turn.turn_id).cloned() else {
            return;
        };
        if edge.generation != generation
            || edge.recipient != recipient
            || turn.state != AgentTurnState::Running
            || edge.started != turn.started_at_unix_seconds
            || self.agent_turn_is_human_paused(&turn.turn_id)
            || self
                .agent_shell_store()
                .get(&turn.pane_id)
                .is_none_or(|shell| shell.session_id != turn.conversation_id)
        {
            return;
        }
        let Some(receipt) = self.agent.request_waits.requests.receipt(edge.generation) else {
            return;
        };
        if receipt.origin().agent_id.as_str() != turn.agent_id
            || receipt.origin().conversation_id != turn.conversation_id
            || !self
                .control
                .message_service()
                .acceptance_sender_is_current(receipt.proof())
            || self
                .agent
                .request_waits
                .anchors
                .get(&edge.generation)
                .is_none_or(|anchor| !self.request_parent_anchor_is_current(turn, anchor))
        {
            return;
        }
        let Some(slot) = self
            .agent
            .request_waits
            .recipients
            .get(&edge.generation)
            .and_then(|slots| slots.get(edge.recipient))
        else {
            return;
        };
        if !self.request_handoff_is_committed(turn, slot)
            || slot.handoff_source.as_ref() != Some(&(turn.turn_id.clone(), source))
            || slot.handoff_digest != Some(digest)
        {
            return;
        }
        let progress = match &slot.outcome {
            Some(Outcome::Reply(_)) => true,
            Some(Outcome::Terminal { .. }) => !slot.terminal_consumed,
            Some(Outcome::FailureReference { child, .. }) => {
                !slot.terminal_consumed
                    && self
                        .agent
                        .owned_child_failures
                        .get(child)
                        .is_some_and(|source| !source.wake_admitted)
            }
            _ => false,
        };
        self.consume_request_wait_receipt(turn, &edge, progress);
    }

    /// Releases one exact receipt after context and its existing scheduling or
    /// accepted provider handoff. The bounded cohort records progress once;
    /// optional content-free tracing cannot undo or authorize the handoff.
    fn consume_request_wait_receipt(
        &mut self,
        turn: &AgentTurnRecord,
        edge: &Edge,
        progress: bool,
    ) {
        let Some(origin) = self
            .agent
            .request_waits
            .requests
            .receipt(edge.generation)
            .map(|receipt| receipt.origin().clone())
        else {
            return;
        };
        let Some(slot) = self
            .agent
            .request_waits
            .recipients
            .get(&edge.generation)
            .and_then(|slots| slots.get(edge.recipient))
        else {
            return;
        };
        let terminal_outcome = matches!(
            slot.outcome,
            Some(Outcome::Terminal { .. } | Outcome::FailureReference { .. })
        );
        let terminal_task = slot.task.clone();
        self.agent.request_waits.edges.remove(&turn.turn_id);
        if terminal_outcome && let Some(task) = terminal_task {
            for slots in self.agent.request_waits.recipients.values_mut() {
                for slot in slots {
                    if slot.task.as_ref().is_some_and(|other| {
                        other.turn == task.turn
                            && other.conversation == task.conversation
                            && other.started == task.started
                    }) {
                        slot.terminal_consumed = true;
                    }
                }
            }
        }
        self.release_request_sources(edge.generation);
        self.agent.request_waits.recipients.remove(&edge.generation);
        self.agent.request_waits.anchors.remove(&edge.generation);
        self.agent
            .request_waits
            .fingerprints
            .remove(&edge.generation);
        self.agent
            .request_waits
            .requests
            .consume(edge.generation, &origin);

        let _ = self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "request_dependency consumed generation={} progress_witness={progress}",
                edge.generation
            ),
        );
    }
}

/// Binds both canonical source identity and content without retaining another
/// body. Length-prefixing prevents label/content boundary ambiguity.
fn canonical_handoff_digest(label: &str, content: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update((label.len() as u64).to_le_bytes());
    digest.update(label.as_bytes());
    digest.update(content.as_bytes());
    digest.finalize().into()
}
