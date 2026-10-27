//! Receiver-owned native commit evidence and presentation recovery.
//!
//! Admission reserves receipt resources before canonical receive commit. Sealed
//! same-runtime evidence survives context lifecycle changes until exact transport
//! settlement; restored display sources never acquire that authority. Display
//! eligibility and persistence retries do not change custody or task ownership.

use super::*;
use crate::runtime::{
    PeerMessageLogMode, runtime_agent_peer_message_log_mode_from_config,
    runtime_peer_message_presentation_is_visible,
};
use crate::storage::snapshot::{
    MAX_UNSETTLED_PEER_PRESENTATION_BYTES, MAX_UNSETTLED_PEER_PRESENTATION_PAYLOAD_BYTES,
    MAX_UNSETTLED_PEER_PRESENTATIONS, peer_presentation_source_bytes,
};

impl RuntimeSessionService {
    /// Renders one retained receiver source without consulting transport mail.
    ///
    /// Persistence retries and snapshot recovery reuse the source captured at
    /// receive commit. They must not derive labels or payloads from the
    /// retention-bounded MMP queue, and they may call this only before the
    /// one-time live row has been installed.
    fn echo_received_peer_message_presentation_to_pane(
        &mut self,
        receipt: &super::super::RuntimeReceivedPeerMessagePresentation,
        receive_identity: &str,
    ) -> Result<()> {
        let entry = mez_core::presentation::PresentationEntry {
            id: receive_identity.to_string(),
            occurrence: receipt.occurrence,
            first_row: true,
        };
        self.with_presentation_entry(&receipt.pane_id, entry, |service| {
            if receipt.direct_parent {
                service.append_committed_received_direct_parent_message_to_terminal_buffer(
                    &receipt.pane_id,
                    receive_identity,
                    &receipt.content_type,
                    &receipt.payload,
                )
            } else {
                service.append_committed_received_peer_message_to_terminal_buffer(
                    &receipt.pane_id,
                    receive_identity,
                    &receipt.peer_label,
                    &receipt.content_type,
                    &receipt.payload,
                )
            }
        })
    }

