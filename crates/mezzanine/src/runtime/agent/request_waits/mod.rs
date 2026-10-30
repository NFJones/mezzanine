//! Exact request ownership and context-first targeted MMP wait handoff.
//!
//! Lower receipts reserve finite retention before acceptance. This adapter adds
//! live native caller and child-attempt evidence within that headroom, selects
//! one immutable recipient per wait, and retains outcomes until context and fair
//! wake admission commit. Neither correlation text nor lifecycle bridge flags
//! establish ownership. Ordinary mail can resume inference without consuming an
//! explicit edge. Restore discards these capabilities rather than replaying work.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use super::{
    ActionResult, ActionStatus, AgentActionPayload, AgentTurnExecution, AgentTurnRecord,
    AgentTurnState, Result, RuntimeSessionService,
};
use mez_agent::messaging::{
    Delivery, Envelope, MessageRequestOrigin, MessageRequests, MessageScope,
};

/// Maximum bytes for one complete captured child identity, leaving bounded
/// diagnostic and container headroom inside the lower6144-byte reservation.
const TASK_IDENTITY_BYTES: usize = 512;

/// Fixed-size original parent root/process evidence. Unknown OS observation is
/// distinct from a legitimately process-free pane; executable text is absent.
#[derive(Debug)]
struct ParentAnchor {
    /// Runtime-owned root PID, adapter epoch and independent kernel lifetime.
    root: Option<RootAnchor>,
    /// Observed process role, PID, adapter generation and kernel start token.
    process: std::result::Result<
        ObservedProcess,
        crate::runtime::processes::RuntimePaneProcessIdentityUnavailable,
    >,
    /// Proven non-live pane with no owned root, never inferred from OS errors.
    process_free: bool,
}

/// Independently observed native root incarnation, without executable content.
#[derive(Debug, Clone, Copy)]
struct RootAnchor {
    /// Exact manager-owned root PID.
    pid: u32,
    /// Adapter handoff epoch, when one exists.
    generation: Option<u64>,
    /// Independently observed kernel lifetime, when readable.
    token: Option<u64>,
}

/// Fixed-size kernel observation used only for native incarnation comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservedProcess {
    /// Native root versus foreground fallback are distinct observations.
    role: crate::runtime::processes::RuntimePaneProcessRole,
    /// Actual process whose kernel lifetime was verified.
    pid: u32,
    /// Exact adapter generation, if applicable.
    generation: Option<u64>,
    /// Kernel lifetime verified alongside executable observation.
    token: u64,
}

impl From<crate::runtime::processes::RuntimePaneProcessIdentity> for ObservedProcess {
    /// Drops executable text after preserving only exact incarnation evidence.
    fn from(process: crate::runtime::processes::RuntimePaneProcessIdentity) -> Self {
        Self {
            role: process.role,
            pid: process.process_id,
            generation: process.generation,
            token: process.start_token,
        }
    }
}

/// Exact child task known live at send acceptance, never a later task lookup.
#[derive(Debug, Clone)]
struct TaskAttempt {
    /// Immutable task identity and incarnation.
    turn: String,
    /// Original child conversation, independent of subsequent persistent tasks.
    conversation: String,
    /// Exact task creation generation alongside its unique native ID.
    started: u64,
}

/// First authoritative outcome for one accepted recipient. No payload mailbox.
#[derive(Debug, Clone)]
enum Outcome {
    /// Authenticated response already committed as ordinary canonical mail.
    Reply(u64),
    /// Exact owned task terminal result, retained before best-effort bridge work.
    Terminal { content: String },
    /// Existing native failure custody owns the content; retain only its key.
    FailureReference { label: String, child: String },
    /// The captured registration ended; private peer work remains undisclosed.
    Unavailable,
}

/// Reserved per-recipient handoff slot in frozen acceptance order.
#[derive(Debug)]
struct RecipientState {
    /// Optional locally owned task proven at send acceptance.
    task: Option<TaskAttempt>,
    /// First authenticated response or authoritative lifecycle outcome.
    outcome: Option<Outcome>,
    /// Another outstanding request already consumed this same captured task.
    terminal_consumed: bool,
    /// Exact canonical response source protected until context/wake handoff.
    peer_source: Option<(String, mez_agent::ContextEventSequence)>,
    /// Typed handoff reference protected across QueueFull and context retries.
    handoff_source: Option<(String, mez_agent::ContextEventSequence)>,
}

