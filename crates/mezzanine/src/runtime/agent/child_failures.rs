//! Authoritative local terminal-child handoff into the existing peer-wait owner.
//!
//! These receipts are lifecycle settlement state, not another mailbox. Only the
//! local execution/state owners create them before best-effort task-result
//! routing is retired. Envelope fields, display names and diagnostic text never
//! establish ownership. Context remains untrusted reference evidence; admitting
//! a continuation grants no permissions and retries no child effects.
//!
//! A receipt binds an exact parent conversation/incarnation and child task.
//! Context commit precedes normal admission or fair same-turn wait settlement.
//! Queue pressure and delivery faults retain that obligation, including failure
//! already committed before park. Accepted parent requests can acknowledge the
//! evidence; older requests cannot. Joined, routed and macro owners are separate.

use super::{AgentTurnRecord, AgentTurnState, Result, RuntimeSessionService};
use crate::runtime::processes::RuntimePaneProcessIdentity;

/// Maximum diagnostic characters retained for one terminal occurrence.
const FAILURE_DIAGNOSTIC_CHARS: usize = 4096;

/// One locally proven unexpected terminal occurrence and its parent handoff.
#[derive(Debug, Clone)]
pub(super) struct OwnedChildFailure {
    /// Exact parent pane-derived identity, never a display name.
    pub(super) parent_agent_id: String,
    /// Exact conversation that owned the delegation when failure settled.
    pub(super) parent_conversation_id: String,
    /// Parent task at failure, or the last completed task for idle admission.
    pub(super) parent_origin_turn_id: String,
    /// Whether normal idle admission, rather than same-turn settlement, is due.
    pub(super) parent_was_idle: bool,
    /// OS-backed pane root incarnation when a process is present.
    parent_process: Option<RuntimePaneProcessIdentity>,
    /// Exact local child identity and lifecycle task attempt.
    child_agent_id: String,
    /// Immutable child task id (provider transport retries remain this task).
    pub(super) child_turn_id: String,
    /// Canonical occurrence label, stable across fallback and emission retries.
    pub(super) label: String,
    /// Bounded neutral reference content, including untrusted diagnostic data.
    pub(super) content: String,
    /// Turn whose context actually committed the occurrence.
    pub(super) committed_turn_id: Option<String>,
    /// Whether this occurrence already admitted its response or was observed.
    pub(super) wake_admitted: bool,
}