    /// Returns the stable receiver-owned identity for one accepted MMP delivery.
    ///
    /// Payload text is deliberately absent: two accepted envelopes can carry the
    /// same text and each must retain a distinct presentation row, while a retry
    /// of one accepted envelope must reuse the exact same receipt.
    pub(crate) fn received_peer_message_presentation_id(
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
    /// The live outbox shares the snapshot's 1,024-record and 16 MiB logical
    /// source byte bounds.
    /// Invisible media also retains a native commit proof and counts toward this
    /// bound. A new delivery is not acknowledged when its receipt cannot be retained,
    /// preserving lossless backpressure rather than dropping a receiver row.
    pub(super) fn can_admit_received_peer_message_presentation(
        &self,
        recipient: &AgentId,
        sequence: mez_agent::messaging::MessageSequence,
        envelope: &Envelope,
        turn: &AgentTurnRecord,
    ) -> bool {
        self.can_reserve_peer_receipts(recipient, turn, std::iter::once((sequence, envelope)))
    }

    /// Reserves exact planned turn/source copies before canonical batch commit.
    /// Invisible media retains commit evidence and pays the same byte charge.
    pub(crate) fn can_admit_peer_receipts_for_turn(
        &self,
        recipient: &AgentId,
        turn: &AgentTurnRecord,
        deliveries: &[(mez_agent::messaging::MessageSequence, Envelope)],
    ) -> bool {
        self.can_reserve_peer_receipts(
            recipient,
            turn,
            deliveries
                .iter()
                .map(|(sequence, envelope)| (*sequence, envelope)),
        )
    }

    /// Checks unique incoming receipts against both live bounds without cloning
    /// payloads. Truncated payloads conservatively reserve their full byte cap.
    fn can_reserve_peer_receipts<'a>(
        &self,
        recipient: &AgentId,
        turn: &AgentTurnRecord,
        deliveries: impl Iterator<Item = (mez_agent::messaging::MessageSequence, &'a Envelope)>,
    ) -> bool {
        let mut bytes = self.peer_receipt_bytes();
        let mut new_identities = BTreeSet::new();
        for (sequence, envelope) in deliveries {
            let identity =
                Self::received_peer_message_presentation_id(recipient, sequence, envelope);
            if self
                .agent
                .received_peer_message_presentations
                .contains_key(&identity)
                || !new_identities.insert(identity.clone())
            {
                continue;
            }
            let direct_parent = self.runtime_peer_message_sender_is_direct_parent(
                recipient.as_str(),
                envelope.sender.agent_id.as_str(),
            );
            let label = if direct_parent {
                "parent".to_string()
            } else {
                self.runtime_peer_message_endpoint_label(envelope.sender.agent_id.as_str())
            };
            bytes = bytes.saturating_add(peer_presentation_source_bytes([
                identity.len(),
                recipient.as_str().len(),
                turn.pane_id.len(),
                turn.conversation_id.len(),
                turn.turn_id.len(),
                label.len(),
                envelope.content_type.len(),
                envelope
                    .payload
                    .len()
                    .min(MAX_UNSETTLED_PEER_PRESENTATION_PAYLOAD_BYTES),
            ]));
            if bytes > MAX_UNSETTLED_PEER_PRESENTATION_BYTES
                || self
                    .agent
                    .received_peer_message_presentations
                    .len()
                    .saturating_add(new_identities.len())
                    > MAX_UNSETTLED_PEER_PRESENTATIONS
            {
                return false;
            }
        }
        bytes <= MAX_UNSETTLED_PEER_PRESENTATION_BYTES
    }

    /// Returns the current immutable live source charge, including invisible proof.
    fn peer_receipt_bytes(&self) -> usize {
        self.agent
            .received_peer_message_presentations
            .iter()
            .fold(0usize, |bytes, (identity, row)| {
                bytes.saturating_add(row.logical_bytes(identity))
            })
    }

    /// Returns whether an idle receive batch fits the early count reservation.
    /// Exact planned-turn bytes are checked again before canonical mutation.
    pub(crate) fn can_admit_received_peer_message_presentation_batch(
        &self,
        recipient: &AgentId,
        deliveries: &[(mez_agent::messaging::MessageSequence, Envelope)],
    ) -> bool {
        let new_identities = deliveries
            .iter()
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
        if let Some(receipt) = self
            .agent
            .received_peer_message_presentations
            .get_mut(&identity)
        {
            receipt.native_commit_proven = true;
            return;
        }
        let occurrence = self.capture_presentation_occurrence(
            mez_core::presentation::PresentationOccurrenceBasis::LocalAcceptance,
        );
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
            .or_insert(super::super::RuntimeReceivedPeerMessagePresentation {
                native_commit_proven: true,
                recipient_agent_id,
                occurrence,
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
                    occurrence: receipt.occurrence,
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
        let mut bytes = self.peer_receipt_bytes();
        let mut identities = BTreeSet::new();
        for entry in outbox {
            if !self
                .agent
                .received_peer_message_presentations
                .contains_key(&entry.identity)
                && identities.insert(entry.identity.as_str())
            {
                bytes = bytes.saturating_add(entry.logical_bytes());
            }
        }
        if bytes > MAX_UNSETTLED_PEER_PRESENTATION_BYTES
            || self
                .agent
                .received_peer_message_presentations
                .len()
                .saturating_add(identities.len())
                > MAX_UNSETTLED_PEER_PRESENTATIONS
        {
            return Err(MezError::invalid_state(
                "restored peer receipts exceed count or byte capacity",
            ));
        }
        for entry in outbox {
            let recipient_agent_id =
                AgentId::opaque(entry.recipient_agent_id.clone()).ok_or_else(|| {
                    MezError::invalid_state("snapshot peer presentation recipient is invalid")
                })?;
            self.agent
                .received_peer_message_presentations
                .entry(entry.identity.clone())
                .or_insert(super::super::RuntimeReceivedPeerMessagePresentation {
                    native_commit_proven: false,
                    recipient_agent_id,
                    occurrence: entry.occurrence,
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
        if outbox.len() > MAX_UNSETTLED_PEER_PRESENTATIONS
            || outbox.iter().fold(0usize, |bytes, row| {
                bytes.saturating_add(row.logical_bytes())
            }) > MAX_UNSETTLED_PEER_PRESENTATION_BYTES
        {
            return Err(MezError::invalid_state(
                "snapshot peer receipts exceed count or byte capacity",
            ));
        }
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
        let mut candidate_bytes = self.peer_receipt_bytes();
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
                    candidate_bytes =
                        candidate_bytes.saturating_add(peer_presentation_source_bytes([
                            identity.len(),
                            recipient.as_str().len(),
                            pane_id.len(),
                            conversation_id.len(),
                            0,
                            peer_label.len(),
                            delivery.envelope.content_type.len(),
                            delivery
                                .envelope
                                .payload
                                .len()
                                .min(MAX_UNSETTLED_PEER_PRESENTATION_PAYLOAD_BYTES),
                        ]));
                    if candidate_bytes > MAX_UNSETTLED_PEER_PRESENTATION_BYTES
                        || self
                            .agent
                            .received_peer_message_presentations
                            .len()
                            .saturating_add(candidates.len())
                            .saturating_add(1)
                            > MAX_UNSETTLED_PEER_PRESENTATIONS
                    {
                        return Err(MezError::invalid_state(
                            "legacy snapshot peer presentation reconstruction exceeds count or byte capacity",
                        ));
                    }
                    candidates.push((
                        identity,
                        super::super::RuntimeReceivedPeerMessagePresentation {
                            native_commit_proven: false,
                            recipient_agent_id: recipient.clone(),
                            occurrence: Default::default(),
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
            let acknowledged = self.received_peer_message_presentation_is_acknowledged(&receipt);
            if !pane_owns_conversation {
                if receipt.native_commit_proven && !acknowledged {
                    continue;
                }
                self.agent
                    .received_peer_message_presentations
                    .remove(&identity);
                continue;
            }
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
                        &receipt,
                        identity.as_str(),
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
                    let entry = mez_core::presentation::PresentationEntry {
                        id: identity.clone(),
                        occurrence: receipt.occurrence,
                        first_row: true,
                    };
                    self.with_presentation_entry(&receipt.pane_id, entry, |service| {
                        service.persist_received_peer_message_presentation_only(
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
                        )
                    })?;
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
            self.echo_received_peer_message_presentation_to_pane(&receipt, identity.as_str())?;
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
    pub(super) fn received_peer_message_presentation_is_acknowledged(
        &self,
        receipt: &super::super::RuntimeReceivedPeerMessagePresentation,
    ) -> bool {
        self.control
            .message_service()
            .subscription(&receipt.recipient_agent_id)
            .is_some_and(|cursor| cursor.last_sequence >= receipt.sequence)
    }

    /// Returns whether transport can still deliver an unacknowledged receipt.
    pub(super) fn received_peer_message_presentation_is_transport_deliverable(
        &self,
        receipt: &super::super::RuntimeReceivedPeerMessagePresentation,
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
                receipt.conversation_id == conversation_id
                    && receipt.presentation_attempted
                    && self.received_peer_message_presentation_is_acknowledged(receipt)
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
    /// A closed pane cannot display or replay another receiver row. Keep only
    /// sealed unacknowledged native commit proof until the cursor reconciles or
    /// exact registration retirement commits it; these rows cannot repaint a
    /// replacement conversation. The existing receipt and custody caps apply.
    pub(crate) fn clear_received_peer_message_presentations_for_pane(&mut self, pane_id: &str) {
        let identities = self
            .agent
            .received_peer_message_presentations
            .iter()
            .filter(|(_, receipt)| {
                receipt.pane_id == pane_id
                    && (!receipt.native_commit_proven
                        || self.received_peer_message_presentation_is_acknowledged(receipt))
            })
            .map(|(identity, _)| identity.clone())
            .collect::<Vec<_>>();
        for receipt in self
            .agent
            .received_peer_message_presentations
            .values_mut()
            .filter(|receipt| receipt.pane_id == pane_id && receipt.native_commit_proven)
        {
            receipt.presentation_eligible = false;
        }
        for identity in identities {
            self.agent
                .received_peer_message_presentations
                .remove(&identity);
        }
    }

    /// Retires receipts bound to an old conversation before a pane renders a replacement.
    ///
    /// Once a pane leaves a conversation, its unresolved rows can no longer be
    /// rendered or durably retried through that pane. Drop display-only rows,
    /// but retain sealed unacknowledged native proof until transport settlement.
    /// Flush never renders that proof through the replacement conversation.
    pub(crate) fn clear_received_peer_message_presentations_for_conversation(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
    ) {
        let protected = self
            .agent
            .received_peer_message_presentations
            .iter()
            .filter(|(_, receipt)| {
                receipt.native_commit_proven
                    && !self.received_peer_message_presentation_is_acknowledged(receipt)
            })
            .map(|(identity, _)| identity.clone())
            .collect::<BTreeSet<_>>();
        self.agent
            .received_peer_message_presentations
            .retain(|identity, receipt| {
                receipt.pane_id != pane_id
                    || receipt.conversation_id != conversation_id
                    || protected.contains(identity)
            });
        for receipt in self
            .agent
            .received_peer_message_presentations
            .values_mut()
            .filter(|receipt| {
                receipt.pane_id == pane_id
                    && receipt.conversation_id == conversation_id
                    && receipt.native_commit_proven
            })
        {
            receipt.presentation_eligible = false;
        }
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
            .filter(|(_, receipt)| {
                receipt.turn_id == turn_id
                    && self.received_peer_message_presentation_is_acknowledged(receipt)
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
    }
}

impl super::super::RuntimeReceivedPeerMessagePresentation {
    /// Charges exactly the source strings retained by a live receiver receipt.
    fn logical_bytes(&self, identity: &str) -> usize {
        peer_presentation_source_bytes([
            identity.len(),
            self.recipient_agent_id.as_str().len(),
            self.pane_id.len(),
            self.conversation_id.len(),
            self.turn_id.len(),
            self.peer_label.len(),
            self.content_type.len(),
            self.payload.len(),
        ])
    }
}
