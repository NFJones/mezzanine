//! Provider interaction and MAAP action-surface types.
//!
//! This module owns the small, shared vocabulary that describes what kind of
//! model interaction is in progress and which concrete MAAP actions are exposed
//! for that interaction. Keeping these types together avoids mixing action
//! routing rules with context block storage and provider message assembly.

use std::collections::BTreeSet;

/// Describes the kind of provider interaction Mezzanine is requesting.
///
/// The interaction kind is controller-owned state. It tells providers whether
/// the model is currently deciding which capability it needs or emitting
/// executable MAAP actions after a capability has been granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelInteractionKind {
    /// The model may speak to the user or request a coarse capability, but it
    /// must not emit executable filesystem, shell, network, MCP, or subagent
    /// actions.
    CapabilityDecision,
    /// The model may emit only the executable MAAP actions exposed through the
    /// request's allowed-action set.
    ActionExecution,
    /// The model is repairing malformed MAAP for the same interaction surface.
    Repair,
    /// The model is producing an internal automatic sizing decision. The
    /// response is parsed as structured JSON and is not replayed as ordinary
    /// conversation context.
    AutoSizing,
}

impl ModelInteractionKind {
    /// Returns the stable provider/debug name for the interaction kind.
    pub fn as_str(self) -> &'static str {
        match self {
            ModelInteractionKind::CapabilityDecision => "capability_decision",
            ModelInteractionKind::ActionExecution => "action_execution",
            ModelInteractionKind::Repair => "repair",
            ModelInteractionKind::AutoSizing => "auto_sizing",
        }
    }
}

/// Coarse capabilities the model may request before executable actions are
/// exposed.
///
/// Capabilities are intentionally broader than individual MAAP actions. The
/// controller can grant or deny them with simple policy and runtime-context
/// checks, while the model still chooses the concrete action once a capability
/// is granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentCapability {
    /// Respond to the user without external effects.
    RespondOnly,
    /// Execute a pane shell command.
    Shell,
    /// Search external HTTP(S) information.
    NetworkSearch,
    /// Fetch an external HTTP(S) URL.
    NetworkFetch,
    /// Call an available MCP tool.
    Mcp,
    /// Send a local agent message or spawn a subagent.
    Subagent,
    /// Request a Mezzanine configuration change.
    ConfigChange,
    /// Search or store persistent memory records.
    Memory,
    /// Add, query, or delete local project issue records.
    Issues,
}

impl AgentCapability {
    /// Returns the stable schema/debug name for the capability.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentCapability::RespondOnly => "respond_only",
            AgentCapability::Shell => "shell",
            AgentCapability::NetworkSearch => "network_search",
            AgentCapability::NetworkFetch => "network_fetch",
            AgentCapability::Mcp => "mcp",
            AgentCapability::Subagent => "subagent",
            AgentCapability::ConfigChange => "config_change",
            AgentCapability::Memory => "memory",
            AgentCapability::Issues => "issues",
        }
    }

    /// Parses a model-authored capability name.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "respond_only" => Some(AgentCapability::RespondOnly),
            "shell" => Some(AgentCapability::Shell),
            "network_search" => Some(AgentCapability::NetworkSearch),
            "network_fetch" => Some(AgentCapability::NetworkFetch),
            "mcp" => Some(AgentCapability::Mcp),
            "subagent" => Some(AgentCapability::Subagent),
            "config_change" => Some(AgentCapability::ConfigChange),
            "memory" => Some(AgentCapability::Memory),
            "issues" => Some(AgentCapability::Issues),
            _ => None,
        }
    }

    /// Returns every provider-visible capability name.
    pub fn all_names() -> &'static [&'static str] {
        &[
            "respond_only",
            "shell",
            "network_search",
            "network_fetch",
            "mcp",
            "subagent",
            "config_change",
            "memory",
            "issues",
        ]
    }
}

/// Concrete MAAP action kinds that may be exposed in one provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AllowedAction {
    /// User-facing text.
    Say,
    /// Non-executing capability request.
    RequestCapability,
    /// Skill catalog request.
    RequestSkills,
    /// Skill context loading.
    CallSkill,
    /// Pane shell command.
    ShellCommand,
    /// Apply a patch.
    ApplyPatch,
    /// External web search.
    WebSearch,
    /// External URL fetch.
    FetchUrl,
    /// Local agent message.
    SendMessage,
    /// Subagent spawn.
    SpawnAgent,
    /// Configuration change.
    ConfigChange,
    /// MCP tool call.
    McpCall,
    /// Search persistent memory records.
    MemorySearch,
    /// Store one persistent memory record.
    MemoryStore,
    /// Add one local project issue.
    IssueAdd,
    /// Query local project issues.
    IssueQuery,
    /// Delete one local project issue.
    IssueDelete,
    /// Abort the turn.
    Abort,
}

