//! Serialized parent coordination driven by committed mail from an exact join.
//!
//! A mail receipt requests fair scheduler admission; it grants no action or
//! permission authority. Original task executions remain independently owned by
//! the completion component while each new response uses its normal action
//! dispatcher. Consumed mail never respawns a child or completes an await.

use super::*;
use crate::runtime::agent::{AgentTurnState, Envelope};
use mez_agent::SchedulerErrorKind;
use std::collections::{BTreeMap, BTreeSet};

/// One unanswered question and the joins already outstanding when it arrived.
#[derive(Debug)]
struct JoinedQuestion {
    /// Exact child attempt; replacement attempts never inherit a question.
    dependency: JoinedSubagentDependency,
    /// Accepted question occurrence, used to reject an older accepted reply.
    sequence: u64,
    /// Existing original group/action obligations, shared within one cohort.
    baseline: Arc<BTreeSet<(ContextExecutionGroupId, String)>>,
}

/// Current committed mail and response ownership for one original parent turn.
#[derive(Debug)]
pub(in crate::runtime::agent) struct JoinedCoordination {
    /// Frozen parent identity, including its conversation and incarnation.
    turn: AgentTurnRecord,
    /// Latest actual committed transport occurrence for each exact child task.
    mail: BTreeMap<String, (JoinedSubagentDependency, u64)>,
    /// Unanswered questions survive normal tool and helper continuations.
    questions: BTreeMap<String, JoinedQuestion>,
    /// Explicit model completion ends coordination without completing its joins.
    end_requested: bool,
    /// Mail consumed by an accepted response, never just request preparation.
    consumed: u64,
    /// Actual actor claim generation and the mail boundary offered to it.
    claim: Option<(u64, u64)>,
    /// An accepted coordination response still owns its ordinary action batch.
    applying: bool,
    /// Accepted response group; the previous stored batch cannot settle it.
    applying_group: Option<ContextExecutionGroupId>,
    /// Exact trusted resume of an unissued answer, distinct from peer mail.
    human_resume: Option<(u64, ContextExecutionGroupId)>,
}

impl RuntimeSessionService {
    /// Reports actual outstanding task joins independently of the latest batch.
    pub(crate) fn parent_has_pending_joined_tasks(&self, turn_id: &str) -> bool {
        self.agent
            .joined_subagent_dependencies
            .values()
            .any(|dependency| dependency.parent_turn_id == turn_id)
    }

    /// Retains mail only at the existing proven canonical receive boundary.
    /// Runtime bridge notices and other senders cannot create this owner. Exact
    /// repeated delivery coalesces without consuming or renewing the receipt.
    pub(in crate::runtime::agent) fn record_joined_coordination_mail(
        &mut self,
        turn: &AgentTurnRecord,
        sequence: u64,
        envelope: &Envelope,
    ) {
        if crate::runtime::control::runtime_owned_bridge_message(envelope) {
            return;
        }
        let Some(origin) = self.runtime_outbound_delivery_origin(sequence, envelope) else {
            return;
        };
        let Some(dependency) = self
            .agent
            .joined_subagent_dependencies
            .values()
            .find(|dependency| {
                dependency.parent_turn_id == turn.turn_id
                    && dependency.child_turn_id == origin.turn_id
                    && dependency.child_agent_id == envelope.sender.agent_id.as_str()
            })
            .cloned()
        else {
            return;
        };
        let Some(child) = self.agent_turn_ledger().turn(&dependency.child_turn_id) else {
            return;
        };
        if child.agent_id != dependency.child_agent_id
            || !matches!(
                child.state,
                AgentTurnState::Running | AgentTurnState::Blocked
            )
            || self
                .agent_shell_store()
                .get(&child.pane_id)
                .is_none_or(|session| {
                    session.session_id != child.conversation_id
                        || session.running_turn_id.as_deref() != Some(child.turn_id.as_str())
                })
        {
            return;
        }
        let owner = self
            .agent
            .joined_coordination
            .entry(turn.turn_id.clone())
            .or_insert_with(|| JoinedCoordination {
                turn: turn.clone(),
                mail: BTreeMap::new(),
                consumed: 0,
                questions: BTreeMap::new(),
                end_requested: false,
                claim: None,
                applying: false,
                applying_group: None,
                human_resume: None,
            });
        if owner.turn.agent_id != turn.agent_id
            || owner.turn.pane_id != turn.pane_id
            || owner.turn.conversation_id != turn.conversation_id
            || owner.turn.started_at_unix_seconds != turn.started_at_unix_seconds
        {
            return;
        }
        let receipt = owner
            .mail
            .entry(dependency.child_turn_id.clone())
            .or_insert((dependency.clone(), sequence));
        if receipt.0 == dependency {
            receipt.1 = receipt.1.max(sequence);
        }
    }

