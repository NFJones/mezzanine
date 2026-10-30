//! Runtime agent MMP message action helpers.
//!
//! This module owns provider-produced `send_message` execution and sender
//! identity management for the runtime agent harness. Actor-serialized delivery
//! commits canonical input before acknowledging transport; sender action handling
//! never grants replay authority. The receipts child owns bounded receive proof,
//! display recovery, and outbox admission independently of disposable replay.

use std::collections::{BTreeMap, BTreeSet};

use crate::runtime::runtime_effective_config_value;

mod receipts;

use mez_agent::SchedulerErrorKind;

use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentId, AgentTurnExecution,
    AgentTurnRecord, AgentTurnState, Envelope, EventKind, MezError, PaneId, Result,
    RuntimeSessionService, RuntimeSideEffect, ScheduledWork, SenderIdentity, current_unix_seconds,
    json_escape, runtime_agent_turn_state_from_action_results,
    runtime_execution_ready_for_provider_continuation, runtime_maap_message_content_type,
    runtime_message_recipient, runtime_message_scope, runtime_mezzanine_error_code,
    validate_mmp_payload_metadata,
};
use crate::runtime::{RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition};

/// Cadence of the dedicated peer-message delivery and expiry sweep.
///
/// The interval matches the runtime's short status-refresh cadence so pending
/// peer delivery and TTL expiry stay prompt without adding a permanent busy
/// tick. This timer is independent of the runtime actor tick.
pub(crate) const PEER_MESSAGE_DELIVERY_INTERVAL_MS: u64 = 1_000;

/// Maximum characters carried by one prompt-derived fallback objective.
///
/// The fallback exists only for a turn whose response carried no
/// model-authored objective. It summarizes the turn's own text into one bounded
/// factual status line instead of republishing that text, and the shared
/// objective bounds still apply to the result.
pub(crate) const RUNTIME_AGENT_OBJECTIVE_FALLBACK_CHARS: usize = 160;

impl RuntimeSessionService {
    /// Commits unread runtime-agent messages at the actor boundary in delivery
    /// sequence order and acknowledges each message only after its canonical
    /// reference event exists.
    ///
    /// Messages for agents without an active queued, running, or blocked turn
    /// remain behind the durable delivery cursor. A later prompt builder places
    /// them before that task's prelude and prompt. Messages for an active turn
    /// append at arrival time; an older in-flight provider claim will therefore
    /// be rejected by the canonical event high-water check.
    ///
    /// Only model-originated peer mail starts a turn for an idle recipient.
    /// Runtime-owned bridge and lifecycle notifications keep their
    /// pre-idle-turn behavior: they wait behind the durable cursor and are
    /// injected with the recipient's next turn.
    ///
    /// Every committed message gets one recipient-pane presentation attempt
    /// through its canonical peer-message block. Normal mode emits a row only
    /// for the exact canonical plaintext media type; other committed payloads
    /// remain durable and model-visible without pane presentation.
    pub(crate) fn deliver_pending_runtime_agent_messages(&mut self, now_ms: u64) -> Result<usize> {
        self.recover_received_peer_message_turn_scheduling(now_ms)?;
        self.recover_joined_coordination_wakes()?;
        self.recover_owned_child_failures(now_ms)?;
        self.recover_request_waits_at(now_ms)?;
        // Presentation is a recoverable receiver-side projection. Once a
        // receipt and cursor exist, it must never block provider admission or
        // a parked peer wait from resuming; the retained receipt retries on the
        // next delivery sweep.
        let _ = self.flush_received_peer_message_presentations();
        let pending_peer_wait_wakes = self
            .agent
            .agent_turn_peer_wait_wake_pending
            .iter()
            .filter_map(|turn_id| self.agent_turn_ledger().turn(turn_id).cloned())
            .collect::<Vec<_>>();
        for turn in pending_peer_wait_wakes {
            let _ = self.resume_parked_peer_wait(&turn)?;
        }
        let native_commits = self.proven_runtime_message_commits()?;
        let ready = self
            .control
            .message_service_mut()
            .fanout_ready_excluding_native_commits(now_ms, usize::MAX, &native_commits)?;
        let mut committed = 0usize;
        for fanout in ready {
            let recipient = fanout.recipient;
            if !recipient.as_str().starts_with("agent-") {
                continue;
            }
            let Some(turn) = self.runtime_agent_active_turn(recipient.as_str()) else {
                if fanout.batch.messages.iter().all(|message| {
                    crate::runtime::control::runtime_owned_bridge_message(&message.envelope)
                }) {
                    continue;
                }
                let pane_id = recipient.as_str().trim_start_matches("agent-").to_string();
                // The presentation attempt follows the commit rather than this
                // batch. Starting the turn commits every unread message through
                // `peer_message_turn_context`; media type and log mode then
                // decide whether each committed message creates a pane row.
                // The loop limit, a missing session, and any other refusal
                // commit nothing and create no presentation.
                let started = self.start_runtime_peer_message_turn(&pane_id, now_ms)?;
                committed = committed.saturating_add(started);
                continue;
            };
            if !self.agent_turn_contexts().contains_key(&turn.turn_id) {
                continue;
            }

            let mut model_message_count = 0usize;
            for message in fanout.batch.messages {
                let label = crate::runtime::control::runtime_peer_message_block_label(
                    message.sequence,
                    message.envelope.id.as_str(),
                );
                let already_committed =
                    self.runtime_native_message_commit_is_proven(&recipient, &message);
                if !already_committed {
                    if !self.can_admit_received_peer_message_presentation(
                        &recipient,
                        message.sequence,
                        message.envelope.as_ref(),
                        &turn,
                    ) {
                        break;
                    }
                    if !crate::runtime::control::runtime_owned_bridge_message(&message.envelope) {
                        model_message_count = model_message_count.saturating_add(1);
                    }
                    let content = crate::runtime::control::runtime_peer_message_context_content(
                        &message.envelope,
                    );
                    self.agent_turn_contexts_mut()
                        .get_mut(&turn.turn_id)
                        .ok_or_else(|| {
                            MezError::invalid_state("runtime agent turn context is unavailable")
                        })?
                        .append_peer_message_event(label, content)?;
                    self.register_received_peer_message_presentation(
                        recipient.clone(),
                        message.sequence,
                        &turn,
                        message.envelope.as_ref().clone(),
                    )?;
                    committed = committed.saturating_add(1);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "peer_message committed sequence={} message_id={} event_high_water={}",
                            message.sequence,
                            message.envelope.id,
                            self.agent_turn_contexts()
                                .get(&turn.turn_id)
                                .map(mez_agent::AgentContext::event_sequence_high_water_mark)
                                .unwrap_or(0)
                        ),
                    )?;
                }
                self.record_joined_coordination_mail(
                    &turn,
                    message.sequence,
                    message.envelope.as_ref(),
                );
                self.advance_runtime_message_subscription(&recipient, message.sequence)?;
            }

