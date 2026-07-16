//! Concrete product integration state ownership.
//!
//! This component owns live application bindings that join otherwise separate
//! configuration, security, provider, storage, and hook domains. Its state is
//! intentionally private: runtime adapters may borrow focused values through
//! typed operations, but the session coordinator does not expose a second
//! crate-wide field bag.

use std::path::{Path, PathBuf};

use crate::async_runtime::AsyncRuntimeActorMetrics;
use crate::config::ConfigLayer;
use mez_agent::ApprovalPolicy;
use mez_agent::memory::SessionMemoryStore;
use mez_agent::permissions::{BlockedApprovalQueue, PermissionPolicy, SessionApprovalStore};

use super::service_state::RuntimeMetricsSnapshot;

mod security;

use security::RuntimeSecurityState;

/// Owns concrete application integration bindings for one runtime session.
#[derive(Debug, Default)]
pub(in crate::runtime) struct RuntimeIntegrationComponent {
    config_layers: Vec<ConfigLayer>,
    config_root: Option<PathBuf>,
    async_runtime_metrics: Option<AsyncRuntimeActorMetrics>,
    runtime_metrics: RuntimeMetricsSnapshot,
    security: RuntimeSecurityState,
}

impl RuntimeIntegrationComponent {
    /// Returns the active configuration layers in precedence order.
    pub(in crate::runtime) fn config_layers(&self) -> &[ConfigLayer] {
        &self.config_layers
    }

    /// Returns active configuration layers for transactional mutation.
    pub(in crate::runtime) fn config_layers_mut(&mut self) -> &mut Vec<ConfigLayer> {
        &mut self.config_layers
    }

    /// Replaces every active configuration layer atomically.
    pub(in crate::runtime) fn replace_config_layers(&mut self, layers: Vec<ConfigLayer>) {
        self.config_layers = layers;
    }

    /// Returns the optional project configuration root.
    pub(in crate::runtime) fn config_root(&self) -> Option<&Path> {
        self.config_root.as_deref()
    }

    /// Replaces the optional project configuration root.
    pub(in crate::runtime) fn set_config_root(&mut self, root: Option<PathBuf>) {
        self.config_root = root;
    }

    /// Returns the latest async-actor metrics snapshot.
    pub(in crate::runtime) fn async_runtime_metrics(&self) -> Option<&AsyncRuntimeActorMetrics> {
        self.async_runtime_metrics.as_ref()
    }

    /// Replaces the latest async-actor metrics snapshot.
    pub(in crate::runtime) fn set_async_runtime_metrics(
        &mut self,
        metrics: Option<AsyncRuntimeActorMetrics>,
    ) {
        self.async_runtime_metrics = metrics;
    }

    /// Returns application runtime metrics.
    pub(in crate::runtime) fn runtime_metrics(&self) -> &RuntimeMetricsSnapshot {
        &self.runtime_metrics
    }

    /// Returns application runtime metrics for serialized mutation.
    pub(in crate::runtime) fn runtime_metrics_mut(&mut self) -> &mut RuntimeMetricsSnapshot {
        &mut self.runtime_metrics
    }

    /// Returns the effective session permission policy.
    pub(in crate::runtime) fn permission_policy(&self) -> &PermissionPolicy {
        self.security.permission_policy()
    }

    /// Returns the permission policy for serialized mutation.
    pub(in crate::runtime) fn permission_policy_mut(&mut self) -> &mut PermissionPolicy {
        self.security.permission_policy_mut()
    }

    /// Replaces the effective session permission policy.
    pub(in crate::runtime) fn replace_permission_policy(&mut self, policy: PermissionPolicy) {
        self.security.replace_permission_policy(policy);
    }

    /// Returns the explicit live approval-bypass override.
    pub(in crate::runtime) fn live_approval_bypass_override(&self) -> Option<bool> {
        self.security.live_approval_bypass_override()
    }

    /// Replaces the explicit live approval-bypass override.
    pub(in crate::runtime) fn set_live_approval_bypass_override(&mut self, value: Option<bool>) {
        self.security.set_live_approval_bypass_override(value);
    }

    /// Returns the explicit live approval-policy override.
    pub(in crate::runtime) fn live_approval_policy_override(&self) -> Option<ApprovalPolicy> {
        self.security.live_approval_policy_override()
    }

    /// Replaces the explicit live approval-policy override.
    pub(in crate::runtime) fn set_live_approval_policy_override(
        &mut self,
        value: Option<ApprovalPolicy>,
    ) {
        self.security.set_live_approval_policy_override(value);
    }

    /// Returns the blocked approval queue.
    pub(in crate::runtime) fn blocked_approvals(&self) -> &BlockedApprovalQueue {
        self.security.blocked_approvals()
    }

    /// Returns the blocked approval queue for decision mutation.
    pub(in crate::runtime) fn blocked_approvals_mut(&mut self) -> &mut BlockedApprovalQueue {
        self.security.blocked_approvals_mut()
    }

    /// Clears all blocked approval requests during session replacement.
    pub(in crate::runtime) fn reset_blocked_approvals(&mut self) {
        self.security.reset_blocked_approvals();
    }

    /// Returns session-scoped approval grants.
    pub(in crate::runtime) fn session_approvals(&self) -> &SessionApprovalStore {
        self.security.session_approvals()
    }

    /// Returns session-scoped approval grants for decision mutation.
    pub(in crate::runtime) fn session_approvals_mut(&mut self) -> &mut SessionApprovalStore {
        self.security.session_approvals_mut()
    }

    /// Clears all session approval grants during session replacement.
    pub(in crate::runtime) fn reset_session_approvals(&mut self) {
        self.security.reset_session_approvals();
    }

    /// Returns session-scoped in-memory records.
    pub(in crate::runtime) fn session_memory(&self) -> &SessionMemoryStore {
        self.security.session_memory()
    }

    /// Returns session-scoped in-memory records for mutation.
    pub(in crate::runtime) fn session_memory_mut(&mut self) -> &mut SessionMemoryStore {
        self.security.session_memory_mut()
    }
}