/// One selected dependency for an exact native waiting task.
#[derive(Debug, Clone)]
struct Edge {
    /// Local dependency generation, not a progress witness by itself.
    generation: u64,
    /// One selected original recipient index, never a live selector.
    recipient: usize,
    /// Exact waiting task incarnation.
    started: u64,
}

/// Same-runtime owner, cleared on restore and original conversation retirement.
#[derive(Debug)]
pub(super) struct RuntimeRequestWaits {
    /// Fresh source namespace on construction/restore. Local counters alone
    /// cannot identify references in persisted conversation history.
    label_epoch: [u8; 32],
    /// Bounded lower accepted-send receipts and reserved resource accounting.
    requests: MessageRequests,
    /// Bounded per-recipient task/outcome data charged to those reservations.
    recipients: BTreeMap<u64, Vec<RecipientState>>,
    /// At most one selected edge per waiting turn; unchanged reparks reuse it.
    edges: BTreeMap<String, Edge>,
    /// Fixed-size process evidence charged to reserved receipt overhead.
    anchors: BTreeMap<u64, ParentAnchor>,
    /// Fixed-size immutable native intent identity for safe partial-effect retry.
    fingerprints: BTreeMap<u64, [u8; 32]>,
}

impl Default for RuntimeRequestWaits {
    /// Creates volatile ownership with a fresh, content-free source namespace.
    fn default() -> Self {
        Self {
            label_epoch: rand::random(),
            requests: MessageRequests::default(),
            recipients: BTreeMap::new(),
            edges: BTreeMap::new(),
            anchors: BTreeMap::new(),
            fingerprints: BTreeMap::new(),
        }
    }
}

impl RuntimeSessionService {
    /// Captures native process/root tokens without retaining executable paths.
    fn capture_request_parent_anchor(&self, turn: &AgentTurnRecord) -> ParentAnchor {
        let root = self
            .primary_pid_for_live_pane_process(&turn.pane_id)
            .map(|pid| RootAnchor {
                pid,
                generation: self
                    .adapter_owned_pane_process_instance(&turn.pane_id)
                    .map(|instance| instance.generation),
                token: mez_mux::process::process_start_token_for_pid(pid),
            });
        let process_free = root.is_none()
            && super::runtime_pane_by_id(&self.session, &turn.pane_id)
                .is_ok_and(|(_, pane)| !pane.live);
        let process = self
            .pane_process_identity(&turn.pane_id)
            .map(ObservedProcess::from);
        ParentAnchor {
            root,
            process,
            process_free,
        }
    }

    /// Revalidates original OS incarnation before lifecycle-driven admission.
    /// Uncertainty holds the edge; a replacement process cannot inherit it.
    fn request_parent_anchor_is_current(
        &self,
        turn: &AgentTurnRecord,
        anchor: &ParentAnchor,
    ) -> bool {
        if let Some(root) = anchor.root
            && (self.primary_pid_for_live_pane_process(&turn.pane_id) != Some(root.pid)
                || root.generation.is_some()
                    && self
                        .adapter_owned_pane_process_instance(&turn.pane_id)
                        .map(|instance| instance.generation)
                        != root.generation
                || root.token.is_some()
                    && mez_mux::process::process_start_token_for_pid(root.pid) != root.token)
        {
            return false;
        }
        if anchor.process_free {
            return self
                .primary_pid_for_live_pane_process(&turn.pane_id)
                .is_none()
                && super::runtime_pane_by_id(&self.session, &turn.pane_id)
                    .is_ok_and(|(_, pane)| !pane.live);
        }
        let Ok(current) = self.pane_process_identity(&turn.pane_id) else {
            return false;
        };
        if let Ok(expected) = anchor.process {
            return expected == ObservedProcess::from(current);
        }
        anchor.root.is_some_and(|root| {
            (root.generation.is_some() || root.token.is_some())
                && current.process_id == root.pid
                && root
                    .generation
                    .is_none_or(|value| current.generation == Some(value))
                && root.token.is_none_or(|value| current.start_token == value)
        })
    }

