//! Live provider, MCP, model, and agent-profile bindings.

use std::collections::BTreeMap;

use mez_agent::mcp::McpRegistry;
use mez_agent::{
    PresetRegistry as RuntimePresetRegistry, ProviderRegistry as RuntimeProviderRegistry,
    SubagentProfile,
};

use crate::runtime::service_state::{
    RuntimeAgentPersonalityProfile, RuntimeMcpTransportSet, RuntimeModelProfileOverrideStore,
};

/// Owns concrete live bindings used to resolve and execute agent work.
#[derive(Debug)]
pub(super) struct RuntimeBindingsState {
    mcp_registry: McpRegistry,
    mcp_transports: RuntimeMcpTransportSet,
    provider_registry: RuntimeProviderRegistry,
    preset_registry: RuntimePresetRegistry,
    subagent_profiles: BTreeMap<String, SubagentProfile>,
    agent_personality_profiles: BTreeMap<String, RuntimeAgentPersonalityProfile>,
    default_agent_personality: Option<String>,
    custom_agent_system_prompt: Option<String>,
    agent_personality_selections: BTreeMap<String, String>,
    model_profile_overrides: RuntimeModelProfileOverrideStore,
}

impl RuntimeBindingsState {
    pub(super) fn new(
        provider_registry: RuntimeProviderRegistry,
        subagent_profiles: BTreeMap<String, SubagentProfile>,
    ) -> Self {
        Self {
            mcp_registry: McpRegistry::default(),
            mcp_transports: RuntimeMcpTransportSet::default(),
            provider_registry,
            preset_registry: RuntimePresetRegistry::default(),
            subagent_profiles,
            agent_personality_profiles: BTreeMap::new(),
            default_agent_personality: None,
            custom_agent_system_prompt: None,
            agent_personality_selections: BTreeMap::new(),
            model_profile_overrides: RuntimeModelProfileOverrideStore::default(),
        }
    }

    pub(super) fn mcp_registry(&self) -> &McpRegistry {
        &self.mcp_registry
    }

    pub(super) fn mcp_registry_mut(&mut self) -> &mut McpRegistry {
        &mut self.mcp_registry
    }

    pub(super) fn mcp_transports_mut(&mut self) -> &mut RuntimeMcpTransportSet {
        &mut self.mcp_transports
    }

    pub(super) fn provider_registry(&self) -> &RuntimeProviderRegistry {
        &self.provider_registry
    }

    pub(super) fn provider_registry_mut(&mut self) -> &mut RuntimeProviderRegistry {
        &mut self.provider_registry
    }

    pub(super) fn replace_provider_registry(&mut self, registry: RuntimeProviderRegistry) {
        self.provider_registry = registry;
    }

    pub(super) fn preset_registry(&self) -> &RuntimePresetRegistry {
        &self.preset_registry
    }

    pub(super) fn preset_registry_mut(&mut self) -> &mut RuntimePresetRegistry {
        &mut self.preset_registry
    }

    pub(super) fn subagent_profiles(&self) -> &BTreeMap<String, SubagentProfile> {
        &self.subagent_profiles
    }

    pub(super) fn replace_subagent_profiles(
        &mut self,
        profiles: BTreeMap<String, SubagentProfile>,
    ) {
        self.subagent_profiles = profiles;
    }

    pub(super) fn agent_personality_profiles(
        &self,
    ) -> &BTreeMap<String, RuntimeAgentPersonalityProfile> {
        &self.agent_personality_profiles
    }

    pub(super) fn replace_agent_personality_profiles(
        &mut self,
        profiles: BTreeMap<String, RuntimeAgentPersonalityProfile>,
    ) {
        self.agent_personality_profiles = profiles;
    }

    pub(super) fn default_agent_personality(&self) -> Option<&str> {
        self.default_agent_personality.as_deref()
    }

    pub(super) fn set_default_agent_personality(&mut self, personality: Option<String>) {
        self.default_agent_personality = personality;
    }

    pub(super) fn custom_agent_system_prompt(&self) -> Option<&str> {
        self.custom_agent_system_prompt.as_deref()
    }

    pub(super) fn set_custom_agent_system_prompt(&mut self, prompt: Option<String>) {
        self.custom_agent_system_prompt = prompt;
    }

    pub(super) fn agent_personality_selections(&self) -> &BTreeMap<String, String> {
        &self.agent_personality_selections
    }

    pub(super) fn agent_personality_selections_mut(&mut self) -> &mut BTreeMap<String, String> {
        &mut self.agent_personality_selections
    }

    pub(super) fn model_profile_overrides(&self) -> &RuntimeModelProfileOverrideStore {
        &self.model_profile_overrides
    }

    pub(super) fn model_profile_overrides_mut(&mut self) -> &mut RuntimeModelProfileOverrideStore {
        &mut self.model_profile_overrides
    }
}
