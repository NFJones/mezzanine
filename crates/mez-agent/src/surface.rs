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
/// executable MAAP actions after a capability has been granted. Runtime-owned
/// default gates may still widen a capability-decision request with already
/// available diagnostic or integration actions such as MCP and memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelInteractionKind {
    /// The model may speak to the user or request a coarse capability. The base
    /// surface is non-executing, but runtime-owned default gates can add
    /// already-available MCP or memory actions before provider submission.
    CapabilityDecision,
    /// The model may emit only the executable MAAP actions exposed through the
    /// request's allowed-action set.
    ActionExecution,
    /// The controller has settled capability decisions and the model is
    /// continuing on the resulting concrete action surface.
    CapabilityContinuation,
    /// The model is repairing malformed MAAP for the same interaction surface.
    MaapRepair,
    /// The model is producing an internal automatic sizing decision. The
    /// response is parsed as structured JSON and is not replayed as ordinary
    /// conversation context.
    AutoSizing,
    /// The model is judging one completed agent-macro step. The response is a
    /// constrained JSON decision that the runtime validates and executes; it is
    /// not a MAAP action batch and is not replayed as conversation content.
    MacroJudge,
    /// The model is classifying an ambiguous sandbox payload failure from
    /// bounded runtime evidence. The response is structured JSON and cannot
    /// grant execution authority.
    SandboxFailureAssessment,
    /// The model is compacting conversation context into durable summary text.
    /// The response is raw summary text and never uses a MAAP tool schema.
    Compaction,
    /// The model is extracting durable memories from supplied source context.
    /// The response is raw JSON and never uses a MAAP tool schema.
    Memory,
    /// The model is retrying after provider output exhaustion and must return
    /// one minimal complete action batch or final answer.
    OutputLimitRetry,
    /// A routed worker is returning the structured JSON handoff requested by
    /// its controller task context.
    RoutedHandoff,
    /// A routed worker is correcting a rejected structured JSON handoff.
    RoutedHandoffRepair,
    /// The parent model is presenting completed routed-worker evidence to the
    /// original user.
    RoutedPresentation,
    /// The parent model is explaining a routed workflow failure to the user.
    RoutedFailureExplanation,
    /// The model is producing a bounded user-facing summary of a terminal
    /// provider or controller failure.
    FailureSummary,
    /// The model is producing a bounded single-line display title for one
    /// conversation from already-published, bounded inputs.
    ///
    /// The response is never replayed as conversation content: product code
    /// sanitizes it under the shared title bounds and stores it as display
    /// state only. It exposes no tools and never grants execution authority.
    SessionTitle,
}