    /// Captures the caller's actual native identity, not model-supplied fields.
    fn request_origin(turn: &AgentTurnRecord) -> Result<MessageRequestOrigin> {
        Ok(MessageRequestOrigin {
            agent_id: super::AgentId::opaque(turn.agent_id.clone()).ok_or_else(|| {
                super::MezError::invalid_state("native request sender identity is invalid")
            })?,
            conversation_id: turn.conversation_id.clone(),
            turn_id: turn.turn_id.clone(),
            started_at_unix_seconds: turn.started_at_unix_seconds,
        })
    }

    /// Proves current ownership through lineage and the indexed live child task.
    /// Missing live work is reply/registration-only, never adopted from history.
    fn capture_request_task(
        &self,
        parent: &AgentTurnRecord,
        recipient: &str,
    ) -> Option<TaskAttempt> {
        let lineage = self.subagent_lineage(recipient)?;
        if lineage.parent_agent_id != parent.agent_id
            || !self.subagent_lineage_has_live_parent_authority(recipient)
        {
            return None;
        }
        let pane = recipient.strip_prefix("agent-")?;
        let child = self.agent_turn_ledger().latest_turn_for_pane(pane)?;
        let conversation = self
            .persistent_subagent(recipient)
            .map(|owner| owner.parent_conversation_id.as_str())
            .or_else(|| {
                child
                    .parent_turn_id
                    .as_deref()
                    .and_then(|id| self.agent_turn_ledger().turn(id))
                    .map(|turn| turn.conversation_id.as_str())
            });
        if conversation != Some(parent.conversation_id.as_str())
            || self.terminal_result_disposition(&child.turn_id).is_some()
            || self
                .agent
                .settled_subagent_result_turns
                .contains(&child.turn_id)
            || self
                .persistent_subagent(recipient)
                .is_some_and(|owner| owner.conversation_id != child.conversation_id)
            || child.agent_id != recipient
            || !matches!(
                child.state,
                AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
            )
            || self
                .agent_shell_store()
                .get(pane)
                .is_none_or(|shell| shell.session_id != child.conversation_id)
        {
            return None;
        }
        Some(TaskAttempt {
            turn: child.turn_id.clone(),
            conversation: child.conversation_id.clone(),
            started: child.started_at_unix_seconds,
        })
    }

    /// Reserves and accepts a tracked native send under one exclusive actor
    /// transaction. Preflight task metadata is bounded before any accepted effect;
    /// no delivery, provider work, or actor callback interleaves with capture.
    pub(super) fn accept_tracked_runtime_message(
        &mut self,
        turn: &AgentTurnRecord,
        envelope: Envelope,
        scope: MessageScope,
        now_ms: u64,
    ) -> mez_agent::messaging::Result<Delivery> {
        self.prune_request_wait_owners();
        let fingerprint = request_fingerprint(&envelope, scope);
        let origin = Self::request_origin(turn).map_err(|_| {
            mez_agent::messaging::MessageError::invalid_state("native request origin unavailable")
        })?;
        if let Some(receipt) = self.agent.request_waits.requests.find(
            &origin.agent_id,
            &origin.conversation_id,
            &envelope.id,
        ) {
            if receipt.origin() == &origin
                && self
                    .agent
                    .request_waits
                    .fingerprints
                    .get(&receipt.generation())
                    == Some(&fingerprint)
                && self
                    .control
                    .message_service()
                    .acceptance_sender_is_current(receipt.proof())
            {
                return Ok(receipt.delivery().clone());
            }
            return Err(mez_agent::messaging::MessageError::conflict(
                "tracked send belongs to another original owner",
            ));
        }
        let recipients = self
            .control
            .message_service()
            .preview_message_recipients(&envelope, scope)?;
        let limits = self.agent.request_waits.requests.limits();
        let (_, used_units, used_bytes) = self.agent.request_waits.requests.usage();
        let origin_count = self
            .agent
            .request_waits
            .requests
            .receipts()
            .filter(|receipt| {
                receipt.origin().agent_id == origin.agent_id
                    && receipt.origin().conversation_id == origin.conversation_id
            })
            .count();
        let remaining = limits.units.saturating_sub(used_units);
        if origin_count >= limits.per_conversation
            || remaining < 4
            || recipients.len() > (remaining - 2) / 2
            || recipients
                .len()
                .saturating_mul(mez_agent::messaging::MESSAGE_REQUEST_HANDOFF_BYTES)
                .saturating_add(512)
                > limits.metadata_bytes.saturating_sub(used_bytes)
        {
            return Err(mez_agent::messaging::MessageError::new(
                mez_agent::messaging::MessageErrorKind::Capacity,
                "tracked request preflight capacity exceeded",
            ));
        }
        let mut slots = Vec::with_capacity(recipients.len());
        for recipient in &recipients {
            let task = self.capture_request_task(turn, recipient.as_str());
            if task
                .as_ref()
                .is_some_and(|task| task.turn.len() + task.conversation.len() > TASK_IDENTITY_BYTES)
            {
                return Err(mez_agent::messaging::MessageError::new(
                    mez_agent::messaging::MessageErrorKind::Capacity,
                    "tracked task identity metadata capacity exceeded",
                ));
            }
            slots.push(RecipientState {
                task,
                outcome: None,
                terminal_consumed: false,
                peer_source: None,
                handoff_source: None,
            });
        }
        let anchor = self.capture_request_parent_anchor(turn);
        drop(recipients);
        let receipt = self.agent.request_waits.requests.accept(
            self.control.message_service_mut(),
            origin,
            envelope,
            scope,
            now_ms,
        )?;
        let generation = receipt.generation();
        let delivery = receipt.delivery().clone();
        // No actor callback or mutation of message registration occurred between
        // preview and acceptance; both use the same immutable eligible ordering.
        self.agent
            .request_waits
            .recipients
            .insert(generation, slots);
        self.agent.request_waits.anchors.insert(generation, anchor);
        self.agent
            .request_waits
            .fingerprints
            .insert(generation, fingerprint);
        Ok(delivery)
    }

