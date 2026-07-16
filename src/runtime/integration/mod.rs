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
use crate::auth::AuthStore;
use crate::config::ConfigLayer;
use crate::hooks::{FocusedShellHookQueue, HookDefinition, HookExecutionResult};
use crate::project::ProjectTrustStore;
use mez_agent::ApprovalPolicy;
use mez_agent::mcp::McpRegistry;
use mez_agent::memory::SessionMemoryStore;
use mez_agent::permissions::{BlockedApprovalQueue, PermissionPolicy, SessionApprovalStore};
use mez_agent::{
    PresetRegistry as RuntimePresetRegistry, ProviderRegistry as RuntimeProviderRegistry,
    SubagentProfile,
};

use super::service_state::{
    PendingFocusedShellHookTransaction, RuntimeAgentPersonalityProfile, RuntimeMcpTransportSet,
    RuntimeMetricsSnapshot, RuntimeModelProfileOverrideStore,
};

mod bindings;
mod credentials;
mod hooks;
mod security;

use bindings::RuntimeBindingsState;
use credentials::RuntimeCredentialState;
use hooks::RuntimeHookState;
use security::RuntimeSecurityState;

/// Owns concrete application integration bindings for one runtime session.
#[derive(Debug)]
pub(crate) struct RuntimeIntegrationComponent {
    config_layers: Vec<ConfigLayer>,
    config_root: Option<PathBuf>,
    async_runtime_metrics: Option<AsyncRuntimeActorMetrics>,
    runtime_metrics: RuntimeMetricsSnapshot,
    security: RuntimeSecurityState,
    bindings: RuntimeBindingsState,
    credentials: RuntimeCredentialState,
    hooks: RuntimeHookState,
}