impl ModelInteractionKind {
    /// Returns the stable provider/debug name for the interaction kind.
    pub fn as_str(self) -> &'static str {
        match self {
            ModelInteractionKind::CapabilityDecision => "capability_decision",
            ModelInteractionKind::ActionExecution => "action_execution",
            ModelInteractionKind::CapabilityContinuation => "capability_continuation",
            ModelInteractionKind::MaapRepair => "maap_repair",
            ModelInteractionKind::AutoSizing => "auto_sizing",
            ModelInteractionKind::MacroJudge => "macro_judge",
            ModelInteractionKind::SandboxFailureAssessment => "sandbox_failure_assessment",
            ModelInteractionKind::Compaction => "compaction",
            ModelInteractionKind::Memory => "memory",
            ModelInteractionKind::OutputLimitRetry => "output_limit_retry",
            ModelInteractionKind::RoutedHandoff => "routed_handoff",
            ModelInteractionKind::RoutedHandoffRepair => "routed_handoff_repair",
            ModelInteractionKind::RoutedPresentation => "routed_presentation",
            ModelInteractionKind::RoutedFailureExplanation => "routed_failure_explanation",
            ModelInteractionKind::FailureSummary => "failure_summary",
            ModelInteractionKind::SessionTitle => "session_title",
        }
    }

    /// Reports whether this provider request expects a MAAP action batch.
    pub fn expects_maap_batch(self) -> bool {
        matches!(
            self,
            ModelInteractionKind::CapabilityDecision
                | ModelInteractionKind::ActionExecution
                | ModelInteractionKind::CapabilityContinuation
                | ModelInteractionKind::MaapRepair
                | ModelInteractionKind::OutputLimitRetry
                | ModelInteractionKind::RoutedPresentation
                | ModelInteractionKind::RoutedFailureExplanation
                | ModelInteractionKind::FailureSummary
        )
    }

    /// Reports whether this provider request expects runtime-owned JSON.
    pub fn expects_structured_json(self) -> bool {
        matches!(
            self,
            ModelInteractionKind::AutoSizing
                | ModelInteractionKind::MacroJudge
                | ModelInteractionKind::SandboxFailureAssessment
                | ModelInteractionKind::RoutedHandoff
                | ModelInteractionKind::RoutedHandoffRepair
        )
    }

    /// Reports whether this interaction returns a routed-worker handoff.
    pub fn is_routed_handoff(self) -> bool {
        matches!(self, Self::RoutedHandoff | Self::RoutedHandoffRepair)
    }

    /// Returns the stable mode-specific system instruction for exceptional
    /// interactions that share the ordinary MAAP response envelope.
    pub fn system_instruction(self) -> Option<&'static str> {
        match self {
            ModelInteractionKind::CapabilityContinuation => None,
            ModelInteractionKind::MaapRepair => Some(
                "The previous provider response failed MAAP validation before any action executed. Return exactly one corrected MAAP action batch on the currently allowed surface. Do not mention the repair process to the user.",
            ),
            ModelInteractionKind::OutputLimitRetry => Some(
                "The previous response hit the provider output limit. If safe partial assistant text is appended, treat it as already emitted and continue from it without repetition. Return one minimal complete MAAP batch when work remains or one short final answer when it does not. Omit progress prose, plans, evidence recaps, command logs, and explanations from this retry.",
            ),
            ModelInteractionKind::RoutedHandoff => Some(
                "Complete the routed handoff task from controller-origin context. Return only the requested structured JSON handoff; do not continue implementation or address the end user.",
            ),
            ModelInteractionKind::RoutedHandoffRepair => Some(
                "Correct the routed handoff using the appended invalid-output and validation evidence. Return only the requested corrected structured JSON handoff.",
            ),
            ModelInteractionKind::RoutedPresentation => Some(
                "Answer the original user from the appended routed-worker result and handoff evidence. Preserve the worker's facts, do not redo its work, and do not discuss internal routing unless it is necessary to explain the result.",
            ),
            ModelInteractionKind::RoutedFailureExplanation => Some(
                "Give the original user one concise, accurate explanation of the routed workflow failure using the appended evidence. Do not claim success, invent missing results, or retry the routed work.",
            ),
            ModelInteractionKind::FailureSummary => Some(
                "Return one concise user-facing summary of the terminal failure evidence. State what failed and any concrete next step without inventing completion or emitting executable actions.",
            ),
            ModelInteractionKind::CapabilityDecision
            | ModelInteractionKind::ActionExecution
            | ModelInteractionKind::AutoSizing
            | ModelInteractionKind::MacroJudge
            | ModelInteractionKind::SandboxFailureAssessment
            | ModelInteractionKind::Compaction
            | ModelInteractionKind::Memory
            | ModelInteractionKind::SessionTitle => None,
        }
    }

    /// Returns the diagnostic reason when this mode intentionally changes the
    /// request's stable instruction profile.
    pub fn expected_cache_break_reason(self) -> Option<&'static str> {
        match self {
            ModelInteractionKind::CapabilityDecision
            | ModelInteractionKind::ActionExecution
            | ModelInteractionKind::CapabilityContinuation => None,
            ModelInteractionKind::MaapRepair
            | ModelInteractionKind::AutoSizing
            | ModelInteractionKind::MacroJudge
            | ModelInteractionKind::SandboxFailureAssessment
            | ModelInteractionKind::Compaction
            | ModelInteractionKind::Memory
            | ModelInteractionKind::OutputLimitRetry
            | ModelInteractionKind::RoutedHandoff
            | ModelInteractionKind::RoutedHandoffRepair
            | ModelInteractionKind::RoutedPresentation
            | ModelInteractionKind::RoutedFailureExplanation
            | ModelInteractionKind::FailureSummary
            | ModelInteractionKind::SessionTitle => Some(self.as_str()),
        }
    }

    /// Returns mode guidance that belongs at the chronological transition
    /// boundary instead of in the cache-sensitive system instruction prefix.
    pub fn chronological_instruction(self) -> Option<&'static str> {
        match self {
            ModelInteractionKind::CapabilityContinuation => Some(
                "Continue the active task using the appended controller capability decisions and the currently allowed action surface. Do not repeat the capability request or describe the controller negotiation unless it affects the user-facing result.",
            ),
            _ => None,
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
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
    /// Search configured MCP server metadata.
    McpServerSearch,
    /// Retrieve configured MCP server metadata.
    McpServerGet,
    /// MCP tool call.
    McpCall,
    /// Search persistent memory records.
    MemorySearch,
    /// Store one persistent memory record.
    MemoryStore,
    /// List discoverable session agents (read-only discovery).
    ListAgents,
    /// Add one local project issue.
    IssueAdd,
    /// Update one local project issue.
    IssueUpdate,
    /// Query local project issues.
    IssueQuery,
    /// Delete one local project issue.
    IssueDelete,
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
            AllowedAction::McpServerSearch => "mcp_server_search",
            AllowedAction::McpServerGet => "mcp_server_get",
            AllowedAction::McpCall => "mcp_call",
            AllowedAction::MemorySearch => "memory_search",
            AllowedAction::MemoryStore => "memory_store",
            AllowedAction::ListAgents => "list_agents",
            AllowedAction::IssueAdd => "issue_add",
            AllowedAction::IssueUpdate => "issue_update",
            AllowedAction::IssueQuery => "issue_query",
            AllowedAction::IssueDelete => "issue_delete",
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
            "mcp_server_search" => Some(AllowedAction::McpServerSearch),
            "mcp_server_get" => Some(AllowedAction::McpServerGet),
            "mcp_call" => Some(AllowedAction::McpCall),
            "memory_search" => Some(AllowedAction::MemorySearch),
            "memory_store" => Some(AllowedAction::MemoryStore),
            "list_agents" => Some(AllowedAction::ListAgents),
            "issue_add" => Some(AllowedAction::IssueAdd),
            "issue_update" => Some(AllowedAction::IssueUpdate),
            "issue_query" => Some(AllowedAction::IssueQuery),
            "issue_delete" => Some(AllowedAction::IssueDelete),
            _ => None,
        }
    }
}

