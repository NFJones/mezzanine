//! Runtime agent MMP message action helpers.
//!
//! This module owns provider-produced `send_message` execution and sender
//! identity management for the runtime agent harness. It keeps envelope
//! validation, message-service delivery, and provider-continuation context
//! handling together.

use std::collections::{BTreeMap, BTreeSet};

use crate::runtime::{
    PeerMessageLogMode, runtime_agent_peer_message_log_mode_from_config,
    runtime_effective_config_value, runtime_peer_message_presentation_is_visible,
};
use crate::storage::snapshot::MAX_UNSETTLED_PEER_PRESENTATIONS;

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
        // Presentation is a recoverable receiver-side projection. Once a
        // receipt and cursor exist, it must never block provider admission or
        // a parked peer wait from resuming; the retained receipt retries on the
        // next delivery sweep.
        let _ = self.flush_received_peer_message_presentations();
        let ready = self
            .control
            .message_service_mut()
            .fanout_ready(now_ms, usize::MAX);
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
                    self.agent_turn_contexts()
                        .get(&turn.turn_id)
                        .is_some_and(|context| {
                            context.blocks().iter().any(|block| {
                                block.source == mez_agent::ContextSourceKind::PeerMessage
                                    && block.label == label
                            })
                        });
                if !already_committed {
                    if !self.can_admit_received_peer_message_presentation(
                        &recipient,
                        message.sequence,
                        message.envelope.as_ref(),
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
                    );
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
                self.control
                    .message_service_mut()
                    .advance_subscription(&recipient, message.sequence)?;
            }

            let _ = self.flush_received_peer_message_presentations();

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
        Ok(committed)
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
        for (_, _, identities) in &orphan_groups {
            for identity in identities {
                self.agent
                    .received_peer_message_presentations
                    .remove(identity);
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

    /// Renders one retained receiver source without consulting transport mail.
    ///
    /// Persistence retries and snapshot recovery reuse the source captured at
    /// receive commit. They must not derive labels or payloads from the
    /// retention-bounded MMP queue, and they may call this only before the
    /// one-time live row has been installed.
    fn echo_received_peer_message_presentation_to_pane(
        &mut self,
        pane_id: &str,
        receive_identity: &str,
        peer_label: &str,
        direct_parent: bool,
        content_type: &str,
        payload: &str,
    ) -> Result<()> {
        if direct_parent {
            self.append_committed_received_direct_parent_message_to_terminal_buffer(
                pane_id,
                receive_identity,
                content_type,
                payload,
            )
        } else {
            self.append_committed_received_peer_message_to_terminal_buffer(
                pane_id,
                receive_identity,
                peer_label,
                content_type,
                payload,
            )
        }
    }

    /// Returns the stable receiver-owned identity for one accepted MMP delivery.
    ///
    /// Payload text is deliberately absent: two accepted envelopes can carry the
    /// same text and each must retain a distinct presentation row, while a retry
    /// of one accepted envelope must reuse the exact same receipt.
    fn received_peer_message_presentation_id(
        recipient: &AgentId,
        sequence: mez_agent::messaging::MessageSequence,
        envelope: &Envelope,
    ) -> String {
        format!(
            "peer-message recipient={} sequence={sequence} id={}",
            recipient, envelope.id
        )
    }

    /// Returns whether a delivery can retain its reconstructible receiver receipt.
    ///
    /// The live outbox intentionally shares the snapshot's 1,024-record bound.
    /// A new delivery is not acknowledged when its receipt cannot be retained,
    /// preserving lossless backpressure rather than dropping a receiver row.
    fn can_admit_received_peer_message_presentation(
        &self,
        recipient: &AgentId,
        sequence: mez_agent::messaging::MessageSequence,
        envelope: &Envelope,
    ) -> bool {
        if !self.received_peer_message_presentation_is_visible(&envelope.content_type) {
            return true;
        }
        let identity = Self::received_peer_message_presentation_id(recipient, sequence, envelope);
        self.agent
            .received_peer_message_presentations
            .contains_key(&identity)
            || self.agent.received_peer_message_presentations.len()
                < MAX_UNSETTLED_PEER_PRESENTATIONS
    }

    /// Returns whether an idle receive batch can retain every visible receipt.
    ///
    /// Idle delivery creates one turn for the entire unread batch, so capacity
    /// must be reserved for all new identities before its cursor advances.
    pub(crate) fn can_admit_received_peer_message_presentation_batch(
        &self,
        recipient: &AgentId,
        deliveries: &[(mez_agent::messaging::MessageSequence, Envelope)],
    ) -> bool {
        let new_identities = deliveries
            .iter()
            .filter(|(_, envelope)| {
                self.received_peer_message_presentation_is_visible(&envelope.content_type)
            })
            .map(|(sequence, envelope)| {
                Self::received_peer_message_presentation_id(recipient, *sequence, envelope)
            })
            .filter(|identity| {
                !self
                    .agent
                    .received_peer_message_presentations
                    .contains_key(identity)
            })
            .collect::<BTreeSet<_>>();
        self.agent
            .received_peer_message_presentations
            .len()
            .saturating_add(new_identities.len())
            <= MAX_UNSETTLED_PEER_PRESENTATIONS
    }

    /// Records one committed inbound delivery before its cursor acknowledgement.
    pub(crate) fn register_received_peer_message_presentation(
        &mut self,
        recipient_agent_id: AgentId,
        sequence: mez_agent::messaging::MessageSequence,
        turn: &AgentTurnRecord,
        envelope: Envelope,
    ) {
        let identity =
            Self::received_peer_message_presentation_id(&recipient_agent_id, sequence, &envelope);
        let direct_parent = self.runtime_peer_message_sender_is_direct_parent(
            recipient_agent_id.as_str(),
            envelope.sender.agent_id.as_str(),
        );
        let peer_label = if direct_parent {
            "parent".to_string()
        } else {
            self.runtime_peer_message_endpoint_label(envelope.sender.agent_id.as_str())
        };
        let presentation_eligible =
            self.received_peer_message_presentation_is_visible(&envelope.content_type);
        self.agent
            .received_peer_message_presentations
            .entry(identity)
            .or_insert(super::RuntimeReceivedPeerMessagePresentation {
                recipient_agent_id,
                pane_id: turn.pane_id.clone(),
                conversation_id: turn.conversation_id.clone(),
                turn_id: turn.turn_id.clone(),
                sequence,
                content_type: envelope.content_type.clone(),
                payload: crate::runtime::control::runtime_peer_message_logged_payload(
                    &envelope.payload,
                ),
                peer_label,
                direct_parent,
                presentation_eligible,
                presentation_attempted: false,
            });
    }

    /// Captures durable recovery sources only after their delivery cursors commit.
    ///
    /// Pre-cursor receipts protect an in-memory partial receive commit until a
    /// remaining delivery can advance the cumulative cursor. They cannot appear
    /// in a v6 snapshot because the payload validates every receipt against its
    /// acknowledged cursor; restart recovery instead derives those deliveries
    /// from retained transport state.
    pub(crate) fn snapshot_unsettled_received_peer_message_presentations(
        &self,
    ) -> Vec<crate::storage::snapshot::SnapshotUnsettledPeerPresentation> {
        self.agent
            .received_peer_message_presentations
            .iter()
            .filter(|(_, receipt)| self.received_peer_message_presentation_is_acknowledged(receipt))
            .map(|(identity, receipt)| {
                crate::storage::snapshot::SnapshotUnsettledPeerPresentation {
                    identity: identity.clone(),
                    recipient_agent_id: receipt.recipient_agent_id.to_string(),
                    pane_id: receipt.pane_id.clone(),
                    conversation_id: receipt.conversation_id.clone(),
                    turn_id: receipt.turn_id.clone(),
                    sequence: receipt.sequence,
                    peer_label: receipt.peer_label.clone(),
                    direct_parent: receipt.direct_parent,
                    content_type: receipt.content_type.clone(),
                    payload: receipt.payload.clone(),
                    presentation_eligible: receipt.presentation_eligible,
                    live_rendered: receipt.presentation_attempted,
                }
            })
            .collect()
    }

    /// Restores the receiver-owned presentation outbox captured with a session
    /// snapshot.
    ///
    /// The source belongs to the recipient and survives separately from MMP
    /// transport retention. A restored receipt is still gated by its delivery
    /// cursor before it can render or permit recovered provider work.
    pub(crate) fn restore_unsettled_received_peer_message_presentations(
        &mut self,
        outbox: &[crate::storage::snapshot::SnapshotUnsettledPeerPresentation],
    ) -> Result<()> {
        for entry in outbox {
            let recipient_agent_id =
                AgentId::opaque(entry.recipient_agent_id.clone()).ok_or_else(|| {
                    MezError::invalid_state("snapshot peer presentation recipient is invalid")
                })?;
            self.agent
                .received_peer_message_presentations
                .entry(entry.identity.clone())
                .or_insert(super::RuntimeReceivedPeerMessagePresentation {
                    recipient_agent_id,
                    pane_id: entry.pane_id.clone(),
                    conversation_id: entry.conversation_id.clone(),
                    turn_id: entry.turn_id.clone(),
                    sequence: entry.sequence,
                    content_type: entry.content_type.clone(),
                    payload: entry.payload.clone(),
                    peer_label: entry.peer_label.clone(),
                    direct_parent: entry.direct_parent,
                    presentation_eligible: entry.presentation_eligible,
                    presentation_attempted: entry.live_rendered,
                });
        }
        Ok(())
    }

    /// Restores only v6 snapshot receipts that remain absent from durable presentation storage.
    ///
    /// Snapshot startup can inspect the durable receiver log before admitting
    /// an outbox entry, unlike `/resume` rollback which must restore its exact
    /// in-memory receipt set without consulting external state. Filtering here
    /// prevents stale durable receipts from consuming capacity or scheduling.
    pub(crate) fn restore_snapshot_unsettled_received_peer_message_presentations(
        &mut self,
        outbox: &[crate::storage::snapshot::SnapshotUnsettledPeerPresentation],
    ) -> Result<()> {
        let mut unsettled = Vec::with_capacity(outbox.len());
        for entry in outbox {
            if !self.received_peer_message_presentation_is_persisted(
                &entry.conversation_id,
                &entry.identity,
            )? {
                unsettled.push(entry.clone());
            }
        }
        self.restore_unsettled_received_peer_message_presentations(&unsettled)
    }

    /// Reconstructs unsettled receiver receipts after durable message and pane
    /// session state have been restored from a snapshot.
    ///
    /// A cursor proves that a recipient committed the canonical context event,
    /// while the presentation log proves whether its receiver row settled. The
    /// retained queue supplies any acknowledged envelope whose row is absent;
    /// existing persisted identities are intentionally not reintroduced.
    pub(crate) fn reconstruct_received_peer_message_presentations_after_restore(
        &mut self,
        reconstruct_from_transport: bool,
    ) -> Result<()> {
        if !reconstruct_from_transport {
            return Ok(());
        }
        let sessions = self
            .agent_shell_store()
            .sessions()
            .filter(|session| !session.ephemeral)
            .map(|session| (session.pane_id.clone(), session.session_id.clone()))
            .collect::<Vec<_>>();
        let mut candidates = Vec::new();
        let mut candidate_identities = BTreeSet::new();
        for (pane_id, conversation_id) in sessions {
            let recipient = AgentId::opaque(format!("agent-{pane_id}"))
                .ok_or_else(|| MezError::invalid_state("runtime agent id is invalid for MMP"))?;
            let Some(cursor) = self.control.message_service().subscription(&recipient) else {
                continue;
            };
            let deliveries = self
                .control
                .message_service()
                .historical_receive_through_subscribed(&recipient, cursor.last_sequence)
                .map_err(|error| MezError::invalid_state(error.to_string()))?;
            for delivery in deliveries {
                if !self
                    .received_peer_message_presentation_is_visible(&delivery.envelope.content_type)
                {
                    continue;
                }
                let identity = Self::received_peer_message_presentation_id(
                    &recipient,
                    delivery.sequence,
                    delivery.envelope.as_ref(),
                );
                if self.received_peer_message_presentation_is_persisted(
                    &conversation_id,
                    identity.as_str(),
                )? {
                    continue;
                }
                let direct_parent = self.runtime_peer_message_sender_is_direct_parent(
                    recipient.as_str(),
                    delivery.envelope.sender.agent_id.as_str(),
                );
                let peer_label = if direct_parent {
                    "parent".to_string()
                } else {
                    self.runtime_peer_message_endpoint_label(
                        delivery.envelope.sender.agent_id.as_str(),
                    )
                };
                if !self
                    .agent
                    .received_peer_message_presentations
                    .contains_key(&identity)
                    && candidate_identities.insert(identity.clone())
                {
                    candidates.push((
                        identity,
                        super::RuntimeReceivedPeerMessagePresentation {
                            recipient_agent_id: recipient.clone(),
                            pane_id: pane_id.clone(),
                            conversation_id: conversation_id.clone(),
                            turn_id: String::new(),
                            sequence: delivery.sequence,
                            content_type: delivery.envelope.content_type.clone(),
                            payload: crate::runtime::control::runtime_peer_message_logged_payload(
                                &delivery.envelope.payload,
                            ),
                            peer_label,
                            direct_parent,
                            presentation_eligible: self
                                .received_peer_message_presentation_is_visible(
                                    &delivery.envelope.content_type,
                                ),
                            presentation_attempted: false,
                        },
                    ));
                }
            }
        }
        if self
            .agent
            .received_peer_message_presentations
            .len()
            .saturating_add(candidates.len())
            > MAX_UNSETTLED_PEER_PRESENTATIONS
        {
            return Err(MezError::invalid_state(
                "legacy snapshot peer presentation reconstruction exceeds its receipt bound",
            ));
        }
        self.agent
            .received_peer_message_presentations
            .extend(candidates);
        Ok(())
    }

    /// Completes every committed receive presentation that survived a recoverable
    /// turn-start failure.
    ///
    /// The receipt is removed only after the renderer succeeds. This creates the
    /// small boundary shared by user-started and peer-triggered turns: context,
    /// receipt, cursor, and scheduler work may be revisited, but one accepted
    /// sequence/message identity can install at most one pane presentation row.
    pub(crate) fn flush_received_peer_message_presentations(&mut self) -> Result<()> {
        let pending = self
            .agent
            .received_peer_message_presentations
            .iter()
            .map(|(identity, receipt)| (identity.clone(), receipt.clone()))
            .collect::<Vec<_>>();
        #[cfg(test)]
        if !pending.is_empty() && self.take_received_peer_message_presentation_failure_for_tests() {
            return Err(MezError::invalid_state(
                "injected received peer-message presentation failure",
            ));
        }
        for (identity, receipt) in pending {
            let pane_owns_conversation = self
                .agent_shell_store()
                .get(&receipt.pane_id)
                .is_some_and(|session| {
                    !session.ephemeral && session.session_id == receipt.conversation_id
                });
            if !pane_owns_conversation {
                self.agent
                    .received_peer_message_presentations
                    .remove(&identity);
                continue;
            }
            let acknowledged = self
                .control
                .message_service()
                .subscription(&receipt.recipient_agent_id)
                .is_some_and(|cursor| cursor.last_sequence >= receipt.sequence);
            if !acknowledged {
                continue;
            }
            if !receipt.presentation_eligible {
                self.agent
                    .received_peer_message_presentations
                    .remove(&identity);
                continue;
            }
            if self.persistence.transcript_store().is_none() {
                if !receipt.presentation_attempted {
                    self.echo_received_peer_message_presentation_to_pane(
                        &receipt.pane_id,
                        identity.as_str(),
                        &receipt.peer_label,
                        receipt.direct_parent,
                        &receipt.content_type,
                        &receipt.payload,
                    )?;
                }
                self.agent
                    .received_peer_message_presentations
                    .remove(&identity);
                continue;
            }
            if self.received_peer_message_presentation_is_persisted(
                &receipt.conversation_id,
                identity.as_str(),
            )? {
                self.agent
                    .received_peer_message_presentations
                    .remove(&identity);
                continue;
            }
            if receipt.presentation_attempted {
                if !self
                    .persistence
                    .presentation_write_pending(&receipt.conversation_id)
                {
                    self.persist_received_peer_message_presentation_only(
                        &receipt.pane_id,
                        &receipt.conversation_id,
                        &receipt.turn_id,
                        &crate::runtime::render::PeerMessagePresentation {
                            receive_identity: Some(identity.as_str()),
                            peer_label: &receipt.peer_label,
                            content_type: Some(&receipt.content_type),
                            payload: &receipt.payload,
                            direct_parent: receipt.direct_parent,
                            presentation_eligible: receipt.presentation_eligible,
                        },
                    )?;
                    if self.received_peer_message_presentation_is_persisted(
                        &receipt.conversation_id,
                        identity.as_str(),
                    )? {
                        self.agent
                            .received_peer_message_presentations
                            .remove(&identity);
                    }
                }
                continue;
            }
            self.echo_received_peer_message_presentation_to_pane(
                &receipt.pane_id,
                identity.as_str(),
                &receipt.peer_label,
                receipt.direct_parent,
                &receipt.content_type,
                &receipt.payload,
            )?;
            if let Some(receipt) = self
                .agent
                .received_peer_message_presentations
                .get_mut(&identity)
            {
                receipt.presentation_attempted = true;
            }
            if !self.persistence.transcript_uses_adapter()
                && !self.received_peer_message_presentation_is_persisted(
                    &receipt.conversation_id,
                    identity.as_str(),
                )?
            {
                self.persist_received_peer_message_presentation_only(
                    &receipt.pane_id,
                    &receipt.conversation_id,
                    &receipt.turn_id,
                    &crate::runtime::render::PeerMessagePresentation {
                        receive_identity: Some(identity.as_str()),
                        peer_label: &receipt.peer_label,
                        content_type: Some(&receipt.content_type),
                        payload: &receipt.payload,
                        direct_parent: receipt.direct_parent,
                        presentation_eligible: receipt.presentation_eligible,
                    },
                )?;
            }
            if self.received_peer_message_presentation_is_persisted(
                &receipt.conversation_id,
                identity.as_str(),
            )? {
                self.agent
                    .received_peer_message_presentations
                    .remove(&identity);
            }
        }
        Ok(())
    }

    /// Reports whether one committed envelope is eligible for a receiver pane row.
    ///
    /// The receipt owner must use the same normal-versus-verbose media-type gate
    /// as the renderer. Suppressed payloads intentionally have neither a live
    /// row nor a durable presentation source, so retaining a receipt for them
    /// would create unbounded state that can never settle.
    fn received_peer_message_presentation_is_visible(&self, content_type: &str) -> bool {
        let log_mode = runtime_effective_config_value(self.integration.config_layers())
            .map(|value| runtime_agent_peer_message_log_mode_from_config(&value))
            .unwrap_or(PeerMessageLogMode::Normal);
        runtime_peer_message_presentation_is_visible(log_mode, Some(content_type))
    }

    /// Returns whether the receipt's delivery cursor confirms its transport commit.
    fn received_peer_message_presentation_is_acknowledged(
        &self,
        receipt: &super::RuntimeReceivedPeerMessagePresentation,
    ) -> bool {
        self.control
            .message_service()
            .subscription(&receipt.recipient_agent_id)
            .is_some_and(|cursor| cursor.last_sequence >= receipt.sequence)
    }

    /// Returns whether transport can still deliver an unacknowledged receipt.
    fn received_peer_message_presentation_is_transport_deliverable(
        &self,
        receipt: &super::RuntimeReceivedPeerMessagePresentation,
        now_ms: u64,
    ) -> bool {
        self.control
            .message_service()
            .fanout_ready_for(&receipt.recipient_agent_id, now_ms, usize::MAX)
            .ok()
            .flatten()
            .is_some_and(|fanout| {
                fanout
                    .batch
                    .messages
                    .iter()
                    .any(|message| message.sequence == receipt.sequence)
            })
    }

    /// Reports whether the durable presentation owner has settled one receipt.
    ///
    /// The transcript store is the durable authority for receiver-visible rows.
    /// An actor-side attempt never completes a receipt on its own: asynchronous
    /// persistence leaves it pending until the stored source proves settlement.
    fn received_peer_message_presentation_is_persisted(
        &self,
        conversation_id: &str,
        identity: &str,
    ) -> Result<bool> {
        let Some(store) = self.persistence.transcript_store() else {
            return Ok(false);
        };
        Ok(store
            .inspect_presentation(conversation_id)?
            .iter()
            .any(|entry| {
                entry
                    .source_text
                    .as_deref()
                    .and_then(|source_text| {
                        entry
                            .source_content_type
                            .as_deref()
                            .and_then(|content_type| {
                                crate::runtime::render::peer_message_presentation_receive_identity(
                                    content_type,
                                    source_text,
                                )
                            })
                    })
                    .is_some_and(|source_identity| source_identity == identity)
            }))
    }

    /// Completes attempted receipts only after the presentation worker confirms
    /// that their durable conversation log contains the receipt identity.
    pub(crate) fn settle_received_peer_message_presentations(
        &mut self,
        conversation_id: &str,
    ) -> Result<()> {
        let identities = self
            .agent
            .received_peer_message_presentations
            .iter()
            .filter(|(_, receipt)| {
                receipt.conversation_id == conversation_id && receipt.presentation_attempted
            })
            .filter_map(|(identity, receipt)| {
                self.received_peer_message_presentation_is_persisted(
                    &receipt.conversation_id,
                    identity,
                )
                .ok()
                .filter(|persisted| *persisted)
                .map(|_| identity.clone())
            })
            .collect::<Vec<_>>();
        for identity in identities {
            self.agent
                .received_peer_message_presentations
                .remove(&identity);
        }
        Ok(())
    }

    /// Drops receipt state owned by a pane that has retired from the session.
    ///
    /// A closed pane can never display or replay another receiver row. Pending
    /// and completed identities are therefore removed together, keeping the
    /// in-memory idempotency set bounded by live pane ownership.
    pub(crate) fn clear_received_peer_message_presentations_for_pane(&mut self, pane_id: &str) {
        let identities = self
            .agent
            .received_peer_message_presentations
            .iter()
            .filter(|(_, receipt)| receipt.pane_id == pane_id)
            .map(|(identity, _)| identity.clone())
            .collect::<Vec<_>>();
        for identity in identities {
            self.agent
                .received_peer_message_presentations
                .remove(&identity);
        }
    }

    /// Retires receipts bound to an old conversation before a pane renders a replacement.
    ///
    /// Once a pane leaves a conversation, its unresolved rows can no longer be
    /// rendered or durably retried through that pane. Removing them prevents an
    /// old receipt from crossing the conversation ownership boundary.
    pub(crate) fn clear_received_peer_message_presentations_for_conversation(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
    ) {
        self.agent
            .received_peer_message_presentations
            .retain(|_, receipt| {
                receipt.pane_id != pane_id || receipt.conversation_id != conversation_id
            });
    }

    /// Prunes completed receipt identities whose owning turn has reached a
    /// terminal state while retaining unresolved persistence receipts.
    pub(crate) fn clear_completed_received_peer_message_presentations_for_turn(
        &mut self,
        turn_id: &str,
    ) {
        let identities = self
            .agent
            .received_peer_message_presentations
            .iter()
            .filter(|(_, receipt)| receipt.turn_id == turn_id)
            .filter_map(|(identity, receipt)| {
                self.received_peer_message_presentation_is_persisted(
                    &receipt.conversation_id,
                    identity,
                )
                .ok()
                .filter(|persisted| *persisted)
                .map(|_| identity.clone())
            })
            .collect::<Vec<_>>();
        for identity in identities {
            self.agent
                .received_peer_message_presentations
                .remove(&identity);
        }
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
    /// Spawn-owned lineage preserves generated names and literal-name mode
    /// exactly. Selectors and unavailable lineage retain their provider-supplied
    /// recipient expression without affecting routing or authority.
    pub(crate) fn runtime_outbound_recipient_display_label(
        &self,
        recipient: &crate::runtime::Recipient,
        fallback: &str,
    ) -> String {
        let crate::runtime::Recipient::Agent(agent_id) = recipient else {
            return fallback.to_string();
        };
        self.subagent_lineage(agent_id.as_str())
            .map(|lineage| lineage.display_name.trim())
            .filter(|display_name| !display_name.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| fallback.to_string())
    }

    /// Resolves a peer label from its live pane title without exposing metadata
    /// beyond the endpoint's already-authorized runtime identity.
    pub(crate) fn runtime_peer_message_endpoint_label(&self, agent_id: &str) -> String {
        let Some(pane_id) = agent_id.strip_prefix("agent-") else {
            return agent_id.to_string();
        };
        self.session
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .find(|pane| pane.id.as_str() == pane_id)
            .map(|pane| pane.title.trim())
            .filter(|title| !title.is_empty())
            .map(str::to_string)
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
        let agent_id = format!("agent-{pane_id}");
        if self.agent_shell_store().get(pane_id).is_none() {
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
            context,
            delivered_message_sequence,
            delivered_messages,
            delivered_message_count,
            imported_history_events,
        } = self.peer_message_turn_context(pane_id, now_ms)?;
        let Some(delivered_message_sequence) = delivered_message_sequence else {
            return Ok(0);
        };
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
        self.set_agent_turn_imported_history_events(turn_id.clone(), imported_history_events);
        self.set_agent_turn_model_profile(turn_id.clone(), model_profile);
        for (sequence, envelope) in delivered_messages.iter().cloned() {
            self.register_received_peer_message_presentation(
                recipient.clone(),
                sequence,
                &turn,
                envelope,
            );
        }
        #[cfg(test)]
        if self.take_peer_message_receive_after_context_storage_failure_for_tests() {
            return Err(MezError::invalid_state(
                "injected peer-message idle-turn failure after context storage",
            ));
        }
        self.control
            .message_service_mut()
            .advance_subscription(&recipient, delivered_message_sequence)?;
        #[cfg(test)]
        if self.take_peer_message_receive_after_cursor_advance_failure_for_tests() {
            return Err(MezError::invalid_state(
                "injected peer-message idle-turn failure after cursor advancement",
            ));
        }
        self.set_agent_peer_message_turn_count(&agent_id, started_turns.saturating_add(1));
        self.clear_agent_peer_message_limit_reported(&agent_id);
        if !self.agent_work_is_scheduled(&turn_id) {
            self.enqueue_agent_work(ScheduledWork {
                turn_id: turn_id.clone(),
                conversation_id,
                agent_id: agent_id.clone(),
                pane_id: Some(pane_id.to_string()),
                kind: mez_agent::ScheduledWorkKind::ShellCapable,
            })?;
        }
        self.start_ready_agent_turns()?;
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
        Ok(delivered_message_count)
    }

    /// Returns whether any recipient currently has deliverable peer mail.
    ///
    /// Mail that is waiting only on the peer-message loop limit is not
    /// deliverable: the limit is a stable terminal state for that agent until
    /// direct user input resets the counter, so it must not keep re-arming the
    /// delivery timer (and re-reporting the limit) once per tick. Runtime-owned
    /// bridge traffic never starts a turn, so it keeps the ordinary pending
    /// behavior that also drives TTL expiry.
    pub(crate) fn has_pending_peer_messages(&mut self, now_ms: u64) -> bool {
        if self
            .agent
            .received_peer_message_presentations
            .values()
            .any(|receipt| {
                self.received_peer_message_presentation_is_transport_deliverable(receipt, now_ms)
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
        let ready = self.control.message_service_mut().fanout_ready(now_ms, 1);
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
                    && matches!(
                        turn.state,
                        AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
                    )
            })
            .cloned()
    }

    /// Builds the desired dedicated peer-message delivery timer transition.
    ///
    /// The dedicated timer is armed only while deliverable peer mail exists and
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
            applied: committed > 0,
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
        let delivery = match self.control.message_service_mut().accept_at_with_scope(
            &sender.agent_id,
            envelope,
            message_scope,
            now_ms,
        ) {
            Ok(delivery) => delivery,
            Err(error) => {
                let error = MezError::from(error);
                let mut result = ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    "transport_error",
                    error.message().to_string(),
                )?;
                result.structured_content_json = Some(format!(
                    r#"{{"recipient":"{}","scope":"{}","message_id":null,"delivery_status":"failed","protocol_error":{{"code":"{}","message":"{}"}}}}"#,
                    json_escape(recipient),
                    scope,
                    runtime_mezzanine_error_code(error.kind()),
                    json_escape(error.message())
                ));
                return Ok(result);
            }
        };
        self.deliver_pending_runtime_agent_messages(now_ms)?;
        let result = ActionResult::succeeded(
            turn,
            action,
            vec![format!(
                "message {} delivered to {} recipient(s)",
                delivery.message_id, delivery.queued_recipients
            )],
            Some(format!(
                r#"{{"recipient":"{}","scope":"{}","message_id":"{}","delivery_status":"accepted","queued_recipients":{},"sequence":{},"protocol_error":null}}"#,
                json_escape(recipient),
                scope,
                json_escape(&delivery.message_id),
                delivery.queued_recipients,
                delivery.sequence
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

    /// Persists one bounded display mirror of a published agent objective.
    ///
    /// The mirror exists so archived and offline conversations can still resolve
    /// a policy-derived title. It is written only from the published objective
    /// value, is bounded by the shared title rules, and never authorizes
    /// anything. The mirror is a cache: a missing, unchanged, or unreadable
    /// mirror is not an error and never fails a turn. A title change also
    /// refreshes the cached prompt-selector candidates and any open saved-session
    /// browser so every surface renders the same row title.
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
        if self.runtime_agent_conversation_is_ephemeral(conversation_id) {
            return false;
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return false;
        };
        let changed = store
            .mirror_session_objective(
                conversation_id,
                objective.unwrap_or_default(),
                current_unix_seconds(),
            )
            .unwrap_or(false);
        if changed {
            self.invalidate_agent_prompt_selector_extra_candidates();
            let _ = self.refresh_saved_session_overlay_after_title_change();
        }
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