            let _ = self.flush_received_peer_message_presentations();

            if turn.state == AgentTurnState::Blocked && model_message_count > 0 {
                self.agent
                    .agent_turn_peer_wait_wake_pending
                    .insert(turn.turn_id.clone());
                let _ = self.resume_parked_peer_wait(&turn)?;
            }

            if turn.state == AgentTurnState::Queued && !self.agent_work_is_scheduled(&turn.turn_id)
            {
                self.enqueue_agent_work(ScheduledWork {
                    turn_id: turn.turn_id.clone(),
                    conversation_id: turn.conversation_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    pane_id: Some(turn.pane_id.clone()),
                    kind: mez_agent::ScheduledWorkKind::ShellCapable,
                })?;
                self.start_ready_agent_turns()?;
            }

            if turn.state == AgentTurnState::Running
                && !self.agent_provider_task_is_owned(&turn.turn_id)
                && self
                    .agent_turn_executions()
                    .get(&turn.turn_id)
                    .is_none_or(runtime_execution_ready_for_provider_continuation)
            {
                self.queue_agent_provider_task(turn.turn_id.clone());
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    "provider_task queued reason=local_message_arrival",
                )?;
            }
        }
        self.recover_request_waits_at(now_ms)?;
        self.recover_joined_coordination_wakes()?;
        Ok(committed)
    }

    /// Fairly reacquires capacity and settles one parked peer wait after model
    /// mail or authoritative owned-child failure committed to its turn context.
    ///
    /// A full scheduler queue is recoverable: the wake marker remains so a
    /// later delivery sweep retries without losing the committed peer mail.
    pub(super) fn resume_parked_peer_wait(&mut self, turn: &AgentTurnRecord) -> Result<bool> {
        if self.resume_targeted_request_wait(turn)? {
            return Ok(true);
        }
        if turn.state != AgentTurnState::Blocked {
            self.agent
                .agent_turn_peer_wait_wake_pending
                .remove(&turn.turn_id);
            return Ok(false);
        }
        let waiting_for_peer_message =
            self.agent_turn_executions()
                .get(&turn.turn_id)
                .is_some_and(|execution| {
                    execution.action_results.iter().any(|result| {
                        result.action_type == "wait" && result.status == ActionStatus::Running
                    })
                });
        if !waiting_for_peer_message {
            self.agent
                .agent_turn_peer_wait_wake_pending
                .remove(&turn.turn_id);
            return Ok(false);
        }
        match self.agent.agent_scheduler.requeue_waiting(&turn.turn_id) {
            Ok(_) => {}
            Err(error) if error.kind() == SchedulerErrorKind::QueueFull => return Ok(false),
            Err(error) => return Err(error.into()),
        }
        let owned_child_failed = self.agent.owned_child_failures.values().any(|receipt| {
            receipt.committed_turn_id.as_deref() == Some(turn.turn_id.as_str())
                && !receipt.wake_admitted
        });
        let settled_wait = self
            .agent_turn_executions_mut()
            .get_mut(&turn.turn_id)
            .and_then(|execution| {
                let mut settled = false;
                for result in &mut execution.action_results {
                    if result.action_type == "wait" && result.status == ActionStatus::Running {
                        result.status = ActionStatus::Succeeded;
                        result.content = vec![mez_agent::ActionContentBlock::text(
                            if owned_child_failed {
                                "owned child terminated unexpectedly; reassess the missing peer answer"
                            } else {
                                "peer mail received; resuming this turn"
                            },
                        )];
                        result.structured_content_json = Some(if owned_child_failed {
                            r#"{"state":"owned_child_failed"}"#
                        } else {
                            r#"{"state":"peer_message_received"}"#
                        }.to_string());
                        settled = true;
                    }
                }
                if settled {
                    execution.final_turn = false;
                    execution.terminal_state = runtime_agent_turn_state_from_action_results(
                        &execution.action_results,
                        execution.final_turn,
                    );
                }
                settled.then(|| {
                    execution
                        .action_results
                        .iter()
                        .find(|result| result.action_type == "wait")
                        .cloned()
                        .expect("settled peer wait result must remain in execution")
                })
            });
        let Some(settled_wait) = settled_wait else {
            return Ok(false);
        };
        self.agent
            .agent_turn_peer_wait_wake_pending
            .remove(&turn.turn_id);
        self.append_action_result_context_if_absent(&turn.turn_id, &settled_wait)?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            if owned_child_failed {
                "scheduler waiting -> queued reason=owned_child_failed capacity=reacquire"
            } else {
                "scheduler waiting -> queued reason=peer_message_received capacity=reacquire"
            },
        )?;
        self.start_ready_agent_turns()?;
        Ok(true)
    }

    /// Re-admits original queued receive turns after a recoverable receive commit fault.
    ///
    /// Presentation flushing deliberately does not schedule work. This recovery
    /// boundary runs only from the delivery sweep after turn context, profile,
    /// prompt metadata, and cursor state are committed; it therefore admits the
    /// original turn exactly once without letting display persistence own a
    /// partially constructed scheduler claim.
    fn recover_received_peer_message_turn_scheduling(&mut self, now_ms: u64) -> Result<()> {
        let receipt_groups = self
            .agent
            .received_peer_message_presentations
            .iter()
            .map(|(identity, receipt)| {
                (
                    (
                        receipt.recipient_agent_id.to_string(),
                        receipt.turn_id.clone(),
                    ),
                    identity.clone(),
                )
            })
            .fold(
                BTreeMap::<(String, String), Vec<String>>::new(),
                |mut groups, (group, identity)| {
                    groups.entry(group).or_default().push(identity);
                    groups
                },
            );
        let mut orphan_groups = Vec::new();
        for ((recipient, turn_id), identities) in receipt_groups {
            let group_has_unacknowledged_receipt = identities.iter().any(|identity| {
                self.agent
                    .received_peer_message_presentations
                    .get(identity)
                    .is_some_and(|receipt| {
                        !self.received_peer_message_presentation_is_acknowledged(receipt)
                    })
            });
            let group_has_delivery_path = identities.iter().any(|identity| {
                self.agent
                    .received_peer_message_presentations
                    .get(identity)
                    .is_some_and(|receipt| {
                        self.received_peer_message_presentation_is_transport_deliverable(
                            receipt, now_ms,
                        )
                    })
            });
            if group_has_unacknowledged_receipt && !group_has_delivery_path {
                orphan_groups.push((recipient, turn_id, identities));
            }
        }
        // Decide admission from the original live delivery evidence, then
        // settle canonical transport commits before orphan cleanup can remove
        // their contexts. ACK recovery must not grant an expired queued turn
        // new scheduler authority.
        self.reconcile_committed_runtime_peer_messages()?;
        for (_, _, identities) in &orphan_groups {
            for identity in identities {
                let retain_proof = self
                    .agent
                    .received_peer_message_presentations
                    .get(identity)
                    .is_some_and(|receipt| {
                        receipt.native_commit_proven
                            && !self.received_peer_message_presentation_is_acknowledged(receipt)
                    });
                if retain_proof {
                    if let Some(receipt) = self
                        .agent
                        .received_peer_message_presentations
                        .get_mut(identity)
                    {
                        receipt.presentation_eligible = false;
                    }
                } else {
                    self.agent
                        .received_peer_message_presentations
                        .remove(identity);
                }
            }
        }
        for (_, turn_id, _) in orphan_groups {
            let Some(turn) = self.agent_turn_ledger().turn(&turn_id).cloned() else {
                continue;
            };
            if turn.state != AgentTurnState::Queued {
                continue;
            }
            let _ = self.cancel_agent_work(&turn_id);
            self.finish_agent_turn_without_shell_session(&turn, AgentTurnState::Interrupted)?;
        }
        let turn_ids = self
            .agent
            .received_peer_message_presentations
            .values()
            .filter(|receipt| self.received_peer_message_presentation_is_acknowledged(receipt))
            .filter_map(|receipt| {
                self.agent_turn_ledger()
                    .turn(&receipt.turn_id)
                    .filter(|turn| turn.state == AgentTurnState::Queued)
                    .filter(|turn| {
                        !self.pane_is_closing(&turn.pane_id)
                            && self
                                .agent_shell_store()
                                .get(&turn.pane_id)
                                .is_some_and(|shell| shell.session_id == turn.conversation_id)
                    })
                    .filter(|turn| self.agent_turn_contexts().contains_key(&turn.turn_id))
                    .filter(|turn| self.agent_turn_model_profile(&turn.turn_id).is_some())
                    .map(|turn| turn.turn_id.clone())
            })
            .collect::<BTreeSet<_>>();
        let mut recovered = 0usize;
        for turn_id in turn_ids {
            if self.agent_work_is_scheduled(&turn_id) {
                continue;
            }
            let Some(turn) = self.agent_turn_ledger().turn(&turn_id).cloned() else {
                continue;
            };
            self.enqueue_agent_work(ScheduledWork {
                turn_id: turn.turn_id,
                conversation_id: turn.conversation_id,
                agent_id: turn.agent_id,
                pane_id: Some(turn.pane_id),
                kind: mez_agent::ScheduledWorkKind::ShellCapable,
            })?;
            recovered = recovered.saturating_add(1);
        }
        if recovered > 0 {
            self.start_ready_agent_turns()?;
        }
        Ok(())
    }

    /// Reports whether a sender is the recipient's exact direct parent for pane
    /// presentation.
    ///
    /// This intentionally differs from live-parent-authority checks: restored
    /// lineage remains valid historical identity for a stable `parent>` marker,
    /// while fenced descendants must fall back because their old parent edge no
    /// longer belongs to the pane's current conversation.
    pub(crate) fn runtime_peer_message_sender_is_direct_parent(
        &self,
        recipient_agent_id: &str,
        sender_agent_id: &str,
    ) -> bool {
        !self.subagent_descendant_is_fenced(recipient_agent_id)
            && self
                .subagent_lineage(recipient_agent_id)
                .is_some_and(|lineage| {
                    !lineage.parent_agent_id.is_empty()
                        && lineage.parent_agent_id == sender_agent_id
                })
    }

    /// Reports whether an outbound recipient is the sender's exact direct parent.
    ///
    /// The comparison uses only spawn-owned lineage and a parsed single-agent
    /// recipient. Selectors and fenced descendants never receive the alias.
    pub(crate) fn runtime_outbound_recipient_is_direct_parent(
        &self,
        sender_agent_id: &str,
        recipient: &crate::runtime::Recipient,
    ) -> bool {
        let crate::runtime::Recipient::Agent(recipient_agent_id) = recipient else {
            return false;
        };
        !self.subagent_descendant_is_fenced(sender_agent_id)
            && self
                .subagent_lineage(sender_agent_id)
                .is_some_and(|lineage| {
                    !lineage.parent_agent_id.is_empty()
                        && lineage.parent_agent_id == recipient_agent_id.as_str()
                })
    }

    /// Resolves a stable presentation label for one outbound recipient.
    ///
    /// A bounded, sanitized spawn-owned lineage name is preferred for concrete
    /// agents. Selectors retain the provider-supplied expression; concrete
    /// agents without a usable name fall back to their canonical identity.
    pub(crate) fn runtime_outbound_recipient_display_label(
        &self,
        recipient: &crate::runtime::Recipient,
        fallback: &str,
    ) -> String {
        let crate::runtime::Recipient::Agent(agent_id) = recipient else {
            return fallback.to_string();
        };
        self.runtime_concrete_agent_identity_label(agent_id.as_str())
    }

    /// Resolves a concrete peer label from its runtime-owned identity metadata.
    ///
    /// Pane, window, and conversation titles are mutable presentation state, not
    /// agent identity. Use a runtime-owned subagent name when one is available;
    /// otherwise retain the canonical agent id.
    pub(crate) fn runtime_peer_message_endpoint_label(&self, agent_id: &str) -> String {
        self.runtime_concrete_agent_identity_label(agent_id)
    }

    /// Resolves the bounded display label for one concrete runtime agent.
    ///
    /// Only runtime-owned lineage supplies a pretty name. Sanitization that
    /// removes the complete candidate falls back to the canonical identity.
    fn runtime_concrete_agent_identity_label(&self, agent_id: &str) -> String {
        if self.subagent_descendant_is_fenced(agent_id) {
            return agent_id.to_string();
        }
        self.subagent_lineage(agent_id)
            .map(|lineage| mez_agent::agent_list_bounded_text(lineage.display_name.trim()))
            .or_else(|| {
                agent_id
                    .strip_prefix("agent-")
                    .and_then(|pane| self.primary_agent_display_name(pane))
                    .map(mez_agent::agent_list_bounded_text)
            })
            .map(|display_name| display_name.trim().to_string())
            .filter(|display_name| !display_name.is_empty())
            .unwrap_or_else(|| agent_id.to_string())
    }

    /// Starts one peer-message-triggered turn for an idle agent.
    ///
    /// A peer message starts one turn for an idle agent, including in ask mode,
    /// so agent pipelines can make progress. An agent that already has a queued,
    /// running, or blocked turn keeps the arrival-time append path instead. The
    /// configured peer-message loop limit bounds runaway message-triggered
    /// iterations; it never caps injected message counts, payload bytes, or
    /// per-window peer turns. The durable cursor advances only after the new turn
    /// stores the canonical peer-message events.
    fn start_runtime_peer_message_turn(&mut self, pane_id: &str, now_ms: u64) -> Result<usize> {
        self.start_runtime_received_context_turn(pane_id, now_ms, None)
    }

    /// Starts an exact-owned idle failure response through normal receive
    /// admission, including the ordinary loop limit and human inhibition.
    pub(super) fn start_runtime_owned_child_failure_turn(
        &mut self,
        pane_id: &str,
        now_ms: u64,
        receipt: &super::child_failures::OwnedChildFailure,
    ) -> Result<usize> {
        self.start_runtime_received_context_turn(pane_id, now_ms, Some(receipt))
    }

    /// Commits ordinary unread mail and an optional local terminal reference
    /// before the existing scheduler owns a receive-triggered turn.
    fn start_runtime_received_context_turn(
        &mut self,
        pane_id: &str,
        now_ms: u64,
        failure: Option<&super::child_failures::OwnedChildFailure>,
    ) -> Result<usize> {
        let agent_id = format!("agent-{pane_id}");
        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Ok(0);
        };
        // Candidates and terminal handoff receipts belong to this exact
        // conversation, not the pane's earlier or future binding. Defer before
        // speculative history/context, loop accounting, or cursor mutation;
        // accepted envelopes and owned-child receipts remain with their owners.
        if self.persistence.bookkeeping_pending(&session.session_id) {
            return Ok(0);
        }
        if self.subagent_descendant_is_fenced(&agent_id) {
            return Ok(0);
        }
        let loop_limit = self.agent_peer_message_loop_limit();
        let started_turns = self.agent_peer_message_turn_count(&agent_id);
        if started_turns >= loop_limit {
            // The limit is a stable episode state rather than a per-tick event:
            // report it once, and stay silent until direct user input resets the
            // counter and clears the episode marker.
            if !self.agent_peer_message_limit_reported(&agent_id) {
                self.mark_agent_peer_message_limit_reported(&agent_id);
                self.append_agent_status_text_to_terminal_buffer(
                    pane_id,
                    &format!(
                        "agent: peer message loop limit {loop_limit} reached; leaving inbox mail pending"
                    ),
                )?;
                let _ = self.append_lifecycle_event(
                    EventKind::Diagnostic,
                    format!(
                        r#"{{"pane_id":"{}","kind":"peer_message_loop_limit","agent_id":"{}","started_turns":{},"loop_limit":{},"message":"peer-message loop limit reached; no new message-triggered turn was started"}}"#,
                        json_escape(pane_id),
                        json_escape(&agent_id),
                        started_turns,
                        loop_limit
                    ),
                );
            }
            return Ok(0);
        }
        let crate::runtime::control::RuntimePeerMessageTurnContext {
            mut context,
            delivered_message_sequence,
            delivered_messages,
            delivered_message_count,
            imported_history_sequence_high_water,
        } = self.peer_message_turn_context(pane_id, now_ms)?;
        if delivered_message_sequence.is_none() && failure.is_none() {
            return Ok(0);
        }
        if let Some(failure) = failure {
            context.append_peer_message_event(failure.label.clone(), failure.content.clone())?;
        }
        let recipient = AgentId::opaque(agent_id.clone())
            .ok_or_else(|| MezError::invalid_state("runtime agent id is invalid for MMP"))?;
        if !self.can_admit_received_peer_message_presentation_batch(&recipient, &delivered_messages)
        {
            return Ok(0);
        }
        let context = self.apply_agent_shell_preference_context(pane_id, context)?;
        let context = self.apply_persisted_context_documents(pane_id, context)?;
        let conversation_id = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .ok_or_else(|| MezError::invalid_state("agent turn conversation is unavailable"))?;
        let turn_id = self.next_agent_turn_id();
        let context_blocks = context.blocks().len();
        let (model_profile_name, model_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let turn = AgentTurnRecord {
            turn_id: turn_id.clone(),
            conversation_id: conversation_id.clone(),
            agent_id: agent_id.clone(),
            pane_id: pane_id.to_string(),
            trigger: mez_agent::AgentTurnTrigger::LocalMessage,
            started_at_unix_seconds: current_unix_seconds(),
            deadline_at_unix_millis: crate::runtime::current_unix_millis()
                .saturating_add(self.agent_turn_timeout_ms()),
            policy_profile: "runtime".to_string(),
            model_profile: model_profile_name.clone(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Queued,
            initial_capability: None,
        };
        if !self.can_admit_peer_receipts_for_turn(&recipient, &turn, &delivered_messages) {
            return Ok(0);
        }
        #[cfg(test)]
        if self.take_peer_message_turn_commit_failure_for_tests() {
            return Err(MezError::invalid_state(
                "injected peer-message idle-turn pre-commit failure",
            ));
        }
        self.agent_turn_ledger_mut().queue_turn(turn.clone())?;
        self.snapshot_agent_native_shell_timeout_for_turn(&turn_id);
        self.agent_turn_contexts_mut()
            .insert(turn_id.clone(), context);
        self.set_agent_turn_imported_history_sequence_high_water(
            turn_id.clone(),
            imported_history_sequence_high_water,
        );
        self.set_agent_turn_model_profile(turn_id.clone(), model_profile);
        if let Some(failure) = failure
            && let Some(stored) = self
                .agent
                .owned_child_failures
                .get_mut(&failure.child_turn_id)
        {
            stored.committed_turn_id = Some(turn_id.clone());
        }
        // A locally retained failure can recover this exact turn after any
        // subsequent cursor/presentation fault. Charge its normal receive
        // admission once at the same infallible context/receipt boundary.
        if failure.is_some() {
            self.set_agent_peer_message_turn_count(&agent_id, started_turns.saturating_add(1));
            self.clear_agent_peer_message_limit_reported(&agent_id);
        }
        for (sequence, envelope) in delivered_messages.iter().cloned() {
            self.register_received_peer_message_presentation(
                recipient.clone(),
                sequence,
                &turn,
                envelope,
            )?;
        }
        #[cfg(test)]
        if self.take_peer_message_receive_after_context_storage_failure_for_tests() {
            return Err(MezError::invalid_state(
                "injected peer-message idle-turn failure after context storage",
            ));
        }
        if let Some(delivered_message_sequence) = delivered_message_sequence {
            self.advance_runtime_message_subscription(&recipient, delivered_message_sequence)?;
        }
        #[cfg(test)]
        if self.take_peer_message_receive_after_cursor_advance_failure_for_tests() {
            return Err(MezError::invalid_state(
                "injected peer-message idle-turn failure after cursor advancement",
            ));
        }
        if failure.is_none() {
            self.set_agent_peer_message_turn_count(&agent_id, started_turns.saturating_add(1));
            self.clear_agent_peer_message_limit_reported(&agent_id);
        }
        self.admit_owned_child_failure_context_turn(&turn)?;
        let _ = self.flush_received_peer_message_presentations();
        #[cfg(test)]
        if self.take_peer_message_turn_post_admission_failure_for_tests() {
            return Err(MezError::invalid_state(
                "injected peer-message idle-turn failure after scheduler admission",
            ));
        }
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            "created state=queued reason=peer_message_arrival",
        )?;
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            &format!(
                "context prepared blocks={} model_profile={} delivered_messages={}",
                context_blocks, model_profile_name, delivered_message_count
            ),
        )?;
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            "scheduler enqueue kind=shell_capable reason=peer_message_arrival",
        )?;
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!("agent: started turn {turn_id} from pending peer mail"),
        )?;
        Ok(delivered_message_count.saturating_add(usize::from(failure.is_some())))
    }

    /// Reuses normal admission after an exact receive context has committed.
    /// Queue pressure leaves the original queued turn recoverable, not a new
    /// continuation or a direct provider call.
    pub(super) fn admit_owned_child_failure_context_turn(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        if !self.agent_work_is_scheduled(&turn.turn_id) {
            self.enqueue_agent_work(ScheduledWork {
                turn_id: turn.turn_id.clone(),
                conversation_id: turn.conversation_id.clone(),
                agent_id: turn.agent_id.clone(),
                pane_id: Some(turn.pane_id.clone()),
                kind: mez_agent::ScheduledWorkKind::ShellCapable,
            })?;
        }
        self.start_ready_agent_turns()?;
        Ok(())
    }

    /// Returns whether any recipient currently has deliverable peer mail.
    ///
    /// Mail that is waiting only on the peer-message loop limit is not
    /// deliverable: the limit is a stable terminal state for that agent until
    /// direct user input resets the counter, so it must not keep re-arming the
    /// delivery timer (and re-reporting the limit) once per tick. Runtime-owned
    /// bridge traffic never starts a turn, so it keeps the ordinary pending
    /// behavior that also drives TTL expiry. Pending bookkeeping uses this same
    /// bounded one-second maintenance cadence. Committed joined questions retain
    /// retry until fair admission even after transport acknowledgement; consumed
    /// receipts and already queued or claimed work do not renew that timer.
    /// Receive admission checks only
    /// its exact conversation fence, never retries the failed archive read.
    pub(crate) fn has_pending_peer_messages(&mut self, now_ms: u64) -> bool {
        // Custody expiry/retirement is independent of live fanout and loop
        // admission. Retry unavailable evidence conservatively rather than
        // silently abandoning an accepted obligation.
        let native_commits = match self.proven_runtime_message_commits() {
            Ok(commits) => commits,
            Err(_) => return true,
        };
        if self
            .control
            .message_service()
            .needs_delivery_outcome_maintenance_with_native_commits(&native_commits)
            .unwrap_or(true)
            || self
                .has_reconcilable_runtime_message_commits()
                .unwrap_or(true)
        {
            return true;
        }
        if self.has_pending_owned_child_failure_handoff() || self.has_request_waits() {
            return true;
        }
        if !self.agent.agent_turn_peer_wait_wake_pending.is_empty()
            || self.has_pending_joined_coordination_wake()
        {
            return true;
        }
        if self
            .agent
            .received_peer_message_presentations
            .values()
            .any(|receipt| {
                (!receipt.native_commit_proven
                    && self.received_peer_message_presentation_is_transport_deliverable(
                        receipt, now_ms,
                    ))
                    || (receipt.presentation_eligible
                        && self.received_peer_message_presentation_is_acknowledged(receipt)
                        && !self
                            .persistence
                            .presentation_write_pending(&receipt.conversation_id))
            })
        {
            return true;
        }
        let loop_limit = self.agent_peer_message_loop_limit();
        let ready = match self
            .control
            .message_service_mut()
            .fanout_ready_excluding_native_commits(now_ms, 1, &native_commits)
        {
            Ok(ready) => ready,
            Err(_) => return true,
        };
        ready.into_iter().any(|fanout| {
            let recipient = fanout.recipient.as_str().to_string();
            if !recipient.starts_with("agent-") {
                return true;
            }
            if fanout.batch.messages.iter().all(|message| {
                crate::runtime::control::runtime_owned_bridge_message(&message.envelope)
            }) {
                return true;
            }
            if self.runtime_agent_active_turn(&recipient).is_some() {
                return true;
            }
            self.agent_peer_message_turn_count(&recipient) < loop_limit
        })
    }

    /// Returns the queued, running, or blocked turn owned by one agent.
    ///
    /// Delivery treats an agent with such a turn as active: peer mail appends at
    /// arrival time instead of starting a new message-triggered turn.
    fn runtime_agent_active_turn(&self, agent_id: &str) -> Option<AgentTurnRecord> {
        self.agent_turn_ledger()
            .turns()
            .iter()
            .rev()
            .find(|turn| {
                turn.agent_id == agent_id
                    && self
                        .agent_shell_store()
                        .get(&turn.pane_id)
                        .is_some_and(|shell| shell.session_id == turn.conversation_id)
                    && matches!(
                        turn.state,
                        AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
                    )
            })
            .cloned()
    }

    /// Builds the desired dedicated peer-message delivery timer transition.
    ///
    /// The dedicated timer is armed only while deliverable peer mail or a
    /// committed recovery obligation exists and
    /// no delivery timer is already active, so it stays an event-driven wakeup
    /// rather than a permanent tick.
    pub(crate) fn peer_message_delivery_timer_transition(
        &mut self,
        timer_active: bool,
        generation: u64,
        now_ms: u64,
    ) -> RuntimeTransition {
        if timer_active || !self.has_pending_peer_messages(now_ms) {
            return RuntimeTransition::default();
        }
        RuntimeTransition {
            applied: false,
            side_effects: vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(
                    RuntimeTimerKind::PeerMessageDelivery,
                    "peer-message-delivery",
                    generation,
                ),
                delay_ms: PEER_MESSAGE_DELIVERY_INTERVAL_MS,
            }],
        }
    }

    /// Applies one dedicated peer-message delivery and expiry pass.
    ///
    /// Returns the transition for the actor so it can re-arm the periodic sweep
    /// while deliverable mail remains. Expired or undeliverable envelopes keep
    /// their existing sender-visible message-service semantics.
    pub(crate) fn apply_peer_message_delivery_timer(
        &mut self,
        now_ms: u64,
        generation: u64,
    ) -> Result<RuntimeTransition> {
        let committed = self.deliver_pending_runtime_agent_messages(now_ms)?;
        // Canonical native input reconciliation above must precede terminal
        // classification. Handoff never grants scheduler/provider authority.
        let native_commits = self.proven_runtime_message_commits()?;
        let terminal = self
            .control
            .message_service_mut()
            .settle_delivery_outcomes_with_native_commits_at(now_ms, &native_commits)?;
        self.reconcile_committed_runtime_peer_messages()?;
        let _ = self.flush_received_peer_message_presentations();
        let handed_off = self.handoff_runtime_message_delivery_outcomes()?;
        let side_effects = if self.has_pending_peer_messages(now_ms) {
            vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(
                    RuntimeTimerKind::PeerMessageDelivery,
                    "peer-message-delivery",
                    generation,
                ),
                delay_ms: PEER_MESSAGE_DELIVERY_INTERVAL_MS,
            }]
        } else {
            Vec::new()
        };
        Ok(RuntimeTransition {
            applied: committed > 0 || terminal > 0 || handed_off > 0,
            side_effects,
        })
    }

    /// Runs the execute running message actions for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn execute_running_message_actions_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        if execution.terminal_state != AgentTurnState::Running {
            return Ok(0);
        }
        let Some(batch) = execution.response.action_batch.clone() else {
            return Ok(0);
        };
        let mut executed = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || execution.action_results[index].action_type != "send_message"
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running message result does not match an action")
                })?;
            execution.action_results[index] =
                self.execute_message_action_for_turn(turn, index, &action)?;
            executed = executed.saturating_add(1);
        }
        self.admit_request_wait_actions(turn, execution)?;
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        self.finalize_settled_outbound_message_previews(&turn.pane_id, &turn.turn_id, execution)?;
        Ok(executed)
    }

    /// Runs the execute message action for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_message_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action_index: usize,
        action: &AgentAction,
    ) -> Result<ActionResult> {
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
                "message execution requires a send_message action",
            ));
        };
        let scope = match runtime_message_scope(scope.as_deref()) {
            Ok(scope) => scope,
            Err(error) => {
                let mut result = ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    "invalid_message_scope",
                    error.message().to_string(),
                )?;
                result.structured_content_json = Some(
                    serde_json::json!({
                        "recipient": recipient,
                        "scope": scope,
                        "message_id": null,
                        "delivery_status": "rejected",
                        "protocol_error": {
                            "code": "invalid_message_scope",
                            "message": error.message(),
                        }
                    })
                    .to_string(),
                );
                return Ok(result);
            }
        };
        let scope = match scope {
            mez_agent::messaging::MessageScope::Project => "project",
            mez_agent::messaging::MessageScope::Session => "session",
        };
        let content_type = runtime_maap_message_content_type(content_type);
        if let Err(error) = validate_mmp_payload_metadata("send", &content_type, payload, None) {
            let error = MezError::from(error);
            let mut result = ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                "invalid_message_payload",
                error.message().to_string(),
            )?;
            result.structured_content_json = Some(format!(
                r#"{{"recipient":"{}","scope":"{}","content_type":"{}","message_id":null,"delivery_status":"rejected","protocol_error":{{"code":"{}","message":"{}"}}}}"#,
                json_escape(recipient),
                scope,
                json_escape(&content_type),
                runtime_mezzanine_error_code(error.kind()),
                json_escape(error.message())
            ));
            return Ok(result);
        }
        if let Some(mut result) = self.queue_macro_managed_message_step(
            turn,
            action,
            recipient,
            scope,
            &content_type,
            payload,
        )? {
            Self::runtime_message_result_with_scope(&mut result, recipient, scope);
            return Ok(result);
        }
        let sender = self.runtime_message_sender_identity(turn)?;
        let recipient_target = match runtime_message_recipient(recipient) {
            Ok(target) => target,
            Err(error) => {
                let mut result = ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    "invalid_message_recipient",
                    error.message().to_string(),
                )?;
                result.structured_content_json = Some(
                    serde_json::json!({
                        "recipient": recipient,
                        "scope": scope,
                        "message_id": null,
                        "delivery_status": "rejected",
                        "delivery_applied": false,
                        "accepted_recipient_forms": [
                            "session", "agent:<id>", "pane:<id>", "window:<id>",
                            "role:<name>", "capability:<name>", "group:<name>"
                        ]
                    })
                    .to_string(),
                );
                return Ok(result);
            }
        };
        let message_id = format!("{}:{}", turn.turn_id, action.id);
        let now_ms = current_unix_seconds().saturating_mul(1000);
        let envelope = Envelope {
            protocol: "mmp/1",
            id: message_id.clone(),
            message_type: "send".to_string(),
            time: format!("runtime:{now_ms}"),
            sender: sender.clone(),
            recipient: recipient_target.clone(),
            correlation_id: correlation_id
                .clone()
                .or_else(|| Some(turn.turn_id.clone())),
            ttl_ms: None,
            content_type: content_type.clone(),
            payload: payload.clone(),
            extension_fields: Vec::new(),
        };
        let message_scope = match scope {
            "project" => mez_agent::messaging::MessageScope::Project,
            "session" => mez_agent::messaging::MessageScope::Session,
            _ => unreachable!("message scope was normalized above"),
        };
        let acceptance = if *expect_reply {
            self.accept_tracked_runtime_message(turn, envelope, message_scope, now_ms)
        } else {
            self.control.message_service_mut().accept_at_with_scope(
                &sender.agent_id,
                envelope,
                message_scope,
                now_ms,
            )
        };
        let delivery = match acceptance {
            Ok(delivery) => delivery,
            Err(error) => {
                let capacity = error.kind() == mez_agent::messaging::MessageErrorKind::Capacity;
                let error_code = if capacity && *expect_reply {
                    "request_tracking_capacity"
                } else if capacity {
                    "capacity"
                } else if error.kind() == mez_agent::messaging::MessageErrorKind::NotFound
                    && error.message() == "message recipient is not registered or available"
                {
                    "message_recipient_unavailable"
                } else {
                    "transport_error"
                };
                let error = MezError::from(error);
                let mut result = ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    error_code,
                    error.message().to_string(),
                )?;
                result.structured_content_json = Some(
                    serde_json::json!({
                        "recipient": recipient,
                        "scope": scope,
                        "message_id": null,
                        "delivery_status": if capacity { "rejected" } else { "failed" },
                        "accepted": false,
                        "delivery_applied": false,
                        "protocol_error": { "code": error_code, "message": error.message() },
                    })
                    .to_string(),
                );
                return Ok(result);
            }
        };
        self.retain_outbound_delivery_owner(turn, action, &delivery);
        if !expect_reply {
            self.record_joined_coordination_reply(turn, &delivery);
        }
        self.deliver_pending_runtime_agent_messages(now_ms)?;
        let result = ActionResult::succeeded(
            turn,
            action,
            vec![format!(
                "message {} delivered to {} recipient(s)",
                delivery.message_id, delivery.queued_recipients
            )],
            Some(format!(
                r#"{{"recipient":"{}","scope":"{}","message_id":"{}","delivery_status":"accepted","queued_recipients":{},"sequence":{},"expect_reply":{},"protocol_error":null}}"#,
                json_escape(recipient),
                scope,
                json_escape(&delivery.message_id),
                delivery.queued_recipients,
                delivery.sequence,
                expect_reply
            )),
        );
        self.settle_accepted_outbound_message_preview(
            &turn.pane_id,
            &turn.turn_id,
            action_index,
            action,
        )?;
        Ok(result)
    }

    /// Adds the public resolved audience to one macro-managed message result.
    ///
    /// Macro bridge outcomes are synthesized by a separate subsystem, so this
    /// final common boundary keeps their result contract aligned with normal
    /// message execution without exposing trusted project membership details.
    pub(crate) fn runtime_message_result_with_scope(
        result: &mut ActionResult,
        recipient: &str,
        scope: &str,
    ) {
        let mut structured = result
            .structured_content_json
            .as_deref()
            .and_then(|content| serde_json::from_str::<serde_json::Value>(content).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        let object = structured
            .as_object_mut()
            .expect("macro message structured result is an object");
        object
            .entry("recipient")
            .or_insert_with(|| serde_json::Value::String(recipient.to_string()));
        object.insert(
            "scope".to_string(),
            serde_json::Value::String(scope.to_string()),
        );
        result.structured_content_json = Some(structured.to_string());
    }

    /// Runs the runtime message sender identity operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn runtime_message_sender_identity(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<SenderIdentity> {
        let agent_id = AgentId::opaque(turn.agent_id.clone())
            .ok_or_else(|| MezError::invalid_args("turn agent id is invalid for MMP"))?;
        let objective = self.runtime_agent_effective_objective(
            &turn.conversation_id,
            self.runtime_agent_turn_objective(turn).as_deref(),
        );
        let identity = if let Some(identity) = self
            .control
            .message_service()
            .registered_identity(&agent_id)
        {
            identity.clone()
        } else {
            let pane_id = PaneId::parse('%', turn.pane_id.clone());
            let project_scope = pane_id
                .as_ref()
                .and_then(|pane_id| self.runtime_message_project_scope(pane_id));
            let window_id = self
                .find_pane_descriptor(&turn.pane_id)
                .map(|descriptor| descriptor.window_id);
            self.control.message_service_mut().ensure_agent_identity(
                SenderIdentity {
                    agent_id,
                    project_scope,
                    pane_id,
                    window_id,
                    role: Some("agent".to_string()),
                    capabilities: vec!["agent-harness".to_string()],
                    objective: objective.clone().flatten(),
                },
                current_unix_seconds().saturating_mul(1000),
            )?
        };
        if let Some(objective) = objective {
            self.publish_prepared_runtime_agent_objective(&turn.agent_id, objective.as_deref());
            self.mirror_runtime_agent_objective(&turn.conversation_id, objective.as_deref());
        }
        self.control
            .message_service()
            .registered_identity(&identity.agent_id)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("runtime MMP identity disappeared after refresh")
            })
    }

    /// Publishes one bounded agent objective for peer discovery.
    ///
    /// Returns true only when the published value changed. The objective is
    /// untrusted discovery data: it is normalized through the shared objective
    /// bounds, is never logged raw, and never authorizes anything. A failed
    /// refresh keeps the previous objective and never fails a turn.
    pub(crate) fn publish_prepared_runtime_agent_objective(
        &mut self,
        agent_id: &str,
        objective: Option<&str>,
    ) -> bool {
        let Some(agent_id) = AgentId::opaque(agent_id.to_string()) else {
            return false;
        };
        let now_ms = current_unix_seconds().saturating_mul(1000);
        self.control
            .message_service_mut()
            .update_agent_objective(&agent_id, objective, now_ms)
            .unwrap_or(false)
    }

    /// Resolves the objective that may be published for one conversation.
    ///
    /// A user-selected durable objective is authoritative until explicitly
    /// cleared. Persistence failures deliberately preserve automatic behavior
    /// for existing runtime-only conversations; the slash mutation boundary
    /// rejects unavailable persistence before accepting a user selection.
    pub(crate) fn runtime_agent_effective_objective(
        &self,
        conversation_id: &str,
        automatic: Option<&str>,
    ) -> Option<Option<String>> {
        if self.runtime_agent_conversation_is_ephemeral(conversation_id) {
            return Some(automatic.map(ToOwned::to_owned));
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Some(automatic.map(ToOwned::to_owned));
        };
        match store.user_objective(conversation_id) {
            Ok(Some(objective)) => Some(Some(objective)),
            Ok(None) => match store.parent_objective(conversation_id) {
                Ok(Some(objective)) => Some(Some(objective)),
                Ok(None) => Some(automatic.map(ToOwned::to_owned)),
                Err(_) => None,
            },
            Err(_) => None,
        }
    }

    /// Synchronizes a pane agent's MMP identity to its currently bound conversation.
    ///
    /// Durable user objectives are restored immediately. Conversations without
    /// a user selection explicitly clear the previous identity value so a
    /// resume, fresh conversation, or fork cannot expose another conversation's
    /// automatic or user-selected objective.
    pub(crate) fn sync_runtime_agent_objective_for_conversation(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
    ) -> Result<()> {
        let objective = if self.runtime_agent_conversation_is_ephemeral(conversation_id) {
            None
        } else if let Some(store) = self.persistence.cloned_transcript_store() {
            store.effective_persisted_objective(conversation_id)?
        } else {
            None
        };
        self.sync_runtime_agent_objective_for_pane(pane_id, conversation_id, objective.as_deref())
    }

    /// Synchronizes one pane identity after that pane changes conversation bindings.
    fn sync_runtime_agent_objective_for_pane(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        objective: Option<&str>,
    ) -> Result<()> {
        let agent_id = format!("agent-{pane_id}");
        let message_agent_id = mez_core::ids::AgentId::opaque(agent_id.clone())
            .ok_or_else(|| MezError::invalid_args("agent id is invalid for MMP"))?;
        let identity_registered = self
            .control
            .message_service()
            .registered_identity(&message_agent_id)
            .is_some();
        if !identity_registered {
            if self.subagent_lineage(&agent_id).is_some() {
                return Ok(());
            }
            self.ensure_runtime_message_identity(
                &agent_id,
                None,
                "agent",
                &["agent-harness"],
                current_unix_seconds().saturating_mul(1000),
            )?;
        }
        let agent_id = message_agent_id;
        if let Some(objective) = objective {
            self.control.message_service_mut().update_agent_objective(
                &agent_id,
                Some(objective),
                current_unix_seconds().saturating_mul(1000),
            )?;
        } else {
            self.control
                .message_service_mut()
                .clear_agent_objective(&agent_id, current_unix_seconds().saturating_mul(1000))?;
        }
        self.mirror_runtime_agent_objective(conversation_id, objective);
        Ok(())
    }

    /// Synchronizes every live durable pane bound to one conversation.
    ///
    /// A conversation may remain live in more than one pane. An explicit user
    /// set or clear therefore updates every authenticated pane identity rather
    /// than leaving peers to observe a stale objective from a sibling pane.
    pub(crate) fn sync_runtime_agent_objectives_for_conversation(
        &mut self,
        conversation_id: &str,
        objective: Option<&str>,
    ) -> Result<()> {
        let pane_ids = self
            .agent_shell_store()
            .sessions()
            .filter(|session| !session.ephemeral && session.session_id == conversation_id)
            .map(|session| session.pane_id.clone())
            .collect::<Vec<_>>();
        for pane_id in pane_ids {
            let agent_id = format!("agent-{pane_id}");
            self.ensure_runtime_message_identity(
                &agent_id,
                None,
                "agent",
                &["agent-harness"],
                current_unix_seconds().saturating_mul(1000),
            )?;
            let agent_id = mez_core::ids::AgentId::opaque(agent_id)
                .ok_or_else(|| MezError::invalid_args("agent id is invalid for MMP"))?;
            if let Some(objective) = objective {
                self.control.message_service_mut().update_agent_objective(
                    &agent_id,
                    Some(objective),
                    current_unix_seconds().saturating_mul(1000),
                )?;
            } else {
                self.control.message_service_mut().clear_agent_objective(
                    &agent_id,
                    current_unix_seconds().saturating_mul(1000),
                )?;
            }
        }
        self.mirror_runtime_agent_objective(conversation_id, objective);
        Ok(())
    }

    /// Synchronizes one pane's already validated objective during a staged
    /// restore or resume commit.
    pub(crate) fn sync_prepared_runtime_agent_objective_for_conversation(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        objective: Option<&str>,
    ) -> Result<()> {
        self.sync_runtime_agent_objective_for_pane(pane_id, conversation_id, objective)
    }

    /// Queues one bounded display mirror of a published agent objective.
    ///
    /// The mirror exists so archived and offline conversations can still resolve
    /// a policy-derived title. It is written only from the published objective
    /// value, is bounded by the shared title rules, and never authorizes
    /// anything. The mirror is a cache: a missing, unchanged, or unreadable
    /// mirror is not an error and never fails a turn. The return value means a
    /// new desired cache revision was queued, not written. Its exact worker
    /// acknowledgment refreshes prompt-selector candidates and saved browsers.
    ///
    /// Ephemeral conversations (routed workers) never persist a transcript and
    /// never enter the saved-session catalog, so they are skipped entirely rather
    /// than growing the bounded mirror index. A published objective that bounds
    /// to nothing retires any retained mirror so the row resolves its title from
    /// the first prompt again.
    pub(crate) fn mirror_runtime_agent_objective(
        &mut self,
        conversation_id: &str,
        objective: Option<&str>,
    ) -> bool {
        self.note_composer_objective(conversation_id, objective);
        if self.runtime_agent_conversation_is_ephemeral(conversation_id) {
            return false;
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return false;
        };
        let changed = self.queue_runtime_objective_mirror(store, conversation_id, objective);
        // Publishing the objective is where a conversation becomes eligible for
        // turn-less generated-title work. Admission is idempotent, never creates
        // a turn, and never fails the objective refresh.
        let _ = self.schedule_runtime_agent_session_title(conversation_id, objective);
        changed
    }

    /// Reports whether one conversation belongs to an ephemeral agent session.
    ///
    /// Routed workers bind fresh runtime-only conversation ids such as
    /// `routed-<parent>-<turn>-worker`. They never persist a transcript, never
    /// enter the saved-session catalog, and can never resolve a mirrored title,
    /// so mirroring them would only grow the bounded index. Title scheduling
    /// shares this guard, because a generated title for such a conversation would
    /// spend a provider call and an index write that no surface can ever render.
    pub(crate) fn runtime_agent_conversation_is_ephemeral(&self, conversation_id: &str) -> bool {
        self.agent_shell_store()
            .sessions()
            .any(|session| session.ephemeral && session.session_id == conversation_id)
    }

    /// Publishes the objective implied by one provider response for a turn.
    ///
    /// A model-authored objective carried in the turn response envelope wins; a
    /// turn that yields none falls back to the bounded, non-verbatim derivation
    /// in [`Self::runtime_agent_objective_from_prompt`] so the objective still
    /// refreshes at least once per turn. Identical values publish nothing, and a
    /// turn that produces neither stays a no-op that keeps the previously
    /// published objective.
    pub(crate) fn publish_runtime_agent_objective_for_response(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> bool {
        let automatic = mez_agent::parse_maap_batch_objective(&execution.response.raw_text)
            .or_else(|| self.runtime_agent_turn_objective(turn));
        let Some(objective) =
            self.runtime_agent_effective_objective(&turn.conversation_id, automatic.as_deref())
        else {
            return false;
        };
        let changed =
            self.publish_prepared_runtime_agent_objective(&turn.agent_id, objective.as_deref());
        self.mirror_runtime_agent_objective(&turn.conversation_id, objective.as_deref());
        changed
    }

    /// Returns the bounded fallback objective implied by one turn's own prompt.
    pub(crate) fn runtime_agent_turn_objective(&self, turn: &AgentTurnRecord) -> Option<String> {
        let context = self.agent_turn_contexts().get(&turn.turn_id)?;
        let prompt = context
            .blocks()
            .iter()
            .rev()
            .find(|block| block.source == mez_agent::ContextSourceKind::UserInstruction)
            .or_else(|| context.blocks().last())?
            .content
            .as_str();
        Self::runtime_agent_objective_from_prompt(prompt)
    }

    /// Derives one bounded fallback objective from turn prompt or task text.
    ///
    /// This is the bounded last-resort source for a turn whose response carried
    /// no model-authored objective: the model-generated `objective` turn field is
    /// the primary source (see `maap_action_batch_schema` and the peer-messaging
    /// prompt section). The derivation is deliberately non-verbatim, because a
    /// published objective is a factual statement of the agent's current work and
    /// never raw prompt text: it keeps the first task-bearing line, drops leading
    /// heading, quote, and list markers, collapses whitespace, and truncates at
    /// [`RUNTIME_AGENT_OBJECTIVE_FALLBACK_CHARS`] on a word boundary. Prompt
    /// ingestion rejects nothing for spanning lines, so an ordinary multi-line
    /// prompt still yields a value; text that still cannot satisfy the shared
    /// objective bounds yields nothing and the previous objective stays in place.
    pub(crate) fn runtime_agent_objective_from_prompt(prompt: &str) -> Option<String> {
        let first_line = prompt.lines().find(|line| !line.trim().is_empty())?;
        let stripped = first_line.trim().trim_start_matches(|character: char| {
            character.is_ascii_digit()
                || matches!(
                    character,
                    '#' | '*' | '-' | '>' | '`' | '|' | '.' | ')' | ' '
                )
        });
        let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            return None;
        }
        let bounded = match collapsed
            .char_indices()
            .nth(RUNTIME_AGENT_OBJECTIVE_FALLBACK_CHARS)
        {
            None => collapsed,
            Some((cut, _)) => {
                let head = &collapsed[..cut];
                let word_end = head.rfind(' ').unwrap_or(head.len());
                format!("{}...", head[..word_end].trim_end())
            }
        };
        mez_agent::messaging::normalize_objective(&bounded).ok()
    }
}