    /// Checks frozen parent binding without granting readiness or authority.
    fn joined_coordination_parent_current(&self, turn_id: &str) -> bool {
        let Some(owner) = self.agent.joined_coordination.get(turn_id) else {
            return false;
        };
        let Some(turn) = self.agent_turn_ledger().turn(turn_id) else {
            return false;
        };
        if !matches!(
            turn.state,
            AgentTurnState::Blocked | AgentTurnState::Running
        ) || turn.agent_id != owner.turn.agent_id
            || turn.pane_id != owner.turn.pane_id
            || turn.conversation_id != owner.turn.conversation_id
            || turn.started_at_unix_seconds != owner.turn.started_at_unix_seconds
            || self
                .agent_shell_store()
                .get(&turn.pane_id)
                .is_none_or(|session| {
                    session.session_id != turn.conversation_id
                        || session.running_turn_id.as_deref() != Some(turn_id)
                })
        {
            return false;
        }
        true
    }

    /// Keeps the committed retry obligation independently of admission gates.
    fn joined_coordination_has_unconsumed_mail(&self, turn_id: &str) -> bool {
        if !self.joined_coordination_parent_current(turn_id) {
            return false;
        }
        let Some(owner) = self.agent.joined_coordination.get(turn_id) else {
            return false;
        };
        owner.mail.values().any(|(dependency, sequence)| {
            *sequence > owner.consumed
                && self.joined_coordination_question_child_current(dependency)
        })
    }

    /// Prevents reentrant receive recovery from settling the previous stored
    /// batch while a newer accepted response is still being applied.
    fn joined_coordination_response_source_current(&self, turn_id: &str) -> bool {
        let Some(owner) = self.agent.joined_coordination.get(turn_id) else {
            return true;
        };
        if !owner.applying {
            return true;
        }
        let Some(group) = owner.applying_group.as_ref() else {
            return false;
        };
        self.agent_turn_executions()
            .get(turn_id)
            .is_some_and(|execution| {
                super::super::provider_execution::provider_log_execution_group_id(execution)
                    .is_ok_and(|current| &current == group)
            })
    }

    /// Identifies an unissued coordination answer that explicit Resume will
    /// supersede. An already settled answer or an intentional Wait must keep
    /// its existing child dependency rather than request another model call.
    pub(in crate::runtime::agent) fn joined_coordination_resume_needs_provider(
        &self,
        turn_id: &str,
    ) -> bool {
        self.joined_coordination_parent_current(turn_id)
            && self.joined_coordination_response_source_current(turn_id)
            && self.has_retained_joined_source(turn_id)
            && self
                .agent
                .joined_coordination
                .get(turn_id)
                .is_some_and(|owner| {
                    owner.applying
                        && self
                            .agent_turn_executions()
                            .get(turn_id)
                            .is_some_and(|execution| {
                                !self
                                    .execution_waiting_for_live_joined_subagents(turn_id, execution)
                                    && (owner.human_resume.is_some()
                                        || execution.action_results.iter().any(|result| {
                                            matches!(
                                                result.status,
                                                mez_agent::ActionStatus::Running
                                                    | mez_agent::ActionStatus::Blocked
                                            ) && result.action_type != "wait"
                                        }))
                            })
                })
    }