/// One configured routed model size offered to `spawn_agent` selections.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SpawnAgentSizeOption {
    /// Routed size bucket name: `small`, `medium`, or `large`.
    pub size: String,
    /// Configured model profile resolved for this bucket.
    pub profile_name: String,
    /// Complete resolved execution contract captured with this frozen catalog.
    ///
    /// Legacy persisted catalogs omit this field and remain readable, but
    /// explicit selections from them fail closed rather than consulting
    /// mutable live profile definitions.
    #[serde(default)]
    pub execution_profile: Option<crate::ModelProfile>,
    /// Reasoning efforts accepted for an explicit size/reasoning pair.
    pub allowed_reasoning_efforts: Vec<String>,
}

/// Product-provided routed-size reasoning contract for `spawn_agent`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SpawnAgentSizing {
    /// Configured routed size offers in small, medium, large order.
    pub sizes: Vec<SpawnAgentSizeOption>,
}

/// Durable structural identity for one spawned-session conversation.
///
/// This value is owned by the child conversation rather than its replaceable
/// pane binding, allowing restart and direct resume to retain delegation
/// capacity semantics.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SubagentSessionLineage {
    /// Direct parent agent that created this child.
    pub parent_agent_id: String,
    /// Root agent that owns the delegation tree.
    pub root_agent_id: String,
    /// Child depth below the root agent.
    pub depth: usize,
    /// Stable human-readable child display name.
    pub display_name: String,
    /// Whether this child is forbidden from spawning descendants.
    pub terminal: bool,
}

impl SubagentSessionLineage {
    /// Validates durable child lineage before it can restore delegation state.
    pub fn validate_persisted(&self) -> Result<(), String> {
        for (field, value) in [
            ("parent agent id", &self.parent_agent_id),
            ("root agent id", &self.root_agent_id),
            ("display name", &self.display_name),
        ] {
            if value.trim().is_empty() {
                return Err(format!("persisted subagent lineage {field} is empty"));
            }
        }
        for (field, value) in [
            ("parent agent id", &self.parent_agent_id),
            ("root agent id", &self.root_agent_id),
        ] {
            if mez_core::ids::AgentId::opaque(value.clone()).is_none() {
                return Err(format!("persisted subagent lineage {field} is invalid"));
            }
        }
        if self.depth == 0 {
            return Err("persisted subagent lineage depth must be greater than zero".to_string());
        }
        Ok(())
    }
}