impl AllowedAction {
    /// Returns the stable MAAP action type for this allowed action.
    pub fn action_type(self) -> &'static str {
        match self {
            AllowedAction::Say => "say",
            AllowedAction::RequestCapability => "request_capability",
            AllowedAction::RequestSkills => "request_skills",
            AllowedAction::CallSkill => "call_skill",
            AllowedAction::ShellCommand => "shell_command",
            AllowedAction::ApplyPatch => "apply_patch",
            AllowedAction::WebSearch => "web_search",
            AllowedAction::FetchUrl => "fetch_url",
            AllowedAction::SendMessage => "send_message",
            AllowedAction::SpawnAgent => "spawn_agent",
            AllowedAction::ConfigChange => "config_change",
            AllowedAction::McpCall => "mcp_call",
            AllowedAction::MemorySearch => "memory_search",
            AllowedAction::MemoryStore => "memory_store",
            AllowedAction::IssueAdd => "issue_add",
            AllowedAction::IssueQuery => "issue_query",
            AllowedAction::IssueDelete => "issue_delete",
            AllowedAction::Abort => "abort",
        }
    }

    /// Maps a MAAP action type to the corresponding allowed-action value.
    pub fn from_action_type(action_type: &str) -> Option<Self> {
        match action_type {
            "say" => Some(AllowedAction::Say),
            "request_capability" => Some(AllowedAction::RequestCapability),
            "request_skills" => Some(AllowedAction::RequestSkills),
            "call_skill" => Some(AllowedAction::CallSkill),
            "shell_command" => Some(AllowedAction::ShellCommand),
            "apply_patch" => Some(AllowedAction::ApplyPatch),
            "web_search" => Some(AllowedAction::WebSearch),
            "fetch_url" => Some(AllowedAction::FetchUrl),
            "send_message" => Some(AllowedAction::SendMessage),
            "spawn_agent" => Some(AllowedAction::SpawnAgent),
            "config_change" => Some(AllowedAction::ConfigChange),
            "mcp_call" => Some(AllowedAction::McpCall),
            "memory_search" => Some(AllowedAction::MemorySearch),
            "memory_store" => Some(AllowedAction::MemoryStore),
            "issue_add" => Some(AllowedAction::IssueAdd),
            "issue_query" => Some(AllowedAction::IssueQuery),
            "issue_delete" => Some(AllowedAction::IssueDelete),
            "abort" => Some(AllowedAction::Abort),
            _ => None,
        }
    }
}

/// Controller-owned concrete action surface for one provider request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedActionSet {
    /// Stores the allowed action values.
    pub actions: BTreeSet<AllowedAction>,
}

impl AllowedActionSet {
    /// Builds the initial non-executing capability-decision surface.
    pub fn capability_decision() -> Self {
        Self::from_actions([AllowedAction::Say, AllowedAction::RequestCapability])
    }

    /// Builds a response-only action surface.
    pub fn respond_only() -> Self {
        Self::from_actions([AllowedAction::Say, AllowedAction::Abort])
    }

    /// Builds the non-effecting base surface for action-execution requests.
    pub fn action_execution_base() -> Self {
        Self::from_actions([
            AllowedAction::Say,
            AllowedAction::RequestCapability,
            AllowedAction::Abort,
        ])
    }

    /// Builds an action surface that can only emit user-facing text.
    pub fn say_only() -> Self {
        Self::from_actions([AllowedAction::Say, AllowedAction::Abort])
    }

    /// Builds the executable action surface exposed after a capability grant.
    pub fn for_capability(capability: AgentCapability) -> Self {
        let mut output = Self::action_execution_base();
        match capability {
            AgentCapability::RespondOnly => {}
            AgentCapability::Shell => {
                output.extend([AllowedAction::ShellCommand, AllowedAction::ApplyPatch])
            }
            AgentCapability::NetworkSearch => output.extend([AllowedAction::WebSearch]),
            AgentCapability::NetworkFetch => output.extend([AllowedAction::FetchUrl]),
            AgentCapability::Mcp => output.extend([AllowedAction::McpCall]),
            AgentCapability::Subagent => {
                output.extend([AllowedAction::SendMessage, AllowedAction::SpawnAgent])
            }
            AgentCapability::ConfigChange => output.extend([AllowedAction::ConfigChange]),
            AgentCapability::Memory => {
                output.extend([AllowedAction::MemorySearch, AllowedAction::MemoryStore])
            }
            AgentCapability::Issues => output.extend([
                AllowedAction::IssueAdd,
                AllowedAction::IssueQuery,
                AllowedAction::IssueDelete,
            ]),
        }
        output
    }

    /// Builds a set from a sequence of actions.
    pub fn from_actions(actions: impl IntoIterator<Item = AllowedAction>) -> Self {
        Self {
            actions: actions.into_iter().collect(),
        }
    }

    /// Adds actions to the set.
    pub fn extend(&mut self, actions: impl IntoIterator<Item = AllowedAction>) {
        self.actions.extend(actions);
    }

    /// Adds all actions from another set.
    pub fn extend_set(&mut self, other: &AllowedActionSet) {
        self.actions.extend(other.actions.iter().copied());
    }

    /// Removes one action from the exposed action surface.
    pub fn remove(&mut self, action: AllowedAction) {
        self.actions.remove(&action);
    }

    /// Returns true when the given action is exposed in this set.
    pub fn contains(&self, action: AllowedAction) -> bool {
        self.actions.contains(&action)
    }

    /// Returns action type names in stable order for trace and debug output.
    pub fn action_type_names(&self) -> Vec<&'static str> {
        self.actions
            .iter()
            .map(|action| action.action_type())
            .collect()
    }
}
