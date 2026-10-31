//! Actor-owned filesystem attempt admission and immutable result custody.
//!
//! Original canonical group/action ownership is captured before dispatch and
//! retained independently of later presentation. Opaque allocation-bound handles
//! claim work once; cancellation fences future work without discarding a known
//! result. Scope snapshots are deliberately not atomic commit leases. The
//! asynchronous producer/commit-lease/settlement bridge must use these owners
//! before switching native patches away from their existing shell adapter.

mod state;
pub(super) use state::NativeFilesystemActions;
pub(crate) use state::{
    NativeFilesystemAttempt, NativeFilesystemRegisteredSource, RuntimeNativeFilesystemDispatch,
};
use state::{NativeFilesystemOwner, NativeFilesystemWork};

use std::sync::Arc;
use std::time::{Duration, Instant};

use mez_agent::native_action::{NativeCommitIdentity, NativePatchOperation};
use mez_agent::permissions::{PathScopes, RuleDecision};
use mez_agent::{
    AgentAction, AgentActionPayload, AgentTurnExecution, AgentTurnRecord, AgentTurnState,
};

use crate::error::{MezError, Result};
use crate::runtime::RuntimeSessionService;
use crate::runtime::processes::native_patch::worker::NativePatchWorkerOutcome;

impl RuntimeSessionService {
    /// Queues an exact canonical native patch slot without lowering shell code.
    /// This captures ownership only; claiming and current scopes are separate,
    /// and no producer or mutation is started by registering this record.
    pub(crate) fn queue_native_filesystem_action(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        execution: &AgentTurnExecution,
    ) -> Result<NativeFilesystemAttempt> {
        self.ensure_completion_admission_open()?;
        if self.agent_turn_is_human_paused(&turn.turn_id) || !self.native_filesystem_turn_live(turn)
        {
            return Err(MezError::conflict(
                "native filesystem turn is not claimable",
            ));
        }
        let AgentActionPayload::ApplyPatch { patch, strip } = &action.payload else {
            return Err(MezError::invalid_args(
                "native filesystem execution requires apply_patch",
            ));
        };
        let operation = NativePatchOperation::parse(patch, *strip)?;
        let owner = self.native_filesystem_slot(turn, action, execution)?;
        let source = self.capture_native_filesystem_context(turn)?;
        let deadline = if turn.deadline_at_unix_millis == 0 {
            None
        } else {
            let remaining = turn
                .deadline_at_unix_millis
                .saturating_sub(crate::runtime::current_unix_millis());
            if remaining == 0 {
                return Err(MezError::conflict(
                    "native filesystem turn deadline expired",
                ));
            }
            Some(
                Instant::now()
                    .checked_add(Duration::from_millis(remaining.min(30_000)))
                    .ok_or_else(|| {
                        MezError::invalid_state("native filesystem turn deadline overflow")
                    })?,
            )
        };
        self.agent.native_filesystem_actions.queue(
            owner,
            source,
            operation,
            self.session.config_generation,
            deadline,
        )
    }