/// Controller-owned concrete action surface for one provider request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct AllowedActionSet {
    /// Stores the allowed action values.
    pub actions: BTreeSet<AllowedAction>,
    /// Product-provided setting-path guidance for config-change actions.
    config_change_setting_path_description: Option<String>,
    /// Product-provided routed-size reasoning offers for `spawn_agent`.
    spawn_agent_sizing: Option<SpawnAgentSizing>,
}

impl AllowedActionSet {
    /// Builds the complete provider-visible action surface used by ordinary
    /// agent turns.
    ///
    /// Capability negotiation and model-selected skill loading are legacy
    /// controller protocols, not executable actions in the static schema.
    pub fn all_enabled() -> Self {
        Self::from_actions([
            AllowedAction::Say,
            AllowedAction::ShellCommand,
            AllowedAction::ApplyPatch,
            AllowedAction::WebSearch,
            AllowedAction::FetchUrl,
            AllowedAction::SendMessage,
            AllowedAction::SpawnAgent,
            AllowedAction::ConfigChange,
            AllowedAction::McpServerSearch,
            AllowedAction::McpServerGet,
            AllowedAction::McpCall,
            AllowedAction::MemorySearch,
            AllowedAction::MemoryStore,
            AllowedAction::ListAgents,
            AllowedAction::IssueAdd,
            AllowedAction::IssueUpdate,
            AllowedAction::IssueQuery,
            AllowedAction::IssueDelete,
        ])
    }

    /// Returns whether this set contains only configurable executable actions.
    pub fn is_configurable(&self) -> bool {
        self.actions
            .iter()
            .all(|action| Self::all_enabled().contains(*action))
    }

    /// Validates a catalog loaded from durable session metadata.
    pub fn validate_persisted(&self) -> Result<(), String> {
        if self.actions.is_empty() {
            return Err("persisted action catalog must contain an executable action".to_string());
        }
        if !self.is_configurable() {
            return Err("persisted action catalog contains a non-configurable action".to_string());
        }
        match (
            self.contains(AllowedAction::ConfigChange),
            self.config_change_setting_path_description.as_deref(),
        ) {
            (true, Some(description)) if !description.trim().is_empty() => {}
            (true, _) => {
                return Err(
                    "persisted config_change action lacks captured setting-path guidance"
                        .to_string(),
                );
            }
            (false, Some(_)) => {
                return Err(
                    "persisted config-change metadata has no config_change owner".to_string(),
                );
            }
            (false, None) => {}
        }
        let sizing = match (
            self.contains(AllowedAction::SpawnAgent),
            self.spawn_agent_sizing.as_ref(),
        ) {
            (true, Some(sizing)) => sizing,
            (true, None) => {
                return Err(
                    "persisted spawn_agent action lacks captured sizing contract".to_string(),
                );
            }
            (false, Some(_)) => {
                return Err("persisted spawn sizing metadata has no spawn_agent owner".to_string());
            }
            (false, None) => return Ok(()),
        };
        let mut sizes = BTreeSet::new();
        for option in &sizing.sizes {
            if !matches!(option.size.as_str(), "small" | "medium" | "large")
                || !sizes.insert(option.size.as_str())
            {
                return Err(
                    "persisted spawn sizing contains an invalid or duplicate size".to_string(),
                );
            }
            if option.profile_name.trim().is_empty() {
                return Err("persisted spawn sizing contains an invalid profile name".to_string());
            }
            if let Some(profile) = option.execution_profile.as_ref()
                && (profile.provider.trim().is_empty() || profile.model.trim().is_empty())
            {
                return Err(
                    "persisted spawn sizing contains an invalid execution profile".to_string(),
                );
            }
            let mut efforts = BTreeSet::new();
            for effort in &option.allowed_reasoning_efforts {
                if !matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh")
                    || !efforts.insert(effort.as_str())
                {
                    return Err(
                        "persisted spawn sizing contains an invalid or duplicate reasoning effort"
                            .to_string(),
                    );
                }
            }
        }
        Ok(())
    }

    /// Builds the initial non-executing capability-decision surface.
    pub fn capability_decision() -> Self {
        Self::from_actions([AllowedAction::Say, AllowedAction::RequestCapability])
    }

    /// Builds a response-only action surface.
    pub fn respond_only() -> Self {
        Self::from_actions([AllowedAction::Say])
    }

    /// Builds the non-effecting base surface for action-execution requests.
    pub fn action_execution_base() -> Self {
        Self::from_actions([AllowedAction::Say, AllowedAction::RequestCapability])
    }

