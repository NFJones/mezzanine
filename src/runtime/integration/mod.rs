//! Concrete product integration state ownership.
//!
//! This component owns live application bindings that join otherwise separate
//! configuration, security, provider, storage, and hook domains. Its state is
//! intentionally private: runtime adapters may borrow focused values through
//! typed operations, but the session coordinator does not expose a second
//! crate-wide field bag.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::async_runtime::AsyncRuntimeActorMetrics;
use crate::config::ConfigLayer;
use mez_agent::ApprovalPolicy;
use mez_agent::mcp::McpRegistry;
use mez_agent::memory::SessionMemoryStore;
use mez_agent::permissions::{BlockedApprovalQueue, PermissionPolicy, SessionApprovalStore};
use mez_agent::{
    PresetRegistry as RuntimePresetRegistry, ProviderRegistry as RuntimeProviderRegistry,
    SubagentProfile,
};

use super::service_state::{
    RuntimeAgentPersonalityProfile, RuntimeMcpTransportSet, RuntimeMetricsSnapshot,
    RuntimeModelProfileOverrideStore,
};

mod bindings;
mod security;

use bindings::RuntimeBindingsState;
use security::RuntimeSecurityState;

/// Owns concrete application integration bindings for one runtime session.
#[derive(Debug)]
pub(in crate::runtime) struct RuntimeIntegrationComponent {
    config_layers: Vec<ConfigLayer>,
    config_root: Option<PathBuf>,
    async_runtime_metrics: Option<AsyncRuntimeActorMetrics>,
    runtime_metrics: RuntimeMetricsSnapshot,
    security: RuntimeSecurityState,
    bindings: RuntimeBindingsState,
}

impl RuntimeIntegrationComponent {
    /// Builds integration ownership from constructor-resolved live catalogs.
    pub(in crate::runtime) fn new(
        provider_registry: RuntimeProviderRegistry,
        subagent_profiles: BTreeMap<String, SubagentProfile>,
    ) -> Self {
        Self {
            config_layers: Vec::new(),
            config_root: None,
            async_runtime_metrics: None,
            runtime_metrics: RuntimeMetricsSnapshot::default(),
            security: RuntimeSecurityState::default(),
            bindings: RuntimeBindingsState::new(provider_registry, subagent_profiles),
        }
    }

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

    /// Returns the canonical MCP registry.
    pub(in crate::runtime) fn mcp_registry(&self) -> &McpRegistry {
        self.bindings.mcp_registry()
    }

    /// Returns the MCP registry for lifecycle mutation.
    pub(in crate::runtime) fn mcp_registry_mut(&mut self) -> &mut McpRegistry {
        self.bindings.mcp_registry_mut()
    }

    /// Returns concrete MCP transport bindings for mutation.
    pub(in crate::runtime) fn mcp_transports_mut(&mut self) -> &mut RuntimeMcpTransportSet {
        self.bindings.mcp_transports_mut()
    }

    /// Returns the live provider registry.
    pub(in crate::runtime) fn provider_registry(&self) -> &RuntimeProviderRegistry {
        self.bindings.provider_registry()
    }

    /// Returns the live provider registry for mutation.
    pub(in crate::runtime) fn provider_registry_mut(&mut self) -> &mut RuntimeProviderRegistry {
        self.bindings.provider_registry_mut()
    }

    /// Replaces the live provider registry atomically.
    pub(in crate::runtime) fn replace_provider_registry(
        &mut self,
        registry: RuntimeProviderRegistry,
    ) {
        self.bindings.replace_provider_registry(registry);
    }

    /// Returns the live model preset registry.
    pub(in crate::runtime) fn preset_registry(&self) -> &RuntimePresetRegistry {
        self.bindings.preset_registry()
    }

    /// Returns the live model preset registry for mutation.
    pub(in crate::runtime) fn preset_registry_mut(&mut self) -> &mut RuntimePresetRegistry {
        self.bindings.preset_registry_mut()
    }

    /// Returns configured subagent profiles.
    pub(in crate::runtime) fn subagent_profiles(&self) -> &BTreeMap<String, SubagentProfile> {
        self.bindings.subagent_profiles()
    }

    /// Replaces configured subagent profiles atomically.
    pub(in crate::runtime) fn replace_subagent_profiles(
        &mut self,
        profiles: BTreeMap<String, SubagentProfile>,
    ) {
        self.bindings.replace_subagent_profiles(profiles);
    }

    /// Returns configured agent personality profiles.
    pub(in crate::runtime) fn agent_personality_profiles(
        &self,
    ) -> &BTreeMap<String, RuntimeAgentPersonalityProfile> {
        self.bindings.agent_personality_profiles()
    }

    /// Replaces configured agent personality profiles atomically.
    pub(in crate::runtime) fn replace_agent_personality_profiles(
        &mut self,
        profiles: BTreeMap<String, RuntimeAgentPersonalityProfile>,
    ) {
        self.bindings.replace_agent_personality_profiles(profiles);
    }

    /// Returns the configured default agent personality id.
    pub(in crate::runtime) fn default_agent_personality(&self) -> Option<&str> {
        self.bindings.default_agent_personality()
    }

    /// Replaces the configured default agent personality id.
    pub(in crate::runtime) fn set_default_agent_personality(
        &mut self,
        personality: Option<String>,
    ) {
        self.bindings.set_default_agent_personality(personality);
    }

    /// Returns the configured custom agent system prompt.
    pub(in crate::runtime) fn custom_agent_system_prompt(&self) -> Option<&str> {
        self.bindings.custom_agent_system_prompt()
    }

    /// Replaces the configured custom agent system prompt.
    pub(in crate::runtime) fn set_custom_agent_system_prompt(&mut self, prompt: Option<String>) {
        self.bindings.set_custom_agent_system_prompt(prompt);
    }

    /// Returns pane-local personality selections.
    pub(in crate::runtime) fn agent_personality_selections(&self) -> &BTreeMap<String, String> {
        self.bindings.agent_personality_selections()
    }

    /// Returns pane-local personality selections for mutation.
    pub(in crate::runtime) fn agent_personality_selections_mut(
        &mut self,
    ) -> &mut BTreeMap<String, String> {
        self.bindings.agent_personality_selections_mut()
    }

    /// Returns model-profile overrides.
    pub(in crate::runtime) fn model_profile_overrides(&self) -> &RuntimeModelProfileOverrideStore {
        self.bindings.model_profile_overrides()
    }

    /// Returns model-profile overrides for mutation.
    pub(in crate::runtime) fn model_profile_overrides_mut(
        &mut self,
    ) -> &mut RuntimeModelProfileOverrideStore {
        self.bindings.model_profile_overrides_mut()
    }
}