    /// Selects only an actual outstanding tracked request in this conversation.
    /// Invalid selection becomes same-turn correction, without allocating an edge.
    pub(super) fn admit_request_wait_actions(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<()> {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return Ok(());
        };
        for action in &batch.actions {
            let AgentActionPayload::Wait {
                for_message: Some(message),
                from_agent,
            } = &action.payload
            else {
                continue;
            };
            let sender = Self::request_origin(turn)?.agent_id;
            let selected = self
                .agent
                .request_waits
                .requests
                .find(&sender, &turn.conversation_id, message)
                .filter(|receipt| {
                    self.control
                        .message_service()
                        .acceptance_sender_is_current(receipt.proof())
                        && self
                            .agent_turn_ledger()
                            .turn(&receipt.origin().turn_id)
                            .is_some_and(|origin| {
                                origin.started_at_unix_seconds
                                    == receipt.origin().started_at_unix_seconds
                                    && !matches!(
                                        origin.state,
                                        AgentTurnState::Failed | AgentTurnState::Interrupted
                                    )
                            })
                })
                .and_then(|receipt| {
                    let recipients = receipt.proof().recipients().collect::<Vec<_>>();
                    let index = match from_agent.as_deref() {
                        Some(agent) => recipients.iter().position(|id| {
                            id.as_str() == agent.strip_prefix("agent:").unwrap_or(agent)
                        })?,
                        None if recipients.len() == 1 => 0,
                        None => return None,
                    };
                    (recipients[index].as_str() != turn.agent_id)
                        .then_some((receipt.generation(), index))
                });
            let selected =
                selected.filter(|(generation, _)| {
                    self.agent.request_waits.edges.iter().all(|(owner, edge)| {
                        edge.generation != *generation || owner == &turn.turn_id
                    })
                });
            if let Some((generation, recipient)) = selected {
                if let Some(edge) = self.agent.request_waits.edges.get_mut(&turn.turn_id) {
                    // An explicit new selection may pivot; unrelated mail alone
                    // never changes it. Unchanged reparks allocate nothing new.
                    *edge = Edge {
                        generation,
                        recipient,
                        started: turn.started_at_unix_seconds,
                    };
                } else {
                    self.agent.request_waits.edges.insert(
                        turn.turn_id.clone(),
                        Edge {
                            generation,
                            recipient,
                            started: turn.started_at_unix_seconds,
                        },
                    );
                }
            } else if let Some(result) = execution
                .action_results
                .iter_mut()
                .find(|result| result.action_id == action.id)
            {
                *result = ActionResult::succeeded(turn, action, vec![
                    "request wait unavailable: select an outstanding expect_reply request and one original non-self recipient; correct the selection or continue authorized independent work".into()],
                    Some(r#"{"state":"request_wait_unavailable","dependency_satisfied":false}"#.into()));
                execution.final_turn = false;
            }
        }
        Ok(())
    }

    /// Records only genuinely committed ordinary send occurrences. Correlation
    /// is compared after exact original sender/recipient interval authentication.
    pub(super) fn record_request_reply_commit(
        &mut self,
        turn: &AgentTurnRecord,
        sequence: u64,
        envelope: &Envelope,
    ) -> Result<()> {
        if envelope.message_type != "send"
            || crate::runtime::control::runtime_owned_bridge_message(envelope)
        {
            return Ok(());
        }
        let Some(reply) = self
            .control
            .message_service()
            .live_acceptance_proof(sequence)
        else {
            return Ok(());
        };
        let mut matched = Vec::new();
        for receipt in self.agent.request_waits.requests.receipts() {
            if receipt.origin().agent_id.as_str() != turn.agent_id
                || receipt.origin().conversation_id != turn.conversation_id
            {
                continue;
            }
            for (index, recipient) in receipt.proof().recipients().enumerate() {
                if self.control.message_service().acceptance_reply_matches(
                    receipt.proof(),
                    recipient,
                    &reply,
                ) && let Some(slot) = self
                    .agent
                    .request_waits
                    .recipients
                    .get_mut(&receipt.generation())
                    .and_then(|slots| slots.get_mut(index))
                    && slot.outcome.is_none()
                {
                    matched.push((receipt.generation(), index));
                }
            }
        }
        if matched.is_empty() {
            return Ok(());
        }
        let label =
            crate::runtime::control::runtime_peer_message_block_label(sequence, &envelope.id);
        let context = self
            .agent_turn_contexts_mut()
            .get_mut(&turn.turn_id)
            .ok_or_else(|| {
                super::MezError::invalid_state("matching response context is missing")
            })?;
        let event = context
            .chronology()
            .iter()
            .find(|event| {
                event.block().label == label
                    && event.block().source == mez_agent::ContextSourceKind::PeerMessage
            })
            .map(|event| event.sequence())
            .ok_or_else(|| super::MezError::invalid_state("matching response commit is missing"))?;
        context.retain_pending_peer_message(event)?;
        for (generation, index) in matched {
            if let Some(slot) = self
                .agent
                .request_waits
                .recipients
                .get_mut(&generation)
                .and_then(|slots| slots.get_mut(index))
            {
                slot.outcome = Some(Outcome::Reply(sequence));
                slot.peer_source = Some((turn.turn_id.clone(), event));
            }
        }
        Ok(())
    }

    /// Captures exact locally owned terminal work before best-effort bridge loss.
    /// Failure reuses the existing native failure receipt label/content when one
    /// exists. Already completed or later persistent tasks cannot be adopted.
    pub(super) fn retain_request_task_terminal(
        &mut self,
        child: &AgentTurnRecord,
        summary: &str,
        output: &str,
    ) {
        let existing = self
            .agent
            .owned_child_failures
            .get(&child.turn_id)
            .map(|receipt| receipt.label.clone());
        for slots in self.agent.request_waits.recipients.values_mut() {
            for slot in slots {
                if slot.task.as_ref().is_some_and(|task| {
                    task.turn == child.turn_id
                        && task.conversation == child.conversation_id
                        && task.started == child.started_at_unix_seconds
                }) && slot.outcome.is_none()
                {
                    if let Some(label) = &existing {
                        slot.outcome = Some(Outcome::FailureReference {
                            label: label.clone(),
                            child: child.turn_id.clone(),
                        });
                        continue;
                    }
                    let content = format!(
                        "[local exact owned task terminal result]\nchild_task={}\nsummary={}\n[untrusted child output reference]\n{}\n[end untrusted child output reference]\nThis is a terminal occurrence, not an approval, answer-quality verdict, or permission to replay work.",
                        child.turn_id,
                        bounded_text(summary, 256),
                        bounded_text(output, 3072)
                    );
                    slot.outcome = Some(Outcome::Terminal { content });
                }
            }
        }
    }

    /// Records intentional retirement of the captured owned task without a
    /// failure diagnostic, automatic replay, or fabricated progress witness.
    pub(super) fn retain_request_task_retired(&mut self, child: &AgentTurnRecord) {
        for slots in self.agent.request_waits.recipients.values_mut() {
            for slot in slots {
                if slot.task.as_ref().is_some_and(|task| {
                    task.turn == child.turn_id
                        && task.conversation == child.conversation_id
                        && task.started == child.started_at_unix_seconds
                }) && slot.outcome.is_none()
                {
                    slot.outcome = Some(Outcome::Unavailable);
                }
            }
        }
    }

    /// Retires only dead original conversations, never completed tasks. Restored
    /// sessions clear the complete owner separately instead of recreating proof.
    pub(super) fn prune_request_wait_owners(&mut self) {
        let retired =
            self.agent
                .request_waits
                .requests
                .receipts()
                .filter_map(|receipt| {
                    let pane = receipt.origin().agent_id.as_str().strip_prefix("agent-")?;
                    (!self
                        .control
                        .message_service()
                        .acceptance_sender_is_current(receipt.proof())
                        || self.agent_shell_store().get(pane).is_none_or(|shell| {
                            shell.session_id != receipt.origin().conversation_id
                        }))
                    .then(|| {
                        (
                            receipt.origin().agent_id.clone(),
                            receipt.origin().conversation_id.clone(),
                        )
                    })
                })
                .collect::<Vec<_>>();
        for (sender, conversation) in retired {
            let generations = self
                .agent
                .request_waits
                .requests
                .receipts()
                .filter(|receipt| {
                    receipt.origin().agent_id == sender
                        && receipt.origin().conversation_id == conversation
                })
                .map(|receipt| receipt.generation())
                .collect::<Vec<_>>();
            for generation in generations {
                self.release_request_sources(generation);
            }
            self.agent
                .request_waits
                .requests
                .retire_conversation(&sender, &conversation);
        }
        self.agent.request_waits.recipients.retain(|generation, _| {
            self.agent
                .request_waits
                .requests
                .receipt(*generation)
                .is_some()
        });
        self.agent.request_waits.edges.retain(|_, edge| {
            self.agent
                .request_waits
                .requests
                .receipt(edge.generation)
                .is_some()
        });
        let ended = self
            .agent
            .request_waits
            .edges
            .keys()
            .filter(|id| {
                self.agent_turn_ledger().turn(id).is_none_or(|turn| {
                    matches!(
                        turn.state,
                        AgentTurnState::Completed
                            | AgentTurnState::Failed
                            | AgentTurnState::Interrupted
                    )
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        for turn in ended {
            self.agent.request_waits.edges.remove(&turn);
        }
        self.agent.request_waits.anchors.retain(|generation, _| {
            self.agent
                .request_waits
                .requests
                .receipt(*generation)
                .is_some()
        });
        self.agent
            .request_waits
            .fingerprints
            .retain(|generation, _| {
                self.agent
                    .request_waits
                    .requests
                    .receipt(*generation)
                    .is_some()
            });
    }

    /// Detects only authoritative ended registrations and commits typed evidence
    /// before trying the normal fair wait owner. Offline presence is not loss.
    pub(super) fn recover_request_waits(&mut self) -> Result<()> {
        self.prune_request_wait_owners();
        for receipt in self.agent.request_waits.requests.receipts() {
            for (index, recipient) in receipt.proof().recipients().enumerate() {
                if !self
                    .control
                    .message_service()
                    .acceptance_recipient_is_current(receipt.proof(), recipient)
                    && let Some(slot) = self
                        .agent
                        .request_waits
                        .recipients
                        .get_mut(&receipt.generation())
                        .and_then(|slots| slots.get_mut(index))
                    && slot.outcome.is_none()
                {
                    slot.outcome = Some(Outcome::Unavailable);
                }
            }
        }
        let turns = self
            .agent
            .request_waits
            .edges
            .keys()
            .filter_map(|id| self.agent_turn_ledger().turn(id).cloned())
            .collect::<Vec<_>>();
        for turn in turns {
            self.resume_targeted_request_wait(&turn)?;
        }
        Ok(())
    }

    /// Settles a selected edge only after canonical evidence and action-result
    /// handoff exist. QueueFull retains the same edge/source for the next sweep.
    /// Unrelated mail uses ordinary wake without consuming this request.
    pub(super) fn resume_targeted_request_wait(&mut self, turn: &AgentTurnRecord) -> Result<bool> {
        let Some(edge) = self.agent.request_waits.edges.get(&turn.turn_id).cloned() else {
            return Ok(false);
        };
        if turn.state != AgentTurnState::Blocked
            || self.agent_turn_is_human_paused(&turn.turn_id)
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
        let origin = receipt.origin().clone();
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
        let terminal_task = selected_slot.task.clone();
        let terminal_outcome = matches!(
            outcome,
            Outcome::Terminal { .. } | Outcome::FailureReference { .. }
        );
        let Some(mut result) = self
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
            .cloned()
        else {
            return Ok(false);
        };
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
        let label = self.request_handoff_label(turn, &edge, kind, &source_identity)?;
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
                    return Err(super::MezError::conflict(
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
                    return Err(super::MezError::conflict(
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
                    return Err(super::MezError::conflict(
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
            execution.terminal_state = super::runtime_agent_turn_state_from_action_results(
                &execution.action_results,
                false,
            );
        }
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
        self.agent
            .agent_turn_peer_wait_wake_pending
            .remove(&turn.turn_id);
        self.start_ready_agent_turns()?;
        Ok(true)
    }

    /// Keeps delivery maintenance eligible while explicit request handoffs exist.
    pub(super) fn has_request_waits(&self) -> bool {
        self.agent
            .request_waits
            .edges
            .iter()
            .filter(|(turn_id, _)| {
                self.agent_turn_ledger()
                    .turn(turn_id)
                    .is_some_and(|turn| turn.state == AgentTurnState::Blocked)
                    && !self.agent_turn_is_human_paused(turn_id)
                    && self
                        .agent_turn_executions()
                        .get(*turn_id)
                        .is_some_and(|execution| {
                            execution.action_results.iter().any(|result| {
                                result.action_type == "wait"
                                    && result.status == ActionStatus::Running
                            })
                        })
            })
            .any(|(_, edge)| {
                self.agent
                    .request_waits
                    .recipients
                    .get(&edge.generation)
                    .and_then(|slots| slots.get(edge.recipient))
                    .is_some_and(|slot| slot.outcome.is_some())
                    || self
                        .agent
                        .request_waits
                        .requests
                        .receipt(edge.generation)
                        .and_then(|receipt| {
                            receipt
                                .proof()
                                .recipients()
                                .nth(edge.recipient)
                                .map(|recipient| {
                                    !self
                                        .control
                                        .message_service()
                                        .acceptance_recipient_is_current(receipt.proof(), recipient)
                                })
                        })
                        .unwrap_or(false)
            })
    }

    /// Drops same-runtime capabilities on snapshot restore, never reconstructing
    /// them from model IDs, retained mail, or serialized lifecycle history.
    pub(super) fn retire_request_waits_for_restore(&mut self) {
        let generations = self
            .agent
            .request_waits
            .recipients
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for generation in generations {
            self.release_request_sources(generation);
        }
        self.agent.request_waits = RuntimeRequestWaits::default();
    }

    /// Releases only this receipt's canonical peer pins after handoff or explicit
    /// conversation retirement. Shared failure references remain protected while
    /// another outstanding request still owns that exact context event.
    fn release_request_sources(&mut self, generation: u64) {
        let pins = self
            .agent
            .request_waits
            .recipients
            .get_mut(&generation)
            .into_iter()
            .flat_map(|slots| slots.iter_mut())
            .flat_map(|slot| {
                [slot.peer_source.take(), slot.handoff_source.take()]
                    .into_iter()
                    .flatten()
            })
            .collect::<Vec<_>>();
        for (turn, sequence) in pins {
            self.release_request_pin_if_unheld(&turn, sequence);
        }
    }

    /// Releases a canonical pin only after every live request reference retires.
    fn release_request_pin_if_unheld(
        &mut self,
        turn: &str,
        sequence: mez_agent::ContextEventSequence,
    ) {
        let held = self
            .agent
            .request_waits
            .recipients
            .values()
            .flatten()
            .any(|slot| {
                [slot.peer_source.as_ref(), slot.handoff_source.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|pin| pin.0 == turn && pin.1 == sequence)
            });
        if !held && let Some(context) = self.agent_turn_contexts_mut().get_mut(turn) {
            context.release_pending_peer_message(sequence);
        }
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
            .ok_or_else(|| super::MezError::invalid_state("request handoff receipt is missing"))?;
        let recipient = receipt
            .proof()
            .recipients()
            .nth(edge.recipient)
            .ok_or_else(|| {
                super::MezError::invalid_state("request handoff recipient is missing")
            })?;
        let slot = self
            .agent
            .request_waits
            .recipients
            .get(&edge.generation)
            .and_then(|slots| slots.get(edge.recipient))
            .ok_or_else(|| {
                super::MezError::invalid_state("request handoff reservation is missing")
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

    /// Injects a stale retained source reference without fabricating acceptance,
    /// context, lifecycle, scheduler custody, or provider claims.
    #[cfg(test)]
    pub(crate) fn replace_request_handoff_reference_for_test(
        &mut self,
        turn: &str,
        sequence: mez_agent::ContextEventSequence,
    ) -> Result<()> {
        let edge = self
            .agent
            .request_waits
            .edges
            .get(turn)
            .ok_or_else(|| super::MezError::invalid_state("test wait edge missing"))?
            .clone();
        let slot = self
            .agent
            .request_waits
            .recipients
            .get_mut(&edge.generation)
            .and_then(|slots| slots.get_mut(edge.recipient))
            .ok_or_else(|| super::MezError::invalid_state("test request reservation missing"))?;
        slot.handoff_source = Some((turn.into(), sequence));
        Ok(())
    }

    /// Revokes a terminal task's subscription without erasing conversation-owned
    /// accepted request receipts or allowing a later task to inherit its edge.
    pub(super) fn retire_request_wait_edge(&mut self, turn: &str) {
        self.agent.request_waits.edges.remove(turn);
    }

    /// Exposes finite owner counts to product regressions without granting mail,
    /// lifecycle, request-consumption, or scheduling authority.
    #[cfg(test)]
    pub(crate) fn tracked_message_request_usage(&self) -> ((usize, usize, usize), usize) {
        (
            self.agent.request_waits.requests.usage(),
            self.agent.request_waits.edges.len(),
        )
    }

    /// Narrows otherwise-empty admission policy for native preaccept controls.
    #[cfg(test)]
    pub(crate) fn set_tracked_message_request_limits_for_test(
        &mut self,
        limits: mez_agent::messaging::MessageRequestLimits,
    ) -> Result<()> {
        if self.agent.request_waits.requests.usage().0 != 0 {
            return Err(super::MezError::invalid_state(
                "test request owner is not empty",
            ));
        }
        self.agent.request_waits.requests = MessageRequests::with_limits(limits)?;
        Ok(())
    }
}

/// Hashes immutable native send intent without retaining another payload. Time
/// is intentionally excluded: a retry reuses the original accepted occurrence.
fn request_fingerprint(envelope: &Envelope, scope: MessageScope) -> [u8; 32] {
    use mez_agent::messaging::Recipient;
    let (kind, recipient) = match &envelope.recipient {
        Recipient::Agent(id) => ("agent", id.as_str()),
        Recipient::Pane(id) => ("pane", id.as_str()),
        Recipient::Window(id) => ("window", id.as_str()),
        Recipient::Session => ("session", ""),
        Recipient::Role(text) => ("role", text.as_str()),
        Recipient::Capability(text) => ("capability", text.as_str()),
        Recipient::Group(text) => ("group", text.as_str()),
    };
    let mut digest = Sha256::new();
    digest.update([u8::from(envelope.ttl_ms.is_some())]);
    digest.update(envelope.ttl_ms.unwrap_or(0).to_le_bytes());
    for field in [
        "mez-tracked-native-intent-v1",
        envelope.message_type.as_str(),
        kind,
        recipient,
        match scope {
            MessageScope::Project => "project",
            MessageScope::Session => "session",
        },
        envelope.content_type.as_str(),
        envelope.payload.as_str(),
        envelope.correlation_id.as_deref().unwrap_or(""),
    ] {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field.as_bytes());
    }
    digest.finalize().into()
}

/// Truncates untrusted result excerpts at a UTF-8 boundary within byte headroom.
fn bounded_text(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
