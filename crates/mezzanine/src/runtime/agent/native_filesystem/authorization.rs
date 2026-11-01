//! Exact human approval custody for native filesystem dispatch and commits.
//!
//! A prior Prompt evaluation is not approval. Only an actual decided request
//! bound to the original blocked action/group/source can mint this private
//! proof. Queued work retains it after the UI continuation reference is removed;
//! every later prompting authorization rechecks the actual original decision.

use super::*;
use crate::runtime::processes::native_filesystem_context::NativeFilesystemContext;
use mez_agent::permissions::{ApprovalDecision, BlockedApprovalRequest, BlockedApprovalState};
use sha2::{Digest, Sha256};

/// Immutable original request, exact payload and source; not a scope/write lease.
#[derive(Clone)]
pub(in crate::runtime::agent) struct NativeFilesystemApproval {
    /// Actual decided queue record, including the deciding controller identity.
    request: BlockedApprovalRequest,
    /// Exact original canonical group, independent of current presentation.
    group: mez_agent::ContextExecutionGroupId,
    /// Human approval binds raw accepted patch bytes rather than normalized work.
    payload: [u8; 32],
    /// Root/navigation/mode/config/child source admitted by that exact decision.
    source: NativeFilesystemContext,
}
impl NativeFilesystemApproval {
    /// Requires the original actual decision and exact frozen work on every use.
    pub(super) fn is_current(
        &self,
        service: &RuntimeSessionService,
        work: &NativeFilesystemWork,
    ) -> bool {
        let payload: [u8; 32] =
            Sha256::digest(work.identity.operation.exact_patch.as_bytes()).into();
        self.request.state == BlockedApprovalState::Approved
            && self.request.decision == Some(ApprovalDecision::Approve)
            && self.request.requesting_agent_id == work.owner.turn.agent_id
            && self.request.pane_id == work.owner.turn.pane_id
            && self.group == work.owner.group
            && self.payload == payload
            && self.source.same_mutation_context(&work.source)
            && service.blocked_approvals().get(&self.request.id) == Some(&self.request)
    }
}
impl RuntimeSessionService {
    /// Routes from immutable original admission, not a current mode or family.
    /// Missing volatile proof may only fail validation, never select executable
    /// shell work from an approval originally minted for native filesystem work.
    pub(in crate::runtime::agent) fn native_filesystem_action_requires_adapter(
        &self,
        turn: &str,
        action: &str,
    ) -> bool {
        self.agent
            .native_filesystem_actions
            .requires_native_adapter(turn, action)
            || self
                .agent
                .blocked_agent_approval_refs
                .values()
                .any(|reference| {
                    reference.native_filesystem_domain
                        && reference.turn_id == turn
                        && reference.action_id == action
                })
    }

    /// Fault injection removes evidence only, never original domain provenance.
    #[cfg(test)]
    pub(crate) fn discard_native_filesystem_approval_proof_for_tests(
        &mut self,
        turn: &str,
        action: &str,
    ) {
        self.agent
            .native_filesystem_actions
            .approvals
            .remove(&(turn.to_string(), action.to_string()));
    }

    /// Captures only an actual approved original blocked action, before its
    /// continuation reference leaves the queue. Missing/changed records fail
    /// closed; bounded pending proof storage grants no worker or mutation rights.
    pub(in crate::runtime::agent) fn retain_native_filesystem_approval(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        execution: &AgentTurnExecution,
        approval_id: &str,
    ) -> Result<()> {
        let reference = self
            .agent
            .blocked_agent_approval_refs
            .get(approval_id)
            .filter(|reference| {
                reference.turn_id == turn.turn_id && reference.action_id == action.id
            })
            .ok_or_else(|| MezError::conflict("native patch approval lost its original action"))?;
        let request = self
            .blocked_approvals()
            .get(approval_id)
            .filter(|request| {
                request.state == BlockedApprovalState::Approved
                    && request.decision == Some(ApprovalDecision::Approve)
                    && request.requesting_agent_id == turn.agent_id
                    && request.pane_id == turn.pane_id
                    && request.action_kind == "apply_patch"
            })
            .cloned()
            .ok_or_else(|| {
                MezError::forbidden("native patch lacks its actual approved decision")
            })?;
        if reference.sandbox_bypass_after_approval {
            return Err(MezError::conflict(
                "native filesystem approval cannot adopt a shell sandbox retry",
            ));
        }
        let AgentActionPayload::ApplyPatch { patch, strip: None } = &action.payload else {
            return Err(MezError::invalid_args(
                "native approval requires a semantic patch",
            ));
        };
        let group = super::super::provider_execution::provider_log_execution_group_id(execution)?;
        let source = self.capture_native_filesystem_context(turn)?;
        let payload: [u8; 32] = Sha256::digest(patch.as_bytes()).into();
        let key = (turn.turn_id.clone(), action.id.clone());
        let original = self
            .agent
            .native_filesystem_actions
            .approvals
            .get_mut(&key)
            .ok_or_else(|| {
                MezError::conflict("native patch approval lost its original pending source")
            })?;
        if original.request.id != request.id
            || original.group != group
            || original.payload != payload
            || !original.source.same_mutation_context(&source)
        {
            return Err(MezError::conflict(
                "native patch approval original source changed",
            ));
        }
        if original.request.state == BlockedApprovalState::Approved && original.request != request {
            return Err(MezError::conflict("native patch approved decision changed"));
        }
        original.request = request;
        Ok(())
    }

    /// Freezes exact action/source when the actual approval request is queued.
    /// Navigation or policy changes while the human decides cannot rebase it.
    pub(in crate::runtime::agent) fn reserve_native_filesystem_approval(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        execution: &AgentTurnExecution,
        approval_id: &str,
    ) -> Result<()> {
        let AgentActionPayload::ApplyPatch { patch, strip: None } = &action.payload else {
            return Err(MezError::invalid_args(
                "native approval requires a semantic patch",
            ));
        };
        let request = self
            .blocked_approvals()
            .get(approval_id)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("native patch pending approval is unavailable")
            })?;
        let source = self.capture_native_filesystem_context(turn)?;
        let approval = NativeFilesystemApproval {
            request,
            group: super::super::provider_execution::provider_log_execution_group_id(execution)?,
            payload: Sha256::digest(patch.as_bytes()).into(),
            source,
        };
        let key = (turn.turn_id.clone(), action.id.clone());
        if let Some(original) = self.agent.native_filesystem_actions.approvals.get(&key) {
            return if original.request.id == approval.request.id
                && original.group == approval.group
                && original.payload == approval.payload
                && original.source.same_mutation_context(&approval.source)
            {
                Ok(())
            } else {
                Err(MezError::conflict(
                    "native patch pending approval source changed",
                ))
            };
        }
        if self
            .agent
            .native_filesystem_actions
            .approvals
            .keys()
            .filter(|(owner, _)| owner == &turn.turn_id)
            .count()
            >= 1024
        {
            return Err(MezError::conflict(
                "native patch pending approval capacity exhausted",
            ));
        }
        self.agent
            .native_filesystem_actions
            .approvals
            .insert(key, approval);
        Ok(())
    }
}