    /// Freezes the original slot from the actor's canonical group map. Supplied
    /// action data must equal its unique authored occurrence and running result.
    fn native_filesystem_slot(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        execution: &AgentTurnExecution,
    ) -> Result<NativeFilesystemOwner> {
        if execution.request.turn_id != turn.turn_id || execution.request.agent_id != turn.agent_id
        {
            return Err(MezError::invalid_state(
                "native filesystem execution attribution changed",
            ));
        }
        let batch =
            execution.response.action_batch.as_ref().ok_or_else(|| {
                MezError::invalid_state("native filesystem action batch unavailable")
            })?;
        let matches = batch
            .actions
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.id == action.id)
            .collect::<Vec<_>>();
        let [(ordinal, original)] = matches.as_slice() else {
            return Err(MezError::invalid_state(
                "native filesystem action occurrence is not unique",
            ));
        };
        if *original != action {
            return Err(MezError::conflict(
                "native filesystem authored action changed",
            ));
        }
        let results = execution
            .action_results
            .iter()
            .filter(|result| result.action_id == action.id)
            .collect::<Vec<_>>();
        let [result] = results.as_slice() else {
            return Err(MezError::invalid_state(
                "native filesystem result slot is not unique",
            ));
        };
        if result.status != mez_agent::ActionStatus::Running
            || result.turn_id != turn.turn_id
            || result.agent_id != turn.agent_id
            || result.action_type != action.action_type()
        {
            return Err(MezError::conflict(
                "native filesystem canonical result is not pending",
            ));
        }
        let group = super::provider_execution::provider_log_execution_group_id(execution)?;
        if self
            .agent
            .agent_execution_groups_by_turn
            .get(&turn.turn_id)
            .and_then(|groups| groups.get(&action.id))
            != Some(&group)
        {
            return Err(MezError::conflict(
                "native filesystem canonical group binding changed",
            ));
        }
        Ok(NativeFilesystemOwner {
            turn: turn.clone(),
            action: action.clone(),
            group,
            ordinal: *ordinal,
            permission: result.permission_evaluation.clone(),
        })
    }

    /// Checks original logical turn liveness without using a current pane label
    /// as authority. Joined waiting parents retain their accepted logical turn.
    fn native_filesystem_turn_live(&self, owner: &AgentTurnRecord) -> bool {
        !self.subagent_descendant_is_fenced(&owner.agent_id)
            && self.agent_turn_ledger().turns().iter().any(|turn| {
                turn.turn_id == owner.turn_id
                    && turn.agent_id == owner.agent_id
                    && turn.pane_id == owner.pane_id
                    && turn.conversation_id == owner.conversation_id
                    && (turn.state == AgentTurnState::Running
                        || (turn.state == AgentTurnState::Blocked
                            && self
                                .agent
                                .agent_scheduler
                                .waiting_turns()
                                .any(|work| work.turn_id == owner.turn_id)))
            })
    }

    /// Claims queued work exactly once after current canonical/source/policy
    /// validation. A refusal retains positive unissued ownership for settlement.
    /// No commit permission or native producer-join evidence is returned here.
    pub(crate) fn claim_native_filesystem_action(
        &mut self,
        attempt: &NativeFilesystemAttempt,
    ) -> Result<Option<RuntimeNativeFilesystemDispatch>> {
        self.ensure_completion_admission_open()?;
        let Some(work) = self.agent.native_filesystem_actions.queued(attempt) else {
            return Ok(None);
        };
        if self.agent_turn_is_human_paused(&work.owner.turn.turn_id)
            || self
                .agent
                .pending_agent_provider_persistence
                .contains(&work.owner.turn.turn_id)
        {
            return Ok(None);
        }
        let result = (|| {
            if !self.native_filesystem_turn_live(&work.owner.turn) {
                return Err(MezError::conflict(
                    "native filesystem original turn retired before claim",
                ));
            }
            // A different current response is presentation, not a replacement
            // of this registered original slot. Validate edits to the same
            // canonical group without rebasing an older accepted group.
            if let Some(execution) = self.agent_turn_executions().get(&work.owner.turn.turn_id) {
                let current_group =
                    super::provider_execution::provider_log_execution_group_id(execution)?;
                if current_group == work.owner.group {
                    let current = self.native_filesystem_slot(
                        &work.owner.turn,
                        &work.owner.action,
                        execution,
                    )?;
                    if current.ordinal != work.owner.ordinal {
                        return Err(MezError::conflict(
                            "native filesystem original occurrence changed before claim",
                        ));
                    }
                } else if execution
                    .response
                    .action_batch
                    .as_ref()
                    .is_some_and(|batch| {
                        batch.actions.iter().any(|action| {
                            self.agent
                                .agent_execution_groups_by_turn
                                .get(&work.owner.turn.turn_id)
                                .and_then(|groups| groups.get(&action.id))
                                != Some(&current_group)
                        })
                    })
                {
                    return Err(MezError::conflict(
                        "native filesystem replacement projection is not canonically registered",
                    ));
                }
            }
            let registered = self
                .agent
                .native_filesystem_actions
                .registered_source(attempt)?;
            let scopes = self
                .native_filesystem_scopes_for_registered_owner(&registered)?
                .ok_or_else(|| {
                    MezError::forbidden("native filesystem source has no path authority")
                })?;
            self.check_native_filesystem_policy(&work, &scopes)?;
            self.agent.native_filesystem_actions.claim(attempt, scopes)
        })();
        if let Err(error) = &result {
            self.agent
                .native_filesystem_actions
                .reject_queued(attempt, error.clone());
        }
        result
    }

    /// Rechecks the non-executable fixed classifier and immutable approval
    /// evidence. Descriptor analysis selects no shell executable or environment;
    /// concrete path authority remains independently enforced by capabilities.
    fn check_native_filesystem_policy(
        &self,
        work: &NativeFilesystemWork,
        scopes: &PathScopes,
    ) -> Result<()> {
        let policy = self.permission_policy_for_turn(&work.owner.turn);
        let current = policy.evaluate_shell_command_structured_with_approvals_scoped(
            work.identity.operation.policy_descriptor(),
            self.session_approvals(),
            Some(scopes),
        );
        match current.decision {
            RuleDecision::Forbid => Err(MezError::forbidden(
                "native filesystem action forbidden by current policy",
            )),
            RuleDecision::Prompt
                if work
                    .owner
                    .permission
                    .as_ref()
                    .is_none_or(|original| original.decision != RuleDecision::Prompt) =>
            {
                Err(MezError::conflict(
                    "native filesystem action requires fresh exact approval",
                ))
            }
            RuleDecision::Allow | RuleDecision::Prompt => Ok(()),
        }
    }

    /// Reads current scopes for one issued attempt, clipped to its initial
    /// ceiling. Original registration survives presentation changes; this
    /// snapshot is still not an atomic actor commit lease or durability proof.
    pub(crate) fn native_filesystem_scope_snapshot(
        &mut self,
        attempt: &NativeFilesystemAttempt,
        identity: &NativeCommitIdentity,
    ) -> Result<PathScopes> {
        let result = (|| {
            let (work, ceiling) = self.agent.native_filesystem_actions.issued(attempt)?;
            if &work.identity != identity
                || !self.native_filesystem_turn_live(&work.owner.turn)
                || self.agent_turn_is_human_paused(&work.owner.turn.turn_id)
            {
                return Err(MezError::conflict("native filesystem issued owner changed"));
            }
            let registered = self
                .agent
                .native_filesystem_actions
                .registered_source(attempt)?;
            let current = self
                .native_filesystem_scopes_for_registered_owner(&registered)?
                .ok_or_else(|| {
                    MezError::forbidden("native filesystem current authority unavailable")
                })?;
            self.check_native_filesystem_policy(&work, &current)?;
            ceiling
                .intersection(&current)
                .map_err(|error| MezError::forbidden(error.message()))
        })();
        if result.is_err() {
            self.agent.native_filesystem_actions.cancel_attempt(attempt);
        }
        result
    }

    /// Validates genuine retained registration without using a presentation CID
    /// as authority. Scope helpers cannot construct or refresh this evidence.
    pub(crate) fn native_filesystem_registered_source_is_current(
        &self,
        source: &NativeFilesystemRegisteredSource,
    ) -> bool {
        self.agent
            .native_filesystem_actions
            .registered_source_current(source)
            && self.native_filesystem_turn_live(source.turn())
            && !self.agent_turn_is_human_paused(&source.turn().turn_id)
    }

    /// Retains actual immutable worker evidence under its original actor owner.
    /// Late known effects survive cancellation and current presentation changes;
    /// this receipt still requires canonical/durable settlement and actual join.
    pub(crate) fn retain_native_filesystem_outcome(
        &mut self,
        attempt: &NativeFilesystemAttempt,
        outcome: Arc<NativePatchWorkerOutcome>,
    ) -> Result<bool> {
        self.agent
            .native_filesystem_actions
            .retain(attempt, outcome)
    }

    /// Reads original retained evidence without replay or mutable access.
    pub(crate) fn retained_native_filesystem_outcome(
        &self,
        attempt: &NativeFilesystemAttempt,
    ) -> Option<Arc<NativePatchWorkerOutcome>> {
        self.agent.native_filesystem_actions.retained(attempt)
    }

    /// Reports an exact preparation/validation failure without consuming effects.
    pub(crate) fn native_filesystem_attempt_failure(
        &self,
        attempt: &NativeFilesystemAttempt,
    ) -> Option<&MezError> {
        self.agent.native_filesystem_actions.failure(attempt)
    }

    /// Transfers only an unissued original owner for canonical refusal handling.
    /// An issued worker can never be retired from cancellation flags alone.
    pub(crate) fn take_unissued_native_filesystem_owner(
        &mut self,
        attempt: &NativeFilesystemAttempt,
    ) -> Result<Arc<NativeFilesystemWork>> {
        self.agent.native_filesystem_actions.take_unissued(attempt)
    }
}
