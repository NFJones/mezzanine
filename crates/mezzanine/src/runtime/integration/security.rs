//! Session-scoped permission, approval, and memory authority state.

use std::collections::BTreeMap;

use mez_agent::memory::SessionMemoryStore;
use mez_agent::permissions::{BlockedApprovalQueue, PermissionPolicy, SessionApprovalStore};
use mez_agent::{ApprovalPolicy, PermissionPreset};

use crate::runtime::config::{ConfiguredPermissions, SandboxConfig};

/// Sparse live permission fields explicitly owned by one pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PanePermissionOverride {
    /// Optional pane-subtree permission preset override.
    pub(crate) preset: Option<PermissionPreset>,
    /// Optional pane-subtree approval-policy override.
    pub(crate) approval_policy: Option<ApprovalPolicy>,
    /// Optional sandbox backend owned only by this exact pane.
    pub(crate) sandbox: Option<SandboxConfig>,
}

impl PanePermissionOverride {
    /// Returns whether neither pane-subtree field is overridden.
    fn is_empty(&self) -> bool {
        self.preset.is_none() && self.approval_policy.is_none() && self.sandbox.is_none()
    }
}

/// Owns authority-bearing state that must change as one serialized session.
#[derive(Debug, Default)]
pub(super) struct RuntimeSecurityState {
    configured_permissions: ConfiguredPermissions,
    pane_permission_overrides: BTreeMap<String, PanePermissionOverride>,
    live_approval_bypass_override: Option<bool>,
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

    pub(super) fn pane_permission_override(&self, pane_id: &str) -> Option<PanePermissionOverride> {
        self.pane_permission_overrides.get(pane_id).cloned()
    }

    pub(super) fn set_pane_permission_preset_override(
        &mut self,
        pane_id: &str,
        value: Option<PermissionPreset>,
    ) {
        let entry = self
            .pane_permission_overrides
            .entry(pane_id.to_string())
            .or_default();
        entry.preset = value;
        if entry.is_empty() {
            self.pane_permission_overrides.remove(pane_id);
        }
    }

    pub(super) fn set_pane_approval_policy_override(
        &mut self,
        pane_id: &str,
        value: Option<ApprovalPolicy>,
    ) {
        let entry = self
            .pane_permission_overrides
            .entry(pane_id.to_string())
            .or_default();
        entry.approval_policy = value;
        if entry.is_empty() {
            self.pane_permission_overrides.remove(pane_id);
        }
    }

    pub(super) fn set_pane_sandbox_override(
        &mut self,
        pane_id: &str,
        value: Option<SandboxConfig>,
    ) {
        let entry = self
            .pane_permission_overrides
            .entry(pane_id.to_string())
            .or_default();
        entry.sandbox = value;
        if entry.is_empty() {
            self.pane_permission_overrides.remove(pane_id);
        }
    }

    pub(super) fn remove_pane_permission_override(&mut self, pane_id: &str) {
        self.pane_permission_overrides.remove(pane_id);
    }

    pub(super) fn clear_pane_permission_overrides(&mut self) {
        self.pane_permission_overrides.clear();
    }

    pub(super) fn live_approval_bypass_override(&self) -> Option<bool> {
        self.live_approval_bypass_override
    }

    pub(super) fn set_live_approval_bypass_override(&mut self, value: Option<bool>) {
        self.live_approval_bypass_override = value;
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