    /// Retains only an actual validated primary Resume after canonical
    /// cancellation. This does not resurrect mail, approve work or reset a
    /// retry budget. The existing completion group owns its archive/generation.
    pub(in crate::runtime::agent) fn record_joined_coordination_human_resume(
        &mut self,
        turn_id: &str,
        generation: u64,
    ) -> Result<()> {
        let turn = self
            .agent_turn_ledger()
            .turn(turn_id)
            .cloned()
            .ok_or_else(|| MezError::conflict("joined resume turn is absent"))?;
        if !self.human_pause_matches_completion_resume(&turn, generation)
            || !self.joined_coordination_parent_current(turn_id)
            || !self.joined_coordination_response_source_current(turn_id)
            || !self.has_retained_joined_source(turn_id)
            || !self
                .agent
                .joined_coordination
                .get(turn_id)
                .is_some_and(|owner| owner.applying)
        {
            return Err(MezError::conflict(
                "joined resume has no exact paused coordination owner",
            ));
        }
        let execution = self
            .agent_turn_executions()
            .get(turn_id)
            .ok_or_else(|| MezError::conflict("joined resume source is absent"))?;
        if !execution
            .action_results
            .iter()
            .all(mez_agent::ActionResult::is_terminal)
            || !execution
                .action_results
                .iter()
                .any(|result| result.status == mez_agent::ActionStatus::Cancelled)
        {
            return Err(MezError::conflict(
                "joined resume has no superseded unissued answer",
            ));
        }
        let group = super::super::provider_execution::provider_log_execution_group_id(execution)?;
        self.reserve_completion_group(turn_id, &group)?;
        let owner = self
            .agent
            .joined_coordination
            .get_mut(turn_id)
            .ok_or_else(|| MezError::invalid_state("joined resume owner disappeared"))?;
        owner.human_resume = Some((generation, group));
        Ok(())
    }

    /// Recognizes the existing checked Resume only while its original group
    /// and generation still own the cancelled answer's continuation.
    fn joined_coordination_human_resume_ready(&self, turn_id: &str) -> bool {
        self.joined_coordination_parent_current(turn_id)
            && self.has_retained_joined_source(turn_id)
            && self
                .agent
                .joined_coordination
                .get(turn_id)
                .is_some_and(|owner| {
                    owner.applying
                        && owner
                            .human_resume
                            .as_ref()
                            .is_some_and(|(generation, group)| {
                                self.completion_human_resume_source_ready(
                                    turn_id,
                                    *generation,
                                    group,
                                )
                            })
                })
    }

    /// Reports whether an unanswered question permits this successful batch's
    /// ordinary continuation. Newly spawned helper joins must finish first;
    /// only another exact child question can temporarily open their own scope.
    pub(in crate::runtime::agent) fn joined_coordination_batch_continues(
        &self,
        turn_id: &str,
        execution: &AgentTurnExecution,
    ) -> bool {
        let Some(owner) = self.agent.joined_coordination.get(turn_id) else {
            return false;
        };
        self.joined_coordination_has_active_question(turn_id)
            && super::super::runtime_execution_ready_for_provider_continuation(execution)
            && !self.agent_turn_is_human_paused(turn_id)
            && owner.applying
    }

    /// A task's join may survive interruption for redirection, but its old
    /// question cannot. Question authority requires the exact live child task
    /// and conversation, independently of the longer lived spawn obligation.
    fn joined_coordination_question_child_current(
        &self,
        dependency: &JoinedSubagentDependency,
    ) -> bool {
        self.agent
            .joined_subagent_dependencies
            .get(&dependency.child_turn_id)
            == Some(dependency)
            && self
                .retained_joined_spawn_action(dependency)
                .is_ok_and(|action| action.is_some())
            && self
                .agent_turn_ledger()
                .turn(&dependency.child_turn_id)
                .is_some_and(|child| {
                    child.agent_id == dependency.child_agent_id
                        && matches!(
                            child.state,
                            AgentTurnState::Running | AgentTurnState::Blocked
                        )
                        && self
                            .agent_shell_store()
                            .get(&child.pane_id)
                            .is_some_and(|session| {
                                session.session_id == child.conversation_id
                                    && session.running_turn_id.as_deref()
                                        == Some(child.turn_id.as_str())
                            })
                })
    }

    /// Checks question purpose without conflating it with physical readiness.
    /// A new helper result gate restricts an earlier question's baseline even
    /// when both questions share the same still-live parent turn.
    fn joined_coordination_has_active_question(&self, turn_id: &str) -> bool {
        let Some(owner) = self.agent.joined_coordination.get(turn_id) else {
            return false;
        };
        owner.applying
            && !owner.end_requested
            && self.joined_coordination_parent_current(turn_id)
            && owner.questions.values().any(|question| {
                self.joined_coordination_question_child_current(&question.dependency)
                    && self
                        .agent
                        .joined_subagent_dependencies
                        .values()
                        .filter(|dependency| dependency.parent_turn_id == turn_id)
                        .all(|dependency| {
                            self.joined_original_group(dependency).is_ok_and(|group| {
                                group.is_some_and(|group| {
                                    question
                                        .baseline
                                        .contains(&(group, dependency.parent_action_id.clone()))
                                })
                            })
                        })
            })
    }