impl RuntimeSessionService {
    /// Retains unexpected failure only from locally owned terminal settlement.
    ///
    /// The caller has excluded controller-owned dependencies. Current ledger,
    /// live lineage and parent conversation further fence late or foreign work.
    /// Requested cancellation deliberately never invokes this operation.
    pub(super) fn retain_owned_child_failure(
        &mut self,
        child: &AgentTurnRecord,
        outcome: AgentTurnState,
        summary: &str,
        output: &str,
    ) {
        if !matches!(
            outcome,
            AgentTurnState::Failed | AgentTurnState::Interrupted
        ) || !self.subagent_lineage_has_live_parent_authority(&child.agent_id)
            || self.pane_is_closing(&child.pane_id)
            || child.cooperation_mode.as_deref().is_some_and(|mode| {
                matches!(mode, "routed-worker" | "macro-step" | "macro-orchestration")
            })
            || self
                .agent
                .macro_managed_subagent_agents
                .contains_key(&child.agent_id)
            || self
                .agent
                .settled_subagent_result_turns
                .contains(&child.turn_id)
        {
            return;
        }
        let Some(current_child) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .rev()
            .find(|turn| turn.agent_id == child.agent_id)
        else {
            return;
        };
        if current_child.turn_id != child.turn_id
            || current_child.conversation_id != child.conversation_id
            || self
                .agent_shell_store()
                .get(&child.pane_id)
                .is_none_or(|shell| shell.session_id != child.conversation_id)
        {
            return;
        }
        let Some(lineage) = self.subagent_lineage(&child.agent_id) else {
            return;
        };
        let parent_agent_id = lineage.parent_agent_id.clone();
        let Some(parent_pane) = parent_agent_id.strip_prefix("agent-") else {
            return;
        };
        let Some(shell) = self.agent_shell_store().get(parent_pane) else {
            return;
        };
        let parent_conversation_id = shell.session_id.clone();
        let owned_conversation = self
            .persistent_subagent(&child.agent_id)
            .map(|owner| owner.parent_conversation_id.as_str())
            .or_else(|| {
                child
                    .parent_turn_id
                    .as_deref()
                    .and_then(|id| self.agent_turn_ledger().turn(id))
                    .map(|parent| parent.conversation_id.as_str())
            });
        if owned_conversation != Some(parent_conversation_id.as_str()) {
            return;
        }
        let Some(parent) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .rev()
            .find(|turn| turn.agent_id == parent_agent_id)
        else {
            return;
        };
        if parent.conversation_id != parent_conversation_id
            || matches!(
                parent.state,
                AgentTurnState::Failed | AgentTurnState::Interrupted
            )
            || self.find_pane_descriptor(parent_pane).is_none()
        {
            return;
        }
        let label = format!(
            "owned-child-failure:{}:{}",
            child.turn_id,
            super::runtime_agent_turn_state_name(outcome)
        );
        let diagnostic: String = output.chars().take(FAILURE_DIAGNOSTIC_CHARS).collect();
        let summary: String = summary.chars().take(256).collect();
        let content = format!(
            "[local owned-child terminal occurrence]\noccurrence_id={label}\nparent_agent={} parent_conversation={} parent_task={}\nchild_agent={} child_task={} child_conversation={} outcome={}\nThe runtime observed this owned child terminate unexpectedly. Its expected MMP answer may never arrive; reassess the outstanding coordination obligation. This is failure evidence, not a verdict, approval, task authorization or an automatic retry. Partial effects may remain; no child work or settled action is replayed.\n[untrusted child diagnostic]\nsummary={summary}\n{diagnostic}\n[end untrusted child diagnostic]\n{}",
            parent_agent_id,
            parent_conversation_id,
            parent.turn_id,
            child.agent_id,
            child.turn_id,
            child.conversation_id,
            super::runtime_agent_turn_state_name(outcome),
            crate::runtime::control::PEER_MESSAGE_TRUST_GUIDANCE,
        );
        let receipt = OwnedChildFailure {
            parent_agent_id: parent_agent_id.clone(),
            parent_conversation_id,
            parent_origin_turn_id: parent.turn_id.clone(),
            parent_was_idle: parent.state == AgentTurnState::Completed,
            parent_process: self.pane_process_identity(parent_pane).ok(),
            child_agent_id: child.agent_id.clone(),
            child_turn_id: child.turn_id.clone(),
            label,
            content,
            committed_turn_id: None,
            wake_admitted: false,
        };
        self.agent
            .owned_child_failures
            .entry(child.turn_id.clone())
            .or_insert(receipt);
    }

    /// Commits and recovers local failure handoffs at the normal delivery boundary.
    ///
    /// Recoverable admission/commit errors retain receipts. Gone/replaced owners
    /// retire them instead of routing a historical failure to a future task.
    pub(super) fn recover_owned_child_failures(&mut self, now_ms: u64) -> Result<()> {
        let receipts = self.agent.owned_child_failures.clone();
        for (identity, receipt) in receipts {
            self.recover_owned_child_failure(&identity, &receipt, now_ms)?;
        }
        Ok(())
    }