    /// Builds an action surface that can only emit user-facing text.
    pub fn say_only() -> Self {
        Self::from_actions([AllowedAction::Say])
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
            AgentCapability::Mcp => output.extend([
                AllowedAction::McpServerSearch,
                AllowedAction::McpServerGet,
                AllowedAction::McpCall,
            ]),
            AgentCapability::Subagent => {
                output.extend([AllowedAction::SendMessage, AllowedAction::SpawnAgent])
            }
            AgentCapability::ConfigChange => output.extend([AllowedAction::ConfigChange]),
            AgentCapability::Memory => {
                output.extend([AllowedAction::MemorySearch, AllowedAction::MemoryStore])
            }
            AgentCapability::Issues => output.extend([
                AllowedAction::IssueAdd,
                AllowedAction::IssueUpdate,
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
            config_change_setting_path_description: None,
            spawn_agent_sizing: None,
        }
    }

    /// Attaches product-specific setting-path guidance to this action surface.
    pub fn with_config_change_setting_path_description(
        mut self,
        description: impl Into<String>,
    ) -> Self {
        self.config_change_setting_path_description = Some(description.into());
        self
    }

    /// Returns product-specific config-change setting-path guidance, if set.
    pub fn config_change_setting_path_description(&self) -> Option<&str> {
        self.config_change_setting_path_description.as_deref()
    }

    /// Attaches routed-size reasoning offers to spawned-child guidance.
    ///
    /// The product supplies one entry per configured routed size so provider
    /// schemas can advertise only size/reasoning pairs the runtime accepts.
    pub fn with_spawn_agent_sizing(mut self, sizing: SpawnAgentSizing) -> Self {
        self.spawn_agent_sizing = Some(sizing);
        self
    }

    /// Returns routed-size reasoning offers for `spawn_agent`, if set.
    pub fn spawn_agent_sizing(&self) -> Option<&SpawnAgentSizing> {
        self.spawn_agent_sizing.as_ref()
    }

    /// Returns a restriction to the supplied actions with orphaned metadata
    /// pruned from the resulting schema contract.
    pub fn restricted_to(&self, actions: impl IntoIterator<Item = AllowedAction>) -> Self {
        let actions = actions.into_iter().collect::<BTreeSet<_>>();
        let actions = self
            .actions
            .intersection(&actions)
            .copied()
            .collect::<BTreeSet<_>>();
        Self {
            config_change_setting_path_description: actions
                .contains(&AllowedAction::ConfigChange)
                .then(|| self.config_change_setting_path_description.clone())
                .flatten(),
            spawn_agent_sizing: actions
                .contains(&AllowedAction::SpawnAgent)
                .then(|| self.spawn_agent_sizing.clone())
                .flatten(),
            actions,
        }
    }

    /// Returns whether every action in `other` is available in this catalog.
    pub fn contains_set(&self, other: &AllowedActionSet) -> bool {
        other.actions.is_subset(&self.actions)
    }

    /// Adds actions to the set.
    pub fn extend(&mut self, actions: impl IntoIterator<Item = AllowedAction>) {
        self.actions.extend(actions);
    }

    /// Adds all actions from another set.
    pub fn extend_set(&mut self, other: &AllowedActionSet) {
        self.actions.extend(other.actions.iter().copied());
        if other.config_change_setting_path_description.is_some() {
            self.config_change_setting_path_description =
                other.config_change_setting_path_description.clone();
        }
        if other.spawn_agent_sizing.is_some() {
            self.spawn_agent_sizing = other.spawn_agent_sizing.clone();
        }
    }

    /// Removes one action from the exposed action surface.
    pub fn remove(&mut self, action: AllowedAction) {
        self.actions.remove(&action);
        if action == AllowedAction::ConfigChange {
            self.config_change_setting_path_description = None;
        }
        if action == AllowedAction::SpawnAgent {
            self.spawn_agent_sizing = None;
        }
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

impl Default for AllowedActionSet {
    /// Enables every configurable MAAP action by default.
    fn default() -> Self {
        Self::all_enabled()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AllowedAction, AllowedActionSet, SpawnAgentSizeOption, SpawnAgentSizing,
        SubagentSessionLineage,
    };

    fn captured_execution_profile() -> crate::ModelProfile {
        crate::ModelProfile {
            provider: "test-provider".to_string(),
            model: "test-model".to_string(),
            ..Default::default()
        }
    }

    /// Verifies a spawned-session lineage cannot restore as depth zero and
    /// thereby acquire root delegation capacity.
    #[test]
    fn persisted_subagent_lineage_rejects_depth_zero() {
        let lineage = SubagentSessionLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 0,
            display_name: "child".to_string(),
            terminal: false,
        };

        assert_eq!(
            lineage.validate_persisted(),
            Err("persisted subagent lineage depth must be greater than zero".to_string())
        );
    }

    /// Verifies durable child lineage rejects unsafe agent identifiers before
    /// restored state can enter delegation traversal.
    #[test]
    fn persisted_subagent_lineage_rejects_invalid_agent_ids() {
        let lineage = SubagentSessionLineage {
            parent_agent_id: "agent-\u{0007}".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 1,
            display_name: "child".to_string(),
            terminal: false,
        };

        assert_eq!(
            lineage.validate_persisted(),
            Err("persisted subagent lineage parent agent id is invalid".to_string())
        );
    }

    /// Verifies restriction intersects authority and prunes metadata whose
    /// owning action is absent from the frozen child catalog.
    #[test]
    fn restriction_prunes_orphaned_catalog_metadata_and_reports_subsets() {
        let parent = AllowedActionSet::from_actions([
            AllowedAction::Say,
            AllowedAction::ConfigChange,
            AllowedAction::SpawnAgent,
        ])
        .with_config_change_setting_path_description("captured setting paths")
        .with_spawn_agent_sizing(SpawnAgentSizing {
            sizes: vec![SpawnAgentSizeOption {
                size: "small".to_string(),
                profile_name: "small-profile".to_string(),
                execution_profile: Some(captured_execution_profile()),
                allowed_reasoning_efforts: vec!["medium".to_string()],
            }],
        });

        let child = parent.restricted_to([AllowedAction::Say, AllowedAction::SpawnAgent]);

        assert_eq!(child.action_type_names(), ["say", "spawn_agent"]);
        assert_eq!(child.config_change_setting_path_description(), None);
        assert_eq!(child.spawn_agent_sizing(), parent.spawn_agent_sizing());
        let mut terminal = child.clone();
        terminal.remove(AllowedAction::SpawnAgent);
        assert_eq!(terminal.spawn_agent_sizing(), None);
        assert!(parent.contains_set(&child));
        assert!(!child.contains_set(&parent));
    }

    /// Verifies persisted schema-bearing actions require the captured metadata
    /// used to freeze their provider-visible contracts at session creation.
    #[test]
    fn persisted_catalog_requires_complete_schema_metadata() {
        let missing_config_guidance = AllowedActionSet::from_actions([AllowedAction::ConfigChange]);
        assert_eq!(
            missing_config_guidance.validate_persisted(),
            Err("persisted config_change action lacks captured setting-path guidance".to_string())
        );

        let missing_spawn_sizing = AllowedActionSet::from_actions([AllowedAction::SpawnAgent]);
        assert_eq!(
            missing_spawn_sizing.validate_persisted(),
            Err("persisted spawn_agent action lacks captured sizing contract".to_string())
        );

        let legacy_sizing = AllowedActionSet::from_actions([AllowedAction::SpawnAgent])
            .with_spawn_agent_sizing(SpawnAgentSizing {
                sizes: vec![SpawnAgentSizeOption {
                    size: "small".to_string(),
                    profile_name: "small-profile".to_string(),
                    execution_profile: None,
                    allowed_reasoning_efforts: vec!["medium".to_string()],
                }],
            });
        assert!(legacy_sizing.validate_persisted().is_ok());

        let decoded_legacy_sizing = serde_json::from_str::<AllowedActionSet>(
            r#"{"actions":["SpawnAgent"],"spawn_agent_sizing":{"sizes":[{"size":"small","profile_name":"small-profile","allowed_reasoning_efforts":["medium"]}]}}"#,
        )
        .unwrap();
        assert!(decoded_legacy_sizing.validate_persisted().is_ok());
        assert!(
            decoded_legacy_sizing
                .spawn_agent_sizing()
                .unwrap()
                .sizes
                .first()
                .unwrap()
                .execution_profile
                .is_none()
        );

        let explicitly_unavailable = AllowedActionSet::from_actions([AllowedAction::SpawnAgent])
            .with_spawn_agent_sizing(SpawnAgentSizing { sizes: Vec::new() });
        assert!(explicitly_unavailable.validate_persisted().is_ok());
    }
}