    /// Revokes only queued or actually claimed question continuations after
    /// their child authority ends. Existing policy feedback, managed assessment,
    /// human inhibition and exact trusted Resume keep their separate owners.
    pub(in crate::runtime::agent) fn joined_coordination_provider_purpose_revoked(
        &self,
        turn_id: &str,
    ) -> bool {
        let Some(owner) = self.agent.joined_coordination.get(turn_id) else {
            return false;
        };
        self.parent_has_pending_joined_tasks(turn_id)
            && self.joined_coordination_parent_current(turn_id)
            && !self.agent_turn_is_human_paused(turn_id)
            && owner.human_resume.is_none()
            && (owner.claim.is_some()
                || self
                    .agent_turn_executions()
                    .get(turn_id)
                    .is_some_and(|execution| {
                        super::super::runtime_execution_ready_for_provider_continuation(execution)
                            || self.execution_waiting_for_live_joined_subagents(turn_id, execution)
                    }))
            && !self.joined_coordination_has_unconsumed_mail(turn_id)
            && !self.joined_coordination_has_active_question(turn_id)
    }

    /// Requires ordinary success and all current physical/publication gates
    /// before an unanswered question may request another inference.
    fn joined_coordination_continuation_ready(&self, turn_id: &str) -> bool {
        self.joined_coordination_response_source_current(turn_id)
            && self.joined_coordination_source_quiescent(turn_id)
            && self
                .agent_turn_executions()
                .get(turn_id)
                .is_some_and(|execution| {
                    self.joined_coordination_batch_continues(turn_id, execution)
                })
    }

    /// Closes only questions addressed by a send accepted in this response's
    /// execution group. Transport retries retain their normal custody; this
    /// never forges an offer, observation, cursor advance or acknowledgement.
    pub(in crate::runtime::agent) fn record_joined_coordination_reply(
        &mut self,
        turn: &AgentTurnRecord,
        delivery: &mez_agent::messaging::Delivery,
    ) {
        let Some(owner) = self.agent.joined_coordination.get(&turn.turn_id) else {
            return;
        };
        if !owner.applying || !self.joined_coordination_parent_current(&turn.turn_id) {
            return;
        }
        let Some(group) = owner.applying_group.as_ref() else {
            return;
        };
        let answered = owner
            .questions
            .iter()
            .filter(|(child, question)| {
                delivery.sequence > question.sequence
                    && self.agent.joined_subagent_dependencies.get(*child)
                        == Some(&question.dependency)
                    && self.runtime_accepted_reply_targets(
                        turn,
                        delivery,
                        group,
                        &question.dependency.child_agent_id,
                    )
            })
            .map(|(child, _)| child.clone())
            .collect::<Vec<_>>();
        if let Some(owner) = self.agent.joined_coordination.get_mut(&turn.turn_id) {
            for child in answered {
                owner.questions.remove(&child);
            }
        }
    }

    /// Derives admission from exact committed mail and a quiescent source.
    /// Neither human inhibition nor unfinished sibling work can be bypassed.
    pub(crate) fn joined_coordination_offer_ready(&self, turn_id: &str) -> bool {
        !self.agent_turn_is_human_paused(turn_id)
            && self.joined_coordination_response_source_current(turn_id)
            && ((self.joined_coordination_has_unconsumed_mail(turn_id)
                && self.joined_coordination_source_quiescent(turn_id))
                || self.joined_coordination_human_resume_ready(turn_id)
                || self.joined_coordination_continuation_ready(turn_id))
    }

    /// Keeps the bounded delivery cadence until an acknowledged question can
    /// acquire capacity. Queued/claimed provider work already owns its wake;
    /// consumed, stale and retired receipts never keep a timer alive.
    pub(in crate::runtime::agent) fn has_pending_joined_coordination_wake(&self) -> bool {
        self.agent.joined_coordination.keys().any(|turn_id| {
            !self.agent.pending_agent_provider_tasks.contains(turn_id)
                && !self
                    .agent
                    .claimed_agent_provider_tasks
                    .contains_key(turn_id)
                && (self.joined_coordination_has_unconsumed_mail(turn_id)
                    || self.joined_coordination_human_resume_ready(turn_id)
                    || self.joined_coordination_continuation_ready(turn_id))
        })
    }