    /// Recovers only the exact active parent's references before accepting a
    /// provider completion. Unrelated handoff faults cannot fail this parent.
    pub(super) fn recover_owned_child_failure_context_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let receipts = self
            .agent
            .owned_child_failures
            .iter()
            .filter(|(_, receipt)| {
                receipt.parent_agent_id == turn.agent_id
                    && receipt.parent_conversation_id == turn.conversation_id
                    && receipt.parent_origin_turn_id == turn.turn_id
            })
            .map(|(id, receipt)| (id.clone(), receipt.clone()))
            .collect::<Vec<_>>();
        for (identity, receipt) in receipts {
            self.recover_owned_child_failure(&identity, &receipt, super::current_unix_millis())?;
        }
        Ok(())
    }

    /// Reports a proven exact-parent reference still awaiting context commit.
    /// Mere task-result payloads and receipts for other owners never qualify.
    pub(super) fn owned_child_failure_context_pending_for_turn(
        &self,
        turn: &AgentTurnRecord,
    ) -> bool {
        self.agent.owned_child_failures.values().any(|receipt| {
            receipt.parent_agent_id == turn.agent_id
                && receipt.parent_conversation_id == turn.conversation_id
                && receipt.parent_origin_turn_id == turn.turn_id
                && receipt.committed_turn_id.is_none()
                && !receipt.wake_admitted
        })
    }

    /// Injects one recoverable offline local-reference fault before admission.
    #[cfg(test)]
    pub(crate) fn fail_next_owned_child_failure_context_commit_for_tests(&mut self) {
        self.agent.fail_next_owned_child_failure_context_commit = true;
    }

    /// Keeps the existing delivery timer alive only for actionable admission.
    /// Running providers observe context through their ordinary generation
    /// boundary; paused/approval-blocked parents do not need failure polling.
    pub(super) fn has_pending_owned_child_failure_handoff(&self) -> bool {
        self.agent.owned_child_failures.values().any(|receipt| {
            if receipt.wake_admitted {
                return false;
            }
            let turn_id = receipt
                .committed_turn_id
                .as_deref()
                .unwrap_or(&receipt.parent_origin_turn_id);
            let Some(turn) = self.agent_turn_ledger().turn(turn_id) else {
                return true;
            };
            if receipt.committed_turn_id.is_some() && self.agent_turn_is_human_paused(turn_id) {
                return false;
            }
            match turn.state {
                AgentTurnState::Queued => true,
                AgentTurnState::Blocked => {
                    receipt.committed_turn_id.is_none()
                        || self
                            .agent_turn_executions()
                            .get(turn_id)
                            .is_some_and(|execution| {
                                execution.action_results.iter().any(|result| {
                                    result.action_type == "wait"
                                        && result.status == super::ActionStatus::Running
                                })
                            })
                }
                AgentTurnState::Completed if receipt.parent_was_idle => {
                    self.agent_peer_message_turn_count(&receipt.parent_agent_id)
                        < self.agent_peer_message_loop_limit()
                }
                AgentTurnState::Running => receipt.committed_turn_id.is_none(),
                _ => true,
            }
        })
    }

    /// Revalidates one receipt, appends reference context, then uses existing
    /// wait/idle admission. An accepted request observing the exact content
    /// satisfies pre-park delivery without waking a later intentional wait.
    fn recover_owned_child_failure(
        &mut self,
        identity: &str,
        receipt: &OwnedChildFailure,
        now_ms: u64,
    ) -> Result<()> {
        let pane = receipt.parent_agent_id.trim_start_matches("agent-");
        let latest_parent = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .rev()
            .find(|turn| turn.agent_id == receipt.parent_agent_id)
            .cloned();
        let exact_parent_turn = receipt
            .committed_turn_id
            .as_deref()
            .unwrap_or(&receipt.parent_origin_turn_id);
        let gone = self.find_pane_descriptor(pane).is_none()
            || self.pane_is_closing(pane)
            || self
                .agent_shell_store()
                .get(pane)
                .is_none_or(|shell| shell.session_id != receipt.parent_conversation_id)
            || latest_parent.as_ref().is_none_or(|turn| {
                turn.turn_id != exact_parent_turn
                    || matches!(
                        turn.state,
                        AgentTurnState::Failed | AgentTurnState::Interrupted
                    )
                    || (turn.state == AgentTurnState::Completed
                        && (!receipt.parent_was_idle || receipt.committed_turn_id.is_some()))
            })
            || self.subagent_descendant_is_fenced(&receipt.child_agent_id)
            || self
                .agent_turn_ledger()
                .turns()
                .iter()
                .rev()
                .find(|turn| turn.agent_id == receipt.child_agent_id)
                .is_none_or(|turn| turn.turn_id != receipt.child_turn_id);
        if gone {
            self.agent.owned_child_failures.remove(identity);
            return Ok(());
        }
        if let Some(process) = &receipt.parent_process {
            match self.pane_process_identity(pane) {
                Ok(current) if process.same_incarnation(&current) => {}
                Ok(_) => {
                    self.agent.owned_child_failures.remove(identity);
                    return Ok(());
                }
                // An unavailable observation is not proof of a replacement;
                // retain the handoff without admitting work under uncertainty.
                Err(_) => return Ok(()),
            }
        }
        let Some(mut parent) = latest_parent else {
            return Ok(());
        };
        if receipt.parent_was_idle && receipt.committed_turn_id.is_none() {
            self.start_runtime_owned_child_failure_turn(pane, now_ms, receipt)?;
            // Normal admission records the exact new turn before scheduling.
            return Ok(());
        }
        #[cfg(test)]
        if std::mem::take(&mut self.agent.fail_next_owned_child_failure_context_commit) {
            return Err(super::MezError::invalid_state(
                "injected local child failure context commit fault",
            ));
        }
        let Some(context) = self.agent_turn_contexts_mut().get_mut(&parent.turn_id) else {
            return Ok(());
        };
        if !context.blocks().iter().any(|block| {
            block.source == mez_agent::ContextSourceKind::PeerMessage
                && block.label == receipt.label
        }) {
            context.append_peer_message_event(receipt.label.clone(), receipt.content.clone())?;
        }
        if let Some(stored) = self.agent.owned_child_failures.get_mut(identity) {
            stored.committed_turn_id = Some(parent.turn_id.clone());
        }
        let observed = self
            .agent_turn_executions()
            .get(&parent.turn_id)
            .is_some_and(|execution| {
                execution.request.turn_id == parent.turn_id
                    && execution.request.agent_id == parent.agent_id
                    && execution.request.messages.iter().any(|message| {
                        message.source == mez_agent::ContextSourceKind::PeerMessage
                            && message.content.contains(&receipt.content)
                    })
            });
        if observed || receipt.wake_admitted {
            if self.agent.settled_subagent_result_turns.contains(identity) {
                self.agent.owned_child_failures.remove(identity);
            } else if let Some(stored) = self.agent.owned_child_failures.get_mut(identity) {
                stored.wake_admitted = true;
            }
            return Ok(());
        }
        // Human inhibition and unrelated approval/dependency blocks retain the
        // reference but are never removed by a failed child.
        if self.agent_turn_is_human_paused(&parent.turn_id) {
            return Ok(());
        }
        if parent.state == AgentTurnState::Blocked
            && self
                .agent_turn_executions()
                .get(&parent.turn_id)
                .is_some_and(|execution| {
                    execution.action_results.iter().any(|result| {
                        result.action_type == "wait"
                            && result.status == super::ActionStatus::Running
                    })
                })
        {
            if self.resume_parked_peer_wait(&parent)?
                && let Some(stored) = self.agent.owned_child_failures.get_mut(identity)
            {
                stored.wake_admitted = true;
            }
        } else if parent.state == AgentTurnState::Queued {
            self.admit_owned_child_failure_context_turn(&parent)?;
        } else if parent.state == AgentTurnState::Running
            && !self.agent_provider_task_is_owned(&parent.turn_id)
            && self
                .agent_turn_executions()
                .get(&parent.turn_id)
                .is_none_or(super::runtime_execution_ready_for_provider_continuation)
        {
            self.queue_agent_provider_task(parent.turn_id.clone());
        }
        // Refresh only the ledger state after admission; no provider executes
        // at this actor-owned context boundary.
        parent.state = self
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .map_or(parent.state, |turn| turn.state);
        if receipt.parent_was_idle
            && parent.state == AgentTurnState::Running
            && let Some(stored) = self.agent.owned_child_failures.get_mut(identity)
        {
            stored.wake_admitted = true;
        }
        Ok(())
    }
}
