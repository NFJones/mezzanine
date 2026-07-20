//! Session-scoped permission, approval, and memory authority state.

use mez_agent::ApprovalPolicy;
use mez_agent::memory::SessionMemoryStore;
use mez_agent::permissions::{BlockedApprovalQueue, PermissionPolicy, SessionApprovalStore};

use crate::runtime::config::ConfiguredPermissions;

/// Owns authority-bearing state that must change as one serialized session.
#[derive(Debug, Default)]
pub(super) struct RuntimeSecurityState {
    configured_permissions: ConfiguredPermissions,
    live_approval_bypass_override: Option<bool>,
    live_approval_policy_override: Option<ApprovalPolicy>,
    blocked_approvals: BlockedApprovalQueue,
    session_approvals: SessionApprovalStore,
    session_memory: SessionMemoryStore,
}

impl RuntimeSecurityState {
    pub(super) fn permission_policy(&self) -> &PermissionPolicy {
        &self.configured_permissions.authorization
    }

    pub(super) fn permission_policy_mut(&mut self) -> &mut PermissionPolicy {
        &mut self.configured_permissions.authorization
    }

    pub(super) fn configured_permissions(&self) -> &ConfiguredPermissions {
        &self.configured_permissions
    }

    pub(super) fn replace_configured_permissions(&mut self, permissions: ConfiguredPermissions) {
        self.configured_permissions = permissions;
    }

    pub(super) fn live_approval_bypass_override(&self) -> Option<bool> {
        self.live_approval_bypass_override
    }

    pub(super) fn set_live_approval_bypass_override(&mut self, value: Option<bool>) {
        self.live_approval_bypass_override = value;
    }

    pub(super) fn live_approval_policy_override(&self) -> Option<ApprovalPolicy> {
        self.live_approval_policy_override
    }

    pub(super) fn set_live_approval_policy_override(&mut self, value: Option<ApprovalPolicy>) {
        self.live_approval_policy_override = value;
    }

    pub(super) fn blocked_approvals(&self) -> &BlockedApprovalQueue {
        &self.blocked_approvals
    }

    pub(super) fn blocked_approvals_mut(&mut self) -> &mut BlockedApprovalQueue {
        &mut self.blocked_approvals
    }

    pub(super) fn reset_blocked_approvals(&mut self) {
        self.blocked_approvals = BlockedApprovalQueue::default();
    }

    pub(super) fn session_approvals(&self) -> &SessionApprovalStore {
        &self.session_approvals
    }

    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub(super) fn session_approvals_mut(&mut self) -> &mut SessionApprovalStore {
        &mut self.session_approvals
    }

    pub(super) fn reset_session_approvals(&mut self) {
        self.session_approvals = SessionApprovalStore::default();
    }

    pub(super) fn session_memory(&self) -> &SessionMemoryStore {
        &self.session_memory
    }

    pub(super) fn session_memory_mut(&mut self) -> &mut SessionMemoryStore {
        &mut self.session_memory
    }
}