    /// Recognizes the response already admitted from exact child mail. This
    /// scopes the existing post-budget action-feedback queue; it does not itself
    /// classify errors, spend retry budget, acknowledge a queue or grant effects.
    pub(in crate::runtime::agent) fn joined_coordination_response_applying(
        &self,
        turn_id: &str,
    ) -> bool {
        let Some(owner) = self.agent.joined_coordination.get(turn_id) else {
            return false;
        };
        let Some(turn) = self.agent_turn_ledger().turn(turn_id) else {
            return false;
        };
        owner.applying
            && turn.state == AgentTurnState::Running
            && !self.agent_turn_is_human_paused(turn_id)
            && self.parent_has_pending_joined_tasks(turn_id)
            && self.has_retained_joined_source(turn_id)
            && turn.agent_id == owner.turn.agent_id
            && turn.pane_id == owner.turn.pane_id
            && turn.conversation_id == owner.turn.conversation_id
            && turn.started_at_unix_seconds == owner.turn.started_at_unix_seconds
            && self
                .agent_shell_store()
                .get(&turn.pane_id)
                .is_some_and(|session| {
                    session.session_id == turn.conversation_id
                        && session.running_turn_id.as_deref() == Some(turn_id)
                })
    }

    /// Distinguishes a pending source claim from ordinary terminal continuation.
    pub(in crate::runtime::agent) fn joined_coordination_retains_current_group(
        &self,
        turn_id: &str,
    ) -> bool {
        self.joined_coordination_offer_ready(turn_id)
            && self
                .agent_turn_executions()
                .get(turn_id)
                .is_some_and(|execution| {
                    self.execution_waiting_for_live_joined_subagents(turn_id, execution)
                })
    }

    /// Enqueues a qualified coordination offer without accepting the pending
    /// original group's ordinary continuation/archive milestones.
    pub(in crate::runtime::agent) fn queue_joined_coordination_provider_task(
        &mut self,
        turn_id: &str,
    ) -> bool {
        if !self.joined_coordination_retains_current_group(turn_id)
            || self.ensure_completion_admission_open().is_err()
            || self
                .agent_turn_ledger()
                .turn(turn_id)
                .is_none_or(|turn| turn.state != AgentTurnState::Running)
            || self
                .agent
                .claimed_agent_provider_tasks
                .contains_key(turn_id)
        {
            return false;
        }
        self.agent
            .pending_agent_provider_tasks
            .insert(turn_id.to_string())
    }

    /// Records only the actual accepted actor lease, without consuming mail.
    pub(in crate::runtime::agent) fn accept_joined_coordination_claim(
        &mut self,
        turn_id: &str,
        generation: u64,
    ) -> bool {
        if !self.joined_coordination_retains_current_group(turn_id) {
            return false;
        }
        let Some(owner) = self.agent.joined_coordination.get_mut(turn_id) else {
            return false;
        };
        if owner
            .claim
            .is_some_and(|(existing, _)| existing == generation)
        {
            return true;
        }
        let through = owner
            .mail
            .values()
            .map(|(_, sequence)| *sequence)
            .max()
            .unwrap_or(owner.consumed);
        owner.claim = Some((generation, through));
        true
    }

    /// Transfers the quiescent original group only when a fresh response reaches
    /// application. Actor ingress already checked its exact claim and canonical
    /// consumed boundary; synchronous test providers use the same source checks.
    pub(in crate::runtime::agent) fn begin_joined_coordination_response(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<bool> {
        if !self.parent_has_pending_joined_tasks(&turn.turn_id) {
            self.agent.joined_coordination.remove(&turn.turn_id);
            return Ok(false);
        }
        let applying = self
            .agent
            .joined_coordination
            .get(&turn.turn_id)
            .is_some_and(|owner| owner.applying);
        let group = super::super::provider_execution::provider_log_execution_group_id(execution)?;
        if !self.joined_coordination_offer_ready(&turn.turn_id) {
            if applying && let Some(owner) = self.agent.joined_coordination.get_mut(&turn.turn_id) {
                owner.applying_group = Some(group);
                owner.human_resume = None;
                owner.end_requested = coordination_end_requested(execution);
            }
            return Ok(applying);
        }
        self.suspend_joined_coordination_source(&turn.turn_id)?;
        let baseline = Arc::new(
            self.agent
                .joined_subagent_dependencies
                .values()
                .filter(|dependency| dependency.parent_turn_id == turn.turn_id)
                .map(|dependency| {
                    self.joined_original_group(dependency).and_then(|group| {
                        group
                            .map(|group| (group, dependency.parent_action_id.clone()))
                            .ok_or_else(|| {
                                MezError::conflict("joined question has no original group")
                            })
                    })
                })
                .collect::<Result<BTreeSet<_>>>()?,
        );
        let owner = self
            .agent
            .joined_coordination
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("joined coordination owner disappeared"))?;
        let through = owner.claim.map(|(_, through)| through).unwrap_or_else(|| {
            owner
                .mail
                .values()
                .map(|(_, sequence)| *sequence)
                .max()
                .unwrap_or(owner.consumed)
        });
        for (child, (dependency, sequence)) in &owner.mail {
            if *sequence > owner.consumed && *sequence <= through {
                owner.questions.insert(
                    child.clone(),
                    JoinedQuestion {
                        dependency: dependency.clone(),
                        sequence: *sequence,
                        baseline: baseline.clone(),
                    },
                );
            }
        }
        owner.end_requested = coordination_end_requested(execution);
        owner.consumed = owner.consumed.max(through);
        owner.claim = None;
        owner.applying = true;
        owner.applying_group = Some(group);
        owner.human_resume = None;
        Ok(true)
    }