impl RuntimeIntegrationComponent {
    /// Builds integration ownership from constructor-resolved live catalogs.
    pub(crate) fn new(
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
            credentials: RuntimeCredentialState::default(),
            hooks: RuntimeHookState::default(),
        }
    }

    /// Returns the active configuration layers in precedence order.
    pub(crate) fn config_layers(&self) -> &[ConfigLayer] {
        &self.config_layers
    }

    /// Returns active configuration layers for transactional mutation.
    pub(crate) fn config_layers_mut(&mut self) -> &mut Vec<ConfigLayer> {
        &mut self.config_layers
    }

    /// Replaces every active configuration layer atomically.
    pub(crate) fn replace_config_layers(&mut self, layers: Vec<ConfigLayer>) {
        self.config_layers = layers;
    }

    /// Returns the optional project configuration root.
    pub(crate) fn config_root(&self) -> Option<&Path> {
        self.config_root.as_deref()
    }

    /// Replaces the optional project configuration root.
    pub(crate) fn set_config_root(&mut self, root: Option<PathBuf>) {
        self.config_root = root;
    }

    /// Returns the latest async-actor metrics snapshot.
    pub(crate) fn async_runtime_metrics(&self) -> Option<&AsyncRuntimeActorMetrics> {
        self.async_runtime_metrics.as_ref()
    }

    /// Replaces the latest async-actor metrics snapshot.
    pub(crate) fn set_async_runtime_metrics(&mut self, metrics: Option<AsyncRuntimeActorMetrics>) {
        self.async_runtime_metrics = metrics;
    }

    /// Returns application runtime metrics.
    pub(crate) fn runtime_metrics(&self) -> &RuntimeMetricsSnapshot {
        &self.runtime_metrics
    }

    /// Returns application runtime metrics for serialized mutation.
    pub(crate) fn runtime_metrics_mut(&mut self) -> &mut RuntimeMetricsSnapshot {
        &mut self.runtime_metrics
    }

    /// Returns the effective session permission policy.
    pub(crate) fn permission_policy(&self) -> &PermissionPolicy {
        self.security.permission_policy()
    }

    /// Returns the permission policy for serialized mutation.
    pub(crate) fn permission_policy_mut(&mut self) -> &mut PermissionPolicy {
        self.security.permission_policy_mut()
    }

    /// Replaces the effective session permission policy.
    pub(crate) fn replace_permission_policy(&mut self, policy: PermissionPolicy) {
        self.security.replace_permission_policy(policy);
    }

    /// Returns the explicit live approval-bypass override.
    pub(crate) fn live_approval_bypass_override(&self) -> Option<bool> {
        self.security.live_approval_bypass_override()
    }

    /// Replaces the explicit live approval-bypass override.
    pub(crate) fn set_live_approval_bypass_override(&mut self, value: Option<bool>) {
        self.security.set_live_approval_bypass_override(value);
    }

    /// Returns the explicit live approval-policy override.
    pub(crate) fn live_approval_policy_override(&self) -> Option<ApprovalPolicy> {
        self.security.live_approval_policy_override()
    }

    /// Replaces the explicit live approval-policy override.
    pub(crate) fn set_live_approval_policy_override(&mut self, value: Option<ApprovalPolicy>) {
        self.security.set_live_approval_policy_override(value);
    }

    /// Returns the blocked approval queue.
    pub(crate) fn blocked_approvals(&self) -> &BlockedApprovalQueue {
        self.security.blocked_approvals()
    }

    /// Returns the blocked approval queue for decision mutation.
    pub(crate) fn blocked_approvals_mut(&mut self) -> &mut BlockedApprovalQueue {
        self.security.blocked_approvals_mut()
    }

    /// Clears all blocked approval requests during session replacement.
    pub(crate) fn reset_blocked_approvals(&mut self) {
        self.security.reset_blocked_approvals();
    }

    /// Returns session-scoped approval grants.
    pub(crate) fn session_approvals(&self) -> &SessionApprovalStore {
        self.security.session_approvals()
    }

    /// Returns session-scoped approval grants for decision mutation.
    pub(crate) fn session_approvals_mut(&mut self) -> &mut SessionApprovalStore {
        self.security.session_approvals_mut()
    }

    /// Clears all session approval grants during session replacement.
    pub(crate) fn reset_session_approvals(&mut self) {
        self.security.reset_session_approvals();
    }

    /// Returns session-scoped in-memory records.
    pub(crate) fn session_memory(&self) -> &SessionMemoryStore {
        self.security.session_memory()
    }

    /// Returns session-scoped in-memory records for mutation.
    pub(crate) fn session_memory_mut(&mut self) -> &mut SessionMemoryStore {
        self.security.session_memory_mut()
    }

    /// Returns the canonical MCP registry.
    pub(crate) fn mcp_registry(&self) -> &McpRegistry {
        self.bindings.mcp_registry()
    }

    /// Returns the MCP registry for lifecycle mutation.
    pub(crate) fn mcp_registry_mut(&mut self) -> &mut McpRegistry {
        self.bindings.mcp_registry_mut()
    }

    /// Returns concrete MCP transport bindings for mutation.
    pub(crate) fn mcp_transports_mut(&mut self) -> &mut RuntimeMcpTransportSet {
        self.bindings.mcp_transports_mut()
    }

    /// Borrows disjoint MCP transport and credential bindings for one execution.
    pub(crate) fn mcp_execution_bindings(
        &mut self,
    ) -> (&mut RuntimeMcpTransportSet, Option<&AuthStore>) {
        (
            self.bindings.mcp_transports_mut(),
            self.credentials.auth_store(),
        )
    }

    /// Returns the live provider registry.
    pub(crate) fn provider_registry(&self) -> &RuntimeProviderRegistry {
        self.bindings.provider_registry()
    }

    /// Returns the live provider registry for mutation.
    pub(crate) fn provider_registry_mut(&mut self) -> &mut RuntimeProviderRegistry {
        self.bindings.provider_registry_mut()
    }

    /// Replaces the live provider registry atomically.
    pub(crate) fn replace_provider_registry(&mut self, registry: RuntimeProviderRegistry) {
        self.bindings.replace_provider_registry(registry);
    }

    /// Returns the live model preset registry.
    pub(crate) fn preset_registry(&self) -> &RuntimePresetRegistry {
        self.bindings.preset_registry()
    }

    /// Returns the live model preset registry for mutation.
    pub(crate) fn preset_registry_mut(&mut self) -> &mut RuntimePresetRegistry {
        self.bindings.preset_registry_mut()
    }

    /// Returns configured subagent profiles.
    pub(crate) fn subagent_profiles(&self) -> &BTreeMap<String, SubagentProfile> {
        self.bindings.subagent_profiles()
    }

    /// Replaces configured subagent profiles atomically.
    pub(crate) fn replace_subagent_profiles(
        &mut self,
        profiles: BTreeMap<String, SubagentProfile>,
    ) {
        self.bindings.replace_subagent_profiles(profiles);
    }

    /// Returns configured agent personality profiles.
    pub(crate) fn agent_personality_profiles(
        &self,
    ) -> &BTreeMap<String, RuntimeAgentPersonalityProfile> {
        self.bindings.agent_personality_profiles()
    }

    /// Replaces configured agent personality profiles atomically.
    pub(crate) fn replace_agent_personality_profiles(
        &mut self,
        profiles: BTreeMap<String, RuntimeAgentPersonalityProfile>,
    ) {
        self.bindings.replace_agent_personality_profiles(profiles);
    }

    /// Returns the configured default agent personality id.
    pub(crate) fn default_agent_personality(&self) -> Option<&str> {
        self.bindings.default_agent_personality()
    }

    /// Replaces the configured default agent personality id.
    pub(crate) fn set_default_agent_personality(&mut self, personality: Option<String>) {
        self.bindings.set_default_agent_personality(personality);
    }

    /// Returns the configured custom agent system prompt.
    pub(crate) fn custom_agent_system_prompt(&self) -> Option<&str> {
        self.bindings.custom_agent_system_prompt()
    }

    /// Replaces the configured custom agent system prompt.
    pub(crate) fn set_custom_agent_system_prompt(&mut self, prompt: Option<String>) {
        self.bindings.set_custom_agent_system_prompt(prompt);
    }

    /// Returns pane-local personality selections.
    pub(crate) fn agent_personality_selections(&self) -> &BTreeMap<String, String> {
        self.bindings.agent_personality_selections()
    }

    /// Returns pane-local personality selections for mutation.
    pub(crate) fn agent_personality_selections_mut(&mut self) -> &mut BTreeMap<String, String> {
        self.bindings.agent_personality_selections_mut()
    }

    /// Returns model-profile overrides.
    pub(crate) fn model_profile_overrides(&self) -> &RuntimeModelProfileOverrideStore {
        self.bindings.model_profile_overrides()
    }

    /// Returns model-profile overrides for mutation.
    pub(crate) fn model_profile_overrides_mut(&mut self) -> &mut RuntimeModelProfileOverrideStore {
        self.bindings.model_profile_overrides_mut()
    }

    /// Returns the optional provider credential store.
    pub(crate) fn auth_store(&self) -> Option<&AuthStore> {
        self.credentials.auth_store()
    }

    /// Replaces the optional provider credential store.
    pub(crate) fn set_auth_store(&mut self, store: Option<AuthStore>) {
        self.credentials.set_auth_store(store);
    }

    /// Returns the proactive provider-token refresh leeway.
    pub(crate) fn provider_auth_refresh_leeway_seconds(&self) -> u64 {
        self.credentials.provider_auth_refresh_leeway_seconds()
    }

    /// Replaces the proactive provider-token refresh leeway.
    pub(crate) fn set_provider_auth_refresh_leeway_seconds(&mut self, seconds: u64) {
        self.credentials
            .set_provider_auth_refresh_leeway_seconds(seconds);
    }

    /// Returns the optional project-trust store.
    pub(crate) fn project_trust_store(&self) -> Option<&ProjectTrustStore> {
        self.credentials.project_trust_store()
    }

    /// Returns the project-trust store for decision mutation.
    pub(crate) fn project_trust_store_mut(&mut self) -> Option<&mut ProjectTrustStore> {
        self.credentials.project_trust_store_mut()
    }

    /// Replaces the optional project-trust store.
    pub(crate) fn set_project_trust_store(&mut self, store: Option<ProjectTrustStore>) {
        self.credentials.set_project_trust_store(store);
    }

    /// Returns the optional project-trust database path.
    pub(crate) fn project_trust_database_path(&self) -> Option<&Path> {
        self.credentials.project_trust_database_path()
    }

    /// Replaces the optional project-trust database path.
    pub(crate) fn set_project_trust_database_path(&mut self, path: Option<PathBuf>) {
        self.credentials.set_project_trust_database_path(path);
    }

    /// Marks a project-trust root as already announced and reports whether it was new.
    pub(crate) fn mark_project_trust_root_announced(&mut self, root: PathBuf) -> bool {
        self.credentials.mark_project_trust_root_announced(root)
    }

    /// Clears a project-trust root announcement marker.
    pub(crate) fn clear_project_trust_root_announcement(&mut self, root: &Path) -> bool {
        self.credentials.clear_project_trust_root_announcement(root)
    }

    /// Returns configured hook definitions.
    pub(crate) fn hook_definitions(&self) -> &[HookDefinition] {
        self.hooks.definitions()
    }

    /// Replaces configured hook definitions.
    pub(crate) fn replace_hook_definitions(&mut self, definitions: Vec<HookDefinition>) {
        self.hooks.replace_definitions(definitions);
    }

    /// Returns the focused-shell hook queue.
    pub(crate) fn focused_shell_hook_queue(&self) -> &FocusedShellHookQueue {
        self.hooks.focused_shell_queue()
    }

    /// Returns the focused-shell hook queue for mutation.
    pub(crate) fn focused_shell_hook_queue_mut(&mut self) -> &mut FocusedShellHookQueue {
        self.hooks.focused_shell_queue_mut()
    }

    /// Replaces the focused-shell hook queue after transactional execution.
    pub(crate) fn replace_focused_shell_hook_queue(&mut self, queue: FocusedShellHookQueue) {
        self.hooks.replace_focused_shell_queue(queue);
    }

    /// Allocates one monotonic focused-shell hook marker.
    pub(crate) fn allocate_focused_shell_hook_marker(&mut self) -> u64 {
        self.hooks.allocate_focused_shell_marker()
    }

    /// Returns pending focused-shell hook transactions.
    pub(crate) fn focused_shell_hook_transactions(
        &self,
    ) -> &BTreeMap<String, PendingFocusedShellHookTransaction> {
        self.hooks.focused_shell_transactions()
    }

    /// Returns pending focused-shell hook transactions for mutation.
    pub(crate) fn focused_shell_hook_transactions_mut(
        &mut self,
    ) -> &mut BTreeMap<String, PendingFocusedShellHookTransaction> {
        self.hooks.focused_shell_transactions_mut()
    }

    /// Returns retained focused-shell hook results.
    pub(crate) fn focused_shell_hook_results(&self) -> &[HookExecutionResult] {
        self.hooks.focused_shell_results()
    }

    /// Returns retained focused-shell hook results for bounded mutation.
    pub(crate) fn focused_shell_hook_results_mut(&mut self) -> &mut Vec<HookExecutionResult> {
        self.hooks.focused_shell_results_mut()
    }
}