    /// Fairly reacquires one blocked parent for still-unconsumed child mail.
    /// QueueFull preserves the exact committed wake for the next delivery sweep.
    pub(in crate::runtime::agent) fn resume_joined_coordination(
        &mut self,
        turn_id: &str,
    ) -> Result<bool> {
        if !self.joined_coordination_offer_ready(turn_id)
            || self
                .agent_turn_ledger()
                .turn(turn_id)
                .is_none_or(|turn| turn.state != AgentTurnState::Blocked)
            || !self
                .agent
                .agent_scheduler
                .waiting_turns()
                .any(|work| work.turn_id == turn_id)
        {
            return Ok(false);
        }
        self.ensure_completion_required_gates_ready(turn_id)?;
        match self.agent.agent_scheduler.requeue_waiting(turn_id) {
            Ok(_) => {}
            Err(error) if error.kind() == SchedulerErrorKind::QueueFull => return Ok(false),
            Err(error) => return Err(error.into()),
        }
        self.start_ready_agent_turns()?;
        Ok(true)
    }

    /// Retries receipts independently of the ordinary Wait-action wake marker.
    pub(in crate::runtime::agent) fn recover_joined_coordination_wakes(&mut self) -> Result<()> {
        let parents = self
            .agent
            .joined_subagent_dependencies
            .values()
            .map(|dependency| dependency.parent_turn_id.clone())
            .chain(self.agent.joined_coordination.keys().cloned())
            .collect::<std::collections::BTreeSet<_>>();
        for parent in parents {
            self.park_joined_coordination_after_settlement(&parent)?;
            self.resume_joined_coordination(&parent)?;
        }
        Ok(())
    }

    /// Returns a successful response to the same outstanding task joins without
    /// reissuing actions. Approval/error/native work retains its normal owner.
    pub(in crate::runtime) fn park_joined_coordination_after_settlement(
        &mut self,
        turn_id: &str,
    ) -> Result<bool> {
        if !self.parent_has_pending_joined_tasks(turn_id)
            || !self.joined_coordination_response_source_current(turn_id)
            || self.agent_turn_is_human_paused(turn_id)
            || self.joined_coordination_offer_ready(turn_id)
            || !self.joined_coordination_source_quiescent(turn_id)
            || self
                .agent
                .claimed_agent_provider_tasks
                .contains_key(turn_id)
        {
            return Ok(false);
        }
        let Some(turn) = self.agent_turn_ledger().turn(turn_id).cloned() else {
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            return Ok(false);
        }
        self.agent.agent_scheduler.wait_running(turn_id)?;
        self.agent_turn_ledger_mut()
            .finish_turn(turn_id, AgentTurnState::Blocked)?;
        self.agent.pending_agent_provider_tasks.remove(turn_id);
        if let Some(owner) = self.agent.joined_coordination.get_mut(turn_id)
            && (owner.questions.is_empty() || owner.end_requested)
        {
            owner.applying = false;
            owner.applying_group = None;
            owner.claim = None;
        }
        self.reconcile_active_turn_sleep_inhibition();
        self.start_ready_agent_turns()?;
        Ok(true)
    }

    /// Ends the accepted answer batch only after its ordinary actions settled.
    /// A parked Wait or unfinished effect keeps response ownership intact.
    pub(in crate::runtime::agent) fn joined_coordination_response_parked(&mut self, turn_id: &str) {
        if self.joined_coordination_source_quiescent(turn_id)
            && let Some(owner) = self.agent.joined_coordination.get_mut(turn_id)
            && (owner.questions.is_empty() || owner.end_requested)
        {
            owner.applying = false;
            owner.applying_group = None;
            owner.claim = None;
        }
    }

    /// Keeps receive receipts bounded to current exact child dependencies.
    /// An applying response may retain its one parent owner until settlement.
    pub(super) fn prune_joined_coordination_receipts(&mut self) {
        let dependencies = &self.agent.joined_subagent_dependencies;
        self.agent.joined_coordination.retain(|_, owner| {
            owner
                .mail
                .retain(|child, (dependency, _)| dependencies.get(child) == Some(dependency));
            owner
                .questions
                .retain(|child, question| dependencies.get(child) == Some(&question.dependency));
            owner.applying || !owner.mail.is_empty()
        });
    }

    /// Releases only the display barrier of an actually accepted joined spawn.
    /// Its result remains Running. Requiring the captured original group/action
    /// and live dependency prevents a forged or later response from bypassing
    /// ordered publication. This grants no provider or effect authority.
    pub(in crate::runtime::agent) fn joined_spawn_allows_later_logs(
        &self,
        execution: &AgentTurnExecution,
        action: &AgentAction,
    ) -> bool {
        if action.action_type() != "spawn_agent"
            || !execution.action_results.iter().any(|result| {
                result.action_id == action.id
                    && result.action_type == "spawn_agent"
                    && result.status == mez_agent::ActionStatus::Running
                    && !result.is_error
            })
        {
            return false;
        }
        let Ok(group) =
            super::super::provider_execution::provider_log_execution_group_id(execution)
        else {
            return false;
        };
        self.agent
            .joined_subagent_dependencies
            .values()
            .any(|dependency| {
                dependency.parent_turn_id == execution.request.turn_id
                    && dependency.parent_action_id == action.id
                    && self.joined_subagent_dependency_has_live_child(dependency)
                    && self
                        .retained_joined_spawn_action(dependency)
                        .is_ok_and(|retained| retained.as_ref() == Some(action))
                    && self
                        .joined_original_group(dependency)
                        .is_ok_and(|original| original.as_ref() == Some(&group))
            })
    }

    /// Exposes read-only coordination diagnostics for genuine lifecycle tests.
    #[cfg(test)]
    pub(crate) fn joined_coordination_state_for_tests(&self, turn_id: &str) -> String {
        format!(
            "owner={:?} current={} source={} quiescent={} offer={} execution={:?}",
            self.agent.joined_coordination.get(turn_id),
            self.joined_coordination_parent_current(turn_id),
            self.joined_coordination_response_source_current(turn_id),
            self.joined_coordination_source_quiescent(turn_id),
            self.joined_coordination_offer_ready(turn_id),
            self.agent_turn_executions()
                .get(turn_id)
                .map(|execution| (&execution.action_results, execution.final_turn))
        )
    }

    /// Counts exact retained question occurrences without creating a receipt.
    /// Redirection tests use this to verify bounded old-attempt retirement.
    #[cfg(test)]
    pub(crate) fn joined_coordination_receipt_count_for_tests(&self, turn_id: &str) -> usize {
        self.agent
            .joined_coordination
            .get(turn_id)
            .map_or(0, |owner| owner.mail.len())
    }

    /// Looks up the immutable original execution group after binding validation.
    pub(in crate::runtime::agent) fn joined_original_group(
        &self,
        dependency: &JoinedSubagentDependency,
    ) -> Result<Option<ContextExecutionGroupId>> {
        self.retained_joined_spawn_action(dependency)?;
        Ok(self
            .agent
            .joined_parent_executions
            .get(&dependency.child_turn_id)
            .map(|owner| owner.source.group.clone()))
    }
}

/// A final Say explicitly ends coordination; Wait's planned final-turn flag
/// instead requests a peer suspension and must retain the unanswered question.
fn coordination_end_requested(execution: &AgentTurnExecution) -> bool {
    execution
        .response
        .action_batch
        .as_ref()
        .is_some_and(|batch| {
            batch.actions.iter().any(|action| {
                matches!(
                    action.payload,
                    mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        ..
                    }
                )
            })
        })
}
